//! The reconciliation loop: GitHub assigned issues -> Orca workspaces -> agent prompts.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};
use tracing::{debug, error, info, warn};

use crate::config::{Config, RepoConfig};
use crate::github::{Conditional, GitHub, Issue};
use crate::orca::{Delivery, Orca, Worktree};
use crate::prompt::{self, PromptContext, Rendered, actor_of, event_key, render_event};
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
        let etag = self.state.repo_mut(&repo.name).issues_etag.clone();
        let listing = self
            .gh
            .assigned_issues(owner, name, &self.login, etag.as_deref())
            .await?;
        let (issues, new_etag) = match listing {
            Conditional::NotModified => {
                debug!(repo = repo.name, "assigned issues unchanged");
                return Ok(());
            }
            Conditional::Modified { value, etag } => (value, etag),
        };
        debug!(
            repo = repo.name,
            count = issues.len(),
            "assigned open issues"
        );

        let mut all_ok = true;
        let assigned: BTreeSet<u64> = issues.iter().map(|i| i.number).collect();
        for issue in &issues {
            if let Err(e) = self.reconcile_issue(repo, owner, name, issue).await {
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
            .filter(|s| s.active && !assigned.contains(&s.number))
            .map(|s| s.number)
            .collect();
        for number in stale {
            if let Err(e) = self.retire_issue(repo, owner, name, number).await {
                all_ok = false;
                warn!(repo = repo.name, issue = number, "retiring failed: {e:#}");
            }
        }

        // Only trust the ETag when every issue was handled; otherwise the next
        // pass must see the full listing again to retry.
        self.state.repo_mut(&repo.name).issues_etag = if all_ok { new_etag } else { None };
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

    fn ctx<'a>(&'a self, repo: &'a RepoConfig) -> PromptContext<'a> {
        PromptContext {
            repo,
            daemon: &self.cfg.daemon,
            bot_login: &self.login,
        }
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
            if let Some(r) = render_event(ev, edited, &self.cfg.daemon) {
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
    ) -> Result<()> {
        let existing = self
            .state
            .repo_mut(&repo.name)
            .issues
            .get(&issue.number)
            .cloned();
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
            _ => self.onboard(repo, owner, name, issue).await,
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
    /// the full issue context to a fresh agent.
    async fn onboard(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        issue: &Issue,
    ) -> Result<()> {
        info!(
            repo = repo.name,
            issue = issue.number,
            title = issue.title,
            "onboarding issue"
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
        let timeline = self.gh.timeline(owner, name, issue.number).await?;
        let diff = self.diff(&BTreeMap::new(), &timeline);
        let ctx = self.ctx(repo);
        let text = prompt::initial_prompt(issue, &diff.rendered, &ctx);

        let prior = self
            .state
            .repo_mut(&repo.name)
            .issues
            .get(&issue.number)
            .cloned();
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

        let comment = format!("ssf: bound to issue #{}", issue.number);
        let wt_name = prior
            .as_ref()
            .and_then(|s| s.worktree_name.clone())
            .unwrap_or_else(|| prompt::worktree_name(issue.number, &issue.title));
        {
            let e = self.entry(repo, issue.number);
            e.title = issue.title.clone();
            e.html_url = issue.html_url.clone();
            e.repo_id = Some(setup.repo_id.clone());
            e.worktree_name = Some(wt_name.clone());
        }

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
                let d = self.deliver_to(repo, issue.number, &text, None).await?;
                d.handle
            }
            None => {
                let created = self
                    .orca
                    .create_worktree(
                        &setup.repo_id,
                        &wt_name,
                        issue.number,
                        None,
                        &comment,
                        repo.base_branch.as_deref(),
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
                self.orca.send_prompt(&handle, &text).await?;
                info!(
                    repo = repo.name,
                    issue = issue.number,
                    handle,
                    "launched {} and sent the issue",
                    repo.harness
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
            return Ok(());
        }
        info!(
            repo = repo.name,
            issue = issue.number,
            events = diff.rendered.len(),
            "delivering new activity"
        );
        let ctx = self.ctx(repo);
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
        let diff = self.diff(&st.seen, &timeline);
        let ctx = self.ctx(repo);
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
        e.seen = diff.seen;
        e.active = true;
        e.cleanup_pending = false;
        e.retired_at = None;
        e.terminal_handle = Some(d.handle);
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        Ok(())
    }

    /// Issue left the assigned-and-open set: tell the agent to stop.
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
            .context("retiring unknown issue")?;
        let issue = self.gh.issue(owner, name, number).await?;
        let closed = issue.state == "closed";
        let still_assigned = issue.is_assigned_to(&self.login);
        if !closed && still_assigned {
            // Listing lag; leave it alone.
            return Ok(());
        }
        info!(
            repo = repo.name,
            issue = number,
            closed,
            "issue no longer active for the bot"
        );
        let timeline = self.gh.timeline(owner, name, number).await?;
        let diff = self.diff(&st.seen, &timeline);
        let ctx = self.ctx(repo);
        let text = if closed {
            prompt::closed_prompt(&issue, &diff.rendered, &ctx)
        } else {
            prompt::unassigned_prompt(&issue, &diff.rendered, &ctx)
        };
        // Retirement is best-effort: a deleted workspace must not keep us
        // retrying, and it is not worth rebuilding one just to say goodbye.
        let workspace_alive = match st.worktree_id.as_deref() {
            Some(id) => self.orca.worktree_exists(id).await.unwrap_or(false),
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
        if closed && workspace_alive {
            if let Some(id) = &st.worktree_id {
                let _ = self.orca.set_status(id, "completed").await;
            }
        }
        let cleanup = closed && workspace_alive && self.cfg.daemon.cleanup_on_close;
        let e = self.entry(repo, number);
        e.active = false;
        e.updated_at = Some(issue.updated_at.clone());
        e.seen = diff.seen;
        e.retired_at = Some(now_iso());
        e.cleanup_pending = cleanup;
        if handle.is_some() {
            e.terminal_handle = handle;
            e.last_prompt_at = Some(now_iso());
            e.prompts_sent += 1;
        }
        Ok(())
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

    /// Deliver a prompt to the issue's agent, bringing the workspace and the
    /// agent back first if either is gone.
    async fn deliver_to(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        text: &str,
        relaunch_text: Option<&str>,
    ) -> Result<Delivery> {
        let st = self.entry(repo, number).clone();
        let alive = match st.worktree_id.as_deref() {
            Some(id) => self.orca.worktree_exists(id).await?,
            None => false,
        };
        if !alive {
            self.rehydrate(repo, number).await?;
        }
        let st = self.entry(repo, number).clone();
        let worktree_id = st
            .worktree_id
            .clone()
            .context("issue has no workspace bound")?;
        let title = format!("{} · #{}", repo.harness, number);
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
            let e = self.entry(repo, number);
            e.launched_at = Some(now_iso());
            if !d.resumed {
                e.agent_session_id = None;
            }
            info!(
                repo = repo.name,
                issue = number,
                resumed = d.resumed,
                "harness relaunched"
            );
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

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
