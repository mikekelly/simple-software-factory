//! The reconciliation loop: GitHub assigned issues -> Orca workspaces -> agent prompts.

mod requests;

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};
use tracing::{debug, error, info, warn};

use crate::allow::{self, AllowList, Source};
use crate::config::DriverKind;
use crate::config::{Config, RepoConfig};
use crate::driver::{Driver, Drivers, Relaunch};
use crate::events::{self, Attach, Conversation, Event};
use crate::github::{Conditional, GitHub, Issue, PrInfo};
#[cfg(test)]
use crate::ipc::Request;
use crate::login::{self, LoginState, Probe};
use crate::orca::{Delivery, Worktree};
use crate::origin::{self, Origin};
use crate::prompt::{
    self, FinalComment, Fyi, ProjectPrompt, PromptContext, Rendered, actor_of, event_key,
    render_event,
};
use crate::release::{self, git};
use crate::sessions;
use crate::state::{
    Blocked, ConflictNotice, HandoverNote, Ignored, IssueState, Overrides, PendingHandover, State,
    StateLock, now_iso, owner_in,
};
use crate::status::session_id;

/// How many retired conversation ids an item keeps (see
/// `IssueState::retired_session_ids`).
const RETIRED_KEPT: usize = 8;

/// Consecutive delivery failures before an issue is re-onboarded from scratch.
const MAX_DELIVERY_FAILURES: u32 = 5;

/// Daemon-side release refusals (the re-check on the pass after `ssf
/// release` found work) before ssf stops telling the agent and leaves the
/// workspace for a person. The synchronous refusal `ssf release` prints is
/// not counted: only the daemon's own refusals can loop.
pub const MAX_RELEASE_REFUSALS: u32 = 3;

/// How long an ignored item that is open and on no listing keeps its
/// record (`Engine::prune_ignored`). A listing that came back short is
/// short for seconds or minutes, so anything still missing after this is
/// not that: the item is off the bot's listings for a reason ssf cannot
/// see (a mention edited away, an assignment withdrawn), and the record
/// is given up rather than kept for ever. Giving up costs at most one
/// onboarding, if the item ever does come back.
const ABSENT_GIVE_UP: Duration = Duration::from_secs(6 * 60 * 60);

/// How long an ignored item that is off the listings is left alone
/// between looks (`Engine::prune_ignored`), so that an item whose listing
/// flaps costs one request every so often rather than one per pass.
const ABSENT_RECHECK: Duration = Duration::from_secs(900);

/// How many absent ignore records one pass looks at, so a repository that
/// loses a long listing does not spend its pass on them; the rest are
/// looked at on the passes that follow.
const ABSENT_LOOKS_PER_PASS: usize = 20;

/// How long a blocked session waits before starting its harness again
/// when the login check cannot tell whether the login is back (or claims
/// it is while the harness disagrees); doubled after every restart that
/// comes back to the prompt, up to `LOGIN_RETRY_MAX`.
const LOGIN_RETRY: Duration = Duration::from_secs(600);
const LOGIN_RETRY_MAX: Duration = Duration::from_secs(3600);
/// How long a retirement held by the item itself waits before the item's
/// timeline is walked again. The listing that dropped it is wrong and
/// stays wrong for a while, so walking it every poll buys nothing. Only
/// the mention re-check is paced: the item itself is still read every
/// pass, so a close is still noticed at once.
const RETIREMENT_RECHECK: Duration = Duration::from_secs(600);
/// A Git command in the advisory check must not hold a daemon pass forever
/// on a broken remote or repository lock.
const CONFLICT_GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// The wait before the next restart after `retries` fruitless ones.
fn retry_wait(retries: u32) -> Duration {
    LOGIN_RETRY
        .saturating_mul(2u32.saturating_pow(retries.min(8)))
        .min(LOGIN_RETRY_MAX)
}

/// The delivery was refused because the session is blocked (its harness
/// is not signed in): the prompt is not lost, the item's bookkeeping is
/// left as it was, and the activity is delivered once the session is
/// back. Not a failure to count against the item.
#[derive(Debug, Clone)]
pub struct SessionBlocked {
    pub session: String,
    pub blocked: Blocked,
}

impl std::fmt::Display for SessionBlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = login::display_name(&self.blocked.harness);
        let what = if self.blocked.reason == Blocked::START {
            "could not be started".to_string()
        } else {
            "has been at its sign-in prompt".to_string()
        };
        write!(
            f,
            "the session on {} is blocked: {name} {what} since {}; {}",
            self.session,
            self.blocked.since,
            crate::status::fix_clause(&self.blocked)
        )
    }
}

impl std::error::Error for SessionBlocked {}

fn is_blocked(e: &anyhow::Error) -> bool {
    e.chain()
        .any(|c| c.downcast_ref::<SessionBlocked>().is_some())
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
    /// Per repository, the collaborators with push access: the allow-list
    /// when the config sets none (see `allow`). Refreshed once per pass
    /// against an ETag; a repository that has never been fetched is not
    /// polled at all (nothing is trusted on a guess).
    collaborators: BTreeMap<String, Collaborators>,
    /// Events already reported as dropped by the allow-list (`repo:key`),
    /// so each is an info line once and debug after: `diff` walks whole
    /// timelines again for relaunch texts and stories.
    dropped_logged: std::sync::Mutex<BTreeSet<String>>,
    /// Whether a harness is signed in where this daemon runs
    /// (`login::probe`; the tests supply their own).
    probe: std::sync::Arc<dyn Fn(&str) -> Probe + Send + Sync>,
    /// Whether a harness is installed where this daemon runs
    /// (`agents::installed`; the tests supply their own).
    installed: std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// The probes run this pass, by harness: one per harness per pass
    /// however many sessions are blocked.
    probes: BTreeMap<String, Probe>,
    /// Repositories whose next pass fetches every listing in full (a
    /// session came back mid-pass and its held activity is owed). Unlike
    /// `probes` above it, this is not per-pass state: the whole point is
    /// that it survives from the pass that arms it to the one that spends
    /// it, so it must never be cleared at the top of a pass.
    refetch: BTreeSet<String>,
    /// The startup pass (`resume_interrupted`) is under way: a harness
    /// started again now is `resumed` after a restart, not a lost terminal.
    startup_pass: bool,
    /// The item (repository name, number) whose onboarding onto a kept
    /// workspace is delivering right now: a relaunch for it is told of in
    /// that onboarding's `attached`, not as a `resumed` of its own.
    onboarding: Option<(String, u64)>,
    /// When each repository's conflict check last ran. This is deliberately
    /// in-memory: a restart gets one fresh check rather than trusting an old
    /// scheduling timestamp.
    conflict_checks: BTreeMap<String, Instant>,
    /// Merge simulations keyed by repository and branch, reused while the
    /// base and branch commit pair remains unchanged.
    conflict_pairs: BTreeMap<(String, String), ConflictPair>,
    /// Held from construction through shutdown, before the state is ever
    /// read. A one-shot engine uses the same guard as the daemon. Declared
    /// last so it drops only after the rest of the engine.
    _state_lock: Option<StateLock>,
}

/// The cached collaborator list of one repository.
#[derive(Debug, Clone, Default)]
struct Collaborators {
    /// Logins with push access, as GitHub gave them.
    logins: Vec<String>,
    etag: Option<String>,
}

#[derive(Debug, Clone)]
struct ConflictPair {
    base_ref: String,
    base_sha: String,
    branch_sha: String,
    conflict: bool,
    files: Vec<String>,
}

/// Run one Git command for the conflict check, retaining its exit status so
/// `merge-tree` can distinguish a real conflict from a failed invocation.
/// The child is killed when the bounded command future is dropped.
async fn conflict_git_status(path: &str, args: &[&str]) -> Result<std::process::Output> {
    let mut command = tokio::process::Command::new("git");
    command
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .kill_on_drop(true);
    let out = tokio::time::timeout(CONFLICT_GIT_TIMEOUT, command.output())
        .await
        .with_context(|| format!("git {} timed out", args.join(" ")))??;
    Ok(out)
}

async fn conflict_git(path: &str, args: &[&str]) -> Result<String> {
    let out = conflict_git_status(path, args).await?;
    if !out.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn normalize_branch(branch: &str) -> String {
    branch
        .trim()
        .strip_prefix("refs/heads/")
        .unwrap_or(branch.trim())
        .to_string()
}

fn base_ref_candidates(base: &str) -> Result<Vec<String>> {
    let base = base.trim();
    if let Some(remote) = base.strip_prefix("refs/remotes/origin/") {
        return Ok(vec![format!("refs/remotes/origin/{remote}")]);
    }
    if let Some(remote) = base.strip_prefix("origin/") {
        return Ok(vec![format!("refs/remotes/origin/{remote}")]);
    }
    if let Some(local) = base.strip_prefix("refs/heads/") {
        return Ok(vec![format!("refs/remotes/origin/{local}")]);
    }
    if base.starts_with("refs/") {
        anyhow::bail!("configured base {base:?} is not an origin branch")
    }
    Ok(vec![format!("refs/remotes/origin/{base}")])
}

fn base_remote_branch(base: &str) -> Result<String> {
    let base = base.trim();
    let branch = base
        .strip_prefix("refs/remotes/origin/")
        .or_else(|| base.strip_prefix("origin/"))
        .or_else(|| base.strip_prefix("refs/heads/"))
        .unwrap_or(base);
    if branch.is_empty() || branch.starts_with("refs/") {
        anyhow::bail!("configured base {base:?} is not an origin branch")
    }
    Ok(branch.to_string())
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
            absent_since: None,
            asked_at: None,
        }
    }

    /// Whether the item is still as it was: unchanged on GitHub (or on no
    /// listing that changed, when `fresh` is `None`), and on no listing it
    /// was not on when it was ignored. A listing it has *left* is not a
    /// change worth looking at — nothing has happened to the item, and a
    /// listing that comes back short would otherwise put every item on it
    /// through onboarding again (issue #138) — but one it has joined is
    /// what an assignment older than the `updated_at` looks like.
    fn stands(&self, fresh: Option<&Issue>, triggers: &[String]) -> bool {
        fresh.is_none_or(|i| i.updated_at == self.updated_at)
            && triggers.iter().all(|t| self.triggers.contains(t))
    }
}

/// An item's whole story as a new session is told it, and what telling
/// it counts as: everything in the timeline is now seen, so the pass that
/// follows does not deliver the same events again (`Engine::onboard`
/// records the same two things for the session it starts).
struct Story {
    text: String,
    seen: BTreeMap<String, String>,
    updated_at: String,
}

/// New events for an issue relative to what has been delivered already.
struct Diff {
    rendered: Vec<Rendered>,
    /// Every key/marker observed (including filtered ones), to be recorded as seen.
    seen: BTreeMap<String, String>,
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

    /// What one item's session runs with: the repository's config with
    /// the item's own launch overrides applied (`ssf handover`). Every
    /// launch, resume, relaunch, login check and event of that item goes
    /// through this rather than through `repo` itself, or a handed-over
    /// session would be started with the old harness's flags or probed as
    /// the wrong harness.
    fn effective(&self, repo: &RepoConfig, number: u64) -> RepoConfig {
        repo.with_overrides(self.overrides_of(repo, number).as_ref())
    }

