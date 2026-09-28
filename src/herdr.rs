//! Driver for herdr (<https://herdr.dev>): a terminal workspace manager
//! whose CLI talks to its running server. A workspace here is a herdr
//! workspace opened on a git worktree ssf made next to its clone; the agent
//! runs in the workspace's root pane, where herdr recognises it and reports
//! its state (`idle`, `working`, `blocked`, `done`).
//!
//! Workspace ids are herdr's id and the checkout it was opened on, as
//! `w7@/path/to/worktree`: herdr's ids alone are opaque, and the path is
//! what says a workspace with that id is still ours before anything is
//! sent to it or removed. That check asks herdr which worktree the
//! workspace is bound to (`worktree list --workspace`), never a pane's
//! cwd, which follows whatever a shell in it does. Prompt handles are
//! pane ids (`w7:p1`).

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::config::HerdrConfig;
use crate::driver::{
    self, AgentInfo, Delivery, FirstPrompt, Relaunch, WorkspaceInfo, Worktree, add_local_worktree,
    checkout_of_worktree, find_local_worktree, number_of_name, remove_local_worktree,
};

mod channel;
pub(crate) use channel::{Channel, Journal, Terminal};

/// The herdr session ssf runs its agents in on a host. It is ssf's alone: a
/// person's own herdr session keeps its config and its
/// `resume_agents_on_restore`, and ssf's agents never land in it (#602).
/// Not configurable. The VM guest keeps herdr's default session: the guest
/// user and its herdr are ssf's already, its seed turns the restore off
/// (#593), and a guest image cannot be moved to another session by the ssf
/// binary it is handed.
pub const SESSION: &str = "ssf";

/// The session ssf's herdr commands go to, and how a person reaches it here.
pub fn session_summary() -> String {
    if crate::vm::in_guest() {
        "herdr's default session in the VM".to_string()
    } else {
        format!("herdr session `{SESSION}` (attach: `herdr session attach {SESSION}`)")
    }
}

/// The command that starts [`SESSION`]'s headless server with ssf's herdr
/// config.
pub fn server_command() -> String {
    format!(
        "HERDR_CONFIG_PATH={} herdr --session {SESSION} server",
        config_path().display()
    )
}

/// herdr's config for [`SESSION`], owned and written by ssf.
pub fn config_path() -> std::path::PathBuf {
    crate::config::config_dir().join("herdr.toml")
}

/// herdr restoring its panes after a restart must not start agents itself:
/// it runs a bare `claude --resume`, without the environment and inbox
/// channel `ssf launch` gives a session, and the daemon then takes that
/// agent for a live one. The daemon's resume_on_start does it (#593, #602).
const CONFIG: &str = "# Written by ssf for its herdr session; ssf rewrites it.\n\
                      [session]\n\
                      resume_agents_on_restore = false\n";

/// Write [`config_path`] unless it already says [`CONFIG`].
pub fn write_config() -> Result<std::path::PathBuf> {
    let path = config_path();
    if std::fs::read_to_string(&path).ok().as_deref() != Some(CONFIG) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&path, CONFIG).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(path)
}

/// A herdr command aimed at ssf's session: [`SESSION`] on a host (its
/// config is the server's, which [`Herdr::ensure_session`] starts), herdr's
/// default session in the VM guest. Every inherited
/// `HERDR_*` variable goes: ssf may itself run inside a herdr pane, whose
/// workspace, pane and socket (`HERDR_SOCKET_PATH` wins over
/// `HERDR_SESSION`) are the person's, not ssf's.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    isolated(
        std::process::Command::new(program),
        std::env::vars_os().map(|(name, _)| name),
        (!crate::vm::in_guest()).then_some(SESSION),
    )
}

fn isolated(
    mut command: std::process::Command,
    inherited: impl IntoIterator<Item = std::ffi::OsString>,
    session: Option<&str>,
) -> std::process::Command {
    for name in inherited {
        if name.to_string_lossy().starts_with("HERDR_") {
            command.env_remove(name);
        }
    }
    if let Some(session) = session {
        command.env("HERDR_SESSION", session);
    }
    command
}

/// The systemd user scope [`SESSION`]'s server runs in on Linux, apart
/// from the ssf service's cgroup, so a restart of the service leaves the
/// agents running (as the VM's scopes do, #600).
const SERVER_SCOPE: &str = "ssf-herdr";

/// The command that starts [`SESSION`]'s server with ssf's herdr config:
/// [`server_command`] as ssf runs it.
fn server_start(program: impl AsRef<std::ffi::OsStr>, config: &Path) -> std::process::Command {
    let mut cmd = command(program);
    cmd.args(["--session", SESSION, "server"])
        .env("HERDR_CONFIG_PATH", config);
    cmd
}

/// Is `name` running, by `herdr session list --json`? `None` when the
/// listing does not say (not herdr 0.9's shape).
pub fn session_running(list: &Value, name: &str) -> Option<bool> {
    list.get("sessions")?
        .as_array()?
        .iter()
        .find(|s| s.get("name").and_then(Value::as_str) == Some(name))
        .map(|s| s.get("running").and_then(Value::as_bool).unwrap_or(false))
        .or(Some(false))
}

/// What [`Herdr::ensure_session`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ensured {
    /// The VM guest: herdr's default session is the guest's own.
    NotOurs,
    /// The server was up already.
    Running,
    /// ssf started it.
    Started,
}

/// Should ssf start [`SESSION`]'s server, given what `session list` said?
/// Only when herdr answered and says it is not running: a listing ssf
/// cannot read is no licence to start a second server.
pub fn should_start(in_guest: bool, listed: &Result<Option<bool>>) -> bool {
    !in_guest && matches!(listed, Ok(Some(false)))
}

/// One start at a time from this process; herdr itself refuses a second
/// server on a running session.
static ENSURE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Human-readable label; checkout names remain the recovery key.
fn workspace_label(repo: &str, number: u64) -> String {
    let name = repo.rsplit('/').next().unwrap_or(repo);
    format!("{name}-{number}")
}

const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

/// Longest prompt text herdr is handed as one argument. herdr takes the
/// text of a prompt on its command line, and the kernel refuses a spawn
/// whose argument is longer than `MAX_ARG_STRLEN` (128 KiB in 4 KiB-page
/// Linux) with `E2BIG` -- reported as "spawning herdr (is herdr
/// installed?): Argument list too long (os error 7)", which is neither
/// true nor actionable. A delivery carries whatever activity arrived
/// since the daemon's last pass, so a pane left overnight or a daemon
/// restarted after a long outage passes that cap -- it once did so with
/// an item's story and **prevented a handover from starting at all**
/// (pg-cbor-schema#33; a first prompt's activity is capped since #406,
/// deliveries are not). The cap is below the kernel's, because the limit
/// counts the argument's terminator and a different page size changes it.
const HERDR_ARG_LIMIT: usize = 96 * 1024;

/// Bytes of prompt text in one `pane send-text` write, for text too long
/// for [`HERDR_ARG_LIMIT`]. Small enough that the paste markers framing
/// the write fit beside it.
const SEND_TEXT_CHUNK: usize = 64 * 1024;

/// How long to give a freshly launched harness to start working on its
/// first prompt. herdr gives up on its own after five seconds when the
/// text went nowhere; a healthy harness takes well under a second.
///
/// This must stay above those five seconds: herdr's stall detection is
/// fixed, so a shorter `--timeout` returns a plain `timeout` error rather
/// than the `agent_prompt_stalled` [`Herdr::send_first_prompt`] reads. Above
/// them, `timeout` means something else -- the agent's state changed but
/// never reached `working` within the window -- which `prompt_failure`
/// classifies `Other` and the caller reports as a failed start.
const FIRST_PROMPT_TIMEOUT_MS: &str = "15000";

#[derive(Clone)]
pub struct Herdr {
    cfg: HerdrConfig,
}

/// One row of `herdr agent list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub pane_id: String,
    pub workspace_id: String,
    pub kind: String,
    /// `idle`, `working`, `blocked`, `done`, `unknown`.
    pub status: String,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub session_id: Option<String>,
}

