//! The reconciliation loop: GitHub assigned issues -> Orca workspaces -> agent prompts.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, SystemTime};
use tracing::{debug, error, info, warn};

use crate::config::{Config, RepoConfig};
use crate::github::{Conditional, GitHub, Issue, PrInfo};
use crate::orca::{Delivery, Orca, Worktree};
use crate::origin::{self, Origin};
use crate::prompt::{
    self, FinalComment, ProjectPrompt, PromptContext, Rendered, actor_of, event_key, render_event,
};
use crate::sessions;
use crate::state::{IssueState, State, now_iso};

/// Consecutive delivery failures before an issue is re-onboarded from scratch.
const MAX_DELIVERY_FAILURES: u32 = 5;

pub struct Engine {
    cfg: Config,
    gh: GitHub,
    orca: Orca,
    login: String,
    state: State,
    failures: BTreeMap<(String, u64), u32>,
    /// Items the bot opened that nothing binds to a session (no origin tag,
    /// no branch match, no human trigger), keyed to the `updated_at` they
    /// were last looked at with, so they are not re-examined every pass.
    ignored: BTreeMap<(String, u64), String>,
}

/// New events for an issue relative to what has been delivered already.
struct Diff {
    rendered: Vec<Rendered>,
    /// Every key/marker observed (including filtered ones), to be recorded as seen.
    seen: BTreeMap<String, String>,
}

impl Engine {
    pub async fn new(cfg: Config) -> Result<Self> {
        let token = cfg.github_token()?;
        let gh = GitHub::new(&cfg.github.api_url, &token)?;
        let me = gh.whoami().await.context("verifying GitHub token")?;
        info!(login = me.login, kind = me.kind, "authenticated to GitHub");
        let orca = Orca::new(cfg.orca.clone());
        let mut state = State::load()?;
        state.bot_login = Some(me.login.clone());
        state.save()?;
        Ok(Self {
            cfg,
            gh,
            orca,
            login: me.login,
            state,
            failures: BTreeMap::new(),
            ignored: BTreeMap::new(),
        })
    }

