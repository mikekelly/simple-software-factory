//! The reconciliation loop: GitHub assigned issues -> Orca workspaces -> agent prompts.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, SystemTime};
use tracing::{debug, error, info, warn};

use crate::config::DriverKind;
use crate::config::{Config, RepoConfig};
use crate::driver::{Driver, Drivers, Relaunch};
use crate::github::{Conditional, GitHub, Issue, PrInfo};
use crate::ipc::{Request, Response};
use crate::orca::{Delivery, Worktree};
use crate::origin::{self, Origin};
use crate::prompt::{
    self, FinalComment, Fyi, ProjectPrompt, PromptContext, Rendered, ReviewEnd, actor_of,
    event_key, render_event,
};
use crate::release::{self, git};
use crate::sessions;
use crate::state::{Ignored, IssueState, State, now_iso};
use crate::status::{reviewer_session_id, session_id};

/// Consecutive delivery failures before an issue is re-onboarded from scratch.
const MAX_DELIVERY_FAILURES: u32 = 5;

/// Daemon-side release refusals (the re-check on the pass after `ssf
/// release` found work) before ssf stops telling the agent and leaves the
/// workspace for a person. The synchronous refusal `ssf release` prints is
/// not counted: only the daemon's own refusals can loop.
pub const MAX_RELEASE_REFUSALS: u32 = 3;

/// Marks an error from the reviewer side of a pull request, so it is
/// counted against the reviewer session rather than the PR's own record.
#[derive(Debug)]
struct ReviewerFailure;

impl std::fmt::Display for ReviewerFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("reviewer session")
    }
}

pub struct Engine {
    cfg: Config,
    gh: GitHub,
    drivers: Drivers,
    /// Drivers that did not answer at the start of this pass; their
    /// repositories are skipped until they do.
    down: Vec<DriverKind>,
    login: String,
    state: State,
    failures: BTreeMap<(String, u64), u32>,
    /// Drivers whose startup pass (resume sessions whose terminals are
    /// gone) has not run yet; each runs on the first pass that finds that
    /// driver ready.
    startup_pending: Vec<DriverKind>,
}

/// The ignore record of a bot-opened item nothing binds to (see
/// [`Ignored`] in `state`): what it was last looked at with, so it is not
/// re-examined every pass, nor after every daemon restart.
impl Ignored {
    fn new(issue: &Issue, triggers: &[String]) -> Self {
        let mut triggers = triggers.to_vec();
        triggers.sort();
        Self {
            updated_at: issue.updated_at.clone(),
            triggers,
        }
    }

    /// Whether the item is still as it was: unchanged on GitHub (or on no
    /// listing that changed, when `fresh` is `None`) and on the same
    /// listings.
    fn stands(&self, fresh: Option<&Issue>, triggers: &[String]) -> bool {
        let mut triggers = triggers.to_vec();
        triggers.sort();
        fresh.is_none_or(|i| i.updated_at == self.updated_at) && triggers == self.triggers
    }
}

/// New events for an issue relative to what has been delivered already.
struct Diff {
    rendered: Vec<Rendered>,
    /// Every key/marker observed (including filtered ones), to be recorded as seen.
    seen: BTreeMap<String, String>,
}

/// Which session record of an item is meant: the item's own (the session
/// that works on it, or is bound to its owner), or the reviewer session of
/// a pull request, which has a workspace of its own next to the author's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Item(u64),
    Reviewer(u64),
}

impl Slot {
    fn number(self) -> u64 {
        match self {
            Slot::Item(n) | Slot::Reviewer(n) => n,
        }
    }

    fn is_reviewer(self) -> bool {
        matches!(self, Slot::Reviewer(_))
    }
}

/// Session id of a record: `owner/repo#N`, or `owner/repo#N:reviewer`.
fn slot_id(repo: &str, slot: Slot) -> String {
    match slot {
        Slot::Item(n) => session_id(repo, n),
        Slot::Reviewer(n) => reviewer_session_id(repo, n),
    }
}

impl Engine {
    /// The driver a repository's sessions run under.
    fn driver(&self, repo: &RepoConfig) -> &Driver {
        let kind = self.cfg.driver_for(repo);
        self.drivers
            .get(kind)
            .expect("sync_drivers keeps a driver for every kind the config uses")
    }

    /// Rebuild the driver set when the config's choice of drivers changed
    /// (a repo added with `--driver`, or the default switched).
    fn sync_drivers(&mut self) {
        if self.drivers.kinds() != self.cfg.drivers_in_use() {
            self.drivers = Drivers::from_config(&self.cfg);
        }
    }

    fn driver_down(&self, repo: &RepoConfig) -> bool {
        self.down.contains(&self.cfg.driver_for(repo))
    }

    /// Ask every driver in use whether it is ready, remembering the ones
    /// that are not. Returns what is wrong with the ones that are not; fails
    /// only when none is.
    async fn check_drivers(&mut self) -> Result<Vec<String>> {
        let mut down = Vec::new();
        let mut errors = Vec::new();
        let mut any_up = false;
        for d in self.drivers.iter() {
            match d.status().await {
                Ok(()) => any_up = true,
                Err(e) => {
                    down.push(d.kind());
                    errors.push(format!("{} unavailable: {e:#}", d.label()));
                }
            }
        }
        for e in &errors {
            if any_up {
                warn!("{e}; its repositories are skipped this pass");
            }
        }
        self.down = down;
        if any_up {
            Ok(errors)
        } else {
            anyhow::bail!("{}", errors.join("; "))
        }
    }