/// One row of `herdr pane list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub pane_id: String,
    pub workspace_id: String,
    pub cwd: Option<String>,
    pub agent: Option<String>,
}

/// `w7@/path` -> (`w7`, `/path`); a bare id has no path to check.
pub fn split_id(id: &str) -> (&str, Option<&str>) {
    match id.split_once('@') {
        Some((ws, path)) => (ws, Some(path)),
        None => (id, None),
    }
}

/// Whether herdr can be handed this text as one argument.
fn fits_one_argument(text: &str) -> bool {
    text.len() <= HERDR_ARG_LIMIT
}

/// How much of `text` one `pane send-text` write carries: at most
/// [`SEND_TEXT_CHUNK`] bytes, and never through the middle of a UTF-8
/// character, whose halves two writes would deliver as broken text.
fn chunk_end(text: &str) -> usize {
    if text.len() <= SEND_TEXT_CHUNK {
        return text.len();
    }
    let mut end = SEND_TEXT_CHUNK;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn is_not_found(e: &anyhow::Error) -> bool {
    let msg = e.to_string().to_lowercase();
    msg.contains("not_found") || msg.contains("not found")
}

/// The label of a scratch session's workspace, which has no item number:
/// the repository's name and the worktree's.
pub fn scratch_label(repo: &str, name: &str) -> String {
    format!("{}-{name}", repo.rsplit('/').next().unwrap_or(repo))
}

pub fn make_id(workspace_id: &str, path: &str) -> String {
    format!("{workspace_id}@{path}")
}

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|x| !x.is_empty())
        .map(str::to_string)
}

pub fn parse_agents(v: &Value) -> Vec<Agent> {
    v.get("agents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| {
            Some(Agent {
                pane_id: s(a, "pane_id")?,
                workspace_id: s(a, "workspace_id").unwrap_or_default(),
                kind: s(a, "agent").unwrap_or_default(),
                status: s(a, "agent_status").unwrap_or_else(|| "unknown".into()),
                cwd: s(a, "cwd"),
                title: s(a, "terminal_title_stripped").or_else(|| s(a, "terminal_title")),
                session_id: a
                    .get("agent_session")
                    .filter(|session| {
                        session.get("kind").and_then(Value::as_str) == Some("id")
                            && session.get("agent") == a.get("agent")
                    })
                    .and_then(|session| s(session, "value")),
            })
        })
        .collect()
}

pub fn parse_panes(v: &Value) -> Vec<Pane> {
    v.get("panes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some(Pane {
                pane_id: s(p, "pane_id")?,
                workspace_id: s(p, "workspace_id").unwrap_or_default(),
                cwd: s(p, "cwd"),
                agent: s(p, "agent"),
            })
        })
        .collect()
}

/// The checkout herdr has a workspace bound to, from `herdr worktree list`.
pub fn bound_worktree(v: &Value, workspace_id: &str) -> Option<String> {
    worktree_rows(v)
        .find(|w| s(w, "open_workspace_id").as_deref() == Some(workspace_id))
        .and_then(|w| s(w, "path"))
}

/// The workspace herdr has open on a checkout, from `herdr worktree list`.
pub fn workspace_on(v: &Value, path: &str) -> Option<String> {
    worktree_rows(v)
        .find(|w| s(w, "path").as_deref() == Some(path))
        .and_then(|w| s(w, "open_workspace_id"))
}

fn worktree_rows(v: &Value) -> impl Iterator<Item = &Value> {
    v.get("worktrees")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// A workspace's checkout root and item number, from its checkout path:
/// ssf's worktrees live in `<root>.worktrees/<name>`.
fn root_and_item(cwd: &str) -> (Option<String>, Option<u64>) {
    let root = checkout_of_worktree(cwd).map(|r| r.to_string_lossy().to_string());
    let item = if root.is_some() {
        Path::new(cwd)
            .file_name()
            .and_then(|n| number_of_name(&n.to_string_lossy()))
    } else {
        None
    };
    (root, item)
}

/// Join `herdr workspace list`, `herdr pane list` and `herdr agent list`
/// into the engine's view. A workspace's checkout is the worktree herdr
/// has it bound to; only a workspace not on a worktree is placed by the
/// cwd of its first pane, which follows whatever a shell in it does.
pub fn join_ps(workspaces: &Value, panes: &[Pane], agents: &[Agent]) -> Vec<WorkspaceInfo> {
    workspaces
        .get("workspaces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|w| {
            let id = s(w, "workspace_id")?;
            let ws_panes: Vec<&Pane> = panes.iter().filter(|p| p.workspace_id == id).collect();
            let cwd = w
                .pointer("/worktree/checkout_path")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
                .map(str::to_string)
                .or_else(|| ws_panes.iter().find_map(|p| p.cwd.clone()));
            let (root, item) = cwd.as_deref().map(root_and_item).unwrap_or((None, None));
            let ws_agents: Vec<AgentInfo> = agents
                .iter()
                .filter(|a| a.workspace_id == id)
                .map(|a| AgentInfo {
                    state: a.status.clone(),
                    agent_type: Some(a.kind.clone()),
                    last_assistant_message: a.title.clone(),
                    ..Default::default()
                })
                .collect();
            Some(WorkspaceInfo {
                worktree_id: match cwd.as_deref() {
                    Some(c) => make_id(&id, c),
                    None => id,
                },
                repo_id: root.unwrap_or_default(),
                path: cwd.unwrap_or_default(),
                display_name: s(w, "label").unwrap_or_default(),
                branch: None,
                column: None,
                status: s(w, "agent_status"),
                is_archived: false,
                live_terminals: ws_panes.len() as u64,
                linked_issue: item,
                linked_pr: None,
                last_activity_at: None,
                agents: ws_agents,
            })
        })
        .collect()
}

/// What to do once `herdr agent wait` has returned and the pane's screen
/// has been read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settle {
    /// A first-run trust dialog is up: answer it with these keys and wait
    /// again.
    Answer(driver::TrustAnswer),
    /// The harness is ready for its prompt.
    Ready,
    /// Blocked on a question ssf does not know: the session is held, and
    /// a person answers it (`ssf attach`) before the prompt goes in. A
    /// prompt pasted into an unknown dialog is lost to it (#541).
    Held,
    /// herdr reports the harness settled but its screen is still blank:
    /// the TUI has not drawn its composer yet, so a prompt sent now is lost.
    /// OpenCode 1.18 is `idle` to herdr 0.9 seconds before it draws.
    Undrawn,
}

/// The decision [`Herdr::settle_harness`] makes from the state herdr
/// reported and what is on the screen. The screen decides first, whatever
/// the state: herdr 0.8.2 reports Codex sitting on its directory-trust
/// dialog as `idle` and Claude Code's as `blocked`, so a state of `idle`
/// is no promise that a prompt would reach the composer (#121). The one
/// state that settles it on its own is `working`: an agent already at
/// work is past any first-run dialog, and what its screen shows is its
/// own output.
/// How often [`Herdr::settle_harness`] looks again at a blank screen.
const UNDRAWN_POLL: Duration = Duration::from_millis(500);

pub fn settle_step(state: &str, screen: &str) -> Settle {
    if state == "working" {
        Settle::Ready
    } else if screen.trim().is_empty() {
        Settle::Undrawn
    } else if let Some(answer) = driver::trust_dialog(screen) {
        Settle::Answer(answer)
    } else if state == "blocked" {
        Settle::Held
    } else {
        Settle::Ready
    }
}

/// A harness in `pane` is at a question ssf does not know. Nothing was
/// typed into it: a prompt pasted there is lost to it (#541). `first` is
/// set when the harness has not had its first prompt yet (a launch, or
/// the first prompt refused), and not for a message to a session already
/// at work, which the next pass simply tries again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtQuestion {
    pub pane: String,
    pub first: bool,
}

impl std::fmt::Display for AtQuestion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the harness in pane {} is at a question ssf does not know; held until a person answers it",
            self.pane
        )
    }
}

impl std::error::Error for AtQuestion {}

/// The [`AtQuestion`] this error is, if it is one.
pub fn at_question(e: &anyhow::Error) -> Option<&AtQuestion> {
    e.chain().find_map(|c| c.downcast_ref::<AtQuestion>())
}

