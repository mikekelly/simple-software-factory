//! The reconciliation loop: GitHub assigned issues -> Orca workspaces -> agent prompts.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, SystemTime};
use tracing::{debug, error, info, warn};

use crate::config::{Config, RepoConfig};
use crate::github::{Conditional, GitHub, Issue, PrInfo};
use crate::ipc::{Request, Response};
use crate::orca::{Delivery, Orca, Worktree};
use crate::origin::{self, Origin};
use crate::prompt::{
    self, FinalComment, Fyi, ProjectPrompt, PromptContext, Rendered, actor_of, event_key,
    render_event,
};
use crate::sessions;
use crate::state::{IssueState, State, now_iso};
use crate::status::session_id;

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
        let listener = bind_socket()?;
        info!(
            repos = self.cfg.repos.len(),
            poll_secs = self.cfg.daemon.poll_interval_secs,
            socket = %crate::ipc::socket_path().display(),
            "ssf daemon started"
        );
        'outer: loop {
            self.tick().await;
            let interval = Duration::from_secs(self.cfg.daemon.poll_interval_secs.max(5));
            let deadline = tokio::time::Instant::now() + interval;
            // Between polls, answer the CLI (`ssf sub|unsub|tell`).
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => break,
                    _ = tokio::signal::ctrl_c() => { info!("interrupted; exiting"); break 'outer; }
                    _ = sigterm.recv() => { info!("SIGTERM; exiting"); break 'outer; }
                    conn = listener.accept() => match conn {
                        Ok((stream, _)) => self.serve(stream).await,
                        Err(e) => warn!("accepting a CLI connection failed: {e}"),
                    }
                }
            }
        }
        self.state.save()?;
        let _ = std::fs::remove_file(crate::ipc::socket_path());
        Ok(())
    }

    /// Answer one CLI connection.
    async fn serve(&mut self, mut stream: tokio::net::UnixStream) {
        let resp = match crate::ipc::read_request(&mut stream).await {
            Ok(req) => {
                debug!(?req, "request from the CLI");
                let resp = self.handle_request(req).await;
                if let Err(e) = self.state.save() {
                    error!("saving state: {e:#}");
                }
                resp
            }
            Err(e) => Response::err(format!("bad request: {e:#}")),
        };
        if let Err(e) = crate::ipc::write_response(&mut stream, &resp).await {
            warn!("answering the CLI failed: {e:#}");
        }
    }

    /// `ssf sub|unsub|tell`, run inside the daemon so the state and the
    /// delivery path are the daemon's own.
    pub async fn handle_request(&mut self, req: Request) -> Response {
        match req {
            Request::Ping => Response::ok(serde_json::json!({"login": self.login})),
            Request::Sub { from, target } => match self.subscribe(&from, &target).await {
                Ok(v) => Response::ok(v),
                Err(e) => Response::err(format!("{e:#}")),
            },
            Request::Unsub { from, target } => match self.unsubscribe(&from, &target) {
                Ok(v) => Response::ok(v),
                Err(e) => Response::err(format!("{e:#}")),
            },
            Request::Tell { from, target, text } => {
                match self.tell(from.as_deref(), &target, &text).await {
                    Ok(v) => Response::ok(v),
                    Err(e) => Response::err(format!("{e:#}")),
                }
            }
        }
    }

    /// A watched repository and an item number out of `owner/repo#N`.
    fn locate(&self, item: &str) -> Result<(RepoConfig, u64)> {
        let o = Origin::parse(item).with_context(|| format!("{item}: expected owner/repo#N"))?;
        let repo = self
            .cfg
            .repos
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(&o.repo))
            .cloned()
            .with_context(|| format!("{} is not a watched repository", o.repo))?;
        Ok((repo, o.number))
    }

    /// The session (`owner/repo#N`, normalised to the owning session) behind
    /// a session id the CLI gave. It must be one ssf has a workspace for.
    fn known_session(&self, id: &str) -> Result<(RepoConfig, u64, String)> {
        let (repo, n) = self.locate(id)?;
        let owner = self.owner_of(&repo, n);
        let known = self
            .state
            .repos
            .get(&repo.name)
            .and_then(|rs| rs.issues.get(&owner))
            .is_some_and(|s| s.seeded);
        if !known {
            anyhow::bail!("{id} is not an agent session ssf knows (see `ssf peers --all`)");
        }
        let id = session_id(&repo.name, owner);
        Ok((repo, owner, id))
    }

    async fn subscribe(&mut self, from: &str, target: &str) -> Result<Value> {
        let (_, _, me) = self.known_session(from)?;
        let (repo, number) = self.locate(target)?;
        let (owner, name) = repo.split()?;
        let existing = self
            .state
            .repos
            .get(&repo.name)
            .and_then(|rs| rs.issues.get(&number))
            .cloned();
        let tracked = existing
            .as_ref()
            .is_some_and(|s| s.seeded || s.subscriber_only);
        if tracked && !existing.as_ref().unwrap().subscriber_only {
            let acting = session_id(&repo.name, self.owner_of(&repo, number));
            if acting.eq_ignore_ascii_case(&me) {
                anyhow::bail!("{target} is your own item (its session is {acting})");
            }
        }
        let (title, owner_session, kind, state) = if tracked {
            let st = existing.unwrap();
            let owner_session = if st.subscriber_only {
                None
            } else {
                Some(session_id(&repo.name, self.owner_of(&repo, number)))
            };
            (
                st.title.clone(),
                owner_session,
                st.kind.clone().unwrap_or_else(|| "issue".into()),
                st.github_state.clone().unwrap_or_else(|| "open".into()),
            )
        } else {
            // Nothing tracks it yet: start polling it for the subscriber,
            // from now on (what happened before is not new).
            let issue = self
                .gh
                .issue(owner, name, number)
                .await
                .with_context(|| format!("fetching {target}"))?;
            let timeline = self.gh.timeline(owner, name, number).await?;
            let seen = self.diff(&BTreeMap::new(), &timeline).seen;
            let is_pr = issue.is_pull_request();
            let e = self.entry(&repo, number);
            e.title = issue.title.clone();
            e.html_url = issue.html_url.clone();
            e.kind = Some(if is_pr {
                "pull_request".into()
            } else {
                "issue".into()
            });
            e.github_state = Some(github_state(&issue, None, false));
            e.updated_at = Some(issue.updated_at.clone());
            e.seen = seen;
            e.subscriber_only = true;
            e.active = false;
            info!(
                repo = repo.name,
                issue = number,
                "tracking for subscribers only"
            );
            (
                issue.title.clone(),
                None,
                e.kind.clone().unwrap(),
                github_state(&issue, None, false),
            )
        };
        let e = self.entry(&repo, number);
        let added = if e.subscribers.iter().any(|s| s.eq_ignore_ascii_case(&me)) {
            false
        } else {
            e.subscribers.push(me.clone());
            true
        };
        info!(
            repo = repo.name,
            issue = number,
            subscriber = me,
            added,
            "subscribed"
        );
        Ok(serde_json::json!({
            "item": session_id(&repo.name, number),
            "title": title,
            "kind": kind,
            "github_state": state,
            "owner": owner_session,
            "subscriber": me,
            "added": added,
        }))
    }

    fn unsubscribe(&mut self, from: &str, target: &str) -> Result<Value> {
        let (_, _, me) = self.known_session(from)?;
        let (repo, number) = self.locate(target)?;
        let Some(st) = self
            .state
            .repos
            .get(&repo.name)
            .and_then(|rs| rs.issues.get(&number))
            .cloned()
        else {
            anyhow::bail!("{target} is not tracked");
        };
        let removed = st.subscribers.iter().any(|s| s.eq_ignore_ascii_case(&me));
        let e = self.entry(&repo, number);
        e.subscribers.retain(|s| !s.eq_ignore_ascii_case(&me));
        let dropped = e.subscriber_only && e.subscribers.is_empty();
        if dropped {
            // Nobody listens any more and nothing else remembers it.
            self.state.repo_mut(&repo.name).issues.remove(&number);
            info!(
                repo = repo.name,
                issue = number,
                "no subscribers left; no longer tracked"
            );
        }
        info!(
            repo = repo.name,
            issue = number,
            subscriber = me,
            removed,
            "unsubscribed"
        );
        Ok(serde_json::json!({
            "item": session_id(&repo.name, number),
            "title": st.title,
            "subscriber": me,
            "removed": removed,
            "untracked": dropped,
        }))
    }

    /// Paste a message into the terminal of the session acting on `target`.
    async fn tell(&mut self, from: Option<&str>, target: &str, text: &str) -> Result<Value> {
        if text.trim().is_empty() {
            anyhow::bail!("nothing to say");
        }
        let (repo, number) = self.locate(target)?;
        let st = self
            .state
            .repos
            .get(&repo.name)
            .and_then(|rs| rs.issues.get(&number))
            .cloned()
            .filter(|s| s.seeded)
            .with_context(|| format!("{target} has no agent session (see `ssf peers --all`)"))?;
        let acting = self.owner_of(&repo, number);
        let ost = self.entry(&repo, acting).clone();
        let alive = match ost.worktree_id.as_deref() {
            Some(id) => self.orca.worktree_exists(id).await.unwrap_or(false),
            None => false,
        };
        if !ost.active && !alive {
            anyhow::bail!(
                "the session on {target} ({}) has retired and its workspace is gone",
                session_id(&repo.name, acting)
            );
        }
        let (sender, sender_title) = match from {
            Some(f) => {
                let (frepo, fnum, fid) = self.known_session(f)?;
                let title = self.entry(&frepo, fnum).title.clone();
                (Some(fid), Some(title).filter(|t| !t.is_empty()))
            }
            None => (None, None),
        };
        let prompt = prompt::tell_prompt(
            sender.as_deref(),
            sender_title.as_deref(),
            text,
            self.cfg.daemon.max_body_chars,
        );
        let d = self.deliver_to(&repo, number, &prompt, None).await?;
        let e = self.entry(&repo, acting);
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        info!(
            repo = repo.name,
            issue = number,
            session = acting,
            from = sender.as_deref().unwrap_or("a human"),
            "delivered a message"
        );
        Ok(serde_json::json!({
            "item": session_id(&repo.name, number),
            "title": st.title,
            "session": session_id(&repo.name, acting),
            "terminal": d.handle,
            "relaunched": d.relaunched,
            "from": sender,
        }))
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
            return self.watch_subscribed(repo, owner, name).await;
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
                && ignored.is_some_and(|at| fresh.as_ref().is_none_or(|i| i.updated_at == *at))
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
        self.watch_subscribed(repo, owner, name).await
    }

    /// Poll the items that are tracked only because sessions subscribed to
    /// them (they are on no listing), and fan their activity out.
    async fn watch_subscribed(&mut self, repo: &RepoConfig, owner: &str, name: &str) -> Result<()> {
        let watched: Vec<IssueState> = self
            .state
            .repo_mut(&repo.name)
            .issues
            .values()
            .filter(|s| {
                s.subscriber_only
                    && !s.active
                    && !s.subscribers.is_empty()
                    && !matches!(s.github_state.as_deref(), Some("closed" | "merged"))
            })
            .cloned()
            .collect();
        let mut failed = false;
        for st in watched {
            let number = st.number;
            let issue = match self.gh.issue(owner, name, number).await {
                Ok(i) => i,
                Err(e) => {
                    failed = true;
                    warn!(
                        repo = repo.name,
                        issue = number,
                        "polling subscribed item failed: {e:#}"
                    );
                    continue;
                }
            };
            if st.updated_at.as_deref() == Some(issue.updated_at.as_str()) {
                continue;
            }
            let timeline = match self.gh.timeline(owner, name, number).await {
                Ok(t) => t,
                Err(e) => {
                    failed = true;
                    warn!(
                        repo = repo.name,
                        issue = number,
                        "polling subscribed item failed: {e:#}"
                    );
                    continue;
                }
            };
            self.record_origins(repo, &issue, &timeline);
            let diff = self.diff(&st.seen, &timeline);
            let closed = issue.state == "closed";
            let merged = closed
                && issue.is_pull_request()
                && self
                    .gh
                    .pull(owner, name, number)
                    .await
                    .map(|p| p.merged)
                    .unwrap_or(false);
            info!(
                repo = repo.name,
                issue = number,
                events = diff.rendered.len(),
                closed,
                "subscribed item changed"
            );
            let what = if closed { Fyi::Closed } else { Fyi::Activity };
            self.fan_out(repo, &issue, &diff.rendered, what, merged, &[])
                .await;
            let e = self.entry(repo, number);
            e.updated_at = Some(issue.updated_at.clone());
            e.seen = diff.seen;
            e.title = issue.title.clone();
            e.github_state = Some(github_state(&issue, None, merged));
            if closed {
                // The subscribers have had the last word on it. An item that
                // never had a session is forgotten; one that did keeps its
                // workspace record for the cleanup.
                if st.seeded {
                    let e = self.entry(repo, number);
                    e.subscriber_only = false;
                    e.subscribers.clear();
                } else {
                    self.state.repo_mut(&repo.name).issues.remove(&number);
                }
            }
        }
        if failed {
            anyhow::bail!("polling a subscribed item failed");
        }
        Ok(())
    }

    /// The session (`owner/repo#N`) that acts on the item a post's origin
    /// tag names: the item's owner, in a watched repository; the tag itself
    /// anywhere else.
    fn acting_session(&self, origin: &str) -> String {
        let Some(o) = Origin::parse(origin) else {
            return origin.to_string();
        };
        match self
            .cfg
            .repos
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(&o.repo))
        {
            Some(r) => session_id(&r.name, self.owner_of(r, o.number)),
            None => origin.to_string(),
        }
    }

    /// `events` as `recipient` (a session id) should see them: without its
    /// own posts, which would only echo its work back at it. Other sessions'
    /// posts stay, labelled with where they came from by the renderer.
    fn for_recipient(&self, events: &[Rendered], recipient: &str) -> Vec<Rendered> {
        if self.cfg.daemon.include_own_events {
            return events.to_vec();
        }
        events
            .iter()
            .filter(|e| {
                e.origin
                    .as_deref()
                    .is_none_or(|o| !self.acting_session(o).eq_ignore_ascii_case(recipient))
            })
            .cloned()
            .collect()
    }

    /// The session id of whoever acts on `number`.
    fn acting_on(&self, repo: &RepoConfig, number: u64) -> String {
        session_id(&repo.name, self.owner_of(repo, number))
    }

    /// Tell every subscriber of an item what happened on it, each without
    /// its own posts. Best effort: a subscriber that cannot be reached is
    /// logged and skipped, never retried, and a retired subscriber whose
    /// workspace is gone is not rebuilt for an FYI. Sessions in `skip` are
    /// left out (a delegating parent that gets a fuller message instead).
    async fn fan_out(
        &mut self,
        repo: &RepoConfig,
        issue: &Issue,
        events: &[Rendered],
        what: Fyi,
        merged: bool,
        skip: &[String],
    ) {
        let st = self.entry(repo, issue.number).clone();
        if st.subscribers.is_empty() {
            return;
        }
        let owner_session = if st.subscriber_only || !st.seeded {
            None
        } else {
            Some(self.acting_on(repo, issue.number))
        };
        for sub in st.subscribers.clone() {
            if owner_session
                .as_deref()
                .is_some_and(|o| o.eq_ignore_ascii_case(&sub))
                || skip.iter().any(|s| s.eq_ignore_ascii_case(&sub))
            {
                continue;
            }
            let Ok((srepo, snum, sid)) = self.known_session(&sub) else {
                warn!(
                    repo = repo.name,
                    issue = issue.number,
                    subscriber = sub,
                    "subscriber is not a session ssf knows; skipping"
                );
                continue;
            };
            let sst = self.entry(&srepo, snum).clone();
            let alive = match sst.worktree_id.as_deref() {
                Some(id) => self.orca.worktree_exists(id).await.unwrap_or(false),
                None => false,
            };
            if !sst.active && !alive {
                debug!(
                    repo = repo.name,
                    issue = issue.number,
                    subscriber = sid,
                    "subscriber has retired; not telling it"
                );
                continue;
            }
            let mine = self.for_recipient(events, &sid);
            if mine.is_empty() && what == Fyi::Activity {
                continue;
            }
            let ctx = self.ctx(repo, &st);
            let text =
                prompt::fyi_prompt(issue, &mine, &ctx, owner_session.as_deref(), merged, what);
            match self.deliver_to(&srepo, snum, &text, None).await {
                Ok(_) => {
                    info!(
                        repo = repo.name,
                        issue = issue.number,
                        subscriber = sid,
                        events = mine.len(),
                        "told a subscriber"
                    );
                    let e = self.entry(&srepo, snum);
                    e.last_prompt_at = Some(now_iso());
                    e.prompts_sent += 1;
                }
                Err(e) => warn!(
                    repo = repo.name,
                    issue = issue.number,
                    subscriber = sid,
                    "could not tell a subscriber: {e:#}"
                ),
            }
        }
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
    fn record_origins(
        &mut self,
        repo: &RepoConfig,
        issue: &Issue,
        timeline: &[Value],
    ) -> origin::Scan {
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
            // like being assigned, always count. A comment that carries an
            // origin tag is kept here: it came from one session and may be
            // news to another, so it is sorted out per recipient
            // (`for_recipient`) instead.
            let own = actor_of(ev).eq_ignore_ascii_case(&self.login);
            let echo = matches!(
                kind,
                "commented" | "cross-referenced" | "referenced" | "committed"
            );
            let tagged = kind == "commented"
                && origin::parse(crate::github::value_str(ev, &["body"]).unwrap_or("")).is_some();
            if own && echo && !tagged && !self.cfg.daemon.include_own_events {
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
            e.subscriber_only = false;
        }
        // Subscribers of an item that had no session until now hear that it
        // got one, with whatever happened since they last heard.
        let since_prior = prior
            .as_ref()
            .filter(|p| p.subscriber_only)
            .map(|p| self.diff(&p.seen, &timeline).rendered);
        let scan = self.record_origins(repo, issue, &timeline);
        let by_bot = issue.author().eq_ignore_ascii_case(&self.login);

        // Who acts on this item. An item a session opened belongs to that
        // session (first binding wins: it never spawns a second one), unless
        // the session handed it off, in which case it gets its own session
        // and the parent hears about it once, when it closes.
        if let Some(tag) = scan
            .origin_tag
            .as_ref()
            .filter(|t| t.is_delegate() && by_bot)
        {
            info!(
                repo = repo.name,
                issue = issue.number,
                parent = %tag.origin,
                "handed off by a session; starting its own"
            );
            self.entry(repo, issue.number).delegated_by = Some(tag.origin.to_string());
            // The delegating parent follows its child.
            let parent = self.acting_session(&tag.origin.to_string());
            let e = self.entry(repo, issue.number);
            if !e
                .subscribers
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&parent))
            {
                e.subscribers.push(parent);
            }
        } else if let Some(owner) = self.find_owner(repo, issue, pr.as_ref(), &scan) {
            return self
                .bind_to(repo, issue, owner, diff, triggers, since_prior)
                .await;
        } else if triggers.iter().all(|t| t == "created") {
            // Opened by the bot, but from nowhere ssf can name and with no
            // human asking for it: not worth a session.
            info!(
                repo = repo.name,
                issue = issue.number,
                "opened by the bot without a usable origin tag; ignoring until it changes"
            );
            self.ignored
                .insert((repo.name.clone(), issue.number), issue.updated_at.clone());
            // Still polled for whoever subscribed to it.
            if prior.as_ref().is_some_and(|p| p.subscriber_only) {
                self.entry(repo, issue.number).subscriber_only = true;
            }
            return Ok(());
        }
        self.refresh_projects(repo, owner, name, issue.number).await;
        let mine = self.for_recipient(&diff.rendered, &session_id(&repo.name, issue.number));

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
                let text = self.initial_text(repo, issue, &mine);
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
                let text = self.initial_text(repo, issue, &mine);
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
        if let Some(since) = since_prior {
            self.fan_out(repo, issue, &since, Fyi::Tracked, false, &[])
                .await;
        }
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
        since_prior: Option<Vec<Rendered>>,
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
        let mine = self.for_recipient(&diff.rendered, &session_id(&repo.name, owner));
        let ctx = self.ctx(repo, &snapshot);
        let text = prompt::tracked_prompt(issue, &mine, &ctx);
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
        if let Some(since) = since_prior {
            self.fan_out(repo, issue, &since, Fyi::Tracked, false, &[])
                .await;
        }
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
        let all = self.for_recipient(&all, &self.acting_on(repo, number));
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
            warn!(
                repo = repo.name,
                issue = issue.number,
                parent,
                "unparseable parent session"
            );
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
                info!(
                    repo = repo.name,
                    issue = issue.number,
                    parent,
                    "told the parent session"
                );
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
        // Subscribers hear first: the owner's own posts are news to them,
        // and a failed delivery to the owner must not replay to them.
        self.fan_out(repo, issue, &diff.rendered, Fyi::Activity, false, &[])
            .await;
        let mine = self.for_recipient(&diff.rendered, &self.acting_on(repo, issue.number));
        if mine.is_empty() {
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
            events = mine.len(),
            "delivering new activity"
        );
        self.refresh_projects(repo, owner, name, issue.number).await;
        let st = IssueState {
            projects: self.entry(repo, issue.number).projects.clone(),
            ..st
        };
        let ctx = self.ctx(repo, &st);
        let text = prompt::followup_prompt(issue, &mine, &ctx);
        // A harness started from scratch has lost its memory, so it gets the
        // whole story rather than just the delta.
        let mut all = self.diff(&BTreeMap::new(), &timeline).rendered;
        all.retain(|r| !diff.rendered.iter().any(|n| n.key == r.key));
        let all = self.for_recipient(&all, &self.acting_on(repo, issue.number));
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
        self.entry(repo, issue.number).subscriber_only = false;
        let me = self.acting_on(repo, issue.number);
        let mine = self.for_recipient(&diff.rendered, &me);
        let ctx = self.ctx(repo, &st);
        let text = prompt::reassigned_prompt(issue, &mine, &ctx);
        let all = self.diff(&BTreeMap::new(), &timeline).rendered;
        let all = self.for_recipient(&all, &me);
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
        self.fan_out(repo, issue, &diff.rendered, Fyi::Tracked, false, &[])
            .await;
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
        let session = self.owner_of(repo, number);
        let mine = self.for_recipient(&diff.rendered, &session_id(&repo.name, session));
        let ctx = self.ctx(repo, &st);
        let text = if closed {
            prompt::closed_prompt(&issue, &mine, &ctx)
        } else {
            prompt::unassigned_prompt(&issue, &mine, &ctx)
        };
        // Subscribers hear about the end of it too; a delegating parent gets
        // its own, fuller message below instead.
        let parent_to_tell = if closed && !st.parent_notified {
            st.delegated_by.clone()
        } else {
            None
        };
        let skip: Vec<String> = parent_to_tell
            .iter()
            .map(|p| self.acting_session(p))
            .collect();
        self.fan_out(
            repo,
            &issue,
            &diff.rendered,
            if closed { Fyi::Closed } else { Fyi::Unassigned },
            merged,
            &skip,
        )
        .await;
        // Retirement is best-effort: a deleted workspace must not keep us
        // retrying, and it is not worth rebuilding one just to say goodbye.
        // An owned item's workspace is its owner's.
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
        if let Some(parent) = parent_to_tell {
            self.notify_parent(repo, &issue, merged, &timeline, &parent)
                .await;
            self.entry(repo, number).parent_notified = true;
        }
        // A retired session hears nothing more: it is unsubscribed everywhere.
        // Its own item keeps its subscribers, and stays polled for them while
        // it is open.
        if !shared {
            let me = session_id(&repo.name, number);
            let dropped = self.state.unsubscribe_everywhere(&me);
            if !dropped.is_empty() {
                info!(
                    repo = repo.name,
                    issue = number,
                    ?dropped,
                    "retired session unsubscribed"
                );
            }
        }
        let e = self.entry(repo, number);
        if !closed && !e.subscribers.is_empty() {
            e.subscriber_only = true;
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
        let live = alive
            && self
                .orca
                .has_live_agent(&worktree_id)
                .await
                .unwrap_or(false);
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
            if let Some(id) = rs
                .issues
                .get(&owner)
                .and_then(|o| o.agent_session_id.clone())
            {
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

/// Listen for the CLI on the daemon's socket, replacing a stale one.
fn bind_socket() -> Result<tokio::net::UnixListener> {
    let path = crate::ipc::socket_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    if path.exists() {
        // Another daemon, or a leftover from one that died?
        if std::os::unix::net::UnixStream::connect(&path).is_ok() {
            anyhow::bail!(
                "another ssf daemon is listening on {}; stop it first",
                path.display()
            );
        }
        let _ = std::fs::remove_file(&path);
    }
    let listener = tokio::net::UnixListener::bind(&path)
        .with_context(|| format!("listening on {}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600));
    Ok(listener)
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
        for (n, o) in [
            (1, None),
            (2, Some(1)),
            (3, Some(2)),
            (4, Some(5)),
            (5, Some(4)),
        ] {
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
        assert_eq!(
            e.find_owner(&r, &untagged, Some(&pr("nobody")), &scan),
            None
        );
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
        assert_eq!(
            e.find_owner(&r, &tagged, Some(&pr("bot/fix")), &scan),
            Some(1)
        );
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

    fn comment(id: u64, who: &str, body: &str) -> Value {
        json!({"event":"commented","id":id,"user":{"login":who},"body":body,
            "html_url":format!("u{id}"),"created_at":"t","updated_at":"t"})
    }

    #[test]
    fn tagged_bot_comments_are_kept_and_sorted_per_recipient() {
        let mut e = engine();
        let r = repo();
        e.cfg.repos.push(r.clone());
        seeded(&mut e, 1, Some("bot/issue-1"), true);
        seeded(&mut e, 3, Some("bot/issue-3"), true);
        // PR 7 is owned by session 1.
        seeded(&mut e, 7, None, true);
        e.entry(&r, 7).shares_workspace_of = Some(1);
        let timeline = vec![
            comment(1, "alice", "human"),
            comment(2, "bot", "untagged bot comment"),
            comment(3, "bot", "from one\n\n<!-- ssf: origin=o/r#1 -->"),
            comment(4, "bot", "from three\n\n<!-- ssf: origin=o/r#3 -->"),
            comment(
                5,
                "bot",
                "from the PR's session\n\n<!-- ssf: origin=o/r#7 -->",
            ),
            comment(6, "bot", "from elsewhere\n\n<!-- ssf: origin=x/y#2 -->"),
        ];
        let d = e.diff(&BTreeMap::new(), &timeline);
        let keys: Vec<&str> = d.rendered.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "commented:1",
                "commented:3",
                "commented:4",
                "commented:5",
                "commented:6"
            ],
            "the untagged bot comment keeps today's rule; tagged ones stay"
        );
        assert_eq!(d.seen.len(), 6, "everything is recorded as seen");
        assert!(d.rendered[1].text.contains("(from the agent on o/r#1)"));

        // Session 1 (which also acts on PR 7) does not get its own posts back.
        let mine = e.for_recipient(&d.rendered, "o/r#1");
        let keys: Vec<&str> = mine.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, vec!["commented:1", "commented:4", "commented:6"]);
        // Session 3 sees session 1's (and the PR's) comments, not its own.
        let theirs = e.for_recipient(&d.rendered, "o/r#3");
        let keys: Vec<&str> = theirs.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["commented:1", "commented:3", "commented:5", "commented:6"]
        );
        // Case-insensitive on the repository, like everything else.
        assert_eq!(e.for_recipient(&d.rendered, "O/R#3").len(), 4);
        assert_eq!(e.acting_session("o/r#7"), "o/r#1");
        assert_eq!(e.acting_session("x/y#2"), "x/y#2");
        assert_eq!(e.acting_session("garbage"), "garbage");
        e.cfg.daemon.include_own_events = true;
        assert_eq!(e.for_recipient(&d.rendered, "o/r#1").len(), 5);
    }

    #[tokio::test]
    async fn subscriptions_through_requests() {
        let mut e = engine();
        let r = repo();
        e.cfg.repos.push(r.clone());
        seeded(&mut e, 1, Some("bot/issue-1"), true);
        seeded(&mut e, 3, Some("bot/issue-3"), true);
        e.entry(&r, 3).title = "Three".into();
        seeded(&mut e, 7, None, true);
        e.entry(&r, 7).shares_workspace_of = Some(1);

        // Session 1 follows item 3.
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#1".into(),
                target: "o/r#3".into(),
            })
            .await;
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.data["owner"], "o/r#3");
        assert_eq!(resp.data["title"], "Three");
        assert_eq!(resp.data["added"], true);
        assert_eq!(e.entry(&r, 3).subscribers, vec!["o/r#1"]);
        // Again: no duplicate.
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#1".into(),
                target: "o/r#3".into(),
            })
            .await;
        assert!(resp.ok);
        assert_eq!(resp.data["added"], false);
        assert_eq!(e.entry(&r, 3).subscribers.len(), 1);
        // From the PR's identity it is still session 1, and its own items
        // are refused.
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#7".into(),
                target: "o/r#1".into(),
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("your own item"));
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#7".into(),
                target: "o/r#7".into(),
            })
            .await;
        assert!(!resp.ok);
        // Unknown sessions and repositories are refused.
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#99".into(),
                target: "o/r#3".into(),
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("not an agent session"));
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#1".into(),
                target: "x/y#3".into(),
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("not a watched repository"));
        let resp = e
            .handle_request(Request::Unsub {
                from: "o/r#1".into(),
                target: "nonsense".into(),
            })
            .await;
        assert!(!resp.ok);

        // A subscriber-only item is dropped with its last subscriber.
        {
            let s = e.entry(&r, 20);
            s.subscriber_only = true;
            s.subscribers = vec!["o/r#1".into(), "o/r#3".into()];
        }
        let resp = e
            .handle_request(Request::Unsub {
                from: "o/r#7".into(),
                target: "o/r#20".into(),
            })
            .await;
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.data["removed"], true);
        assert_eq!(resp.data["untracked"], false);
        assert_eq!(e.entry(&r, 20).subscribers, vec!["o/r#3"]);
        let resp = e
            .handle_request(Request::Unsub {
                from: "o/r#3".into(),
                target: "o/r#20".into(),
            })
            .await;
        assert!(resp.ok);
        assert_eq!(resp.data["untracked"], true);
        assert!(!e.state.repos["o/r"].issues.contains_key(&20));
        // Unsubscribing from something never followed is fine.
        let resp = e
            .handle_request(Request::Unsub {
                from: "o/r#3".into(),
                target: "o/r#1".into(),
            })
            .await;
        assert!(resp.ok);
        assert_eq!(resp.data["removed"], false);

        // Telling a retired session with no workspace is refused, and an
        // empty message too.
        let resp = e
            .handle_request(Request::Tell {
                from: None,
                target: "o/r#3".into(),
                text: "  ".into(),
            })
            .await;
        assert!(!resp.ok);
        e.entry(&r, 3).active = false;
        let resp = e
            .handle_request(Request::Tell {
                from: Some("o/r#1".into()),
                target: "o/r#3".into(),
                text: "hello".into(),
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("retired"));
        let resp = e
            .handle_request(Request::Tell {
                from: None,
                target: "o/r#50".into(),
                text: "hello".into(),
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("no agent session"));
        assert!(e.handle_request(Request::Ping).await.ok);
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