    pub async fn new(cfg: Config) -> Result<Self> {
        let token = cfg.github_token()?;
        let gh = GitHub::new(&cfg.github.api_url, &token)?;
        let me = gh.whoami().await.context("verifying GitHub token")?;
        info!(login = me.login, kind = me.kind, "authenticated to GitHub");
        let drivers = Drivers::from_config(&cfg);
        let mut state = State::load()?;
        state.bot_login = Some(me.login.clone());
        state.save()?;
        let startup_pending = if cfg.daemon.resume_on_start {
            cfg.drivers_in_use()
        } else {
            Vec::new()
        };
        Ok(Self {
            cfg,
            gh,
            drivers,
            down: Vec::new(),
            login: me.login,
            state,
            failures: BTreeMap::new(),
            startup_pending,
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
        // Orca may still be coming up in the same login (the unit starts with
        // the graphical session), and the startup pass needs it: wait a
        // bounded while before the first poll rather than skipping passes.
        let mut stop = false;
        if !self.startup_pending.is_empty() {
            let wait = Duration::from_secs(self.cfg.daemon.startup_orca_wait_secs);
            let started = tokio::time::Instant::now();
            loop {
                let err = match self.check_drivers().await {
                    Ok(_) => break,
                    Err(e) => e,
                };
                let elapsed = started.elapsed();
                if elapsed >= wait {
                    if !wait.is_zero() {
                        warn!(
                            waited_secs = elapsed.as_secs(),
                            "the driver is still not ready; polling starts now and interrupted sessions \
are resumed on the first pass that finds it: {err:#}"
                        );
                    }
                    break;
                }
                info!("waiting for the driver before the first pass: {err:#}");
                let deadline =
                    tokio::time::Instant::now() + Duration::from_secs(10).min(wait - elapsed);
                if self.idle_until(deadline, &listener, &mut sigterm).await {
                    stop = true;
                    break;
                }
            }
        }
        while !stop {
            self.tick().await;
            let interval = Duration::from_secs(self.cfg.daemon.poll_interval_secs.max(5));
            let deadline = tokio::time::Instant::now() + interval;
            stop = self.idle_until(deadline, &listener, &mut sigterm).await;
        }
        self.state.save()?;
        let _ = std::fs::remove_file(crate::ipc::socket_path());
        Ok(())
    }

    /// Answer the CLI (`ssf sub|unsub|tell`) until `deadline`. True when a
    /// signal asked the daemon to exit.
    async fn idle_until(
        &mut self,
        deadline: tokio::time::Instant,
        listener: &tokio::net::UnixListener,
        sigterm: &mut tokio::signal::unix::Signal,
    ) -> bool {
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => return false,
                _ = tokio::signal::ctrl_c() => { info!("interrupted; exiting"); return true; }
                _ = sigterm.recv() => { info!("SIGTERM; exiting"); return true; }
                conn = listener.accept() => match conn {
                    Ok((stream, _)) => self.serve(stream).await,
                    Err(e) => warn!("accepting a CLI connection failed: {e}"),
                }
            }
        }
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
            Request::Release { session, force } => match self.release(&session, force).await {
                Ok(v) => Response::ok(v),
                Err(e) => Response::err(format!("{e:#}")),
            },
            Request::Purge {
                dry_run,
                older_than_days,
                force,
            } => match self.purge(dry_run, older_than_days, force).await {
                Ok(v) => Response::ok(v),
                Err(e) => Response::err(format!("{e:#}")),
            },
        }
    }

    /// A watched repository and an item number out of `owner/repo#N`.
    /// A session or item reference from the CLI: the repository, the number
    /// and whether it names the reviewer session (`owner/repo#N:reviewer`).
    fn locate(&self, item: &str) -> Result<(RepoConfig, u64, bool)> {
        let (o, reviewer) = origin::parse_session(item)
            .with_context(|| format!("{item}: expected owner/repo#N (or owner/repo#N:reviewer)"))?;
        let repo = self
            .cfg
            .repos
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(&o.repo))
            .cloned()
            .with_context(|| format!("{} is not a watched repository", o.repo))?;
        Ok((repo, o.number, reviewer))
    }

    /// The session (`owner/repo#N`, normalised to the owning session, or
    /// `owner/repo#N:reviewer`) behind a session id the CLI gave. It must be
    /// one ssf has a workspace for.
    fn known_session(&self, id: &str) -> Result<(RepoConfig, Slot, String)> {
        let (repo, n, reviewer) = self.locate(id)?;
        let slot = if reviewer {
            Slot::Reviewer(n)
        } else {
            Slot::Item(self.owner_of(&repo, n))
        };
        let known = self.peek(&repo, slot).is_some_and(|s| s.seeded);
        if !known {
            anyhow::bail!("{id} is not an agent session ssf knows (see `ssf peers --all`)");
        }
        let id = slot_id(&repo.name, slot);
        Ok((repo, slot, id))
    }

    /// The record for a slot, if there is one.
    fn peek(&self, repo: &RepoConfig, slot: Slot) -> Option<&IssueState> {
        match slot {
            Slot::Item(n) => self.state.repos.get(&repo.name)?.issues.get(&n),
            Slot::Reviewer(n) => self.state.reviewer(&repo.name, n),
        }
    }

    /// The record for a slot, created empty if missing.
    fn record(&mut self, repo: &RepoConfig, slot: Slot) -> &mut IssueState {
        match slot {
            Slot::Item(n) => self.entry(repo, n),
            Slot::Reviewer(n) => {
                let e = self
                    .state
                    .repo_mut(&repo.name)
                    .reviewers
                    .entry(n)
                    .or_default();
                e.number = n;
                e
            }
        }
    }

    async fn subscribe(&mut self, from: &str, target: &str) -> Result<Value> {
        let (_, _, me) = self.known_session(from)?;
        let (repo, number, reviewer) = self.locate(target)?;
        if reviewer {
            anyhow::bail!(
                "{target} is a reviewer session, not an item; subscribe to the pull request ({}#{number})",
                repo.name
            );
        }
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
        let (repo, number, reviewer) = self.locate(target)?;
        if reviewer {
            anyhow::bail!(
                "{target} is a reviewer session, not an item; unsubscribe from the pull request ({}#{number})",
                repo.name
            );
        }
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
        let (repo, number, reviewer) = self.locate(target)?;
        let slot = if reviewer {
            Slot::Reviewer(number)
        } else {
            Slot::Item(number)
        };
        let st = self
            .peek(&repo, slot)
            .cloned()
            .filter(|s| s.seeded)
            .with_context(|| format!("{target} has no agent session (see `ssf peers --all`)"))?;
        let acting = if reviewer {
            slot
        } else {
            Slot::Item(self.owner_of(&repo, number))
        };
        let ost = self.record(&repo, acting).clone();
        let alive = match ost.worktree_id.as_deref() {
            Some(id) => self
                .driver(&repo)
                .worktree_exists(id)
                .await
                .unwrap_or(false),
            None => false,
        };
        if !ost.active && !alive {
            anyhow::bail!(
                "the session on {target} ({}) has retired and its workspace is gone",
                slot_id(&repo.name, acting)
            );
        }
        let (sender, sender_title) = match from {
            Some(f) => {
                let (frepo, fslot, fid) = self.known_session(f)?;
                let title = self.record(&frepo, fslot).title.clone();
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
        let d = self.deliver(&repo, slot, &prompt, None).await?;
        let e = self.record(&repo, acting);
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        info!(
            repo = repo.name,
            issue = number,
            session = slot_id(&repo.name, acting),
            from = sender.as_deref().unwrap_or("a human"),
            "delivered a message"
        );
        Ok(serde_json::json!({
            "item": session_id(&repo.name, number),
            "title": st.title,
            "session": slot_id(&repo.name, acting),
            "terminal": d.handle,
            "relaunched": d.relaunched,
            "from": sender,
        }))
    }

    /// Pick up edits to the config file between passes (repos, harnesses,
    /// intervals) without a restart. The token is fixed for the process.
    fn reload_config(&mut self) {
        match Config::load() {
            Ok(cfg) => {
                self.cfg = cfg;
                self.sync_drivers();
            }
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
        let down = match self.check_drivers().await {
            Ok(down) => down,
            Err(e) => {
                warn!("skipping this pass: {e:#}");
                self.state.last_error = Some(format!("{e:#}"));
                let _ = self.state.save();
                return;
            }
        };
        // A driver that is down is a visible error even while the others
        // carry on; its repositories are skipped below.
        self.state.last_error = if down.is_empty() {
            None
        } else {
            Some(down.join("; "))
        };
        let ready: Vec<DriverKind> = self
            .startup_pending
            .iter()
            .copied()
            .filter(|k| !self.down.contains(k))
            .collect();
        if !ready.is_empty() {
            self.startup_pending.retain(|k| !ready.contains(k));
            self.resume_interrupted(&ready).await;
        }
        for repo in self.cfg.repos.clone() {
            if self.driver_down(&repo) {
                continue;
            }
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

    /// The startup pass. A daemon restart is invisible to agents (Orca keeps
    /// their terminals), but after a machine restart every session's
    /// terminal is gone, and nothing would bring one back until the next
    /// GitHub event for its item. So, once, when Orca first answers: every
    /// active session that owns its workspace is looked at, and one whose
    /// workspace still exists but has no live agent is started again through
    /// the normal delivery path (resuming its conversation when a session
    /// id was captured, fresh with the item's story otherwise) with one
    /// message saying it was interrupted. One at a time, each waiting for
    /// its harness to settle. Live sessions are not touched, and a missing
    /// workspace is left to rehydration on the next event rather than
    /// re-created on boot.
    async fn resume_interrupted(&mut self, kinds: &[DriverKind]) {
        for repo in self.cfg.repos.clone() {
            if self.driver_down(&repo) || !kinds.contains(&self.cfg.driver_for(&repo)) {
                continue;
            }
            let candidates = self.resume_candidates(&repo);
            for slot in candidates {
                let st = self.record(&repo, slot).clone();
                let Some(worktree_id) = st.worktree_id.clone() else {
                    continue;
                };
                let session = slot_id(&repo.name, slot);
                match self.driver(&repo).worktree_exists(&worktree_id).await {
                    Ok(true) => {}
                    Ok(false) => {
                        debug!(session, "workspace is gone; left to rehydration");
                        continue;
                    }
                    Err(e) => {
                        warn!(session, "could not check the workspace: {e:#}");
                        continue;
                    }
                }
                match self.driver(&repo).has_live_agent(&worktree_id).await {
                    Ok(true) => {
                        debug!(session, "agent is live; nothing to do");
                        continue;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        warn!(session, "could not list the workspace's terminals: {e:#}");
                        continue;
                    }
                }
                let text = prompt::interrupted_prompt(&prompt::Interrupted {
                    number: st.number,
                    title: &st.title,
                    url: &st.html_url,
                    branch: st.branch.as_deref(),
                    path: st.worktree_path.as_deref(),
                    reviewer: slot.is_reviewer(),
                });
                info!(session, "session was interrupted; starting it again");
                match self.deliver(&repo, slot, &text, None).await {
                    Ok(d) => {
                        let e = self.record(&repo, slot);
                        e.terminal_handle = Some(d.handle);
                        e.last_prompt_at = Some(now_iso());
                        e.prompts_sent += 1;
                    }
                    Err(e) => warn!(session, "could not start the session again: {e:#}"),
                }
                if let Err(e) = self.state.save() {
                    error!("saving state: {e:#}");
                }
            }
        }
    }

    /// Sessions the startup pass looks at: the session that acts on every
    /// active, seeded item (the item's own, or its owner's, which may itself
    /// be retired while it still owns open items and so keeps its
    /// workspace), plus active reviewer sessions, which always own theirs.
    /// A session whose workspace is gone, released or about to be removed
    /// is skipped.
    fn resume_candidates(&self, repo: &RepoConfig) -> Vec<Slot> {
        let Some(rs) = self.state.repos.get(&repo.name) else {
            return Vec::new();
        };
        let has_workspace =
            |s: &IssueState| !s.cleanup_pending && !s.release_pending && s.worktree_id.is_some();
        let owners: BTreeSet<u64> = rs
            .issues
            .values()
            .filter(|s| s.seeded && s.active)
            .map(|s| owner_in(&rs.issues, s.number))
            .collect();
        owners
            .into_iter()
            .filter(|n| rs.issues.get(n).is_some_and(&has_workspace))
            .map(Slot::Item)
            .chain(
                rs.reviewers
                    .values()
                    .filter(|s| s.seeded && s.active && has_workspace(s))
                    .map(|s| Slot::Reviewer(s.number)),
            )
            .collect()
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
            if !self.needs_look(repo, number, fresh.as_ref(), &triggers) {
                continue;
            }
            let issue = match fresh {
                Some(i) => i,
                // Not on a listing that changed, and not handled by a
                // session (`needs_look`): fetch it to see what it is now.
                None => match self.gh.issue(owner, name, number).await {
                    Ok(i) => i,
                    Err(e) => {
                        all_ok = false;
                        self.note_failure(repo, number, &e);
                        continue;
                    }
                },
            };
            match self
                .reconcile_issue(repo, owner, name, &issue, pr, triggers)
                .await
            {
                Ok(()) => {
                    self.failures.remove(&(repo.name.clone(), issue.number));
                    self.failures
                        .remove(&(reviewer_session_id(&repo.name, 0), issue.number));
                }
                Err(e) if e.downcast_ref::<ReviewerFailure>().is_some() => {
                    all_ok = false;
                    self.note_reviewer_failure(repo, issue.number, &e);
                }
                Err(e) => {
                    all_ok = false;
                    self.note_failure(repo, issue.number, &e);
                }
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
        // An ignored item that has left every listing (closed, or no longer
        // the bot's) has nothing to be ignored as; if it comes back it is
        // looked at afresh.
        rs.ignored.retain(|n, _| present.contains(n));
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

    /// Whether a pass has to look at an item found on the listings: `fresh`
    /// is the item as a changed listing reported it, `None` when every
    /// listing carrying it was a 304, and `triggers` names the listings it
    /// is on. An item a session handles is looked at when a listing
    /// carrying it changed. One nothing handles is looked at unless it was
    /// ignored and is still as it was then (see [`Ignored`]): the same
    /// `updated_at` on the same listings. An item that was ignored as
    /// created-only and now shows up as assigned (or mentioned, or with a
    /// review requested) is looked at again, and onboarded as usual, even
    /// though GitHub's `updated_at` has not moved.
    fn needs_look(
        &self,
        repo: &RepoConfig,
        number: u64,
        fresh: Option<&Issue>,
        triggers: &[String],
    ) -> bool {
        let tracked = self
            .state
            .repos
            .get(&repo.name)
            .and_then(|rs| rs.issues.get(&number))
            .is_some_and(|s| s.seeded && s.active);
        if tracked {
            return fresh.is_some();
        }
        self.state
            .repos
            .get(&repo.name)
            .and_then(|rs| rs.ignored.get(&number))
            .is_none_or(|at| !at.stands(fresh, triggers))
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
    /// anywhere else. A reviewer session (`owner/repo#N:reviewer`) is its
    /// own session, never the author's.
    fn acting_session(&self, origin: &str) -> String {
        let Some((o, reviewer)) = origin::parse_session(origin) else {
            return origin.to_string();
        };
        match self
            .cfg
            .repos
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(&o.repo))
        {
            Some(r) if reviewer => reviewer_session_id(&r.name, o.number),
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
            let Ok((srepo, sslot, sid)) = self.known_session(&sub) else {
                warn!(
                    repo = repo.name,
                    issue = issue.number,
                    subscriber = sub,
                    "subscriber is not a session ssf knows; skipping"
                );
                continue;
            };
            let sst = self.record(&srepo, sslot).clone();
            let alive = match sst.worktree_id.as_deref() {
                Some(id) => self
                    .driver(&srepo)
                    .worktree_exists(id)
                    .await
                    .unwrap_or(false),
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
            match self.deliver(&srepo, sslot, &text, None).await {
                Ok(_) => {
                    info!(
                        repo = repo.name,
                        issue = issue.number,
                        subscriber = sid,
                        events = mine.len(),
                        "told a subscriber"
                    );
                    let e = self.record(&srepo, sslot);
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

    /// A failure on the reviewer side: counted apart from the PR's own
    /// deliveries, and after enough of them the reviewer record is started
    /// over rather than the PR re-onboarded onto its author.
    fn note_reviewer_failure(&mut self, repo: &RepoConfig, number: u64, err: &anyhow::Error) {
        let key = (reviewer_session_id(&repo.name, 0), number);
        let count = self.failures.entry(key).or_insert(0);
        *count += 1;
        warn!(
            repo = repo.name,
            issue = number,
            attempt = *count,
            "reviewer session failed: {err:#}"
        );
        if *count >= MAX_DELIVERY_FAILURES {
            error!(
                repo = repo.name,
                issue = number,
                "giving up on the reviewer's workspace; it will be started over"
            );
            *count = 0;
            if let Some(rv) = self.state.repo_mut(&repo.name).reviewers.get_mut(&number) {
                rv.seeded = false;
                rv.active = false;
                rv.terminal_handle = None;
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
            driver: self.cfg.driver_for(repo),
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
        e.release_pending = false;
        e.release_forced = false;
        e.released_at = o.released_at;
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
    /// by the bot that carry none: a person typed them as the bot, or the gh
    /// shim was not in effect in whichever session made them, and nothing
    /// can tell which.
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
                    "post by @{login} without an origin tag (a person, or the gh shim not in effect)"
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
            // The bot's own commits and cross-references would only echo
            // the agent's work back at it. Things done *to* the bot, like
            // being assigned, always count. The bot's comments are kept:
            // one that carries an origin tag came from one session and may
            // be news to another, so it is sorted out per recipient
            // (`for_recipient`) instead; one without a tag was typed by a
            // person using the bot account (every session stamps its
            // posts), so it is delivered like any human's.
            let own = actor_of(ev).eq_ignore_ascii_case(&self.login);
            let echo = matches!(kind, "cross-referenced" | "referenced" | "committed");
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
        let mut existing = existing;
        if let Some(st) = existing
            .as_mut()
            .filter(|s| s.seeded && s.triggers != triggers)
        {
            let e = self.entry(repo, issue.number);
            e.triggers = triggers.clone();
            st.triggers = triggers.clone();
        }
        match existing {
            Some(st) if st.seeded && !st.active => {
                self.reactivate(repo, owner, name, issue, st).await?
            }
            Some(st) if st.seeded => {
                if st.updated_at.as_deref() != Some(issue.updated_at.as_str()) {
                    self.follow_up(repo, owner, name, issue, st).await?
                }
            }
            _ => {
                self.onboard(repo, owner, name, issue, pr, triggers.clone())
                    .await?
            }
        }
        self.reconcile_reviewer(repo, owner, name, issue, &triggers)
            .await
            .map_err(|e| e.context(ReviewerFailure))
    }

    /// A review asked of the bot on a pull request one of its own sessions
    /// wrote is not delivered to that session to act on: a reviewer session
    /// (a second workspace on the PR, subscribed to it rather than owning
    /// it) is started, followed up while the request stands, and stood down
    /// when the request is gone. A review is asked for in one of two ways:
    ///
    /// - a review request from the bot, which GitHub only allows on a pull
    ///   request the bot did not open; GitHub drops the request once the
    ///   review is posted, or it is withdrawn;
    /// - the review label (`daemon.review_label`, `review` by default) on
    ///   the pull request, the way for a bot-authored PR, since GitHub
    ///   refuses a review request from a PR's own author. The label is the
    ///   request: once the reviewer has posted a review newer than the
    ///   label, ssf removes the label and stands the reviewer down. Adding
    ///   it again asks for another look.
    ///
    /// The author keeps the PR and sees the review as activity.
    async fn reconcile_reviewer(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        issue: &Issue,
        triggers: &[String],
    ) -> Result<()> {
        let Some(st) = self
            .peek(repo, Slot::Item(issue.number))
            .cloned()
            .filter(|s| s.seeded && s.active && s.shares_workspace_of.is_some())
        else {
            return Ok(());
        };
        if !issue.is_pull_request() {
            return Ok(());
        }
        let requested = triggers.iter().any(|t| t == "review_requested");
        let label = self
            .cfg
            .daemon
            .review_label()
            .filter(|l| issue.has_label(l))
            .map(str::to_string);
        let mut asked: Vec<String> = Vec::new();
        if requested {
            asked.push("review_requested".into());
        }
        if label.is_some() {
            asked.push("review_label".into());
        }
        let rv = self.peek(repo, Slot::Reviewer(issue.number)).cloned();
        let running = rv.as_ref().is_some_and(|r| r.seeded && r.active);
        match (asked.is_empty(), running) {
            (false, false) => {
                self.start_reviewer(repo, owner, name, issue, &st, rv, asked)
                    .await
            }
            (false, true) => {
                let rv = rv.unwrap();
                if rv.triggers != asked {
                    self.record(repo, Slot::Reviewer(issue.number)).triggers = asked.clone();
                }
                // The label has no GitHub-side "fulfilled" signal: look for
                // the reviewer's review since the label was added, and clear
                // the label ourselves when there is one. The label is removed
                // before the record changes, so a failure here is retried on
                // the next pass rather than leaving the label behind.
                let mut timeline = None;
                if let Some(label) = &label
                    && rv.updated_at.as_deref() != Some(issue.updated_at.as_str())
                {
                    let me = reviewer_session_id(&repo.name, issue.number);
                    let t = self.gh.timeline(owner, name, issue.number).await?;
                    if review_posted_since_label(&t, label, &self.login, &me) {
                        info!(
                            repo = repo.name,
                            issue = issue.number,
                            label,
                            "the reviewer has posted its review; removing the label"
                        );
                        self.gh
                            .remove_label(owner, name, issue.number, label)
                            .await?;
                        return self
                            .retire_reviewer(
                                repo,
                                owner,
                                name,
                                issue,
                                ReviewEnd::Fulfilled,
                                Some(&t),
                            )
                            .await;
                    }
                    timeline = Some(t);
                }
                self.review_follow_up(repo, owner, name, issue, rv, timeline)
                    .await
            }
            (true, true) => {
                self.retire_reviewer(repo, owner, name, issue, ReviewEnd::Fulfilled, None)
                    .await
            }
            (true, false) => Ok(()),
        }
    }

    /// Start (or bring back) the reviewer session for a pull request. A
    /// reviewer that was stood down keeps its record, workspace and
    /// conversation, so a repeated request resumes where it left off with
    /// what happened in between. `asked` is what wants the review
    /// (`review_requested`, `review_label`, or both).
    #[allow(clippy::too_many_arguments)]
    async fn start_reviewer(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        issue: &Issue,
        st: &IssueState,
        prior: Option<IssueState>,
        asked: Vec<String>,
    ) -> Result<()> {
        let number = issue.number;
        let slot = Slot::Reviewer(number);
        let me = reviewer_session_id(&repo.name, number);
        let author = self.owner_of(repo, number);
        let pr = match st.pr.clone() {
            Some(p) => p,
            None => self.gh.pull(owner, name, number).await?,
        };
        if !pr.same_repo(&repo.name) || pr.head_ref.is_empty() {
            warn!(
                repo = repo.name,
                issue = number,
                "review requested on a pull request whose branch is not in this repository; \
                 leaving it to the author's session"
            );
            return Ok(());
        }
        let again = prior.as_ref().is_some_and(|p| p.seeded);
        info!(
            repo = repo.name,
            issue = number,
            author,
            again,
            ?asked,
            "review asked on a session's own pull request; {} its reviewer session",
            if again { "bringing back" } else { "starting" }
        );
        let setup = self
            .driver(repo)
            .ensure_project(
                owner,
                name,
                &repo.clone_url(),
                repo.path.as_deref(),
                &self.cfg.projects_dir(self.cfg.driver_for(repo)),
            )
            .await?;
        let timeline = self.gh.timeline(owner, name, number).await?;
        let wt_name = prior
            .as_ref()
            .and_then(|p| p.worktree_name.clone())
            .unwrap_or_else(|| prompt::review_worktree_name(number, &issue.title));
        {
            let e = self.record(repo, slot);
            e.title = issue.title.clone();
            e.html_url = issue.html_url.clone();
            e.repo_id = Some(setup.repo_id.clone());
            e.worktree_name = Some(wt_name.clone());
            e.kind = Some("reviewer".into());
            e.triggers = asked;
            e.github_state = Some("open".into());
            e.pr = Some(pr.clone());
            e.projects = st.projects.clone();
            // For a reviewer record: the session that wrote the PR.
            e.shares_workspace_of = Some(author);
            e.subscriber_only = false;
            e.cleanup_pending = false;
            e.retired_at = None;
        }
        if again {
            let prior = prior.unwrap();
            let diff = self.diff(&prior.seen, &timeline);
            let mine = self.for_recipient(&diff.rendered, &me);
            let rst = self.record(repo, slot).clone();
            let ctx = self.ctx(repo, &rst);
            let text = prompt::review_again_prompt(issue, &mine, &ctx);
            let all = self.diff(&BTreeMap::new(), &timeline).rendered;
            let all = self.for_recipient(&all, &me);
            let mut relaunch = prompt::review_prompt(issue, &all, &ctx);
            relaunch.push_str("\n\n");
            relaunch.push_str(&text);
            let d = self.deliver(repo, slot, &text, Some(&relaunch)).await?;
            if let Some(id) = self.record(repo, slot).worktree_id.clone() {
                let _ = self.driver(repo).set_status(&id, "in-progress").await;
            }
            let e = self.record(repo, slot);
            e.terminal_handle = Some(d.handle);
            e.updated_at = Some(issue.updated_at.clone());
            e.seen = diff.seen;
            e.active = true;
            e.last_prompt_at = Some(now_iso());
            e.prompts_sent += 1;
            return Ok(());
        }
        let diff = self.diff(&BTreeMap::new(), &timeline);
        let mine = self.for_recipient(&diff.rendered, &me);
        // A workspace left from an earlier, unfinished attempt is reused.
        let mut existing: Option<Worktree> = None;
        if let Some(id) = prior.as_ref().and_then(|p| p.worktree_id.clone())
            && self.driver(repo).worktree_exists(&id).await?
        {
            existing = Some(Worktree {
                id,
                path: prior
                    .as_ref()
                    .and_then(|p| p.worktree_path.clone())
                    .unwrap_or_default(),
                branch: prior.as_ref().and_then(|p| p.branch.clone()),
            });
        }
        let handle = match existing {
            Some(wt) => {
                self.remember_worktree(repo, slot, &wt);
                let rst = self.record(repo, slot).clone();
                let ctx = self.ctx(repo, &rst);
                let text = prompt::review_prompt(issue, &mine, &ctx);
                let d = self.deliver(repo, slot, &text, Some(&text)).await?;
                d.handle
            }
            None => {
                let created = self
                    .create_review_workspace(repo, &setup.repo_id, &wt_name, number, &pr)
                    .await?;
                info!(
                    repo = repo.name,
                    issue = number,
                    worktree = created.id,
                    "created the reviewer's workspace"
                );
                self.remember_worktree(repo, slot, &created);
                {
                    let e = self.record(repo, slot);
                    e.launched_at = Some(now_iso());
                    e.agent_session_id = None;
                }
                let title = format!("{} · #{} review", repo.harness, number);
                let cmd = self.launch_command(
                    repo,
                    number,
                    &issue.html_url,
                    &repo.harness_command(),
                    true,
                );
                let rst = self.record(repo, slot).clone();
                let ctx = self.ctx(repo, &rst);
                let text = prompt::review_prompt(issue, &mine, &ctx);
                let handle = self
                    .driver(repo)
                    .start(&created.id, &cmd, &title, &repo.harness, &text)
                    .await?;
                info!(
                    repo = repo.name,
                    issue = number,
                    handle,
                    "launched {} as the reviewer and sent the pull request",
                    repo.harness
                );
                handle
            }
        };
        if let Some(id) = self.record(repo, slot).worktree_id.clone() {
            let _ = self.driver(repo).set_status(&id, "in-progress").await;
        }
        let e = self.record(repo, slot);
        e.terminal_handle = Some(handle);
        e.updated_at = Some(issue.updated_at.clone());
        e.seen = diff.seen;
        e.seeded = true;
        e.active = true;
        e.bound_at = Some(now_iso());
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        tokio::time::sleep(Duration::from_secs(3)).await;
        self.capture_sessions(repo);
        Ok(())
    }

    /// A worktree for reviewing a pull request: at the PR's head, on a local
    /// branch of its own rather than the PR's, so nothing the reviewer does
    /// can move the PR. The reviewer is told how to refresh it.
    async fn create_review_workspace(
        &mut self,
        repo: &RepoConfig,
        repo_id: &str,
        wt_name: &str,
        number: u64,
        pr: &PrInfo,
    ) -> Result<Worktree> {
        let main = self.driver(repo).repo_path(repo_id).await?;
        if let Err(e) = git(&main, &["fetch", "origin", &pr.head_ref]).await {
            warn!(
                repo = repo.name,
                issue = number,
                "fetching PR branch failed: {e:#}"
            );
        }
        if self
            .driver(repo)
            .existing_branch_ref(repo_id, &pr.head_ref)
            .await?
            .is_none()
        {
            anyhow::bail!(
                "the pull request branch {} is not available in the repository checkout",
                pr.head_ref
            );
        }
        let base = format!("origin/{}", pr.head_ref);
        let comment = format!("ssf: reviewing PR #{number}");
        self.driver(repo)
            .create_worktree(repo_id, wt_name, number, &comment, Some(&base))
            .await
    }

    /// The pull request under review changed: tell the reviewer what is new.
    /// `timeline` is the PR's timeline if the caller already fetched it.
    async fn review_follow_up(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        issue: &Issue,
        rv: IssueState,
        timeline: Option<Vec<Value>>,
    ) -> Result<()> {
        if rv.updated_at.as_deref() == Some(issue.updated_at.as_str()) {
            return Ok(());
        }
        let number = issue.number;
        let slot = Slot::Reviewer(number);
        let me = reviewer_session_id(&repo.name, number);
        let timeline = match timeline {
            Some(t) => t,
            None => self.gh.timeline(owner, name, number).await?,
        };
        let diff = self.diff(&rv.seen, &timeline);
        let mine = self.for_recipient(&diff.rendered, &me);
        if mine.is_empty() {
            let e = self.record(repo, slot);
            e.updated_at = Some(issue.updated_at.clone());
            e.seen = diff.seen;
            e.title = issue.title.clone();
            return Ok(());
        }
        info!(
            repo = repo.name,
            issue = number,
            events = mine.len(),
            "delivering new activity to the reviewer"
        );
        let rv = IssueState {
            projects: self.entry(repo, number).projects.clone(),
            ..rv
        };
        let ctx = self.ctx(repo, &rv);
        let text = prompt::review_followup_prompt(issue, &mine, &ctx);
        let mut all = self.diff(&BTreeMap::new(), &timeline).rendered;
        all.retain(|r| !diff.rendered.iter().any(|n| n.key == r.key));
        let all = self.for_recipient(&all, &me);
        let mut relaunch = prompt::review_prompt(issue, &all, &ctx);
        relaunch.push_str("\n\n");
        relaunch.push_str(&text);
        let d = self.deliver(repo, slot, &text, Some(&relaunch)).await?;
        let e = self.record(repo, slot);
        e.updated_at = Some(issue.updated_at.clone());
        e.title = issue.title.clone();
        e.seen = diff.seen;
        e.projects = rv.projects;
        e.terminal_handle = Some(d.handle);
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        Ok(())
    }

    /// Stand a reviewer session down: the review request is gone, or the
    /// pull request is closed. The record and workspace stay (a repeated
    /// request resumes the same conversation) until the PR closes, when the
    /// workspace is done with like any other.
    async fn retire_reviewer(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        issue: &Issue,
        why: ReviewEnd,
        timeline: Option<&[Value]>,
    ) -> Result<()> {
        let number = issue.number;
        let slot = Slot::Reviewer(number);
        let Some(rv) = self.peek(repo, slot).cloned().filter(|r| r.seeded) else {
            return Ok(());
        };
        let closed = matches!(why, ReviewEnd::Closed { .. });
        if !rv.active && !closed {
            return Ok(());
        }
        let me = reviewer_session_id(&repo.name, number);
        info!(
            repo = repo.name,
            issue = number,
            ?why,
            "standing the reviewer session down"
        );
        let alive = match rv.worktree_id.as_deref() {
            Some(id) => self.driver(repo).worktree_exists(id).await.unwrap_or(false),
            None => false,
        };
        let mut seen = None;
        if rv.active {
            let fetched;
            let timeline = match timeline {
                Some(t) => t,
                None => {
                    fetched = self.gh.timeline(owner, name, number).await?;
                    &fetched
                }
            };
            let diff = self.diff(&rv.seen, timeline);
            let mine = self.for_recipient(&diff.rendered, &me);
            seen = Some(diff.seen);
            if alive {
                let ctx = self.ctx(repo, &rv);
                let text = prompt::review_done_prompt(issue, &mine, &ctx, why);
                match self.deliver(repo, slot, &text, None).await {
                    Ok(d) => {
                        let e = self.record(repo, slot);
                        e.terminal_handle = Some(d.handle);
                        e.last_prompt_at = Some(now_iso());
                        e.prompts_sent += 1;
                    }
                    Err(e) => warn!(
                        repo = repo.name,
                        issue = number,
                        "could not notify the reviewer: {e:#}"
                    ),
                }
            }
        }
        if closed
            && alive
            && let Some(id) = &rv.worktree_id
        {
            let _ = self.driver(repo).set_status(id, "completed").await;
        }
        // A reviewer's workspace is a read-only checkout that never holds
        // work of its own, so it still goes on its own once the agent is
        // done (run_cleanups).
        let cleanup = closed && alive;
        let e = self.record(repo, slot);
        e.active = false;
        e.title = issue.title.clone();
        e.updated_at = Some(issue.updated_at.clone());
        if let Some(seen) = seen {
            e.seen = seen;
        }
        e.github_state = Some(match why {
            ReviewEnd::Closed { merged: true } => "merged".into(),
            ReviewEnd::Closed { merged: false } => "closed".into(),
            ReviewEnd::Fulfilled => "open".into(),
        });
        e.retired_at = Some(now_iso());
        e.cleanup_pending = cleanup;
        let dropped = self.state.unsubscribe_everywhere(&me);
        if !dropped.is_empty() {
            info!(
                repo = repo.name,
                issue = number,
                ?dropped,
                "retired reviewer unsubscribed"
            );
        }
        Ok(())
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

    fn remember_worktree(&mut self, repo: &RepoConfig, slot: Slot, wt: &Worktree) {
        let e = self.record(repo, slot);
        e.worktree_id = Some(wt.id.clone());
        e.worktree_path = Some(wt.path.clone());
        if let Some((r, _)) = wt.id.split_once("::") {
            e.repo_id = Some(r.to_string());
        }
        if wt.branch.is_some() {
            e.branch = wt.branch.clone();
        }
        e.cleanup_pending = false;
        e.release_pending = false;
        e.release_forced = false;
        e.released_at = None;
        e.release_refusals = 0;
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
            .driver(repo)
            .ensure_project(
                owner,
                name,
                &repo.clone_url(),
                repo.path.as_deref(),
                &self.cfg.projects_dir(self.cfg.driver_for(repo)),
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
        // Whatever it was ignored as before, it is being looked at afresh.
        self.state
            .repo_mut(&repo.name)
            .ignored
            .remove(&issue.number);

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
            self.state
                .repo_mut(&repo.name)
                .ignored
                .insert(issue.number, Ignored::new(issue, &triggers));
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
            if self.driver(repo).worktree_exists(&id).await? {
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
                .driver(repo)
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
                self.remember_worktree(repo, Slot::Item(issue.number), &wt);
                let _ = self.driver(repo).set_comment(&wt.id, &comment).await;
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
                self.remember_worktree(repo, Slot::Item(issue.number), &created);
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
                    false,
                );
                let text = self.initial_text(repo, issue, &mine);
                let handle = self
                    .driver(repo)
                    .start(&created.id, &cmd, &title, &repo.harness, &text)
                    .await?;
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
            let _ = self.driver(repo).set_status(&id, "in-progress").await;
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
    async fn story(&mut self, repo: &RepoConfig, slot: Slot) -> Result<String> {
        let (owner, name) = repo.split()?;
        let number = slot.number();
        let issue = self.gh.issue(owner, name, number).await?;
        let timeline = self.gh.timeline(owner, name, number).await?;
        let all = self.diff(&BTreeMap::new(), &timeline).rendered;
        let me = match slot {
            Slot::Item(n) => self.acting_on(repo, n),
            Slot::Reviewer(n) => reviewer_session_id(&repo.name, n),
        };
        let all = self.for_recipient(&all, &me);
        let st = self.record(repo, slot).clone();
        let ctx = self.ctx(repo, &st);
        Ok(match slot {
            Slot::Item(_) => prompt::initial_prompt(&issue, &all, &ctx),
            Slot::Reviewer(_) => prompt::review_prompt(&issue, &all, &ctx),
        })
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
            Some(id) => self.driver(repo).worktree_exists(id).await.unwrap_or(false),
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
            let main = self.driver(repo).repo_path(repo_id).await?;
            if let Err(e) = git(&main, &["fetch", "origin", &p.head_ref]).await {
                warn!(
                    repo = repo.name,
                    issue = number,
                    "fetching PR branch failed: {e:#}"
                );
            }
            if self
                .driver(repo)
                .existing_branch_ref(repo_id, &p.head_ref)
                .await?
                .is_some()
            {
                base = Some(format!("origin/{}", p.head_ref));
                checkout = Some(p.head_ref.clone());
            }
        }
        let created = match self
            .driver(repo)
            .create_worktree(repo_id, wt_name, number, comment, base.as_deref())
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
                self.driver(repo)
                    .create_worktree(
                        repo_id,
                        wt_name,
                        number,
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
            let _ = self.driver(repo).set_status(&id, "in-progress").await;
        }
        let e = self.entry(repo, issue.number);
        e.updated_at = Some(issue.updated_at.clone());
        e.title = issue.title.clone();
        e.github_state = Some(github_state(issue, st.pr.as_ref(), false));
        e.seen = diff.seen;
        e.active = true;
        e.cleanup_pending = false;
        // A release the agent asked for before the item came back is off:
        // the session is live again in this workspace.
        e.release_pending = false;
        e.release_forced = false;
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
        // Its reviewer, if it has one, is done too.
        if issue.is_pull_request() {
            let why = if closed {
                ReviewEnd::Closed { merged }
            } else {
                ReviewEnd::Fulfilled
            };
            if let Err(e) = self
                .retire_reviewer(repo, owner, name, &issue, why, Some(&timeline))
                .await
            {
                warn!(
                    repo = repo.name,
                    issue = number,
                    "standing the reviewer down failed: {e:#}"
                );
            }
        }
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
            Some(id) => self
                .driver(repo)
                .worktree_exists(&id)
                .await
                .unwrap_or(false),
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
                let _ = self.driver(repo).set_status(id, "completed").await;
            }
        }
        // The workspace stays, whatever state it is in: the agent releases
        // it with `ssf release` when everything is on origin, and `ssf
        // purge` deals with the rest.
        let e = self.entry(repo, number);
        e.active = false;
        e.title = issue.title.clone();
        e.github_state = Some(github_state(&issue, st.pr.as_ref(), merged));
        e.updated_at = Some(issue.updated_at.clone());
        e.seen = diff.seen;
        e.retired_at = Some(now_iso());
        e.cleanup_pending = false;
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
        // The last open item bound to a retired owner: the owner's session
        // is done now (its workspace stays until released or purged).
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
    /// dependents left, is done: its workspace is marked completed in Orca
    /// and becomes a candidate for `ssf release` and `ssf purge`.
    async fn release_owner(&mut self, repo: &RepoConfig, owner: u64) {
        let o = self.entry(repo, owner).clone();
        let closed = matches!(o.github_state.as_deref(), Some("closed" | "merged"));
        if o.active || !closed || !self.active_dependents(repo, owner).is_empty() {
            return;
        }
        info!(
            repo = repo.name,
            issue = owner,
            "retired session has no open items left; its workspace can be released"
        );
        if let Some(id) = &o.worktree_id {
            let _ = self.driver(repo).set_status(id, "completed").await;
        }
        let e = self.entry(repo, owner);
        e.retired_at = Some(now_iso());
        e.cleanup_pending = false;
    }

    /// `ssf launch ...` wrapper that puts the bot credentials and issue
    /// identity into the harness's environment. The daemon's own config and
    /// state locations are passed along so the wrapper reads the same files.
    fn launch_command(
        &self,
        repo: &RepoConfig,
        number: u64,
        url: &str,
        inner: &str,
        reviewer: bool,
    ) -> String {
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
            "{prefix}{} launch --repo {} --issue {} --issue-url {}{} -- {}",
            shell_quote(&me),
            shell_quote(&repo.name),
            number,
            shell_quote(url),
            if reviewer {
                format!(" --role {}", origin::REVIEWER)
            } else {
                String::new()
            },
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
        self.deliver(repo, Slot::Item(number), text, relaunch_text)
            .await
    }

    /// `deliver_to` for any session record: an item's (routed to its owner)
    /// or a reviewer's (its own).
    async fn deliver(
        &mut self,
        repo: &RepoConfig,
        slot: Slot,
        text: &str,
        relaunch_text: Option<&str>,
    ) -> Result<Delivery> {
        let target = match slot {
            Slot::Item(n) => Slot::Item(self.owner_of(repo, n)),
            r => r,
        };
        let st = self.record(repo, target).clone();
        let alive = match st.worktree_id.as_deref() {
            Some(id) => self.driver(repo).worktree_exists(id).await?,
            None => false,
        };
        if !alive {
            self.rehydrate(repo, target).await?;
        }
        let st = self.record(repo, target).clone();
        let worktree_id = st
            .worktree_id
            .clone()
            .context("issue has no workspace bound")?;
        let live = alive
            && self
                .driver(repo)
                .has_live_agent(&worktree_id)
                .await
                .unwrap_or(false);
        let mut story = None;
        if !live && (target != slot || relaunch_text.is_none()) {
            match self.story(repo, target).await {
                Ok(s) => story = Some(format!("{s}\n\n{text}")),
                Err(e) => warn!(
                    repo = repo.name,
                    session = slot_id(&repo.name, target),
                    "could not assemble the session's story for a fresh harness: {e:#}"
                ),
            }
        }
        let relaunch_text = story.as_deref().or(relaunch_text);
        let title = match target {
            Slot::Item(n) => format!("{} · #{n}", repo.harness),
            Slot::Reviewer(n) => format!("{} · #{n} review", repo.harness),
        };
        let reviewer = target.is_reviewer();
        let resume = st
            .agent_session_id
            .as_deref()
            .and_then(|id| sessions::resume_command(&repo.harness, &repo.harness_command(), id))
            .map(|c| self.launch_command(repo, st.number, &st.html_url, &c, reviewer));
        let relaunch = self.launch_command(
            repo,
            st.number,
            &st.html_url,
            &repo.harness_command(),
            reviewer,
        );
        let d = self
            .driver(repo)
            .deliver(
                &worktree_id,
                st.terminal_handle.as_deref(),
                Relaunch {
                    command: &relaunch,
                    resume_command: resume.as_deref(),
                    harness: &repo.harness,
                    title: &title,
                    text: relaunch_text,
                },
                text,
            )
            .await?;
        if d.relaunched {
            let e = self.record(repo, target);
            e.launched_at = Some(now_iso());
            if !d.resumed {
                e.agent_session_id = None;
            }
            info!(
                repo = repo.name,
                session = slot_id(&repo.name, target),
                resumed = d.resumed,
                "harness relaunched"
            );
        }
        self.record(repo, target).terminal_handle = Some(d.handle.clone());
        if let (Slot::Item(n), Slot::Item(t)) = (slot, target)
            && t != n
        {
            self.mirror_owner(repo, n, t);
        }
        Ok(d)
    }

    /// Re-create the workspace for an issue whose Orca worktree is gone,
    /// starting from its old branch when that still exists.
    async fn rehydrate(&mut self, repo: &RepoConfig, slot: Slot) -> Result<()> {
        let number = slot.number();
        if slot.is_reviewer() {
            return self.rehydrate_reviewer(repo, number).await;
        }
        let st = self.entry(repo, number).clone();
        let repo_id = match st.repo_id.clone() {
            Some(r) => r,
            None => {
                let (owner, name) = repo.split()?;
                self.driver(repo)
                    .ensure_project(
                        owner,
                        name,
                        &repo.clone_url(),
                        repo.path.as_deref(),
                        &self.cfg.projects_dir(self.cfg.driver_for(repo)),
                    )
                    .await?
                    .repo_id
            }
        };
        if let Some(existing) = self
            .driver(repo)
            .find_worktree_for_issue(&repo_id, number)
            .await?
        {
            info!(
                repo = repo.name,
                issue = number,
                worktree = existing.id,
                "found workspace linked to the issue"
            );
            self.remember_worktree(repo, Slot::Item(number), &existing);
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
            match self.driver(repo).existing_branch_ref(&repo_id, short).await {
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
            .driver(repo)
            .create_worktree(&repo_id, &name, number, &comment, base.as_deref())
            .await
        {
            Ok(w) => w,
            Err(e) if base.is_some() => {
                warn!(
                    repo = repo.name,
                    issue = number,
                    "re-create from old branch failed ({e:#}); using default base"
                );
                self.driver(repo)
                    .create_worktree(
                        &repo_id,
                        &name,
                        number,
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
        self.remember_worktree(repo, Slot::Item(number), &created);
        let e = self.entry(repo, number);
        e.terminal_handle = None;
        e.worktree_name = Some(name);
        Ok(())
    }

    /// Re-create a reviewer's workspace: fresh at the pull request's current
    /// head rather than from the reviewer's old local branch.
    async fn rehydrate_reviewer(&mut self, repo: &RepoConfig, number: u64) -> Result<()> {
        let slot = Slot::Reviewer(number);
        let st = self.record(repo, slot).clone();
        let (owner, name) = repo.split()?;
        let repo_id = match st.repo_id.clone() {
            Some(r) => r,
            None => {
                self.driver(repo)
                    .ensure_project(
                        owner,
                        name,
                        &repo.clone_url(),
                        repo.path.as_deref(),
                        &self.cfg.projects_dir(self.cfg.driver_for(repo)),
                    )
                    .await?
                    .repo_id
            }
        };
        // The PR's own record may have a workspace linked to the same number
        // (a PR onboarded on its own); anything else linked to it is ours.
        let own = self.entry(repo, number).worktree_id.clone();
        if let Some(existing) = self
            .driver(repo)
            .find_worktree_for_issue(&repo_id, number)
            .await?
            && own.as_deref() != Some(existing.id.as_str())
        {
            info!(
                repo = repo.name,
                issue = number,
                worktree = existing.id,
                "found the reviewer's workspace linked to the pull request"
            );
            self.remember_worktree(repo, slot, &existing);
            self.record(repo, slot).terminal_handle = None;
            return Ok(());
        }
        let pr = match st.pr.clone() {
            Some(p) => p,
            None => self.gh.pull(owner, name, number).await?,
        };
        let wt_name = st
            .worktree_name
            .clone()
            .unwrap_or_else(|| prompt::review_worktree_name(number, &st.title));
        info!(
            repo = repo.name,
            issue = number,
            name = wt_name,
            "re-creating the reviewer's workspace"
        );
        let created = self
            .create_review_workspace(repo, &repo_id, &wt_name, number, &pr)
            .await?;
        self.remember_worktree(repo, slot, &created);
        let e = self.record(repo, slot);
        e.terminal_handle = None;
        e.worktree_name = Some(wt_name);
        Ok(())
    }

    /// Record harness session ids for workspaces that do not have one yet.
    fn capture_sessions(&mut self, repo: &RepoConfig) {
        if !sessions::supports_resume(&repo.harness) {
            return;
        }
        let rs = self.state.repo_mut(&repo.name);
        for st in rs.issues.values_mut().chain(rs.reviewers.values_mut()) {
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

    /// Remove the workspaces that may go: item workspaces `ssf release`
    /// approved (checked once more here, since the agent may have carried
    /// on), and reviewer workspaces once their agent has wrapped up. Item
    /// workspaces are never removed on the old close-time flag; a stale
    /// one is cleared.
    async fn run_cleanups(&mut self, repo: &RepoConfig) {
        let rs = self.state.repo_mut(&repo.name);
        let pending: Vec<(Slot, IssueState)> = rs
            .issues
            .values()
            .filter(|s| s.release_pending || s.cleanup_pending)
            .map(|s| (Slot::Item(s.number), s.clone()))
            .chain(
                rs.reviewers
                    .values()
                    .filter(|s| s.cleanup_pending && !s.active)
                    .map(|s| (Slot::Reviewer(s.number), s.clone())),
            )
            .collect();
        for (slot, st) in pending {
            if !slot.is_reviewer() {
                if st.cleanup_pending {
                    self.record(repo, slot).cleanup_pending = false;
                }
                if !st.release_pending {
                    continue;
                }
                self.finish_release(repo, st).await;
                continue;
            }
            let Some(id) = st.worktree_id.clone() else {
                self.record(repo, slot).cleanup_pending = false;
                continue;
            };
            let exists = self.driver(repo).worktree_exists(&id).await.unwrap_or(true);
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
                // Give the reviewer its wrap-up time; a session id must be
                // known so the conversation can be resumed later, unless
                // we've waited long enough anyway.
                let busy = self.driver(repo).agent_busy(&id).await.unwrap_or(false);
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
                match self.driver(repo).remove_worktree(&id).await {
                    Ok(()) => info!(
                        repo = repo.name,
                        session = slot_id(&repo.name, slot),
                        worktree = id,
                        "removed the reviewer's workspace"
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
            let e = self.record(repo, slot);
            e.cleanup_pending = false;
            e.worktree_id = None;
            e.worktree_path = None;
            e.terminal_handle = None;
        }
    }

    /// Second half of `ssf release`: the checks again, then the removal.
    async fn finish_release(&mut self, repo: &RepoConfig, st: IssueState) {
        let session = session_id(&repo.name, st.number);
        let Some(id) = st.worktree_id.clone() else {
            self.mark_released(repo, st.number);
            return;
        };
        if !self.driver(repo).worktree_exists(&id).await.unwrap_or(true) {
            info!(session, "workspace is already gone");
            self.mark_released(repo, st.number);
            return;
        }
        if st.active {
            warn!(session, "release dropped: the item is active again");
            self.drop_release(repo, st.number);
            return;
        }
        if !self.active_dependents(repo, st.number).is_empty() {
            warn!(
                session,
                "release dropped: the session owns open items again"
            );
            self.drop_release(repo, st.number);
            return;
        }
        if !st.release_forced {
            let verdict = match st.worktree_path.as_deref() {
                Some(path) => release::inspect(path).await.map(|c| c.problems()),
                None => Err(anyhow::anyhow!("no workspace path recorded")),
            };
            let problems = match verdict {
                Ok(p) => p,
                Err(e) => vec![format!("{e:#}")],
            };
            if !problems.is_empty() {
                self.refuse_release(repo, &st, problems).await;
                return;
            }
        }
        match self.driver(repo).remove_worktree(&id).await {
            Ok(()) => {
                info!(session, worktree = id, "released the workspace");
                self.mark_released(repo, st.number);
            }
            Err(e) => {
                warn!(session, "removing the workspace failed: {e:#}");
                self.drop_release(repo, st.number);
            }
        }
    }

    /// A pending release is off; a later `ssf release` starts from scratch.
    fn drop_release(&mut self, repo: &RepoConfig, number: u64) {
        let e = self.entry(repo, number);
        e.release_pending = false;
        e.release_forced = false;
    }

    /// The pass's own re-check found work in a workspace `ssf release` had
    /// approved: drop the release, count it, and tell the agent what would
    /// be lost so it can fix that and ask again. After
    /// `MAX_RELEASE_REFUSALS` the agent hears no more and the workspace is
    /// kept for a person (`release given up` in status, peers and purge).
    async fn refuse_release(&mut self, repo: &RepoConfig, st: &IssueState, problems: Vec<String>) {
        let session = session_id(&repo.name, st.number);
        let e = self.entry(repo, st.number);
        e.release_pending = false;
        e.release_forced = false;
        e.release_refusals = e.release_refusals.saturating_add(1);
        let n = e.release_refusals;
        warn!(
            session,
            ?problems,
            refusals = n,
            "release refused: the workspace changed since the checks passed"
        );
        if n > MAX_RELEASE_REFUSALS {
            return;
        }
        // Only an agent that is there hears about it; a refusal is not
        // worth starting a harness for.
        let live = match st.worktree_id.as_deref() {
            Some(id) => self.driver(repo).has_live_agent(id).await.unwrap_or(false),
            None => false,
        };
        if !live {
            debug!(session, "no live agent to tell about the refused release");
            return;
        }
        let text = prompt::release_refused_prompt(
            &repo.name,
            st.number,
            &problems,
            n,
            MAX_RELEASE_REFUSALS,
        );
        match self.deliver(repo, Slot::Item(st.number), &text, None).await {
            Ok(d) => {
                let e = self.entry(repo, st.number);
                e.terminal_handle = Some(d.handle);
                e.last_prompt_at = Some(now_iso());
                e.prompts_sent += 1;
            }
            Err(e) => warn!(
                session,
                "could not tell the agent about the refused release: {e:#}"
            ),
        }
    }

    /// The workspace of `number`'s session is gone by our hand: forget its
    /// bindings (the next event re-creates it) and record when.
    fn mark_released(&mut self, repo: &RepoConfig, number: u64) {
        let now = now_iso();
        let e = self.entry(repo, number);
        e.release_pending = false;
        e.release_forced = false;
        e.cleanup_pending = false;
        e.release_refusals = 0;
        e.worktree_id = None;
        e.worktree_path = None;
        e.terminal_handle = None;
        e.released_at = Some(now.clone());
        // Items bound to this session mirror its workspace.
        let bound: Vec<u64> = self
            .state
            .repo_mut(&repo.name)
            .issues
            .values()
            .filter(|s| s.shares_workspace_of == Some(number))
            .map(|s| s.number)
            .collect();
        for n in bound {
            let e = self.entry(repo, n);
            e.worktree_id = None;
            e.worktree_path = None;
            e.terminal_handle = None;
            e.released_at = Some(now.clone());
        }
    }

    /// `ssf release`: the session's workspace goes on the next pass if the
    /// checks pass now (and again then); `force` skips them, for a person
    /// who has looked. Refused while the session still owns open items.
    async fn release(&mut self, session: &str, force: bool) -> Result<Value> {
        let (repo, slot, id) = self.known_session(session)?;
        if slot.is_reviewer() {
            anyhow::bail!(
                "{id} is a reviewer session; its workspace is removed on its own once the review is done"
            );
        }
        let number = slot.number();
        let st = self.entry(&repo, number).clone();
        if st.active {
            anyhow::bail!(
                "{id} is still open and assigned; its workspace is in use. Close or unassign the item first"
            );
        }
        let deps = self.active_dependents(&repo, number);
        if !deps.is_empty() {
            let deps: Vec<String> = deps.iter().map(|n| format!("#{n}")).collect();
            anyhow::bail!(
                "{id} still owns open items ({}); the workspace stays until they close",
                deps.join(", ")
            );
        }
        if st.release_refusals >= MAX_RELEASE_REFUSALS && !force {
            anyhow::bail!(
                "{id}: release given up after {} refusals by the daemon's own re-check; the workspace is kept for a person (`ssf release --as {id} --force` from a shell, or `ssf purge`)",
                st.release_refusals
            );
        }
        let Some(wid) = st.worktree_id.clone() else {
            anyhow::bail!(
                "{id} has no workspace{}",
                if st.released_at.is_some() {
                    " (already released)"
                } else {
                    ""
                }
            );
        };
        // Orca not answering is not a reason to refuse: the pass checks
        // again before removing anything.
        if !self
            .driver(&repo)
            .worktree_exists(&wid)
            .await
            .unwrap_or(true)
        {
            self.mark_released(&repo, number);
            return Ok(serde_json::json!({
                "session": id, "title": st.title, "released": true, "already_gone": true,
            }));
        }
        let path = st
            .worktree_path
            .clone()
            .with_context(|| format!("{id}: no workspace path recorded"))?;
        let check = match release::inspect(&path).await {
            Ok(c) => c.to_json(),
            Err(e) => serde_json::json!({
                "state": "unknown", "safe": false, "problems": [format!("{e:#}")],
            }),
        };
        let safe = check["safe"].as_bool().unwrap_or(false);
        if !safe && !force {
            info!(session = id, "release refused: the workspace holds work");
            return Ok(serde_json::json!({
                "session": id, "title": st.title, "path": path, "released": false, "check": check,
            }));
        }
        let e = self.entry(&repo, number);
        e.release_pending = true;
        // Forced: `finish_release` must not run the checks again, and a
        // person has taken over from the agent's refused attempts.
        e.release_forced = force;
        if force {
            e.release_refusals = 0;
        }
        info!(session = id, forced = !safe, "release accepted");
        Ok(serde_json::json!({
            "session": id, "title": st.title, "path": path, "released": true,
            "forced": !safe, "pending": true, "check": check,
            "poll_interval_secs": self.cfg.daemon.poll_interval_secs,
        }))
    }

    /// Item records whose workspace `ssf purge` may look at: retired and
    /// closed, owning their workspace, with no open item bound to them, and
    /// (with `older_than_days`) retired long enough ago.
    fn purge_candidates(&self, repo: &RepoConfig, older_than_days: Option<u64>) -> Vec<IssueState> {
        let Some(rs) = self.state.repos.get(&repo.name) else {
            return Vec::new();
        };
        let cutoff = older_than_days.map(|d| chrono::Utc::now() - chrono::Duration::days(d as i64));
        rs.issues
            .values()
            .filter(|s| {
                !s.active
                    && !s.subscriber_only
                    && s.shares_workspace_of.is_none()
                    && s.worktree_id.is_some()
                    && matches!(s.github_state.as_deref(), Some("closed" | "merged"))
                    && self.active_dependents(repo, s.number).is_empty()
                    && cutoff.is_none_or(|c| {
                        s.retired_at
                            .as_deref()
                            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                            .is_some_and(|t| t < c)
                    })
            })
            .cloned()
            .collect()
    }

    /// `ssf purge`: every candidate workspace with its state; the clean and
    /// pushed ones (all of them with `force`) are removed unless `dry_run`.
    /// A workspace with a live agent terminal is only listed.
    async fn purge(
        &mut self,
        dry_run: bool,
        older_than_days: Option<u64>,
        force: bool,
    ) -> Result<Value> {
        let mut rows = Vec::new();
        for repo in self.cfg.repos.clone() {
            for st in self.purge_candidates(&repo, older_than_days) {
                let session = session_id(&repo.name, st.number);
                let id = st.worktree_id.clone().unwrap_or_default();
                let mut row = serde_json::json!({
                    "session": session,
                    "title": st.title,
                    "path": st.worktree_path,
                    "retired_at": st.retired_at,
                    "removed": false,
                    "release_given_up": st.release_refusals >= MAX_RELEASE_REFUSALS,
                });
                if !self.driver(&repo).worktree_exists(&id).await? {
                    row["state"] = "already gone".into();
                    if !dry_run {
                        self.mark_released(&repo, st.number);
                        row["removed"] = true.into();
                    }
                    rows.push(row);
                    continue;
                }
                if self.driver(&repo).has_live_agent(&id).await.unwrap_or(true) {
                    row["state"] = "agent running".into();
                    rows.push(row);
                    continue;
                }
                let (state, safe, problems) = match st.worktree_path.as_deref() {
                    Some(path) => match release::inspect(path).await {
                        Ok(c) => (c.state(), c.safe(), c.problems()),
                        Err(e) => ("unknown".into(), false, vec![format!("{e:#}")]),
                    },
                    None => (
                        "unknown".into(),
                        false,
                        vec!["no workspace path recorded".into()],
                    ),
                };
                row["state"] = state.into();
                row["problems"] = problems.into();
                if !dry_run && (safe || force) {
                    match self.driver(&repo).remove_worktree(&id).await {
                        Ok(()) => {
                            info!(
                                session,
                                worktree = id,
                                forced = !safe,
                                "purged the workspace"
                            );
                            self.mark_released(&repo, st.number);
                            row["removed"] = true.into();
                        }
                        Err(e) => {
                            warn!(session, "removing the workspace failed: {e:#}");
                            row["error"] = format!("{e:#}").into();
                        }
                    }
                }
                rows.push(row);
            }
        }
        Ok(serde_json::json!({ "dry_run": dry_run, "force": force, "workspaces": rows }))
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

/// Whether the bot's reviewer session has posted its review on the pull
/// request since the review label was last added (or at all, if the label
/// came with the pull request). A review from the reviewer session counts:
/// one tagged `role=reviewer` for this PR (`me`), or an untagged one (the
/// shim not in effect); a review tagged with another session's origin is
/// that session's doing, not the reviewer's. So does a plain comment
/// carrying the reviewer's tag: GitHub refuses approve/request-changes
/// reviews from the account that opened the PR, and a reviewer that fell
/// back to `gh pr comment` has still delivered its review. An untagged
/// comment does not count, since a person posting as the bot looks the same.
fn review_posted_since_label(timeline: &[Value], label: &str, bot: &str, me: &str) -> bool {
    let mut posted = false;
    for ev in timeline {
        let event = crate::github::value_str(ev, &["event"]);
        match event {
            Some("labeled")
                if crate::github::value_str(ev, &["label", "name"])
                    .is_some_and(|n| n.eq_ignore_ascii_case(label)) =>
            {
                posted = false;
            }
            Some("reviewed" | "commented") if actor_of(ev).eq_ignore_ascii_case(bot) => {
                let body = crate::github::value_str(ev, &["body"]).unwrap_or("");
                let from_reviewer = match origin::parse(body) {
                    Some(t) => t.session().eq_ignore_ascii_case(me),
                    None => event == Some("reviewed"),
                };
                if from_reviewer {
                    posted = true;
                }
            }
            _ => {}
        }
    }
    posted
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
            drivers: Drivers::from_list(vec![Driver::Orca(crate::orca::Orca::new(
                crate::config::OrcaConfig {
                    command: "/nonexistent/orca-for-ssf-tests".into(),
                    ..Default::default()
                },
            ))]),
            down: Vec::new(),
            login: "bot".into(),
            state: State::default(),
            failures: BTreeMap::new(),
            startup_pending: Vec::new(),
        }
    }

    #[test]
    fn drivers_follow_the_config() {
        let mut e = engine();
        let orca = repo();
        let mut herdr = repo();
        herdr.name = "o/h".into();
        herdr.driver = Some(DriverKind::Herdr);
        e.cfg.repos = vec![orca.clone(), herdr.clone()];
        // What a reloaded config that added a herdr repo does.
        e.sync_drivers();
        assert_eq!(e.driver(&orca).kind(), DriverKind::Orca);
        assert_eq!(e.driver(&herdr).kind(), DriverKind::Herdr);
        // And the other way: the default switched, Orca no longer used.
        e.cfg.driver = DriverKind::Herdr;
        e.cfg.repos = vec![herdr.clone()];
        e.sync_drivers();
        assert_eq!(e.drivers.kinds(), vec![DriverKind::Herdr]);
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
        let tagged = issue(7, "bot", Some("<!-- ssf: origin=o/r#1 -->\n\nchild"));
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
            comment(3, "bot", "<!-- ssf: origin=o/r#1 -->\n\nfrom one"),
            comment(4, "bot", "<!-- ssf: origin=o/r#3 -->\n\nfrom three"),
            comment(
                5,
                "bot",
                "<!-- ssf: origin=o/r#7 -->\n\nfrom the PR's session",
            ),
            comment(6, "bot", "<!-- ssf: origin=x/y#2 -->\n\nfrom elsewhere"),
        ];
        let d = e.diff(&BTreeMap::new(), &timeline);
        let keys: Vec<&str> = d.rendered.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "commented:1",
                "commented:2",
                "commented:3",
                "commented:4",
                "commented:5",
                "commented:6"
            ],
            "the untagged bot comment is a person's; tagged ones stay"
        );
        assert_eq!(d.seen.len(), 6, "everything is recorded as seen");
        assert!(
            d.rendered[1]
                .text
                .contains("@bot commented (not from a session) (u2):\n  > untagged bot comment"),
            "{}",
            d.rendered[1].text
        );
        assert!(d.rendered[1].origin.is_none());
        assert!(d.rendered[2].text.contains("(from the agent on o/r#1)"));

        // Session 1 (which also acts on PR 7) does not get its own posts
        // back; the person's post reaches everyone.
        let mine = e.for_recipient(&d.rendered, "o/r#1");
        let keys: Vec<&str> = mine.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["commented:1", "commented:2", "commented:4", "commented:6"]
        );
        // Session 3 sees session 1's (and the PR's) comments, not its own.
        let theirs = e.for_recipient(&d.rendered, "o/r#3");
        let keys: Vec<&str> = theirs.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "commented:1",
                "commented:2",
                "commented:3",
                "commented:5",
                "commented:6"
            ]
        );
        // Case-insensitive on the repository, like everything else.
        assert_eq!(e.for_recipient(&d.rendered, "O/R#3").len(), 5);
        assert_eq!(e.acting_session("o/r#7"), "o/r#1");
        assert_eq!(e.acting_session("x/y#2"), "x/y#2");
        assert_eq!(e.acting_session("garbage"), "garbage");
        e.cfg.daemon.include_own_events = true;
        assert_eq!(e.for_recipient(&d.rendered, "o/r#1").len(), 6);

        // The bot's commits and cross-references are still its own echo,
        // and a plain `gh` comment by the bot login is not.
        let timeline = vec![
            json!({"event":"cross-referenced","id":20,"actor":{"login":"bot"},"created_at":"t",
                "source":{"issue":{"title":"x","html_url":"u"}}}),
            json!({"event":"referenced","id":21,"actor":{"login":"bot"},"commit_id":"abc","created_at":"t"}),
            comment(22, "bot", "typed by hand as the bot"),
        ];
        e.cfg.daemon.include_own_events = false;
        let d = e.diff(&BTreeMap::new(), &timeline);
        let keys: Vec<&str> = d.rendered.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, vec!["commented:22"]);
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

    #[tokio::test]
    async fn reviewer_sessions_have_their_own_identity() {
        let mut e = engine();
        let r = repo();
        e.cfg.repos.push(r.clone());
        seeded(&mut e, 1, Some("bot/issue-1"), true);
        // PR 7 is owned by session 1 and has a reviewer session.
        seeded(&mut e, 7, None, true);
        e.entry(&r, 7).shares_workspace_of = Some(1);
        e.entry(&r, 7).title = "Fix".into();
        {
            let rv = e.record(&r, Slot::Reviewer(7));
            rv.seeded = true;
            rv.active = true;
            rv.title = "Fix".into();
            rv.shares_workspace_of = Some(1);
        }
        assert_eq!(slot_id("o/r", Slot::Item(7)), "o/r#7");
        assert_eq!(slot_id("o/r", Slot::Reviewer(7)), "o/r#7:reviewer");
        assert_eq!(Slot::Reviewer(7).number(), 7);
        // The reviewer is its own session; the PR's identity is the author's.
        assert_eq!(e.acting_session("o/r#7:reviewer"), "o/r#7:reviewer");
        assert_eq!(e.acting_session("O/R#7:reviewer"), "o/r#7:reviewer");
        assert_eq!(e.acting_session("o/r#7"), "o/r#1");
        assert_eq!(e.acting_session("x/y#7:reviewer"), "x/y#7:reviewer");
        assert_eq!(
            e.owner_of(&r, 7),
            1,
            "the reviewer record never owns the PR"
        );

        // A review by the reviewer reaches the author; the author's replies
        // reach the reviewer; neither gets its own posts back.
        let timeline = vec![
            comment(1, "alice", "please review"),
            comment(2, "bot", "<!-- ssf: origin=o/r#1 -->\n\non it"),
            json!({"event":"reviewed","id":3,"user":{"login":"bot"},"state":"changes_requested",
                "body":"<!-- ssf: origin=o/r#7 role=reviewer -->\n\nnits","created_at":"t"}),
            comment(4, "bot", "<!-- ssf: origin=o/r#1 -->\n\nfixed"),
        ];
        let d = e.diff(&BTreeMap::new(), &timeline);
        assert_eq!(d.rendered.len(), 4);
        let keys = |v: &[Rendered]| v.iter().map(|r| r.key.clone()).collect::<Vec<_>>();
        assert_eq!(
            keys(&e.for_recipient(&d.rendered, "o/r#1")),
            vec!["commented:1", "reviewed:3"]
        );
        assert_eq!(
            keys(&e.for_recipient(&d.rendered, "o/r#7:reviewer")),
            vec!["commented:1", "commented:2", "commented:4"]
        );

        // The CLI can name the reviewer.
        let (_, slot, id) = e.known_session("o/r#7:reviewer").unwrap();
        assert_eq!(slot, Slot::Reviewer(7));
        assert_eq!(id, "o/r#7:reviewer");
        let (_, slot, id) = e.known_session("o/r#7").unwrap();
        assert_eq!(slot, Slot::Item(1));
        assert_eq!(id, "o/r#1");
        assert!(
            e.known_session("o/r#1:reviewer").is_err(),
            "no reviewer on the issue"
        );
        assert!(e.peek(&r, Slot::Reviewer(1)).is_none());
        assert!(e.peek(&r, Slot::Reviewer(7)).is_some_and(|s| s.seeded));
        // Reviewer sessions are not items to subscribe to.
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#1".into(),
                target: "o/r#7:reviewer".into(),
            })
            .await;
        assert!(!resp.ok);
        assert!(
            resp.error
                .unwrap()
                .contains("subscribe to the pull request")
        );
        // But a reviewer can subscribe, as itself.
        seeded(&mut e, 3, Some("bot/issue-3"), true);
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#7:reviewer".into(),
                target: "o/r#3".into(),
            })
            .await;
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(e.entry(&r, 3).subscribers, vec!["o/r#7:reviewer"]);

        // Launch commands carry the role.
        let cmd = e.launch_command(&r, 7, "https://gh/7", "claude", true);
        assert!(cmd.contains("--issue 7 --issue-url 'https://gh/7' --role reviewer -- 'claude'"));
        let cmd = e.launch_command(&r, 7, "https://gh/7", "claude", false);
        assert!(!cmd.contains("--role"));

        // Standing a reviewer down when its PR closes: the record retires
        // and it is unsubscribed everywhere; without a workspace there is
        // nothing to clean up.
        e.record(&r, Slot::Reviewer(7)).active = false;
        let closed = issue(7, "bot", None);
        e.retire_reviewer(
            &r,
            "o",
            "r",
            &closed,
            ReviewEnd::Closed { merged: true },
            None,
        )
        .await
        .unwrap();
        let rv = e.peek(&r, Slot::Reviewer(7)).unwrap().clone();
        assert!(!rv.active);
        assert!(rv.retired_at.is_some());
        assert!(!rv.cleanup_pending);
        assert_eq!(rv.github_state.as_deref(), Some("merged"));
        assert!(e.entry(&r, 3).subscribers.is_empty());
        // A reviewer that was already stood down is left alone on a repeat.
        assert!(
            e.retire_reviewer(&r, "o", "r", &closed, ReviewEnd::Fulfilled, None)
                .await
                .is_ok()
        );
    }

    #[test]
    fn the_review_label_is_fulfilled_by_the_reviewers_review() {
        let labeled = |id: u64, name: &str| json!({"event":"labeled","id":id,"actor":{"login":"alice"},"label":{"name":name}});
        let review = |id: u64, who: &str, body: &str| json!({"event":"reviewed","id":id,"user":{"login":who},"state":"approved","body":body});
        let me = "o/r#7:reviewer";
        let posted = |t: &[Value]| review_posted_since_label(t, "review", "Bot", me);
        // Nothing yet, or a review that predates the label.
        assert!(!posted(&[]));
        assert!(!posted(&[labeled(1, "Review")]));
        assert!(!posted(&[
            review(1, "bot", "<!-- ssf: origin=o/r#7 role=reviewer -->\n\nold"),
            labeled(2, "review"),
        ]));
        // The reviewer's review after the label fulfils it; a human's, or
        // another label, does not.
        assert!(posted(&[
            labeled(1, "review"),
            review(2, "bot", "<!-- ssf: origin=o/r#7 role=reviewer -->\n\nlgtm"),
        ]));
        assert!(!posted(&[labeled(1, "review"), review(2, "alice", "lgtm")]));
        // A review with no tag (shim not in effect) still counts; one from
        // another session does not.
        assert!(posted(&[labeled(1, "review"), review(2, "bot", "lgtm")]));
        assert!(!posted(&[
            labeled(1, "review"),
            review(2, "bot", "<!-- ssf: origin=o/r#1 -->\n\nself-approved"),
        ]));
        // A label added again after the review asks for another one.
        assert!(!posted(&[
            labeled(1, "review"),
            review(2, "bot", "<!-- ssf: origin=o/r#7 role=reviewer -->\n\nlgtm"),
            labeled(3, "review"),
        ]));
        // A label that came with the pull request has no event of its own.
        assert!(posted(&[review(
            1,
            "bot",
            "<!-- ssf: origin=o/r#7 role=reviewer -->\n\nlgtm"
        )]));
        // A comment tagged as the reviewer's counts too (the fallback when
        // GitHub refuses approve/request-changes from the PR's author); an
        // untagged comment, a human's, or the author session's does not.
        let comment = |id: u64, who: &str, body: &str| json!({"event":"commented","id":id,"actor":{"login":who},"body":body});
        assert!(posted(&[
            labeled(1, "review"),
            comment(
                2,
                "bot",
                "<!-- ssf: origin=o/r#7 role=reviewer -->\n\nlooks good"
            ),
        ]));
        assert!(!posted(&[
            labeled(1, "review"),
            comment(2, "bot", "looks good")
        ]));
        assert!(!posted(&[
            labeled(1, "review"),
            comment(2, "alice", "looks good")
        ]));
        assert!(!posted(&[
            labeled(1, "review"),
            comment(2, "bot", "<!-- ssf: origin=o/r#3 -->\n\nthanks"),
        ]));
        assert!(!posted(&[
            comment(1, "bot", "<!-- ssf: origin=o/r#7 role=reviewer -->\n\nold"),
            labeled(2, "review"),
        ]));
    }

    #[test]
    fn startup_pass_looks_at_owning_active_sessions_only() {
        let mut e = engine();
        let r = repo();
        let bind = |e: &mut Engine, n: u64| {
            seeded(e, n, Some(&format!("b{n}")), true);
            e.entry(&r, n).worktree_id = Some(format!("repo::/w/{n}"));
        };
        bind(&mut e, 1); // owns its workspace: resumed
        bind(&mut e, 2); // owned by #1: its owner is the one to look at
        e.entry(&r, 2).shares_workspace_of = Some(1);
        bind(&mut e, 3); // closed, workspace released and about to go
        e.entry(&r, 3).active = false;
        e.entry(&r, 3).release_pending = true;
        bind(&mut e, 4); // retired but kept
        e.entry(&r, 4).active = false;
        seeded(&mut e, 5, None, true); // never got a workspace
        bind(&mut e, 6); // active, release approved after a reopen race
        e.entry(&r, 6).release_pending = true;
        e.entry(&r, 7).active = true; // not seeded yet
        e.entry(&r, 7).worktree_id = Some("repo::/w/7".into());
        // #11 closed while #12, bound to its workspace, is still open: the
        // workspace was kept for #12, and its harness is the one to start.
        bind(&mut e, 11);
        e.entry(&r, 11).active = false;
        bind(&mut e, 12);
        e.entry(&r, 12).shares_workspace_of = Some(11);
        // #13 closed with its workspace about to be released, even though
        // #14 still points at it: nothing to bring back. (The stale
        // close-time flag from an older daemon means the same.)
        bind(&mut e, 13);
        e.entry(&r, 13).active = false;
        e.entry(&r, 13).cleanup_pending = true;
        bind(&mut e, 14);
        e.entry(&r, 14).shares_workspace_of = Some(13);
        // Reviewers own their workspace even though the record names the
        // PR's author.
        {
            let rv = e.record(&r, Slot::Reviewer(9));
            rv.seeded = true;
            rv.active = true;
            rv.shares_workspace_of = Some(1);
            rv.worktree_id = Some("repo::/w/9-review".into());
        }
        {
            let rv = e.record(&r, Slot::Reviewer(10));
            rv.seeded = true;
            rv.active = false;
            rv.worktree_id = Some("repo::/w/10-review".into());
        }
        assert_eq!(
            e.resume_candidates(&r),
            vec![Slot::Item(1), Slot::Item(11), Slot::Reviewer(9)]
        );
        assert!(
            e.resume_candidates(&RepoConfig {
                name: "o/other".into(),
                ..Default::default()
            })
            .is_empty()
        );
    }

    #[test]
    fn last_bot_comment_is_the_final_word() {
        let timeline = vec![
            json!({"event":"commented","id":1,"user":{"login":"bot"},"body":"first <!-- ssf: origin=o/r#5 -->","html_url":"u1"}),
            json!({"event":"commented","id":2,"user":{"login":"bot"},"body":"<!-- ssf: origin=o/r#5 -->\n\ndone","html_url":"u2"}),
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

    /// Replays issue #27: an item the bot opened was assigned to it just
    /// before the first pass saw it, but that pass met it through the
    /// creator listing alone (the assignee listing was a 304 against an
    /// ETag from before the assignment), so it was ignored as created-only
    /// with an `updated_at` that already reflected the assignment. When
    /// the assignee listing carried it on a later pass, nothing had
    /// "changed", and the assignment never produced a session.
    #[test]
    fn an_assignment_seen_after_a_created_only_pass_is_not_lost() {
        let mut e = engine();
        let r = repo();
        let created = vec!["created".to_string()];
        let both = vec!["assigned".to_string(), "created".to_string()];
        let i18 = issue(18, "bot", None);

        // Pass 1: first seen on the creator listing only; nothing binds it,
        // so onboarding leaves it ignored at this updated_at and listing.
        assert!(e.needs_look(&r, 18, Some(&i18), &created));
        e.state
            .repo_mut(&r.name)
            .ignored
            .insert(18, Ignored::new(&i18, &created));

        // Pass 2: the creator listing is a 304, or reports it unchanged.
        assert!(!e.needs_look(&r, 18, None, &created));
        assert!(!e.needs_look(&r, 18, Some(&i18), &created));
        // Being subscribed to meanwhile (tracked for subscribers only, no
        // session) changes nothing about that.
        e.entry(&r, 18).subscriber_only = true;
        assert!(!e.needs_look(&r, 18, Some(&i18), &created));

        // Pass 3: the assignee listing now carries it, with the very same
        // updated_at. That is a change for us: the item is looked at, and
        // with no session it is onboarded (with `assigned` in its
        // triggers, so it is not ignored again).
        assert!(e.needs_look(&r, 18, Some(&i18), &both));
        assert!(
            e.needs_look(&r, 18, None, &both),
            "a new listing membership counts even with every listing a 304"
        );
        assert!(!both.iter().all(|t| t == "created"));
        // The same listings in another order are the same listings.
        let reversed = vec!["created".to_string(), "assigned".to_string()];
        assert!(Ignored::new(&i18, &both).stands(Some(&i18), &reversed));
        // The other human triggers count the same way.
        for t in ["mentioned", "review_requested"] {
            assert!(e.needs_look(&r, 18, Some(&i18), &[t.into(), "created".into()]));
        }

        // A change on GitHub with the same listing re-evaluates as before.
        let mut later = i18.clone();
        later.updated_at = "y".into();
        assert!(e.needs_look(&r, 18, Some(&later), &created));

        // Once it has a session, only listings that changed matter, ignored
        // or not.
        seeded(&mut e, 18, None, true);
        assert!(e.needs_look(&r, 18, Some(&i18), &created));
        assert!(!e.needs_look(&r, 18, None, &both));
        // A retired session's item falls back to the ignore record, which
        // onboarding clears (`onboard` removes it before binding).
        e.entry(&r, 18).active = false;
        assert!(!e.needs_look(&r, 18, Some(&i18), &created));
        e.state.repo_mut(&r.name).ignored.remove(&18);
        assert!(e.needs_look(&r, 18, Some(&i18), &created));
    }

    /// A stand-in for the GitHub API on a local port. It answers the four
    /// listings, records the path of every request, and fails any other
    /// request (an item or its timeline), so a test can say exactly what a
    /// pass fetched. The creator listing honours `If-None-Match` against
    /// an ETag the test can roll over (GitHub's do); the other three never
    /// answer 304, so a pass never takes the "nothing changed" shortcut.
    struct GitHubStub {
        base: String,
        hits: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        /// Open items the bot opened, as the creator listing reports them.
        created: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
        /// Bumped to give the creator listing a new ETag: the next request
        /// gets a full listing, whatever it carries.
        created_etag: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl GitHubStub {
        async fn start() -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            use std::sync::{Arc, Mutex};
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let hits: Arc<Mutex<Vec<String>>> = Arc::default();
            let created: Arc<Mutex<Vec<Value>>> = Arc::default();
            let created_etag = Arc::new(AtomicU32::new(1));
            let (h, c, v) = (hits.clone(), created.clone(), created_etag.clone());
            tokio::spawn(async move {
                let other_etags = AtomicU32::new(1);
                loop {
                    let Ok((mut sock, _)) = listener.accept().await else {
                        return;
                    };
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    loop {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let head = String::from_utf8_lossy(&buf).to_string();
                    let mut lines = head.lines();
                    let target = lines
                        .next()
                        .and_then(|l| l.split(' ').nth(1))
                        .unwrap_or("")
                        .to_string();
                    let if_none_match = lines.find_map(|l| {
                        let (k, val) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("if-none-match")
                            .then(|| val.trim().to_string())
                    });
                    h.lock().unwrap().push(target.clone());
                    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                    let (status, etag, body) =
                        if path == "/repos/o/r/issues" && query.starts_with("creator=") {
                            let etag = format!("\"c{}\"", v.load(Ordering::SeqCst));
                            if if_none_match.as_deref() == Some(etag.as_str()) {
                                ("304 Not Modified", etag, String::new())
                            } else {
                                let items = Value::Array(c.lock().unwrap().clone());
                                ("200 OK", etag, items.to_string())
                            }
                        } else if path == "/repos/o/r/issues" || path == "/repos/o/r/pulls" {
                            let n = other_etags.fetch_add(1, Ordering::SeqCst);
                            ("200 OK", format!("\"o{n}\""), "[]".to_string())
                        } else {
                            (
                                "500 Internal Server Error",
                                "\"none\"".to_string(),
                                r#"{"message":"the test expected no fetch"}"#.to_string(),
                            )
                        };
                    let resp = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nETag: {etag}\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                }
            });
            Self {
                base,
                hits,
                created,
                created_etag,
            }
        }

        /// The request paths since the last call.
        fn hits(&self) -> Vec<String> {
            std::mem::take(&mut *self.hits.lock().unwrap())
        }

        fn bump_created_etag(&self) {
            self.created_etag
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn engine_at(api_url: &str) -> Engine {
        let mut e = engine();
        e.gh = GitHub::new(api_url, "t").unwrap();
        e
    }

    /// Every hit is one of the four listings: nothing was fetched by number.
    fn assert_listings_only(hits: &[String]) {
        let fetched: Vec<&String> = hits
            .iter()
            .filter(|h| {
                let path = h.split('?').next().unwrap_or("");
                path != "/repos/o/r/issues" && path != "/repos/o/r/pulls"
            })
            .collect();
        assert!(fetched.is_empty(), "fetched by number: {fetched:?}");
        assert_eq!(hits.len(), 4, "four listings expected: {hits:?}");
    }

    /// Replays issue #34: the ignore records used to live only in memory
    /// while the listing ETags are persisted, so after a daemon restart
    /// every listing answered 304 until something changed, and the first
    /// pass that saw a change fetched every ignored item (issue and
    /// timeline) again to find nothing new. An ignored item is fetched
    /// again only when its `updated_at` moves or it appears on a listing
    /// it was not on before: not for a full listing with unchanged content
    /// (GitHub rolled its ETag over), not for a 304 with another listing
    /// changed, and not after a restart.
    #[tokio::test]
    async fn ignored_items_are_not_fetched_on_a_full_listing_or_after_a_restart() {
        let stub = GitHubStub::start().await;
        let r = repo();
        let created = vec!["created".to_string()];
        let listed = |n: u64| {
            json!({
                "number": n, "title": "t", "body": null, "html_url": format!("https://gh/{n}"),
                "state": "open", "user": {"login": "bot"}, "created_at": "x", "updated_at": "x"
            })
        };
        *stub.created.lock().unwrap() = vec![listed(18), listed(19)];

        // Both were looked at on an earlier pass and ignored as created-only.
        let mut e = engine_at(&stub.base);
        for n in [18, 19] {
            e.state
                .repo_mut(&r.name)
                .ignored
                .insert(n, Ignored::new(&issue(n, "bot", None), &created));
        }

        // Pass 1: no ETags yet, so a full creator listing carrying both,
        // unchanged. Nothing is fetched, and nothing is onboarded (which
        // would fail at Orca here and be counted as a failure).
        e.tick_repo(&r).await.unwrap();
        assert_listings_only(&stub.hits());
        assert!(e.failures.is_empty(), "{:?}", e.failures);
        assert_eq!(e.state.repos[&r.name].created_numbers, vec![18, 19]);

        // A daemon restart: the state file survives, memory does not.
        let dir = std::env::temp_dir().join(format!("ssf-engine-ignored-{}", std::process::id()));
        let path = dir.join("state.json");
        e.state.save_to(&path).unwrap();
        let mut e = engine_at(&stub.base);
        e.state = State::load_from(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            e.state.repos[&r.name]
                .ignored
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![18, 19],
            "the ignore records are persisted"
        );

        // Pass 2: GitHub's ETag rolled over, so a full listing again, with
        // the same content. Nothing is fetched.
        stub.bump_created_etag();
        e.tick_repo(&r).await.unwrap();
        let hits = stub.hits();
        assert!(hits.iter().any(|h| h.contains("creator=")), "{hits:?}");
        assert_listings_only(&hits);
        assert!(e.failures.is_empty(), "{:?}", e.failures);

        // Pass 3: the creator listing is a 304 while another listing
        // changed, so the items come from the cached numbers with no
        // fresh copy. Still nothing is fetched.
        e.tick_repo(&r).await.unwrap();
        assert_listings_only(&stub.hits());
        assert!(e.failures.is_empty(), "{:?}", e.failures);

        // Control: an item nothing remembers is fetched by number on such a
        // pass (the stub fails the fetch, which the pass survives), and its
        // neighbour is not.
        e.state.repo_mut(&r.name).ignored.remove(&19);
        e.tick_repo(&r).await.unwrap();
        let hits = stub.hits();
        assert!(
            hits.contains(&"/repos/o/r/issues/19".to_string()),
            "{hits:?}"
        );
        assert!(
            !hits.iter().any(|h| h.starts_with("/repos/o/r/issues/18")),
            "{hits:?}"
        );

        // An item that left every listing (closed, say) loses its record;
        // one still listed keeps it.
        *stub.created.lock().unwrap() = vec![listed(18)];
        stub.bump_created_etag();
        e.tick_repo(&r).await.unwrap();
        assert_eq!(
            e.state.repos[&r.name]
                .ignored
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![18]
        );
    }

    #[test]
    fn purge_candidates_are_closed_owning_sessions_without_open_dependents() {
        let mut e = engine();
        let r = repo();
        let bind = |e: &mut Engine, n: u64, state: &str, active: bool, retired: &str| {
            seeded(e, n, Some(&format!("b{n}")), active);
            let st = e.entry(&r, n);
            st.worktree_id = Some(format!("repo::/w/{n}"));
            st.worktree_path = Some(format!("/w/{n}"));
            st.github_state = Some(state.into());
            st.retired_at = Some(retired.into());
        };
        bind(&mut e, 1, "closed", false, "2026-01-01T00:00:00Z"); // yes
        bind(&mut e, 2, "merged", false, "2026-01-01T00:00:00Z"); // yes
        bind(&mut e, 3, "open", true, "2026-01-01T00:00:00Z"); // still active
        bind(&mut e, 4, "open", false, "2026-01-01T00:00:00Z"); // unassigned but open
        bind(&mut e, 5, "closed", false, "2026-01-01T00:00:00Z"); // owns open #6
        bind(&mut e, 6, "open", true, "2026-01-01T00:00:00Z");
        e.entry(&r, 6).shares_workspace_of = Some(5);
        bind(&mut e, 7, "closed", false, "2026-01-01T00:00:00Z"); // bound to #5's workspace
        e.entry(&r, 7).shares_workspace_of = Some(5);
        bind(&mut e, 8, "closed", false, "2026-01-01T00:00:00Z"); // already released
        e.entry(&r, 8).worktree_id = None;
        bind(&mut e, 9, "closed", false, "2099-01-01T00:00:00Z"); // retired "just now"
        let nums = |v: Vec<IssueState>| v.into_iter().map(|s| s.number).collect::<Vec<_>>();
        assert_eq!(nums(e.purge_candidates(&r, None)), vec![1, 2, 9]);
        assert_eq!(nums(e.purge_candidates(&r, Some(30))), vec![1, 2]);
        // Once #6 closes, #5's workspace is a candidate too.
        e.entry(&r, 6).active = false;
        assert_eq!(nums(e.purge_candidates(&r, None)), vec![1, 2, 5, 9]);
        assert!(
            e.purge_candidates(
                &RepoConfig {
                    name: "x/y".into(),
                    ..Default::default()
                },
                None
            )
            .is_empty()
        );
    }

    #[tokio::test]
    async fn release_is_refused_for_unknown_reviewer_and_owning_sessions() {
        let mut e = engine();
        let r = repo();
        e.cfg.repos.push(r.clone());
        // Not a session ssf knows.
        let resp = e
            .handle_request(Request::Release {
                session: "o/r#1".into(),
                force: false,
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("not an agent session"));
        // A reviewer session looks after itself.
        {
            let rv = e.record(&r, Slot::Reviewer(2));
            rv.seeded = true;
            rv.worktree_id = Some("repo::/w/2-review".into());
        }
        let resp = e
            .handle_request(Request::Release {
                session: "o/r#2:reviewer".into(),
                force: true,
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("reviewer session"));
        // An owner with an open item bound to it keeps its workspace, even
        // when asked through that item and even with --force.
        seeded(&mut e, 3, Some("b3"), false);
        e.entry(&r, 3).worktree_id = Some("repo::/w/3".into());
        seeded(&mut e, 4, Some("b3"), true);
        e.entry(&r, 4).shares_workspace_of = Some(3);
        let resp = e
            .handle_request(Request::Release {
                session: "o/r#4".into(),
                force: true,
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("#4"));
        assert!(!e.entry(&r, 3).release_pending);
        // Nothing to release once it is gone.
        seeded(&mut e, 5, Some("b5"), false);
        e.entry(&r, 5).released_at = Some("t".into());
        let resp = e
            .handle_request(Request::Release {
                session: "o/r#5".into(),
                force: false,
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("already released"));
    }

    #[test]
    fn marking_released_forgets_the_workspace_on_the_owner_and_its_items() {
        let mut e = engine();
        let r = repo();
        for n in [1, 2] {
            seeded(&mut e, n, Some("b1"), false);
            let st = e.entry(&r, n);
            st.worktree_id = Some("repo::/w/1".into());
            st.worktree_path = Some("/w/1".into());
            st.terminal_handle = Some("h".into());
        }
        e.entry(&r, 2).shares_workspace_of = Some(1);
        e.entry(&r, 1).release_pending = true;
        seeded(&mut e, 3, Some("b3"), false);
        e.entry(&r, 3).worktree_id = Some("repo::/w/3".into());
        e.mark_released(&r, 1);
        for n in [1, 2] {
            let st = e.entry(&r, n).clone();
            assert!(st.worktree_id.is_none(), "#{n}");
            assert!(st.worktree_path.is_none());
            assert!(st.terminal_handle.is_none());
            assert!(st.released_at.is_some());
            assert!(!st.release_pending);
        }
        assert!(e.entry(&r, 3).worktree_id.is_some());
        assert!(e.entry(&r, 3).released_at.is_none());
        // Re-creating the workspace clears the mark.
        let wt = Worktree {
            id: "repo::/w/1b".into(),
            path: "/w/1b".into(),
            branch: None,
        };
        e.remember_worktree(&r, Slot::Item(1), &wt);
        assert!(e.entry(&r, 1).released_at.is_none());
    }

    #[tokio::test]
    async fn daemon_side_refusals_are_capped_and_give_the_workspace_up() {
        let mut e = engine();
        let r = repo();
        e.cfg.repos.push(r.clone());
        seeded(&mut e, 1, Some("b1"), false);
        {
            let st = e.entry(&r, 1);
            st.worktree_id = Some("repo::/w/1".into());
            // No such directory: the re-check cannot pass.
            st.worktree_path = Some("/nonexistent/ssf-w1".into());
            st.github_state = Some("closed".into());
        }
        // The fake Orca cannot be asked, so the workspace counts as still
        // there and no agent is live to tell; the refusal is counted all
        // the same.
        for n in 1..=MAX_RELEASE_REFUSALS {
            e.entry(&r, 1).release_pending = true;
            let st = e.entry(&r, 1).clone();
            e.finish_release(&r, st).await;
            let st = e.entry(&r, 1).clone();
            assert!(!st.release_pending, "attempt {n}");
            assert_eq!(st.release_refusals, n);
            assert!(st.worktree_id.is_some(), "the workspace is kept");
            assert!(st.released_at.is_none());
        }
        // Given up: the agent's next request is refused outright...
        let resp = e
            .handle_request(Request::Release {
                session: "o/r#1".into(),
                force: false,
            })
            .await;
        assert!(!resp.ok);
        let err = resp.error.unwrap();
        assert!(err.contains("given up after 3 refusals"), "{err}");
        assert!(!e.entry(&r, 1).release_pending);
        // ...and it shows as such.
        let sessions = crate::status::sessions(&e.cfg, &e.state, None);
        assert_eq!(sessions[0].workspace_state.as_deref(), Some("given-up"));
        assert!(crate::status::render_peers(&sessions, None).contains("release given up"));
        // A person's forced release goes ahead and resets the count.
        let resp = e
            .handle_request(Request::Release {
                session: "o/r#1".into(),
                force: true,
            })
            .await;
        assert!(resp.ok, "{:?}", resp.error);
        let st = e.entry(&r, 1).clone();
        assert!(st.release_pending);
        assert!(st.release_forced, "forced: no re-check on the pass");
        assert!(st.released_at.is_none());
        assert_eq!(st.release_refusals, 0);
        // Re-creating the workspace also starts afresh.
        e.entry(&r, 1).release_refusals = MAX_RELEASE_REFUSALS;
        let wt = Worktree {
            id: "repo::/w/1b".into(),
            path: "/w/1b".into(),
            branch: None,
        };
        e.remember_worktree(&r, Slot::Item(1), &wt);
        assert_eq!(e.entry(&r, 1).release_refusals, 0);
    }

    #[test]
    fn release_refused_prompt_names_the_work_and_the_last_warning() {
        let problems = vec!["1 uncommitted change".to_string()];
        let p = prompt::release_refused_prompt("o/r", 1, &problems, 1, 3);
        assert!(p.starts_with("[ssf] Release of this workspace refused (1 of 3)"));
        assert!(p.contains("- 1 uncommitted change"));
        assert!(p.contains("run `ssf release` again"));
        let last = prompt::release_refused_prompt("o/r", 1, &problems, 3, 3);
        assert!(last.contains("will not ask again"));
        assert!(!last.contains("run `ssf release` again"));
    }

    #[tokio::test]
    async fn a_release_is_off_once_the_item_is_live_again() {
        let mut e = engine();
        let r = repo();
        e.cfg.repos.push(r.clone());
        // Open and assigned: nothing to release, not even by force.
        seeded(&mut e, 1, Some("b1"), true);
        e.entry(&r, 1).worktree_id = Some("repo::/w/1".into());
        e.entry(&r, 1).worktree_path = Some("/w/1".into());
        let resp = e
            .handle_request(Request::Release {
                session: "o/r#1".into(),
                force: true,
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("still open and assigned"));
        assert!(!e.entry(&r, 1).release_pending);
        // The reopen race: release approved, then the item came back before
        // the pass. The pass drops the release and leaves the workspace.
        {
            let st = e.entry(&r, 1);
            st.release_pending = true;
            st.release_forced = true;
            st.terminal_handle = Some("h".into());
        }
        e.run_cleanups(&r).await;
        let st = e.entry(&r, 1).clone();
        assert!(!st.release_pending);
        assert!(!st.release_forced);
        assert_eq!(st.worktree_id.as_deref(), Some("repo::/w/1"));
        assert_eq!(st.worktree_path.as_deref(), Some("/w/1"));
        assert_eq!(st.terminal_handle.as_deref(), Some("h"));
        assert!(st.released_at.is_none());
        // A dropped forced release does not make the next plain one forced.
        seeded(&mut e, 2, Some("b2"), false);
        {
            let st = e.entry(&r, 2);
            st.worktree_id = Some("repo::/w/2".into());
            st.worktree_path = Some("/nonexistent/ssf-w2".into());
            st.release_pending = true;
            st.release_forced = true;
        }
        seeded(&mut e, 3, Some("b2"), true);
        e.entry(&r, 3).shares_workspace_of = Some(2);
        e.run_cleanups(&r).await; // dependents came back: dropped
        assert!(!e.entry(&r, 2).release_pending);
        assert!(!e.entry(&r, 2).release_forced);
        e.entry(&r, 3).active = false;
        e.entry(&r, 2).release_pending = true;
        e.run_cleanups(&r).await; // plain: re-checked, and the path is gone
        let st = e.entry(&r, 2).clone();
        assert!(!st.release_pending);
        assert_eq!(st.release_refusals, 1);
        assert!(st.worktree_id.is_some());
    }
}
