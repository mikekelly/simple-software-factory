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
    /// Blocked on a question ssf does not know: the prompt goes in anyway.
    AskAnyway,
}

/// The decision [`Herdr::settle_harness`] makes from the state herdr
/// reported and what is on the screen. The screen decides first, whatever
/// the state: herdr 0.8.2 reports Codex sitting on its directory-trust
/// dialog as `idle` and Claude Code's as `blocked`, so a state of `idle`
/// is no promise that a prompt would reach the composer (#121). The one
/// state that settles it on its own is `working`: an agent already at
/// work is past any first-run dialog, and what its screen shows is its
/// own output.
pub fn settle_step(state: &str, screen: &str) -> Settle {
    if state == "working" {
        Settle::Ready
    } else if let Some(answer) = driver::trust_dialog(screen) {
        Settle::Answer(answer)
    } else if state == "blocked" {
        Settle::AskAnyway
    } else {
        Settle::Ready
    }
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
        let out = Command::new(crate::config::herdr_command_path(&self.cfg.command))
            .args(args)
            // The daemon may itself run inside a herdr pane; commands must
            // not default to it.
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_TAB_ID")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_ENV")
            .output()
            .await
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
            .map_err(|e| anyhow!("herdr is not answering (is a herdr session running?): {e:#}"))
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
            format!("{}-{name}", repo.rsplit('/').next().unwrap_or(repo))
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

    /// The visible screen of a pane with its colours, as herdr renders it
    /// (`pane read --source visible --format ansi`): what the web pane mirror
    /// shows.
    pub async fn screen_ansi(&self, pane_id: &str) -> Result<String> {
        self.run_raw(&[
            "pane", "read", pane_id, "--source", "visible", "--format", "ansi",
        ])
        .await
    }

    /// Type into a pane as a person at it would: `text` is written as it is
    /// (control characters and escape sequences included, which is what a
    /// terminal sends for Enter, Backspace or an arrow key), then `keys` are
    /// pressed by name (`enter`, `ctrl+c`, `esc`).
    pub async fn type_input(
        &self,
        pane_id: &str,
        text: Option<&str>,
        keys: &[String],
    ) -> Result<()> {
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            self.run(&["pane", "send-text", pane_id, text]).await?;
        }
        if !keys.is_empty() {
            let mut args = vec!["pane", "send-keys", pane_id];
            args.extend(keys.iter().map(String::as_str));
            self.run(&args).await?;
        }
        Ok(())
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
        for _ in 0..4 {
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
            let text = self.screen(pane_id).await.unwrap_or_default().join("\n");
            match settle_step(&state, &text) {
                Settle::Answer(answer) => {
                    info!(pane_id, state, "accepting the folder trust dialog");
                    self.answer_trust(pane_id, answer).await?;
                    continue;
                }
                Settle::AskAnyway => {
                    // Some other question: the prompt goes in anyway.
                    warn!(pane_id, "{harness} is at a question ssf does not know");
                    return Ok(state);
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
    /// it refuses because the agent is at a question, the text is pasted
    /// raw so the harness can queue it.
    pub async fn send_prompt(&self, pane_id: &str, text: &str) -> Result<()> {
        if !fits_one_argument(text) {
            warn!(
                pane_id,
                bytes = text.len(),
                "the prompt is too long for one herdr argument; pasting it raw"
            );
            return self.paste_prompt(pane_id, text).await;
        }
        match self
            .run(&["agent", "prompt", pane_id, text.trim_end()])
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if prompt_failure(&e.to_string()) == PromptFailure::Blocked => {
                self.paste_raw(pane_id, text).await
            }
            Err(e) => Err(e),
        }
    }

    /// Paste a prompt into the pane ourselves and submit it when herdr will not.
    async fn paste_raw(&self, pane_id: &str, text: &str) -> Result<()> {
        warn!(
            pane_id,
            "agent is blocked on a question; pasting the prompt raw"
        );
        self.paste_prompt(pane_id, text).await
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
                // Sent nothing: the harness is at a question, so paste it in
                // and let the harness queue it.
                PromptFailure::Blocked => return self.paste_raw(pane_id, text).await,
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
                self.launch(
                    workspace_id,
                    relaunch.command,
                    relaunch.title,
                    relaunch.harness,
                )
                .await?
            }
        };
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