    /// The overrides that govern an item: its own, or, for an item bound
    /// to another item's session, that session's (they share the
    /// workspace, so they share the harness in it).
    fn overrides_of(&self, repo: &RepoConfig, number: u64) -> Option<Overrides> {
        let owner = self.owner_of(repo, number);
        self.peek(repo, owner).and_then(|s| s.overrides.clone())
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
        // Take the lock before looking up credentials or reading state: a
        // rejected `ssf-server --once` must not touch a live daemon's state.
        let state_lock = StateLock::acquire()?;
        refuse_live_daemon()?;
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
            collaborators: BTreeMap::new(),
            dropped_logged: std::sync::Mutex::new(BTreeSet::new()),
            probe: std::sync::Arc::new(login::probe),
            installed: std::sync::Arc::new(crate::agents::installed),
            probes: BTreeMap::new(),
            refetch: BTreeSet::new(),
            startup_pass: false,
            onboarding: None,
            conflict_checks: BTreeMap::new(),
            conflict_pairs: BTreeMap::new(),
            _state_lock: Some(state_lock),
        })
    }

    /// Who may drive a repository: its configured list, else the instance
    /// list, else the collaborators fetched this pass (an empty list until
    /// they have been, so nothing slips through on a guess).
    fn allow_list(&self, repo: &RepoConfig) -> AllowList {
        match self.cfg.allowed_users(repo) {
            Some((list, source)) => {
                AllowList::new(&self.login, list.iter().map(String::as_str), source)
            }
            None => AllowList::new(
                &self.login,
                self.collaborators
                    .get(&repo.name)
                    .map(|c| c.logins.iter().map(String::as_str))
                    .into_iter()
                    .flatten(),
                Source::Collaborators,
            ),
        }
    }

    /// Bring the collaborator list of a repository up to date, when the
    /// config leaves the allow-list to it. A fetch that fails keeps the
    /// last good list with a warning; with none cached the pass fails for
    /// this repository rather than running open or shut on a guess.
    async fn refresh_collaborators(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
    ) -> Result<()> {
        if self.cfg.allowed_users(repo).is_some() {
            return Ok(());
        }
        let etag = self
            .collaborators
            .get(&repo.name)
            .and_then(|c| c.etag.clone());
        match self.gh.collaborators(owner, name, etag.as_deref()).await {
            Ok(Conditional::NotModified) => Ok(()),
            Ok(Conditional::Modified { value, etag }) => {
                let logins = allow::pushers(&value);
                let before = self.collaborators.get(&repo.name).map(|c| &c.logins);
                if before != Some(&logins) {
                    info!(
                        repo = repo.name,
                        logins = logins.join(", "),
                        "allowed users are the collaborators with push access"
                    );
                }
                self.collaborators
                    .insert(repo.name.clone(), Collaborators { logins, etag });
                Ok(())
            }
            Err(e) if self.collaborators.contains_key(&repo.name) => {
                warn!(
                    repo = repo.name,
                    "collaborators could not be refreshed; keeping the last list: {e:#}"
                );
                Ok(())
            }
            Err(e) => Err(e.context(format!(
                "collaborators of {} could not be fetched and no allowed_users is configured; \
                 nothing is acted on until one of the two works (see `ssf doctor`)",
                repo.name
            ))),
        }
    }

    /// Whether whoever asked the bot onto an item (see `allow::askers`) is
    /// allowed to; `Err` says who was not, for the log.
    fn gate(
        &self,
        repo: &RepoConfig,
        issue: &Issue,
        timeline: &[Value],
        triggers: &[String],
    ) -> std::result::Result<(), String> {
        let asks = allow::askers(issue, timeline, triggers, &self.login);
        allow::check(&self.allow_list(repo), &asks)
    }

    /// Log an event left out of a delivery because of the allow-list: an
    /// info line the first time, debug after (`diff` sees the same events
    /// again whenever it builds a relaunch text or a story). Project
    /// automation and other `[bot]` accounts fire on every card move, so
    /// they are debug from the start.
    fn dropped(&self, repo: &RepoConfig, key: &str, who: &str) {
        let first = self
            .dropped_logged
            .lock()
            .map(|mut set| set.insert(format!("{}:{key}", repo.name)))
            .unwrap_or(false);
        if first && !allow::is_bot_account(who) {
            info!(
                repo = repo.name,
                key,
                actor = who,
                "dropping event: @{who} is not an allowed user"
            );
        } else {
            debug!(
                repo = repo.name,
                key,
                actor = who,
                "dropping event by @{who}, not an allowed user"
            );
        }
    }

    /// An item nobody allowed asked for: said once, and not looked at again
    /// until it changes (an allowed user assigning or mentioning the bot
    /// later brings it in).
    fn refuse(&mut self, repo: &RepoConfig, issue: &Issue, triggers: &[String], why: &str) {
        info!(
            repo = repo.name,
            issue = issue.number,
            "ignoring {}: {why}",
            issue.html_url
        );
        self.state
            .repo_mut(&repo.name)
            .ignored
            .insert(issue.number, Ignored::new(issue, triggers));
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
            let wait = Duration::from_secs(self.cfg.daemon.startup_driver_wait_secs);
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

    /// The record of an item, if there is one.
    fn peek(&self, repo: &RepoConfig, number: u64) -> Option<&IssueState> {
        self.state.repos.get(&repo.name)?.issues.get(&number)
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

    /// Check the owning sessions in one repository for a merge conflict with
    /// its current base. The check is paced independently of GitHub polling,
    /// and does nothing when no eligible session has a live agent to receive
    /// the advisory.
    async fn check_conflicts(&mut self, repo: &RepoConfig) -> Result<()> {
        let interval = Duration::from_secs(self.cfg.conflict_check_interval_secs(repo));
        if interval.is_zero() {
            return Ok(());
        }
        if self
            .conflict_checks
            .get(&repo.name)
            .is_some_and(|last| last.elapsed() < interval)
        {
            return Ok(());
        }

        let candidates = self.conflict_candidates(repo).await;
        if candidates.is_empty() {
            return Ok(());
        }
        // Set this before Git work so a failed remote or a locked checkout is
        // retried at the configured cadence rather than on every ten-second
        // daemon pass.
        self.conflict_checks
            .insert(repo.name.clone(), Instant::now());

        let root = self.conflict_repo_root(repo, &candidates).await?;
        let (base_ref, base_sha) = self.conflict_base(&root, repo).await?;
        let base_name = base_ref
            .strip_prefix("refs/remotes/")
            .or_else(|| base_ref.strip_prefix("refs/heads/"))
            .unwrap_or(&base_ref);

        for st in candidates {
            let Some(branch) = st.branch.as_deref().map(normalize_branch) else {
                continue;
            };
            let Some(worktree) = st.worktree_path.as_deref() else {
                continue;
            };
            let Ok(actual) =
                conflict_git(worktree, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await
            else {
                warn!(
                    repo = repo.name,
                    issue = st.number,
                    "could not read the session worktree branch for conflict check"
                );
                continue;
            };
            if normalize_branch(&actual) != branch {
                warn!(
                    repo = repo.name,
                    issue = st.number,
                    expected = branch,
                    actual,
                    "session worktree branch differs from state; skipping conflict check"
                );
                continue;
            }
            let Ok(branch_sha) = conflict_git(worktree, &["rev-parse", "--verify", "HEAD"]).await
            else {
                warn!(
                    repo = repo.name,
                    issue = st.number,
                    "could not read the session branch commit for conflict check"
                );
                continue;
            };
            let key = (repo.name.clone(), branch.clone());
            let pair = match self.conflict_pairs.get(&key) {
                Some(pair) if pair.base_sha == base_sha && pair.branch_sha == branch_sha => {
                    let mut pair = pair.clone();
                    pair.base_ref = base_ref.clone();
                    pair
                }
                _ => match self.simulate_conflict(&root, &base_sha, &branch_sha).await {
                    Ok((conflict, files)) => {
                        let pair = ConflictPair {
                            base_ref: base_ref.clone(),
                            base_sha: base_sha.clone(),
                            branch_sha: branch_sha.clone(),
                            conflict,
                            files,
                        };
                        self.conflict_pairs.insert(key, pair.clone());
                        pair
                    }
                    Err(e) => {
                        warn!(
                            repo = repo.name,
                            issue = st.number,
                            branch,
                            "could not simulate merge for conflict check: {e:#}"
                        );
                        continue;
                    }
                },
            };
            let fingerprint = ConflictNotice {
                base_ref: pair.base_ref.clone(),
                base_sha: pair.base_sha.clone(),
                branch_sha: pair.branch_sha.clone(),
            };
            if !pair.conflict {
                self.entry(repo, st.number).conflict_notice = None;
                continue;
            }
            if self
                .peek(repo, st.number)
                .and_then(|s| s.conflict_notice.as_ref())
                == Some(&fingerprint)
            {
                continue;
            }
            let text = prompt::conflict_prompt(base_name, &base_sha, &pair.files);
            match self.deliver_to(repo, st.number, &text, None).await {
                Ok(d) => {
                    let e = self.entry(repo, st.number);
                    e.terminal_handle = Some(d.handle);
                    e.last_prompt_at = Some(now_iso());
                    e.prompts_sent += 1;
                    e.conflict_notice = Some(fingerprint);
                }
                Err(e) => warn!(
                    repo = repo.name,
                    issue = st.number,
                    "could not tell the session about its branch conflict: {e:#}"
                ),
            }
        }
        Ok(())
    }

    /// Owning sessions are selected before any Git work, so a repository with
    /// only retired, released, blocked, handed-over or bound-child records
    /// does not fetch merely because it remains configured.
    async fn conflict_candidates(&self, repo: &RepoConfig) -> Vec<IssueState> {
        let Some(rs) = self.state.repos.get(&repo.name) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for st in rs.issues.values() {
            if !(st.seeded
                && st.active
                && st.shares_workspace_of.is_none()
                && st.retired_at.is_none()
                && st.released_at.is_none()
                && !st.release_pending
                && !st.cleanup_pending
                && st.handover.is_none()
                && st.blocked.is_none()
                && st.worktree_id.is_some()
                && st.worktree_path.is_some()
                && st.branch.is_some())
            {
                continue;
            }
            let Some(id) = st.worktree_id.as_deref() else {
                continue;
            };
            if self.driver(repo).has_live_agent(id).await.unwrap_or(false) {
                out.push(st.clone());
            }
        }
        out
    }

    async fn conflict_repo_root(
        &self,
        repo: &RepoConfig,
        candidates: &[IssueState],
    ) -> Result<String> {
        if let Some(path) = repo.path.as_deref() {
            return Ok(path.to_string());
        }
        let st = candidates
            .first()
            .context("eligible conflict-check session has no workspace")?;
        let repo_id = st
            .repo_id
            .as_deref()
            .context("eligible conflict-check session has no repository")?;
        self.driver(repo).repo_path(repo_id).await
    }

    async fn conflict_base(&self, root: &str, repo: &RepoConfig) -> Result<(String, String)> {
        let configured = match repo.base_branch.as_deref().map(str::trim) {
            Some(b) if !b.is_empty() => b.to_string(),
            _ => conflict_default_base(root).await?,
        };
        let configured = match configured.as_str() {
            "origin/HEAD" | "refs/remotes/origin/HEAD" => conflict_git(
                root,
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    "refs/remotes/origin/HEAD",
                ],
            )
            .await
            .context("resolving origin/HEAD")?,
            _ => configured,
        };
        let remote_branch = base_remote_branch(&configured)?;
        let refspec = format!("+refs/heads/{remote_branch}:refs/remotes/origin/{remote_branch}");
        conflict_git(root, &["fetch", "--quiet", "origin", &refspec])
            .await
            .with_context(|| {
                format!("fetching origin/{remote_branch} for conflict checks in {root}")
            })?;
        let refs = base_ref_candidates(&configured)?;
        for reference in refs {
            if let Ok(sha) =
                conflict_git(root, &["rev-parse", "--verify", "--quiet", &reference]).await
            {
                return Ok((reference, sha));
            }
        }
        anyhow::bail!("base branch {configured} not found in {root}")
    }

    async fn simulate_conflict(
        &self,
        root: &str,
        base_sha: &str,
        branch_sha: &str,
    ) -> Result<(bool, Vec<String>)> {
        let args = [
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--messages",
            "-z",
            base_sha,
            branch_sha,
        ];
        let out = conflict_git_status(root, &args).await?;
        if out.status.success() {
            return Ok((false, Vec::new()));
        }
        if out.status.code() != Some(1) {
            anyhow::bail!(
                "git merge-tree failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        if !out.stderr.is_empty() {
            anyhow::bail!(
                "git merge-tree failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let records: Vec<&[u8]> = out.stdout.split(|b| *b == 0).collect();
        let mut files = BTreeSet::new();
        // With --name-only -z, merge-tree puts the merged tree oid first,
        // then the affected paths, then an empty record before its message
        // records. Keeping the raw bytes preserves filenames containing
        // whitespace or newlines and also covers rename/delete conflicts
        // whose prose does not say "Merge conflict in".
        if let Some((_, paths)) = records.split_first() {
            for path in paths.iter().take_while(|p| !p.is_empty()) {
                let file = String::from_utf8_lossy(path);
                if !file.is_empty() {
                    files.insert(file.to_string());
                }
            }
        }
        Ok((true, files.into_iter().collect()))
    }

    /// One reconciliation pass over every configured repo.
    pub async fn tick(&mut self) {
        self.reload_config();
        self.state.last_poll_at = Some(now_iso());
        // Per-pass state only. `refetch` is deliberately not reset here: it
        // has to outlive the pass that armed it (issue #141).
        self.probes.clear();
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
            // Before anything is delivered or resumed: a session that has
            // been handed over is replaced first.
            self.run_handovers(&repo).await;
            if let Err(e) = self.tick_repo(&repo).await {
                warn!(repo = repo.name, "pass failed: {e:#}");
                self.state.last_error = Some(format!("{}: {e:#}", repo.name));
            }
            if let Err(e) = self.check_conflicts(&repo).await {
                warn!(repo = repo.name, "branch conflict check failed: {e:#}");
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
        self.startup_pass = true;
        for repo in self.cfg.repos.clone() {
            if self.driver_down(&repo) || !kinds.contains(&self.cfg.driver_for(&repo)) {
                continue;
            }
            // A session started fresh gets its item's story, which is
            // filtered by the allow-list: the collaborators have to be
            // known first, or every human post would be left out of it.
            let refreshed = match repo.split() {
                Ok((owner, name)) => self.refresh_collaborators(&repo, owner, name).await,
                Err(e) => Err(e),
            };
            if let Err(e) = refreshed {
                warn!(
                    repo = repo.name,
                    "skipping the startup pass for this repository: {e:#}"
                );
                continue;
            }
            let candidates = self.resume_candidates(&repo);
            for number in candidates {
                let st = self.entry(&repo, number).clone();
                let Some(worktree_id) = st.worktree_id.clone() else {
                    continue;
                };
                let session = session_id(&repo.name, number);
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
                });
                info!(session, "session was interrupted; starting it again");
                match self.deliver_to(&repo, number, &text, None).await {
                    Ok(d) => {
                        let e = self.entry(&repo, number);
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
        self.startup_pass = false;
    }

    /// Sessions the startup pass looks at: the session that acts on every
    /// active, seeded item (the item's own, or its owner's, which may itself
    /// be retired while it still owns open items and so keeps its
    /// workspace). A session whose workspace is gone, released or about to
    /// be removed is skipped.
    fn resume_candidates(&self, repo: &RepoConfig) -> Vec<u64> {
        let Some(rs) = self.state.repos.get(&repo.name) else {
            return Vec::new();
        };
        // A session with a handover pending is left alone: the pass ends
        // it and starts the new one, and bringing the old harness back
        // only to stop it would waste a launch (and a login check).
        let has_workspace = |s: &IssueState| {
            !s.cleanup_pending
                && !s.release_pending
                && s.handover.is_none()
                && s.worktree_id.is_some()
        };
        let owners: BTreeSet<u64> = rs
            .issues
            .values()
            .filter(|s| s.seeded && s.active)
            .map(|s| owner_in(&rs.issues, s.number))
            .collect();
        owners
            .into_iter()
            .filter(|n| rs.issues.get(n).is_some_and(&has_workspace))
            .collect()
    }

    async fn tick_repo(&mut self, repo: &RepoConfig) -> Result<()> {
        let (owner, name) = repo.split()?;
        self.refresh_collaborators(repo, owner, name).await?;
        // Sessions whose harness sits at a login prompt are found (and
        // brought back) before anything is delivered this pass.
        self.check_logins(repo).await;
        if self.refetch.remove(&repo.name) {
            self.clear_etags(repo);
        }
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
        // An item on a listing again settles whatever a hiccup held, even
        // when nothing else about it needs looking at this pass, so a
        // later hold is a new incident and starts its count again.
        for n in &present {
            if self
                .state
                .repos
                .get(&repo.name)
                .and_then(|r| r.issues.get(n))
                .is_some_and(|s| s.retirement_held_at.is_some())
            {
                self.clear_hold(repo, *n);
            }
        }
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
                        self.note_failure(repo, number, &e).await;
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
                }
                // Held, not failed: the item is looked at again when its
                // listing changes, and in full once the session is back.
                Err(e) if is_blocked(&e) => {
                    debug!(repo = repo.name, issue = issue.number, "held: {e:#}");
                }
                Err(e) => {
                    all_ok = false;
                    self.note_failure(repo, issue.number, &e).await;
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
            // A blocked session cannot be told its item closed; the item
            // stays active in the record until the session is back.
            let acting = self.owner_of(repo, number);
            if self.peek(repo, acting).is_some_and(|s| s.blocked.is_some()) {
                debug!(
                    repo = repo.name,
                    issue = number,
                    "retirement held: the session is blocked"
                );
                continue;
            }
            match self.retire_issue(repo, owner, name, number).await {
                Ok(()) => {}
                Err(e) if is_blocked(&e) => {
                    debug!(repo = repo.name, issue = number, "retirement held: {e:#}");
                }
                Err(e) => {
                    all_ok = false;
                    warn!(repo = repo.name, issue = number, "retiring failed: {e:#}");
                }
            }
        }

        self.prune_ignored(repo, owner, name, &present).await;

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

    /// Forget the ignore records that have nothing left to guard.
    ///
    /// A record only does anything while its item is on a listing, so an
    /// item missing from this pass's listings is no reason to drop one:
    /// GitHub's filtered listings come back short now and then, and
    /// dropping a record on absence alone means every item missing from
    /// one short listing is onboarded again when the listing recovers
    /// (issue #138: 21 of them, on and off for hours). `retire_issue`
    /// has guarded sessions against the same lag from the start.
    ///
    /// So a missing item is looked at instead: its record goes if the
    /// item is closed, or gone from GitHub altogether, and stands
    /// otherwise. A record whose item stays off every listing for
    /// [`ABSENT_GIVE_UP`] is given up then, without a request: that is no
    /// longer a listing that lagged, and a record nothing can consult is
    /// not worth keeping for ever. Both clocks are in the record itself,
    /// so a daemon restart does not set them back.
    ///
    /// The looks are rationed, the giving up is not: a record is looked at
    /// at most once per [`ABSENT_RECHECK`], longest-unlooked-at first and
    /// [`ABSENT_LOOKS_PER_PASS`] to a pass, so neither a listing that
    /// flaps nor one that drops a hundred items at once spends a pass or
    /// the rate limit, and every record's turn comes round.
    async fn prune_ignored(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        present: &BTreeSet<u64>,
    ) {
        let mut absent: Vec<(u64, Ignored)> = {
            let Some(rs) = self.state.repos.get_mut(&repo.name) else {
                return;
            };
            // Back on a listing: the absence is over. What was asked about
            // it stands, so a flapping listing does not buy a look every
            // time the item comes back.
            for (_, at) in rs.ignored.iter_mut().filter(|(n, _)| present.contains(n)) {
                at.absent_since = None;
            }
            rs.ignored
                .iter_mut()
                .filter(|(n, _)| !present.contains(n))
                .map(|(n, at)| {
                    // A stamp that cannot be read (a hand-edited state
                    // file, a clock that went backwards) is replaced
                    // rather than believed: an unreadable one would
                    // otherwise read as "just now" for ever, and freeze
                    // both the look and the giving up.
                    if at
                        .absent_since
                        .as_deref()
                        .is_some_and(|t| since(t).is_none())
                    {
                        at.absent_since = Some(now_iso());
                    }
                    if at.asked_at.as_deref().is_some_and(|t| since(t).is_none()) {
                        at.asked_at = None;
                    }
                    at.absent_since.get_or_insert_with(now_iso);
                    (*n, at.clone())
                })
                .collect()
        };

        // Giving up costs nothing, so it is not rationed.
        absent.retain(|(number, at)| {
            let absent_for = at
                .absent_since
                .as_deref()
                .and_then(since)
                .unwrap_or_default();
            if absent_for < ABSENT_GIVE_UP {
                return true;
            }
            info!(
                repo = repo.name,
                issue = number,
                hours = ABSENT_GIVE_UP.as_secs() / 3600,
                "on no listing for hours; giving up its ignore record"
            );
            self.state.repo_mut(&repo.name).ignored.remove(number);
            false
        });

        // The one asked about longest ago goes first, and one never asked
        // about before that, so a repository with more absent records than
        // a pass looks at works through them rather than round the first.
        absent.retain(|(_, at)| {
            at.asked_at
                .as_deref()
                .and_then(since)
                .is_none_or(|d| d >= ABSENT_RECHECK)
        });
        absent.sort_by(|a, b| a.1.asked_at.cmp(&b.1.asked_at));
        if absent.len() > ABSENT_LOOKS_PER_PASS {
            debug!(
                repo = repo.name,
                absent = absent.len(),
                "more absent ignore records than one pass looks at; the rest wait"
            );
            absent.truncate(ABSENT_LOOKS_PER_PASS);
        }
        for (number, _) in absent {
            let gone = match self.gh.issue_opt(owner, name, number).await {
                // Closed, or gone from GitHub altogether (deleted, or not
                // readable with this token any more). An item transferred
                // to another repository answers for its new home, where it
                // is open, so that record waits for the giving up.
                Ok(item) => item.is_none_or(|i| i.state == "closed"),
                // Nothing was answered: keep the record and ask again when
                // the next look is due rather than on every pass for as
                // long as the failure lasts.
                Err(e) => {
                    debug!(
                        repo = repo.name,
                        issue = number,
                        "keeping the ignore record of an item that could not be fetched: {e:#}"
                    );
                    false
                }
            };
            let rs = self.state.repo_mut(&repo.name);
            if gone {
                rs.ignored.remove(&number);
            } else if let Some(at) = rs.ignored.get_mut(&number) {
                at.asked_at = Some(now_iso());
            }
        }
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
            let diff = self.diff(repo, &st.seen, &timeline);
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
            let Ok((srepo, snumber, sid)) = self.known_session(&sub) else {
                warn!(
                    repo = repo.name,
                    issue = issue.number,
                    subscriber = sub,
                    "subscriber is not a session ssf knows; skipping"
                );
                continue;
            };
            let sst = self.entry(&srepo, snumber).clone();
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
            match self.deliver_to(&srepo, snumber, &text, None).await {
                Ok(_) => {
                    info!(
                        repo = repo.name,
                        issue = issue.number,
                        subscriber = sid,
                        events = mine.len(),
                        "told a subscriber"
                    );
                    let e = self.entry(&srepo, snumber);
                    e.last_prompt_at = Some(now_iso());
                    e.prompts_sent += 1;
                }
                Err(e) if is_blocked(&e) => debug!(
                    repo = repo.name,
                    issue = issue.number,
                    subscriber = sid,
                    "subscriber not told: {e:#}"
                ),
                Err(e) => warn!(
                    repo = repo.name,
                    issue = issue.number,
                    subscriber = sid,
                    "could not tell a subscriber: {e:#}"
                ),
            }
        }
    }

    // ---- sessions blocked on a login ------------------------------------

    /// The sessions of a repository that can be blocked: the same ones the
    /// startup pass looks at (owning, active, with a workspace).
    async fn check_logins(&mut self, repo: &RepoConfig) {
        let candidates = self.resume_candidates(repo);
        if candidates.is_empty() {
            return;
        }
        let ps = match self.driver(repo).ps().await {
            Ok(p) => p,
            Err(e) => {
                debug!(repo = repo.name, "login check skipped: {e:#}");
                return;
            }
        };
        for number in candidates {
            let st = self.entry(repo, number).clone();
            let Some(wt) = st.worktree_id.clone() else {
                continue;
            };
            if let Some(b) = st.blocked.clone() {
                self.recover(repo, number, &st, b).await;
                continue;
            }
            // Only an idle harness is read: a working one is not at a login
            // prompt, and its screen may quote anything.
            if ps.iter().any(|w| w.worktree_id == wt && w.is_working()) {
                continue;
            }
            let handle = match self
                .driver(repo)
                .live_handle(&wt, st.terminal_handle.as_deref())
                .await
            {
                Ok(Some(h)) => h,
                _ => continue,
            };
            let Ok(screen) = self.driver(repo).screen(&handle).await else {
                continue;
            };
            let harness = self.effective(repo, number).harness;
            if let Some(detail) = crate::driver::login_dialog(&harness, &screen.join("\n")) {
                self.entry(repo, number).terminal_handle = Some(handle);
                self.set_blocked(repo, number, detail).await;
                self.report_blocked(repo, number).await;
            }
        }
        if let Err(e) = self.state.save() {
            error!("saving state: {e:#}");
        }
    }

    /// Is the harness signed in here? Asked once per harness per pass, off
    /// the runtime's workers (the status commands take a second or two).
    async fn probe_harness(&mut self, harness: &str) -> Probe {
        if let Some(p) = self.probes.get(harness) {
            return p.clone();
        }
        let probe = self.probe.clone();
        let h = harness.to_string();
        let p = match tokio::task::spawn_blocking(move || probe(&h)).await {
            Ok(p) => p,
            Err(e) => Probe {
                state: LoginState::Unknown,
                detail: format!("the login check did not finish: {e}"),
                fingerprint: None,
            },
        };
        self.probes.insert(harness.to_string(), p.clone());
        p
    }

    /// Record that the session's harness is at a login prompt. Nothing is
    /// delivered to it from now on; the item is told once (see
    /// `report_blocked`) and the login is checked every pass. On a record
    /// that already exists (the harness was started again and came back
    /// to the prompt) only the attempt is noted, so the item is not told
    /// twice and the next attempt waits longer.
    async fn set_blocked(&mut self, repo: &RepoConfig, number: u64, detail: String) -> Blocked {
        self.set_blocked_for(repo, number, Blocked::LOGIN, detail)
            .await
    }

    /// [`set_blocked`](Self::set_blocked) for a harness that could not be
    /// started at all (`Blocked::START`): the item is held the same way,
    /// and `recover` starts it again with the same backoff. A harness
    /// that would not start and is not signed in where the daemon runs is
    /// recorded as the login block it really is, so the item is told the
    /// thing worth fixing.
    async fn set_blocked_for(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        reason: &str,
        detail: String,
    ) -> Blocked {
        let session = session_id(&repo.name, number);
        let harness = self.effective(repo, number).harness;
        let probe = self.probe_harness(&harness).await;
        let reason = if reason == Blocked::START && probe.state == LoginState::SignedOut {
            Blocked::LOGIN
        } else {
            reason
        };
        let e = self.entry(repo, number);
        if let Some(cur) = e.blocked.as_mut() {
            cur.reason = reason.to_string();
            cur.detail = detail;
            cur.credential = probe.fingerprint;
            cur.retried_at = Some(now_iso());
            cur.retries += 1;
            let b = cur.clone();
            warn!(
                repo = repo.name,
                session,
                retries = b.retries,
                "started again and is blocked still; next attempt in {}s",
                retry_wait(b.retries).as_secs()
            );
            return b;
        }
        let b = Blocked {
            reason: reason.to_string(),
            harness: harness.clone(),
            detail,
            since: now_iso(),
            reported: false,
            credential: probe.fingerprint,
            retried_at: None,
            retries: 0,
            told_at: None,
            tell_failures: 0,
        };
        warn!(
            repo = repo.name,
            session,
            harness,
            detail = b.detail,
            "session is blocked: {} {}; {}",
            login::display_name(&harness),
            if reason == Blocked::START {
                "could not be started"
            } else {
                "is at its sign-in prompt"
            },
            crate::status::fix_clause(&b)
        );
        e.blocked = Some(b.clone());
        b
    }

    /// The `blocked` event on the session's item, once per block: why
    /// deliveries are held (the harness is not signed in, or it could not
    /// be started at all) and how to fix it. The record says it has been
    /// posted whether or not the post went through (`post_event` is best
    /// effort), so a failed post is not tried again every pass.
    async fn report_blocked(&mut self, repo: &RepoConfig, number: u64) {
        let st = self.entry(repo, number).clone();
        let Some(b) = st.blocked.clone().filter(|b| !b.reported) else {
            return;
        };
        self.entry(repo, number).blocked.as_mut().unwrap().reported = true;
        // The start error is the driver's or the harness's own words, and
        // the post is read by something that looks for sign-in prompts.
        let reason = if b.reason == Blocked::START {
            format!(
                "could not be started: {}",
                safe_error(&events::one_line(&b.detail))
            )
        } else {
            "not signed in".to_string()
        };
        self.post_event(
            repo,
            number,
            Event::Blocked {
                harness: login::display_name(&b.harness),
                reason,
                fix: crate::status::fix_for(&b),
            },
        )
        .await;
    }

    async fn post_comment(&self, repo: &RepoConfig, number: u64, body: &str) -> Result<String> {
        let (owner, name) = repo.split()?;
        self.gh.comment(owner, name, number, body).await
    }

    /// One of the daemon's own posts on an item (see `events`), as the bot
    /// with the `🤖 ssf` byline and an `event=` tag, so nothing takes it
    /// for a session's or a person's. Best effort: a post that cannot be
    /// made is logged, never retried and never an error to the caller;
    /// nothing at all is posted where event comments are off.
    async fn post_event(&mut self, repo: &RepoConfig, number: u64, event: Event) {
        if !self.cfg.event_comments(repo) {
            return;
        }
        let Some(origin) = Origin::new(&repo.name, number) else {
            return;
        };
        let kind = match self.peek(repo, number).and_then(|s| s.kind.as_deref()) {
            Some("pull_request") => "pull request",
            _ => "issue",
        };
        let body = events::comment(&origin, kind, &event);
        let session = session_id(&repo.name, number);
        match self.post_comment(repo, number, &body).await {
            Ok(url) => info!(
                repo = repo.name,
                session,
                event = event.name(),
                url,
                "posted the event on the item"
            ),
            Err(e) => warn!(
                repo = repo.name,
                session,
                event = event.name(),
                "could not post the event on the item: {e:#}"
            ),
        }
    }

    /// What the harness of `number`'s workspace runs with, for an
    /// `attached` post: the repository's harness, model and effort as
    /// configured, the driver, and the workspace's branch when known.
    fn launch_of(&self, repo: &RepoConfig, number: u64) -> events::Launch {
        self.launch_with(repo, number, self.overrides_of(repo, number).as_ref())
    }

    /// [`launch_of`](Self::launch_of) for overrides the item does not have
    /// (yet): what a handover's target would be started with.
    fn launch_with(
        &self,
        repo: &RepoConfig,
        number: u64,
        overrides: Option<&Overrides>,
    ) -> events::Launch {
        let eff = repo.with_overrides(overrides);
        events::Launch {
            harness: login::display_name(&eff.harness),
            model: eff.model.clone(),
            effort: eff.effort.clone(),
            command: eff.command.clone(),
            driver: self.cfg.driver_for(repo).id().to_string(),
            branch: self.peek(repo, number).and_then(|s| s.branch.clone()),
        }
    }

    /// A blocked session, once per pass: told the item if that is still
    /// owed; lifted if its harness has moved past the login prompt (a
    /// person ran `/login` in the terminal); otherwise, when the login
    /// check says the credential is back (a new credential file, or a
    /// signed-in answer once the retry wait is over), the stuck harness is
    /// quit (if it is still there) and started again with its conversation
    /// resumed, then given one message and, through the listings fetched
    /// afresh, whatever was held. `deliver` decides what the restart
    /// found: a working harness lifts the block, the prompt again notes
    /// the attempt.
    async fn recover(&mut self, repo: &RepoConfig, number: u64, st: &IssueState, b: Blocked) {
        let session = session_id(&repo.name, number);
        let harness = self.effective(repo, number).harness;
        if !b.reported {
            self.report_blocked(repo, number).await;
        }
        let Some(wt) = st.worktree_id.clone() else {
            return;
        };
        let handle = match self
            .driver(repo)
            .live_handle(&wt, st.terminal_handle.as_deref())
            .await
        {
            Ok(h) => h,
            Err(e) => {
                debug!(session, "cannot check the blocked session: {e:#}");
                return;
            }
        };
        // What the item is still owed: a handover's summary sits here
        // until a session has actually read it, so a note that is still
        // there says no session has had its first message yet, whatever
        // the block was recorded as.
        let owed = self
            .peek(repo, number)
            .is_some_and(|s| s.handover_note.is_some());
        // Telling a harness that is running costs a listing read, so it
        // is not tried every pass: once when the block is first looked
        // at, then on the same curve as a restart -- but counted apart
        // from the restarts, so a person who signs in at a terminal a
        // failed restart just left behind is answered on the next pass
        // rather than at the end of the restart's wait.
        let tell_due = match b.told_at.as_deref() {
            None => true,
            Some(t) => age(t) >= retry_wait(b.tell_failures),
        };
        if let Some(h) = &handle {
            let Ok(screen) = self.driver(repo).screen(h).await else {
                return;
            };
            if crate::driver::login_dialog(&harness, &screen.join("\n")).is_none() {
                if b.reason == Blocked::LOGIN && !owed {
                    info!(
                        session,
                        "the harness is past its sign-in prompt; deliveries resume"
                    );
                    self.unblock(repo, number, &b, Conversation::Kept).await;
                    return;
                }
                // The other cases: a harness that would not start, running
                // all the same (`start` gave up on a pane that came up but
                // never settled), and a harness that came up at its
                // sign-in prompt with the handover's first message going
                // into that screen, now signed in by a person. Either way
                // the session is there and has never been told what it is
                // for, so the screen showing no sign-in prompt is not
                // enough to lift the block; what it is owed goes first.
                if !tell_due {
                    debug!(session, "the running harness was told already; waiting");
                    return;
                }
                self.tell_a_started_harness(repo, number, &b).await;
                return;
            }
        }
        let probe = self.probe_harness(&harness).await;
        let changed = probe.fingerprint.is_some() && probe.fingerprint != b.credential;
        let last = b.retried_at.as_deref().unwrap_or(&b.since);
        let due = changed || age(last) >= retry_wait(b.retries);
        if probe.state == LoginState::SignedOut || !due {
            debug!(session, state = ?probe.state, changed, "still blocked ({})", probe.detail);
            return;
        }
        info!(
            session,
            state = ?probe.state,
            changed,
            gone = handle.is_none(),
            reason = b.reason,
            "starting the harness again ({})",
            probe.detail
        );
        if let Some(h) = &handle
            && let Err(e) = self.driver(repo).stop_agent(&wt, h).await
        {
            warn!(session, "could not quit the blocked harness: {e:#}");
            if let Some(cur) = self.entry(repo, number).blocked.as_mut() {
                cur.retried_at = Some(now_iso());
                cur.credential = probe.fingerprint;
            }
            return;
        }
        let text = if b.reason == Blocked::START {
            prompt::start_again_prompt(&prompt::LoginBack {
                harness: &login::display_name(&harness),
                since: &b.since,
                number: st.number,
                title: &st.title,
                url: &st.html_url,
            })
        } else {
            prompt::login_back_prompt(&prompt::LoginBack {
                harness: &login::display_name(&harness),
                since: &b.since,
                number: st.number,
                title: &st.title,
                url: &st.html_url,
            })
        };
        match self.deliver_to(repo, number, &text, None).await {
            Ok(_) => {
                let e = self.entry(repo, number);
                e.last_prompt_at = Some(now_iso());
                e.prompts_sent += 1;
            }
            Err(e) if is_blocked(&e) => {
                debug!(session, "{e:#}");
            }
            Err(e) => {
                warn!(session, "could not start the harness again: {e:#}");
                if let Some(cur) = self.entry(repo, number).blocked.as_mut() {
                    cur.retried_at = Some(now_iso());
                    cur.credential = probe.fingerprint;
                }
            }
        }
    }

    /// The session of a `start` block whose harness turns out to be
    /// running after all: it was never given its first message, so
    /// nothing has told it what the item is, or what the agent that
    /// handed the item over left for it. That message goes now, and the
    /// block is lifted only once it has landed.
    async fn tell_a_started_harness(&mut self, repo: &RepoConfig, number: u64, b: &Blocked) {
        let session = session_id(&repo.name, number);
        // The attempt is noted before it is made, so a message that
        // cannot be assembled or does not land waits for the backoff
        // instead of costing a listing read on every pass. The restart
        // backoff is left alone: this is not a restart.
        let attempted = Blocked {
            told_at: Some(now_iso()),
            tell_failures: b.tell_failures + 1,
            ..b.clone()
        };
        if let Some(cur) = self.entry(repo, number).blocked.as_mut() {
            cur.told_at = attempted.told_at.clone();
            cur.tell_failures = attempted.tell_failures;
        }
        let story = match self.first_message(repo, number).await {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    session,
                    "the harness is running but cannot be told what it took on: {e:#}"
                );
                return;
            }
        };
        // Everything is held for a session whose record says it is
        // blocked, this message included, so the record is cleared for
        // it -- and put back, with the note, if it does not land.
        let e = self.entry(repo, number);
        let note = e.handover_note.take();
        e.blocked = None;
        match self.deliver_to(repo, number, &story.text, None).await {
            Ok(d) => {
                let e = self.entry(repo, number);
                e.terminal_handle = Some(d.handle);
                e.last_prompt_at = Some(now_iso());
                e.prompts_sent += 1;
                // The story told it everything on the item, so the pass
                // that follows has nothing to deliver again.
                e.seen = story.seen;
                e.updated_at = Some(story.updated_at);
                info!(
                    session,
                    "the harness that would not start is running and has its first message; \
deliveries resume"
                );
                // The conversation was never restarted: this is the one
                // the handover started, told at last.
                self.unblock(repo, number, b, Conversation::Kept).await;
            }
            Err(e) => {
                warn!(session, "could not tell the running harness: {e:#}");
                let cur = self.entry(repo, number);
                cur.handover_note = note;
                // The delivery may have recorded a block of its own (the
                // pane died as the message went out, and the harness
                // started in its place came up at a sign-in screen): what
                // it saw is the fresher answer and says the thing worth
                // fixing, so it stands -- but the hold is the same hold,
                // so how long it has run, that the item was told of it,
                // and both backoffs come from the record it replaces.
                match cur.blocked.as_mut() {
                    Some(fresh) => {
                        fresh.since = attempted.since.clone();
                        fresh.reported = attempted.reported;
                        fresh.retried_at = attempted.retried_at.clone();
                        fresh.retries = attempted.retries;
                        fresh.told_at = attempted.told_at.clone();
                        fresh.tell_failures = attempted.tell_failures;
                    }
                    None => cur.blocked = Some(attempted),
                }
            }
        }
    }

    /// The session is back: forget the block, fetch every listing in full
    /// on this pass so what was held is delivered, and, when the item was
    /// told of the block, tell it the hold is over (`unblocked`, with how
    /// long it lasted and what became of the conversation: `relaunched`
    /// is the restart's `resumed` flag, `None` when the harness carried on
    /// because a person signed in at its terminal).
    async fn unblock(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        b: &Blocked,
        conversation: Conversation,
    ) {
        self.entry(repo, number).blocked = None;
        self.forget_etags(repo);
        if !b.reported {
            return;
        }
        self.post_event(
            repo,
            number,
            Event::Unblocked {
                harness: login::display_name(&b.harness),
                held: age(&b.since),
                conversation,
            },
        )
        .await;
    }

    /// One full listing set is owed, so that every item involving the bot
    /// is looked at again whatever the ETags said. Where it lands depends
    /// on when the session came back: an unblock before the pass reads its
    /// listings gets it on that pass, and the flag it armed is spent,
    /// unused, when that pass begins; one after gets it on the next
    /// pass, because the pass under way stores the ETags it read at its
    /// end and so puts back what was cleared here. Either way it is one:
    /// the pass that spends the flag clears the ETags without arming it
    /// again, so the pass after that is back to conditional requests.
    fn forget_etags(&mut self, repo: &RepoConfig) {
        self.clear_etags(repo);
        self.refetch.insert(repo.name.clone());
    }

    /// Drop the repository's cached listing ETags, so the listings this
    /// pass reads are full ones. Nothing beyond this pass is owed: the
    /// ETags it reads are stored at its end and used again next time.
    fn clear_etags(&mut self, repo: &RepoConfig) {
        let rs = self.state.repo_mut(&repo.name);
        rs.issues_etag = None;
        rs.mentioned_etag = None;
        rs.pulls_etag = None;
        rs.created_etag = None;
    }

    /// Count a failure against an item; at `MAX_DELIVERY_FAILURES` in a
    /// row the binding is given up (the item is onboarded afresh on its
    /// next look) and, when there was a binding to give up (the item was
    /// seeded), the item is told so (`gave-up`). An item that never got a
    /// session (nothing to clone, a workspace the driver cannot make)
    /// fails every look and drops nothing: it is not told each time.
    async fn note_failure(&mut self, repo: &RepoConfig, number: u64, err: &anyhow::Error) {
        let key = (repo.name.clone(), number);
        let count = self.failures.entry(key.clone()).or_insert(0);
        *count += 1;
        let count = *count;
        warn!(
            repo = repo.name,
            issue = number,
            attempt = count,
            "handling issue failed: {err:#}"
        );
        if count >= MAX_DELIVERY_FAILURES {
            error!(
                repo = repo.name,
                issue = number,
                "giving up on the current workspace binding; the issue will be re-onboarded"
            );
            self.failures.insert(key, 0);
            let mut had_binding = false;
            if let Some(st) = self.state.repo_mut(&repo.name).issues.get_mut(&number) {
                had_binding = st.seeded;
                st.seeded = false;
                st.terminal_handle = None;
            }
            if had_binding {
                self.post_event(
                    repo,
                    number,
                    Event::GaveUp {
                        failures: count,
                        last_error: safe_error(&events::one_line(&format!("{err:#}"))),
                    },
                )
                .await;
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
        let harness = self.effective(repo, st.number).harness;
        let harness_prompt = st
            .worktree_path
            .as_deref()
            .and_then(|p| ProjectPrompt::load_harness(repo, Path::new(p), &harness));
        PromptContext {
            repo,
            daemon: &self.cfg.daemon,
            bot_login: &self.login,
            driver: self.cfg.driver_for(repo),
            pr: st.pr.as_ref(),
            triggers: &st.triggers,
            owner: st.shares_workspace_of,
            delegated_by: st.delegated_by.as_deref(),
            handed_over_from: None,
            projects: &st.projects,
            project_prompt,
            harness_prompt,
            vm_guest: crate::vm::in_guest(),
            pushes_as: self.cfg.git_identity(Some(repo)).credential.prompt_pusher(),
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
        e.driver = o.driver;
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

    /// What is new on an item's timeline since `seen`, rendered for the
    /// prompts. Events by a login that may not drive the repository are
    /// left out (and still counted as seen): they are neither delivered to
    /// the owner nor fanned out. Commits carry no login (`author.name` is
    /// a git name) and pass; pushing needs write access to the branch.
    fn diff(&self, repo: &RepoConfig, seen: &BTreeMap<String, String>, timeline: &[Value]) -> Diff {
        let allowed = self.allow_list(repo);
        let mut rendered = Vec::new();
        let mut observed = BTreeMap::new();
        let mut filtered: Value;
        for ev in timeline {
            let mut ev = ev;
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
            // posts), so it is delivered like any human's. The daemon's
            // own event posts (`event=` in the tag) are for people: no
            // agent, owner or subscriber, ever sees one.
            let actor = actor_of(ev);
            let own = actor.eq_ignore_ascii_case(&self.login);
            let echo = matches!(kind, "cross-referenced" | "referenced" | "committed");
            if own && echo && !self.cfg.daemon.include_own_events {
                debug!(key, "skipping bot's own event");
                continue;
            }
            if own
                && kind == "commented"
                && origin::is_event_post(crate::github::value_str(ev, &["body"]).unwrap_or(""))
            {
                debug!(key, "skipping the daemon's own event post");
                continue;
            }
            match kind {
                "committed" => {}
                // A batch of review comments: each has its own author.
                "line-commented" | "commit-commented" => {
                    let Some(comments) = ev.get("comments").and_then(Value::as_array) else {
                        continue;
                    };
                    let kept: Vec<Value> = comments
                        .iter()
                        .filter(|c| {
                            let who = crate::github::value_str(c, &["user", "login"])
                                .unwrap_or("unknown");
                            let ok = allowed.allows(who);
                            if !ok {
                                self.dropped(repo, &key, who);
                            }
                            ok
                        })
                        .cloned()
                        .collect();
                    if kept.is_empty() {
                        continue;
                    }
                    if kept.len() != comments.len() {
                        filtered = ev.clone();
                        filtered["comments"] = Value::Array(kept);
                        ev = &filtered;
                    }
                }
                _ => {
                    if !allowed.allows(&actor) {
                        self.dropped(repo, &key, &actor);
                        continue;
                    }
                }
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

    fn remember_worktree(&mut self, repo: &RepoConfig, number: u64, wt: &Worktree) {
        let driver = self.cfg.driver_for(repo);
        let e = self.entry(repo, number);
        e.worktree_id = Some(wt.id.clone());
        e.worktree_path = Some(wt.path.clone());
        e.driver = Some(driver.id().into());
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
        // Who asked comes first: an item nobody allowed asked for gets no
        // project, no workspace and no session.
        let timeline = self.gh.timeline(owner, name, issue.number).await?;
        if let Err(why) = self.gate(repo, issue, &timeline, &triggers) {
            self.refuse(repo, issue, &triggers, &why);
            return Ok(());
        }
        // Whatever it was ignored as before, it is being looked at afresh.
        self.state
            .repo_mut(&repo.name)
            .ignored
            .remove(&issue.number);
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
        let diff = self.diff(repo, &BTreeMap::new(), &timeline);

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
        let driver = self.cfg.driver_for(repo);
        {
            let e = self.entry(repo, issue.number);
            e.title = issue.title.clone();
            e.html_url = issue.html_url.clone();
            e.repo_id = Some(setup.repo_id.clone());
            e.driver = Some(driver.id().into());
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
            .map(|p| self.diff(repo, &p.seen, &timeline).rendered);
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
                self.remember_worktree(repo, issue.number, &wt);
                let _ = self.driver(repo).set_comment(&wt.id, &comment).await;
                let text = self.initial_text(repo, issue, &mine);
                // The item is attached again, to what it had: `deliver_to`
                // leaves a relaunch for this onboarding to be told here.
                self.onboarding = Some((repo.name.clone(), issue.number));
                let delivered = self.deliver_to(repo, issue.number, &text, None).await;
                self.onboarding = None;
                let d = delivered?;
                let launch = self.launch_of(repo, issue.number);
                let handed_off_from = self.entry(repo, issue.number).delegated_by.clone();
                self.post_event(
                    repo,
                    issue.number,
                    Event::Attached(Attach::Kept {
                        launch,
                        handed_off_from,
                        conversation: if d.relaunched {
                            Conversation::of(d.resumed)
                        } else {
                            Conversation::Kept
                        },
                    }),
                )
                .await;
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
                let eff = self.effective(repo, issue.number);
                let title = format!("{} · #{}", eff.harness, issue.number);
                let cmd = self.launch_command(
                    repo,
                    issue.number,
                    &issue.html_url,
                    &eff.harness_command(),
                );
                let text = self.initial_text(repo, issue, &mine);
                let handle = self
                    .driver(repo)
                    .start(&created.id, &cmd, &title, &eff.harness, &text)
                    .await?;
                info!(
                    repo = repo.name,
                    issue = issue.number,
                    handle,
                    "launched {} and sent the {}",
                    eff.harness,
                    if is_pr { "pull request" } else { "issue" }
                );
                let launch = self.launch_of(repo, issue.number);
                let handed_off_from = self.entry(repo, issue.number).delegated_by.clone();
                self.post_event(
                    repo,
                    issue.number,
                    Event::Attached(Attach::Started {
                        launch,
                        handed_off_from,
                    }),
                )
                .await;
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
        // The bound item hears which session took it (the owner's own item
        // heard about that session when it started).
        self.post_event(
            repo,
            issue.number,
            Event::Attached(Attach::Bound {
                session: session_id(&repo.name, owner),
                shares: owner,
            }),
        )
        .await;
        if let Some(since) = since_prior {
            self.fan_out(repo, issue, &since, Fyi::Tracked, false, &[])
                .await;
        }
        Ok(())
    }

    /// The item's word in a prompt or a post: `issue` or `pull request`.
    fn item_kind(&self, repo: &RepoConfig, number: u64) -> &'static str {
        match self.peek(repo, number).and_then(|s| s.kind.as_deref()) {
            Some("pull_request") => "pull request",
            _ => "issue",
        }
    }

    /// What a session that has been told nothing yet is owed: whatever a
    /// handover left for it (the outgoing agent's summary, or the fact
    /// that it left none) ahead of the item's whole story. The note stays
    /// on the record until a session has actually been given it -- a
    /// start that fails is tried again later, and the words the outgoing
    /// agent left go with that attempt rather than being lost with the
    /// pane that never came up.
    async fn first_message(&mut self, repo: &RepoConfig, number: u64) -> Result<Story> {
        let note = self
            .peek(repo, number)
            .and_then(|s| s.handover_note.clone());
        let kind = self.item_kind(repo, number);
        let mut story = self
            .story(repo, number, note.as_ref().map(|n| n.from.as_str()), None)
            .await?;
        if let Some(n) = note {
            story.text = prompt::handover_prompt(&n.from, kind, n.summary.as_deref(), &story.text);
        }
        Ok(story)
    }

    /// The whole story of an item as its initial prompt would tell it, for
    /// a harness that starts from scratch and needs context for whatever is
    /// about to be delivered, with what telling it counts as seen.
    async fn story(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        handed_over_from: Option<&str>,
        target_harness: Option<&str>,
    ) -> Result<Story> {
        let (owner, name) = repo.split()?;
        let issue = self.gh.issue(owner, name, number).await?;
        let timeline = self.gh.timeline(owner, name, number).await?;
        let diff = self.diff(repo, &BTreeMap::new(), &timeline);
        let me = self.acting_on(repo, number);
        let all = self.for_recipient(&diff.rendered, &me);
        let st = self.entry(repo, number).clone();
        let mut ctx = PromptContext {
            handed_over_from,
            ..self.ctx(repo, &st)
        };
        if let Some(harness) = target_harness {
            ctx.harness_prompt = st
                .worktree_path
                .as_deref()
                .and_then(|p| ProjectPrompt::load_harness(repo, Path::new(p), harness));
        }
        Ok(Story {
            text: prompt::initial_prompt(&issue, &all, &ctx),
            seen: diff.seen,
            updated_at: issue.updated_at,
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
        let diff = self.diff(repo, &st.seen, &timeline);
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
        let mut all = self.diff(repo, &BTreeMap::new(), &timeline).rendered;
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
        let timeline = self.gh.timeline(owner, name, issue.number).await?;
        if let Err(why) = self.gate(repo, issue, &timeline, &st.triggers) {
            self.refuse(repo, issue, &st.triggers, &why);
            return Ok(());
        }
        info!(
            repo = repo.name,
            issue = issue.number,
            "issue assigned again; reactivating"
        );
        self.record_origins(repo, issue, &timeline);
        let diff = self.diff(repo, &st.seen, &timeline);
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
        let all = self.diff(repo, &BTreeMap::new(), &timeline).rendered;
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

    /// Whether a retirement for this item was held recently enough that
    /// the item does not need reading again yet.
    fn held_recently(&self, repo: &RepoConfig, number: u64) -> bool {
        let Some(st) = self
            .state
            .repos
            .get(&repo.name)
            .and_then(|r| r.issues.get(&number))
        else {
            return false;
        };
        let Some(at) = st.retirement_held_at.as_deref() else {
            return false;
        };
        let window = chrono::Duration::from_std(RETIREMENT_RECHECK).unwrap();
        chrono::DateTime::parse_from_rfc3339(at)
            .map(|t| chrono::Utc::now().signed_duration_since(t.with_timezone(&chrono::Utc)))
            // A stamp ahead of the clock (a backward step from NTP, or a
            // guest resuming without a reliable one) would otherwise read
            // as fresh until the clock caught up, holding for hours.
            .is_ok_and(|age| age >= chrono::Duration::zero() && age < window)
    }

    /// Whether an open item still carries any of the triggers it was
    /// onboarded on, read from the item rather than from a listing, and on
    /// what. The timeline is fetched only if a mention has to be
    /// re-checked, and is handed back so the caller does not fetch it
    /// twice.
    async fn still_ours(
        &self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        issue: &Issue,
        triggers: &[String],
        timeline: &mut Option<Vec<Value>>,
    ) -> StillOurs {
        let has = |t: &str| triggers.iter().any(|x| x == t);
        // Not gated on the triggers: they are rewritten from the listings
        // every pass, so the assigned listing hiccupping is exactly when
        // this is worth asking, and the item is already in hand.
        if issue.is_assigned_to(&self.login) {
            return StillOurs::Certain;
        }
        if has("created") && issue.author().eq_ignore_ascii_case(&self.login) {
            return StillOurs::Certain;
        }
        if has("mentioned") {
            // The timeline walk is the expensive part, so it is what the
            // hold paces; everything above is read from the item already
            // in hand, and a closed item never reaches here at all.
            if self.held_recently(repo, issue.number) {
                return StillOurs::Paced;
            }
            match self.gh.timeline(owner, name, issue.number).await {
                Ok(tl) => {
                    let found = mentions_bot(issue, &tl, &self.login);
                    *timeline = Some(tl);
                    if found {
                        return StillOurs::Paced;
                    }
                }
                // Without the timeline there is no evidence either way. A
                // retirement is the destructive reading, so the item keeps
                // the benefit of the doubt until a pass can read it.
                Err(e) => {
                    warn!(
                        repo = repo.name,
                        issue = issue.number,
                        "could not re-check the mention before retiring: {e:#}"
                    );
                    return StillOurs::Paced;
                }
            }
        }
        if has("review_requested") && issue.is_pull_request() {
            match self.gh.pull(owner, name, issue.number).await {
                Ok(pr) => {
                    if pr.requests_review_from(&self.login) {
                        return StillOurs::Certain;
                    }
                }
                Err(e) => {
                    warn!(
                        repo = repo.name,
                        issue = issue.number,
                        "could not re-check the review request before retiring: {e:#}"
                    );
                    return StillOurs::Paced;
                }
            }
        }
        StillOurs::No
    }

    /// Forget a held retirement: the item is on a listing again, or has
    /// been judged on something that needs no pacing.
    fn clear_hold(&mut self, repo: &RepoConfig, number: u64) {
        let e = self.entry(repo, number);
        e.retirement_held_at = None;
        e.retirement_announced = false;
    }

    /// Record a hold that rests on the paced re-check. The walk that
    /// answers it is the expensive part, so this stamps the item and the
    /// next walk waits out `RETIREMENT_RECHECK`. The stamp is only written
    /// by a pass that actually read the item, so a hold expires rather
    /// than rolling forward.
    fn note_paced_hold(&mut self, repo: &RepoConfig, number: u64) -> bool {
        if self.held_recently(repo, number) {
            debug!(
                repo = repo.name,
                issue = number,
                "retirement still held: the item carried a trigger recently"
            );
            return true;
        }
        let e = self.entry(repo, number);
        e.retirement_held_at = Some(now_iso());
        if e.retirement_announced {
            debug!(
                repo = repo.name,
                issue = number,
                "retirement held again: the listings still disagree with the item"
            );
        } else {
            e.retirement_announced = true;
            // A listing disagreeing with an item is worth seeing once.
            info!(
                repo = repo.name,
                issue = number,
                "retirement held: the listings dropped the item but it still carries a trigger"
            );
        }
        true
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
        // A listing can lag, and can come back without an item that is
        // still the bot's. The item itself is the only reliable evidence,
        // so every trigger it was onboarded on is re-checked against it
        // before a session is told to stop. Testing only assignment and
        // authorship retired a mention-triggered session whenever its
        // listing hiccupped, even though the mention was still sitting in
        // the issue, and the session was reattached on the next pass.
        let mut timeline = None;
        let verdict = if closed {
            StillOurs::No
        } else {
            self.still_ours(repo, owner, name, &issue, &st.triggers, &mut timeline)
                .await
        };
        let hold = match verdict {
            StillOurs::No => false,
            // Judged on the item itself, so there is no walk to pace and
            // no bookkeeping to keep.
            StillOurs::Certain => {
                self.clear_hold(repo, number);
                debug!(
                    repo = repo.name,
                    issue = number,
                    "retirement held: the item itself still names the bot"
                );
                true
            }
            StillOurs::Paced => self.note_paced_hold(repo, number),
        };
        if hold {
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
        let timeline = match timeline {
            Some(t) => t,
            None => self.gh.timeline(owner, name, number).await?,
        };
        self.record_origins(repo, &issue, &timeline);
        let diff = self.diff(repo, &st.seen, &timeline);
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
        e.retirement_held_at = None;
        e.retirement_announced = false;
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
    /// state locations are passed along so the wrapper reads the same files,
    /// and the VM guest flag so `ssf guide` in the session knows where it is.
    fn launch_command(&self, repo: &RepoConfig, number: u64, url: &str, inner: &str) -> String {
        let me = crate::client_executable()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "ssf".to_string());
        let mut prefix = String::new();
        for var in [
            "SSF_CONFIG_DIR",
            "SSF_STATE_DIR",
            "SSF_GITHUB_TOKEN",
            crate::vm::GUEST_ENV,
        ] {
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
            Some(id) => self.driver(repo).worktree_exists(id).await?,
            None => false,
        };
        // A session at a login prompt takes nothing; the prompt is held.
        // With the stuck harness gone the delivery goes ahead: starting it
        // again is the way to find out whether the login is back, and what
        // it shows decides the record below.
        let held = match st.blocked.clone() {
            Some(b) => {
                let stuck = alive
                    && match st.worktree_id.as_deref() {
                        Some(id) => self.driver(repo).has_live_agent(id).await.unwrap_or(false),
                        None => false,
                    };
                if stuck {
                    return Err(SessionBlocked {
                        session: session_id(&repo.name, target),
                        blocked: b,
                    }
                    .into());
                }
                debug!(
                    repo = repo.name,
                    session = session_id(&repo.name, target),
                    "blocked harness is gone; starting it again decides"
                );
                Some(b)
            }
            None => None,
        };
        let re_created = if alive {
            None
        } else {
            self.rehydrate(repo, target).await?
        };
        let st = self.entry(repo, target).clone();
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
        // A handover whose new session never came up left its summary on
        // the item: the harness started here is the one that takes it on.
        let mut note_given = false;
        if !live && (target != number || relaunch_text.is_none()) {
            let owed = self
                .peek(repo, target)
                .is_some_and(|s| s.handover_note.is_some());
            match self.first_message(repo, target).await {
                Ok(s) => {
                    story = Some(format!("{}\n\n{text}", s.text));
                    note_given = owed;
                }
                Err(e) => warn!(
                    repo = repo.name,
                    session = session_id(&repo.name, target),
                    "could not assemble the session's story for a fresh harness: {e:#}"
                ),
            }
        }
        let relaunch_text = story.as_deref().or(relaunch_text);
        // The note is taken off the record when the message carrying it
        // goes out, and put back if that message turns out to have gone
        // into a sign-in screen (below).
        let mut spent_note = None;
        let eff = self.effective(repo, target);
        let title = format!("{} · #{target}", eff.harness);
        let resume = st
            .agent_session_id
            .as_deref()
            .and_then(|id| sessions::resume_command(&eff.harness, &eff.harness_command(), id))
            .map(|c| self.launch_command(repo, st.number, &st.html_url, &c));
        let relaunch = self.launch_command(repo, st.number, &st.html_url, &eff.harness_command());
        let d = self
            .driver(repo)
            .deliver(
                &worktree_id,
                st.terminal_handle.as_deref(),
                Relaunch {
                    command: &relaunch,
                    resume_command: resume.as_deref(),
                    harness: &eff.harness,
                    title: &title,
                    text: relaunch_text,
                },
                text,
            )
            .await?;
        if d.relaunched {
            let e = self.entry(repo, target);
            e.launched_at = Some(now_iso());
            if !d.resumed {
                e.agent_session_id = None;
                // A resumed conversation is not shown the relaunch text,
                // so the note is spent only on a fresh one.
                if note_given {
                    spent_note = e.handover_note.take();
                }
            }
            info!(
                repo = repo.name,
                session = session_id(&repo.name, target),
                resumed = d.resumed,
                "harness relaunched"
            );
            // A re-created workspace is news whatever the harness shows.
            if let Some(reason) = re_created {
                let launch = self.launch_of(repo, target);
                self.post_event(
                    repo,
                    target,
                    Event::Attached(Attach::ReCreated {
                        launch,
                        reason,
                        conversation: Conversation::of(d.resumed),
                    }),
                )
                .await;
            }
        }
        self.entry(repo, target).terminal_handle = Some(d.handle.clone());
        // A harness started again on a machine that is not signed in shows
        // its login prompt instead of taking the prompt: the session is
        // blocked from here, and the prompt is held for later.
        if d.relaunched
            && let Ok(screen) = self.driver(repo).screen(&d.handle).await
            && let Some(detail) = crate::driver::login_dialog(&eff.harness, &screen.join("\n"))
        {
            // The message that carried the note went into a sign-in
            // screen, so no session has read it: it waits on the item for
            // the start that gets through.
            if let Some(note) = spent_note {
                self.entry(repo, target).handover_note = Some(note);
            }
            let b = self.set_blocked(repo, target, detail).await;
            return Err(SessionBlocked {
                session: session_id(&repo.name, target),
                blocked: b,
            }
            .into());
        }
        // A harness started again in its existing workspace is `resumed`;
        // one started to lift a login block is told of in `unblocked`, and
        // one started by the target's own onboarding onto a kept workspace
        // in that onboarding's `attached`.
        let onboarding = self
            .onboarding
            .as_ref()
            .is_some_and(|(r, n)| r.eq_ignore_ascii_case(&repo.name) && *n == target);
        if d.relaunched && re_created.is_none() && held.is_none() && !onboarding {
            self.post_event(
                repo,
                target,
                Event::Resumed {
                    harness: login::display_name(&eff.harness),
                    conversation: Conversation::of(d.resumed),
                    after: if self.startup_pass {
                        "restart"
                    } else {
                        "lost terminal"
                    },
                },
            )
            .await;
        }
        if let Some(b) = held {
            let conversation = d
                .relaunched
                .then_some(d.resumed)
                .map_or(Conversation::Kept, Conversation::of);
            self.unblock(repo, target, &b, conversation).await;
        }
        if target != number {
            self.mirror_owner(repo, number, target);
        }
        Ok(d)
    }

    /// A binding written by a driver other than the one the repository runs
    /// in now (`driver` changed since the workspace was made) is dropped
    /// before the workspace is looked for, so the item goes through the
    /// current driver's project setup as a new one would, rather than the
    /// old driver's repo id being handed to the new driver as its own (an
    /// Orca uuid taken for a checkout path, or the other way round). The
    /// workspace name and branch stay, so the branch is picked up as the
    /// base as for any re-created workspace. A record from before the
    /// driver was written down is judged by the shape of its repo id.
    /// Says whether a binding was dropped.
    fn drop_foreign_binding(&mut self, repo: &RepoConfig, number: u64) -> bool {
        let current = self.cfg.driver_for(repo);
        let st = self.entry(repo, number).clone();
        let Some(repo_id) = st.repo_id.as_deref() else {
            return false;
        };
        let made_by = match st.driver.as_deref() {
            Some(d) => d.to_string(),
            None if self.driver(repo).owns_repo_id(repo_id) => return false,
            None => DriverKind::of_repo_id(repo_id)
                .map(|k| k.id().to_string())
                .unwrap_or_else(|| "another driver".into()),
        };
        if made_by == current.id() {
            return false;
        }
        info!(
            repo = repo.name,
            session = session_id(&repo.name, number),
            "workspace was made by {made_by}; re-creating it on {}",
            current.id()
        );
        let e = self.entry(repo, number);
        e.repo_id = None;
        e.driver = None;
        e.worktree_id = None;
        e.worktree_path = None;
        e.terminal_handle = None;
        true
    }

    /// The current driver's id for the repository, from the record when
    /// it has one and from the driver's project setup otherwise; written
    /// back so the next look does not set the project up again.
    async fn repo_id_for(&mut self, repo: &RepoConfig, number: u64) -> Result<String> {
        if let Some(r) = self.entry(repo, number).repo_id.clone() {
            return Ok(r);
        }
        let (owner, name) = repo.split()?;
        let driver = self.cfg.driver_for(repo);
        let repo_id = self
            .driver(repo)
            .ensure_project(
                owner,
                name,
                &repo.clone_url(),
                repo.path.as_deref(),
                &self.cfg.projects_dir(driver),
            )
            .await?
            .repo_id;
        let e = self.entry(repo, number);
        e.repo_id = Some(repo_id.clone());
        e.driver = Some(driver.id().into());
        Ok(repo_id)
    }

    /// Re-create the workspace for an issue whose worktree is gone,
    /// starting from its old branch when that still exists. Says why it
    /// was re-created (`workspace gone`, or `driver switch` when a binding
    /// made by another driver was dropped first), or `None` when the
    /// driver already had a workspace linked to the issue and nothing was
    /// made.
    async fn rehydrate(&mut self, repo: &RepoConfig, number: u64) -> Result<Option<&'static str>> {
        let switched = self.drop_foreign_binding(repo, number);
        let repo_id = self.repo_id_for(repo, number).await?;
        let st = self.entry(repo, number).clone();
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
            self.remember_worktree(repo, number, &existing);
            let e = self.entry(repo, number);
            e.terminal_handle = None;
            return Ok(None);
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
        self.remember_worktree(repo, number, &created);
        let e = self.entry(repo, number);
        e.terminal_handle = None;
        e.worktree_name = Some(name);
        Ok(Some(if switched {
            "driver switch"
        } else {
            "workspace gone"
        }))
    }

    /// Record harness session ids for workspaces that do not have one yet.
    fn capture_sessions(&mut self, repo: &RepoConfig) {
        // Per item, since a handed-over item runs a harness of its own:
        // the records worth looking at, each with the harness it runs.
        let candidates: Vec<u64> = match self.state.repos.get(&repo.name) {
            Some(rs) => rs
                .issues
                .values()
                .filter(|s| s.agent_session_id.is_none() && s.worktree_id.is_some())
                .map(|s| s.number)
                .collect(),
            None => Vec::new(),
        };
        let harnesses: Vec<(u64, String)> = candidates
            .into_iter()
            .map(|n| (n, self.effective(repo, n).harness))
            .filter(|(_, h)| sessions::supports_resume(h))
            .collect();
        let rs = self.state.repo_mut(&repo.name);
        for (number, harness) in harnesses {
            let Some(st) = rs.issues.get_mut(&number) else {
                continue;
            };
            let (Some(path), Some(launched)) =
                (st.worktree_path.as_deref(), st.launched_at.as_deref())
            else {
                continue;
            };
            let retired = st.retired_session_ids.clone();
            let since = capture_since(launched, st.handed_over_at.as_deref());
            if let Some(id) = sessions::capture(&harness, path, since, &retired) {
                info!(
                    repo = repo.name,
                    issue = st.number,
                    session = id,
                    "captured {harness} session"
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

    /// Remove the workspaces `ssf release` approved (checked once more
    /// here, since the agent may have carried on). Workspaces are never
    /// removed on the old close-time flag; a stale one is cleared.
    async fn run_cleanups(&mut self, repo: &RepoConfig) {
        let rs = self.state.repo_mut(&repo.name);
        let pending: Vec<IssueState> = rs
            .issues
            .values()
            .filter(|s| s.release_pending || s.cleanup_pending)
            .cloned()
            .collect();
        for st in pending {
            if st.cleanup_pending {
                self.entry(repo, st.number).cleanup_pending = false;
            }
            if st.release_pending {
                self.finish_release(repo, st).await;
            }
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
        if !st.release_forced && !self.active_dependents(repo, st.number).is_empty() {
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
                self.post_event(
                    repo,
                    st.number,
                    Event::Released {
                        by: "ssf release",
                        forced: st.release_forced,
                        branch: st.branch.clone(),
                    },
                )
                .await;
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
        match self.deliver_to(repo, st.number, &text, None).await {
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
        // The item comes back on the repository's own harness.
        e.overrides = None;
        e.handover = None;
        e.handover_note = None;
        e.handed_over_at = None;
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

    // ---- handovers ------------------------------------------------------

    /// `ssf handover`: record that the item's session is to be replaced by
    /// one on another harness, model or effort in the same workspace. The
    /// refusals are synchronous, so the agent that asked hears the reason
    /// straight away; the work itself happens on the daemon's next pass
    /// (`run_handovers`), because ending the caller's own terminal while
    /// it waits for this answer would lose the answer.
    async fn handover(
        &mut self,
        session: &str,
        harness: &str,
        model: Option<&str>,
        effort: Option<&str>,
        summary: Option<&str>,
        by: Option<&str>,
    ) -> Result<Value> {
        let (repo, number, id) = self.known_session(session)?;
        let st = self.entry(&repo, number).clone();
        if !st.active || st.worktree_id.is_none() {
            anyhow::bail!(
                "{id}: the item has no running session; nothing to hand over (assign the bot to it instead)"
            );
        }
        let harness = harness.trim();
        if !crate::agents::is_known(harness) {
            anyhow::bail!("{harness} is not a harness ssf knows (see `ssf agents`)");
        }
        crate::models::validate(harness, model, effort)?;
        let name = login::display_name(harness);
        if !(self.installed)(harness) {
            anyhow::bail!("{name} is not installed where the daemon runs (see `ssf agents`)");
        }
        // Asked afresh rather than off the pass's memo: an operator who
        // signs the harness in and runs the command again must get the
        // new answer, not the one from up to a poll interval ago.
        self.probes.remove(harness);
        let probe = self.probe_harness(harness).await;
        if probe.state == LoginState::SignedOut {
            anyhow::bail!(
                "{name} is not signed in here; {}",
                login::how_to_sign_in(harness)
            );
        }
        if let Some(h) = st.handover.as_ref() {
            anyhow::bail!("a handover to {} is already pending", h.harness);
        }
        if st.release_pending {
            anyhow::bail!("a release is pending on this item");
        }
        // Trimmed as `models::validate` reads them, or a stray space
        // would be shell-quoted into the launch command and the harness
        // would refuse the model it was given.
        let overrides = Overrides {
            harness: harness.to_string(),
            model: model.map(|m| m.trim().to_string()),
            effort: effort.map(|e| e.trim().to_string()),
        };
        let from = self.effective(&repo, number);
        let to = repo.with_overrides(Some(&overrides));
        if to.harness == from.harness && to.model == from.model && to.effort == from.effort {
            anyhow::bail!("the item is already on {harness} with that model and effort");
        }
        if summary.is_some_and(|s| s.trim().is_empty()) {
            anyhow::bail!("the summary is empty: write a summary or hand over with no summary");
        }
        let chars = summary.map(|s| s.chars().count());
        if let Some(n) = chars.filter(|n| *n > crate::ipc::MAX_SUMMARY_CHARS) {
            anyhow::bail!(
                "the summary is {n} characters; the most a handover carries is {}",
                crate::ipc::MAX_SUMMARY_CHARS
            );
        }
        // The summary is pasted into the new session's terminal, where the
        // login check reads the screen: one that quotes a sign-in prompt
        // would block the session it starts. The CLI refuses it too, where
        // the author can fix it; this is the daemon's own guard.
        if let Some(line) = summary.and_then(crate::driver::login_prompt_line) {
            anyhow::bail!(
                "{}",
                crate::summary_quotes_a_sign_in_screen_text(&crate::driver::redact_login_phrases(
                    &line
                ))
            );
        }
        let pending = PendingHandover {
            harness: overrides.harness.clone(),
            model: overrides.model.clone(),
            effort: overrides.effort.clone(),
            summary: summary.map(str::to_string),
            by: by.map(str::to_string),
            requested_at: now_iso(),
        };
        self.entry(&repo, number).handover = Some(pending);
        info!(
            session = id,
            harness,
            model,
            effort,
            summary_chars = chars,
            by,
            "handover recorded"
        );
        // The command is on both sides because it is what decides an
        // unset model or effort, and the `handed-over` post says so; the
        // command itself is on the item in every `attached` post anyway.
        let launch = |c: &RepoConfig| {
            serde_json::json!({
                "harness": c.harness,
                "model": c.model,
                "effort": c.effort,
                "command": c.command,
            })
        };
        Ok(serde_json::json!({
            "session": id,
            "title": st.title,
            "from": launch(&from),
            "to": launch(&to),
            "summary_chars": chars,
            "poll_interval_secs": self.cfg.daemon.poll_interval_secs,
        }))
    }

    /// Carry out the handovers `ssf handover` accepted, before anything
    /// else this repository does on this pass: the old session is not worth
    /// resuming or delivering to.
    async fn run_handovers(&mut self, repo: &RepoConfig) {
        let pending: Vec<(u64, PendingHandover)> = match self.state.repos.get(&repo.name) {
            Some(rs) => rs
                .issues
                .values()
                .filter_map(|s| s.handover.clone().map(|h| (s.number, h)))
                .collect(),
            None => Vec::new(),
        };
        if pending.is_empty() {
            return;
        }
        // The new session is given the item's story, which the allow-list
        // filters: the collaborators have to be known first, or every
        // human post would be left out of it. A handover the daemon
        // cannot read the repository for waits for the next pass.
        let refreshed = match repo.split() {
            Ok((owner, name)) => self.refresh_collaborators(repo, owner, name).await,
            Err(e) => Err(e),
        };
        if let Err(e) = refreshed {
            warn!(
                repo = repo.name,
                "handovers wait for the next pass on this repository: {e:#}"
            );
            return;
        }
        for (number, h) in pending {
            self.finish_handover(repo, number, h).await;
            if let Err(e) = self.state.save() {
                error!("saving state: {e:#}");
            }
        }
    }

    /// Second half of `ssf handover`: end the session that is there, keep
    /// its workspace and branch, write the item's launch overrides, and
    /// start the new session in the same workspace with the outgoing
    /// agent's summary ahead of the item's story.
    async fn finish_handover(&mut self, repo: &RepoConfig, number: u64, h: PendingHandover) {
        let session = session_id(&repo.name, number);
        let st = self.entry(repo, number).clone();
        if !st.active {
            self.refuse_handover(repo, number, &h, "the item is no longer active".into())
                .await;
            return;
        }
        // The workspace normally stays; one that went missing between the
        // request and now is re-created rather than the handover lost.
        let alive = match st.worktree_id.as_deref() {
            Some(id) => self.driver(repo).worktree_exists(id).await.unwrap_or(false),
            None => false,
        };
        if !alive && let Err(e) = self.rehydrate(repo, number).await {
            self.refuse_handover(
                repo,
                number,
                &h,
                format!("the workspace is gone and could not be re-created: {e:#}"),
            )
            .await;
            return;
        }
        let st = self.entry(repo, number).clone();
        let Some(wt) = st.worktree_id.clone() else {
            self.refuse_handover(repo, number, &h, "the item has no workspace".into())
                .await;
            return;
        };
        // The new session's first message is built before anything is
        // stopped: a story that cannot be assembled is a refusal, not a
        // session ended with nothing to put in its place.
        let from_launch = self.launch_of(repo, number);
        let from_harness = self.effective(repo, number).harness;
        let from_name = login::display_name(&from_harness);
        // Who the new session really takes over from. Normally the
        // harness the item is on; but a handover whose harness never came
        // up left a note of its own, and the session that wrote it is
        // still the last one that worked the item, so its name is the one
        // carried forward. The `handed-over` post keeps saying what the
        // item was configured on.
        let pending_note = self
            .peek(repo, number)
            .and_then(|s| s.handover_note.clone());
        let note_from = pending_note
            .as_ref()
            .map(|n| n.from.clone())
            .unwrap_or_else(|| from_name.clone());
        // A summary nobody has read yet is not thrown away by a handover
        // that carries none of its own: the session that wrote it is long
        // gone, and the one starting now is the first that can act on it.
        // A handover that does bring a summary replaces it, since that is
        // the newer account of where the item stands.
        let summary = h
            .summary
            .clone()
            .or_else(|| pending_note.and_then(|n| n.summary));
        let story = match self
            .story(repo, number, Some(&note_from), Some(&h.harness))
            .await
        {
            Ok(s) => s,
            Err(e) => {
                self.refuse_handover(
                    repo,
                    number,
                    &h,
                    format!("the item could not be read for the new session: {e:#}"),
                )
                .await;
                return;
            }
        };
        let kind = self.item_kind(repo, number);
        let text = prompt::handover_prompt(&note_from, kind, summary.as_deref(), &story.text);
        // End the caller's pane. The workspace stays, so the new session
        // opens on the same checkout and branch.
        match self
            .driver(repo)
            .live_handle(&wt, st.terminal_handle.as_deref())
            .await
        {
            Ok(Some(handle)) => {
                if let Err(e) = self.driver(repo).stop_agent(&wt, &handle).await {
                    self.refuse_handover(
                        repo,
                        number,
                        &h,
                        format!("could not stop the running agent: {e:#}"),
                    )
                    .await;
                    return;
                }
            }
            Ok(None) => debug!(session, "no agent to stop; starting the new session"),
            Err(e) => {
                self.refuse_handover(
                    repo,
                    number,
                    &h,
                    format!("could not stop the running agent: {e:#}"),
                )
                .await;
                return;
            }
        }
        // A hold on the item ends here: the session it was held for is
        // gone. The item was told the hold was on, so it is told it is
        // over, before the handover itself is posted.
        if let Some(b) = st.blocked.clone() {
            self.unblock(repo, number, &b, Conversation::HandedOver)
                .await;
        }
        // Items bound to this session mirror its conversation id, so the
        // one being retired goes from them too (`capture_sessions` writes
        // the owner's new id to them once there is one).
        let bound: Vec<u64> = self
            .state
            .repo_mut(&repo.name)
            .issues
            .values()
            .filter(|s| s.shares_workspace_of == Some(number))
            .map(|s| s.number)
            .collect();
        // The conversations the outgoing agent leaves behind: the id on
        // the record, and whatever its harness last wrote in this
        // workspace, which is what a session ssf never captured an id for
        // leaves behind (see `retired_conversations`). The transcript
        // directories live under the real home, so no engine test covers
        // this call; `retired_conversations` is what the tests pin.
        let newest = st
            .worktree_path
            .as_deref()
            .and_then(|p| sessions::capture(&from_harness, p, SystemTime::UNIX_EPOCH, &[]));
        // The old session is retired on the record; everything about the
        // item and its workspace stays.
        let retired =
            retired_conversations(self.entry(repo, number).agent_session_id.take(), newest);
        {
            let e = self.entry(repo, number);
            // The conversations being dropped are remembered, so the
            // harness starting in this workspace is never given the
            // outgoing agent's transcript as its own (`capture_sessions`,
            // `sessions::capture`).
            retire(e, &retired);
            e.terminal_handle = None;
            // The hold, if there was one, was closed just above.
            e.launched_at = None;
            e.handed_over_at = Some(now_iso());
            e.overrides = Some(h.overrides());
            e.handover = None;
            // What the outgoing agent left is kept on the item until a
            // session has read it: the start below can fail, or come up
            // at a sign-in screen, and the harness started again minutes
            // later is the one that takes the work on.
            e.handover_note = Some(HandoverNote {
                from: note_from.clone(),
                summary: summary.clone(),
            });
        }
        for n in bound {
            let e = self.entry(repo, n);
            e.agent_session_id = None;
            retire(e, &retired);
        }
        let eff = self.effective(repo, number);
        let to_launch = self.launch_of(repo, number);
        let title = format!("{} · #{number}", eff.harness);
        let cmd = self.launch_command(repo, number, &st.html_url, &eff.harness_command());
        self.entry(repo, number).launched_at = Some(now_iso());
        let handle = match self
            .driver(repo)
            .start(&wt, &cmd, &title, &eff.harness, &text)
            .await
        {
            Ok(handle) => handle,
            Err(e) => {
                // The handover stands (the item keeps the overrides), but
                // nothing is running: the item is blocked as it is for a
                // harness that comes up at its sign-in prompt, with the
                // same restart-with-backoff recovery, and the old session
                // is not brought back.
                warn!(
                    session,
                    harness = eff.harness,
                    "handed over, but the new harness could not be started: {e:#}"
                );
                self.post_event(
                    repo,
                    number,
                    handed_over(&from_launch, &to_launch, &h, summary.is_some(), None),
                )
                .await;
                let why = safe_error(&events::one_line(&format!("{e:#}")));
                self.set_blocked_for(repo, number, Blocked::START, why)
                    .await;
                self.report_blocked(repo, number).await;
                return;
            }
        };
        info!(
            session,
            harness = eff.harness,
            handle,
            from = from_name,
            summary = summary.is_some(),
            "handed the item over to a new session"
        );
        {
            let e = self.entry(repo, number);
            e.terminal_handle = Some(handle.clone());
            e.last_prompt_at = Some(now_iso());
            e.prompts_sent += 1;
            // The story told the new session everything on the item, so
            // the pass that follows has nothing to deliver again (an
            // onboarding records the same two things for its session).
            e.seen = story.seen;
            e.updated_at = Some(story.updated_at);
        }
        // Nothing is owed to the session that is gone.
        self.failures.remove(&(repo.name.clone(), number));
        self.post_event(
            repo,
            number,
            handed_over(&from_launch, &to_launch, &h, summary.is_some(), None),
        )
        .await;
        // A harness started on a machine it is not signed in on shows its
        // sign-in prompt instead of taking the message: the new session is
        // blocked from here, and the old one is not brought back.
        if let Ok(screen) = self.driver(repo).screen(&handle).await
            && let Some(detail) = crate::driver::login_dialog(&eff.harness, &screen.join("\n"))
        {
            self.set_blocked(repo, number, detail).await;
            self.report_blocked(repo, number).await;
            return;
        }
        // The message went to a harness that took it, not to a sign-in
        // screen: what the outgoing agent left has been read.
        self.entry(repo, number).handover_note = None;
        self.post_event(
            repo,
            number,
            Event::Attached(Attach::HandedOver {
                launch: to_launch,
                from: from_name,
            }),
        )
        .await;
    }

    /// `ssf handover --cancel`: drop a handover the daemon has recorded
    /// and not carried out yet. The session that is there keeps the item,
    /// and hears so if it is still running -- it was told to stop working
    /// when the handover was recorded, and nothing else can reach it
    /// while one is pending. Nothing is posted on the item: the handover
    /// was never announced there.
    async fn cancel_handover(&mut self, session: &str) -> Result<Value> {
        let (repo, number, id) = self.known_session(session)?;
        let st = self.entry(&repo, number).clone();
        let Some(h) = st.handover.clone() else {
            anyhow::bail!("{id}: no handover is pending on this item");
        };
        self.entry(&repo, number).handover = None;
        let name = login::display_name(&h.harness);
        info!(session = id, harness = h.harness, "handover cancelled");
        let live = match st.worktree_id.as_deref() {
            Some(w) => self.driver(&repo).has_live_agent(w).await.unwrap_or(false),
            None => false,
        };
        let mut told = false;
        if live {
            let text = prompt::handover_cancelled_prompt(&name);
            match self.deliver_to(&repo, number, &text, None).await {
                Ok(d) => {
                    told = true;
                    let e = self.entry(&repo, number);
                    e.terminal_handle = Some(d.handle);
                    e.last_prompt_at = Some(now_iso());
                    e.prompts_sent += 1;
                }
                Err(e) => warn!(
                    session = id,
                    "could not tell the agent the handover was cancelled: {e:#}"
                ),
            }
        }
        Ok(serde_json::json!({
            "session": id,
            "title": st.title,
            "harness": h.harness,
            "harness_name": name,
            "requested_at": h.requested_at,
            "told": told,
        }))
    }

    /// A handover that was accepted and cannot be carried out: nothing
    /// changes, the item says so, and the agent that asked (if it is still
    /// there) hears it in one message.
    async fn refuse_handover(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        h: &PendingHandover,
        why: String,
    ) {
        let session = session_id(&repo.name, number);
        warn!(session, harness = h.harness, "handover refused: {why}");
        self.entry(repo, number).handover = None;
        // The reason can carry a driver's or a harness's own words, and
        // both places it goes (the item, and the agent's screen) are
        // read by something that looks for sign-in prompts.
        let why = safe_error(&events::one_line(&why));
        let from = self.launch_of(repo, number);
        let to = self.launch_with(repo, number, Some(&h.overrides()));
        self.post_event(
            repo,
            number,
            handed_over(&from, &to, h, h.summary.is_some(), Some(why.clone())),
        )
        .await;
        let st = self.entry(repo, number).clone();
        let live = match st.worktree_id.as_deref() {
            Some(id) => self.driver(repo).has_live_agent(id).await.unwrap_or(false),
            None => false,
        };
        if !live {
            debug!(session, "no live agent to tell about the refused handover");
            return;
        }
        let text = prompt::handover_refused_prompt(&login::display_name(&h.harness), &why);
        match self.deliver_to(repo, number, &text, None).await {
            Ok(d) => {
                let e = self.entry(repo, number);
                e.terminal_handle = Some(d.handle);
                e.last_prompt_at = Some(now_iso());
                e.prompts_sent += 1;
            }
            Err(e) => warn!(
                session,
                "could not tell the agent about the refused handover: {e:#}"
            ),
        }
    }

    /// `ssf release`: the session's workspace goes on the next pass if the
    /// checks pass now (and again then); `force` skips them, for a person
    /// who has looked, including the check for open items bound to the
    /// session.
    async fn release(&mut self, session: &str, force: bool) -> Result<Value> {
        let (repo, number, id) = self.known_session(session)?;
        let st = self.entry(&repo, number).clone();
        if st.active {
            let (why, fix) = why_active(&st.triggers);
            anyhow::bail!("{id} {why}; its workspace is in use. {fix} first");
        }
        if let Some(h) = st.handover.as_ref() {
            anyhow::bail!("{id}: a handover to {} is pending", h.harness);
        }
        let deps = self.active_dependents(&repo, number);
        if !deps.is_empty() && !force {
            let deps: Vec<String> = deps.iter().map(|n| format!("#{n}")).collect();
            anyhow::bail!(
                "{id} still owns open items ({}); close them first, or a person can release the workspace with `ssf release --as {id} --force` (work in it may be lost)",
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
        info!(session = id, forced = force, "release accepted");
        Ok(serde_json::json!({
            "session": id, "title": st.title, "path": path, "released": true,
            "forced": force, "pending": true, "check": check,
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
                let workspace_gone = !self.driver(&repo).worktree_exists(&id).await?;
                // The driver's workspace can be gone while the git checkout
                // is not (a workspace closed by hand leaves it behind), and
                // the checkout is what holds the work: it is judged like
                // any other, and removed with git rather than the driver.
                let checkout = st
                    .worktree_path
                    .as_deref()
                    .filter(|p| Path::new(p).join(".git").exists());
                if workspace_gone && checkout.is_none() {
                    row["state"] = "already gone".into();
                    if !dry_run {
                        self.mark_released(&repo, st.number);
                        row["removed"] = true.into();
                    }
                    rows.push(row);
                    continue;
                }
                if workspace_gone {
                    row["workspace"] = "gone".into();
                } else if self.driver(&repo).has_live_agent(&id).await.unwrap_or(true) {
                    row["state"] = "agent running".into();
                    rows.push(row);
                    continue;
                }
                let (state, safe, problems) = checkout_state(st.worktree_path.as_deref()).await;
                row["state"] = state.into();
                row["problems"] = problems.into();
                if !dry_run && (safe || force) {
                    let removed = match checkout {
                        Some(path) if workspace_gone => {
                            crate::driver::remove_stray_worktree(path).await
                        }
                        _ => self.driver(&repo).remove_worktree(&id).await,
                    };
                    match removed {
                        Ok(()) => {
                            info!(
                                session,
                                worktree = id,
                                forced = !safe,
                                workspace_gone,
                                "purged the workspace"
                            );
                            self.mark_released(&repo, st.number);
                            self.post_event(
                                &repo,
                                st.number,
                                Event::Released {
                                    by: "ssf purge",
                                    forced: force,
                                    branch: st.branch.clone(),
                                },
                            )
                            .await;
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

async fn conflict_default_base(root: &str) -> Result<String> {
    if let Ok(reference) = conflict_git(
        root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .await
        && !reference.is_empty()
    {
        return Ok(reference);
    }
    let branch = conflict_git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    if branch.is_empty() || branch == "HEAD" {
        anyhow::bail!("cannot tell the base branch of {root}");
    }
    Ok(branch)
}

/// What `ssf release` and `ssf purge` say of a checkout: its short state,
/// whether removing it loses nothing, and the reasons when it would.
async fn checkout_state(path: Option<&str>) -> (String, bool, Vec<String>) {
    match path {
        Some(path) => match release::inspect(path).await {
            Ok(c) => (c.state(), c.safe(), c.problems()),
            Err(e) => ("unknown".into(), false, vec![format!("{e:#}")]),
        },
        None => (
            "unknown".into(),
            false,
            vec!["no workspace path recorded".into()],
        ),
    }
}

/// Remember conversations as ones never to resume or capture again, on
/// one record. Only the last few are kept: a workspace is not handed over
/// dozens of times, and every capture scans the list.
fn retire(e: &mut IssueState, ids: &[String]) {
    for id in ids {
        if !e.retired_session_ids.contains(id) {
            e.retired_session_ids.push(id.clone());
        }
    }
    let extra = e.retired_session_ids.len().saturating_sub(RETIRED_KEPT);
    e.retired_session_ids.drain(..extra);
}

/// The conversations a handover leaves behind in one workspace: the id
/// ssf captured for the outgoing session, and the newest transcript its
/// harness wrote in that workspace, whatever its age.
///
/// The second is what keeps a same-harness handover honest. `now_iso`
/// stamps `handed_over_at` to the whole second, so a transcript the
/// outgoing agent flushed as it exited can carry an mtime inside the
/// capture window; and a session whose id was never captured (the pass
/// that would have done it never ran) leaves nothing on the record to
/// exclude. Either way the newest transcript in the workspace is the one
/// the harness starting next would adopt as its own.
fn retired_conversations(captured: Option<String>, newest: Option<String>) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for id in [captured, newest].into_iter().flatten() {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// How far back `capture_sessions` looks for a workspace's transcript.
/// The launch time is taken a moment early, since the harness writes its
/// transcript around it -- but never back past a handover: the seconds
/// before the launch that followed one hold the outgoing agent's own
/// transcript, and adopting that would resume the session that handed the
/// item away. The slack is kept for the relaunches that come later, whose
/// launch time is long after the handover.
fn capture_since(launched_at: &str, handed_over_at: Option<&str>) -> SystemTime {
    let iso = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(SystemTime::from)
    };
    let Some(launched) = iso(launched_at) else {
        return SystemTime::UNIX_EPOCH;
    };
    let since = launched - Duration::from_secs(5);
    match handed_over_at.and_then(iso) {
        Some(h) if h > since => h,
        _ => since,
    }
}

/// The `handed-over` post of one handover, refused or not. `summary`
/// says whether the new session is given one, which a handover that
/// carries none of its own still does when an earlier one's summary is
/// waiting on the item unread.
fn handed_over(
    from: &events::Launch,
    to: &events::Launch,
    h: &PendingHandover,
    summary: bool,
    refused: Option<String>,
) -> Event {
    Event::HandedOver {
        from: from.clone(),
        to: to.clone(),
        summary,
        by: h.by.clone(),
        refused,
    }
}

/// Retain the socket's older-daemon check while the file lock protects new
/// engines. An installed daemon from before the lock existed has no lock
/// file, but its live socket still tells a one-shot run to leave its state
/// alone.
fn refuse_live_daemon() -> Result<()> {
    let path = crate::ipc::socket_path();
    if std::os::unix::net::UnixStream::connect(&path).is_ok() {
        anyhow::bail!(
            "another ssf daemon is listening on {}; stop it first",
            path.display()
        );
    }
    Ok(())
}

/// Listen for the CLI on the daemon's socket, replacing a stale one.
fn bind_socket() -> Result<tokio::net::UnixListener> {
    let path = crate::ipc::socket_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    if path.exists() {
        // A daemon from before `StateLock` could have bound between this
        // engine's constructor check and this bind. Leave its socket alone;
        // only replace one left by a process that died.
        refuse_live_daemon()?;
        let _ = std::fs::remove_file(&path);
    }
    let listener = tokio::net::UnixListener::bind(&path)
        .with_context(|| format!("listening on {}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600));
    Ok(listener)
}

/// The last comment the bot left on an item, as its session's final word.
/// The daemon's own event posts (a `released` after the agent signed off,
/// say) are not the agent's words and do not count.
fn last_bot_comment(timeline: &[Value], bot: &str) -> Option<FinalComment> {
    timeline
        .iter()
        .rev()
        .filter(|ev| ev.get("event").and_then(Value::as_str) == Some("commented"))
        .find(|ev| {
            actor_of(ev).eq_ignore_ascii_case(bot)
                && !origin::is_event_post(crate::github::value_str(ev, &["body"]).unwrap_or(""))
        })
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

/// An error message fit to write on an item: a harness's sign-in phrases
/// in it (its own words, passed up through a delivery error) are
/// replaced by `[…]`, and the whole message withheld if it still passes
/// for a login prompt after that, so the post can never pass for one
/// when echoed on a screen; the log has it in full. Give it the line as
/// it will be posted (`events::one_line`): the check reads every line of
/// what it is given, but the redaction should be done on the text that
/// goes out, not on a form of it that is collapsed afterwards.
fn safe_error(text: &str) -> String {
    let redacted = crate::driver::redact_login_phrases(text);
    if crate::driver::quotes_login_prompt(&redacted) {
        "(withheld: the message quotes a sign-in prompt; see the daemon log)".into()
    } else {
        redacted
    }
}

/// How long ago an RFC 3339 time was (zero when it cannot be read).
/// How long ago `iso` was, or `None` when it cannot be read as a time or
/// claims to be in the future. `age` reads both of those as "just now",
/// which is the safe answer for a countdown that only ever waits longer;
/// a caller that would wait for ever on it wants to know instead.
fn since(iso: &str) -> Option<Duration> {
    chrono::DateTime::parse_from_rfc3339(iso)
        .ok()
        .and_then(|t| {
            (chrono::Utc::now() - t.with_timezone(&chrono::Utc))
                .to_std()
                .ok()
        })
}

fn age(iso: &str) -> Duration {
    chrono::DateTime::parse_from_rfc3339(iso)
        .ok()
        .and_then(|t| {
            (chrono::Utc::now() - t.with_timezone(&chrono::Utc))
                .to_std()
                .ok()
        })
        .unwrap_or_default()
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
/// Why an item is still the bot's, and what would end that, for the
/// message `ssf release` refuses with. Unassigning only helps an item that
/// is actually assigned, and a mention cannot be withdrawn at all, so the
/// remedy has to follow the trigger the item is held by.
fn why_active(triggers: &[String]) -> (&'static str, &'static str) {
    let has = |t: &str| triggers.iter().any(|x| x == t);
    if has("assigned") {
        ("is still open and assigned", "Close or unassign the item")
    } else if has("review_requested") {
        (
            "still asks the bot for a review",
            "Close the pull request, or withdraw the review request",
        )
    } else if has("mentioned") {
        (
            "is still open and mentions the bot, which is not something anyone can withdraw",
            "Close the item",
        )
    } else if has("created") {
        ("is still open and was opened by the bot", "Close the item")
    } else {
        ("is still open for the bot", "Close the item")
    }
}

/// What an open item said when it was re-read before retiring a session.
#[derive(Debug, PartialEq, Eq)]
enum StillOurs {
    /// Nothing on the item carries a trigger any more: the listings were
    /// right to drop it.
    No,
    /// Something read straight off the item says it is still the bot's:
    /// an assignment, its author, or a live review request. Cheap to ask
    /// and as authoritative as the listing derived from it.
    Certain,
    /// A mention says so, or a re-check could not be made. The walk that
    /// answers it is expensive, so it is paced.
    Paced,
}

/// Whether an item still mentions the bot: in its body, in a comment, or
/// in one of a pull request's inline review comments. That is what puts it
/// on the `mentioned` listing, so it is what says the listing was right to
/// carry it. `allow::askers` already walks exactly that shape for the
/// gate, so this asks it rather than walking the timeline again.
fn mentions_bot(issue: &Issue, timeline: &[Value], login: &str) -> bool {
    !crate::allow::askers(issue, timeline, &["mentioned".to_string()], login).is_empty()
}

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

    pub(super) fn engine() -> Engine {
        let _ = rustls::crypto::ring::default_provider().install_default();
        // These tests are about everything but access: anyone may drive.
        // The allow-list tests below set their own lists.
        let mut cfg = Config::default();
        cfg.daemon.allowed_users = Some(vec!["*".into()]);
        cfg.daemon.accepted_anyone_risk = true;
        // The stand-in driver below is Orca; herdr is the default now.
        cfg.driver = Some(DriverKind::Orca);
        Engine {
            cfg,
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
            collaborators: BTreeMap::new(),
            dropped_logged: std::sync::Mutex::new(BTreeSet::new()),
            probe: std::sync::Arc::new(|_| Probe {
                state: LoginState::Unknown,
                detail: "test".into(),
                fingerprint: None,
            }),
            installed: std::sync::Arc::new(|_| true),
            probes: BTreeMap::new(),
            refetch: BTreeSet::new(),
            startup_pass: false,
            onboarding: None,
            conflict_checks: BTreeMap::new(),
            conflict_pairs: BTreeMap::new(),
            _state_lock: None,
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
        e.cfg.driver = Some(DriverKind::Herdr);
        e.cfg.repos = vec![herdr.clone()];
        e.sync_drivers();
        assert_eq!(e.drivers.kinds(), vec![DriverKind::Herdr]);
    }

    pub(super) fn repo() -> RepoConfig {
        RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            ..Default::default()
        }
    }

    /// A real checkout with one branch that conflicts with a moved `main`,
    /// plus a stub session that can receive the advisory.
    async fn conflict_fixture(
        name: &str,
        number: u64,
    ) -> (
        Engine,
        RepoConfig,
        crate::driver::StubDriver,
        crate::release::testkit::Scratch,
        String,
    ) {
        use crate::release::testkit::{scratch, sh};

        let s = scratch(name).await;
        let worktree_name = format!("issue-{number}-conflict");
        let (path, branch) = crate::driver::add_local_worktree(&s.work, &worktree_name, None)
            .await
            .unwrap();
        std::fs::write(std::path::Path::new(&path).join("a.txt"), "feature\n").unwrap();
        sh(&path, &["add", "a.txt"]).await;
        sh(&path, &["commit", "-q", "-m", "feature"]).await;
        std::fs::write(std::path::Path::new(&s.work).join("a.txt"), "base\n").unwrap();
        sh(&s.work, &["add", "a.txt"]).await;
        sh(&s.work, &["commit", "-q", "-m", "base"]).await;
        sh(&s.work, &["push", "-q", "origin", "main"]).await;
        // Do not rely on remote.origin.fetch: the production check supplies
        // an explicit branch refspec, which this narrow mapping exercises.
        sh(
            &s.work,
            &[
                "config",
                "remote.origin.fetch",
                "+refs/heads/other:refs/remotes/origin/other",
            ],
        )
        .await;
        sh(&s.work, &["remote", "set-head", "origin", "main"]).await;
        // Remove the stale tracking ref: only the explicit refspec in the
        // conflict check can restore it under this narrow mapping.
        sh(&s.work, &["update-ref", "-d", "refs/remotes/origin/main"]).await;

        let mut e = engine();
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.daemon.conflict_check_interval_secs = 1;
        let mut r = repo();
        r.path = Some(s.work.clone());
        d.seed(&format!("w{number}"), &format!("t{number}"), READY_SCREEN);
        {
            let st = e.entry(&r, number);
            st.html_url = format!("https://gh/{number}");
            st.seeded = true;
            st.active = true;
            st.worktree_id = Some(format!("w{number}"));
            st.worktree_path = Some(path.clone());
            st.repo_id = Some(s.work.clone());
            st.branch = Some(branch);
        }
        (e, r, d, s, path)
    }

    fn issue(number: u64, author: &str, body: Option<&str>) -> Issue {
        serde_json::from_value(json!({
            "number": number, "title": "t", "body": body, "html_url": format!("https://gh/{number}"),
            "state": "open", "user": {"login": author}, "created_at": "x", "updated_at": "x"
        }))
        .unwrap()
    }

    /// Put an ignored item's absence (and the look it has had) `by` into
    /// the past, the way waiting would.
    fn rewind_absence(e: &mut Engine, r: &RepoConfig, number: u64, by: Duration) {
        let then = (chrono::Utc::now() - chrono::Duration::from_std(by).unwrap())
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let at = e
            .state
            .repo_mut(&r.name)
            .ignored
            .get_mut(&number)
            .expect("an ignore record to rewind");
        if at.absent_since.is_some() {
            at.absent_since = Some(then.clone());
        }
        if at.asked_at.is_some() {
            at.asked_at = Some(then);
        }
    }

    /// The item numbers a pass asked GitHub about by number.
    fn looked_at(hits: &[String]) -> BTreeSet<u64> {
        hits.iter()
            .filter_map(|h| h.strip_prefix("/repos/o/r/issues/"))
            .filter_map(|n| n.parse().ok())
            .collect()
    }

    fn ignored_numbers(e: &Engine, r: &RepoConfig) -> Vec<u64> {
        e.state.repos[&r.name].ignored.keys().copied().collect()
    }

    pub(super) fn seeded(e: &mut Engine, number: u64, branch: Option<&str>, active: bool) {
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
            comment(
                8,
                "bot",
                "🤖 ssf <!-- ssf: origin=o/r#3 event=attached -->\n\n```ssf\nssf attaching agent to issue:\nharness: Claude Code\n```",
            ),
        ];
        let d = e.diff(&r, &BTreeMap::new(), &timeline);
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
            "the untagged bot comment is a person's; tagged ones stay; the daemon's event post is nobody's"
        );
        assert_eq!(d.seen.len(), 7, "everything is recorded as seen");
        assert!(d.seen.contains_key("commented:8"));
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
        let d = e.diff(&r, &BTreeMap::new(), &timeline);
        let keys: Vec<&str> = d.rendered.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, vec!["commented:22"]);
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
        assert_eq!(e.resume_candidates(&r), vec![1, 11]);
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
            // The daemon's own post after the agent's last word is not it.
            json!({"event":"commented","id":5,"user":{"login":"bot"},"body":"🤖 ssf <!-- ssf: origin=o/r#5 event=released -->\n\n```ssf\nssf releasing workspace of issue:\nby: ssf release\n```","html_url":"u5"}),
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
        /// How many times the creator listing has been served in full (a
        /// request with no `If-None-Match`, or one whose ETag has moved
        /// on), rather than answered 304.
        created_fulls: std::sync::Arc<std::sync::atomic::AtomicU32>,
        /// Open items assigned to the bot, as the assignee listing reports
        /// them (a fresh ETag every time: always a full listing).
        assigned: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
        /// Timelines by item number (`[]` for an unknown item).
        timelines: std::sync::Arc<std::sync::Mutex<BTreeMap<u64, Vec<Value>>>>,
        /// Items served by number (`/repos/o/r/issues/N`), for the paths
        /// that read one item rather than a listing (a session's story).
        issues: std::sync::Arc<std::sync::Mutex<BTreeMap<u64, Value>>>,
        /// Pull requests served by number (`/repos/o/r/pulls/N`). A number
        /// that is not here answers 500, which is how a test says the
        /// fetch failed.
        pulls: std::sync::Arc<std::sync::Mutex<BTreeMap<u64, Value>>>,
        /// The collaborators endpoint: `None` answers 403 (no access), a
        /// list is served with an ETag that changes when it is set.
        collaborators: std::sync::Arc<std::sync::Mutex<Option<Vec<Value>>>>,
        collab_version: std::sync::Arc<std::sync::atomic::AtomicU32>,
        /// Comments posted (`/repos/o/r/issues/N/comments`), in order:
        /// the path and the comment body.
        posts: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
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
            let created_fulls = Arc::new(AtomicU32::new(0));
            let assigned: Arc<Mutex<Vec<Value>>> = Arc::default();
            let timelines: Arc<Mutex<BTreeMap<u64, Vec<Value>>>> = Arc::default();
            let issues: Arc<Mutex<BTreeMap<u64, Value>>> = Arc::default();
            let pulls: Arc<Mutex<BTreeMap<u64, Value>>> = Arc::default();
            let collaborators: Arc<Mutex<Option<Vec<Value>>>> = Arc::default();
            let collab_version = Arc::new(AtomicU32::new(1));
            let posts: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
            let p = posts.clone();
            let (h, c, v) = (hits.clone(), created.clone(), created_etag.clone());
            let cf = created_fulls.clone();
            let (a, t, k, kv) = (
                assigned.clone(),
                timelines.clone(),
                collaborators.clone(),
                collab_version.clone(),
            );
            let i = issues.clone();
            let pl = pulls.clone();
            tokio::spawn(async move {
                let other_etags = AtomicU32::new(1);
                loop {
                    let Ok((mut sock, _)) = listener.accept().await else {
                        return;
                    };
                    // The head, then as much body as `Content-Length` says.
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    let mut head_len = None;
                    loop {
                        if head_len.is_none() {
                            head_len = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
                        }
                        if let Some(hl) = head_len {
                            let head = String::from_utf8_lossy(&buf[..hl]);
                            let len = head
                                .lines()
                                .find_map(|l| {
                                    let (k, v) = l.split_once(':')?;
                                    k.eq_ignore_ascii_case("content-length")
                                        .then(|| v.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            if buf.len() >= hl + len {
                                break;
                            }
                        }
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let hl = head_len.unwrap_or(buf.len());
                    let head = String::from_utf8_lossy(&buf[..hl]).to_string();
                    let sent = String::from_utf8_lossy(&buf[hl..]).to_string();
                    let mut lines = head.lines();
                    let first = lines.next().unwrap_or("").to_string();
                    let method = first.split(' ').next().unwrap_or("").to_string();
                    let target = first.split(' ').nth(1).unwrap_or("").to_string();
                    let if_none_match = lines.find_map(|l| {
                        let (k, val) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("if-none-match")
                            .then(|| val.trim().to_string())
                    });
                    h.lock().unwrap().push(target.clone());
                    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                    let (status, etag, body) = if path == "/user" {
                        (
                            "200 OK",
                            "\"user\"".to_string(),
                            r#"{"login":"bot","id":1,"type":"User"}"#.to_string(),
                        )
                    } else if method == "POST" && path.ends_with("/comments") {
                        let comment = serde_json::from_str::<Value>(&sent)
                            .ok()
                            .and_then(|v| v["body"].as_str().map(str::to_string))
                            .unwrap_or(sent.clone());
                        p.lock().unwrap().push((path.to_string(), comment));
                        (
                            "201 Created",
                            "\"p\"".to_string(),
                            r#"{"html_url":"https://gh/comment"}"#.to_string(),
                        )
                    } else if path == "/repos/o/r/issues" && query.starts_with("creator=") {
                        let etag = format!("\"c{}\"", v.load(Ordering::SeqCst));
                        if if_none_match.as_deref() == Some(etag.as_str()) {
                            ("304 Not Modified", etag, String::new())
                        } else {
                            cf.fetch_add(1, Ordering::SeqCst);
                            let items = Value::Array(c.lock().unwrap().clone());
                            ("200 OK", etag, items.to_string())
                        }
                    } else if path == "/repos/o/r/issues" && query.starts_with("assignee=") {
                        let n = other_etags.fetch_add(1, Ordering::SeqCst);
                        let items = Value::Array(a.lock().unwrap().clone());
                        ("200 OK", format!("\"o{n}\""), items.to_string())
                    } else if path == "/repos/o/r/issues" || path == "/repos/o/r/pulls" {
                        let n = other_etags.fetch_add(1, Ordering::SeqCst);
                        ("200 OK", format!("\"o{n}\""), "[]".to_string())
                    } else if path == "/repos/o/r/collaborators" {
                        let etag = format!("\"k{}\"", kv.load(Ordering::SeqCst));
                        match k.lock().unwrap().clone() {
                                None => (
                                    "403 Forbidden",
                                    etag,
                                    r#"{"message":"Must have push access to view repository collaborators."}"#
                                        .to_string(),
                                ),
                                Some(_) if if_none_match.as_deref() == Some(etag.as_str()) => {
                                    ("304 Not Modified", etag, String::new())
                                }
                                Some(list) => ("200 OK", etag, Value::Array(list).to_string()),
                            }
                    } else if let Some(pull) = path
                        .strip_prefix("/repos/o/r/pulls/")
                        .and_then(|n| n.parse::<u64>().ok())
                        .and_then(|n| pl.lock().unwrap().get(&n).cloned())
                    {
                        ("200 OK", "\"pr\"".to_string(), pull.to_string())
                    } else if let Some(item) = path
                        .strip_prefix("/repos/o/r/issues/")
                        .and_then(|n| n.parse::<u64>().ok())
                        .and_then(|n| i.lock().unwrap().get(&n).cloned())
                    {
                        // A null stands for an item GitHub does not have.
                        if item.is_null() {
                            (
                                "404 Not Found",
                                "\"i\"".to_string(),
                                r#"{"message":"Not Found"}"#.to_string(),
                            )
                        } else {
                            ("200 OK", "\"i\"".to_string(), item.to_string())
                        }
                    } else if let Some(n) = path
                        .strip_prefix("/repos/o/r/issues/")
                        .and_then(|rest| rest.strip_suffix("/timeline"))
                        .and_then(|n| n.parse::<u64>().ok())
                    {
                        let events = t.lock().unwrap().get(&n).cloned().unwrap_or_default();
                        (
                            "200 OK",
                            "\"t\"".to_string(),
                            Value::Array(events).to_string(),
                        )
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
                created_fulls,
                assigned,
                timelines,
                issues,
                pulls,
                collaborators,
                collab_version,
                posts,
            }
        }

        /// The comment endpoints posted to since the last call.
        fn posts(&self) -> Vec<String> {
            self.post_bodies().into_iter().map(|(p, _)| p).collect()
        }

        /// The comments posted since the last call: endpoint and body.
        fn post_bodies(&self) -> Vec<(String, String)> {
            std::mem::take(&mut *self.posts.lock().unwrap())
        }

        fn set_collaborators(&self, list: Option<Vec<Value>>) {
            *self.collaborators.lock().unwrap() = list;
            self.collab_version
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }

        /// Serve one item by number, for the paths that read it directly.
        fn set_issue(&self, number: u64, item: Value) {
            self.issues.lock().unwrap().insert(number, item);
        }

        /// Answer 404 for one item, as GitHub does for one that was
        /// deleted, or that this token may not read any more.
        fn set_missing(&self, number: u64) {
            self.issues.lock().unwrap().insert(number, Value::Null);
        }

        /// Serve one pull request by number. A number never set answers
        /// 500, so a test can say the fetch failed.
        fn set_pull(&self, number: u64, pull: Value) {
            self.pulls.lock().unwrap().insert(number, pull);
        }

        fn set_timeline(&self, number: u64, events: Vec<Value>) {
            self.timelines.lock().unwrap().insert(number, events);
        }

        fn set_assigned(&self, items: Vec<Value>) {
            *self.assigned.lock().unwrap() = items;
        }

        /// The request paths since the last call.
        fn hits(&self) -> Vec<String> {
            std::mem::take(&mut *self.hits.lock().unwrap())
        }

        /// How many full creator listings have been served so far.
        fn created_fulls(&self) -> u32 {
            self.created_fulls.load(std::sync::atomic::Ordering::SeqCst)
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

    #[tokio::test]
    async fn engine_constructor_refuses_a_second_owner_before_auth_or_state_access() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let mut cfg = Config::default();
        cfg.github.api_url = stub.base.clone();
        cfg.github.token = Some("test-token".into());

        let first = Engine::new(cfg.clone()).await.unwrap();
        assert_eq!(stub.hits(), vec!["/user"]);
        let before = std::fs::read(crate::state::state_path()).unwrap();

        let err = match Engine::new(cfg.clone()).await {
            Ok(_) => panic!("a second engine acquired the same state directory"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("another ssf daemon is listening"));
        assert!(stub.hits().is_empty(), "the rejected engine called GitHub");
        assert_eq!(
            std::fs::read(crate::state::state_path()).unwrap(),
            before,
            "the rejected engine rewrote state"
        );

        drop(first);
        let second = Engine::new(cfg).await.unwrap();
        assert_eq!(stub.hits(), vec!["/user"]);
        drop(second);
    }

    #[tokio::test]
    async fn engine_constructor_refuses_a_live_socket_before_auth_or_state_access() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let mut cfg = Config::default();
        cfg.github.api_url = stub.base.clone();
        cfg.github.token = Some("test-token".into());
        let path = crate::ipc::socket_path();
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let err = match Engine::new(cfg).await {
            Ok(_) => panic!("an engine started beside a legacy daemon socket"),
            Err(err) => err,
        };
        assert_eq!(
            err.to_string(),
            format!(
                "another ssf daemon is listening on {}; stop it first",
                path.display()
            )
        );
        assert!(stub.hits().is_empty(), "the refused engine called GitHub");
        assert!(
            !crate::state::state_path().exists(),
            "the refused engine created state"
        );
        drop(listener);
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

        // An item that leaves every listing is looked at before its record
        // is dropped (issue #138): closed, so #19's record goes, while #18
        // is still listed and keeps its own.
        e.state
            .repo_mut(&r.name)
            .ignored
            .insert(19, Ignored::new(&issue(19, "bot", None), &created));
        stub.set_issue(
            19,
            json!({
                "number": 19, "title": "t", "body": null, "html_url": "https://gh/19",
                "state": "closed", "user": {"login": "bot"}, "created_at": "x", "updated_at": "x"
            }),
        );
        *stub.created.lock().unwrap() = vec![listed(18)];
        stub.bump_created_etag();
        let _ = stub.hits();
        e.tick_repo(&r).await.unwrap();
        let hits = stub.hits();
        assert!(
            hits.contains(&"/repos/o/r/issues/19".to_string()),
            "#19 was looked at before being forgotten: {hits:?}"
        );
        assert!(
            !hits.iter().any(|h| h.starts_with("/repos/o/r/issues/18")),
            "#18 is still listed, so nothing is asked about it: {hits:?}"
        );
        assert_eq!(
            e.state.repos[&r.name]
                .ignored
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![18]
        );
    }

    /// Replays issue #141: `forget_etags` armed the `refetch` flag and
    /// `tick_repo` spent the flag by calling `forget_etags` again, which
    /// armed it afresh, so one `unblock` made the repository fetch all
    /// four listings in full on every pass for the life of the daemon.
    /// One unblock owes exactly one full fetch, and the pass after it is
    /// back to conditional requests.
    #[tokio::test]
    async fn one_unblock_owes_exactly_one_full_fetch() {
        let stub = GitHubStub::start().await;
        let r = repo();
        let created = vec!["created".to_string()];
        *stub.created.lock().unwrap() = vec![json!({
            "number": 18, "title": "t", "body": null, "html_url": "https://gh/18",
            "state": "open", "user": {"login": "bot"}, "created_at": "x", "updated_at": "x"
        })];
        let mut e = engine_at(&stub.base);
        // The one listed item is already ignored as created-only, so these
        // passes fetch nothing by number and onboard nothing.
        e.state
            .repo_mut(&r.name)
            .ignored
            .insert(18, Ignored::new(&issue(18, "bot", None), &created));

        // Pass 1: no ETags yet, so a full creator listing. That listing is
        // the one the stub answers 304 to, so it stands for all four here.
        e.tick_repo(&r).await.unwrap();
        assert_eq!(stub.created_fulls(), 1);

        // Pass 2: the ETag it stored is sent back and answered 304.
        e.tick_repo(&r).await.unwrap();
        assert_eq!(stub.created_fulls(), 1, "pass 2 was conditional");

        // A session comes back. The pass it comes back on has read its
        // listings already (and stores the ETags it read at its end), so
        // the full fetch it is owed falls to the next pass.
        let b = Blocked {
            reason: "login".into(),
            harness: "claude".into(),
            detail: "Login expired".into(),
            since: now_iso(),
            reported: false,
            credential: None,
            retried_at: None,
            retries: 0,
            told_at: None,
            tell_failures: 0,
        };
        e.unblock(&r, 18, &b, Conversation::Kept).await;
        assert!(e.refetch.contains(&r.name));
        assert!(e.state.repos[&r.name].created_etag.is_none());

        // Pass 3 spends the flag: one full listing, so what was held is seen.
        e.tick_repo(&r).await.unwrap();
        assert_eq!(stub.created_fulls(), 2, "pass 3 fetched in full");
        assert!(e.refetch.is_empty(), "the flag is spent, not re-armed");

        // Pass 4, and every pass after it, is back to conditional requests.
        for _ in 0..3 {
            e.tick_repo(&r).await.unwrap();
        }
        assert_eq!(stub.created_fulls(), 2, "one unblock, one full fetch");
        assert!(e.refetch.is_empty());
        assert!(e.failures.is_empty(), "{:?}", e.failures);
        assert!(stub.post_bodies().is_empty());
    }

    /// Resetting the owed `refetch` with `tick`'s per-pass state loses the
    /// full listing after a session comes back late in the preceding pass.
    /// Exercise the public pass boundary with cached ETags restored: the
    /// owed pass is full once, and the pass after it is conditional again.
    #[tokio::test(flavor = "current_thread")]
    async fn tick_preserves_one_owed_full_fetch_across_the_pass_boundary() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let r = repo();
        let created = vec!["created".to_string()];
        *stub.created.lock().unwrap() = vec![json!({
            "number": 18, "title": "t", "body": null, "html_url": "https://gh/18",
            "state": "open", "user": {"login": "bot"}, "created_at": "x", "updated_at": "x"
        })];
        let mut e = engine_at(&stub.base);
        e.drivers = Drivers::from_list(vec![Driver::Stub(crate::driver::StubDriver::new(
            DriverKind::Orca,
        ))]);
        e.cfg.github.api_url = stub.base.clone();
        e.cfg.repos = vec![r.clone()];
        e.cfg.daemon.conflict_check_interval_secs = 0;
        e.cfg.save().unwrap();
        e.state
            .repo_mut(&r.name)
            .ignored
            .insert(18, Ignored::new(&issue(18, "bot", None), &created));

        // Establish and then exercise the cached creator-listing ETag.
        e.tick().await;
        assert!(e.state.last_error.is_none(), "{:?}", e.state.last_error);
        assert_eq!(stub.created_fulls(), 1, "the first pass was full");
        e.tick().await;
        assert!(e.state.last_error.is_none(), "{:?}", e.state.last_error);
        assert_eq!(stub.created_fulls(), 1, "the second pass was conditional");

        // A session comes back after its pass read the listings. That pass
        // writes its cached ETag back, leaving only `refetch` to make the
        // next outer tick fetch the listing in full.
        let b = Blocked {
            reason: "login".into(),
            harness: "claude".into(),
            detail: "Login expired".into(),
            since: now_iso(),
            reported: false,
            credential: None,
            retried_at: None,
            retries: 0,
            told_at: None,
            tell_failures: 0,
        };
        let read_this_pass = e.state.repos[&r.name].created_etag.clone().unwrap();
        e.unblock(&r, 18, &b, Conversation::Kept).await;
        e.state.repo_mut(&r.name).created_etag = Some(read_this_pass);

        e.tick().await;
        assert!(e.state.last_error.is_none(), "{:?}", e.state.last_error);
        assert_eq!(stub.created_fulls(), 2, "the owed pass was full");
        assert!(e.refetch.is_empty(), "the owed fetch was spent once");

        let _ = stub.hits();
        e.tick().await;
        assert!(e.state.last_error.is_none(), "{:?}", e.state.last_error);
        let hits = stub.hits();
        assert_eq!(
            hits.iter()
                .filter(|h| h.starts_with("/repos/o/r/issues?creator="))
                .count(),
            1,
            "the final pass requested the creator listing: {hits:?}"
        );
        assert_eq!(
            stub.created_fulls(),
            2,
            "the pass after the owed fetch was conditional"
        );
        assert!(e.refetch.is_empty(), "the owed fetch was not re-armed");
        assert!(e.failures.is_empty(), "{:?}", e.failures);
        assert!(stub.post_bodies().is_empty());
    }

    /// The case the `refetch` flag exists for at the repository-pass level:
    /// a session that comes back after the pass has read its listings
    /// (`reconcile_issue`, rather than `check_logins`). That pass
    /// stores the ETags it read at its end, putting back the ones the
    /// unblock cleared, so only the flag can make the next pass a full one
    /// — and only the next one.
    #[tokio::test]
    async fn an_unblock_after_the_listings_were_read_makes_the_next_pass_full() {
        let stub = GitHubStub::start().await;
        let r = repo();
        let created = vec!["created".to_string()];
        *stub.created.lock().unwrap() = vec![json!({
            "number": 18, "title": "t", "body": null, "html_url": "https://gh/18",
            "state": "open", "user": {"login": "bot"}, "created_at": "x", "updated_at": "x"
        })];
        let mut e = engine_at(&stub.base);
        e.state
            .repo_mut(&r.name)
            .ignored
            .insert(18, Ignored::new(&issue(18, "bot", None), &created));

        // Pass 1 has no ETags, pass 2 sends the ones it stored and is
        // answered 304.
        e.tick_repo(&r).await.unwrap();
        assert_eq!(stub.created_fulls(), 1);
        e.tick_repo(&r).await.unwrap();
        assert_eq!(stub.created_fulls(), 1, "pass 2 was conditional");

        // The session comes back part-way through a pass that has already
        // read its listings: the ETags it clears are written back when that
        // pass stores what it read, so nothing but the flag survives it.
        let b = Blocked {
            reason: "login".into(),
            harness: "claude".into(),
            detail: "Login expired".into(),
            since: now_iso(),
            reported: false,
            credential: None,
            retried_at: None,
            retries: 0,
            told_at: None,
            tell_failures: 0,
        };
        let read_this_pass = e.state.repos[&r.name].created_etag.clone();
        assert!(read_this_pass.is_some());
        e.unblock(&r, 18, &b, Conversation::Kept).await;
        e.state.repo_mut(&r.name).created_etag = read_this_pass;

        // The next pass is a full one on the strength of the flag alone.
        e.tick_repo(&r).await.unwrap();
        assert_eq!(stub.created_fulls(), 2, "the next pass was full");
        assert!(e.refetch.is_empty());

        // And only that one: the passes after it are conditional again.
        for _ in 0..3 {
            e.tick_repo(&r).await.unwrap();
        }
        assert_eq!(stub.created_fulls(), 2, "one unblock, one full fetch");
        assert!(e.failures.is_empty(), "{:?}", e.failures);
        assert!(stub.post_bodies().is_empty());
    }

    /// A bot-opened item with no usable origin tag is onboarded once and
    /// then skipped, not re-onboarded on every pass. This is what issue
    /// #138 suspected was broken; it was not, and this guards the path it
    /// named (it passes without the rest of this change).
    #[tokio::test]
    async fn a_rejected_bot_opened_item_is_not_onboarded_again() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        *stub.created.lock().unwrap() = vec![json!({
            "number": 18, "title": "t", "body": "no tag here",
            "html_url": "https://gh/18", "state": "open",
            "user": {"login": "bot"}, "created_at": "x", "updated_at": "x"
        })];

        // Pass 1: onboarded, found to be nobody's, ignored.
        e.tick_repo(&r).await.unwrap();
        assert!(e.failures.is_empty(), "{:?}", e.failures);
        let hits = stub.hits();
        assert!(
            hits.iter()
                .any(|h| h.starts_with("/repos/o/r/issues/18/timeline")),
            "{hits:?}"
        );
        assert!(
            e.state.repos[&r.name].ignored.contains_key(&18),
            "the rejection records an ignore: {:?}",
            e.state.repos[&r.name].ignored
        );

        // Pass 2, listing unchanged (304): nothing is looked at.
        e.tick_repo(&r).await.unwrap();
        assert_listings_only(&stub.hits());

        // Pass 3, the same listing served in full (GitHub rolled its ETag):
        // still nothing.
        stub.bump_created_etag();
        e.tick_repo(&r).await.unwrap();
        assert_listings_only(&stub.hits());
        assert!(
            e.state.repos[&r.name].ignored.contains_key(&18),
            "the record survives the pass"
        );
    }

    /// A bot-opened item with no usable origin tag, on a stub that serves
    /// it from the `creator` listing and by number.
    fn untagged_listed(n: u64, state: &str) -> Value {
        json!({
            "number": n, "title": "t", "body": "no tag here",
            "html_url": format!("https://gh/{n}"), "state": state,
            "user": {"login": "bot"}, "created_at": "x", "updated_at": "x"
        })
    }

    /// Issue #138: the daemon onboarded the same 21 bot-opened items every
    /// half hour. The rejection records the ignore (and did before this),
    /// but the prune at the end of a pass dropped every record whose item
    /// was missing from the listings that pass, and GitHub's filtered
    /// listings come back short now and then — `retire_issue` has guarded
    /// sessions against exactly that lag from the start. So: a listing
    /// that comes back short costs no re-onboarding when it recovers.
    #[tokio::test]
    async fn a_short_listing_does_not_throw_away_ignore_records() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        *stub.created.lock().unwrap() = vec![untagged_listed(18, "open")];
        stub.set_issue(18, untagged_listed(18, "open"));

        // Onboarded once, found to be nobody's, ignored.
        e.tick_repo(&r).await.unwrap();
        assert!(e.failures.is_empty(), "{:?}", e.failures);
        assert_eq!(ignored_numbers(&e, &r), vec![18]);

        // The listing comes back empty, and then it recovers.
        *stub.created.lock().unwrap() = vec![];
        stub.bump_created_etag();
        e.tick_repo(&r).await.unwrap();
        assert_eq!(ignored_numbers(&e, &r), vec![18], "the record stands");
        *stub.created.lock().unwrap() = vec![untagged_listed(18, "open")];
        stub.bump_created_etag();
        let _ = stub.hits();
        e.tick_repo(&r).await.unwrap();

        // Nothing was fetched by number, so nothing was onboarded again.
        assert_listings_only(&stub.hits());
        assert!(e.failures.is_empty(), "{:?}", e.failures);
        assert_eq!(ignored_numbers(&e, &r), vec![18]);
    }

    /// The other half of the same prune: an item that is really gone loses
    /// its record, and an absence is asked about once rather than on every
    /// pass it lasts.
    #[tokio::test]
    async fn an_ignored_item_is_asked_about_once_and_forgotten_when_it_is_gone() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        *stub.created.lock().unwrap() =
            vec![untagged_listed(18, "open"), untagged_listed(19, "open")];
        stub.set_issue(18, untagged_listed(18, "open"));
        stub.set_issue(19, untagged_listed(19, "open"));
        e.tick_repo(&r).await.unwrap();
        assert_eq!(ignored_numbers(&e, &r), vec![18, 19]);

        // Both missing: each is asked about once...
        *stub.created.lock().unwrap() = vec![];
        stub.bump_created_etag();
        let _ = stub.hits();
        e.tick_repo(&r).await.unwrap();
        let hits = stub.hits();
        for n in [18, 19] {
            assert!(
                hits.contains(&format!("/repos/o/r/issues/{n}")),
                "asked about #{n}: {hits:?}"
            );
        }

        // ...and not again while the absence lasts.
        stub.bump_created_etag();
        e.tick_repo(&r).await.unwrap();
        let hits = stub.hits();
        assert!(
            !hits.iter().any(|h| h.starts_with("/repos/o/r/issues/1")),
            "{hits:?}"
        );
        assert_eq!(ignored_numbers(&e, &r), vec![18, 19]);

        // #19 comes back on the listing and goes missing again, closed
        // this time. A listing that flaps buys no look of its own: the
        // record is asked about again when the next look is due, and its
        // record goes then. #18 keeps its own.
        *stub.created.lock().unwrap() =
            vec![untagged_listed(18, "open"), untagged_listed(19, "open")];
        stub.bump_created_etag();
        e.tick_repo(&r).await.unwrap();
        *stub.created.lock().unwrap() = vec![untagged_listed(18, "open")];
        stub.set_issue(19, untagged_listed(19, "closed"));
        stub.bump_created_etag();
        let _ = stub.hits();
        e.tick_repo(&r).await.unwrap();
        assert_listings_only(&stub.hits());
        assert_eq!(
            ignored_numbers(&e, &r),
            vec![18, 19],
            "asked about too soon"
        );

        rewind_absence(&mut e, &r, 19, ABSENT_RECHECK);
        stub.bump_created_etag();
        e.tick_repo(&r).await.unwrap();
        assert_eq!(ignored_numbers(&e, &r), vec![18]);
        assert!(e.failures.is_empty(), "{:?}", e.failures);

        // #18 goes missing while open: looked at once, record kept, and
        // then left alone. An item that never comes back (its mention
        // edited away, say) would otherwise be looked at for ever, so the
        // record is given up once the absence is too long to be a lagging
        // listing — without asking GitHub again.
        *stub.created.lock().unwrap() = vec![];
        stub.bump_created_etag();
        e.tick_repo(&r).await.unwrap();
        assert_eq!(ignored_numbers(&e, &r), vec![18]);
        let _ = stub.hits();
        stub.bump_created_etag();
        e.tick_repo(&r).await.unwrap();
        assert_listings_only(&stub.hits());
        assert_eq!(ignored_numbers(&e, &r), vec![18], "given up too soon");

        rewind_absence(&mut e, &r, 18, ABSENT_GIVE_UP);
        stub.bump_created_etag();
        e.tick_repo(&r).await.unwrap();
        assert!(ignored_numbers(&e, &r).is_empty(), "the record goes");
        assert_listings_only(&stub.hits());
    }

    /// The record of an item GitHub 404s for (deleted, or no longer
    /// readable with this token) goes; a fetch that fails
    /// answers nothing, so that record stands and is not asked about
    /// again until the backoff is up.
    #[tokio::test]
    async fn an_ignored_item_gone_from_github_loses_its_record_but_a_failure_does_not() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        let created = vec!["created".to_string()];
        for n in [18, 19] {
            e.state
                .repo_mut(&r.name)
                .ignored
                .insert(n, Ignored::new(&issue(n, "bot", None), &created));
        }
        // #18 is gone from GitHub; #19's fetch fails (the stub answers 500
        // for an item it was not given).
        stub.set_missing(18);
        *stub.created.lock().unwrap() = vec![];
        e.tick_repo(&r).await.unwrap();
        assert_eq!(ignored_numbers(&e, &r), vec![19]);

        // The failure is not retried on every pass while it lasts.
        let _ = stub.hits();
        stub.bump_created_etag();
        e.tick_repo(&r).await.unwrap();
        assert_listings_only(&stub.hits());
        assert_eq!(ignored_numbers(&e, &r), vec![19]);
    }

    /// More absent records than a pass looks at: the looks are rationed
    /// and rotate, so every record's turn comes round, while giving up
    /// costs no request and so waits for nothing.
    #[tokio::test]
    async fn absent_records_beyond_a_pass_s_looks_are_not_starved() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        let created = vec!["created".to_string()];
        let n = ABSENT_LOOKS_PER_PASS as u64;
        // Twice what a pass looks at, all open and on no listing, plus one
        // that has been absent long enough to be given up.
        for i in 1..=2 * n {
            e.state
                .repo_mut(&r.name)
                .ignored
                .insert(i, Ignored::new(&issue(i, "bot", None), &created));
            stub.set_issue(i, untagged_listed(i, "open"));
        }
        e.state
            .repo_mut(&r.name)
            .ignored
            .insert(9000, Ignored::new(&issue(9000, "bot", None), &created));

        e.tick_repo(&r).await.unwrap();
        let mut asked: BTreeSet<u64> = looked_at(&stub.hits());
        assert_eq!(asked.len(), ABSENT_LOOKS_PER_PASS, "one pass's worth");

        // Every record is due again on the next pass, which is the
        // ordinary case: the prune only runs on a pass where a listing
        // changed, and those are rarer than the look interval.
        for i in 1..=2 * n {
            rewind_absence(&mut e, &r, i, ABSENT_RECHECK);
        }
        // #9000 sorts last by number and was never looked at, so under a
        // budget that ran in number order it would wait behind everything.
        // It is past the giving up, which the budget does not ration.
        rewind_absence(&mut e, &r, 9000, ABSENT_GIVE_UP);
        stub.bump_created_etag();
        e.tick_repo(&r).await.unwrap();
        assert!(
            !e.state.repos[&r.name].ignored.contains_key(&9000),
            "given up whatever the budget was spent on"
        );
        asked.extend(looked_at(&stub.hits()));

        // The ones the earlier passes could not reach are asked about on
        // the passes that follow, rather than the same few going round.
        for _ in 0..3 {
            for i in 1..=2 * n {
                rewind_absence(&mut e, &r, i, ABSENT_RECHECK);
            }
            stub.bump_created_etag();
            e.tick_repo(&r).await.unwrap();
            asked.extend(looked_at(&stub.hits()));
        }
        let missed: Vec<u64> = (1..=2 * n).filter(|i| !asked.contains(i)).collect();
        assert!(missed.is_empty(), "never looked at: {missed:?}");
        assert_eq!(
            e.state.repos[&r.name].ignored.len(),
            2 * n as usize,
            "all still open, so all still ignored"
        );
    }

    /// A clock in the record that cannot be read — a hand-edited state
    /// file, or a machine clock that went backwards — is replaced rather
    /// than believed. `age` reads an unreadable stamp as "just now",
    /// which would freeze both the look and the giving up for ever.
    #[tokio::test]
    async fn an_unreadable_clock_in_an_ignore_record_is_replaced() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        let created = vec!["created".to_string()];
        let at = e
            .state
            .repo_mut(&r.name)
            .ignored
            .entry(18)
            .or_insert_with(|| Ignored::new(&issue(18, "bot", None), &created));
        at.absent_since = Some("not a date".into());
        at.asked_at = Some("not a date".into());
        stub.set_issue(18, untagged_listed(18, "open"));

        e.tick_repo(&r).await.unwrap();
        let at = &e.state.repos[&r.name].ignored[&18];
        assert!(
            at.absent_since.as_deref().and_then(since).is_some(),
            "the absence is dated afresh: {at:?}"
        );
        assert!(
            at.asked_at.as_deref().and_then(since).is_some(),
            "and the look that was due happened: {at:?}"
        );
    }

    /// A record that names two listings is not re-onboarded when one of
    /// them comes back short: the item is still on the other, so the
    /// prune never sees it, and only a listing it has *joined* counts as
    /// a change (issue #138, which the trigger-set comparison would
    /// otherwise reintroduce for every gate refusal an item collects two
    /// triggers from).
    #[tokio::test]
    async fn an_ignored_item_that_leaves_one_of_its_listings_is_left_alone() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        let item = json!({
            "number": 5, "title": "t", "body": "@bot look at this",
            "html_url": "https://gh/5", "state": "open", "user": {"login": "stranger"},
            "assignees": [{"login": "bot"}], "created_at": "x", "updated_at": "u1"
        });
        e.state.repo_mut(&r.name).ignored.insert(
            5,
            Ignored::new(
                &serde_json::from_value(item.clone()).unwrap(),
                &["assigned".to_string(), "mentioned".to_string()],
            ),
        );
        // Only the assignee listing carries it this pass: nothing is
        // fetched, and the record is left as it was.
        stub.set_assigned(vec![item]);
        e.tick_repo(&r).await.unwrap();
        assert_listings_only(&stub.hits());
        assert_eq!(
            e.state.repos[&r.name].ignored[&5].triggers,
            vec!["assigned".to_string(), "mentioned".to_string()],
        );
        assert!(e.failures.is_empty(), "{:?}", e.failures);
    }

    /// The prune asks whether the item is still there, not whose it is, so
    /// a record made by the gate (`refuse`) survives a short listing too:
    /// an item a stranger mentioned the bot on is neither assigned to the
    /// bot nor opened by it, and used to lose its record on the first
    /// listing that came back without it.
    #[tokio::test]
    async fn a_refused_item_keeps_its_record_when_its_listing_comes_back_short() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        let mentioned = vec!["mentioned".to_string()];
        let item = json!({
            "number": 42, "title": "t", "body": "@bot look at this",
            "html_url": "https://gh/42", "state": "open",
            "user": {"login": "stranger"}, "created_at": "x", "updated_at": "x"
        });
        e.state.repo_mut(&r.name).ignored.insert(
            42,
            Ignored::new(&serde_json::from_value(item.clone()).unwrap(), &mentioned),
        );
        stub.set_issue(42, item);
        // On no listing this pass: still open, so the record stands.
        e.tick_repo(&r).await.unwrap();
        assert_eq!(ignored_numbers(&e, &r), vec![42]);
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

    /// A workspace closed by hand leaves its checkout on disk. Purge used
    /// to call that "already gone" and forget the record, leaving the
    /// directory, and whatever only it held, for nobody to find.
    #[tokio::test]
    async fn purge_judges_a_checkout_whose_workspace_is_gone_by_the_checkout() {
        use crate::release::testkit::{scratch, sh};
        let s = scratch("purge-stray").await;
        let (path, _) = crate::driver::add_local_worktree(&s.work, "issue-1-x", None)
            .await
            .unwrap();
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Herdr);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.driver = Some(DriverKind::Herdr);
        e.cfg.repos = vec![repo()];
        let r = repo();
        for (n, p) in [(1, path.clone()), (2, "/nonexistent/w/2".to_string())] {
            seeded(&mut e, n, Some(&format!("bot/issue-{n}-x")), false);
            let st = e.entry(&r, n);
            // Neither workspace is one the driver knows.
            st.worktree_id = Some(format!("w{n}@{p}"));
            st.worktree_path = Some(p);
            st.github_state = Some("closed".into());
            st.retired_at = Some("2026-01-01T00:00:00Z".into());
        }
        let rows = |v: Value| {
            v["workspaces"]
                .as_array()
                .cloned()
                .unwrap()
                .into_iter()
                .map(|r| {
                    (
                        r["session"].as_str().unwrap().to_string(),
                        r["state"].as_str().unwrap().to_string(),
                        r["workspace"].as_str().map(str::to_string),
                        r["removed"].as_bool().unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        };
        // Dry run: the checkout on disk is judged (its branch was never
        // pushed), the one that is not is already gone.
        let v = e.purge(true, None, false).await.unwrap();
        assert_eq!(
            rows(v),
            vec![
                (
                    "o/r#1".to_string(),
                    "unpushed commits".to_string(),
                    Some("gone".to_string()),
                    false
                ),
                ("o/r#2".to_string(), "already gone".to_string(), None, false),
            ]
        );
        assert!(Path::new(&path).is_dir());
        // For real: the unpushed one is kept, the other forgotten.
        let v = e.purge(false, None, false).await.unwrap();
        assert!(!rows(v)[0].3);
        assert!(Path::new(&path).is_dir());
        assert!(e.peek(&r, 1).unwrap().worktree_id.is_some());
        assert!(e.peek(&r, 2).unwrap().worktree_id.is_none());
        // Pushed: clean and pushed, so it goes, with git since no driver
        // has it.
        sh(&path, &["push", "-q", "-u", "origin", "bot/issue-1-x"]).await;
        let v = e.purge(false, None, false).await.unwrap();
        assert_eq!(
            rows(v),
            vec![(
                "o/r#1".to_string(),
                "clean and pushed".to_string(),
                Some("gone".to_string()),
                true
            )]
        );
        assert!(!Path::new(&path).exists());
        assert!(e.peek(&r, 1).unwrap().worktree_id.is_none());
        assert!(
            d.log().iter().all(|l| !l.starts_with("remove:")),
            "{:?}",
            d.log()
        );
    }

    #[tokio::test]
    async fn release_is_refused_for_unknown_active_and_non_forced_dependent_sessions() {
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
        // An owner with an open item bound to it keeps its workspace until
        // someone explicitly overrides that protection.
        seeded(&mut e, 3, Some("b3"), false);
        e.entry(&r, 3).worktree_id = Some("repo::/w/3".into());
        seeded(&mut e, 4, Some("b3"), true);
        e.entry(&r, 4).shares_workspace_of = Some(3);
        let resp = e
            .handle_request(Request::Release {
                session: "o/r#4".into(),
                force: false,
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("#4"));
        assert!(!e.entry(&r, 3).release_pending);
        // --force is still never allowed to remove a live owner's workspace.
        seeded(&mut e, 6, Some("b6"), true);
        e.entry(&r, 6).triggers = vec!["assigned".into()];
        e.entry(&r, 6).worktree_id = Some("repo::/w/6".into());
        let resp = e
            .handle_request(Request::Release {
                session: "o/r#6".into(),
                force: true,
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("still open and assigned"));
        assert!(!e.entry(&r, 6).release_pending);
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

    #[tokio::test]
    async fn forced_release_removes_a_workspace_with_open_bound_items() {
        use crate::release::testkit::scratch;

        let clean = scratch("force-bound-release").await;
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        seeded(&mut e, 3, Some("bot/issue-3"), false);
        {
            let st = e.entry(&r, 3);
            st.title = "Original work".into();
            st.html_url = "https://gh/3".into();
            st.worktree_id = Some("w3".into());
            st.worktree_path = Some(clean.work.clone());
        }
        d.seed("w3", "t3", READY_SCREEN);
        seeded(&mut e, 4, Some("bot/issue-3"), true);
        {
            let st = e.entry(&r, 4);
            st.shares_workspace_of = Some(3);
            st.worktree_id = Some("w3".into());
            st.worktree_path = Some(clean.work.clone());
            st.terminal_handle = Some("t3".into());
        }

        // The ordinary request preserves the open follow-up's workspace.
        let refused = e
            .handle_request(Request::Release {
                session: "o/r#4".into(),
                force: false,
            })
            .await;
        assert!(!refused.ok);
        assert!(refused.error.unwrap().contains("#4"));

        // A person who uses --force may release the owning session through
        // the bound item's identity. The response and the daemon pass both
        // record it as forced.
        let accepted = e
            .handle_request(Request::Release {
                session: "o/r#4".into(),
                force: true,
            })
            .await;
        assert!(accepted.ok, "{:?}", accepted.error);
        assert_eq!(accepted.data["session"], "o/r#3");
        assert_eq!(accepted.data["forced"], true);
        assert_eq!(accepted.data["pending"], true);
        assert_eq!(accepted.data["check"]["safe"], true);
        assert!(e.entry(&r, 3).release_pending);
        assert!(e.entry(&r, 3).release_forced);

        e.run_cleanups(&r).await;
        assert_eq!(d.log(), vec!["remove:w3"]);
        for n in [3, 4] {
            let st = e.entry(&r, n).clone();
            assert!(st.worktree_id.is_none(), "#{n}");
            assert!(st.worktree_path.is_none(), "#{n}");
            assert!(st.terminal_handle.is_none(), "#{n}");
            assert!(st.released_at.is_some(), "#{n}");
        }
        assert!(e.entry(&r, 4).active, "the follow-up stays open");
        assert_eq!(e.entry(&r, 4).shares_workspace_of, Some(3));

        // An unchanged poll of the still-open follow-up does not recreate
        // the workspace. It will be rehydrated only when activity needs
        // delivery to the session.
        e.entry(&r, 4).updated_at = Some("x".into());
        *stub.created.lock().unwrap() = vec![json!({
            "number": 4, "title": "Follow-up", "body": "work",
            "html_url": "https://gh/4", "state": "open",
            "user": {"login": "bot"}, "created_at": "x", "updated_at": "x"
        })];
        e.tick_repo(&r).await.unwrap();
        assert!(e.entry(&r, 3).worktree_id.is_none());
        assert!(e.entry(&r, 4).worktree_id.is_none());
        assert!(d.log().is_empty(), "the unchanged poll did not deliver");

        // A later delivery to the follow-up re-creates its owner's
        // workspace and mirrors the new binding back onto the follow-up.
        stub.set_issue(
            3,
            json!({
                "number": 3, "title": "Original work", "body": "work",
                "html_url": "https://gh/3", "state": "closed",
                "user": {"login": "alice"}, "created_at": "x", "updated_at": "x"
            }),
        );
        stub.set_timeline(3, vec![]);
        e.deliver_to(&r, 4, "later activity", None).await.unwrap();
        let owner = e.entry(&r, 3).clone();
        let dependent = e.entry(&r, 4).clone();
        assert!(owner.worktree_id.is_some());
        assert_eq!(dependent.worktree_id, owner.worktree_id);
        assert_eq!(dependent.worktree_path, owner.worktree_path);
        assert!(owner.released_at.is_none());
        assert!(dependent.released_at.is_none());
        assert_eq!(dependent.shares_workspace_of, Some(3));
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
        e.remember_worktree(&r, 1, &wt);
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
        e.remember_worktree(&r, 1, &wt);
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
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        let r = repo();
        e.cfg.repos.push(r.clone());
        // Open and assigned: nothing to release, not even by force.
        seeded(&mut e, 1, Some("b1"), true);
        e.entry(&r, 1).triggers = vec!["assigned".into()];
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
        d.seed("repo::/w/1", "h", READY_SCREEN);
        e.run_cleanups(&r).await;
        let st = e.entry(&r, 1).clone();
        assert!(!st.release_pending);
        assert!(!st.release_forced);
        assert_eq!(st.worktree_id.as_deref(), Some("repo::/w/1"));
        assert_eq!(st.worktree_path.as_deref(), Some("/w/1"));
        assert_eq!(st.terminal_handle.as_deref(), Some("h"));
        assert!(st.released_at.is_none());
        // A plain release is also dropped if a bound item becomes active
        // between request and the daemon's pass.
        seeded(&mut e, 2, Some("b2"), false);
        {
            let st = e.entry(&r, 2);
            st.worktree_id = Some("w2".into());
            st.worktree_path = Some("/nonexistent/ssf-w2".into());
            st.release_pending = true;
        }
        d.seed("w2", "t2", READY_SCREEN);
        seeded(&mut e, 3, Some("b2"), true);
        e.entry(&r, 3).shares_workspace_of = Some(2);
        e.run_cleanups(&r).await;
        assert!(!e.entry(&r, 2).release_pending);
        assert!(!e.entry(&r, 2).release_forced);
        assert_eq!(e.entry(&r, 2).worktree_id.as_deref(), Some("w2"));
        e.entry(&r, 3).active = false;
        e.entry(&r, 2).release_pending = true;
        e.run_cleanups(&r).await; // plain: re-checked, and the path is gone
        let st = e.entry(&r, 2).clone();
        assert!(!st.release_pending);
        assert_eq!(st.release_refusals, 1);
        assert!(st.worktree_id.is_some());
    }

    // ---- a driver switch --------------------------------------------------

    /// Replays issue #105: after `driver` went from Orca to herdr, an item
    /// from before the switch still carried Orca's repo id, which the herdr
    /// driver took for a checkout path. Each delivery failed on it, and the
    /// item recovered only once five failures had it re-onboarded. Now the
    /// first delivery re-creates the workspace on the current driver.
    #[tokio::test]
    async fn a_workspace_made_by_the_old_driver_is_re_created_on_the_new_one() {
        const ORCA_REPO: &str = "1b790ad2-4421-43dc-9f46-f7c09d0c321f";
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Herdr);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.driver = Some(DriverKind::Herdr);
        e.cfg.repos = vec![repo()];
        stub.set_timeline(5, vec![assigned_by(1, "alice")]);
        seeded(&mut e, 5, Some("bot/issue-5-fix-the-widget"), true);
        {
            let st = e.entry(&repo(), 5);
            st.title = "Fix the widget".into();
            st.html_url = "https://gh/5".into();
            st.worktree_name = Some("issue-5-fix-the-widget".into());
            // As a state file from before the driver was written down has it.
            st.driver = None;
            st.repo_id = Some(ORCA_REPO.into());
            st.worktree_id = Some(format!(
                "{ORCA_REPO}::/home/me/orca/projects/r.worktrees/issue-5-fix-the-widget"
            ));
            st.worktree_path =
                Some("/home/me/orca/projects/r.worktrees/issue-5-fix-the-widget".into());
            st.terminal_handle = Some("orca-terminal".into());
        }
        let delivered = e.deliver_to(&repo(), 5, "hello", None).await.unwrap();
        assert!(delivered.relaunched);
        let st = e.entry(&repo(), 5).clone();
        // The binding went through the current driver's project setup.
        assert_eq!(st.repo_id.as_deref(), Some("stub"));
        assert_eq!(st.driver.as_deref(), Some("herdr"));
        assert_eq!(
            st.worktree_id.as_deref(),
            Some("stub::/stub.worktrees/issue-5-fix-the-widget"),
            "re-created under its old name"
        );
        assert_eq!(st.terminal_handle.as_deref(), Some("t1"));
        assert!(st.seeded, "not re-onboarded");
        assert!(e.failures.is_empty(), "no failure counted");
        let log = d.log();
        assert_eq!(
            log[0],
            "relaunch:stub::/stub.worktrees/issue-5-fix-the-widget:false"
        );
        assert!(
            log[1].starts_with("deliver:stub::/stub.worktrees/issue-5-fix-the-widget:"),
            "{log:?}"
        );
        // The item is told the agent was attached again, and why.
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#5 event=attached -->\n\n\
             ```ssf\n\
             ssf attaching agent to issue again:\n\
             harness: Claude Code\n\
             model: the harness's default\n\
             effort: the harness's default\n\
             driver: herdr\n\
             branch: bot/issue-5-fix-the-widget\n\
             re-created: driver switch\n\
             conversation: fresh\n\
             ```"
        );
        // The record now says herdr: the next delivery finds the workspace
        // as it is and nothing is re-created, so nothing is posted.
        e.deliver_to(&repo(), 5, "again", None).await.unwrap();
        assert_eq!(
            d.log(),
            vec!["deliver:stub::/stub.worktrees/issue-5-fix-the-widget:again"]
        );
        assert!(stub.posts().is_empty());
        assert_eq!(
            e.entry(&repo(), 5).worktree_id.as_deref(),
            Some("stub::/stub.worktrees/issue-5-fix-the-widget")
        );

        // And the other way round: a record that says herdr, with the
        // checkout path as its repo id, once the repository runs in Orca.
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.driver = Some(DriverKind::Orca);
        {
            let st = e.entry(&repo(), 5);
            st.driver = Some("herdr".into());
            st.repo_id = Some("/home/me/ssf/projects/r".into());
            st.worktree_id =
                Some("w7@/home/me/ssf/projects/r.worktrees/issue-5-fix-the-widget".into());
            st.worktree_path =
                Some("/home/me/ssf/projects/r.worktrees/issue-5-fix-the-widget".into());
        }
        e.deliver_to(&repo(), 5, "hello", None).await.unwrap();
        let st = e.entry(&repo(), 5).clone();
        assert_eq!(st.repo_id.as_deref(), Some("stub"));
        assert_eq!(st.driver.as_deref(), Some("orca"));
        assert_eq!(
            st.worktree_id.as_deref(),
            Some("stub::/stub.worktrees/issue-5-fix-the-widget")
        );
        assert!(e.failures.is_empty());
        assert_eq!(
            d.log()[0],
            "relaunch:stub::/stub.worktrees/issue-5-fix-the-widget:false"
        );
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert!(
            posts[0].1.contains("driver: orca\n")
                && posts[0].1.contains("re-created: driver switch\n"),
            "{}",
            posts[0].1
        );
    }

    /// A record from before the driver was written down whose repo id fits
    /// the current driver is left alone: no re-creation for its own sake.
    #[tokio::test]
    async fn a_binding_that_fits_the_current_driver_is_kept() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Herdr);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.driver = Some(DriverKind::Herdr);
        e.cfg.repos = vec![repo()];
        seeded(&mut e, 5, None, true);
        {
            let st = e.entry(&repo(), 5);
            st.driver = None;
            st.repo_id = Some("stub".into());
            st.worktree_id = Some("w5".into());
            st.worktree_path = Some("/w/5".into());
        }
        d.seed("w5", "t5", READY_SCREEN);
        e.deliver_to(&repo(), 5, "hello", None).await.unwrap();
        let st = e.entry(&repo(), 5).clone();
        assert_eq!(st.worktree_id.as_deref(), Some("w5"));
        assert_eq!(st.repo_id.as_deref(), Some("stub"));
        assert_eq!(d.log(), vec!["deliver:w5:hello"]);
    }
    // ---- sessions blocked on a login ------------------------------------

    const LOGIN_SCREEN: &[&str] = &[
        "❯ [ssf] New activity on #5:",
        "",
        "  Login expired · Please run /login",
        "",
        "❯ ",
        "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
    ];
    const READY_SCREEN: &[&str] = &["⏺ Done.", "", "❯ ", "  ⏵⏵ bypass permissions on"];
    /// Pi's sign-in prompt, which reads nothing like Claude Code's.
    const PI_LOGIN_SCREEN: &[&str] = &["  Use /login to log into a provider", "❯ "];

    /// An engine on the stub driver with item 5 seeded on workspace `w5`,
    /// its agent live in terminal `t5` showing `screen`.
    fn blocked_setup(stub: &GitHubStub, screen: &[&str]) -> (Engine, crate::driver::StubDriver) {
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.repos = vec![repo()];
        seeded(&mut e, 5, Some("bot/issue-5"), true);
        let st = e.entry(&repo(), 5);
        st.title = "Fix the widget".into();
        st.html_url = "https://gh/5".into();
        st.worktree_id = Some("w5".into());
        st.worktree_path = Some("/w/5".into());
        st.terminal_handle = Some("t5".into());
        st.agent_session_id = Some("sess-5".into());
        st.updated_at = Some("u1".into());
        d.seed("w5", "t5", screen);
        (e, d)
    }

    /// A new answer from the login check; `tick` clears the per-pass cache
    /// but these tests drive `tick_repo` directly.
    fn probe_returning(e: &mut Engine, state: LoginState, fingerprint: Option<&str>) {
        e.probes.clear();
        let fp = fingerprint.map(str::to_string);
        e.probe = std::sync::Arc::new(move |_| Probe {
            state,
            detail: "test".into(),
            fingerprint: fp.clone(),
        });
    }

    #[tokio::test]
    async fn a_session_at_a_login_prompt_is_blocked_told_and_held() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = blocked_setup(&stub, LOGIN_SCREEN);
        probe_returning(&mut e, LoginState::SignedOut, Some("cred-old"));
        // New activity on the item this pass.
        stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
        stub.set_timeline(
            5,
            vec![assigned_by(1, "alice"), comment(2, "alice", "please hurry")],
        );
        e.tick_repo(&repo()).await.unwrap();
        let st = e.entry(&repo(), 5).clone();
        let b = st.blocked.clone().expect("blocked");
        assert_eq!(b.reason, "login");
        assert_eq!(b.harness, "claude");
        assert_eq!(b.detail, "Login expired · Please run /login");
        assert!(b.reported);
        assert_eq!(b.credential.as_deref(), Some("cred-old"));
        // The item was told once, by the daemon (not as the session).
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(posts[0].0, "/repos/o/r/issues/5/comments");
        assert_eq!(
            posts[0].1,
            format!(
                "🤖 ssf <!-- ssf: origin=o/r#5 event=blocked -->\n\n\
                 ```ssf\n\
                 ssf holding deliveries to agent on issue:\n\
                 harness: Claude Code\n\
                 reason: not signed in\n\
                 fix: {}\n\
                 ```",
                login::how_to_sign_in("claude").replace('`', "")
            )
        );
        assert!(posts[0].1.contains("fix: claude auth login on the host"));
        // Nothing was pasted, and the activity is still owed: `updated_at`
        // did not move, the comment is not marked seen, no failure counted.
        assert!(d.log().is_empty(), "no delivery into a blocked session");
        assert_eq!(st.updated_at.as_deref(), Some("u1"));
        assert!(!st.seen.contains_key("comment:2"), "{:?}", st.seen.keys());
        assert!(e.failures.is_empty());
        // A second pass: still blocked, still one comment, still nothing pasted.
        e.tick_repo(&repo()).await.unwrap();
        assert!(stub.posts().is_empty());
        assert!(d.log().is_empty());
        assert!(e.entry(&repo(), 5).blocked.is_some());
        // Direct deliveries (a tell, a subscriber's FYI) are refused with
        // the reason, not silently lost.
        let err = e.deliver_to(&repo(), 5, "hello", None).await.unwrap_err();
        assert!(is_blocked(&err), "{err:#}");
        assert!(
            err.to_string()
                .contains("Claude Code has been at its sign-in prompt"),
            "{err:#}"
        );
        assert!(err.to_string().contains("claude auth login"), "{err:#}");
        let err = e.tell(None, "o/r#5", "hello").await.unwrap_err();
        assert!(is_blocked(&err), "{err:#}");
        assert!(d.log().is_empty());
    }

    #[tokio::test]
    async fn a_blocked_session_is_started_again_once_the_login_is_back() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = blocked_setup(&stub, LOGIN_SCREEN);
        let since = (chrono::Utc::now() - chrono::Duration::minutes(12))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        e.entry(&repo(), 5).blocked = Some(Blocked {
            reason: "login".into(),
            harness: "claude".into(),
            detail: "Login expired · Please run /login".into(),
            since: since.clone(),
            reported: true,
            credential: Some("cred-old".into()),
            retried_at: None,
            retries: 0,
            told_at: None,
            tell_failures: 0,
        });
        e.state.repo_mut("o/r").issues_etag = Some("etag".into());
        stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
        stub.set_timeline(5, vec![assigned_by(1, "alice")]);
        // Signed out: nothing happens.
        probe_returning(&mut e, LoginState::SignedOut, Some("cred-old"));
        e.tick_repo(&repo()).await.unwrap();
        assert!(e.entry(&repo(), 5).blocked.is_some());
        assert!(d.log().is_empty());
        assert!(stub.posts().is_empty());
        // Signed in, same credential, block younger than the retry
        // interval as far as the record says? It is 12 minutes old, so
        // the retry is due; make it fresh first to see it held back.
        e.entry(&repo(), 5).blocked.as_mut().unwrap().retried_at = Some(now_iso());
        probe_returning(&mut e, LoginState::SignedIn, Some("cred-old"));
        e.tick_repo(&repo()).await.unwrap();
        assert!(e.entry(&repo(), 5).blocked.is_some());
        assert!(d.log().is_empty(), "not due yet");
        // A new credential file: the harness is quit and started again
        // with its conversation resumed, given the login-back message,
        // the listings are fetched afresh and the item is told.
        probe_returning(&mut e, LoginState::SignedIn, Some("cred-new"));
        d.with(|s| s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect());
        let fulls_before = stub.created_fulls();
        e.tick_repo(&repo()).await.unwrap();
        let st = e.entry(&repo(), 5).clone();
        assert!(st.blocked.is_none(), "{:?}", st.blocked);
        let log = d.log();
        assert_eq!(log[0], "stop:t5");
        assert_eq!(log[1], "relaunch:w5:true");
        assert!(
            log[2].starts_with("deliver:w5:[ssf] Your Claude Code sign-in lapsed at"),
            "{log:?}"
        );
        assert_eq!(st.terminal_handle.as_deref(), Some("t1"));
        assert_eq!(st.prompts_sent, 1);
        // One `unblocked` post carries the conversation line; the restart
        // that lifted the block is not a `resumed` event of its own.
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#5 event=unblocked -->\n\n\
             ```ssf\n\
             ssf resuming deliveries to agent on issue:\n\
             harness: Claude Code\n\
             held for: 12 min\n\
             conversation: resumed\n\
             ```"
        );
        // ETags were dropped by the recovery, then set again by the pass.
        assert!(e.state.repos["o/r"].issues_etag.is_some());
        // The recovery ran before the pass read its listings, so the full
        // fetch it is owed is this pass, and the `refetch` flag it armed is
        // spent unused at the start of it: the passes after this one are
        // conditional again (issue #141).
        assert_eq!(stub.created_fulls(), fulls_before + 1, "one full fetch");
        assert!(e.refetch.is_empty());
        for _ in 0..2 {
            e.tick_repo(&repo()).await.unwrap();
        }
        assert_eq!(
            stub.created_fulls(),
            fulls_before + 1,
            "and no more after it"
        );
        // A failing item would clear all four ETags for its own reasons and
        // make the count above misleading; a quiet pass also posts nothing.
        assert!(e.failures.is_empty(), "{:?}", e.failures);
        assert!(stub.post_bodies().is_empty());
    }

    #[tokio::test]
    async fn a_harness_started_again_onto_the_login_prompt_stays_blocked_quietly() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = blocked_setup(&stub, LOGIN_SCREEN);
        let since = (chrono::Utc::now() - chrono::Duration::minutes(30))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        e.entry(&repo(), 5).blocked = Some(Blocked {
            reason: "login".into(),
            harness: "claude".into(),
            detail: "Login expired · Please run /login".into(),
            since: since.clone(),
            reported: true,
            credential: Some("cred-old".into()),
            retried_at: None,
            retries: 0,
            told_at: None,
            tell_failures: 0,
        });
        stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
        stub.set_timeline(5, vec![assigned_by(1, "alice")]);
        // The check claims signed in but the harness comes back to the
        // same prompt: the record keeps its start, no second comment, and
        // the next attempt waits for the retry interval.
        probe_returning(&mut e, LoginState::Unknown, None);
        d.with(|s| s.relaunch_screen = LOGIN_SCREEN.iter().map(|l| l.to_string()).collect());
        e.tick_repo(&repo()).await.unwrap();
        let st = e.entry(&repo(), 5).clone();
        let b = st.blocked.expect("still blocked");
        assert_eq!(b.since, since);
        assert!(b.reported);
        assert!(b.retried_at.is_some());
        assert_eq!(b.retries, 1, "the next attempt waits twice as long");
        assert_eq!(retry_wait(0).as_secs(), 600);
        assert_eq!(retry_wait(1).as_secs(), 1200);
        assert_eq!(retry_wait(3).as_secs(), 3600);
        assert_eq!(retry_wait(30).as_secs(), 3600);
        let log = d.log();
        assert_eq!(log[0], "stop:t5");
        assert_eq!(log[1], "relaunch:w5:true");
        assert!(stub.posts().is_empty(), "the item was told already");
        // And not again on the very next pass.
        e.tick_repo(&repo()).await.unwrap();
        assert!(d.log().is_empty());
    }

    #[tokio::test]
    async fn a_person_signing_in_at_the_terminal_lifts_the_block_without_a_restart() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
        e.entry(&repo(), 5).blocked = Some(Blocked {
            reason: "login".into(),
            harness: "claude".into(),
            detail: "Login expired · Please run /login".into(),
            since: now_iso(),
            reported: true,
            credential: None,
            retried_at: None,
            retries: 0,
            told_at: None,
            tell_failures: 0,
        });
        e.state.repo_mut("o/r").issues_etag = Some("etag".into());
        stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
        stub.set_timeline(
            5,
            vec![assigned_by(1, "alice"), comment(2, "alice", "go on")],
        );
        probe_returning(&mut e, LoginState::Unknown, None);
        e.tick_repo(&repo()).await.unwrap();
        let st = e.entry(&repo(), 5).clone();
        assert!(st.blocked.is_none());
        // No restart, and the held activity went in on the same pass.
        let log = d.log();
        assert_eq!(log.len(), 1, "{log:?}");
        assert!(
            log[0].starts_with("deliver:w5:[ssf] New activity"),
            "{log:?}"
        );
        assert_eq!(st.updated_at.as_deref(), Some("u2"));
        // The item had been told of the block, so it hears the hold is
        // over, however quick, and that nothing was started again.
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#5 event=unblocked -->\n\n\
             ```ssf\n\
             ssf resuming deliveries to agent on issue:\n\
             harness: Claude Code\n\
             held for: less than a minute\n\
             conversation: kept\n\
             ```"
        );
    }

    #[tokio::test]
    async fn a_relaunch_that_lands_on_a_login_prompt_blocks_the_session() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
        // The agent is gone (a reboot); the machine is not signed in.
        d.with(|s| {
            s.live.clear();
            s.relaunch_screen = LOGIN_SCREEN.iter().map(|l| l.to_string()).collect();
        });
        probe_returning(&mut e, LoginState::SignedOut, None);
        let err = e
            .deliver_to(&repo(), 5, "[ssf] hello", None)
            .await
            .unwrap_err();
        assert!(is_blocked(&err), "{err:#}");
        let st = e.entry(&repo(), 5).clone();
        let b = st.blocked.clone().expect("blocked");
        assert!(!b.reported, "the comment is left to the pass");
        assert_eq!(st.terminal_handle.as_deref(), Some("t1"));
        let log = d.log();
        assert_eq!(log[0], "relaunch:w5:true");
        // The next pass reports it, once.
        stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
        stub.set_timeline(5, vec![assigned_by(1, "alice")]);
        e.tick_repo(&repo()).await.unwrap();
        assert!(e.entry(&repo(), 5).blocked.as_ref().unwrap().reported);
        assert_eq!(stub.posts(), vec!["/repos/o/r/issues/5/comments"]);
        e.tick_repo(&repo()).await.unwrap();
        assert!(stub.posts().is_empty());
        // A working agent is never read for a login prompt.
        let (mut e, d) = blocked_setup(&stub, LOGIN_SCREEN);
        d.with(|s| {
            s.working.insert("w5".into());
        });
        stub.set_timeline(5, vec![assigned_by(1, "alice")]);
        e.tick_repo(&repo()).await.unwrap();
        assert!(e.entry(&repo(), 5).blocked.is_none());
    }

    #[tokio::test]
    async fn a_blocked_harness_that_is_gone_is_judged_by_its_restart() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = blocked_setup(&stub, LOGIN_SCREEN);
        let since = (chrono::Utc::now() - chrono::Duration::minutes(30))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let record = Blocked {
            reason: "login".into(),
            harness: "claude".into(),
            detail: "Login expired · Please run /login".into(),
            since: since.clone(),
            reported: true,
            credential: Some("cred-old".into()),
            retried_at: None,
            retries: 0,
            told_at: None,
            tell_failures: 0,
        };
        e.entry(&repo(), 5).blocked = Some(record.clone());
        // The terminal vanished (a reboot, a closed terminal).
        d.with(|s| {
            s.live.clear();
            s.relaunch_screen = LOGIN_SCREEN.iter().map(|l| l.to_string()).collect();
        });
        stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
        stub.set_timeline(5, vec![assigned_by(1, "alice")]);
        // Still signed out: nothing is started, nothing is said.
        probe_returning(&mut e, LoginState::SignedOut, Some("cred-old"));
        e.tick_repo(&repo()).await.unwrap();
        assert_eq!(e.entry(&repo(), 5).blocked, Some(record.clone()));
        assert!(d.log().is_empty());
        assert!(stub.posts().is_empty());
        // The check says signed in but the restart lands on the prompt:
        // the record stays (no comment either way), the attempt is noted.
        probe_returning(&mut e, LoginState::SignedIn, Some("cred-new"));
        e.tick_repo(&repo()).await.unwrap();
        let b = e.entry(&repo(), 5).blocked.clone().expect("still blocked");
        assert_eq!(b.since, since);
        assert!(b.reported);
        assert_eq!(b.retries, 1);
        assert_eq!(b.credential.as_deref(), Some("cred-new"));
        let log = d.log();
        assert_eq!(log[0], "relaunch:w5:true", "{log:?}");
        assert!(stub.posts().is_empty());
        // A restart that comes up working lifts the block, with the one
        // "signed in again" comment, and the held activity follows.
        e.entry(&repo(), 5).blocked.as_mut().unwrap().retried_at = Some(since.clone());
        d.with(|s| {
            s.live.clear();
            s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect();
        });
        stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
        stub.set_timeline(
            5,
            vec![assigned_by(1, "alice"), comment(2, "alice", "go on")],
        );
        e.tick_repo(&repo()).await.unwrap();
        let st = e.entry(&repo(), 5).clone();
        assert!(st.blocked.is_none(), "{:?}", st.blocked);
        let log = d.log();
        assert_eq!(log[0], "relaunch:w5:true", "{log:?}");
        assert!(
            log[1].starts_with("deliver:w5:[ssf] Your Claude Code sign-in lapsed"),
            "{log:?}"
        );
        assert!(
            log.iter()
                .any(|l| l.starts_with("deliver:w5:[ssf] New activity")),
            "{log:?}"
        );
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert!(
            posts[0].1.contains("event=unblocked -->")
                && posts[0]
                    .1
                    .contains("held for: 30 min\nconversation: resumed\n"),
            "{}",
            posts[0].1
        );
        assert_eq!(st.updated_at.as_deref(), Some("u2"));
        // Gone again while still blocked, activity arrives through the
        // normal path and the restart is fine: lifted the same way, once.
        e.entry(&repo(), 5).blocked = Some(record.clone());
        d.with(|s| s.live.clear());
        stub.set_assigned(vec![assigned_item(5, "alice", "u3")]);
        stub.set_timeline(
            5,
            vec![assigned_by(1, "alice"), comment(3, "alice", "and again")],
        );
        probe_returning(&mut e, LoginState::SignedOut, Some("cred-new"));
        e.tick_repo(&repo()).await.unwrap();
        assert!(e.entry(&repo(), 5).blocked.is_none());
        let log = d.log();
        assert_eq!(log[0], "relaunch:w5:true", "{log:?}");
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert!(posts[0].1.contains("event=unblocked -->"), "{}", posts[0].1);
    }

    /// The `attached` post on onboarding: one per start, with the launch
    /// as configured, and never again for a pass or a daemon restart that
    /// finds the item as it was.
    #[tokio::test]
    async fn onboarding_posts_one_attached_event_and_no_more_after_that() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        let mut r = repo();
        r.model = Some("fable-5.1".into());
        r.effort = Some("high".into());
        e.cfg.repos = vec![r.clone()];
        stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
        stub.set_timeline(5, vec![assigned_by(1, "alice")]);
        e.tick_repo(&r).await.unwrap();
        let st = e.entry(&r, 5).clone();
        assert!(st.seeded && st.active, "{st:?}");
        assert!(e.failures.is_empty(), "{:?}", e.failures);
        let log = d.log();
        assert!(
            log[0].starts_with("start:stub::/stub.worktrees/issue-5-t:"),
            "{log:?}"
        );
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(posts[0].0, "/repos/o/r/issues/5/comments");
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#5 event=attached -->\n\n\
             ```ssf\n\
             ssf attaching agent to issue:\n\
             harness: Claude Code\n\
             model: fable-5.1\n\
             effort: high\n\
             driver: orca\n\
             branch: bot/issue-5-t\n\
             ```"
        );
        // The post is on the item's timeline now; the next pass neither
        // delivers it to the agent nor posts again.
        let event_post = comment(2, "bot", &posts[0].1);
        stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
        stub.set_timeline(5, vec![assigned_by(1, "alice"), event_post.clone()]);
        e.tick_repo(&r).await.unwrap();
        assert!(d.log().is_empty(), "nothing to deliver");
        assert!(stub.posts().is_empty());
        let st = e.entry(&r, 5).clone();
        assert_eq!(st.updated_at.as_deref(), Some("u2"));
        assert!(st.seen.contains_key("commented:2"), "{:?}", st.seen.keys());
        assert!(
            st.untagged.is_empty(),
            "not a person's post: {:?}",
            st.untagged
        );
        assert!(
            st.origins.is_empty(),
            "not a session's post: {:?}",
            st.origins
        );
        assert_eq!(
            crate::status::sessions(&e.cfg, &e.state, None)[0].untagged_posts,
            0
        );
        // A daemon restart: the state file survives, memory does not, and
        // an unchanged item is not attached again.
        let dir = std::env::temp_dir().join(format!("ssf-engine-events-{}", std::process::id()));
        let path = dir.join("state.json");
        e.state.save_to(&path).unwrap();
        let mut e = engine_at(&stub.base);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.repos = vec![r.clone()];
        e.state = State::load_from(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        e.tick_repo(&r).await.unwrap();
        assert!(d.log().is_empty(), "{:?}", d.log());
        assert!(stub.posts().is_empty());
    }

    /// An item bound to another item's session hears which one took it.
    #[tokio::test]
    async fn binding_to_an_owning_session_posts_attached_on_the_bound_item() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        // Session 1 is live on w1; item 7 was opened by it.
        seeded(&mut e, 1, Some("bot/issue-1"), true);
        {
            let st = e.entry(&r, 1);
            st.worktree_id = Some("w1".into());
            st.worktree_path = Some("/w/1".into());
            st.terminal_handle = Some("t1".into());
            st.repo_id = Some("stub".into());
            st.driver = Some("orca".into());
        }
        d.seed("w1", "t1", READY_SCREEN);
        let opened = json!({
            "number": 7, "title": "child", "body": "🤖#1 says: <!-- ssf: origin=o/r#1 -->\n\nfollow-up",
            "html_url": "https://gh/7", "state": "open", "user": {"login": "bot"},
            "assignees": [{"login": "bot"}], "created_at": "x", "updated_at": "u1"
        });
        stub.set_assigned(vec![opened]);
        stub.set_timeline(7, vec![assigned_by(1, "bot")]);
        e.tick_repo(&r).await.unwrap();
        let st = e.entry(&r, 7).clone();
        assert_eq!(st.shares_workspace_of, Some(1), "{st:?}");
        let log = d.log();
        assert!(
            log[0].starts_with("deliver:w1:[ssf] Now tracking issue #7"),
            "{log:?}"
        );
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(posts[0].0, "/repos/o/r/issues/7/comments");
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#7 event=attached -->\n\n\
             ```ssf\n\
             ssf attaching agent to issue:\n\
             session: o/r#1\n\
             shares: workspace of #1\n\
             ```"
        );
    }

    /// A daemon event post on a timeline reaches no agent: not the item's
    /// own session, not a subscriber.
    #[tokio::test]
    async fn event_posts_are_not_delivered_or_fanned_out() {
        let mut e = engine();
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        let r = repo();
        e.cfg.repos.push(r.clone());
        seeded(&mut e, 1, Some("bot/issue-1"), true);
        seeded(&mut e, 3, Some("bot/issue-3"), true);
        for n in [1, 3] {
            let st = e.entry(&r, n);
            st.worktree_id = Some(format!("w{n}"));
            st.terminal_handle = Some(format!("t{n}"));
            d.seed(&format!("w{n}"), &format!("t{n}"), READY_SCREEN);
        }
        // Session 1 follows item 3, whose only news is the daemon saying
        // it resumed session 3's harness.
        e.entry(&r, 3).subscribers = vec!["o/r#1".into()];
        let o = Origin::new("o/r", 3).unwrap();
        let post = events::comment(
            &o,
            "issue",
            &Event::Resumed {
                harness: "Claude Code".into(),
                conversation: Conversation::Fresh,
                after: "restart",
            },
        );
        let timeline = vec![comment(9, "bot", &post)];
        let diff = e.diff(&r, &BTreeMap::new(), &timeline);
        assert!(diff.rendered.is_empty(), "{:?}", diff.rendered);
        assert!(diff.seen.contains_key("commented:9"));
        assert!(
            e.for_recipient(&diff.rendered, "o/r#1").is_empty()
                && e.for_recipient(&diff.rendered, "o/r#3").is_empty()
        );
        e.fan_out(
            &r,
            &issue(3, "alice", None),
            &diff.rendered,
            Fyi::Activity,
            false,
            &[],
        )
        .await;
        assert!(d.log().is_empty(), "{:?}", d.log());
    }

    /// `event_comments` off, for the instance or the repository, posts
    /// nothing and changes nothing else.
    #[tokio::test]
    async fn event_comments_can_be_switched_off() {
        let _sandbox = crate::config::test_support::sandbox();
        // The instance says no: onboarding posts nothing, but happens.
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        e.cfg.daemon.event_comments = false;
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
        stub.set_timeline(5, vec![assigned_by(1, "alice")]);
        e.tick_repo(&r).await.unwrap();
        assert!(e.entry(&r, 5).seeded);
        assert!(d.log()[0].starts_with("start:"));
        assert!(stub.posts().is_empty());

        // The repository says yes over an instance that says no: a
        // blocked session is reported.
        let (mut e, d) = blocked_setup(&stub, LOGIN_SCREEN);
        e.cfg.daemon.event_comments = false;
        e.cfg.repos[0].event_comments = Some(true);
        probe_returning(&mut e, LoginState::SignedOut, None);
        stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
        stub.set_timeline(5, vec![assigned_by(1, "alice"), comment(2, "alice", "go")]);
        e.tick_repo(&e.cfg.repos[0].clone()).await.unwrap();
        assert!(e.entry(&repo(), 5).blocked.as_ref().unwrap().reported);
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert!(posts[0].1.contains("event=blocked -->"));
        assert!(d.log().is_empty(), "held");

        // The repository says no over an instance that says yes: still
        // blocked and held, nothing posted, and the record still says
        // reported so the pass does not try again.
        let (mut e, d) = blocked_setup(&stub, LOGIN_SCREEN);
        e.cfg.repos[0].event_comments = Some(false);
        probe_returning(&mut e, LoginState::SignedOut, None);
        e.tick_repo(&e.cfg.repos[0].clone()).await.unwrap();
        let st = e.entry(&repo(), 5).clone();
        assert!(st.blocked.as_ref().unwrap().reported);
        assert!(stub.posts().is_empty());
        assert!(d.log().is_empty(), "held");
        assert_eq!(st.updated_at.as_deref(), Some("u1"));
        let err = e.deliver_to(&repo(), 5, "hello", None).await.unwrap_err();
        assert!(is_blocked(&err), "{err:#}");
    }

    /// A workspace removed by `ssf release` or `ssf purge` is told of on
    /// the owning item, once, with who did it; one already gone is not.
    #[tokio::test]
    async fn releasing_or_purging_a_workspace_posts_released() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        let closed = |e: &mut Engine, n: u64| {
            seeded(e, n, Some(&format!("bot/issue-{n}")), false);
            let st = e.entry(&repo(), n);
            st.worktree_id = Some(format!("w{n}"));
            // No such directory: the checks cannot pass, so only a
            // forced removal goes ahead.
            st.worktree_path = Some(format!("/nonexistent/ssf-w{n}"));
            st.github_state = Some("closed".into());
            st.kind = Some("pull_request".into());
            st.retired_at = Some(now_iso());
            d.with(|s| {
                s.worktrees.insert(format!("w{n}"));
            });
        };
        closed(&mut e, 1);
        closed(&mut e, 2);
        // Item 3 shares session 1's workspace: nothing is posted on it.
        seeded(&mut e, 3, None, false);
        e.entry(&r, 3).shares_workspace_of = Some(1);
        e.entry(&r, 3).worktree_id = Some("w1".into());
        e.entry(&r, 3).github_state = Some("closed".into());
        {
            let st = e.entry(&r, 1);
            st.release_pending = true;
            st.release_forced = true;
        }
        let st = e.entry(&r, 1).clone();
        e.finish_release(&r, st).await;
        assert!(e.entry(&r, 1).worktree_id.is_none());
        assert!(e.entry(&r, 3).worktree_id.is_none());
        assert_eq!(d.log(), vec!["remove:w1"]);
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(posts[0].0, "/repos/o/r/issues/1/comments");
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#1 event=released -->\n\n\
             ```ssf\n\
             ssf releasing workspace of pull request:\n\
             by: ssf release\n\
             forced: yes\n\
             branch: bot/issue-1\n\
             ```"
        );
        // Already gone: marked released, nothing said.
        {
            let st = e.entry(&r, 1);
            st.worktree_id = Some("w1".into());
            st.release_pending = true;
        }
        let st = e.entry(&r, 1).clone();
        e.finish_release(&r, st).await;
        assert!(e.entry(&r, 1).released_at.is_some());
        assert!(stub.posts().is_empty());
        // Purge, forced since the checks cannot run.
        let out = e.purge(false, None, true).await.unwrap();
        assert_eq!(out["workspaces"][0]["removed"], true, "{out}");
        assert_eq!(d.log(), vec!["remove:w2"]);
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(posts[0].0, "/repos/o/r/issues/2/comments");
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#2 event=released -->\n\n\
             ```ssf\n\
             ssf releasing workspace of pull request:\n\
             by: ssf purge\n\
             forced: yes\n\
             branch: bot/issue-2\n\
             ```"
        );
        // A dry run removes nothing and says nothing.
        closed(&mut e, 4);
        e.purge(true, None, true).await.unwrap();
        assert!(d.log().is_empty());
        assert!(stub.posts().is_empty());
    }

    /// The fifth failure in a row drops the binding and says so once,
    /// with the error on one line.
    #[tokio::test]
    async fn giving_up_on_a_binding_posts_gave_up_once() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        seeded(&mut e, 5, Some("bot/issue-5"), true);
        e.entry(&r, 5).terminal_handle = Some("t5".into());
        let err =
            anyhow::anyhow!("no such terminal\n  (it was closed)").context("delivering to orca");
        for n in 1..MAX_DELIVERY_FAILURES {
            e.note_failure(&r, 5, &err).await;
            assert_eq!(e.failures[&("o/r".to_string(), 5)], n);
            assert!(e.entry(&r, 5).seeded);
        }
        assert!(stub.posts().is_empty());
        e.note_failure(&r, 5, &err).await;
        let st = e.entry(&r, 5).clone();
        assert!(!st.seeded && st.terminal_handle.is_none(), "{st:?}");
        assert_eq!(e.failures[&("o/r".to_string(), 5)], 0);
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#5 event=gave-up -->\n\n\
             ```ssf\n\
             ssf giving up on agent binding for issue:\n\
             failures: 5\n\
             last error: delivering to orca: no such terminal (it was closed)\n\
             next: re-onboarding the item\n\
             ```"
        );
        // Five more without an onboarding in between: the count resets
        // again, but there is no binding to drop and nothing to say.
        for _ in 0..MAX_DELIVERY_FAILURES {
            e.note_failure(&r, 5, &err).await;
        }
        assert_eq!(e.failures[&("o/r".to_string(), 5)], 0);
        assert!(!e.entry(&r, 5).seeded);
        assert!(stub.posts().is_empty(), "no binding, no post");
        // An item that never got a session (its onboarding fails every
        // look) is counted and reset the same way, and never told.
        e.entry(&r, 9).title = "never onboarded".into();
        for n in 1..=MAX_DELIVERY_FAILURES {
            e.note_failure(&r, 9, &err).await;
            assert_eq!(
                e.failures[&("o/r".to_string(), 9)],
                n % MAX_DELIVERY_FAILURES
            );
        }
        assert!(stub.posts().is_empty());
        // A sign-in phrase in an error is cut out; the rest is kept.
        assert_eq!(
            safe_error("orca: the screen said: Login expired · Please run /login"),
            "orca: the screen said: […] · Please […]"
        );
        assert_eq!(
            safe_error("gh: You are not logged into any GitHub hosts. Run gh auth login."),
            "gh: You are […]to any GitHub hosts. Run gh auth login."
        );
        assert_eq!(safe_error("plain failure"), "plain failure");
        assert!(!crate::driver::quotes_login_prompt(&safe_error(
            "NOT LOGGED IN\nInvalid API key\nSign in with ChatGPT"
        )));
        // Every line of what ssf is about to write down counts, however
        // long the text is: a phrase on the second line of a screen dump
        // is out of what a screen check reads, but the same dump collapsed
        // onto the one line that is posted puts it right there.
        let mut dump = String::from(
            "delivery failed; the screen showed:\nLogin expired · Please run /login\n",
        );
        for i in 0..25 {
            dump.push_str(&format!("│ line {i} of the transcript\n"));
        }
        assert!(
            crate::driver::quotes_login_prompt(&dump),
            "found wherever it stands"
        );
        assert_eq!(
            crate::driver::login_dialog("claude", &dump),
            None,
            "a screen is judged by its bottom"
        );
        let posted = safe_error(&events::one_line(&dump));
        assert!(
            posted.starts_with("delivery failed; the screen showed: […] · Please […] │ line 0"),
            "{posted}"
        );
        assert!(!crate::driver::quotes_login_prompt(&posted));
        assert!(!posted.contains("run /login"));
    }

    /// A relaunch that is not the target's own onboarding (a bound item's
    /// delivery bringing back an owner whose binding was given up) is a
    /// `resumed` on the owner like any other.
    #[tokio::test]
    async fn a_given_up_owner_relaunched_for_a_dependent_posts_resumed() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        seeded(&mut e, 5, Some("bot/issue-5"), true);
        {
            let st = e.entry(&r, 5);
            st.seeded = false;
            st.worktree_id = Some("w5".into());
            st.worktree_path = Some("/w/5".into());
            st.repo_id = Some("stub".into());
            st.driver = Some("orca".into());
        }
        seeded(&mut e, 8, None, true);
        {
            let st = e.entry(&r, 8);
            st.kind = Some("pull_request".into());
            st.shares_workspace_of = Some(5);
        }
        d.with(|s| {
            s.worktrees.insert("w5".into());
            s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect();
        });
        e.deliver_to(&r, 8, "[ssf] a comment on the PR", None)
            .await
            .unwrap();
        assert_eq!(d.log()[0], "relaunch:w5:false");
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(posts[0].0, "/repos/o/r/issues/5/comments");
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#5 event=resumed -->\n\n\
             ```ssf\n\
             ssf resuming agent on issue:\n\
             harness: Claude Code\n\
             conversation: fresh\n\
             after: lost terminal\n\
             ```"
        );
        assert!(e.onboarding.is_none());
    }

    /// An item onboarded onto a workspace it already had (its binding was
    /// dropped, or the state file was lost) is told it was attached again
    /// to a kept workspace, once: the relaunch inside is not a `resumed`.
    #[tokio::test]
    async fn onboarding_onto_a_kept_workspace_posts_attached_again() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        // As `note_failure` leaves an item after giving up: workspace
        // remembered, binding dropped; the agent in it is gone too.
        seeded(&mut e, 5, Some("bot/issue-5-t"), true);
        {
            let st = e.entry(&r, 5);
            st.seeded = false;
            st.worktree_id = Some("w5".into());
            st.worktree_path = Some("/w/5".into());
            st.worktree_name = Some("issue-5-t".into());
            st.repo_id = Some("stub".into());
            st.driver = Some("orca".into());
        }
        d.with(|s| {
            s.worktrees.insert("w5".into());
            s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect();
        });
        stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
        stub.set_timeline(5, vec![assigned_by(1, "alice")]);
        e.tick_repo(&r).await.unwrap();
        let st = e.entry(&r, 5).clone();
        assert!(st.seeded && st.active, "{st:?}");
        assert_eq!(st.worktree_id.as_deref(), Some("w5"), "kept");
        let log = d.log();
        assert_eq!(log[0], "relaunch:w5:false", "{log:?}");
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#5 event=attached -->\n\n\
             ```ssf\n\
             ssf attaching agent to issue again:\n\
             harness: Claude Code\n\
             model: the harness's default\n\
             effort: the harness's default\n\
             driver: orca\n\
             branch: bot/issue-5-t\n\
             workspace: kept\n\
             conversation: fresh\n\
             ```"
        );
        // With the agent still there: attached again, conversation kept.
        e.entry(&r, 5).seeded = false;
        stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
        e.tick_repo(&r).await.unwrap();
        assert!(d.log()[0].starts_with("deliver:w5:"));
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert!(
            posts[0]
                .1
                .ends_with("workspace: kept\nconversation: kept\n```"),
            "{}",
            posts[0].1
        );
    }

    /// A workspace that is gone at delivery time is re-created and the
    /// item told, with `workspace gone` as the reason.
    #[tokio::test]
    async fn a_gone_workspace_is_re_created_and_the_item_told() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.repos = vec![repo()];
        seeded(&mut e, 5, Some("bot/issue-5-fix-the-widget"), true);
        {
            let st = e.entry(&repo(), 5);
            st.title = "Fix the widget".into();
            st.html_url = "https://gh/5".into();
            st.worktree_name = Some("issue-5-fix-the-widget".into());
            st.repo_id = Some("stub".into());
            st.driver = Some("orca".into());
            st.worktree_id = Some("stub::/stub.worktrees/issue-5-fix-the-widget".into());
            st.worktree_path = Some("/stub.worktrees/issue-5-fix-the-widget".into());
            st.agent_session_id = Some("sess-5".into());
        }
        // The stub driver has no such workspace: it is re-created.
        let delivered = e.deliver_to(&repo(), 5, "hello", None).await.unwrap();
        assert!(delivered.relaunched);
        assert_eq!(
            d.log()[0],
            "relaunch:stub::/stub.worktrees/issue-5-fix-the-widget:true"
        );
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#5 event=attached -->\n\n\
             ```ssf\n\
             ssf attaching agent to issue again:\n\
             harness: Claude Code\n\
             model: the harness's default\n\
             effort: the harness's default\n\
             driver: orca\n\
             branch: bot/issue-5-fix-the-widget\n\
             re-created: workspace gone\n\
             conversation: resumed\n\
             ```"
        );
    }

    /// A pull request bound to a session's workspace says so as one.
    #[tokio::test]
    async fn a_bound_pull_request_is_attached_as_one() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        seeded(&mut e, 1, Some("bot/issue-1"), true);
        {
            let st = e.entry(&r, 1);
            st.worktree_id = Some("w1".into());
            st.terminal_handle = Some("t1".into());
        }
        d.seed("w1", "t1", READY_SCREEN);
        // As `onboard` has it before binding: kind and PR details known.
        {
            let st = e.entry(&r, 8);
            st.kind = Some("pull_request".into());
            st.pr = Some(pr("bot/issue-1"));
        }
        let mut item = issue(8, "bot", Some("fixes #1"));
        item.pull_request = Some(json!({}));
        let diff = e.diff(&r, &BTreeMap::new(), &[]);
        e.bind_to(&r, &item, 1, diff, vec!["created".into()], None)
            .await
            .unwrap();
        assert_eq!(e.entry(&r, 8).shares_workspace_of, Some(1));
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#8 event=attached -->\n\n\
             ```ssf\n\
             ssf attaching agent to pull request:\n\
             session: o/r#1\n\
             shares: workspace of #1\n\
             ```"
        );
    }

    /// An item a session handed off gets a session of its own, and its
    /// `attached` names the parent.
    #[tokio::test]
    async fn a_delegated_item_is_attached_with_its_parent_named() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        let r = repo();
        e.cfg.repos = vec![r.clone()];
        seeded(&mut e, 1, Some("bot/issue-1"), true);
        let opened = json!({
            "number": 7, "title": "child", "body": "🤖#1 says: <!-- ssf: origin=o/r#1 mode=delegate -->\n\nover to you",
            "html_url": "https://gh/7", "state": "open", "user": {"login": "bot"},
            "assignees": [{"login": "bot"}], "created_at": "x", "updated_at": "u1"
        });
        stub.set_assigned(vec![opened]);
        stub.set_timeline(7, vec![assigned_by(1, "bot")]);
        e.tick_repo(&r).await.unwrap();
        let st = e.entry(&r, 7).clone();
        assert_eq!(st.delegated_by.as_deref(), Some("o/r#1"), "{st:?}");
        assert!(st.shares_workspace_of.is_none(), "a session of its own");
        assert!(d.log()[0].starts_with("start:stub::/stub.worktrees/issue-7-child:"));
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(posts[0].0, "/repos/o/r/issues/7/comments");
        assert!(
            posts[0]
                .1
                .ends_with("driver: orca\nbranch: bot/issue-7-child\nhanded off from: o/r#1\n```"),
            "{}",
            posts[0].1
        );
    }

    // ---- the resume path (#131) -------------------------------------------

    fn resumed_block(conversation: &str) -> String {
        format!(
            "🤖 ssf <!-- ssf: origin=o/r#5 event=resumed -->\n\n\
             ```ssf\n\
             ssf resuming agent on issue:\n\
             harness: Claude Code\n\
             conversation: {conversation}\n\
             after: lost terminal\n\
             ```"
        )
    }

    /// The engine's side of #131 (the driver's decision itself is
    /// `herdr::resume_verdict`, tested there): a delivery the driver
    /// reports as resumed is the one launch there is, the conversation id
    /// is kept, and the block says `resumed`.
    #[tokio::test]
    async fn a_resumed_agent_at_work_is_settled_and_not_doubled() {
        let stub = GitHubStub::start().await;
        let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
        d.with(|s| {
            s.live.clear();
            s.resume = crate::driver::StubResume::Settles;
            s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect();
        });
        let delivered = e.deliver_to(&repo(), 5, "[ssf] hello", None).await.unwrap();
        assert!(delivered.relaunched && delivered.resumed);
        assert_eq!(d.log(), vec!["relaunch:w5:true", "deliver:w5:[ssf] hello"]);
        let launches = d.launches();
        assert_eq!(launches.len(), 1, "one launch, the resume: {launches:?}");
        assert!(launches[0].contains("--resume sess-5"), "{launches:?}");
        let st = e.entry(&repo(), 5).clone();
        assert_eq!(st.agent_session_id.as_deref(), Some("sess-5"), "kept");
        assert_eq!(st.terminal_handle.as_deref(), Some("t1"));
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(posts[0].1, resumed_block("resumed"));
    }

    /// A delivery the driver reports as fresh after a resume it gave up
    /// on: the conversation id goes, the fresh harness gets the whole
    /// story, and the block says `fresh` because that is what happened.
    #[tokio::test]
    async fn a_resume_whose_harness_exits_is_followed_by_one_fresh_harness() {
        let stub = GitHubStub::start().await;
        let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
        d.with(|s| {
            s.live.clear();
            s.resume = crate::driver::StubResume::Exits;
            s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect();
        });
        let delivered = e.deliver_to(&repo(), 5, "[ssf] hello", None).await.unwrap();
        assert!(delivered.relaunched && !delivered.resumed);
        let log = d.log();
        assert_eq!(log[0], "resume-exited:w5");
        assert_eq!(log[1], "relaunch:w5:false");
        let launches = d.launches();
        assert_eq!(
            launches.len(),
            2,
            "the resume, then the fresh start: {launches:?}"
        );
        assert!(launches[0].contains("--resume sess-5"), "{launches:?}");
        assert!(!launches[1].contains("--resume"), "{launches:?}");
        let st = e.entry(&repo(), 5).clone();
        assert_eq!(st.agent_session_id, None, "a fresh conversation");
        assert_eq!(st.terminal_handle.as_deref(), Some("t1"));
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(posts[0].1, resumed_block("fresh"));
    }

    /// The shape #133 asks a test for: the driver kept an agent that was
    /// alive when the wait ran out, and the engine ends with one handle,
    /// `resumed`, the conversation id kept and no second launch. (The
    /// stub's `Unsettled` and `Settles` reach the engine as the same
    /// delivery; the difference is the driver's, in `resume_verdict`.)
    #[tokio::test]
    async fn a_resume_alive_past_the_wait_is_kept_not_replaced() {
        let stub = GitHubStub::start().await;
        let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
        d.with(|s| {
            s.live.clear();
            s.resume = crate::driver::StubResume::Unsettled;
            s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect();
        });
        let delivered = e.deliver_to(&repo(), 5, "[ssf] hello", None).await.unwrap();
        assert!(delivered.relaunched && delivered.resumed);
        assert_eq!(
            d.log(),
            vec![
                "resume-unsettled:w5",
                "relaunch:w5:true",
                "deliver:w5:[ssf] hello"
            ]
        );
        assert_eq!(d.launches().len(), 1, "no second launch");
        let st = e.entry(&repo(), 5).clone();
        assert_eq!(st.terminal_handle.as_deref(), Some("t1"));
        assert_eq!(st.agent_session_id.as_deref(), Some("sess-5"), "kept");
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(posts[0].1, resumed_block("resumed"));
    }

    /// The startup pass says `after: restart`; a relaunch at delivery
    /// time says `after: lost terminal`.
    #[tokio::test]
    async fn a_relaunch_posts_resumed_with_why() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
        // The terminal is gone; a delivery starts the harness again.
        d.with(|s| {
            s.live.clear();
            s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect();
        });
        e.deliver_to(&repo(), 5, "[ssf] hello", None).await.unwrap();
        assert_eq!(d.log()[0], "relaunch:w5:true");
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#5 event=resumed -->\n\n\
             ```ssf\n\
             ssf resuming agent on issue:\n\
             harness: Claude Code\n\
             conversation: resumed\n\
             after: lost terminal\n\
             ```"
        );
        // A live agent: a delivery, no relaunch, no post.
        e.deliver_to(&repo(), 5, "[ssf] again", None).await.unwrap();
        assert_eq!(d.log(), vec!["deliver:w5:[ssf] again"]);
        assert!(stub.posts().is_empty());
        // Gone again over a restart: the startup pass brings it back,
        // fresh this time (no session id captured).
        d.with(|s| s.live.clear());
        e.entry(&repo(), 5).agent_session_id = None;
        stub.set_collaborators(Some(vec![]));
        e.resume_interrupted(&[DriverKind::Orca]).await;
        assert!(!e.startup_pass);
        let log = d.log();
        assert_eq!(log[0], "relaunch:w5:false", "{log:?}");
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert!(
            posts[0].1.ends_with(
                "ssf resuming agent on issue:\nharness: Claude Code\nconversation: fresh\nafter: restart\n```"
            ),
            "{}",
            posts[0].1
        );
    }

    /// Every text ssf puts on a screen or that agents read must stay free
    /// of the phrases `driver::login_dialog` looks for, or a healthy
    /// session would be blocked again by its own echo.
    /// A launch for the texts checked below; the harness is what is
    /// under test.
    fn handover_launch(harness: &str) -> events::Launch {
        events::Launch {
            harness: harness.to_string(),
            model: None,
            effort: None,
            command: None,
            driver: "herdr".into(),
            branch: Some("refs/heads/bot/issue-5".into()),
        }
    }

    #[test]
    fn ssf_texts_never_look_like_a_login_prompt() {
        use crate::driver::login_dialog;
        let harnesses = [
            "claude", "codex", "gemini", "copilot", "opencode", "pi", "omp", "grok", "crush", "zzz",
        ];
        for h in harnesses {
            let name = login::display_name(h);
            let fix = login::how_to_sign_in(h);
            let b = Blocked {
                reason: "login".into(),
                harness: h.into(),
                detail: "Login expired · Please run /login".into(),
                since: now_iso(),
                reported: true,
                credential: None,
                retried_at: None,
                retries: 0,
                told_at: None,
                tell_failures: 0,
            };
            let o = Origin::new("o/r", 5).unwrap();
            let texts = [
                prompt::login_back_prompt(&prompt::LoginBack {
                    harness: &name,
                    since: &b.since,
                    number: 5,
                    title: "Fix it",
                    url: "https://gh/5",
                }),
                events::comment(
                    &o,
                    "issue",
                    &Event::Blocked {
                        harness: name.clone(),
                        reason: "not signed in".into(),
                        fix: fix.clone(),
                    },
                ),
                // The other block: a harness that would not start at all,
                // whose reason line carries the driver's own words.
                events::comment(
                    &o,
                    "issue",
                    &Event::Blocked {
                        harness: name.clone(),
                        reason: format!(
                            "could not be started: {}",
                            safe_error(&events::one_line(
                                "herdr said: the pane exited at once\nLogin expired · Please run /login"
                            ))
                        ),
                        fix: crate::status::fix_for(&Blocked {
                            reason: Blocked::START.into(),
                            harness: h.into(),
                            ..b.clone()
                        }),
                    },
                ),
                prompt::start_again_prompt(&prompt::LoginBack {
                    harness: &name,
                    since: &b.since,
                    number: 5,
                    title: "Fix it",
                    url: "https://gh/5",
                }),
                crate::status::BlockedView::from_blocked(&Blocked {
                    reason: Blocked::START.into(),
                    detail: "the pane exited at once".into(),
                    ..b.clone()
                })
                .describe(),
                SessionBlocked {
                    session: "o/r#5".into(),
                    blocked: Blocked {
                        reason: Blocked::START.into(),
                        ..b.clone()
                    },
                }
                .to_string(),
                events::comment(
                    &o,
                    "issue",
                    &Event::Unblocked {
                        harness: name.clone(),
                        held: Duration::from_secs(12 * 60),
                        conversation: Conversation::Resumed,
                    },
                ),
                events::comment(
                    &o,
                    "pull request",
                    &Event::Unblocked {
                        harness: name.clone(),
                        held: Duration::from_secs(30),
                        conversation: Conversation::Kept,
                    },
                ),
                // A delivery error as the daemon would write it down: a
                // harness's own sign-in words in it are withheld, and
                // backticks (which would end the fence) stripped.
                events::comment(
                    &o,
                    "issue",
                    &Event::GaveUp {
                        failures: 5,
                        last_error: safe_error(&events::one_line(
                            "orca said:\n```\nLogin expired · Please run /login\n```\nnot logged in",
                        )),
                    },
                ),
                events::comment(
                    &o,
                    "issue",
                    &Event::GaveUp {
                        failures: 5,
                        last_error: safe_error("`orca worktree deliver` failed: no such terminal"),
                    },
                ),
                events::comment(
                    &o,
                    "issue",
                    &Event::GaveUp {
                        failures: 5,
                        last_error: safe_error(
                            "gh: You are not logged into any GitHub hosts. Run gh auth login.",
                        ),
                    },
                ),
                prompt::handover_prompt(
                    &name,
                    "issue",
                    Some("Branch pushed; the parser is left."),
                    "the item's story",
                ),
                prompt::handover_prompt(&name, "pull request", None, "the item's story"),
                prompt::handover_refused_prompt(&name, "the item is no longer active"),
                prompt::handover_cancelled_prompt(&name),
                crate::handover_cancelled_text("o/r#5", "Fix it", &name, true),
                crate::handover_cancelled_text("o/r#5", "Fix it", &name, false),
                prompt::handover_refused_prompt(
                    &name,
                    &events::one_line("could not stop the running agent: no such terminal"),
                ),
                events::comment(
                    &o,
                    "issue",
                    &Event::HandedOver {
                        from: handover_launch(&name),
                        to: handover_launch("Pi"),
                        summary: true,
                        by: Some("o/r#5".into()),
                        refused: None,
                    },
                ),
                events::comment(
                    &o,
                    "issue",
                    &Event::HandedOver {
                        from: handover_launch("Pi"),
                        to: handover_launch(&name),
                        summary: false,
                        by: None,
                        refused: Some(safe_error(&events::one_line(
                            "could not stop the running agent: Login expired · Please run /login",
                        ))),
                    },
                ),
                events::comment(
                    &o,
                    "issue",
                    &Event::Attached(Attach::HandedOver {
                        launch: handover_launch(&name),
                        from: "Pi".into(),
                    }),
                ),
                crate::handover_recorded_text(
                    "o/r#5",
                    "Fix it",
                    &name,
                    Some("fable-5.1"),
                    Some("high"),
                    None,
                    Some(1_234),
                    10,
                ),
                crate::handover_recorded_text("o/r#5", "Fix it", &name, None, None, None, None, 10),
                crate::handover_recorded_text(
                    "o/r#5",
                    "Fix it",
                    &name,
                    None,
                    None,
                    Some("claude --dangerously-skip-permissions"),
                    None,
                    10,
                ),
                crate::summary_quotes_a_sign_in_screen_text(&crate::driver::redact_login_phrases(
                    "the pane said Login expired · Please run /login, so I stopped",
                )),
                crate::status::BlockedView::from_blocked(&b).describe(),
                SessionBlocked {
                    session: "o/r#5".into(),
                    blocked: b.clone(),
                }
                .to_string(),
                fix.clone(),
            ];
            for text in texts {
                // As the harness would show it: at the bottom of the screen,
                // after the agent's prompt marker, and without the `[ssf]`
                // marker, which would make the echo skip pass over the very
                // line under test.
                let screen = format!("⏺ {}\n\n❯ ", text.replace("[ssf]", ""));
                for judge in harnesses {
                    assert_eq!(
                        login_dialog(judge, &screen),
                        None,
                        "{judge} takes this {h} text for a login prompt: {text}"
                    );
                }
            }
        }
    }

    // ---- handovers ------------------------------------------------------

    /// An item ready to be handed over: item 5 on `w5` with a live agent,
    /// and enough on the GitHub stub for the new session's story.
    fn handover_setup(stub: &GitHubStub) -> (Engine, crate::driver::StubDriver) {
        let (e, d) = blocked_setup(stub, READY_SCREEN);
        stub.set_issue(5, assigned_item(5, "alice", "u1"));
        stub.set_timeline(5, vec![assigned_by(1, "alice")]);
        (e, d)
    }

    #[tokio::test]
    async fn harness_notes_follow_the_session_through_handover_and_restart() {
        let sandbox = crate::config::test_support::sandbox();
        let worktree = sandbox.root().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        for (file, text) in [
            ("SSF.md", "Shared project guidance."),
            ("SSF.claude.md", "Claude-only guidance."),
            ("SSF.codex.md", "Codex-only guidance."),
        ] {
            std::fs::write(worktree.join(file), text).unwrap();
        }
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        let r = repo();
        e.entry(&r, 5).worktree_path = Some(worktree.to_string_lossy().into_owned());
        let issue: Issue = serde_json::from_value(assigned_item(5, "alice", "u1")).unwrap();
        let initial = e.initial_text(&r, &issue, &[]);
        assert!(initial.contains("Shared project guidance."));
        assert!(initial.contains("Claude-only guidance."));
        assert!(!initial.contains("Codex-only guidance."));

        e.handover("o/r#5", "codex", None, None, None, None)
            .await
            .unwrap();
        e.run_handovers(&r).await;
        let prompts = d.prompts();
        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert!(prompts[0].contains("Shared project guidance."));
        assert!(prompts[0].contains("Codex-only guidance."));
        assert!(!prompts[0].contains("Claude-only guidance."));

        // A fresh session after the handover uses the persisted override.
        let restarted = e.first_message(&r, 5).await.unwrap().text;
        assert!(restarted.contains("Shared project guidance."));
        assert!(restarted.contains("Codex-only guidance."));
        assert!(!restarted.contains("Claude-only guidance."));
    }

    /// The whole path of `ssf handover` with a summary: recorded
    /// synchronously, carried out on the next pass in the same workspace,
    /// with the two posts and the overrides left on the item.
    #[tokio::test]
    async fn a_handover_replaces_the_session_in_the_same_workspace() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        let v = e
            .handover(
                "o/r#5",
                "pi",
                Some("openai/gpt-6"),
                Some("high"),
                Some("Branch pushed; the parser is left."),
                Some("o/r#5"),
            )
            .await
            .unwrap();
        assert_eq!(v["session"], "o/r#5");
        assert_eq!(v["title"], "Fix the widget");
        assert_eq!(v["from"]["harness"], "claude");
        assert_eq!(v["from"]["model"], Value::Null);
        assert_eq!(v["to"]["harness"], "pi");
        assert_eq!(v["to"]["model"], "openai/gpt-6");
        assert_eq!(v["to"]["effort"], "high");
        assert_eq!(v["summary_chars"], 34);
        // Recorded and nothing else: the agent that asked is still there.
        assert!(e.entry(&repo(), 5).handover.is_some());
        assert!(d.log().is_empty(), "{:?}", d.log());
        assert!(stub.posts().is_empty());
        // While it is pending, nothing else touches the session.
        let err = e
            .handover("o/r#5", "codex", None, None, None, None)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("a handover to pi is already pending"),
            "{err:#}"
        );
        let err = e.tell(None, "o/r#5", "hello").await.unwrap_err();
        assert!(
            err.to_string().contains("a handover to pi is pending"),
            "{err:#}"
        );
        assert!(!e.resume_candidates(&repo()).contains(&5));
        // Delivery failures counted against the session that is going.
        e.failures.insert(("o/r".into(), 5), 2);

        e.run_handovers(&repo()).await;
        let log = d.log();
        assert_eq!(log[0], "stop:t5", "{log:?}");
        assert!(
            log[1].starts_with("start:w5:You took over this issue from a session on Claude"),
            "{log:?}"
        );
        assert_eq!(log.len(), 2, "{log:?}");
        let launched = d.launches();
        assert_eq!(launched.len(), 1, "{launched:?}");
        assert!(
            launched[0].starts_with("pi:") && launched[0].contains("openai/gpt-6"),
            "{launched:?}"
        );
        let st = e.entry(&repo(), 5).clone();
        assert!(st.handover.is_none(), "carried out");
        assert_eq!(
            st.overrides,
            Some(Overrides {
                harness: "pi".into(),
                model: Some("openai/gpt-6".into()),
                effort: Some("high".into()),
            })
        );
        // The old session is retired on the record; the workspace is not,
        // and nothing counted against it follows the new one.
        assert!(st.agent_session_id.is_none());
        assert!(e.failures.is_empty());
        // Its conversation is remembered as retired, so the transcript it
        // wrote moments ago is not captured as the new session's.
        assert_eq!(st.retired_session_ids, vec!["sess-5".to_string()]);
        assert!(st.blocked.is_none());
        assert_eq!(st.worktree_id.as_deref(), Some("w5"));
        assert_eq!(st.branch.as_deref(), Some("refs/heads/bot/issue-5"));
        assert!(st.seeded && st.active);
        assert!(st.terminal_handle.is_some());
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 2, "{posts:?}");
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#5 event=handed-over -->\n\n\
             ```ssf\n\
             ssf handing over issue:\n\
             from: Claude Code\n\
             from model: the harness's default\n\
             from effort: the harness's default\n\
             to: Pi\n\
             to model: openai/gpt-6\n\
             to effort: high\n\
             summary: yes\n\
             by: o/r#5\n\
             ```"
        );
        assert_eq!(
            posts[1].1,
            "🤖 ssf <!-- ssf: origin=o/r#5 event=attached -->\n\n\
             ```ssf\n\
             ssf attaching agent to issue:\n\
             harness: Pi\n\
             model: openai/gpt-6\n\
             effort: high\n\
             driver: orca\n\
             branch: bot/issue-5\n\
             handed over from: Claude Code\n\
             ```"
        );
    }

    /// Without a summary, and asked for by a person at a shell: the post
    /// says both, and the new session is told to read the item.
    #[tokio::test]
    async fn a_handover_without_a_summary_says_so() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        e.handover("o/r#5", "codex", None, None, None, None)
            .await
            .unwrap();
        e.run_handovers(&repo()).await;
        let log = d.log();
        assert!(
            log[1].starts_with("start:w5:You took over this issue from a session on Claude"),
            "{log:?}"
        );
        assert_eq!(
            e.entry(&repo(), 5).overrides,
            Some(Overrides {
                harness: "codex".into(),
                model: None,
                effort: None,
            })
        );
        let posts = stub.post_bodies();
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#5 event=handed-over -->\n\n\
             ```ssf\n\
             ssf handing over issue:\n\
             from: Claude Code\n\
             from model: the harness's default\n\
             from effort: the harness's default\n\
             to: Codex\n\
             to model: the harness's default\n\
             to effort: the harness's default\n\
             summary: no\n\
             by: a person at the terminal\n\
             ```"
        );
    }

    /// The new session is told the item's whole story, so what happened
    /// between the request and the pass is in that first message and is
    /// not delivered to it a second time by the pass that follows.
    #[tokio::test]
    async fn the_story_the_new_session_is_told_counts_as_delivered() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        e.handover("o/r#5", "pi", None, None, Some("half done"), None)
            .await
            .unwrap();
        // A comment lands after the handover was recorded: the pass runs
        // the handovers before it looks at the item.
        stub.set_issue(5, assigned_item(5, "alice", "u2"));
        stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
        stub.set_timeline(
            5,
            vec![
                assigned_by(1, "alice"),
                comment(2, "alice", "one more thing: keep the flag"),
            ],
        );
        e.run_handovers(&repo()).await;
        let prompts = d.prompts();
        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert!(
            prompts[0].contains("one more thing: keep the flag"),
            "the story carries the new comment: {}",
            prompts[0]
        );
        let st = e.entry(&repo(), 5).clone();
        assert_eq!(st.updated_at.as_deref(), Some("u2"));
        assert!(st.seen.contains_key("commented:2"), "{:?}", st.seen);
        // The rest of the pass has nothing left to tell the new session.
        let _ = (d.log(), stub.post_bodies());
        e.tick_repo(&repo()).await.unwrap();
        let log = d.log();
        assert!(log.is_empty(), "delivered twice: {log:?}");
    }

    /// Every launch of the item after a handover uses its overrides: the
    /// re-created workspace, and the startup pass.
    #[tokio::test]
    async fn the_overrides_outlive_the_handover_pass() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        e.handover("o/r#5", "codex", None, None, None, None)
            .await
            .unwrap();
        e.run_handovers(&repo()).await;
        let _ = (d.log(), d.launches(), stub.post_bodies());
        // The terminal is gone: the startup pass brings the session back,
        // on the harness the handover put on the item.
        d.with(|s| {
            s.live.remove("w5");
        });
        e.entry(&repo(), 5).agent_session_id = Some("sess-5".into());
        e.resume_interrupted(&[DriverKind::Orca]).await;
        let launched = d.launches();
        assert_eq!(launched.len(), 1, "{launched:?}");
        assert!(
            launched[0].starts_with("codex:") && launched[0].contains("resume sess-5"),
            "{launched:?}"
        );
        // And so does a workspace that has to be re-created.
        let _ = (d.log(), stub.post_bodies());
        d.with(|s| {
            s.worktrees.remove("w5");
            s.live.remove("w5");
        });
        e.entry(&repo(), 5).repo_id = Some("stub".into());
        e.deliver_to(&repo(), 5, "[ssf] hello", None).await.unwrap();
        let launched = d.launches();
        assert!(
            launched.iter().all(|l| l.starts_with("codex:")),
            "{launched:?}"
        );
    }

    /// The same harness with another model: the repository's own command
    /// still starts the agent, the effort the repository set carries over,
    /// and the post names the command both ends run under (without it,
    /// `the command's` in the model line refers to nothing).
    #[tokio::test]
    async fn a_model_only_handover_keeps_the_repository_command() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        let r = RepoConfig {
            command: Some("claude --dangerously-skip-permissions".into()),
            effort: Some("high".into()),
            ..repo()
        };
        e.cfg.repos = vec![r.clone()];
        let v = e
            .handover(
                "o/r#5",
                "claude",
                Some("opus"),
                None,
                Some("what is left"),
                None,
            )
            .await
            .unwrap();
        assert_eq!(v["from"]["harness"], "claude");
        assert_eq!(v["from"]["model"], Value::Null);
        assert_eq!(v["from"]["effort"], "high");
        assert_eq!(v["to"]["model"], "opus");
        assert_eq!(v["to"]["effort"], "high", "the repository's effort stays");
        e.run_handovers(&r).await;
        let launched = d.launches();
        assert_eq!(launched.len(), 1, "{launched:?}");
        assert!(
            launched[0].starts_with("claude:")
                && launched[0].contains("--dangerously-skip-permissions"),
            "{launched:?}"
        );
        assert!(launched[0].contains("opus"), "{launched:?}");
        // A handover on the same harness is where a transcript is most
        // easily mixed up, so the item says one happened whether or not
        // an id was ever captured for the session that left.
        assert!(e.entry(&r, 5).handed_over_at.is_some());
        assert_eq!(
            e.entry(&r, 5).overrides,
            Some(Overrides {
                harness: "claude".into(),
                model: Some("opus".into()),
                effort: None,
            })
        );
        let posts = stub.post_bodies();
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#5 event=handed-over -->\n\n\
             ```ssf\n\
             ssf handing over issue:\n\
             from: Claude Code\n\
             from model: the command's\n\
             from effort: high\n\
             from command: claude --dangerously-skip-permissions\n\
             to: Claude Code\n\
             to model: opus\n\
             to effort: high\n\
             to command: claude --dangerously-skip-permissions\n\
             summary: yes\n\
             by: a person at the terminal\n\
             ```"
        );
    }

    /// A handover asked for on an item bound to another session's
    /// workspace is the owning session's: one workspace, one harness in
    /// it, and the bound item shows what its owner runs.
    #[tokio::test]
    async fn a_handover_on_a_bound_item_is_the_owning_session_s() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        seeded(&mut e, 6, Some("bot/issue-5"), true);
        {
            let st = e.entry(&repo(), 6);
            st.shares_workspace_of = Some(5);
            st.worktree_id = Some("w5".into());
            st.worktree_path = Some("/w/5".into());
            st.title = "Follow-up".into();
            st.html_url = "https://gh/6".into();
            // The bound item mirrors the owner's conversation.
            st.agent_session_id = Some("sess-5".into());
        }
        let v = e
            .handover(
                "o/r#6",
                "pi",
                None,
                None,
                Some("what is left"),
                Some("o/r#6"),
            )
            .await
            .unwrap();
        assert_eq!(v["session"], "o/r#5", "the owning session's");
        assert_eq!(v["title"], "Fix the widget");
        assert!(e.entry(&repo(), 5).handover.is_some());
        assert!(e.entry(&repo(), 6).handover.is_none());
        e.run_handovers(&repo()).await;
        let log = d.log();
        assert_eq!(log[0], "stop:t5", "{log:?}");
        assert!(log[1].starts_with("start:w5:"), "{log:?}");
        assert!(e.entry(&repo(), 5).overrides.is_some());
        assert!(
            e.entry(&repo(), 6).overrides.is_none(),
            "the override lives on the owner"
        );
        // The retired conversation is gone from the bound item too, and
        // is not offered back to the new session through the mirror.
        let bound_state = e.entry(&repo(), 6).clone();
        assert!(bound_state.agent_session_id.is_none());
        assert_eq!(bound_state.retired_session_ids, vec!["sess-5".to_string()]);
        // Both items run the new harness, and say so.
        assert_eq!(e.effective(&repo(), 6).harness, "pi");
        let sessions = crate::status::sessions(&e.cfg, &e.state, Some(&[]));
        let bound = sessions.iter().find(|s| s.number == 6).unwrap();
        assert_eq!(bound.harness, "pi");
        assert_eq!(
            sessions.iter().find(|s| s.number == 5).unwrap().harness,
            "pi"
        );
    }

    /// Each synchronous refusal, with its reason.
    #[tokio::test]
    async fn handovers_are_refused_with_the_reason() {
        let stub = GitHubStub::start().await;
        let (mut e, _d) = handover_setup(&stub);
        let msg = |r: Result<Value>| r.unwrap_err().to_string();
        // Not a session ssf knows.
        assert!(
            msg(e.handover("o/r#9", "pi", None, None, None, None).await)
                .contains("is not an agent session ssf knows")
        );
        // A harness nothing knows, and a model or effort the harness
        // cannot take.
        assert!(
            msg(e.handover("o/r#5", "zzz", None, None, None, None).await)
                .contains("zzz is not a harness ssf knows")
        );
        assert!(
            msg(e
                .handover("o/r#5", "pi", None, Some("turbo"), None, None)
                .await)
            .contains("is not a level pi accepts")
        );
        // Not installed here, then not signed in here.
        e.installed = std::sync::Arc::new(|_| false);
        assert!(
            msg(e.handover("o/r#5", "pi", None, None, None, None).await)
                .contains("Pi is not installed where the daemon runs")
        );
        e.installed = std::sync::Arc::new(|_| true);
        probe_returning(&mut e, LoginState::SignedOut, None);
        let err = msg(e.handover("o/r#5", "pi", None, None, None, None).await);
        assert!(err.contains("Pi is not signed in here"), "{err}");
        assert!(err.contains(&login::how_to_sign_in("pi")), "{err}");
        // The signed-out answer is not remembered: an operator who signs
        // the harness in and runs the command again is not told the same
        // thing until the next pass, because the command drops the memo
        // for that harness before it asks.
        e.probe = std::sync::Arc::new(|_| Probe {
            state: LoginState::SignedIn,
            detail: "test".into(),
            fingerprint: None,
        });
        e.handover("o/r#5", "pi", None, None, None, None)
            .await
            .expect("the fresh login is seen straight away");
        e.entry(&repo(), 5).handover = None;
        probe_returning(&mut e, LoginState::Unknown, None);
        // The target is what the item already runs.
        assert!(
            msg(e.handover("o/r#5", "claude", None, None, None, None).await)
                .contains("already on claude with that model and effort")
        );
        // An empty summary is not a summary: the CLI refuses it, and so
        // does the daemon, for a request that did not come through it.
        assert!(
            msg(e
                .handover("o/r#5", "pi", None, None, Some("  \n"), None)
                .await)
            .contains("the summary is empty")
        );
        // A summary that would read as the new harness's sign-in screen.
        let err = msg(e
            .handover(
                "o/r#5",
                "pi",
                None,
                None,
                Some("I got stuck: the pane kept saying Please run /login"),
                None,
            )
            .await);
        assert!(
            err.contains("would read as a harness's own sign-in screen"),
            "{err}"
        );
        assert!(!crate::driver::quotes_login_prompt(&err), "{err}");
        // A summary longer than the cap.
        let long = "x".repeat(crate::ipc::MAX_SUMMARY_CHARS + 1);
        assert!(
            msg(e
                .handover("o/r#5", "pi", None, None, Some(&long), None)
                .await)
            .contains("the most a handover carries is 8000")
        );
        // A release is pending on it.
        e.entry(&repo(), 5).release_pending = true;
        assert!(
            msg(e.handover("o/r#5", "pi", None, None, None, None).await)
                .contains("a release is pending on this item")
        );
        e.entry(&repo(), 5).release_pending = false;
        // And a release is refused while a handover is pending.
        e.handover("o/r#5", "pi", None, None, None, None)
            .await
            .unwrap();
        e.entry(&repo(), 5).active = false;
        assert!(
            msg(e.release("o/r#5", false).await).contains("a handover to pi is pending"),
            "a release must not race the handover"
        );
        // The item has no running session at all.
        e.entry(&repo(), 5).handover = None;
        assert!(
            msg(e.handover("o/r#5", "pi", None, None, None, None).await)
                .contains("the item has no running session")
        );
    }

    /// A handover the pass cannot carry out: nothing changes, the item
    /// says so, and the agent that asked is told to carry on.
    #[tokio::test]
    async fn a_handover_the_pass_cannot_carry_out_is_refused_on_the_item() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        e.handover(
            "o/r#5",
            "pi",
            None,
            None,
            Some("what is left"),
            Some("o/r#5"),
        )
        .await
        .unwrap();
        // The item is dropped between the request and the pass.
        e.entry(&repo(), 5).active = false;
        e.run_handovers(&repo()).await;
        let st = e.entry(&repo(), 5).clone();
        assert!(st.handover.is_none(), "the pending handover is off");
        assert!(st.overrides.is_none(), "nothing was changed");
        assert_eq!(st.agent_session_id.as_deref(), Some("sess-5"));
        let log = d.log();
        assert_eq!(log.len(), 1, "{log:?}");
        assert_eq!(
            log[0],
            "deliver:w5:[ssf] Handover to Pi refused: the item is no longer active. "
        );
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#5 event=handed-over -->\n\n\
             ```ssf\n\
             ssf not handing over issue:\n\
             to: Pi\n\
             to model: the harness's default\n\
             to effort: the harness's default\n\
             by: o/r#5\n\
             refused: the item is no longer active\n\
             ```"
        );
    }

    /// The daemon restarting between the request and the pass changes
    /// nothing: the pending handover is in the state file.
    #[tokio::test]
    async fn a_pending_handover_survives_a_restart() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        e.handover("o/r#5", "pi", None, None, Some("half done"), None)
            .await
            .unwrap();
        // As a restart leaves it: the state as written, read back.
        let written = serde_json::to_string(&e.state).unwrap();
        let mut e = engine_at(&stub.base);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.repos = vec![repo()];
        e.state = serde_json::from_str(&written).unwrap();
        let h = e.entry(&repo(), 5).handover.clone().unwrap();
        assert_eq!(h.harness, "pi");
        assert_eq!(h.summary.as_deref(), Some("half done"));
        e.run_handovers(&repo()).await;
        assert!(e.entry(&repo(), 5).handover.is_none());
        assert_eq!(
            e.entry(&repo(), 5)
                .overrides
                .as_ref()
                .map(|o| o.harness.clone()),
            Some("pi".into())
        );
        let log = d.log();
        assert_eq!(log[0], "stop:t5", "{log:?}");
        assert!(log[1].starts_with("start:w5:You took over"), "{log:?}");
    }

    /// The new harness comes up at its own sign-in prompt: the session is
    /// blocked as any other, and the old one is not brought back.
    #[tokio::test]
    async fn a_new_harness_at_its_sign_in_prompt_blocks_the_new_session() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        d.with(|s| {
            s.relaunch_screen = vec!["  Use /login to log into a provider".into(), "❯ ".into()];
        });
        e.handover("o/r#5", "pi", None, None, None, Some("o/r#5"))
            .await
            .unwrap();
        e.run_handovers(&repo()).await;
        let st = e.entry(&repo(), 5).clone();
        assert!(st.overrides.is_some(), "the handover stands");
        let b = st.blocked.clone().expect("blocked");
        assert_eq!(b.harness, "pi");
        assert!(b.reported);
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 2, "{posts:?}");
        assert!(posts[0].1.contains("ssf handing over issue:"), "{posts:?}");
        assert_eq!(
            posts[1].1,
            format!(
                "🤖 ssf <!-- ssf: origin=o/r#5 event=blocked -->\n\n\
                 ```ssf\n\
                 ssf holding deliveries to agent on issue:\n\
                 harness: Pi\n\
                 reason: not signed in\n\
                 fix: {}\n\
                 ```",
                login::how_to_sign_in("pi").replace('`', "")
            )
        );
    }

    /// The new harness cannot be started at all (a model id it refuses,
    /// a binary that exits at once): the handover stands, the item is
    /// blocked with the usual post and the usual recovery, and no
    /// `attached` claims a session that is not there.
    #[tokio::test]
    async fn a_new_harness_that_will_not_start_blocks_the_item() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        d.with(|s| {
            s.start_error = Some("pi exited at once: ambiguous model gpt-5.5".into());
        });
        e.handover(
            "o/r#5",
            "pi",
            Some("openai/gpt-6"),
            None,
            None,
            Some("o/r#5"),
        )
        .await
        .unwrap();
        e.run_handovers(&repo()).await;
        let st = e.entry(&repo(), 5).clone();
        assert!(st.handover.is_none(), "carried out, not left pending");
        assert!(st.overrides.is_some(), "the handover stands");
        assert!(st.terminal_handle.is_none(), "nothing is running");
        let b = st.blocked.clone().expect("blocked");
        // The login check cannot tell (the default in these tests), so
        // the block stands as what was seen: the harness would not start.
        assert_eq!(b.reason, Blocked::START);
        assert_eq!(b.harness, "pi");
        assert!(b.reported);
        assert!(b.detail.contains("ambiguous model"), "{}", b.detail);
        // The item says both, in order, and nothing says a session attached.
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 2, "{posts:?}");
        assert!(posts[0].1.contains("ssf handing over issue:"), "{posts:?}");
        assert_eq!(
            posts[1].1,
            "🤖 ssf <!-- ssf: origin=o/r#5 event=blocked -->\n\n\
             ```ssf\n\
             ssf holding deliveries to agent on issue:\n\
             harness: Pi\n\
             reason: could not be started: pi exited at once: ambiguous model gpt-5.5\n\
             fix: start Pi by hand in the workspace, or fix the model or effort and hand over again\n\
             ```"
        );
        // A person sees it in the status commands.
        let view = crate::status::BlockedView::from_blocked(&b);
        assert!(
            view.describe().starts_with("Pi could not be started since"),
            "{}",
            view.describe()
        );
        // And the recovery is the usual one: after the wait the harness is
        // started again in the same workspace, on the item's overrides.
        let _ = (d.log(), d.launches());
        if let Some(cur) = e.entry(&repo(), 5).blocked.as_mut() {
            cur.since = "2020-01-01T00:00:00Z".into();
        }
        let st = e.entry(&repo(), 5).clone();
        let b = st.blocked.clone().unwrap();
        e.recover(&repo(), 5, &st, b).await;
        assert!(e.entry(&repo(), 5).blocked.is_none(), "the block is lifted");
        let launched = d.launches();
        assert_eq!(launched.len(), 1, "{launched:?}");
        assert!(launched[0].starts_with("pi:"), "{launched:?}");
        let log = d.log();
        assert!(log.iter().any(|l| l.starts_with("relaunch:w5:")), "{log:?}");
    }

    /// `ssf handover --cancel`: the way back out of a pending handover,
    /// which otherwise refuses every other command on the item.
    #[tokio::test]
    async fn a_pending_handover_can_be_cancelled() {
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        let nothing = e
            .handle_request(crate::ipc::Request::CancelHandover {
                session: "o/r#5".into(),
            })
            .await;
        assert!(!nothing.ok);
        assert!(
            nothing.error.unwrap().contains("no handover is pending"),
            "refused with the reason"
        );
        e.handover("o/r#5", "pi", None, None, Some("half done"), Some("o/r#5"))
            .await
            .unwrap();
        let r = e
            .handle_request(crate::ipc::Request::CancelHandover {
                session: "o/r#5".into(),
            })
            .await;
        assert!(r.ok, "{:?}", r.error);
        assert_eq!(r.data["session"], "o/r#5");
        assert_eq!(r.data["harness_name"], "Pi");
        assert_eq!(r.data["told"], true);
        // The agent that was told to stop hears that it carries on, and
        // nothing is posted on the item: the handover was never announced.
        let log = d.log();
        assert_eq!(log.len(), 1, "{log:?}");
        assert_eq!(
            log[0],
            "deliver:w5:[ssf] The handover to Pi was cancelled: this session keeps t"
        );
        assert!(stub.posts().is_empty());
        // Nothing pending: the pass leaves the session alone and the
        // ordinary commands work again.
        assert!(e.entry(&repo(), 5).handover.is_none());
        e.run_handovers(&repo()).await;
        assert!(d.log().is_empty(), "the session stays");
        assert!(e.entry(&repo(), 5).overrides.is_none());
        assert!(e.resume_candidates(&repo()).contains(&5));
        e.tell(None, "o/r#5", "hello").await.unwrap();
    }

    /// A session blocked on its harness's sign-in prompt may hand over --
    /// that is a way out of the block -- and the hold on the item is
    /// closed when it does, rather than standing over a session that is
    /// no longer there.
    #[tokio::test]
    async fn a_handover_closes_an_outstanding_hold_on_the_item() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        e.entry(&repo(), 5).blocked = Some(Blocked {
            reason: Blocked::LOGIN.into(),
            harness: "claude".into(),
            detail: "Login expired · Please run /login".into(),
            since: (chrono::Utc::now() - chrono::Duration::minutes(20))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            reported: true,
            credential: None,
            retried_at: None,
            retries: 0,
            told_at: None,
            tell_failures: 0,
        });
        e.handover("o/r#5", "pi", None, None, Some("half done"), None)
            .await
            .unwrap();
        e.run_handovers(&repo()).await;
        let st = e.entry(&repo(), 5).clone();
        assert!(st.blocked.is_none(), "{:?}", st.blocked);
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 3, "{posts:?}");
        assert_eq!(
            posts[0].1,
            "🤖 ssf <!-- ssf: origin=o/r#5 event=unblocked -->\n\n\
             ```ssf\n\
             ssf resuming deliveries to agent on issue:\n\
             harness: Claude Code\n\
             held for: 20 min\n\
             conversation: handed over\n\
             ```"
        );
        assert!(posts[1].1.contains("ssf handing over issue:"), "{posts:?}");
        assert!(
            posts[2].1.contains("ssf attaching agent to issue:"),
            "{posts:?}"
        );
        let log = d.log();
        assert_eq!(log[0], "stop:t5", "{log:?}");
    }

    /// A harness that would not start and is not signed in where the
    /// daemon runs is recorded as the sign-in block it really is: that is
    /// the thing to fix, and the recovery from #85 is the one that fits.
    #[tokio::test]
    async fn a_harness_that_will_not_start_and_is_signed_out_is_a_login_block() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        d.with(|s| s.start_error = Some("pi exited at once".into()));
        // Accepted while the check cannot tell; signed out by the time
        // the pass runs (a login that lapsed in between).
        e.handover("o/r#5", "pi", None, None, None, None)
            .await
            .unwrap();
        probe_returning(&mut e, LoginState::SignedOut, Some("cred-old"));
        e.run_handovers(&repo()).await;
        let b = e.entry(&repo(), 5).blocked.clone().expect("blocked");
        assert_eq!(b.reason, Blocked::LOGIN);
        assert_eq!(b.harness, "pi");
        assert_eq!(b.credential.as_deref(), Some("cred-old"));
        assert!(
            b.detail.contains("exited at once"),
            "what was seen is kept: {}",
            b.detail
        );
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 2, "{posts:?}");
        assert!(posts[1].1.contains("reason: not signed in"), "{posts:?}");
        assert!(
            posts[1].1.contains(&format!(
                "fix: {}",
                login::how_to_sign_in("pi").replace('`', "")
            )),
            "{posts:?}"
        );
        // And it recovers as a sign-in block does: nothing while the
        // check still says signed out, whatever the backoff says.
        let _ = d.log();
        e.entry(&repo(), 5).blocked.as_mut().unwrap().since = "2020-01-01T00:00:00Z".into();
        let st = e.entry(&repo(), 5).clone();
        e.recover(&repo(), 5, &st, b).await;
        assert!(d.log().is_empty(), "still signed out");
        assert!(e.entry(&repo(), 5).blocked.is_some());
    }

    /// The summary is the point of a handover, so it outlives a new
    /// harness that will not come up: it waits on the item until a
    /// session has read it, and the restart carries it.
    #[tokio::test]
    async fn the_summary_outlives_a_harness_that_would_not_start() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        d.with(|s| s.start_error = Some("pi exited at once".into()));
        e.handover(
            "o/r#5",
            "pi",
            None,
            None,
            Some("The parser is half migrated; the flag is unverified."),
            Some("o/r#5"),
        )
        .await
        .unwrap();
        e.run_handovers(&repo()).await;
        let st = e.entry(&repo(), 5).clone();
        assert_eq!(
            st.blocked.as_ref().map(|b| b.reason.as_str()),
            Some("start")
        );
        let note = st.handover_note.clone().expect("the summary is kept");
        assert_eq!(note.from, "Claude Code");
        assert!(note.summary.unwrap().contains("half migrated"));
        // A restart that comes up at a sign-in prompt read the message as
        // a screen, not as a session: the block becomes the sign-in one
        // and the summary is still owed.
        let _ = (d.log(), d.prompts(), stub.post_bodies());
        d.with(|s| s.relaunch_screen = PI_LOGIN_SCREEN.iter().map(|l| l.to_string()).collect());
        e.entry(&repo(), 5).blocked.as_mut().unwrap().since = "2020-01-01T00:00:00Z".into();
        let st = e.entry(&repo(), 5).clone();
        let b = st.blocked.clone().unwrap();
        e.recover(&repo(), 5, &st, b).await;
        let st = e.entry(&repo(), 5).clone();
        assert_eq!(
            st.blocked.as_ref().map(|b| b.reason.as_str()),
            Some("login"),
            "{:?}",
            st.blocked
        );
        assert!(
            st.handover_note.is_some(),
            "the message went into the sign-in screen, so the summary waits"
        );
        // The restart after the backoff tells the new harness what the
        // outgoing agent left, then the item's story.
        let _ = (d.log(), d.prompts(), stub.post_bodies());
        d.with(|s| s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect());
        {
            let b = e.entry(&repo(), 5).blocked.as_mut().unwrap();
            b.since = "2020-01-01T00:00:00Z".into();
            b.retried_at = None;
        }
        let st = e.entry(&repo(), 5).clone();
        let b = st.blocked.clone().unwrap();
        e.recover(&repo(), 5, &st, b).await;
        let prompts = d.prompts();
        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert!(
            prompts[0].starts_with("You took over this issue from a session on Claude Code"),
            "{}",
            prompts[0]
        );
        assert!(
            prompts[0].contains("The parser is half migrated; the flag is unverified."),
            "the summary is delivered: {}",
            prompts[0]
        );
        // Read once: the next start is not given it again.
        assert!(e.entry(&repo(), 5).handover_note.is_none());
    }

    /// A `start` that failed on a harness that is running all the same
    /// (the pane came up but never settled): the block is not lifted on
    /// the screen alone, the session is given what it was never told.
    #[tokio::test]
    async fn a_started_harness_behind_a_start_block_is_told_before_the_block_lifts() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        d.with(|s| s.start_error = Some("pi did not become idle in time".into()));
        e.handover("o/r#5", "pi", None, None, Some("half done"), None)
            .await
            .unwrap();
        e.run_handovers(&repo()).await;
        assert!(e.entry(&repo(), 5).blocked.is_some());
        // The pane is there after all, and idle.
        d.seed("w5", "t9", READY_SCREEN);
        let _ = (d.log(), d.prompts(), stub.post_bodies());
        let st = e.entry(&repo(), 5).clone();
        let b = st.blocked.clone().unwrap();
        e.recover(&repo(), 5, &st, b).await;
        // Delivered into the pane that is there, not restarted.
        let log = d.log();
        assert_eq!(log.len(), 1, "{log:?}");
        assert!(
            log[0].starts_with("deliver:w5:You took over this issue"),
            "{log:?}"
        );
        let prompts = d.prompts();
        assert!(prompts[0].contains("half done"), "{}", prompts[0]);
        let st = e.entry(&repo(), 5).clone();
        assert!(st.blocked.is_none(), "the block is lifted");
        assert!(st.handover_note.is_none(), "read once");
        assert_eq!(st.terminal_handle.as_deref(), Some("t9"));
        // What the story showed counts as seen, so the pass that follows
        // does not deliver it again.
        assert_eq!(st.updated_at.as_deref(), Some("u1"));
        assert!(st.seen.contains_key("assigned:1"), "{:?}", st.seen);
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert!(
            posts[0]
                .1
                .contains("ssf resuming deliveries to agent on issue:"),
            "{posts:?}"
        );
    }

    /// A handover whose harness comes up at its sign-in prompt: the first
    /// message went into that screen, so nothing has read the summary.
    /// A person signing in at the terminal is not enough to lift the
    /// block on its own -- the session still has to be told.
    #[tokio::test]
    async fn a_handover_blocked_at_the_sign_in_prompt_is_told_when_a_person_signs_in() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        d.with(|s| s.relaunch_screen = PI_LOGIN_SCREEN.iter().map(|l| l.to_string()).collect());
        e.handover("o/r#5", "pi", None, None, Some("half migrated"), None)
            .await
            .unwrap();
        e.run_handovers(&repo()).await;
        let st = e.entry(&repo(), 5).clone();
        assert_eq!(
            st.blocked.as_ref().map(|b| b.reason.as_str()),
            Some("login")
        );
        assert!(st.handover_note.is_some(), "nothing has read the summary");
        // A person runs the sign-in in the terminal: the pane that is
        // there is past its prompt, but was never told what it is for.
        let handle = st.terminal_handle.clone().expect("the pane came up");
        d.with(|s| {
            s.screens.insert(
                handle.clone(),
                READY_SCREEN.iter().map(|l| l.to_string()).collect(),
            )
        });
        let _ = (d.log(), d.prompts(), stub.post_bodies());
        let b = st.blocked.clone().unwrap();
        e.recover(&repo(), 5, &st, b).await;
        // Told where it stands, not restarted.
        let log = d.log();
        assert_eq!(log.len(), 1, "{log:?}");
        assert!(
            log[0].starts_with("deliver:w5:You took over this issue"),
            "{log:?}"
        );
        let prompts = d.prompts();
        assert!(prompts[0].contains("half migrated"), "{}", prompts[0]);
        let st = e.entry(&repo(), 5).clone();
        assert!(st.blocked.is_none(), "the block is lifted");
        assert!(st.handover_note.is_none(), "read once");
        assert_eq!(st.terminal_handle.as_deref(), Some(handle.as_str()));
        let posts = stub.post_bodies();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert!(
            posts[0]
                .1
                .contains("ssf resuming deliveries to agent on issue:"),
            "{posts:?}"
        );
    }

    /// Telling a harness that is running costs a read of the item, so it
    /// is not tried on every pass while it fails: the attempt is noted
    /// and the next one waits for the backoff.
    #[tokio::test]
    async fn a_started_harness_is_told_once_per_backoff() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        d.with(|s| s.start_error = Some("pi did not become idle in time".into()));
        e.handover("o/r#5", "pi", None, None, Some("half done"), None)
            .await
            .unwrap();
        e.run_handovers(&repo()).await;
        // The pane is there after all, and the item cannot be read: the
        // message it is owed cannot be assembled.
        d.seed("w5", "t9", READY_SCREEN);
        stub.issues.lock().unwrap().remove(&5);
        let _ = (d.log(), d.prompts(), stub.post_bodies(), stub.hits());
        for _ in 0..2 {
            let st = e.entry(&repo(), 5).clone();
            let b = st.blocked.clone().expect("still blocked");
            e.recover(&repo(), 5, &st, b).await;
        }
        let reads = stub
            .hits()
            .into_iter()
            .filter(|h| h.starts_with("/repos/o/r/issues/5"))
            .count();
        assert_eq!(reads, 1, "one attempt, not one per pass");
        assert!(d.prompts().is_empty(), "nothing landed");
        let st = e.entry(&repo(), 5).clone();
        let b = st.blocked.expect("still blocked");
        assert_eq!(b.tell_failures, 1, "the next attempt waits");
        assert!(b.told_at.is_some());
        // The restart backoff is untouched: telling is not a restart.
        assert_eq!(b.retries, 0);
        assert!(b.retried_at.is_none());
        assert!(st.handover_note.is_some(), "the summary is still owed");
    }

    /// The telling waits on its own backoff, not the restart's: a
    /// restart that came back to the prompt a moment ago says nothing
    /// about a person who has just signed in at the pane it left, and
    /// that person is answered on the next pass.
    #[tokio::test]
    async fn a_failed_restart_does_not_hold_up_telling_a_running_harness() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        d.with(|s| s.start_error = Some("pi did not become idle in time".into()));
        e.handover("o/r#5", "pi", None, None, Some("half done"), None)
            .await
            .unwrap();
        e.run_handovers(&repo()).await;
        // A restart was tried a moment ago and got nowhere.
        {
            let b = e.entry(&repo(), 5).blocked.as_mut().unwrap();
            b.retries = 1;
            b.retried_at = Some(now_iso());
        }
        // The pane is there after all, and past any prompt.
        d.seed("w5", "t9", READY_SCREEN);
        let _ = (d.log(), d.prompts(), stub.post_bodies());
        let st = e.entry(&repo(), 5).clone();
        let b = st.blocked.clone().unwrap();
        e.recover(&repo(), 5, &st, b).await;
        let prompts = d.prompts();
        assert_eq!(prompts.len(), 1, "told at once: {prompts:?}");
        assert!(prompts[0].contains("half done"), "{}", prompts[0]);
        let st = e.entry(&repo(), 5).clone();
        assert!(st.blocked.is_none(), "the block is lifted");
        assert!(st.handover_note.is_none(), "read once");
    }

    /// The pane dies as the message goes out and the harness started in
    /// its place comes up at a sign-in screen: what that screen said is
    /// the block from now on, but it is the same hold -- reported once,
    /// held from when it began, with both backoffs where they were.
    #[tokio::test]
    async fn a_message_that_lands_in_a_sign_in_screen_keeps_the_hold_it_had() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        d.with(|s| s.start_error = Some("pi did not become idle in time".into()));
        e.handover("o/r#5", "pi", None, None, Some("half done"), None)
            .await
            .unwrap();
        e.run_handovers(&repo()).await;
        let before = e.entry(&repo(), 5).blocked.clone().expect("blocked");
        assert!(before.reported, "the item was told of the hold");
        // Nothing is live in the workspace by the time the message goes
        // out, and the harness started in its place shows Pi's sign-in
        // prompt.
        d.with(|s| {
            s.live.remove("w5");
            s.relaunch_screen = PI_LOGIN_SCREEN.iter().map(|l| l.to_string()).collect();
        });
        e.tell_a_started_harness(&repo(), 5, &before).await;
        let st = e.entry(&repo(), 5).clone();
        let b = st.blocked.clone().expect("still blocked");
        assert_eq!(b.reason, Blocked::LOGIN, "the fresher answer stands");
        assert!(b.detail.contains("/login"), "{}", b.detail);
        assert_eq!(b.since, before.since, "the same hold, from when it began");
        assert!(b.reported, "and the item is not told of it twice");
        assert_eq!(b.tell_failures, 1, "the message did not land");
        assert!(st.handover_note.is_some(), "the summary is still owed");
        // The pass that follows finds it reported: no second `blocked`.
        e.recover(&repo(), 5, &st, b).await;
        let posts = stub.post_bodies();
        let blocked = posts
            .iter()
            .filter(|(_, body)| body.contains("event=blocked"))
            .count();
        assert_eq!(blocked, 1, "one hold, one post: {posts:?}");
    }

    /// A second handover that brings no summary of its own does not
    /// destroy the one still waiting: the session that wrote it is long
    /// gone, and the harness starting now is the first that can act on
    /// it. One that does bring a summary replaces it (the newer account
    /// of where the item stands), which is what the test above shows.
    #[tokio::test]
    async fn a_second_handover_without_a_summary_keeps_the_one_still_owed() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        d.with(|s| s.start_error = Some("pi exited at once".into()));
        e.handover("o/r#5", "pi", None, None, Some("half migrated"), None)
            .await
            .unwrap();
        e.run_handovers(&repo()).await;
        assert!(
            e.entry(&repo(), 5).handover_note.is_some(),
            "nobody read it"
        );
        let _ = (d.log(), d.prompts(), stub.post_bodies());
        // Handed on again with nothing to add: Codex still has to be
        // told what the session that did the work left.
        e.handover("o/r#5", "codex", None, None, None, None)
            .await
            .unwrap();
        e.run_handovers(&repo()).await;
        let prompts = d.prompts();
        assert!(
            prompts[0].starts_with("You took over this issue from a session on Claude Code"),
            "{}",
            prompts[0]
        );
        assert!(prompts[0].contains("half migrated"), "{}", prompts[0]);
        assert!(
            e.entry(&repo(), 5).handover_note.is_none(),
            "read at last, so nothing is owed"
        );
        // The post says what the new session was given, not what the
        // command carried.
        let posts = stub.post_bodies();
        let handed = posts
            .iter()
            .find(|(_, b)| b.contains("event=handed-over"))
            .expect("the handover is posted");
        assert!(handed.1.contains("\nsummary: yes\n"), "{}", handed.1);
    }

    /// A second handover on an item whose first one never ran: the words
    /// the new session is given still name the session that did the work,
    /// not the harness that failed to come up.
    #[tokio::test]
    async fn a_second_handover_names_the_session_that_did_the_work() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let (mut e, d) = handover_setup(&stub);
        d.with(|s| s.start_error = Some("pi exited at once".into()));
        e.handover("o/r#5", "pi", None, None, Some("half migrated"), None)
            .await
            .unwrap();
        e.run_handovers(&repo()).await;
        assert_eq!(
            e.entry(&repo(), 5)
                .handover_note
                .as_ref()
                .map(|n| n.from.as_str()),
            Some("Claude Code")
        );
        let _ = (d.log(), d.prompts(), stub.post_bodies());
        e.handover(
            "o/r#5",
            "codex",
            None,
            None,
            Some("still half migrated"),
            None,
        )
        .await
        .unwrap();
        e.run_handovers(&repo()).await;
        let st = e.entry(&repo(), 5).clone();
        assert!(st.handover_note.is_none(), "Codex took it on");
        let prompts = d.prompts();
        assert!(
            prompts[0].starts_with("You took over this issue from a session on Claude Code"),
            "{}",
            prompts[0]
        );
        assert!(prompts[0].contains("still half migrated"), "{}", prompts[0]);
        // The post says what the item was configured on all the same.
        let posts = stub.post_bodies();
        let handed = posts
            .iter()
            .find(|(_, b)| b.contains("event=handed-over"))
            .expect("the handover is posted");
        assert!(handed.1.contains("\nfrom: Pi\n"), "{posts:?}");
        assert!(handed.1.contains("\nto: Codex\n"), "{posts:?}");
    }

    /// What a handover retires: the conversation on the record and the
    /// last one its harness wrote in the workspace. The second covers the
    /// session ssf never captured an id for, and the transcript flushed
    /// on the way out inside the second `handed_over_at` is stamped to.
    #[test]
    fn a_handover_retires_the_workspace_s_last_conversation_too() {
        let id = |s: &str| Some(s.to_string());
        // Both known and different: both go.
        assert_eq!(
            retired_conversations(id("sess-5"), id("sess-6")),
            vec!["sess-5".to_string(), "sess-6".to_string()]
        );
        // The usual case: the record's id is the newest transcript.
        assert_eq!(
            retired_conversations(id("sess-5"), id("sess-5")),
            vec!["sess-5".to_string()]
        );
        // Never captured: the transcript alone is what there is to skip.
        assert_eq!(
            retired_conversations(None, id("sess-6")),
            vec!["sess-6".to_string()]
        );
        // A harness that keeps no transcripts (or an empty workspace):
        // nothing but the record's id.
        assert_eq!(
            retired_conversations(id("sess-5"), None),
            vec!["sess-5".to_string()]
        );
        assert!(retired_conversations(None, None).is_empty());
        // On the record, both are remembered and neither twice.
        let mut st = IssueState::default();
        retire(&mut st, &retired_conversations(id("sess-5"), id("sess-6")));
        retire(&mut st, &retired_conversations(id("sess-6"), None));
        assert_eq!(st.retired_session_ids, vec!["sess-5", "sess-6"]);
    }

    /// Where `capture_sessions` starts looking for a transcript: a moment
    /// before the launch, but never back past a handover, whose outgoing
    /// agent wrote its own transcript in those same seconds.
    #[test]
    fn the_capture_window_never_reaches_back_past_a_handover() {
        let at = |s: &str| SystemTime::from(chrono::DateTime::parse_from_rfc3339(s).unwrap());
        let launched = "2026-09-07T12:00:30Z";
        // No handover: the slack stands.
        assert_eq!(
            capture_since(launched, None),
            at("2026-09-07T12:00:25Z"),
            "five seconds of slack"
        );
        // The handover is inside the slack: the window starts there.
        assert_eq!(
            capture_since(launched, Some("2026-09-07T12:00:28Z")),
            at("2026-09-07T12:00:28Z")
        );
        // A later relaunch is long past it: the slack stands again.
        assert_eq!(
            capture_since("2026-09-07T13:00:30Z", Some("2026-09-07T12:00:28Z")),
            at("2026-09-07T13:00:25Z")
        );
        // Nothing readable: everything is too new to adopt.
        assert_eq!(capture_since("not a time", None), SystemTime::UNIX_EPOCH);
    }

    fn assigned_item(number: u64, author: &str, updated_at: &str) -> Value {
        json!({
            "number": number, "title": "t", "body": "do it", "html_url": format!("https://gh/{number}"),
            "state": "open", "user": {"login": author}, "assignees": [{"login": "bot"}],
            "created_at": "x", "updated_at": updated_at
        })
    }

    fn assigned_by(id: u64, who: &str) -> Value {
        json!({"event":"assigned","id":id,"actor":{"login":who},"assignee":{"login":"bot"},"created_at":"t"})
    }

    #[test]
    fn events_by_unlisted_users_are_not_delivered() {
        let mut e = engine();
        e.cfg.daemon.allowed_users = Some(vec!["alice".into()]);
        let r = repo();
        e.cfg.repos.push(r.clone());
        let timeline = vec![
            comment(1, "alice", "hi"),
            comment(2, "Mallory", "evil"),
            comment(3, "bot", "<!-- ssf: origin=o/r#1 -->\n\nfrom one"),
            comment(4, "bot", "typed as the bot"),
            json!({"event":"labeled","id":5,"actor":{"login":"mallory"},"label":{"name":"review"},"created_at":"t"}),
            json!({"event":"labeled","id":6,"actor":{"login":"ALICE"},"label":{"name":"bug"},"created_at":"t"}),
            json!({"event":"committed","sha":"abc123def","author":{"name":"Mallory","date":"t"},"message":"m"}),
            json!({"event":"reviewed","id":8,"user":{"login":"mallory"},"state":"approved","body":"lgtm","created_at":"t"}),
            json!({"event":"line-commented","id":9,"comments":[
                {"id":91,"user":{"login":"mallory"},"body":"x","path":"a","line":1,"created_at":"t"},
                {"id":92,"user":{"login":"alice"},"body":"y","path":"a","line":2,"created_at":"t"}]}),
            json!({"event":"commented","id":10,"user":{"login":"github-project-automation[bot]"},"body":"moved","created_at":"t"}),
            json!({"event":"line-commented","id":11,"comments":[
                {"id":93,"user":{"login":"mallory"},"body":"z","path":"a","line":1,"created_at":"t"}]}),
        ];
        let keys = |d: &Diff| d.rendered.iter().map(|r| r.key.clone()).collect::<Vec<_>>();
        let d = e.diff(&r, &BTreeMap::new(), &timeline);
        assert_eq!(
            keys(&d),
            vec![
                "commented:1",
                "commented:3",
                "commented:4",
                "labeled:6",
                "committed:abc123def",
                "line-commented:9"
            ]
        );
        // Only alice's line comment is rendered out of the batch.
        let batch = &d.rendered[5];
        assert!(
            batch.text.contains("@alice") && !batch.text.contains("mallory"),
            "{}",
            batch.text
        );
        // Everything is still counted as seen, so nothing dropped comes
        // back as news later, and each drop is remembered so it is logged
        // once (info) however often the timeline is walked again.
        assert_eq!(d.seen.len(), 11);
        assert!(e.dropped_logged.lock().unwrap().contains("o/r:commented:2"));
        assert_eq!(e.dropped_logged.lock().unwrap().len(), 6);
        // A repository list replaces the instance list.
        e.cfg.repos[0].allowed_users = Some(vec!["MALLORY".into()]);
        let r = e.cfg.repos[0].clone();
        let d = e.diff(&r, &BTreeMap::new(), &timeline);
        assert_eq!(
            keys(&d),
            vec![
                "commented:2",
                "commented:3",
                "commented:4",
                "labeled:5",
                "committed:abc123def",
                "reviewed:8",
                "line-commented:9",
                "line-commented:11"
            ]
        );
        // The wildcard delivers everything, bot accounts included.
        e.cfg.repos[0].allowed_users = Some(vec!["*".into()]);
        let r = e.cfg.repos[0].clone();
        assert_eq!(e.diff(&r, &BTreeMap::new(), &timeline).rendered.len(), 11);
        // No list at all and no collaborators fetched yet: nobody but the bot.
        e.cfg.repos[0].allowed_users = None;
        e.cfg.daemon.allowed_users = None;
        let r = e.cfg.repos[0].clone();
        assert_eq!(
            keys(&e.diff(&r, &BTreeMap::new(), &timeline)),
            vec!["commented:3", "commented:4", "committed:abc123def"]
        );
        assert_eq!(e.allow_list(&r).source, Source::Collaborators);
    }

    #[tokio::test]
    async fn items_asked_for_by_unlisted_users_are_ignored_until_an_allowed_user_asks() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        e.cfg.daemon.allowed_users = Some(vec!["Alice".into()]);
        let r = repo();
        e.cfg.repos.push(r.clone());
        stub.set_assigned(vec![assigned_item(5, "mallory", "u1")]);
        stub.set_timeline(5, vec![assigned_by(1, "mallory")]);
        e.tick_repo(&r).await.unwrap();
        let rs = e.state.repos.get("o/r").unwrap();
        assert_eq!(
            rs.ignored.get(&5).map(|i| i.updated_at.as_str()),
            Some("u1")
        );
        assert!(rs.issues.get(&5).is_none_or(|s| !s.seeded), "no session");
        assert!(e.failures.is_empty(), "a refusal is not a failure");
        assert!(stub.hits().iter().any(|h| h.contains("/issues/5/timeline")));
        // Unchanged: not read again.
        e.tick_repo(&r).await.unwrap();
        assert!(!stub.hits().iter().any(|h| h.contains("/issues/5/")));
        // Alice assigns the bot herself: the item changed, and now it is
        // taken on. The driver is not there in this test, so onboarding
        // fails after the gate, which is the point: the ignore record is
        // gone and a delivery failure is counted instead.
        stub.set_assigned(vec![assigned_item(5, "mallory", "u2")]);
        stub.set_timeline(5, vec![assigned_by(1, "mallory"), assigned_by(2, "alice")]);
        e.tick_repo(&r).await.unwrap();
        let rs = e.state.repos.get("o/r").unwrap();
        assert!(!rs.ignored.contains_key(&5));
        assert_eq!(e.failures.get(&("o/r".to_string(), 5)), Some(&1));
    }

    #[tokio::test]
    async fn a_retired_item_assigned_again_by_an_unlisted_user_stays_retired() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        e.cfg.daemon.allowed_users = Some(vec!["alice".into()]);
        let r = repo();
        e.cfg.repos.push(r.clone());
        seeded(&mut e, 5, Some("bot/issue-5"), false);
        e.entry(&r, 5).triggers = vec!["assigned".into()];
        stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
        stub.set_timeline(5, vec![assigned_by(1, "alice"), assigned_by(2, "mallory")]);
        e.tick_repo(&r).await.unwrap();
        assert!(!e.peek(&r, 5).unwrap().active);
        assert!(e.state.repos["o/r"].ignored.contains_key(&5));
        assert!(e.failures.is_empty());
    }

    #[tokio::test]
    async fn the_startup_pass_knows_the_collaborators_before_it_resumes_anything() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        e.cfg.daemon.allowed_users = None;
        e.cfg.daemon.accepted_anyone_risk = false;
        let r = repo();
        e.cfg.repos.push(r.clone());
        seeded(&mut e, 1, Some("bot/issue-1"), true);
        e.entry(&r, 1).worktree_id = Some("wt1".into());
        // No answer from GitHub: the repository is skipped, nothing is
        // resumed on a guess.
        e.resume_interrupted(&[DriverKind::Orca]).await;
        assert!(stub.hits().iter().any(|h| h.contains("/collaborators")));
        assert!(!e.allow_list(&r).allows("alice"));
        // With an answer, the list is in place before any session is
        // looked at (the driver is not there in this test, so the session
        // itself is left alone after that).
        stub.set_collaborators(Some(vec![
            json!({"login": "alice", "permissions": {"push": true}}),
        ]));
        e.resume_interrupted(&[DriverKind::Orca]).await;
        assert!(e.allow_list(&r).allows("alice"));
        assert_eq!(e.allow_list(&r).source, Source::Collaborators);
    }

    #[tokio::test]
    async fn collaborators_with_push_access_are_the_default_list() {
        let stub = GitHubStub::start().await;
        let mut e = engine_at(&stub.base);
        e.cfg.daemon.allowed_users = None;
        e.cfg.daemon.accepted_anyone_risk = false;
        let r = repo();
        e.cfg.repos.push(r.clone());
        // No list and no answer from GitHub: the pass fails, closed.
        let err = e.tick_repo(&r).await.unwrap_err();
        assert!(format!("{err:#}").contains("collaborators"), "{err:#}");
        assert!(!e.allow_list(&r).allows("alice"));
        stub.set_collaborators(Some(vec![
            json!({"login": "Alice", "permissions": {"pull": true, "push": true}}),
            json!({"login": "reader", "permissions": {"pull": true, "push": false}}),
            json!({"login": "some-app[bot]", "permissions": {"push": true}}),
        ]));
        e.tick_repo(&r).await.unwrap();
        let l = e.allow_list(&r);
        assert!(l.allows("alice") && l.allows("ALICE") && l.allows("bot"));
        assert!(!l.allows("reader") && !l.allows("some-app[bot]"));
        assert_eq!(l.source, Source::Collaborators);
        assert_eq!(l.describe(), "@alice (collaborators with push access)");
        // Once per pass, conditionally: the second answer is a 304.
        stub.hits();
        e.tick_repo(&r).await.unwrap();
        let hits = stub.hits();
        assert_eq!(
            hits.iter().filter(|h| h.contains("/collaborators")).count(),
            1
        );
        assert!(e.allow_list(&r).allows("alice"));
        // A refresh that fails keeps the last list.
        stub.set_collaborators(None);
        e.tick_repo(&r).await.unwrap();
        assert!(e.allow_list(&r).allows("alice"));
        // A configured list is used instead, and nothing is fetched.
        e.cfg.repos[0].allowed_users = Some(vec![]);
        let r = e.cfg.repos[0].clone();
        stub.hits();
        e.tick_repo(&r).await.unwrap();
        assert!(!stub.hits().iter().any(|h| h.contains("/collaborators")));
        assert!(!e.allow_list(&r).allows("alice"));
        assert_eq!(e.allow_list(&r).source, Source::Repo);
    }

    /// A stamp far enough in the past that the paced re-check runs, while
    /// still leaving a hold for the retirement to clear.
    const EXPIRED: &str = "2026-01-01T00:00:00Z";

    /// Replays issue #137: the mentioned listing came back without an item
    /// whose mention was still sitting in the issue, so the session was
    /// told to stop and reattached on the next pass, over and over.
    /// Retirement re-reads the item now, so a listing that loses it
    /// changes nothing while the mention is there, and the session still
    /// retires once the mention has really gone.
    #[tokio::test]
    async fn a_mention_still_in_the_item_holds_the_session_through_an_empty_listing() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let r = repo();
        let item = |body: &str| {
            json!({
                "number": 5, "title": "t", "body": body, "html_url": "https://gh/5",
                "state": "open", "user": {"login": "alice"},
                "created_at": "x", "updated_at": "u1"
            })
        };
        let comment = |body: &str| {
            json!({
                "event": "commented", "body": body, "html_url": "https://gh/5#c1",
                "updated_at": "u2", "actor": {"login": "alice"}, "user": {"login": "alice"}
            })
        };
        // The stub's mentioned listing is always empty, which is the
        // listing that retired this session on the live factory.
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.repos = vec![r.clone()];
        seeded(&mut e, 5, Some("bot/issue-5"), true);
        {
            let st = e.entry(&r, 5);
            st.triggers = vec!["mentioned".into()];
            st.worktree_id = Some("w5".into());
            st.worktree_path = Some("/w/5".into());
            st.terminal_handle = Some("t5".into());
        }
        d.seed("w5", "t5", READY_SCREEN);

        // The mention is in the item's body: the session stays.
        stub.set_issue(5, item("please look @bot"));
        stub.set_timeline(5, vec![]);
        e.tick_repo(&r).await.unwrap();
        assert!(
            e.entry(&r, 5).active,
            "retired although the body still mentions the bot"
        );

        assert!(
            e.entry(&r, 5).retirement_held_at.is_some(),
            "the hold was not recorded"
        );
        assert!(
            e.entry(&r, 5).retirement_announced,
            "the incident was not announced"
        );

        // While that hold is fresh the timeline is not walked again: the
        // listing is wrong and stays wrong, and the walk is the expensive
        // part. The item itself is still read every pass, so a close is
        // still noticed at once.
        stub.hits();
        e.tick_repo(&r).await.unwrap();
        let paths: Vec<String> = stub.hits();
        assert!(
            !paths.iter().any(|p| p.contains("/timeline")),
            "the timeline was walked inside the hold: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p == "/repos/o/r/issues/5"),
            "the item itself was not read inside the hold: {paths:?}"
        );

        // Once the interval has passed the item is read again. This time
        // the mention is in a review comment, one level down in the
        // timeline the way GitHub reports a batch of them.
        e.entry(&r, 5).retirement_held_at = Some(EXPIRED.into());
        stub.set_issue(5, item("nothing to see"));
        stub.set_timeline(
            5,
            vec![json!({
                "event": "line-commented",
                "comments": [{"body": "@bot what do you think?", "user": {"login": "alice"}}]
            })],
        );
        e.tick_repo(&r).await.unwrap();
        assert!(
            e.entry(&r, 5).active,
            "retired although a review comment still mentions the bot"
        );

        // A near miss is not a mention, so this one does retire.
        e.entry(&r, 5).retirement_held_at = Some(EXPIRED.into());
        stub.set_timeline(5, vec![comment("ask @bot-2, not this one")]);
        e.tick_repo(&r).await.unwrap();
        assert!(
            !e.entry(&r, 5).active,
            "kept although nothing mentions the bot any more"
        );
        assert!(
            e.entry(&r, 5).retirement_held_at.is_none(),
            "the hold outlived the retirement"
        );
        assert!(
            !e.entry(&r, 5).retirement_announced,
            "the next incident would announce itself as an old one"
        );
    }

    /// Only the paced arm keeps the bookkeeping. An item held on something
    /// read straight off it clears the hold, so an assignment cannot
    /// suppress a mention re-check that has never run.
    #[tokio::test]
    async fn a_hold_on_the_item_itself_leaves_no_pacing_behind() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let r = repo();
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.repos = vec![r.clone()];
        seeded(&mut e, 5, Some("bot/issue-5"), true);
        {
            let st = e.entry(&r, 5);
            st.triggers = vec!["assigned".into(), "mentioned".into()];
            st.worktree_id = Some("w5".into());
            st.worktree_path = Some("/w/5".into());
            st.terminal_handle = Some("t5".into());
            // A hold left by an earlier pass.
            st.retirement_held_at = Some(now_iso());
        }
        d.seed("w5", "t5", READY_SCREEN);
        // Still assigned, so the item itself answers and no walk is paced.
        stub.set_issue(
            5,
            json!({
                "number": 5, "title": "t", "body": "no mention here",
                "html_url": "https://gh/5", "state": "open", "user": {"login": "alice"},
                "assignees": [{"login": "bot"}],
                "created_at": "x", "updated_at": "u1"
            }),
        );
        stub.set_timeline(5, vec![]);
        e.tick_repo(&r).await.unwrap();
        assert!(e.entry(&r, 5).active, "an assigned item was retired");
        assert!(
            e.entry(&r, 5).retirement_held_at.is_none(),
            "an assignment left a stamp pacing a walk it never made"
        );

        // So the moment the assignment goes, the mention is re-checked at
        // once rather than waiting out a stamp it never earned.
        stub.set_issue(
            5,
            json!({
                "number": 5, "title": "t", "body": "no mention here",
                "html_url": "https://gh/5", "state": "open", "user": {"login": "alice"},
                "created_at": "x", "updated_at": "u1"
            }),
        );
        stub.hits();
        e.tick_repo(&r).await.unwrap();
        let paths = stub.hits();
        assert!(
            paths.iter().any(|p| p.contains("/issues/5/timeline")),
            "the mention was not re-checked once the assignment went: {paths:?}"
        );
        assert!(
            !e.entry(&r, 5).active,
            "nothing named the bot, so it retires"
        );
    }

    /// A hold paces the timeline walk and nothing else. A close is still
    /// noticed on the next pass, an expired hold reads the item again, and
    /// a stamp ahead of the clock counts as expired rather than holding
    /// until wall-clock catches up.
    #[tokio::test]
    async fn a_hold_paces_the_re_read_without_delaying_a_close_or_outliving_the_clock() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let r = repo();
        let item = |state: &str| {
            json!({
                "number": 5, "title": "t", "body": "no mention here",
                "html_url": "https://gh/5", "state": state, "user": {"login": "alice"},
                "created_at": "x", "updated_at": "u1"
            })
        };
        let held_session = |e: &mut Engine, at: &str| {
            seeded(e, 5, Some("bot/issue-5"), true);
            let st = e.entry(&repo(), 5);
            st.triggers = vec!["mentioned".into()];
            st.worktree_id = Some("w5".into());
            st.worktree_path = Some("/w/5".into());
            st.terminal_handle = Some("t5".into());
            st.retirement_held_at = Some(at.to_string());
        };

        // A closed item retires on the next pass, hold or no hold: the
        // hold only ever paces the mention re-check, which a closed item
        // never reaches.
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.repos = vec![r.clone()];
        held_session(&mut e, &now_iso());
        d.seed("w5", "t5", READY_SCREEN);
        stub.set_issue(5, item("closed"));
        stub.set_timeline(5, vec![]);
        e.tick_repo(&r).await.unwrap();
        assert!(
            !e.entry(&r, 5).active,
            "a closed item waited for the hold to expire"
        );

        // An expired hold reads the item again and retires it.
        let mut e = engine_at(&stub.base);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.repos = vec![r.clone()];
        held_session(&mut e, "2026-01-01T00:00:00Z");
        stub.set_issue(5, item("open"));
        e.tick_repo(&r).await.unwrap();
        assert!(!e.entry(&r, 5).active, "an expired hold went on holding");

        // So does one stamped ahead of the clock, which would otherwise
        // read as fresh until the clock caught up with it.
        let mut e = engine_at(&stub.base);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.repos = vec![r.clone()];
        held_session(&mut e, "2099-01-01T00:00:00Z");
        e.tick_repo(&r).await.unwrap();
        assert!(
            !e.entry(&r, 5).active,
            "a hold stamped in the future held forever"
        );
    }

    /// An item back on a listing settles whatever a hiccup held, so a
    /// later hold is a new incident rather than a stamp that never moves.
    #[tokio::test]
    async fn an_item_back_on_a_listing_clears_the_hold_it_left_behind() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let r = repo();
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.repos = vec![r.clone()];
        seeded(&mut e, 5, Some("bot/issue-5"), true);
        {
            let st = e.entry(&r, 5);
            st.triggers = vec!["assigned".into()];
            st.worktree_id = Some("w5".into());
            st.worktree_path = Some("/w/5".into());
            st.terminal_handle = Some("t5".into());
            // The same `updated_at` the listing reports, so the pass has
            // no reason to look at the item: the hold must still clear.
            st.updated_at = Some("u2".into());
            st.retirement_held_at = Some(now_iso());
        }
        d.seed("w5", "t5", READY_SCREEN);
        stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
        stub.set_timeline(5, vec![]);
        e.tick_repo(&r).await.unwrap();
        assert!(
            e.entry(&r, 5).retirement_held_at.is_none(),
            "the hold survived the item coming back onto a listing"
        );
    }

    /// The review-request arm of the same guard, and what a failed
    /// re-check does: a fetch that says nothing holds the retirement,
    /// because retiring is the destructive reading of missing evidence.
    #[tokio::test]
    async fn a_review_request_still_on_the_pull_request_holds_the_session() {
        let _sandbox = crate::config::test_support::sandbox();
        let stub = GitHubStub::start().await;
        let r = repo();
        stub.set_issue(
            7,
            json!({
                "number": 7, "title": "t", "body": "no mention here",
                "html_url": "https://gh/7", "state": "open", "user": {"login": "alice"},
                "pull_request": {"url": "https://gh/pulls/7"},
                "created_at": "x", "updated_at": "u1"
            }),
        );
        let mut e = engine_at(&stub.base);
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.repos = vec![r.clone()];
        seeded(&mut e, 7, Some("bot/issue-7"), true);
        {
            let st = e.entry(&r, 7);
            st.triggers = vec!["review_requested".into()];
            st.worktree_id = Some("w7".into());
            st.worktree_path = Some("/w/7".into());
            st.terminal_handle = Some("t7".into());
        }
        d.seed("w7", "t7", READY_SCREEN);

        // The pull request was never registered, so fetching it fails.
        // Nothing is known either way, so the session is kept.
        e.tick_repo(&r).await.unwrap();
        assert!(
            e.entry(&r, 7).active,
            "retired on a re-check that could not be made"
        );

        // It still asks the bot for a review: kept, and for a good reason.
        e.entry(&r, 7).retirement_held_at = Some(EXPIRED.into());
        stub.set_pull(
            7,
            json!({
                "head": {"ref": "b", "repo": {"full_name": "o/r"}},
                "base": {"ref": "main"},
                "requested_reviewers": [{"login": "Bot"}]
            }),
        );
        e.tick_repo(&r).await.unwrap();
        assert!(e.entry(&r, 7).active, "retired although the review stands");

        // The request has been withdrawn: now it retires.
        e.entry(&r, 7).retirement_held_at = Some(EXPIRED.into());
        stub.set_pull(
            7,
            json!({
                "head": {"ref": "b", "repo": {"full_name": "o/r"}},
                "base": {"ref": "main"},
                "requested_reviewers": []
            }),
        );
        e.tick_repo(&r).await.unwrap();
        assert!(
            !e.entry(&r, 7).active,
            "kept although the review request is gone"
        );
    }

    /// The refusal names a remedy that fits the item. Unassigning helps
    /// only an item that is assigned, and a mention cannot be withdrawn.
    #[test]
    fn the_release_refusal_follows_the_trigger_that_holds_the_item() {
        let t = |s: &str| vec![s.to_string()];
        assert_eq!(why_active(&t("assigned")).1, "Close or unassign the item");
        assert!(why_active(&t("mentioned")).0.contains("mentions the bot"));
        assert_eq!(why_active(&t("mentioned")).1, "Close the item");
        assert!(why_active(&t("review_requested")).0.contains("review"));
        assert!(why_active(&t("created")).0.contains("opened by the bot"));
        // An assignment is the clearest thing to act on, so it wins.
        assert_eq!(
            why_active(&["mentioned".to_string(), "assigned".to_string()]).1,
            "Close or unassign the item"
        );
        assert_eq!(why_active(&[]).1, "Close the item");
    }

    #[tokio::test]
    async fn conflict_simulation_reports_paths_without_touching_the_worktree() {
        use crate::release::testkit::{scratch, sh};

        let s = scratch("conflict-merge-tree").await;
        let (path, branch) = crate::driver::add_local_worktree(&s.work, "issue-1-conflict", None)
            .await
            .unwrap();
        std::fs::write(std::path::Path::new(&path).join("a.txt"), "feature\n").unwrap();
        sh(&path, &["add", "a.txt"]).await;
        sh(&path, &["commit", "-q", "-m", "feature"]).await;
        std::fs::write(std::path::Path::new(&s.work).join("a.txt"), "base\n").unwrap();
        sh(&s.work, &["add", "a.txt"]).await;
        sh(&s.work, &["commit", "-q", "-m", "base"]).await;
        sh(&s.work, &["push", "-q", "origin", "main"]).await;
        let base = conflict_git(&s.work, &["rev-parse", "refs/remotes/origin/main"])
            .await
            .unwrap();
        let head = conflict_git(&path, &["rev-parse", "HEAD"]).await.unwrap();
        let before = std::fs::read(std::path::Path::new(&path).join("a.txt")).unwrap();
        let (conflict, files) = engine()
            .simulate_conflict(&s.work, &base, &head)
            .await
            .unwrap();
        assert!(conflict);
        assert_eq!(files, vec!["a.txt"]);
        assert_eq!(
            std::fs::read(std::path::Path::new(&path).join("a.txt")).unwrap(),
            before,
            "merge-tree must not change the agent worktree"
        );
        assert_eq!(branch, "refs/heads/bot/issue-1-conflict");

        // A modify/delete conflict has no "Merge conflict in" prose, so the
        // NUL-delimited name section is the source of truth for it too.
        let s = scratch("conflict-modify-delete").await;
        let (path, _) = crate::driver::add_local_worktree(&s.work, "issue-4-conflict", None)
            .await
            .unwrap();
        std::fs::write(std::path::Path::new(&path).join("a.txt"), "feature\n").unwrap();
        sh(&path, &["add", "a.txt"]).await;
        sh(&path, &["commit", "-q", "-m", "feature"]).await;
        sh(&s.work, &["rm", "-q", "a.txt"]).await;
        sh(&s.work, &["commit", "-q", "-m", "delete"]).await;
        sh(&s.work, &["push", "-q", "origin", "main"]).await;
        let base = conflict_git(&s.work, &["rev-parse", "refs/remotes/origin/main"])
            .await
            .unwrap();
        let head = conflict_git(&path, &["rev-parse", "HEAD"]).await.unwrap();
        let (conflict, files) = engine()
            .simulate_conflict(&s.work, &base, &head)
            .await
            .unwrap();
        assert!(conflict);
        assert_eq!(files, vec!["a.txt"]);
    }

    #[tokio::test]
    async fn conflict_simulation_allows_clean_divergence() {
        use crate::release::testkit::{scratch, sh};

        let s = scratch("clean-merge-tree").await;
        let (path, _) = crate::driver::add_local_worktree(&s.work, "issue-3-clean", None)
            .await
            .unwrap();
        std::fs::write(std::path::Path::new(&path).join("feature.txt"), "feature\n").unwrap();
        sh(&path, &["add", "feature.txt"]).await;
        sh(&path, &["commit", "-q", "-m", "feature"]).await;
        std::fs::write(std::path::Path::new(&s.work).join("base.txt"), "base\n").unwrap();
        sh(&s.work, &["add", "base.txt"]).await;
        sh(&s.work, &["commit", "-q", "-m", "base"]).await;
        sh(&s.work, &["push", "-q", "origin", "main"]).await;
        let base = conflict_git(&s.work, &["rev-parse", "refs/remotes/origin/main"])
            .await
            .unwrap();
        let head = conflict_git(&path, &["rev-parse", "HEAD"]).await.unwrap();
        assert_eq!(
            engine()
                .simulate_conflict(&s.work, &base, &head)
                .await
                .unwrap(),
            (false, Vec::new())
        );
    }

    #[tokio::test]
    async fn conflict_check_notifies_once_and_guards_stale_branch_state() {
        use crate::release::testkit::{scratch, sh};

        let s = scratch("conflict-notice").await;
        let (path, branch) = crate::driver::add_local_worktree(&s.work, "issue-2-conflict", None)
            .await
            .unwrap();
        std::fs::write(std::path::Path::new(&path).join("a.txt"), "feature\n").unwrap();
        sh(&path, &["add", "a.txt"]).await;
        sh(&path, &["commit", "-q", "-m", "feature"]).await;
        std::fs::write(std::path::Path::new(&s.work).join("a.txt"), "base\n").unwrap();
        sh(&s.work, &["add", "a.txt"]).await;
        sh(&s.work, &["commit", "-q", "-m", "base"]).await;
        sh(&s.work, &["push", "-q", "origin", "main"]).await;

        let mut e = engine();
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.daemon.conflict_check_interval_secs = 1;
        let mut r = repo();
        r.path = Some(s.work.clone());
        d.seed("w2", "t2", READY_SCREEN);
        {
            let st = e.entry(&r, 2);
            st.html_url = "https://gh/2".into();
            st.seeded = true;
            st.active = true;
            st.worktree_id = Some("w2".into());
            st.worktree_path = Some(path.clone());
            st.repo_id = Some(s.work.clone());
            st.branch = Some(branch);
        }

        e.check_conflicts(&r).await.unwrap();
        let first = d.prompts();
        assert_eq!(first.len(), 1);
        assert!(first[0].contains("origin/main"));
        assert!(first[0].contains("a.txt"));
        assert!(first[0].contains("rebase"));
        assert!(e.entry(&r, 2).conflict_notice.is_some());

        // The interval is deliberately bypassed here to exercise persisted
        // pair deduplication as each daemon pass would see it.
        e.conflict_checks.clear();
        e.check_conflicts(&r).await.unwrap();
        assert!(d.prompts().is_empty(), "the same divergence was repeated");

        // A stale state branch must never make the daemon inspect or notify
        // about a different branch checked out in the agent worktree.
        sh(&path, &["checkout", "-q", "-b", "other"]).await;
        e.conflict_checks.clear();
        e.check_conflicts(&r).await.unwrap();
        assert!(d.prompts().is_empty(), "stale branch state caused a notice");
    }

    #[tokio::test]
    async fn failed_conflict_delivery_is_retried() {
        let (mut e, r, d, _scratch, _path) = conflict_fixture("conflict-retry", 5).await;
        d.with(|s| {
            s.deliver_error = Some("delivery failed".into());
        });

        e.check_conflicts(&r).await.unwrap();
        assert!(e.entry(&r, 5).conflict_notice.is_none());
        let _ = d.prompts();

        // Once delivery is available again, the same pair must be delivered
        // and recorded.
        e.conflict_checks.clear();
        e.check_conflicts(&r).await.unwrap();
        assert!(e.entry(&r, 5).conflict_notice.is_some());
        assert_eq!(d.prompts().len(), 1);
    }

    #[tokio::test]
    async fn conflict_notice_survives_reload_even_with_an_empty_merge_cache() {
        let (mut e, r, d, scratch, path) = conflict_fixture("conflict-reload", 6).await;
        e.check_conflicts(&r).await.unwrap();
        assert!(e.entry(&r, 6).conflict_notice.is_some());
        let _ = d.prompts();

        let saved = serde_json::to_string(&e.state).unwrap();
        let mut restarted = engine();
        restarted.cfg.daemon.conflict_check_interval_secs = 1;
        let d2 = crate::driver::StubDriver::new(DriverKind::Orca);
        restarted.drivers = Drivers::from_list(vec![Driver::Stub(d2.clone())]);
        d2.seed("w6", "t6", READY_SCREEN);
        restarted.state = serde_json::from_str(&saved).unwrap();
        assert!(restarted.conflict_pairs.is_empty());
        restarted.check_conflicts(&r).await.unwrap();
        assert!(d2.prompts().is_empty());
        assert_eq!(
            restarted.entry(&r, 6).worktree_path.as_deref(),
            Some(path.as_str())
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn changed_branch_or_base_commit_is_a_new_conflict_notice() {
        use crate::release::testkit::sh;

        let (mut e, mut r, d, scratch, path) = conflict_fixture("conflict-changed", 7).await;
        r.base_branch = Some("origin/HEAD".into());
        e.check_conflicts(&r).await.unwrap();
        let _ = d.prompts();

        std::fs::write(std::path::Path::new(&path).join("extra.txt"), "extra\n").unwrap();
        sh(&path, &["add", "extra.txt"]).await;
        sh(&path, &["commit", "-q", "-m", "extra"]).await;
        e.conflict_checks.clear();
        e.check_conflicts(&r).await.unwrap();
        assert_eq!(d.prompts().len(), 1, "changed branch SHA was not noticed");

        std::fs::write(
            std::path::Path::new(&scratch.work).join("base-extra.txt"),
            "base\n",
        )
        .unwrap();
        sh(&scratch.work, &["add", "base-extra.txt"]).await;
        sh(&scratch.work, &["commit", "-q", "-m", "base-extra"]).await;
        sh(&scratch.work, &["push", "-q", "origin", "main"]).await;
        e.conflict_checks.clear();
        e.check_conflicts(&r).await.unwrap();
        assert_eq!(d.prompts().len(), 1, "changed base SHA was not noticed");
    }

    #[tokio::test]
    async fn inactive_session_states_do_not_fetch_or_notify() {
        let mut e = engine();
        let d = crate::driver::StubDriver::new(DriverKind::Orca);
        e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
        e.cfg.daemon.conflict_check_interval_secs = 1;
        let mut r = repo();
        r.path = Some("/no/such/checkout".into());
        for number in 10..=15 {
            d.seed(&format!("w{number}"), &format!("t{number}"), READY_SCREEN);
            let st = e.entry(&r, number);
            st.seeded = true;
            st.active = true;
            st.worktree_id = Some(format!("w{number}"));
            st.worktree_path = Some("/no/such/worktree".into());
            st.branch = Some(format!("refs/heads/bot/issue-{number}"));
        }
        e.entry(&r, 10).retired_at = Some(now_iso());
        e.entry(&r, 11).released_at = Some(now_iso());
        e.entry(&r, 12).blocked = Some(Blocked {
            reason: Blocked::LOGIN.into(),
            since: now_iso(),
            ..Default::default()
        });
        e.entry(&r, 13).handover = Some(PendingHandover {
            harness: "claude".into(),
            ..Default::default()
        });
        e.entry(&r, 14).shares_workspace_of = Some(10);
        e.entry(&r, 15).active = false;
        e.check_conflicts(&r).await.unwrap();
        assert!(d.prompts().is_empty());
        assert!(
            e.conflict_checks.is_empty(),
            "ineligible records triggered a fetch"
        );
    }

    #[tokio::test]
    async fn disabled_conflict_checks_do_not_fetch() {
        let (mut e, r, d, _scratch, _path) = conflict_fixture("conflict-disabled", 16).await;
        e.cfg.daemon.conflict_check_interval_secs = 0;
        e.check_conflicts(&r).await.unwrap();
        assert!(d.prompts().is_empty());
        assert!(e.conflict_checks.is_empty());
    }

    #[tokio::test]
    async fn failed_base_fetch_does_not_use_a_stale_remote_commit() {
        use crate::release::testkit::sh;

        let (mut e, r, d, scratch, _path) = conflict_fixture("conflict-fetch-fails", 17).await;
        sh(
            &scratch.work,
            &["remote", "set-url", "origin", "/no/such/origin.git"],
        )
        .await;
        assert!(e.check_conflicts(&r).await.is_err());
        assert!(e.entry(&r, 17).conflict_notice.is_none());
        assert!(d.prompts().is_empty());
    }

    #[tokio::test]
    async fn conflict_interval_skips_a_second_fetch() {
        use crate::release::testkit::sh;

        let (mut e, r, d, scratch, _path) = conflict_fixture("conflict-interval", 18).await;
        e.cfg.daemon.conflict_check_interval_secs = 3600;
        e.check_conflicts(&r).await.unwrap();
        let _ = d.prompts();
        sh(
            &scratch.work,
            &["remote", "set-url", "origin", "/no/such/origin.git"],
        )
        .await;
        // A fetch here would fail; the configured interval makes this pass a
        // no-op, proving one fetch per repository interval.
        e.check_conflicts(&r).await.unwrap();
        assert!(d.prompts().is_empty());
    }
}