/// The agent states [`Herdr::settle_harness`] waits for: every one but
/// `unknown`. `working` is among them because a harness that is at work
/// is settled by any measure, and a resumed Claude Code with queued
/// messages is at work at once (#131); `herdr agent wait` without
/// `--until` would wait for `idle`, `done` or `blocked` alone.
pub const SETTLED_STATES: [&str; 4] = ["idle", "working", "blocked", "done"];

/// The `herdr agent wait` invocation that settles a pane within `timeout`
/// milliseconds.
pub fn settle_wait_args<'a>(pane_id: &'a str, timeout_ms: &'a str) -> Vec<&'a str> {
    let mut args = vec!["agent", "wait", pane_id, "--timeout", timeout_ms];
    for state in SETTLED_STATES {
        args.extend(["--until", state]);
    }
    args
}

/// What a resume came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeVerdict {
    /// The resumed agent is in the pane: the state herdr settled on, or
    /// `unsettled` when the wait ran out with the agent alive all the
    /// same (#133).
    Resumed(String),
    /// Nothing is running the resumed conversation, for this reason.
    Failed(String),
}

/// The decision [`Herdr::deliver`] makes after a resume: from how the
/// settle wait ended, whether herdr reports an agent in the pane once it
/// has, and -- only when it does not -- what the pane shows. An agent in
/// the pane is the resumed conversation whatever the wait said: the wait
/// times out on a state herdr cannot name, and the screen of a live agent
/// is its own output, which may well quote a harness's "no conversation
/// found" (this repository's agents do). With no agent there, the screen
/// says whether the harness could not find the session, and otherwise the
/// wait's own failure is the reason.
pub fn resume_verdict(
    wait: &Result<String, String>,
    agent_present: bool,
    screen: &[String],
) -> ResumeVerdict {
    if agent_present {
        return ResumeVerdict::Resumed(match wait {
            Ok(state) => state.clone(),
            Err(_) => "unsettled".into(),
        });
    }
    if crate::sessions::resume_failed(screen) {
        return ResumeVerdict::Failed("the harness could not find its session".into());
    }
    ResumeVerdict::Failed(match wait {
        Ok(_) => "the harness exited after the wait".into(),
        Err(e) => e.clone(),
    })
}

/// How long a herdr command may run before it is killed, so a wedged
/// server cannot freeze the daemon (#611). A command given its own
/// `--timeout` (milliseconds) gets that long plus the same margin.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

fn run_timeout(args: &[&str]) -> Duration {
    let own = args
        .windows(2)
        .find(|w| w[0] == "--timeout")
        .and_then(|w| w[1].parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_default();
    RUN_TIMEOUT + own
}

/// Why `herdr agent prompt` refused or gave up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptFailure {
    /// herdr thinks the agent is at a question, so it sent nothing.
    Blocked,
    /// The text went in but herdr never saw the harness working on it:
    /// no state change at all (`agent_prompt_stalled`, what a paste
    /// swallowed by a dialog looks like), or a change that never reached
    /// `working` before the timeout (a harness that went straight to a
    /// question, or one herdr's sampler missed). The screen decides.
    Stalled,
    /// Anything else: no pane, no server, a herdr of the wrong version.
    Other,
}

/// Read `herdr agent prompt`'s failure out of the message it printed.
pub fn prompt_failure(message: &str) -> PromptFailure {
    if message.contains("agent_blocked") {
        PromptFailure::Blocked
    } else if message.contains("agent_prompt_stalled")
        || message.contains("[timeout]")
        || message.contains("\"timeout\"")
    {
        PromptFailure::Stalled
    } else {
        PromptFailure::Other
    }
}

/// What to do about a stalled first prompt, from the pane's screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterStall {
    /// A first-run dialog swallowed the paste: answer it with these keys
    /// and send the prompt again.
    Retry(driver::TrustAnswer),
    /// The prompt is on the screen: submit the existing text, never paste it
    /// for a second time.
    Submit,
    /// The screen has neither a known dialog nor enough of the prompt to say
    /// where delivery got to. Observe the agent before doing anything else.
    Observe,
}

/// How much of each early prompt line identifies even a collapsed composer
/// card (OMP renders `# GitHub is…` and `https://git…`). Two matches are
/// required when the prompt supplies them, so prose that merely quotes a
/// dialog does not masquerade as the whole prompt.
const PROMPT_MARKER_CHARS: usize = 10;

pub fn prompt_on_screen(screen: &str, prompt: &str) -> bool {
    // Composer input is at the bottom of every supported harness. Restricting
    // the match to that area keeps the same prompt in conversation history
    // from looking like unsent input after a fast completed turn.
    let tail = screen.lines().rev().take(24).collect::<Vec<_>>().join("\n");
    let mut markers = Vec::new();
    for line in prompt.lines().map(str::trim).filter(|line| {
        !line.is_empty()
            && !(line.starts_with('<') && line.ends_with('>'))
            && line.chars().count() >= PROMPT_MARKER_CHARS
    }) {
        let marker: String = line.chars().take(PROMPT_MARKER_CHARS).collect();
        if !markers.contains(&marker) {
            markers.push(marker);
        }
        if markers.len() == 6 {
            break;
        }
    }
    let required = markers.len().min(2);
    required != 0
        && markers
            .iter()
            .filter(|marker| tail.contains(*marker))
            .count()
            >= required
}

/// What [`Herdr::send_first_prompt`] does when herdr reports the prompt
/// stalled. A stall means only that herdr saw no state change within its
/// five seconds. The text may have been swallowed by a first-run dialog,
/// may be waiting in the composer because its submit key was lost, or may
/// have started while state detection lagged. Only the screen-local actions
/// are decided here; the caller observes ambiguous cases before acting.
///
/// The screen has to be read carefully here: ssf's own prompt may quote a
/// dialog, and OMP collapses a long paste to short ellipsized lines. Several
/// early prompt markers distinguish that composer card from quoted prose.
pub fn after_stall(screen: &str, prompt: &str) -> AfterStall {
    if prompt_on_screen(screen, prompt) {
        return AfterStall::Submit;
    }
    match driver::trust_dialog(screen) {
        Some(answer) => AfterStall::Retry(answer),
        None => AfterStall::Observe,
    }
}

impl Herdr {
    pub fn new(cfg: HerdrConfig) -> Self {
        Self { cfg }
    }

    pub fn command(&self) -> &str {
        &self.cfg.command
    }

    /// Run a herdr command and return its `result`. herdr prints JSON on
    /// stdout on success and a JSON error on stderr otherwise.
    pub async fn run(&self, args: &[&str]) -> Result<Value> {
        let stdout = self.run_raw(args).await?;
        if stdout.trim().is_empty() {
            // Some commands (`pane run`, `send-keys`) print nothing on success.
            return Ok(Value::Null);
        }
        let parsed: Option<Value> = stdout
            .find('{')
            .and_then(|i| serde_json::from_str(&stdout[i..]).ok());
        let Some(v) = parsed else {
            bail!(
                "herdr {} produced no JSON: {}",
                crate::driver::redacted_args(args).join(" "),
                stdout.trim().chars().take(400).collect::<String>()
            );
        };
        Ok(v.get("result").cloned().unwrap_or(v))
    }

    /// Run a herdr command and return its stdout (`read --format text`
    /// prints the screen as it is).
    pub async fn run_raw(&self, args: &[&str]) -> Result<String> {
        debug!(cmd = %self.cfg.command, args = ?driver::redacted_args(args), "herdr");
        self.run_raw_within(args, run_timeout(args)).await
    }