    pub async fn run_forever(mut self) -> Result<()> {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .context("installing SIGTERM handler")?;
        info!(
            repos = self.cfg.repos.len(),
            poll_secs = self.cfg.daemon.poll_interval_secs,
            "ssf daemon started"
        );
        loop {
            self.tick().await;
            let interval = Duration::from_secs(self.cfg.daemon.poll_interval_secs.max(5));
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = tokio::signal::ctrl_c() => { info!("interrupted; exiting"); break; }
                _ = sigterm.recv() => { info!("SIGTERM; exiting"); break; }
            }
        }
        self.state.save()?;
        Ok(())
    }

    /// Pick up edits to the config file between passes (repos, harnesses,
    /// intervals) without a restart. The token is fixed for the process.
    fn reload_config(&mut self) {
        match Config::load() {
            Ok(cfg) => self.cfg = cfg,
            Err(e) => warn!("config reload failed, keeping previous: {e:#}"),
        }
    }

    /// One reconciliation pass over every configured repo.
    pub async fn tick(&mut self) {
        self.reload_config();
        self.state.last_poll_at = Some(now_iso());
        if self.cfg.repos.is_empty() {
            warn!("no repos configured; nothing to do (see `ssf repo add`)");
            return;
        }
        if let Err(e) = self.orca.status().await {
            warn!("Orca unavailable, skipping this pass: {e:#}");
            self.state.last_error = Some(format!("Orca unavailable: {e:#}"));
            let _ = self.state.save();
            return;
        }
        self.state.last_error = None;
        for repo in self.cfg.repos.clone() {
            if let Err(e) = self.tick_repo(&repo).await {
                warn!(repo = repo.name, "pass failed: {e:#}");
                self.state.last_error = Some(format!("{}: {e:#}", repo.name));
            }
            self.capture_sessions(&repo);
            self.run_cleanups(&repo).await;
            if let Err(e) = self.state.save() {
                error!("saving state: {e:#}");
            }
        }
    }

    async fn tick_repo(&mut self, repo: &RepoConfig) -> Result<()> {
        let (owner, name) = repo.split()?;
        let rs = self.state.repo_mut(&repo.name).clone();

        // Four listings, one per trigger. Each carries its own ETag; a 304
        // means that listing (and every item on it) is exactly as last time,
        // so its cached numbers stand in for the contents. The fourth is the
        // bot's own open items: a session hears about what it opened
        // without anyone having to assign or mention it.
        let assigned = self
            .gh
            .items(
                owner,
                name,
                "assignee",
                &self.login,
                rs.issues_etag.as_deref(),
            )
            .await?;
        let mentioned = self
            .gh
            .items(
                owner,
                name,
                "mentioned",
                &self.login,
                rs.mentioned_etag.as_deref(),
            )
            .await?;
        let reviews = self
            .gh
            .review_requested(owner, name, &self.login, rs.pulls_etag.as_deref())
            .await?;
        let created = self
            .gh
            .items(
                owner,
                name,
                "creator",
                &self.login,
                rs.created_etag.as_deref(),
            )
            .await?;
        if matches!(assigned, Conditional::NotModified)
            && matches!(mentioned, Conditional::NotModified)
            && matches!(reviews, Conditional::NotModified)
            && matches!(created, Conditional::NotModified)
        {
            debug!(repo = repo.name, "nothing changed");
            return Ok(());
        }

        // number -> (fresh item if we have one, pr info, triggers)
        let mut items: BTreeMap<u64, (Option<Issue>, Option<PrInfo>, Vec<String>)> =
            BTreeMap::new();
        let mut note = |n: u64, issue: Option<Issue>, pr: Option<PrInfo>, trigger: &str| {
            let e = items.entry(n).or_insert((None, None, Vec::new()));
            if issue.is_some() {
                e.0 = issue;
            }
            if pr.is_some() {
                e.1 = pr;
            }
            if !e.2.iter().any(|t| t == trigger) {
                e.2.push(trigger.to_string());
            }
        };
        let (assigned_numbers, issues_etag) = match assigned {
            Conditional::Modified { value, etag } => {
                let nums: Vec<u64> = value.iter().map(|i| i.number).collect();
                for i in value {
                    note(i.number, Some(i), None, "assigned");
                }
                (nums, Some(etag))
            }
            Conditional::NotModified => {
                for n in &rs.assigned_numbers {
                    note(*n, None, None, "assigned");
                }
                (rs.assigned_numbers.clone(), None)
            }
        };
        let (mentioned_numbers, mentioned_etag) = match mentioned {
            Conditional::Modified { value, etag } => {
                let nums: Vec<u64> = value.iter().map(|i| i.number).collect();
                for i in value {
                    note(i.number, Some(i), None, "mentioned");
                }
                (nums, Some(etag))
            }
            Conditional::NotModified => {
                for n in &rs.mentioned_numbers {
                    note(*n, None, None, "mentioned");
                }
                (rs.mentioned_numbers.clone(), None)
            }
        };
        let (review_numbers, pulls_etag) = match reviews {
            Conditional::Modified { value, etag } => {
                let nums: Vec<u64> = value.iter().map(|(i, _)| i.number).collect();
                for (i, pr) in value {
                    note(i.number, Some(i), Some(pr), "review_requested");
                }
                (nums, Some(etag))
            }
            Conditional::NotModified => {
                for n in &rs.review_numbers {
                    note(*n, None, None, "review_requested");
                }
                (rs.review_numbers.clone(), None)
            }
        };
        let (created_numbers, created_etag) = match created {
            Conditional::Modified { value, etag } => {
                let nums: Vec<u64> = value.iter().map(|i| i.number).collect();
                for i in value {
                    note(i.number, Some(i), None, "created");
                }
                (nums, Some(etag))
            }
            Conditional::NotModified => {
                for n in &rs.created_numbers {
                    note(*n, None, None, "created");
                }
                (rs.created_numbers.clone(), None)
            }
        };
        debug!(
            repo = repo.name,
            count = items.len(),
            "open items involving the bot"
        );

        let mut all_ok = true;
        let present: BTreeSet<u64> = items.keys().copied().collect();
        for (number, (fresh, pr, triggers)) in items {
            let tracked = self
                .state
                .repo_mut(&repo.name)
                .issues
                .get(&number)
                .map(|s| s.seeded && s.active)
                .unwrap_or(false);
            // An unchanged listing only matters for items we already handle,
            // and an item nothing binds to a session stays ignored until it
            // changes.
            let ignored = self.ignored.get(&(repo.name.clone(), number));
            if !tracked
                && ignored.is_some_and(|at| {
                    fresh.as_ref().is_none_or(|i| i.updated_at == *at)
                })
            {
                continue;
            }
            let issue = match fresh {
                Some(i) => i,
                None if tracked => continue,
                None => match self.gh.issue(owner, name, number).await {
                    Ok(i) => i,
                    Err(e) => {
                        all_ok = false;
                        self.note_failure(repo, number, &e);
                        continue;
                    }
                },
            };
            if let Err(e) = self
                .reconcile_issue(repo, owner, name, &issue, pr, triggers)
                .await
            {
                all_ok = false;
                self.note_failure(repo, issue.number, &e);
            } else {
                self.failures.remove(&(repo.name.clone(), issue.number));
            }
        }

        let stale: Vec<u64> = self
            .state
            .repo_mut(&repo.name)
            .issues
            .values()
            .filter(|s| s.active && !present.contains(&s.number))
            .map(|s| s.number)
            .collect();
        for number in stale {
            if let Err(e) = self.retire_issue(repo, owner, name, number).await {
                all_ok = false;
                warn!(repo = repo.name, issue = number, "retiring failed: {e:#}");
            }
        }

        // Only trust the ETags when every item was handled; otherwise the next
        // pass must see the full listings again to retry.
        let rs = self.state.repo_mut(&repo.name);
        if all_ok {
            if let Some(t) = issues_etag {
                rs.issues_etag = t;
                rs.assigned_numbers = assigned_numbers;
            }
            if let Some(t) = mentioned_etag {
                rs.mentioned_etag = t;
                rs.mentioned_numbers = mentioned_numbers;
            }
            if let Some(t) = pulls_etag {
                rs.pulls_etag = t;
                rs.review_numbers = review_numbers;
            }
            if let Some(t) = created_etag {
                rs.created_etag = t;
                rs.created_numbers = created_numbers;
            }
        } else {
            rs.issues_etag = None;
            rs.mentioned_etag = None;
            rs.pulls_etag = None;
            rs.created_etag = None;
        }
        Ok(())
    }

    fn note_failure(&mut self, repo: &RepoConfig, number: u64, err: &anyhow::Error) {
        let key = (repo.name.clone(), number);
        let count = self.failures.entry(key).or_insert(0);
        *count += 1;
        warn!(
            repo = repo.name,
            issue = number,
            attempt = *count,
            "handling issue failed: {err:#}"
        );
        if *count >= MAX_DELIVERY_FAILURES {
            error!(
                repo = repo.name,
                issue = number,
                "giving up on the current workspace binding; the issue will be re-onboarded"
            );
            *count = 0;
            if let Some(st) = self.state.repo_mut(&repo.name).issues.get_mut(&number) {
                st.seeded = false;
                st.terminal_handle = None;
            }
        }
    }

    fn ctx<'a>(&'a self, repo: &'a RepoConfig, st: &'a IssueState) -> PromptContext<'a> {
        // The repository's prompt file is read from the item's own checkout,
        // so a PR branch that changes it is seen as the branch has it.
        let project_prompt = st
            .worktree_path
            .as_deref()
            .and_then(|p| ProjectPrompt::load(repo, Path::new(p)));
        PromptContext {
            repo,
            daemon: &self.cfg.daemon,
            bot_login: &self.login,
            pr: st.pr.as_ref(),
            triggers: &st.triggers,
            owner: st.shares_workspace_of,
            delegated_by: st.delegated_by.as_deref(),
            projects: &st.projects,
            project_prompt,
        }
    }

    /// The full first message for an item, built once its workspace is
    /// known so the prompt file in that checkout can be included.
    fn initial_text(&mut self, repo: &RepoConfig, issue: &Issue, rendered: &[Rendered]) -> String {
        let snapshot = self.entry(repo, issue.number).clone();
        let ctx = self.ctx(repo, &snapshot);
        prompt::initial_prompt(issue, rendered, &ctx)
    }

    /// Refresh which project boards the item is on. Best effort: a failed
    /// lookup (no `project` scope, GraphQL hiccup) keeps whatever was known
    /// and is logged, since the prompt is still useful without it.
    async fn refresh_projects(&mut self, repo: &RepoConfig, owner: &str, name: &str, number: u64) {
        match self.gh.project_items(owner, name, number).await {
            Ok(projects) => self.entry(repo, number).projects = projects,
            Err(e) => warn!(
                repo = repo.name,
                issue = number,
                "could not look up project boards: {e:#}"
            ),
        }
    }

    /// The session that acts on an item: the item's own, or the one it is
    /// bound to (following a chain of bindings, which is normally one hop).
    fn owner_of(&self, repo: &RepoConfig, number: u64) -> u64 {
        match self.state.repos.get(&repo.name) {
            Some(rs) => owner_in(&rs.issues, number),
            None => number,
        }
    }

    /// Active items bound to `number`'s session.
    fn active_dependents(&self, repo: &RepoConfig, number: u64) -> Vec<u64> {
        self.state
            .repos
            .get(&repo.name)
            .map(|rs| {
                rs.issues
                    .values()
                    .filter(|s| s.active && s.shares_workspace_of == Some(number))
                    .map(|s| s.number)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Copy the owner's workspace and harness details onto an item bound to
    /// it, so status and delivery records agree.
    fn mirror_owner(&mut self, repo: &RepoConfig, number: u64, owner: u64) {
        let o = self.entry(repo, owner).clone();
        let e = self.entry(repo, number);
        e.worktree_id = o.worktree_id;
        e.worktree_path = o.worktree_path;
        e.repo_id = o.repo_id;
        e.branch = o.branch;
        e.terminal_handle = o.terminal_handle;
        e.agent_session_id = o.agent_session_id;
        e.launched_at = o.launched_at;
        e.cleanup_pending = false;
    }

    /// The session an item being discovered belongs to, if any. First the
    /// origin tag in its body (the session that opened it, unless that was
    /// a hand-off), then, for a same-repo pull request, the session whose
    /// workspace is on the PR's branch. A retired session still counts: it
    /// is brought back rather than duplicated.
    fn find_owner(
        &self,
        repo: &RepoConfig,
        issue: &Issue,
        pr: Option<&PrInfo>,
        scan: &origin::Scan,
    ) -> Option<u64> {
        let issues = &self.state.repos.get(&repo.name)?.issues;
        if let Some(tag) = &scan.origin_tag {
            if tag.is_delegate() {
                return None;
            }
            if !tag.origin.repo.eq_ignore_ascii_case(&repo.name) {
                info!(
                    repo = repo.name,
                    issue = issue.number,
                    origin = %tag.origin,
                    "opened from a session on another repository; not binding to it"
                );
            } else if tag.origin.number != issue.number {
                match issues.get(&tag.origin.number).filter(|s| s.seeded) {
                    Some(o) => return Some(owner_in(issues, o.number)),
                    None => warn!(
                        repo = repo.name,
                        issue = issue.number,
                        origin = %tag.origin,
                        "opened from a session ssf does not know; not binding to it"
                    ),
                }
            }
        }
        let p = pr.filter(|p| p.same_repo(&repo.name) && !p.head_ref.is_empty())?;
        let head = format!("refs/heads/{}", p.head_ref);
        issues
            .values()
            .filter(|s| {
                s.seeded
                    && s.number != issue.number
                    && s.shares_workspace_of.is_none()
                    && s.branch.as_deref() == Some(head.as_str())
            })
            .max_by_key(|s| (s.active, s.bound_at.clone()))
            .map(|s| s.number)
    }

    /// Parse origin tags out of the item body and its timeline, and flag posts
    /// by the bot that carry none: the gh shim was not in effect in whichever
    /// session made them, so nothing can tell which session that was.
    fn record_origins(&mut self, repo: &RepoConfig, issue: &Issue, timeline: &[Value]) -> origin::Scan {
        let scan = origin::scan(issue, timeline, &self.login);
        let login = self.login.clone();
        let e = self.entry(repo, issue.number);
        for (key, url) in &scan.untagged {
            if !e.untagged.contains_key(key) {
                warn!(
                    repo = repo.name,
                    issue = issue.number,
                    url,
                    "post by @{login} without an origin tag (gh shim not in effect)"
                );
            }
        }
        e.origin = scan.origin.clone();
        e.origins = scan.origins.clone();
        e.untagged = scan.untagged.clone();
        scan
    }

    fn diff(&self, seen: &BTreeMap<String, String>, timeline: &[Value]) -> Diff {
        let mut rendered = Vec::new();
        let mut observed = BTreeMap::new();
        for ev in timeline {
            let Some(key) = event_key(ev) else { continue };
            let kind = ev.get("event").and_then(Value::as_str).unwrap_or("");
            let marker = crate::github::value_str(ev, &["updated_at"])
                .filter(|_| kind == "commented")
                .unwrap_or("")
                .to_string();
            let previous = seen.get(&key);
            let is_new = previous.is_none();
            let edited = previous.is_some_and(|m| !m.is_empty() && *m != marker);
            observed.insert(key.clone(), marker.clone());
            if !is_new && !edited {
                continue;
            }
            // The bot's own activity (its comments, commits, PRs) would only
            // echo the agent's work back at it. Things done *to* the bot,
            // like being assigned, always count.
            let own = actor_of(ev).eq_ignore_ascii_case(&self.login);
            let echo = matches!(
                kind,
                "commented" | "cross-referenced" | "referenced" | "committed"
            );
            if own && echo && !self.cfg.daemon.include_own_events {
                debug!(key, "skipping bot's own event");
                continue;
            }
            if let Some(r) = render_event(ev, edited, &self.cfg.daemon, &self.login) {
                rendered.push(r);
            }
        }
        Diff {
            rendered,
            seen: observed,
        }
    }

    async fn reconcile_issue(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        issue: &Issue,
        pr: Option<PrInfo>,
        triggers: Vec<String>,
    ) -> Result<()> {
        let existing = self
            .state
            .repo_mut(&repo.name)
            .issues
            .get(&issue.number)
            .cloned();
        if let Some(st) = &existing {
            if st.seeded && st.triggers != triggers {
                let e = self.entry(repo, issue.number);
                e.triggers = triggers.clone();
            }
        }
        match existing {
            Some(st) if st.seeded && !st.active => {
                self.reactivate(repo, owner, name, issue, st).await
            }
            Some(st) if st.seeded => {
                if st.updated_at.as_deref() == Some(issue.updated_at.as_str()) {
                    return Ok(());
                }
                self.follow_up(repo, owner, name, issue, st).await
            }
            _ => self.onboard(repo, owner, name, issue, pr, triggers).await,
        }
    }

    fn entry(&mut self, repo: &RepoConfig, number: u64) -> &mut IssueState {
        let e = self
            .state
            .repo_mut(&repo.name)
            .issues
            .entry(number)
            .or_default();
        e.number = number;
        e
    }

    fn remember_worktree(&mut self, repo: &RepoConfig, number: u64, wt: &Worktree) {
        let e = self.entry(repo, number);
        e.worktree_id = Some(wt.id.clone());
        e.worktree_path = Some(wt.path.clone());
        e.repo_id = wt.id.split_once("::").map(|(r, _)| r.to_string());
        if wt.branch.is_some() {
            e.branch = wt.branch.clone();
        }
        e.cleanup_pending = false;
    }

    /// First contact: make sure the project and a workspace exist, then send
    /// the full context to a fresh agent. Pull requests get a workspace on
    /// their own branch, or share the workspace of the issue that produced
    /// the branch.
    async fn onboard(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        issue: &Issue,
        pr: Option<PrInfo>,
        triggers: Vec<String>,
    ) -> Result<()> {
        let is_pr = issue.is_pull_request();
        info!(
            repo = repo.name,
            issue = issue.number,
            title = issue.title,
            pr = is_pr,
            ?triggers,
            "onboarding"
        );
        let setup = self
            .orca
            .ensure_project(
                owner,
                name,
                &repo.clone_url(),
                repo.path.as_deref(),
                &self.cfg.projects_dir(),
            )
            .await?;
        let pr = match (is_pr, pr) {
            (true, Some(p)) => Some(p),
            (true, None) => Some(self.gh.pull(owner, name, issue.number).await?),
            (false, _) => None,
        };
        let timeline = self.gh.timeline(owner, name, issue.number).await?;
        let diff = self.diff(&BTreeMap::new(), &timeline);

        let prior = self
            .state
            .repo_mut(&repo.name)
            .issues
            .get(&issue.number)
            .cloned();
        let wt_name = prior
            .as_ref()
            .and_then(|s| s.worktree_name.clone())
            .unwrap_or_else(|| prompt::worktree_name_for(issue.number, &issue.title, is_pr));
        {
            let e = self.entry(repo, issue.number);
            e.title = issue.title.clone();
            e.html_url = issue.html_url.clone();
            e.repo_id = Some(setup.repo_id.clone());
            e.worktree_name = Some(wt_name.clone());
            e.kind = Some(if is_pr {
                "pull_request".into()
            } else {
                "issue".into()
            });
            e.triggers = triggers.clone();
            e.github_state = Some(github_state(issue, pr.as_ref(), false));
            e.pr = pr.clone();
            e.shares_workspace_of = None;
            e.delegated_by = None;
            e.parent_notified = false;
        }
        let scan = self.record_origins(repo, issue, &timeline);
        let by_bot = issue.author().eq_ignore_ascii_case(&self.login);

        // Who acts on this item. An item a session opened belongs to that
        // session (first binding wins: it never spawns a second one), unless
        // the session handed it off, in which case it gets its own session
        // and the parent hears about it once, when it closes.
        if let Some(tag) = scan.origin_tag.as_ref().filter(|t| t.is_delegate() && by_bot) {
            info!(
                repo = repo.name,
                issue = issue.number,
                parent = %tag.origin,
                "handed off by a session; starting its own"
            );
            self.entry(repo, issue.number).delegated_by = Some(tag.origin.to_string());
        } else if let Some(owner) = self.find_owner(repo, issue, pr.as_ref(), &scan) {
            return self.bind_to(repo, issue, owner, diff, triggers).await;
        } else if triggers.iter().all(|t| t == "created") {
            // Opened by the bot, but from nowhere ssf can name and with no
            // human asking for it: not worth a session.
            info!(
                repo = repo.name,
                issue = issue.number,
                "opened by the bot without a usable origin tag; ignoring until it changes"
            );
            self.ignored.insert(
                (repo.name.clone(), issue.number),
                issue.updated_at.clone(),
            );
            return Ok(());
        }
        self.refresh_projects(repo, owner, name, issue.number).await;

        let mut existing: Option<Worktree> = None;
        if let Some(id) = prior.as_ref().and_then(|s| s.worktree_id.clone()) {
            if self.orca.worktree_exists(&id).await? {
                existing = Some(Worktree {
                    id,
                    path: prior
                        .as_ref()
                        .and_then(|s| s.worktree_path.clone())
                        .unwrap_or_default(),
                    branch: prior.as_ref().and_then(|s| s.branch.clone()),
                });
            }
        }
        if existing.is_none() {
            existing = self
                .orca
                .find_worktree_for_issue(&setup.repo_id, issue.number)
                .await?;
        }

        let comment = format!(
            "ssf: bound to {} #{}",
            if is_pr { "PR" } else { "issue" },
            issue.number
        );
        let handle = match existing {
            Some(wt) => {
                info!(
                    repo = repo.name,
                    issue = issue.number,
                    worktree = wt.id,
                    "reusing existing workspace"
                );
                self.remember_worktree(repo, issue.number, &wt);
                let _ = self.orca.set_comment(&wt.id, &comment).await;
                let text = self.initial_text(repo, issue, &diff.rendered);
                let d = self.deliver_to(repo, issue.number, &text, None).await?;
                d.handle
            }
            None => {
                let created = self
                    .create_workspace(
                        repo,
                        &setup.repo_id,
                        &wt_name,
                        issue.number,
                        &comment,
                        pr.as_ref(),
                    )
                    .await?;
                info!(
                    repo = repo.name,
                    issue = issue.number,
                    worktree = created.id,
                    "created workspace"
                );
                self.remember_worktree(repo, issue.number, &created);
                {
                    let e = self.entry(repo, issue.number);
                    e.launched_at = Some(now_iso());
                    e.agent_session_id = None;
                }
                let title = format!("{} · #{}", repo.harness, issue.number);
                let cmd = self.launch_command(
                    repo,
                    issue.number,
                    &issue.html_url,
                    &repo.harness_command(),
                );
                let handle = self
                    .orca
                    .launch_in_worktree(&created.id, &cmd, &title, &repo.harness)
                    .await?;
                let text = self.initial_text(repo, issue, &diff.rendered);
                self.orca.send_prompt(&handle, &text).await?;
                info!(
                    repo = repo.name,
                    issue = issue.number,
                    handle,
                    "launched {} and sent the {}",
                    repo.harness,
                    if is_pr { "pull request" } else { "issue" }
                );
                handle
            }
        };
        if let Some(id) = self.entry(repo, issue.number).worktree_id.clone() {
            let _ = self.orca.set_status(&id, "in-progress").await;
        }

        let e = self.entry(repo, issue.number);
        e.terminal_handle = Some(handle);
        e.updated_at = Some(issue.updated_at.clone());
        e.seen = diff.seen;
        e.seeded = true;
        e.active = true;
        e.bound_at = Some(now_iso());
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        // The session file appears once the first message is processed.
        tokio::time::sleep(Duration::from_secs(3)).await;
        self.capture_sessions(repo);
        Ok(())
    }

    /// Bind an item to another item's session and tell that agent about it.
    /// The owner is brought back first if it was retired or its workspace is
    /// gone (delivery does that), and is kept from being cleaned up while it
    /// has active dependents.
    async fn bind_to(
        &mut self,
        repo: &RepoConfig,
        issue: &Issue,
        owner: u64,
        diff: Diff,
        triggers: Vec<String>,
    ) -> Result<()> {
        info!(
            repo = repo.name,
            issue = issue.number,
            owner,
            ?triggers,
            "bound to the owning session"
        );
        self.entry(repo, owner).cleanup_pending = false;
        {
            let e = self.entry(repo, issue.number);
            e.shares_workspace_of = Some(owner);
            e.triggers = triggers;
        }
        self.mirror_owner(repo, issue.number, owner);
        let snapshot = self.entry(repo, issue.number).clone();
        let ctx = self.ctx(repo, &snapshot);
        let text = prompt::tracked_prompt(issue, &diff.rendered, &ctx);
        let d = self.deliver_to(repo, issue.number, &text, None).await?;
        let e = self.entry(repo, issue.number);
        e.terminal_handle = Some(d.handle);
        e.updated_at = Some(issue.updated_at.clone());
        e.seen = diff.seen;
        e.seeded = true;
        e.active = true;
        e.bound_at = Some(now_iso());
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        Ok(())
    }

    /// The whole story of an item as its initial prompt would tell it, for
    /// a harness that starts from scratch and needs context for whatever is
    /// about to be delivered.
    async fn story(&mut self, repo: &RepoConfig, number: u64) -> Result<String> {
        let (owner, name) = repo.split()?;
        let issue = self.gh.issue(owner, name, number).await?;
        let timeline = self.gh.timeline(owner, name, number).await?;
        let all = self.diff(&BTreeMap::new(), &timeline).rendered;
        let st = self.entry(repo, number).clone();
        let ctx = self.ctx(repo, &st);
        Ok(prompt::initial_prompt(&issue, &all, &ctx))
    }

    /// Tell the session that handed an item off that it has closed, with the
    /// last comment its agent left. Best effort, and never worth rebuilding a
    /// retired parent's workspace for.
    async fn notify_parent(
        &mut self,
        repo: &RepoConfig,
        issue: &Issue,
        merged: bool,
        timeline: &[Value],
        parent: &str,
    ) {
        let Some(origin) = Origin::parse(parent) else {
            warn!(repo = repo.name, issue = issue.number, parent, "unparseable parent session");
            return;
        };
        let Some(prepo) = self
            .cfg
            .repos
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(&origin.repo))
            .cloned()
        else {
            warn!(
                repo = repo.name,
                issue = issue.number,
                parent,
                "parent session's repository is not watched; not notifying"
            );
            return;
        };
        let known = self
            .state
            .repos
            .get(&prepo.name)
            .and_then(|rs| rs.issues.get(&origin.number))
            .is_some_and(|s| s.seeded);
        if !known {
            warn!(
                repo = repo.name,
                issue = issue.number,
                parent,
                "parent session is unknown to ssf; not notifying"
            );
            return;
        }
        let owner = self.owner_of(&prepo, origin.number);
        let ost = self.entry(&prepo, owner).clone();
        let alive = match ost.worktree_id.as_deref() {
            Some(id) => self.orca.worktree_exists(id).await.unwrap_or(false),
            None => false,
        };
        if !ost.active && !alive {
            info!(
                repo = repo.name,
                issue = issue.number,
                parent,
                "parent session is retired and its workspace gone; not notifying"
            );
            return;
        }
        let last = last_bot_comment(timeline, &self.login);
        let cst = self.entry(repo, issue.number).clone();
        let ctx = self.ctx(repo, &cst);
        let text = prompt::delegated_closed_prompt(issue, merged, last.as_ref(), &ctx);
        match self.deliver_to(&prepo, origin.number, &text, None).await {
            Ok(d) => {
                info!(repo = repo.name, issue = issue.number, parent, "told the parent session");
                let e = self.entry(&prepo, origin.number);
                e.terminal_handle = Some(d.handle);
                e.last_prompt_at = Some(now_iso());
                e.prompts_sent += 1;
            }
            Err(e) => warn!(
                repo = repo.name,
                issue = issue.number,
                parent,
                "could not tell the parent session: {e:#}"
            ),
        }
    }

    /// Create the git worktree for an item. Pull requests from this repo are
    /// checked out on their head branch so pushes update the PR; anything
    /// else starts from the configured base.
    async fn create_workspace(
        &mut self,
        repo: &RepoConfig,
        repo_id: &str,
        wt_name: &str,
        number: u64,
        comment: &str,
        pr: Option<&PrInfo>,
    ) -> Result<Worktree> {
        let mut base = repo.base_branch.clone();
        let mut checkout: Option<String> = None;
        if let Some(p) = pr.filter(|p| p.same_repo(&repo.name) && !p.head_ref.is_empty()) {
            let main = self.orca.repo_path(repo_id).await?;
            if let Err(e) = git(&main, &["fetch", "origin", &p.head_ref]).await {
                warn!(
                    repo = repo.name,
                    issue = number,
                    "fetching PR branch failed: {e:#}"
                );
            }
            if self
                .orca
                .existing_branch_ref(repo_id, &p.head_ref)
                .await?
                .is_some()
            {
                base = Some(format!("origin/{}", p.head_ref));
                checkout = Some(p.head_ref.clone());
            }
        }
        let created = match self
            .orca
            .create_worktree(repo_id, wt_name, number, None, comment, base.as_deref())
            .await
        {
            Ok(w) => w,
            Err(e) if checkout.is_some() => {
                warn!(
                    repo = repo.name,
                    issue = number,
                    "creating from the PR branch failed ({e:#}); using the default base"
                );
                checkout = None;
                self.orca
                    .create_worktree(
                        repo_id,
                        wt_name,
                        number,
                        None,
                        comment,
                        repo.base_branch.as_deref(),
                    )
                    .await?
            }
            Err(e) => return Err(e),
        };
        let mut created = created;
        if let Some(branch) = checkout {
            match checkout_branch(&created.path, &branch).await {
                Ok(()) => created.branch = Some(format!("refs/heads/{branch}")),
                Err(e) => warn!(
                    repo = repo.name,
                    issue = number,
                    branch,
                    "could not check out the PR branch: {e:#}"
                ),
            }
        }
        Ok(created)
    }

    /// The issue changed since we last looked: deliver whatever is new.
    async fn follow_up(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        issue: &Issue,
        st: IssueState,
    ) -> Result<()> {
        let timeline = self.gh.timeline(owner, name, issue.number).await?;
        self.record_origins(repo, issue, &timeline);
        let diff = self.diff(&st.seen, &timeline);
        if diff.rendered.is_empty() {
            debug!(
                repo = repo.name,
                issue = issue.number,
                "updated but nothing new to deliver"
            );
            let e = self.entry(repo, issue.number);
            e.updated_at = Some(issue.updated_at.clone());
            e.seen = diff.seen;
            e.title = issue.title.clone();
            e.github_state = Some(github_state(issue, st.pr.as_ref(), false));
            return Ok(());
        }
        info!(
            repo = repo.name,
            issue = issue.number,
            events = diff.rendered.len(),
            "delivering new activity"
        );
        self.refresh_projects(repo, owner, name, issue.number).await;
        let st = IssueState {
            projects: self.entry(repo, issue.number).projects.clone(),
            ..st
        };
        let ctx = self.ctx(repo, &st);
        let text = prompt::followup_prompt(issue, &diff.rendered, &ctx);
        // A harness started from scratch has lost its memory, so it gets the
        // whole story rather than just the delta.
        let mut all = self.diff(&BTreeMap::new(), &timeline).rendered;
        all.retain(|r| !diff.rendered.iter().any(|n| n.key == r.key));
        let mut relaunch_text = prompt::initial_prompt(issue, &all, &ctx);
        relaunch_text.push_str("\n\n");
        relaunch_text.push_str(&text);
        let d = self
            .deliver_to(repo, issue.number, &text, Some(&relaunch_text))
            .await?;
        let e = self.entry(repo, issue.number);
        e.updated_at = Some(issue.updated_at.clone());
        e.title = issue.title.clone();
        e.github_state = Some(github_state(issue, st.pr.as_ref(), false));
        e.seen = diff.seen;
        e.terminal_handle = Some(d.handle);
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        Ok(())
    }

    /// Previously retired issue is assigned to the bot again.
    async fn reactivate(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        issue: &Issue,
        st: IssueState,
    ) -> Result<()> {
        info!(
            repo = repo.name,
            issue = issue.number,
            "issue assigned again; reactivating"
        );
        let timeline = self.gh.timeline(owner, name, issue.number).await?;
        self.record_origins(repo, issue, &timeline);
        let diff = self.diff(&st.seen, &timeline);
        self.refresh_projects(repo, owner, name, issue.number).await;
        let st = IssueState {
            projects: self.entry(repo, issue.number).projects.clone(),
            ..st
        };
        let ctx = self.ctx(repo, &st);
        let text = prompt::reassigned_prompt(issue, &diff.rendered, &ctx);
        let all = self.diff(&BTreeMap::new(), &timeline).rendered;
        let relaunch_text = prompt::initial_prompt(issue, &all, &ctx);
        let d = self
            .deliver_to(repo, issue.number, &text, Some(&relaunch_text))
            .await?;
        if let Some(id) = self.entry(repo, issue.number).worktree_id.clone() {
            let _ = self.orca.set_status(&id, "in-progress").await;
        }
        let e = self.entry(repo, issue.number);
        e.updated_at = Some(issue.updated_at.clone());
        e.title = issue.title.clone();
        e.github_state = Some(github_state(issue, st.pr.as_ref(), false));
        e.seen = diff.seen;
        e.active = true;
        e.cleanup_pending = false;
        e.retired_at = None;
        e.terminal_handle = Some(d.handle);
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        Ok(())
    }

    /// Item left the set of open things involving the bot: tell the agent to stop.
    async fn retire_issue(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        number: u64,
    ) -> Result<()> {
        let st = self
            .state
            .repo_mut(&repo.name)
            .issues
            .get(&number)
            .cloned()
            .context("retiring unknown item")?;
        let issue = self.gh.issue(owner, name, number).await?;
        let closed = issue.state == "closed";
        let own = st.triggers.iter().any(|t| t == "created")
            && issue.author().eq_ignore_ascii_case(&self.login);
        if !closed && (issue.is_assigned_to(&self.login) || own) {
            // Listing lag: still assigned, still requested for review, or
            // still the bot's own open item.
            return Ok(());
        }
        let merged = closed
            && issue.is_pull_request()
            && match self.gh.pull(owner, name, number).await {
                Ok(info) => info.merged,
                Err(_) => st.pr.as_ref().is_some_and(|p| p.merged),
            };
        info!(
            repo = repo.name,
            issue = number,
            closed,
            "item no longer active for the bot"
        );
        let timeline = self.gh.timeline(owner, name, number).await?;
        self.record_origins(repo, &issue, &timeline);
        let diff = self.diff(&st.seen, &timeline);
        let ctx = self.ctx(repo, &st);
        let text = if closed {
            prompt::closed_prompt(&issue, &diff.rendered, &ctx)
        } else {
            prompt::unassigned_prompt(&issue, &diff.rendered, &ctx)
        };
        // Retirement is best-effort: a deleted workspace must not keep us
        // retrying, and it is not worth rebuilding one just to say goodbye.
        // An owned item's workspace is its owner's.
        let session = self.owner_of(repo, number);
        let workspace_alive = match self.entry(repo, session).worktree_id.clone() {
            Some(id) => self.orca.worktree_exists(&id).await.unwrap_or(false),
            None => false,
        };
        let handle = if workspace_alive {
            match self.deliver_to(repo, number, &text, None).await {
                Ok(d) => Some(d.handle),
                Err(e) => {
                    warn!(
                        repo = repo.name,
                        issue = number,
                        "could not notify agent: {e:#}"
                    );
                    None
                }
            }
        } else {
            None
        };
        let shared = st.shares_workspace_of.is_some();
        // The workspace is done with only when nothing bound to this session
        // is still open.
        let dependents = self.active_dependents(repo, number);
        let done = closed && workspace_alive && !shared && dependents.is_empty();
        if done {
            if let Some(id) = &st.worktree_id {
                let _ = self.orca.set_status(id, "completed").await;
            }
        }
        let cleanup = done && self.cfg.daemon.cleanup_on_close;
        let e = self.entry(repo, number);
        e.active = false;
        e.title = issue.title.clone();
        e.github_state = Some(github_state(&issue, st.pr.as_ref(), merged));
        e.updated_at = Some(issue.updated_at.clone());
        e.seen = diff.seen;
        e.retired_at = Some(now_iso());
        e.cleanup_pending = cleanup;
        if handle.is_some() {
            e.terminal_handle = handle;
            e.last_prompt_at = Some(now_iso());
            e.prompts_sent += 1;
        }
        if closed && !dependents.is_empty() {
            info!(
                repo = repo.name,
                issue = number,
                ?dependents,
                "closed, but its session still owns open items; keeping the workspace"
            );
        }
        // The last open item bound to a retired owner: the owner's workspace
        // can go now, with the usual grace period from this moment.
        if shared && workspace_alive {
            self.release_owner(repo, session).await;
        }
        if closed && !st.parent_notified {
            if let Some(parent) = st.delegated_by.clone() {
                self.notify_parent(repo, &issue, merged, &timeline, &parent)
                    .await;
                self.entry(repo, number).parent_notified = true;
            }
        }
        Ok(())
    }

    /// An owner that is itself retired and closed, and has no active
    /// dependents left, becomes eligible for cleanup.
    async fn release_owner(&mut self, repo: &RepoConfig, owner: u64) {
        let o = self.entry(repo, owner).clone();
        let closed = matches!(o.github_state.as_deref(), Some("closed" | "merged"));
        if o.active || !closed || !self.active_dependents(repo, owner).is_empty() {
            return;
        }
        info!(
            repo = repo.name,
            issue = owner,
            "retired session has no open items left; releasing its workspace"
        );
        if let Some(id) = &o.worktree_id {
            let _ = self.orca.set_status(id, "completed").await;
        }
        let cleanup = self.cfg.daemon.cleanup_on_close;
        let e = self.entry(repo, owner);
        e.retired_at = Some(now_iso());
        e.cleanup_pending = cleanup;
    }

    /// `ssf launch ...` wrapper that puts the bot credentials and issue
    /// identity into the harness's environment. The daemon's own config and
    /// state locations are passed along so the wrapper reads the same files.
    fn launch_command(&self, repo: &RepoConfig, number: u64, url: &str, inner: &str) -> String {
        let me = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "ssf".to_string());
        let mut prefix = String::new();
        for var in ["SSF_CONFIG_DIR", "SSF_STATE_DIR", "SSF_GITHUB_TOKEN"] {
            if let Ok(v) = std::env::var(var) {
                prefix.push_str(&format!("{var}={} ", shell_quote(&v)));
            }
        }
        format!(
            "{prefix}{} launch --repo {} --issue {} --issue-url {} -- {}",
            shell_quote(&me),
            shell_quote(&repo.name),
            number,
            shell_quote(url),
            shell_quote(inner)
        )
    }

    /// Deliver a prompt to the agent that acts on an item (its own session,
    /// or its owner's), bringing the workspace and the agent back first if
    /// either is gone. A harness that has to start from scratch gets
    /// `relaunch_text` instead, or, when that is missing or the prompt is
    /// about another session's item, the session's own story followed by
    /// the prompt.
    async fn deliver_to(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        text: &str,
        relaunch_text: Option<&str>,
    ) -> Result<Delivery> {
        let target = self.owner_of(repo, number);
        let st = self.entry(repo, target).clone();
        let alive = match st.worktree_id.as_deref() {
            Some(id) => self.orca.worktree_exists(id).await?,
            None => false,
        };
        if !alive {
            self.rehydrate(repo, target).await?;
        }
        let st = self.entry(repo, target).clone();
        let worktree_id = st
            .worktree_id
            .clone()
            .context("issue has no workspace bound")?;
        let live = alive && self.orca.has_live_agent(&worktree_id).await.unwrap_or(false);
        let mut story = None;
        if !live && (target != number || relaunch_text.is_none()) {
            match self.story(repo, target).await {
                Ok(s) => story = Some(format!("{s}\n\n{text}")),
                Err(e) => warn!(
                    repo = repo.name,
                    issue = target,
                    "could not assemble the session's story for a fresh harness: {e:#}"
                ),
            }
        }
        let relaunch_text = story.as_deref().or(relaunch_text);
        let title = format!("{} · #{}", repo.harness, target);
        let resume = st
            .agent_session_id
            .as_deref()
            .and_then(|id| sessions::resume_command(&repo.harness, &repo.harness_command(), id))
            .map(|c| self.launch_command(repo, st.number, &st.html_url, &c));
        let relaunch = self.launch_command(repo, st.number, &st.html_url, &repo.harness_command());
        let d = self
            .orca
            .deliver(
                &worktree_id,
                st.terminal_handle.as_deref(),
                &relaunch,
                resume.as_deref(),
                &repo.harness,
                &title,
                text,
                relaunch_text,
            )
            .await?;
        if d.relaunched {
            let e = self.entry(repo, target);
            e.launched_at = Some(now_iso());
            if !d.resumed {
                e.agent_session_id = None;
            }
            info!(
                repo = repo.name,
                issue = target,
                resumed = d.resumed,
                "harness relaunched"
            );
        }
        self.entry(repo, target).terminal_handle = Some(d.handle.clone());
        if target != number {
            self.mirror_owner(repo, number, target);
        }
        Ok(d)
    }

    /// Re-create the workspace for an issue whose Orca worktree is gone,
    /// starting from its old branch when that still exists.
    async fn rehydrate(&mut self, repo: &RepoConfig, number: u64) -> Result<()> {
        let st = self.entry(repo, number).clone();
        let repo_id = match st.repo_id.clone() {
            Some(r) => r,
            None => {
                let (owner, name) = repo.split()?;
                self.orca
                    .ensure_project(
                        owner,
                        name,
                        &repo.clone_url(),
                        repo.path.as_deref(),
                        &self.cfg.projects_dir(),
                    )
                    .await?
                    .repo_id
            }
        };
        if let Some(existing) = self.orca.find_worktree_for_issue(&repo_id, number).await? {
            info!(
                repo = repo.name,
                issue = number,
                worktree = existing.id,
                "found workspace linked to the issue"
            );
            self.remember_worktree(repo, number, &existing);
            let e = self.entry(repo, number);
            e.terminal_handle = None;
            return Ok(());
        }
        let name = st
            .worktree_name
            .clone()
            .unwrap_or_else(|| prompt::worktree_name(number, &st.title));
        let comment = format!("ssf: bound to issue #{number} (re-created)");
        let mut base = repo.base_branch.clone();
        if let Some(branch) = st.branch.as_deref() {
            let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
            match self.orca.existing_branch_ref(&repo_id, short).await {
                Ok(Some(r)) => base = Some(r),
                Ok(None) => debug!(
                    repo = repo.name,
                    issue = number,
                    branch = short,
                    "old branch is gone"
                ),
                Err(e) => warn!(
                    repo = repo.name,
                    issue = number,
                    "could not check old branch: {e:#}"
                ),
            }
        }
        info!(
            repo = repo.name,
            issue = number,
            name,
            base = base.as_deref().unwrap_or("default"),
            "re-creating workspace"
        );
        let created = match self
            .orca
            .create_worktree(&repo_id, &name, number, None, &comment, base.as_deref())
            .await
        {
            Ok(w) => w,
            Err(e) if base.is_some() => {
                warn!(
                    repo = repo.name,
                    issue = number,
                    "re-create from old branch failed ({e:#}); using default base"
                );
                self.orca
                    .create_worktree(
                        &repo_id,
                        &name,
                        number,
                        None,
                        &comment,
                        repo.base_branch.as_deref(),
                    )
                    .await?
            }
            Err(e) => return Err(e),
        };
        let mut created = created;
        if let Some(branch) = st
            .branch
            .as_deref()
            .map(|b| b.strip_prefix("refs/heads/").unwrap_or(b).to_string())
        {
            if base.is_some() {
                match checkout_branch(&created.path, &branch).await {
                    Ok(()) => created.branch = Some(format!("refs/heads/{branch}")),
                    Err(e) => warn!(
                        repo = repo.name,
                        issue = number,
                        branch,
                        "could not check out the old branch: {e:#}"
                    ),
                }
            }
        }
        self.remember_worktree(repo, number, &created);
        let e = self.entry(repo, number);
        e.terminal_handle = None;
        e.worktree_name = Some(name);
        Ok(())
    }

    /// Record harness session ids for workspaces that do not have one yet.
    fn capture_sessions(&mut self, repo: &RepoConfig) {
        if !sessions::supports_resume(&repo.harness) {
            return;
        }
        let rs = self.state.repo_mut(&repo.name);
        for st in rs.issues.values_mut() {
            if st.agent_session_id.is_some() || st.worktree_id.is_none() {
                continue;
            }
            let (Some(path), Some(launched)) =
                (st.worktree_path.as_deref(), st.launched_at.as_deref())
            else {
                continue;
            };
            let since = chrono::DateTime::parse_from_rfc3339(launched)
                .map(|t| SystemTime::from(t) - Duration::from_secs(5))
                .unwrap_or(SystemTime::UNIX_EPOCH);
            if let Some(id) = sessions::capture(&repo.harness, path, since) {
                info!(
                    repo = repo.name,
                    issue = st.number,
                    session = id,
                    "captured {} session",
                    repo.harness
                );
                st.agent_session_id = Some(id);
            }
        }
        let owned: Vec<(u64, u64)> = rs
            .issues
            .values()
            .filter_map(|s| s.shares_workspace_of.map(|o| (s.number, o)))
            .collect();
        for (number, owner) in owned {
            if let Some(id) = rs.issues.get(&owner).and_then(|o| o.agent_session_id.clone()) {
                if let Some(s) = rs.issues.get_mut(&number) {
                    s.agent_session_id = Some(id);
                }
            }
        }
    }

    /// Remove workspaces of closed issues once their agent has wrapped up.
    async fn run_cleanups(&mut self, repo: &RepoConfig) {
        let pending: Vec<IssueState> = self
            .state
            .repo_mut(&repo.name)
            .issues
            .values()
            .filter(|s| s.cleanup_pending && !s.active)
            .cloned()
            .collect();
        for st in pending {
            if st.shares_workspace_of.is_some() {
                let e = self.entry(repo, st.number);
                e.cleanup_pending = false;
                continue;
            }
            if !self.active_dependents(repo, st.number).is_empty() {
                debug!(
                    repo = repo.name,
                    issue = st.number,
                    "cleanup waiting: session still owns open items"
                );
                continue;
            }
            let Some(id) = st.worktree_id.clone() else {
                self.entry(repo, st.number).cleanup_pending = false;
                continue;
            };
            let exists = self.orca.worktree_exists(&id).await.unwrap_or(true);
            let grace = Duration::from_secs(self.cfg.daemon.cleanup_grace_secs);
            let retired = st
                .retired_at
                .as_deref()
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .map(SystemTime::from)
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let overdue = SystemTime::now()
                .duration_since(retired)
                .unwrap_or_default()
                > grace;
            if exists {
                // Give the agent its wrap-up time; a session id must be known
                // so the conversation can be resumed later, unless we've waited
                // long enough anyway.
                let busy = self.orca.agent_busy(&id).await.unwrap_or(false);
                let resumable =
                    st.agent_session_id.is_some() || !sessions::supports_resume(&repo.harness);
                if (busy || !resumable) && !overdue {
                    debug!(
                        repo = repo.name,
                        issue = st.number,
                        busy,
                        resumable,
                        "cleanup waiting"
                    );
                    continue;
                }
                match self.orca.remove_worktree(&id).await {
                    Ok(()) => info!(
                        repo = repo.name,
                        issue = st.number,
                        worktree = id,
                        "removed workspace of closed issue"
                    ),
                    Err(e) => {
                        warn!(
                            repo = repo.name,
                            issue = st.number,
                            "removing workspace failed: {e:#}"
                        );
                        if !overdue {
                            continue;
                        }
                    }
                }
            }
            let e = self.entry(repo, st.number);
            e.cleanup_pending = false;
            e.worktree_id = None;
            e.worktree_path = None;
            e.terminal_handle = None;
        }
    }
}

