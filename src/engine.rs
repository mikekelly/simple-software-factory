//! The reconciliation loop: GitHub assigned issues -> Orca workspaces -> agent prompts.

mod requests;

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use crate::allow::{self, AllowList, Source};
use crate::config::DriverKind;
use crate::config::{Config, RepoConfig};
use crate::driver::{Driver, Drivers, Relaunch};
use crate::events::{self, Attach, Conversation, Event};
use crate::github::{Conditional, GitHub, Issue, PrInfo, RepositoryIdentity};
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

const IDENTITY_CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);

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
    /// Repository identity runs separately from the normal issue-poll cadence.
    identity_checked_at: Option<Instant>,
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

mod implementation {
    mod conflicts;
    mod delivery;
    mod handovers;
    mod identity;
    mod issues;
    mod lifecycle;
    mod onboarding;
    mod reconciliation;
    mod releases;
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
mod tests;