    async fn run_raw_within(&self, args: &[&str], limit: Duration) -> Result<String> {
        let mut child = Command::from(command(crate::config::herdr_command_path(
            &self.cfg.command,
        )));
        child.args(args).kill_on_drop(true);
        let out = tokio::time::timeout(limit, child.output())
            .await
            .map_err(|_| {
                let shown = driver::redacted_args(args).join(" ");
                warn!(cmd = %self.cfg.command, "herdr {shown} timed out; killed");
                anyhow!("herdr {shown} timed out after {}s", limit.as_secs())
            })?
            .with_context(|| format!("spawning {} (is herdr installed?)", self.cfg.command))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            let msg = stderr
                .find('{')
                .and_then(|i| serde_json::from_str::<Value>(&stderr[i..]).ok())
                .map(|e| {
                    let err = e.get("error").unwrap_or(&e);
                    let code = err.get("code").and_then(Value::as_str).unwrap_or("error");
                    let message = err
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| err.to_string());
                    format!("[{code}] {message}")
                })
                .unwrap_or_else(|| {
                    format!(
                        "(exit {:?}) {}",
                        out.status.code(),
                        stderr.trim().chars().take(400).collect::<String>()
                    )
                });
            bail!(
                "herdr {} failed: {msg}",
                crate::driver::redacted_args(args).join(" ")
            );
        }
        Ok(stdout.to_string())
    }

    /// Is the herdr server up?
    pub async fn status(&self) -> Result<()> {
        self.run(&["workspace", "list"])
            .await
            .map(|_| ())
            .map_err(|e| anyhow!("herdr is not answering in {}: {e:#}", session_summary()))
    }

    /// Start [`SESSION`]'s server with ssf's herdr config unless it is
    /// running (#602). On a host only. Detached from ssf, in a systemd user
    /// scope of its own where there is one; its output goes to
    /// `herdr-server.log` in ssf's state directory.
    pub async fn ensure_session(&self) -> Result<Ensured> {
        if crate::vm::in_guest() {
            return Ok(Ensured::NotOurs);
        }
        let _one = ENSURE.lock().await;
        let listed = self.session_listed().await;
        if !should_start(false, &listed) {
            return match listed {
                Ok(Some(true)) => Ok(Ensured::Running),
                Ok(_) => {
                    bail!("`herdr session list --json` did not list sessions as herdr 0.9 does")
                }
                Err(e) => Err(e),
            };
        }
        let config = write_config()?;
        let mut cmd = server_start(
            crate::config::herdr_command_path(&self.cfg.command),
            &config,
        );
        info!(session = SESSION, config = %config.display(), "starting the herdr server");
        if cfg!(test) {
            // Tests never start a server on the machine running them.
            bail!("not starting a herdr server from a test");
        }
        let log = crate::config::state_dir().join("herdr-server.log");
        if let Some(dir) = log.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let (program, pid) = tokio::task::spawn_blocking(move || {
            let pid = crate::vm::spawn_detached(&mut cmd, Some(&log), Some(SERVER_SCOPE));
            (cmd.get_program().to_owned(), pid)
        })
        .await?;
        let pid =
            pid.with_context(|| format!("starting {program:?} --session {SESSION} server"))?;
        // The server stays this daemon's child: reap it when it exits
        // (`herdr session stop ssf`, a crash) so none is left a zombie.
        std::thread::spawn(move || unsafe {
            libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), 0);
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if matches!(self.session_listed().await, Ok(Some(true))) {
                info!(pid, session = SESSION, "herdr server started");
                return Ok(Ensured::Started);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        bail!(
            "started the herdr server (pid {pid}), but session `{SESSION}` is not running after 10s; see {}",
            crate::config::state_dir()
                .join("herdr-server.log")
                .display()
        )
    }

    async fn session_listed(&self) -> Result<Option<bool>> {
        let list = self.run(&["session", "list", "--json"]).await?;
        Ok(session_running(&list, SESSION))
    }

    async fn agents(&self) -> Result<Vec<Agent>> {
        Ok(parse_agents(&self.run(&["agent", "list"]).await?))
    }

    async fn panes(&self, workspace_id: &str) -> Result<Vec<Pane>> {
        Ok(parse_panes(
            &self
                .run(&["pane", "list", "--workspace", workspace_id])
                .await?,
        ))
    }

    /// Does herdr have a workspace with this id?
    async fn workspace_exists(&self, workspace_id: &str) -> Result<bool> {
        match self.run(&["workspace", "get", workspace_id]).await {
            Ok(_) => Ok(true),
            Err(e) if is_not_found(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The checkout herdr has a workspace bound to, from its own worktree
    /// list keyed by the workspace; `None` when herdr has no such workspace
    /// or it is not open on a worktree. Panes are not consulted: their cwd
    /// follows whatever a shell in them does.
    async fn bound_path(&self, workspace_id: &str) -> Result<Option<String>> {
        match self
            .run(&["worktree", "list", "--workspace", workspace_id])
            .await
        {
            Ok(v) => Ok(bound_worktree(&v, workspace_id)),
            Err(e) if is_not_found(&e) || e.to_string().contains("not_git_worktree") => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The herdr workspace id behind one of our ids, provided the workspace
    /// is still bound to the checkout the id names.
    async fn ours(&self, id: &str) -> Result<Option<String>> {
        let (ws, path) = split_id(id);
        let Some(path) = path else {
            return Ok(self.workspace_exists(ws).await?.then(|| ws.to_string()));
        };
        match self.bound_path(ws).await? {
            Some(open_on) if open_on == path => Ok(Some(ws.to_string())),
            Some(open_on) => {
                warn!(
                    id,
                    open_on, "herdr workspace is open on another checkout; treating it as gone"
                );
                Ok(None)
            }
            None => Ok(None),
        }
    }

    /// The herdr workspace open on `path`, if any.
    async fn workspace_for_path(&self, repo_root: &str, path: &str) -> Result<Option<String>> {
        let v = self.run(&["worktree", "list", "--cwd", repo_root]).await?;
        Ok(workspace_on(&v, path))
    }

    /// Open (or find open) a herdr workspace on a local worktree.
    async fn open(&self, repo_root: &str, path: &str, label: &str) -> Result<String> {
        if let Some(ws) = self.workspace_for_path(repo_root, path).await? {
            return Ok(ws);
        }
        let v = self
            .run(&[
                "worktree",
                "open",
                "--cwd",
                repo_root,
                "--path",
                path,
                "--label",
                label,
                "--no-focus",
            ])
            .await?;
        v.pointer("/workspace/workspace_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| driver::err_no_field("worktree open returned no workspace id", &v))
    }

    pub async fn find_worktree_for_issue(
        &self,
        repo_root: &str,
        repo: &str,
        number: u64,
    ) -> Result<Option<Worktree>> {
        let Some(w) = find_local_worktree(repo_root, number).await? else {
            return Ok(None);
        };
        let label = workspace_label(repo, number);
        let ws = self.open(repo_root, &w.path, &label).await?;
        Ok(Some(Worktree {
            id: make_id(&ws, &w.path),
            path: w.path,
            branch: w.branch,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_worktree(
        &self,
        repo_root: &str,
        repo: &str,
        name: &str,
        number: u64,
        comment: &str,
        base_branch: Option<&str>,
    ) -> Result<Worktree> {
        let (path, branch) = add_local_worktree(repo_root, name, base_branch).await?;
        // A scratch session's workspace is labelled by its name, since it
        // has no item number.
        let label = if number == crate::driver::NO_ITEM {
            scratch_label(repo, name)
        } else {
            workspace_label(repo, number)
        };
        let ws = match self.open(repo_root, &path, &label).await {
            Ok(ws) => ws,
            Err(e) => {
                let _ = remove_local_worktree(repo_root, &path).await;
                return Err(e);
            }
        };
        let id = make_id(&ws, &path);
        let _ = self.set_comment(&id, comment).await;
        Ok(Worktree {
            id,
            path,
            branch: Some(branch),
        })
    }

    /// A workspace on a checkout that already exists: a scratch session's
    /// plain worktree from when it ran in tmux (#491), moving to a pane
    /// (#565). Nothing is created or removed on disk.
    pub async fn open_worktree(&self, repo_root: &str, label: &str, path: &str) -> Result<String> {
        let ws = self.open(repo_root, path, label).await?;
        Ok(make_id(&ws, path))
    }

    pub async fn worktree_exists(&self, id: &str) -> Result<bool> {
        Ok(self.ours(id).await?.is_some())
    }

    pub async fn ps(&self) -> Result<Vec<WorkspaceInfo>> {
        let workspaces = self.run(&["workspace", "list"]).await?;
        let agents = self.agents().await?;
        let panes = parse_panes(&self.run(&["pane", "list"]).await?);
        let mut rows = join_ps(&workspaces, &panes, &agents);
        // Transcript metadata is local to the machine running this driver.
        // In VM mode `ssf status` runs in the guest, alongside its agents.
        tokio::task::spawn_blocking(move || {
            for row in &mut rows {
                let (workspace, _) = split_id(&row.worktree_id);
                row.last_activity_at = agents
                    .iter()
                    .filter(|a| a.workspace_id == workspace)
                    .filter_map(|a| {
                        crate::sessions::last_activity(&a.kind, &row.path, a.session_id.as_deref()?)
                    })
                    .max();
            }
            rows
        })
        .await
        .context("reading agent activity")
    }

    pub async fn has_live_agent(&self, id: &str) -> Result<bool> {
        let (ws, _) = split_id(id);
        Ok(self.agents().await?.iter().any(|a| a.workspace_id == ws))
    }

    /// Close the workspace and remove its checkout. Only a workspace that is
    /// still bound to our checkout is touched.
    pub async fn remove_worktree(&self, id: &str) -> Result<()> {
        let Some(workspace_id) = self.ours(id).await? else {
            return Ok(());
        };
        let workspace_id = workspace_id.as_str();
        let (_, path) = split_id(id);
        let panes = self.panes(workspace_id).await.unwrap_or_default();
        for p in &panes {
            if p.agent.is_some() {
                let _ = self.run(&["pane", "send-keys", &p.pane_id, "ctrl+c"]).await;
            }
        }
        match self
            .run(&["worktree", "remove", "--workspace", workspace_id, "--force"])
            .await
        {
            Ok(_) => {}
            Err(e) => {
                warn!(
                    workspace_id,
                    "herdr worktree remove failed ({e:#}); closing the workspace"
                );
                self.run(&["workspace", "close", workspace_id]).await?;
            }
        }
        // Whatever herdr did with the checkout, git must agree.
        if let Some(path) = path {
            let (root, _) = root_and_item(path);
            if let Some(root) = root {
                let _ = remove_local_worktree(&root, path).await;
            }
        }
        Ok(())
    }

    pub async fn set_comment(&self, id: &str, comment: &str) -> Result<()> {
        let (workspace_id, _) = split_id(id);
        let token = format!("note={comment}");
        self.run(&[
            "workspace",
            "report-metadata",
            workspace_id,
            "--source",
            "ssf",
            "--token",
            &token,
        ])
        .await?;
        Ok(())
    }

    pub async fn set_status(&self, id: &str, status: &str) -> Result<()> {
        let (workspace_id, _) = split_id(id);
        let token = format!("status={status}");
        self.run(&[
            "workspace",
            "report-metadata",
            workspace_id,
            "--source",
            "ssf",
            "--token",
            &token,
        ])
        .await?;
        Ok(())
    }

    /// A pane of the workspace at a shell prompt (no agent in it), made if
    /// there is none.
    async fn shell_pane(&self, workspace_id: &str) -> Result<String> {
        let panes = self.panes(workspace_id).await?;
        if let Some(p) = panes.iter().find(|p| p.agent.is_none()) {
            return Ok(p.pane_id.clone());
        }
        let cwd = panes.iter().find_map(|p| p.cwd.clone());
        let mut args = vec!["tab", "create", "--workspace", workspace_id, "--no-focus"];
        if let Some(c) = cwd.as_deref() {
            args.extend(["--cwd", c]);
        }
        let v = self.run(&args).await?;
        v.pointer("/root_pane/pane_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| driver::err_no_field("tab create returned no pane id", &v))
    }

    /// Rendered screen of a pane, as lines.
    pub async fn screen(&self, pane_id: &str) -> Result<Vec<String>> {
        self.screen_from(pane_id, "visible").await
    }

    /// Recent logical lines retain more of a collapsed or scrolled composer
    /// than the visible viewport, which is what prompt recovery needs.
    async fn recent_screen(&self, pane_id: &str) -> Result<Vec<String>> {
        self.screen_from(pane_id, "recent-unwrapped").await
    }

    async fn screen_from(&self, pane_id: &str, source: &str) -> Result<Vec<String>> {
        let text = self
            .run_raw(&[
                "pane", "read", pane_id, "--source", source, "--format", "text",
            ])
            .await?;
        Ok(text.lines().map(str::to_string).collect())
    }

    /// Wait until herdr sees an agent in the pane and it is ready for input
    /// or already at work, answering the folder-trust dialog Claude Code and
    /// Codex show on a new worktree. Returns the state herdr reported when
    /// the harness settled.
    ///
    /// The wait names every state but `unknown` ([`SETTLED_STATES`]):
    /// without `--until`, `herdr agent wait` returns on `idle`, `done` or
    /// `blocked` and never on `working`, and a resumed Claude Code with
    /// queued messages is `working` from its first second, so the wait
    /// timed out on every such resume and the daemon took the timeout for
    /// a failed resume (#131).
    ///
    /// The screen is read after every wait whatever state herdr reports,
    /// because the reported state does not say whether a dialog is up:
    /// herdr 0.8.2 calls Codex 0.152.0 sitting on its directory-trust
    /// dialog `idle`, where it calls Claude Code's equivalent `blocked`
    /// (#121). A prompt pasted into that dialog is swallowed by it -- the
    /// Enter answers the question and the text is gone -- so the dialog is
    /// answered and the harness waited for again.
    pub async fn settle_harness(&self, pane_id: &str, harness: &str) -> Result<String> {
        let deadline = Instant::now() + Duration::from_millis(self.cfg.tui_idle_timeout_ms);
        let mut detected = false;
        while Instant::now() < deadline {
            if self.agents().await?.iter().any(|a| a.pane_id == pane_id) {
                detected = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(1000)).await;
        }
        if !detected {
            bail!("herdr did not detect {harness} in pane {pane_id} in time");
        }
        let mut state = String::new();
        let mut answers = 0;
        while answers < 4 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("{harness} in {pane_id} did not settle in time");
            }
            let t = left.as_millis().to_string();
            let v = self.run(&settle_wait_args(pane_id, &t)).await?;
            state = v
                .get("agent_status")
                .or_else(|| v.pointer("/agent/agent_status"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            // A screen that will not read decides nothing; the state does.
            let Ok(text) = self.screen(pane_id).await.map(|s| s.join("\n")) else {
                return Ok(state);
            };
            match settle_step(&state, &text) {
                Settle::Answer(answer) => {
                    info!(pane_id, state, "accepting the folder trust dialog");
                    self.answer_trust(pane_id, answer).await?;
                    answers += 1;
                    continue;
                }
                Settle::Undrawn => {
                    // A screen still blank at the deadline launches as before
                    // this wait existed, rather than failing the launch.
                    if deadline.saturating_duration_since(Instant::now()) <= UNDRAWN_POLL {
                        warn!(
                            pane_id,
                            "{harness} drew nothing before the deadline; going on"
                        );
                        return Ok(state);
                    }
                    tokio::time::sleep(UNDRAWN_POLL).await;
                    continue;
                }
                Settle::Held => {
                    // Some other question: a prompt pasted now is lost to
                    // it. The caller holds the session until a person has
                    // answered it, without waiting here.
                    warn!(pane_id, "{harness} is at a question ssf does not know");
                    return Err(AtQuestion {
                        pane: pane_id.to_string(),
                        first: true,
                    }
                    .into());
                }
                Settle::Ready => return Ok(state),
            }
        }
        // Only four answered dialogs in a row get here, so one is still on
        // the screen: the launch goes ahead, and the first prompt meets it.
        warn!(
            pane_id,
            "{harness} still shows a first-run dialog after four answers; going on anyway"
        );
        Ok(state)
    }

    /// Is the agent in the pane blocked at a question ssf does not know
    /// ([`Settle::Held`])? `false` once a person has answered it, the
    /// screen is a trust dialog ssf answers itself, or the agent is gone.
    pub async fn at_question_now(&self, pane_id: &str) -> Result<bool> {
        let Some(agent) = self
            .agents()
            .await?
            .into_iter()
            .find(|a| a.pane_id == pane_id)
        else {
            return Ok(false);
        };
        let text = self.screen(pane_id).await?.join("\n");
        Ok(settle_step(&agent.status, &text) == Settle::Held)
    }

    /// A paste that bypasses `agent prompt` has no herdr refusal to meet a
    /// question with, so the pane is looked at first: at a question, or
    /// unreadable, nothing is pasted and the error is an [`AtQuestion`].
    async fn no_question_before_paste(&self, pane_id: &str, first: bool) -> Result<()> {
        if self.at_question_now(pane_id).await.unwrap_or(true) {
            return Err(AtQuestion {
                pane: pane_id.to_string(),
                first,
            }
            .into());
        }
        Ok(())
    }

    /// Answer a trust dialog in the pane with the keys it takes.
    async fn answer_trust(&self, pane_id: &str, answer: driver::TrustAnswer) -> Result<()> {
        if answer == driver::TrustAnswer::DownEnter {
            self.run(&["agent", "send-keys", pane_id, "down"]).await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        self.run(&["agent", "send-keys", pane_id, "enter"]).await?;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        Ok(())
    }

    /// Run the harness in a shell pane of the workspace and wait for it.
    pub async fn launch(
        &self,
        id: &str,
        command: &str,
        title: &str,
        harness: &str,
    ) -> Result<String> {
        let pane = self.run_harness(id, command).await?;
        self.settle_harness(&pane, harness).await?;
        let _ = self.run(&["pane", "rename", &pane, title]).await;
        Ok(pane)
    }

    /// Run the harness in a shell pane of the workspace, without waiting
    /// for it: the pane is known to the caller even when the wait fails.
    async fn run_harness(&self, id: &str, command: &str) -> Result<String> {
        let (workspace_id, _) = split_id(id);
        let pane = self.shell_pane(workspace_id).await?;
        self.run(&["pane", "run", &pane, command]).await?;
        Ok(pane)
    }

    /// Does herdr report an agent in the pane, whatever its state? A
    /// listing that fails is an error, not "no agent": what follows a "no"
    /// here is a Ctrl-C into the pane or a fresh harness.
    async fn agent_in(&self, pane_id: &str) -> Result<bool> {
        Ok(self.agents().await?.iter().any(|a| a.pane_id == pane_id))
    }

    /// Make sure nothing of a resume the daemon has given up on is running
    /// before a fresh harness goes into the workspace, so one workspace
    /// never holds two agents (#131). The daemon gives up only on a pane
    /// herdr reports no agent in, so there is normally nothing to stop:
    /// Claude Code exits when it cannot find the session and leaves the
    /// pane at its shell, which the fresh launch reuses. An agent that
    /// turns up in the pane after all -- herdr noticing a slow-starting
    /// harness late -- is the resumed conversation, not a thing to stop,
    /// and is returned as the handle. A live agent elsewhere in the
    /// workspace is an error rather than a fresh launch beside it.
    async fn clear_failed_resume(&self, id: &str, pane_id: &str) -> Result<Option<String>> {
        let (ws, _) = split_id(id);
        // herdr notices a harness a second or so after it starts; a few
        // seconds' grace before anything is typed into the pane.
        for _ in 0..6 {
            if self.agent_in(pane_id).await? {
                return Ok(Some(pane_id.to_string()));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        // Whatever the harness left in the pane, back to its shell. One
        // Ctrl-C ends no harness, so an agent that shows itself now is
        // still the resumed conversation.
        let _ = self.run(&["pane", "send-keys", pane_id, "ctrl+c"]).await;
        tokio::time::sleep(Duration::from_millis(1000)).await;
        if self.agent_in(pane_id).await? {
            return Ok(Some(pane_id.to_string()));
        }
        if let Some(h) = self.live_handle(id, None).await? {
            bail!("an agent is live in pane {h} of workspace {ws}; not starting another beside it");
        }
        Ok(None)
    }

    /// The live agent pane a delivery would go to: `preferred` if it is
    /// still one, else any in the workspace.
    pub async fn live_handle(&self, id: &str, preferred: Option<&str>) -> Result<Option<String>> {
        let (ws, _) = split_id(id);
        let agents = self.agents().await?;
        let live: Vec<&Agent> = agents.iter().filter(|a| a.workspace_id == ws).collect();
        Ok(preferred
            .and_then(|h| live.iter().find(|a| a.pane_id == h))
            .or_else(|| live.first())
            .map(|a| a.pane_id.clone()))
    }

    /// Quit the agent in a pane: Ctrl-C twice (which ends every harness at
    /// a prompt or a login screen), and if herdr still sees an agent there
    /// after a few seconds, close the pane; the workspace keeps its other
    /// panes and `launch` opens a new tab when none is free.
    pub async fn stop_agent(&self, id: &str, pane_id: &str) -> Result<()> {
        let (ws, _) = split_id(id);
        for _ in 0..2 {
            let _ = self.run(&["pane", "send-keys", pane_id, "ctrl+c"]).await;
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        let gone = |agents: &[Agent]| !agents.iter().any(|a| a.pane_id == pane_id);
        for _ in 0..6 {
            if gone(&self.agents().await?) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        warn!(pane_id, "agent did not quit on ctrl+c; closing the pane");
        self.run(&["pane", "close", pane_id]).await?;
        for _ in 0..6 {
            if gone(&self.agents().await?) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        bail!("an agent is still reported in pane {pane_id} of workspace {ws}")
    }

    /// Give the agent in a pane a prompt. `agent prompt` pastes for us; if
    /// it refuses because the agent is at a question, nothing is pasted
    /// (text pasted into a question is lost to it, #541): the error is an
    /// [`AtQuestion`], and the caller holds the prompt for the next pass.
    pub async fn send_prompt(&self, pane_id: &str, text: &str) -> Result<()> {
        if !fits_one_argument(text) {
            warn!(
                pane_id,
                bytes = text.len(),
                "the prompt is too long for one herdr argument; pasting it raw"
            );
            self.no_question_before_paste(pane_id, false).await?;
            return self.paste_prompt(pane_id, text).await;
        }
        match self
            .run(&["agent", "prompt", pane_id, text.trim_end()])
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if prompt_failure(&e.to_string()) == PromptFailure::Blocked => Err(AtQuestion {
                pane: pane_id.to_string(),
                first: false,
            }
            .into()),
            Err(e) => Err(e),
        }
    }

    /// Paste a prompt into the pane and submit it with Enter, for text that
    /// has to go there rather than through herdr's own prompt handling. The
    /// harness is given the bracketed paste and the Enter `agent prompt`
    /// would have made of it.
    ///
    /// One Enter does not always submit a paste. OMP asks how to represent
    /// a paste this size and spends the Enter on that answer, leaving the
    /// prompt in the composer -- and herdr reports no state change either
    /// way, so nothing else notices it never started a turn. The composer
    /// is therefore read back, and a prompt still sitting there gets one
    /// more Enter. Only Enter is repeated: the prompt is never pasted
    /// twice, which is what would double a delivery.
    async fn paste_prompt(&self, pane_id: &str, text: &str) -> Result<()> {
        let text = text.trim_end();
        self.type_text(pane_id, text).await?;
        tokio::time::sleep(Duration::from_millis(400)).await;
        self.run(&["pane", "send-keys", pane_id, "enter"]).await?;
        for _ in 0..5 {
            tokio::time::sleep(Duration::from_millis(600)).await;
            let screen = self
                .recent_screen(pane_id)
                .await
                .unwrap_or_default()
                .join("\n");
            if prompt_on_screen(&screen, text) {
                warn!(
                    pane_id,
                    "the pasted prompt is still in the composer; submitting it again"
                );
                return self
                    .run(&["pane", "send-keys", pane_id, "enter"])
                    .await
                    .map(|_| ());
            }
        }
        Ok(())
    }

    /// Type literal text into a pane as one bracketed paste, in as many
    /// `pane send-text` writes as [`SEND_TEXT_CHUNK`] needs. herdr takes
    /// the text as an argument, so a whole item story cannot go in one
    /// write: the paste is opened before the first write and closed after
    /// the last, and the pane reads one paste however many writes carried
    /// it. Nothing is submitted here.
    async fn type_text(&self, pane_id: &str, text: &str) -> Result<()> {
        let mut rest = text;
        let mut opened = false;
        loop {
            let (chunk, tail) = rest.split_at(chunk_end(rest));
            let framed = match (opened, tail.is_empty()) {
                (false, true) => format!("{PASTE_START}{chunk}{PASTE_END}"),
                (false, false) => format!("{PASTE_START}{chunk}"),
                (true, true) => format!("{chunk}{PASTE_END}"),
                (true, false) => chunk.to_string(),
            };
            self.run(&["pane", "send-text", pane_id, &framed]).await?;
            if tail.is_empty() {
                return Ok(());
            }
            rest = tail;
            opened = true;
        }
    }

    /// Give a harness that has just been launched its first prompt, and
    /// confirm it landed: herdr waits until the agent is *working* on it
    /// (not until the turn is done, which is a first prompt's whole answer)
    /// and reports `agent_prompt_stalled` when nothing happened. That is
    /// the difference between a session that has its instructions and one
    /// sitting at an empty composer, which is otherwise invisible -- a
    /// plain `agent prompt` returns success for a paste a first-run dialog
    /// swallowed (#121). A stall with a trust dialog on the screen is that
    /// dialog: it is answered and the prompt sent once more.
    ///
    /// A stall with no dialog is observed once more before anything is sent.
    /// If the prompt is sitting in the composer, only Enter is sent. A
    /// successful key delivery is accepted even when the harness's state
    /// never changes: retrying Enter can turn the assignment into repeated
    /// steering messages when Herdr cannot narrate the agent (#317).
    pub async fn send_first_prompt(&self, pane_id: &str, text: &str) -> Result<()> {
        let mut answered_dialog = false;
        loop {
            let e = match self.submit_first_prompt(pane_id, text).await {
                Ok(()) => return Ok(()),
                Err(e) => e,
            };
            match prompt_failure(&e.to_string()) {
                // Sent nothing: the harness is at a question. A trust
                // dialog is answered; anything else holds the prompt until
                // a person has answered it.
                PromptFailure::Blocked => {
                    let screen = self.screen(pane_id).await.unwrap_or_default().join("\n");
                    match driver::trust_dialog(&screen) {
                        Some(answer) if !answered_dialog => {
                            self.answer_trust(pane_id, answer).await?;
                            answered_dialog = true;
                        }
                        _ => {
                            return Err(AtQuestion {
                                pane: pane_id.to_string(),
                                first: true,
                            }
                            .into());
                        }
                    }
                }
                PromptFailure::Stalled => {
                    let screen = self
                        .recent_screen(pane_id)
                        .await
                        .unwrap_or_default()
                        .join("\n");
                    match after_stall(&screen, text) {
                        AfterStall::Retry(answer) if !answered_dialog => {
                            info!(pane_id, "the first prompt met a trust dialog; answering it");
                            self.answer_trust(pane_id, answer).await?;
                            answered_dialog = true;
                        }
                        AfterStall::Submit => {
                            warn!(
                                pane_id,
                                "the first prompt is still in the composer; submitting it again"
                            );
                            return self.submit_existing_prompt(pane_id).await;
                        }
                        AfterStall::Observe => {
                            warn!(pane_id, "first prompt stalled; observing before recovery");
                            if self.wait_for_prompt_start(pane_id).await.is_ok() {
                                return Ok(());
                            }
                            let later = self
                                .recent_screen(pane_id)
                                .await
                                .unwrap_or_default()
                                .join("\n");
                            match after_stall(&later, text) {
                                AfterStall::Submit => {
                                    return self.submit_existing_prompt(pane_id).await;
                                }
                                AfterStall::Retry(answer) if !answered_dialog => {
                                    info!(pane_id, "a first-run dialog appeared after the stall");
                                    self.answer_trust(pane_id, answer).await?;
                                    answered_dialog = true;
                                }
                                _ => bail!(
                                    "first prompt in {pane_id} remains unconfirmed; its text was \
not sent again"
                                ),
                            }
                        }
                        AfterStall::Retry(_) => {
                            bail!("first prompt in {pane_id} met the trust dialog again")
                        }
                    }
                }
                PromptFailure::Other => return Err(e),
            }
        }
    }

    /// Recover a first prompt after the daemon saw the harness but did not
    /// record the session as seeded. Text already on the screen is submitted
    /// in place, and a known first-run dialog is answered before a fresh
    /// delivery. An ambiguous screen is accepted: the attempt was recorded
    /// before terminal input, so sending again could steer a session that
    /// already consumed the prompt.
    async fn recover_first_prompt(&self, pane_id: &str, text: &str) -> Result<()> {
        if self.agent_started_prompt(pane_id).await? {
            return Ok(());
        }
        let screen = self.recent_screen(pane_id).await?.join("\n");
        match after_stall(&screen, text) {
            AfterStall::Submit => self.submit_existing_prompt(pane_id).await,
            AfterStall::Retry(answer) => {
                self.answer_trust(pane_id, answer).await?;
                self.send_first_prompt(pane_id, text).await
            }
            AfterStall::Observe => {
                warn!(
                    pane_id,
                    "an earlier first-prompt attempt is no longer visible; accepting it without \
resending"
                );
                Ok(())
            }
        }
    }

    async fn agent_started_prompt(&self, pane_id: &str) -> Result<bool> {
        Ok(self
            .agents()
            .await?
            .iter()
            .any(|agent| agent.pane_id == pane_id && agent.status == "working"))
    }

    async fn submit_existing_prompt(&self, pane_id: &str) -> Result<()> {
        self.run(&["agent", "send-keys", pane_id, "enter"]).await?;
        if let Err(e) = self.wait_for_prompt_start(pane_id).await {
            if prompt_failure(&e.to_string()) == PromptFailure::Stalled {
                warn!(
                    pane_id,
                    "the harness did not report starting after the prompt was submitted; \
accepting the successful Enter without retrying: {e:#}"
                );
            } else {
                return Err(e);
            }
        }
        Ok(())
    }

    async fn wait_for_prompt_start(&self, pane_id: &str) -> Result<()> {
        self.run(&[
            "agent",
            "wait",
            pane_id,
            "--until",
            "working",
            "--until",
            "blocked",
            "--timeout",
            FIRST_PROMPT_TIMEOUT_MS,
        ])
        .await
        .map(|_| ())
    }

    /// `agent prompt`, waiting only until the harness starts working on it.
    async fn prompt_until_working(&self, pane_id: &str, text: &str) -> Result<()> {
        self.run(&[
            "agent",
            "prompt",
            pane_id,
            text.trim_end(),
            "--wait",
            "--until",
            "working",
            "--timeout",
            FIRST_PROMPT_TIMEOUT_MS,
        ])
        .await
        .map(|_| ())
    }

    /// Give the harness its first prompt, however long it is. `agent prompt`
    /// cannot carry a story past [`HERDR_ARG_LIMIT`] -- herdr takes the text
    /// as an argument -- so a longer one is typed into the pane and
    /// submitted with the same Enter, and waited on the same way: a wait
    /// that times out is the `[timeout]` [`prompt_failure`] reads as a
    /// stall, so the screen decides exactly as it does for a short one.
    async fn submit_first_prompt(&self, pane_id: &str, text: &str) -> Result<()> {
        if fits_one_argument(text) {
            return self.prompt_until_working(pane_id, text).await;
        }
        warn!(
            pane_id,
            bytes = text.len(),
            "the first prompt is too long for one herdr argument; typing it into the pane"
        );
        self.no_question_before_paste(pane_id, true).await?;
        self.paste_prompt(pane_id, text).await?;
        self.wait_until_working(pane_id).await
    }

    /// `agent wait`, until the harness starts working on the prompt it was
    /// just given. The counterpart of [`Herdr::prompt_until_working`] for a
    /// prompt herdr was not handed through `agent prompt`.
    async fn wait_until_working(&self, pane_id: &str) -> Result<()> {
        self.run(&[
            "agent",
            "wait",
            pane_id,
            "--until",
            "working",
            "--timeout",
            FIRST_PROMPT_TIMEOUT_MS,
        ])
        .await
        .map(|_| ())
    }

    pub(crate) async fn claude_inbox(
        &self,
        pane_id: &str,
    ) -> Option<crate::claude_delivery::Inbox> {
        let info = self
            .run(&["pane", "process-info", "--pane", pane_id])
            .await
            .ok()?;
        crate::claude_delivery::discover(&info).await
    }

    pub(crate) async fn codex_channel_available(&self, pane: &str, mailbox: &Path) -> Result<bool> {
        let info = self.run(&["pane", "process-info", "--pane", pane]).await?;
        crate::codex_delivery::available(&info, mailbox).await
    }

    /// Start the harness again in a workspace whose agent is gone, with
    /// nothing typed into it: resume its conversation when `relaunch` has a
    /// resume command and the resume stays up, else a fresh harness. `held`
    /// (a session-bound channel with an outstanding delivery) refuses the
    /// fresh start. Returns the pane, whether it resumed, and whether the
    /// resumed agent is already at work.
    async fn start_again(
        &self,
        workspace_id: &str,
        relaunch: &Relaunch<'_>,
        held: bool,
    ) -> Result<(String, bool, bool)> {
        let mut resumed = false;
        let mut handle = None;
        // A resumed agent already at work takes the message as a steering
        // prompt, queued behind its turn, rather than a confirmed first
        // prompt: it has its instructions, and is not at an empty composer.
        let mut at_work = false;
        if let Some(cmd) = relaunch.resume_command {
            warn!(
                workspace_id,
                cmd = driver::redacted(cmd),
                "no live agent; resuming harness session"
            );
            // A pane the resume has been run in is known from here on, so
            // whatever the wait says, what is in it can be dealt with.
            let pane = self.run_harness(workspace_id, cmd).await?;
            let wait = self
                .settle_harness(&pane, relaunch.harness)
                .await
                .map_err(|e| format!("{e:#}"));
            let present = self.agent_in(&pane).await?;
            // The screen is the harness's only when no agent is in the
            // pane; with one there it is the agent's own output.
            let screen = if present {
                Vec::new()
            } else {
                self.screen(&pane).await.unwrap_or_default()
            };
            match resume_verdict(&wait, present, &screen) {
                ResumeVerdict::Resumed(state) => {
                    if let Err(e) = &wait {
                        warn!(
                            workspace_id,
                            pane,
                            "the resumed harness did not settle ({e}) but its agent is alive; \
keeping it"
                        );
                    }
                    info!(workspace_id, pane, state, "harness resumed its session");
                    let _ = self.run(&["pane", "rename", &pane, relaunch.title]).await;
                    at_work = state == "working";
                    resumed = true;
                    handle = Some(pane);
                }
                ResumeVerdict::Failed(why) => {
                    warn!(
                        workspace_id,
                        pane,
                        "giving up on the resume ({why}); starting fresh once the pane is clear"
                    );
                    if let Some(p) = self.clear_failed_resume(workspace_id, &pane).await? {
                        warn!(
                            workspace_id,
                            pane = p,
                            "an agent turned up in the resumed pane after all; keeping it"
                        );
                        let _ = self.run(&["pane", "rename", &p, relaunch.title]).await;
                        resumed = true;
                        handle = Some(p);
                    }
                }
            }
        }
        if held && handle.is_none() {
            bail!(
                "{} could not resume its outstanding native delivery; refusing a fresh terminal submission",
                relaunch.harness
            );
        }
        let handle = match handle {
            Some(h) => h,
            None => {
                warn!(
                    workspace_id,
                    command = driver::redacted(relaunch.command),
                    "no live agent; relaunching harness"
                );
                // Pi and OMP have no resume command of ssf's: their launcher
                // continues the transcript it finds in the mailbox (#659).
                resumed = relaunch.channel.is_some_and(|(mailbox, _)| {
                    crate::delivery_channel::launcher_resumes(
                        relaunch.harness,
                        relaunch.command,
                        mailbox,
                    )
                });
                self.launch(
                    workspace_id,
                    relaunch.command,
                    relaunch.title,
                    relaunch.harness,
                )
                .await?
            }
        };
        Ok((handle, resumed, at_work))
    }

    /// Start the harness again in a workspace with no live agent, typing
    /// nothing (a scratch session waits at its composer for the person at
    /// its terminal, #487): see `start_again`.
    pub async fn restart(&self, workspace_id: &str, relaunch: &Relaunch<'_>) -> Result<Delivery> {
        if let Some(handle) = self.live_handle(workspace_id, None).await? {
            return Ok(Delivery {
                handle,
                relaunched: false,
                resumed: false,
            });
        }
        let (handle, resumed, _) = self.start_again(workspace_id, relaunch, false).await?;
        Ok(Delivery {
            handle,
            relaunched: true,
            resumed,
        })
    }

    pub async fn deliver(
        &self,
        workspace_id: &str,
        preferred_handle: Option<&str>,
        relaunch: &Relaunch<'_>,
        text: &str,
    ) -> Result<Delivery> {
        let (ws, _) = split_id(workspace_id);
        let agents = self.agents().await?;
        let live: Vec<&Agent> = agents.iter().filter(|a| a.workspace_id == ws).collect();
        let channel = crate::harness::channel(relaunch.harness);
        let target = if channel.session_bound() {
            // A saved pane is an address, not a preference. Never deliver to a
            // neighbour if its agent exits or its pane hosts a different harness.
            match preferred_handle {
                Some(handle) => live
                    .iter()
                    .find(|a| a.pane_id == handle && a.kind == relaunch.harness),
                None => {
                    let candidates: Vec<_> =
                        live.iter().filter(|a| a.kind == relaunch.harness).collect();
                    if candidates.len() > 1 {
                        bail!(
                            "{} delivery has multiple live sessions and no saved pane",
                            relaunch.harness
                        );
                    }
                    candidates.first().copied()
                }
            }
        } else {
            preferred_handle
                .and_then(|h| live.iter().find(|a| a.pane_id == h))
                .or_else(|| live.first())
        }
        .map(|a| a.pane_id.clone());
        if let Some(handle) = target {
            match relaunch.first_prompt {
                FirstPrompt::No => {
                    channel
                        .deliver(self, &handle, relaunch.channel, text)
                        .await?
                }
                FirstPrompt::Send => self.send_first_prompt(&handle, text).await?,
                FirstPrompt::Recover => self.recover_first_prompt(&handle, text).await?,
            }
            return Ok(Delivery {
                handle,
                relaunched: false,
                resumed: false,
            });
        }
        // An earlier attempt at this event, or a binding to one conversation,
        // is settled in that conversation: a session-bound channel with one
        // outstanding is resumed or nothing, never started afresh.
        let recorded = relaunch
            .channel
            .is_some_and(|(mailbox, sequence)| channel.has_record(mailbox, sequence, text));
        let held = channel.session_bound()
            && (recorded
                || relaunch
                    .channel
                    .is_some_and(|(mailbox, _)| channel.has_binding(mailbox)));
        if held && relaunch.resume_command.is_none() {
            bail!(
                "{} has an outstanding native delivery journal but no saved session to resume (no terminal fallback)",
                relaunch.harness
            );
        }
        let (handle, resumed, at_work) = self.start_again(workspace_id, relaunch, held).await?;
        // A channel that takes the event here has the harness's own
        // conversation to take it in (a Pi/OMP launch resumes its transcript).
        if channel
            .relaunched(self, &handle, relaunch.channel, recorded, resumed, text)
            .await?
        {
            return Ok(Delivery {
                handle,
                relaunched: true,
                resumed: true,
            });
        }
        let body = match relaunch.text {
            Some(full) if !resumed => full,
            _ => text,
        };
        // The harness was launched just above, so this is its first
        // prompt: confirm it landed rather than paste and hope. A resumed
        // one already at work is steered instead (#133): herdr does not
        // track turns, so waiting for `working` there says nothing. One
        // herdr reported no state for goes the confirmed way, since it
        // may as well be sitting on a dialog.
        if at_work {
            self.send_prompt(&handle, body).await?;
        } else {
            self.send_first_prompt(&handle, body).await?;
        }
        Ok(Delivery {
            handle,
            relaunched: true,
            resumed,
        })
    }
}

#[cfg(test)]
mod delivery_tests;
#[cfg(test)]
mod tests;