/// Follow `shares_workspace_of` to the session that acts on `number`.
fn owner_in(issues: &BTreeMap<u64, IssueState>, number: u64) -> u64 {
    let mut cur = number;
    let mut seen = BTreeSet::new();
    while let Some(next) = issues.get(&cur).and_then(|s| s.shares_workspace_of) {
        if next == cur || !seen.insert(cur) {
            break;
        }
        cur = next;
    }
    cur
}

/// The last comment the bot left on an item, as its session's final word.
fn last_bot_comment(timeline: &[Value], bot: &str) -> Option<FinalComment> {
    timeline
        .iter()
        .rev()
        .filter(|ev| ev.get("event").and_then(Value::as_str) == Some("commented"))
        .find(|ev| actor_of(ev).eq_ignore_ascii_case(bot))
        .map(|ev| {
            let body = crate::github::value_str(ev, &["body"]).unwrap_or("");
            FinalComment {
                author: actor_of(ev),
                session: origin::parse(body).map(|t| t.origin.to_string()),
                url: crate::github::value_str(ev, &["html_url"])
                    .unwrap_or("")
                    .to_string(),
                body: origin::strip(body),
            }
        })
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Run git in `path`, returning stdout.
async fn git(path: &str, args: &[&str]) -> Result<String> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .await
        .context("running git")?;
    if !out.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Switch a fresh worktree onto `branch`, tracking `origin/branch` when it
/// exists, so the agent's pushes land where the pull request lives.
async fn checkout_branch(path: &str, branch: &str) -> Result<()> {
    let _ = git(path, &["fetch", "origin", branch]).await;
    let remote = git(
        path,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{branch}"),
        ],
    )
    .await
    .is_ok();
    if remote {
        git(
            path,
            &[
                "checkout",
                "-B",
                branch,
                "--track",
                &format!("origin/{branch}"),
            ],
        )
        .await?;
    } else {
        git(path, &["checkout", branch]).await?;
    }
    Ok(())
}

/// `open`, `closed` or `merged`, as `ssf status` reports it.
fn github_state(issue: &Issue, pr: Option<&PrInfo>, merged: bool) -> String {
    if merged || pr.is_some_and(|p| p.merged) {
        "merged".into()
    } else if issue.state == "closed" {
        "closed".into()
    } else {
        "open".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn engine() -> Engine {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Engine {
            cfg: Config::default(),
            gh: GitHub::new("https://api.github.invalid", "t").unwrap(),
            orca: Orca::new(Default::default()),
            login: "bot".into(),
            state: State::default(),
            failures: BTreeMap::new(),
            ignored: BTreeMap::new(),
        }
    }

    fn repo() -> RepoConfig {
        RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            ..Default::default()
        }
    }

    fn issue(number: u64, author: &str, body: Option<&str>) -> Issue {
        serde_json::from_value(json!({
            "number": number, "title": "t", "body": body, "html_url": format!("https://gh/{number}"),
            "state": "open", "user": {"login": author}, "created_at": "x", "updated_at": "x"
        }))
        .unwrap()
    }

    fn seeded(e: &mut Engine, number: u64, branch: Option<&str>, active: bool) {
        let st = e.entry(&repo(), number);
        st.seeded = true;
        st.active = active;
        st.branch = branch.map(|b| format!("refs/heads/{b}"));
        st.bound_at = Some(format!("2026-01-0{}T00:00:00Z", number));
    }

    fn pr(head: &str) -> PrInfo {
        PrInfo {
            head_ref: head.into(),
            head_repo: "o/r".into(),
            base_ref: "main".into(),
            ..Default::default()
        }
    }

    #[test]
    fn owner_follows_bindings_and_survives_cycles() {
        let mut issues: BTreeMap<u64, IssueState> = BTreeMap::new();
        for (n, o) in [(1, None), (2, Some(1)), (3, Some(2)), (4, Some(5)), (5, Some(4))] {
            issues.insert(
                n,
                IssueState {
                    number: n,
                    shares_workspace_of: o,
                    ..Default::default()
                },
            );
        }
        assert_eq!(owner_in(&issues, 3), 1);
        assert_eq!(owner_in(&issues, 1), 1);
        assert_eq!(owner_in(&issues, 9), 9, "unknown items own themselves");
        let cyclic = owner_in(&issues, 4);
        assert!(cyclic == 4 || cyclic == 5);
    }

    #[test]
    fn origin_tag_binds_to_the_opening_session() {
        let mut e = engine();
        let r = repo();
        seeded(&mut e, 1, Some("bot/issue-1"), true);
        let tagged = issue(7, "bot", Some("child\n\n<!-- ssf: origin=o/r#1 -->"));
        let scan = origin::scan(&tagged, &[], "bot");
        assert_eq!(e.find_owner(&r, &tagged, None, &scan), Some(1));
        // Through a chain: the PR was bound to the issue, a comment-opened
        // issue from the PR's session lands on the issue's session too.
        e.entry(&r, 7).seeded = true;
        e.entry(&r, 7).shares_workspace_of = Some(1);
        let grandchild = issue(8, "bot", Some("<!-- ssf: origin=o/r#7 -->"));
        let scan = origin::scan(&grandchild, &[], "bot");
        assert_eq!(e.find_owner(&r, &grandchild, None, &scan), Some(1));
        // A hand-off is not bound.
        let delegated = issue(9, "bot", Some("<!-- ssf: origin=o/r#1 mode=delegate -->"));
        let scan = origin::scan(&delegated, &[], "bot");
        assert_eq!(e.find_owner(&r, &delegated, None, &scan), None);
        assert!(scan.origin_tag.unwrap().is_delegate());
        // A human's body with a pasted tag is not an origin.
        let human = issue(10, "alice", Some("<!-- ssf: origin=o/r#1 -->"));
        let scan = origin::scan(&human, &[], "bot");
        assert_eq!(e.find_owner(&r, &human, None, &scan), None);
        // Another repository's session, or one ssf never tracked: no binding.
        let elsewhere = issue(11, "bot", Some("<!-- ssf: origin=x/y#1 -->"));
        let scan = origin::scan(&elsewhere, &[], "bot");
        assert_eq!(e.find_owner(&r, &elsewhere, None, &scan), None);
        let unknown = issue(12, "bot", Some("<!-- ssf: origin=o/r#99 -->"));
        let scan = origin::scan(&unknown, &[], "bot");
        assert_eq!(e.find_owner(&r, &unknown, None, &scan), None);
    }

    #[test]
    fn pull_request_branch_binds_to_the_workspace_on_it() {
        let mut e = engine();
        let r = repo();
        seeded(&mut e, 1, Some("bot/fix"), false);
        seeded(&mut e, 2, Some("bot/fix"), true);
        seeded(&mut e, 3, Some("bot/other"), true);
        let untagged = issue(7, "bot", None);
        let scan = origin::scan(&untagged, &[], "bot");
        assert_eq!(
            e.find_owner(&r, &untagged, Some(&pr("bot/fix")), &scan),
            Some(2),
            "an active session beats a retired one"
        );
        assert_eq!(e.find_owner(&r, &untagged, Some(&pr("nobody")), &scan), None);
        assert_eq!(e.find_owner(&r, &untagged, None, &scan), None);
        // A retired session on the branch is still the owner (it gets
        // rehydrated) rather than duplicated.
        e.entry(&r, 2).active = false;
        e.entry(&r, 2).branch = None;
        assert_eq!(
            e.find_owner(&r, &untagged, Some(&pr("bot/fix")), &scan),
            Some(1)
        );
        // A fork's branch name means nothing here.
        let mut fork = pr("bot/fix");
        fork.head_repo = "someone/r".into();
        assert_eq!(e.find_owner(&r, &untagged, Some(&fork), &scan), None);
        // An unknown origin tag falls back to the branch.
        let tagged = issue(8, "bot", Some("<!-- ssf: origin=o/r#99 -->"));
        let scan = origin::scan(&tagged, &[], "bot");
        assert_eq!(e.find_owner(&r, &tagged, Some(&pr("bot/fix")), &scan), Some(1));
    }

    #[test]
    fn dependents_and_mirroring() {
        let mut e = engine();
        let r = repo();
        seeded(&mut e, 1, Some("bot/fix"), true);
        {
            let o = e.entry(&r, 1);
            o.worktree_id = Some("repo::/w/1".into());
            o.worktree_path = Some("/w/1".into());
            o.agent_session_id = Some("sess".into());
            o.terminal_handle = Some("h1".into());
        }
        seeded(&mut e, 2, None, true);
        e.entry(&r, 2).shares_workspace_of = Some(1);
        seeded(&mut e, 3, None, false);
        e.entry(&r, 3).shares_workspace_of = Some(1);
        assert_eq!(e.active_dependents(&r, 1), vec![2]);
        assert!(e.active_dependents(&r, 2).is_empty());
        assert_eq!(e.owner_of(&r, 2), 1);
        e.mirror_owner(&r, 2, 1);
        let c = e.entry(&r, 2).clone();
        assert_eq!(c.worktree_id.as_deref(), Some("repo::/w/1"));
        assert_eq!(c.branch.as_deref(), Some("refs/heads/bot/fix"));
        assert_eq!(c.agent_session_id.as_deref(), Some("sess"));
        assert_eq!(c.terminal_handle.as_deref(), Some("h1"));
    }

    #[test]
    fn last_bot_comment_is_the_final_word() {
        let timeline = vec![
            json!({"event":"commented","id":1,"user":{"login":"bot"},"body":"first <!-- ssf: origin=o/r#5 -->","html_url":"u1"}),
            json!({"event":"commented","id":2,"user":{"login":"bot"},"body":"done\n\n<!-- ssf: origin=o/r#5 -->","html_url":"u2"}),
            json!({"event":"commented","id":3,"user":{"login":"alice"},"body":"thanks","html_url":"u3"}),
            json!({"event":"closed","id":4,"actor":{"login":"alice"}}),
        ];
        let c = last_bot_comment(&timeline, "Bot").unwrap();
        assert_eq!(c.body, "done");
        assert_eq!(c.url, "u2");
        assert_eq!(c.session.as_deref(), Some("o/r#5"));
        assert_eq!(c.author, "bot");
        assert!(last_bot_comment(&timeline[2..], "bot").is_none());
    }
}
