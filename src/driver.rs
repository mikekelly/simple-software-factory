//! The driver boundary: everything the engine asks of whatever runs the
//! agents. Orca was the only such thing; now it is one of two, chosen per
//! repository (`driver = "orca" | "herdr"`).
//!
//! A driver owns three things. A *project*: a checkout of the repository on
//! this machine (Orca keeps its own registry of those; the others clone
//! into `projects_dir`). A *workspace* per item: a git worktree on the
//! item's branch, with an id the engine stores and hands back. And the
//! *agent* in it: started with a command, given prompts, asked whether it
//! is alive or busy. The engine never looks behind the ids.
//!
//! The git side that the non-Orca drivers share (clone, worktree add and
//! remove, branch lookup) lives here too.

use anyhow::{Context, Result, anyhow, bail};
use std::path::{Path, PathBuf};
use tracing::info;

use crate::config::{Config, DriverKind};
use crate::herdr::Herdr;
use crate::orca::{Delivery, Orca, ProjectSetup, WorkspaceInfo, Worktree};
use crate::release::git;

/// How many lines at the bottom of the screen a trust dialog is looked
/// for in. Twelve covers the tallest of them (Claude Code's question, its
/// two options and the *Enter to confirm* line, with the box drawing
/// around them) and stops the wording matching where it is merely text
/// on the screen.
/// Two dozen rather than a dozen: Claude Code's bypass-permissions
/// acceptance needs two phrases that straddle its box, and a wrapped
/// paragraph inside it pushes the first one up the screen.
const TRUST_TAIL_LINES: usize = 24;

/// Keys that accept a harness's first-run trust question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustAnswer {
    /// The accepting option is already selected.
    Enter,
    /// The accepting option is the second one.
    DownEnter,
}

/// If `screen` shows a harness asking whether to trust the worktree, the keys
/// that say yes. The default launch commands answer this with a flag where
/// one exists (Gemini, Pi, Copilot); Claude Code and Codex have none, and
/// a `repo.command` may drop the flags, so the drivers answer the dialog
/// from the screen. Claude Code preselects *No, exit*; Codex preselects
/// *Yes, continue*; Gemini and Pi preselect *Trust*. Claude Code's one-time
/// acceptance of its bypass-permissions mode (shown on a machine that never
/// ran it that way, such as a fresh VM) is answered the same way.
///
/// Only the bottom [`TRUST_TAIL_LINES`] non-empty lines are looked at,
/// the way `login_dialog_in` bounds its own search: a dialog's options and
/// its *Press enter* line sit at the bottom of the screen, while the same
/// words in something the agent is showing -- a prompt of ssf's own in the
/// composer, a file it is reading -- scroll past above (#121).
pub fn trust_dialog(screen: &str) -> Option<TrustAnswer> {
    let lines: Vec<&str> = screen
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let start = lines.len().saturating_sub(TRUST_TAIL_LINES);
    let text = lines[start..].join("\n").to_lowercase();
    if text.contains("trust this folder")
        || (text.contains("bypass permissions mode") && text.contains("yes, i accept"))
    {
        Some(TrustAnswer::DownEnter)
    } else if text.contains("trust the contents of this directory")
        || text.contains("trust the files in this folder")
        || text.contains("trust project folder")
    {
        Some(TrustAnswer::Enter)
    } else {
        None
    }
}

/// How many lines at the bottom of the screen a login prompt is looked
/// for in: the harness's own status line, its answer to the last prompt
/// and its login screen all sit there, while an agent quoting the same
/// words in a file it is reading scrolls past above.
const LOGIN_TAIL_LINES: usize = 15;

/// If `screen` shows `harness` asking for a login (an expired session, a
/// revoked token, or a fresh machine with no credential), what it says.
/// A session whose screen shows this is blocked on auth, not idle:
/// prompts pasted into it are lost, and nothing inside the session can
/// fix it. The phrases come from the harnesses themselves (Claude Code
/// 2.1.258, Codex 0.152.0, Gemini 0.57.0, Grok 1.0, Pi 0.84, Oh My Pi,
/// OpenCode 1.18, Crush 0.92, seen live with an empty config home) and
/// from Claude Code's own list of errors a person has to fix.
///
/// Two things keep an agent's own screen from tripping this: only the
/// bottom of the screen counts, and a line inside echoed `[ssf]` text (a
/// pasted prompt, or activity delivered from the item, where a person may
/// well have quoted the phrase) is skipped: from a line carrying `[ssf]`
/// through the bullet and quote lines (`- `, `> `) that follow it. Every
/// string ssf itself writes into a terminal or that agents read stays
/// free of these phrases (`prompt::login_back_prompt`, the `blocked` and
/// `unblocked` event posts, `BlockedView::describe`, `SessionBlocked`),
/// which `engine::tests::ssf_texts_never_look_like_a_login_prompt` pins.
pub fn login_dialog(harness: &str, screen: &str) -> Option<String> {
    login_dialog_in(harness, screen)
}

/// The harnesses `login_dialog` knows the sign-in prompts of.
pub const HARNESSES: &[&str] = &[
    "claude", "codex", "gemini", "copilot", "grok", "pi", "omp", "opencode", "crush",
];

/// Would `text`, shown at the bottom of any harness's screen, pass for
/// that harness's sign-in prompt? For text ssf is about to write down
/// that came from elsewhere (an error message, say) and might be quoted
/// on a screen later.
pub fn quotes_login_prompt(text: &str) -> bool {
    login_prompt_line(text).is_some()
}

/// The line of `text` that carries any harness's sign-in phrase, as it is
/// written there, for a message that has to name what is wrong (redact it
/// with `redact_login_phrases` before showing it anywhere a harness
/// screen is read).
///
/// The whole text is read, line by line, unlike [`login_dialog`], which
/// judges a screen: there only the bottom counts and an echoed `[ssf]`
/// block is skipped, because an agent quoting the words on its own screen
/// is not a sign-in prompt. Text ssf is about to write down or paste
/// somewhere gets no such benefit of the doubt: a phrase forty lines into
/// a handover summary is still a phrase that can end up at the bottom of
/// a screen, and `[ssf]` in it proves nothing about who wrote it.
pub fn login_prompt_line(text: &str) -> Option<String> {
    let phrases = COMMON_LOGIN_PHRASES
        .iter()
        .chain(HARNESSES.iter().flat_map(|h| login_phrases(h).iter()));
    text.lines().map(str::trim).find_map(|line| {
        let lower = line.to_lowercase();
        phrases.clone().find(|p| lower.contains(**p))?;
        Some(
            line.trim_matches(|c: char| c == '\u{2502}' || c == '\u{2503}' || c.is_whitespace())
                .chars()
                .take(120)
                .collect(),
        )
    })
}

/// What every harness says one way or another when it is not signed in.
const COMMON_LOGIN_PHRASES: &[&str] = &["not logged in"];

/// The phrases (lowercase) `harness` shows at its sign-in prompt.
fn login_phrases(harness: &str) -> &'static [&'static str] {
    match harness {
        "claude" => &[
            "login expired",
            "run /login",
            "select login method",
            "oauth token expired",
            "oauth token revoked",
            "run claude auth login",
            "invalid api key",
        ],
        "codex" => &[
            "sign in with chatgpt",
            "re-run codex login",
            "run codex login",
            "provide your own api key",
        ],
        "gemini" => &[
            "how would you like to authenticate",
            "no authentication method selected",
            "sign in with google",
        ],
        "copilot" => &["run /login"],
        "grok" => &[
            "approve in your browser to finish signing in",
            "waiting for approval",
        ],
        "pi" | "omp" => &[
            "use /login to log into a provider",
            "no models available",
            "select provider to login",
            "set up your providers",
        ],
        "opencode" => &["run /connect to add an ai provider"],
        "crush" => &["let's choose a provider and model"],
        _ => &[],
    }
}

/// `text` with every sign-in phrase of every harness (and the common
/// one) replaced by `[…]`, matched without regard to case, so an error
/// message can be written down without the words that would make it
/// pass for a login prompt, and without losing the rest of it. Check the
/// result with `quotes_login_prompt`: a phrase can survive in another
/// spelling.
pub fn redact_login_phrases(text: &str) -> String {
    let mut out = text.to_string();
    for phrase in COMMON_LOGIN_PHRASES
        .iter()
        .chain(HARNESSES.iter().flat_map(|h| login_phrases(h).iter()))
    {
        // The phrases are ASCII, so the ASCII-lowered copy keeps every
        // byte offset of the original.
        let lowered = out.to_ascii_lowercase();
        let mut rebuilt = String::with_capacity(out.len());
        let mut from = 0;
        while let Some(rel) = lowered[from..].find(phrase) {
            let at = from + rel;
            rebuilt.push_str(&out[from..at]);
            rebuilt.push_str("[\u{2026}]");
            from = at + phrase.len();
        }
        rebuilt.push_str(&out[from..]);
        out = rebuilt;
    }
    out
}

fn login_dialog_in(harness: &str, screen: &str) -> Option<String> {
    let tail: Vec<&str> = screen
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let start = tail.len().saturating_sub(LOGIN_TAIL_LINES);
    let mut in_echo = false;
    let candidates: Vec<&str> = tail[start..]
        .iter()
        .copied()
        .filter(|l| {
            if l.contains("[ssf]") {
                in_echo = true;
                return false;
            }
            if in_echo && (l.starts_with('-') || l.starts_with('>')) {
                return false;
            }
            in_echo = false;
            true
        })
        .collect();
    let text = candidates.join("\n").to_lowercase();
    let common = COMMON_LOGIN_PHRASES;
    let own = login_phrases(harness);
    let hit = common
        .iter()
        .chain(own.iter())
        .find(|p| text.contains(**p))?;
    // The line it was found on, as the harness printed it.
    let line = candidates
        .iter()
        .find(|l| l.to_lowercase().contains(hit))
        .map(|l| l.trim_matches(|c: char| c == '│' || c == '┃' || c.is_whitespace()))
        .unwrap_or(hit);
    Some(line.chars().take(120).collect())
}

/// One configured driver.
#[derive(Clone)]
pub enum Driver {
    Orca(Orca),
    Herdr(Herdr),
    /// For tests: a driver whose workspaces, agents and screens are set by
    /// the test (see `StubDriver`).
    #[cfg(test)]
    Stub(StubDriver),
}

/// How a prompt is delivered when the agent has to be started again: the
/// commands to start fresh or to resume, and what to send in each case.
pub struct Relaunch<'a> {
    /// Command that starts the harness from scratch.
    pub command: &'a str,
    /// Command that resumes the harness's earlier conversation, if one is
    /// known and the harness can.
    pub resume_command: Option<&'a str>,
    pub harness: &'a str,
    pub title: &'a str,
    /// What a fresh harness gets instead of the prompt (the whole story).
    pub text: Option<&'a str>,
}

/// A launch command fit for a log line: the bot token that
/// `Engine::launch_command` puts in front of the wrapper (as
/// `SSF_GITHUB_TOKEN='...'`) is replaced by `<redacted>`. Everything else
/// is left as it is.
pub fn redacted(command: &str) -> String {
    const KEY: &str = "SSF_GITHUB_TOKEN=";
    let mut out = String::with_capacity(command.len());
    let mut rest = command;
    while let Some(at) = rest.find(KEY) {
        let start = at + KEY.len();
        out.push_str(&rest[..start]);
        let value = &rest[start..];
        let len = if let Some(inner) = value.strip_prefix('\'') {
            // A quoted value ends at its closing quote (shell_quote writes
            // an embedded quote as '\'', which also ends here).
            inner.find('\'').map(|i| i + 2).unwrap_or(value.len())
        } else {
            value.find(char::is_whitespace).unwrap_or(value.len())
        };
        out.push_str("<redacted>");
        rest = &value[len..];
    }
    out.push_str(rest);
    out
}

/// `redacted` over a CLI argument list.
pub fn redacted_args(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| redacted(a)).collect()
}

impl Driver {
    pub fn new(kind: DriverKind, cfg: &Config) -> Self {
        match kind {
            DriverKind::Orca => Driver::Orca(Orca::new(cfg.orca.clone())),
            DriverKind::Herdr => Driver::Herdr(Herdr::new(cfg.herdr.clone())),
        }
    }

    pub fn kind(&self) -> DriverKind {
        match self {
            Driver::Orca(_) => DriverKind::Orca,
            Driver::Herdr(_) => DriverKind::Herdr,
            #[cfg(test)]
            Driver::Stub(d) => d.kind,
        }
    }

    pub fn label(&self) -> &'static str {
        self.kind().label()
    }

    /// Could this driver have written `repo_id`? A record that says which
    /// driver made its workspace is not judged by this; one from before
    /// that was kept is (see `Engine::drop_foreign_binding`).
    pub fn owns_repo_id(&self, repo_id: &str) -> bool {
        match self {
            // The stub's own id, or a real driver's shape for the kind the
            // stub stands in for.
            #[cfg(test)]
            Driver::Stub(_) => {
                repo_id == "stub" || DriverKind::of_repo_id(repo_id) == Some(self.kind())
            }
            _ => DriverKind::of_repo_id(repo_id) == Some(self.kind()),
        }
    }

    /// The executable the driver runs.
    pub fn command(&self) -> &str {
        match self {
            Driver::Orca(d) => d.command(),
            Driver::Herdr(d) => d.command(),
            #[cfg(test)]
            Driver::Stub(_) => "stub",
        }
    }

    /// Is the driver there and ready to take commands?
    pub async fn status(&self) -> Result<()> {
        match self {
            Driver::Orca(d) => d.status().await.map(|_| ()),
            Driver::Herdr(d) => d.status().await,
            #[cfg(test)]
            Driver::Stub(_) => Ok(()),
        }
    }

    /// Make sure the repository has a checkout to make workspaces from,
    /// cloning it or importing `existing_path` when it has none.
    pub async fn ensure_project(
        &self,
        owner: &str,
        repo: &str,
        clone_url: &str,
        existing_path: Option<&str>,
        projects_dir: &Path,
    ) -> Result<ProjectSetup> {
        match self {
            Driver::Orca(d) => {
                d.ensure_project(owner, repo, clone_url, existing_path, projects_dir)
                    .await
            }
            Driver::Herdr(_) => {
                ensure_local_checkout(repo, clone_url, existing_path, projects_dir).await
            }
            #[cfg(test)]
            Driver::Stub(d) => d.ensure_project(),
        }
    }

    /// The workspace already bound to item `number`, if the driver can tell.
    pub async fn find_worktree_for_issue(
        &self,
        repo_id: &str,
        number: u64,
    ) -> Result<Option<Worktree>> {
        match self {
            Driver::Orca(d) => d.find_worktree_for_issue(repo_id, number).await,
            Driver::Herdr(d) => d.find_worktree_for_issue(repo_id, number).await,
            // The stub fails on a checkout it does not own, as the real
            // drivers do on another driver's id.
            #[cfg(test)]
            Driver::Stub(_) => {
                if repo_id != "stub" {
                    bail!("checkout {repo_id} is not a directory (a repo id from another driver?)");
                }
                Ok(None)
            }
        }
    }

    /// Create the workspace for an item: a checkout named `name` on a branch
    /// of its own, from `base_branch` (the driver's default base without).
    pub async fn create_worktree(
        &self,
        repo_id: &str,
        name: &str,
        number: u64,
        comment: &str,
        base_branch: Option<&str>,
    ) -> Result<Worktree> {
        match self {
            Driver::Orca(d) => {
                d.create_worktree(repo_id, name, number, None, comment, base_branch)
                    .await
            }
            Driver::Herdr(d) => d.create_worktree(repo_id, name, comment, base_branch).await,
            #[cfg(test)]
            Driver::Stub(d) => d.create_worktree(name),
        }
    }

    /// Filesystem path of the repository's main checkout.
    pub async fn repo_path(&self, repo_id: &str) -> Result<String> {
        match self {
            Driver::Orca(d) => d.repo_path(repo_id).await,
            Driver::Herdr(_) => Ok(repo_root(repo_id).to_string()),
            #[cfg(test)]
            Driver::Stub(_) => Ok("/stub".into()),
        }
    }

    /// A ref to base a re-created workspace on: the local branch if it still
    /// exists, else its remote-tracking copy.
    pub async fn existing_branch_ref(&self, repo_id: &str, branch: &str) -> Result<Option<String>> {
        let path = self.repo_path(repo_id).await?;
        existing_branch_ref_at(&path, branch).await
    }

    /// Start the harness in a workspace that has no agent yet and give it
    /// its first prompt. Returns the handle later prompts go to.
    pub async fn start(
        &self,
        worktree_id: &str,
        command: &str,
        title: &str,
        harness: &str,
        text: &str,
    ) -> Result<String> {
        match self {
            Driver::Orca(d) => {
                let handle = d
                    .launch_in_worktree(worktree_id, command, title, harness)
                    .await?;
                d.send_prompt(&handle, text).await?;
                Ok(handle)
            }
            Driver::Herdr(d) => {
                let handle = d.launch(worktree_id, command, title, harness).await?;
                d.send_first_prompt(&handle, text).await?;
                Ok(handle)
            }
            #[cfg(test)]
            Driver::Stub(d) => d.start(worktree_id, command, harness, text),
        }
    }

    /// Whether the workspace still exists.
    pub async fn worktree_exists(&self, worktree_id: &str) -> Result<bool> {
        match self {
            Driver::Orca(d) => d.worktree_exists(worktree_id).await,
            Driver::Herdr(d) => d.worktree_exists(worktree_id).await,
            #[cfg(test)]
            Driver::Stub(d) => Ok(d.worktree_exists(worktree_id)),
        }
    }

    /// Every workspace the driver knows about, with the agents in it.
    pub async fn ps(&self) -> Result<Vec<WorkspaceInfo>> {
        match self {
            Driver::Orca(d) => d.ps().await,
            Driver::Herdr(d) => d.ps().await,
            #[cfg(test)]
            Driver::Stub(d) => Ok(d.ps()),
        }
    }

    /// Stop the workspace's agent and remove the workspace.
    pub async fn remove_worktree(&self, worktree_id: &str) -> Result<()> {
        match self {
            Driver::Orca(d) => d.remove_worktree(worktree_id).await,
            Driver::Herdr(d) => d.remove_worktree(worktree_id).await,
            #[cfg(test)]
            Driver::Stub(d) => {
                d.remove_worktree(worktree_id);
                Ok(())
            }
        }
    }

    /// A note on the workspace for people looking at the driver's UI.
    pub async fn set_comment(&self, worktree_id: &str, comment: &str) -> Result<()> {
        match self {
            Driver::Orca(d) => d.set_comment(worktree_id, comment).await,
            Driver::Herdr(d) => d.set_comment(worktree_id, comment).await,
            #[cfg(test)]
            Driver::Stub(_) => Ok(()),
        }
    }

    /// Board column (`in-progress`, `completed`) where the driver has one.
    pub async fn set_status(&self, worktree_id: &str, status: &str) -> Result<()> {
        match self {
            Driver::Orca(d) => d.set_status(worktree_id, status).await,
            Driver::Herdr(d) => d.set_status(worktree_id, status).await,
            #[cfg(test)]
            Driver::Stub(_) => Ok(()),
        }
    }

    /// Is there an agent in the workspace that a prompt would reach without
    /// starting one?
    pub async fn has_live_agent(&self, worktree_id: &str) -> Result<bool> {
        match self {
            Driver::Orca(d) => d.has_live_agent(worktree_id).await,
            Driver::Herdr(d) => d.has_live_agent(worktree_id).await,
            #[cfg(test)]
            Driver::Stub(d) => Ok(d.live_handle(worktree_id).is_some()),
        }
    }

    /// The terminal (pane) of the live agent in the workspace, preferring
    /// `preferred` when it is still one; `None` when [`Driver::deliver`]
    /// would have to start the harness again.
    pub async fn live_handle(
        &self,
        worktree_id: &str,
        preferred: Option<&str>,
    ) -> Result<Option<String>> {
        match self {
            Driver::Orca(d) => d.live_handle(worktree_id, preferred).await,
            Driver::Herdr(d) => d.live_handle(worktree_id, preferred).await,
            #[cfg(test)]
            Driver::Stub(d) => Ok(d.live_handle(worktree_id)),
        }
    }

    /// The rendered screen of a terminal, as lines.
    pub async fn screen(&self, handle: &str) -> Result<Vec<String>> {
        match self {
            Driver::Orca(d) => d.screen(handle).await,
            Driver::Herdr(d) => d.screen(handle).await,
            #[cfg(test)]
            Driver::Stub(d) => Ok(d.screen(handle)),
        }
    }

    /// Quit the agent in a terminal (a harness stuck on a login prompt,
    /// say) so the next delivery starts it again. The workspace stays.
    pub async fn stop_agent(&self, worktree_id: &str, handle: &str) -> Result<()> {
        match self {
            Driver::Orca(d) => d.stop_agent(worktree_id, handle).await,
            Driver::Herdr(d) => d.stop_agent(worktree_id, handle).await,
            #[cfg(test)]
            Driver::Stub(d) => {
                d.stop_agent(worktree_id, handle);
                Ok(())
            }
        }
    }

    /// Deliver a prompt to the workspace's agent, starting the harness again
    /// (resuming its conversation when it can) if it is gone.
    pub async fn deliver(
        &self,
        worktree_id: &str,
        preferred_handle: Option<&str>,
        relaunch: Relaunch<'_>,
        text: &str,
    ) -> Result<Delivery> {
        match self {
            Driver::Orca(d) => {
                d.deliver(
                    worktree_id,
                    preferred_handle,
                    relaunch.command,
                    relaunch.resume_command,
                    relaunch.harness,
                    relaunch.title,
                    text,
                    relaunch.text,
                )
                .await
            }
            Driver::Herdr(d) => {
                d.deliver(worktree_id, preferred_handle, &relaunch, text)
                    .await
            }
            #[cfg(test)]
            Driver::Stub(d) => d.deliver(worktree_id, &relaunch, text),
        }
    }
}

/// The drivers a daemon runs, one per kind in use.
#[derive(Clone)]
pub struct Drivers {
    list: Vec<Driver>,
}

impl Drivers {
    pub fn from_config(cfg: &Config) -> Self {
        let list = cfg
            .drivers_in_use()
            .into_iter()
            .map(|k| Driver::new(k, cfg))
            .collect();
        Self { list }
    }

    /// For tests: a set with the given drivers.
    #[cfg(test)]
    pub fn from_list(list: Vec<Driver>) -> Self {
        Self { list }
    }

    /// The kinds in the set, in `DriverKind` order.
    pub fn kinds(&self) -> Vec<DriverKind> {
        let mut out: Vec<DriverKind> = self.list.iter().map(Driver::kind).collect();
        out.sort();
        out.dedup();
        out
    }

    pub fn get(&self, kind: DriverKind) -> Option<&Driver> {
        self.list.iter().find(|d| d.kind() == kind)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Driver> {
        self.list.iter()
    }
}

// ---- a driver for tests -------------------------------------------------

/// A driver for the engine tests: no process behind it, just what the
/// test says exists. Workspaces are created on demand, an agent is "live"
/// once started or delivered to, every screen is what the test set for
/// that handle (or the screen a relaunched harness shows), and every
/// operation is logged so a test can assert what the engine did.
#[cfg(test)]
#[derive(Clone, Default)]
pub struct StubDriver {
    pub kind: DriverKind,
    inner: std::sync::Arc<std::sync::Mutex<StubState>>,
}

/// What a stub resume does when a delivery finds no live agent and has a
/// `resume_command`: the three shapes the herdr driver tells apart (#131).
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StubResume {
    /// The resumed agent settles: idle, or at work on its queued messages
    /// for longer than the wait, which is settled too.
    #[default]
    Settles,
    /// The harness exits at once (it could not find the session): nothing
    /// is left to stop, and a fresh harness follows in the same pane.
    Exits,
    /// The resumed agent is alive but the wait ran out without herdr
    /// saying what it was doing: it is kept, as the resumed conversation
    /// (#133); no fresh harness is started.
    Unsettled,
}

#[cfg(test)]
#[derive(Default)]
pub struct StubState {
    /// What the next resume does.
    pub resume: StubResume,
    pub worktrees: std::collections::BTreeSet<String>,
    /// worktree id -> handle of its live agent.
    pub live: std::collections::BTreeMap<String, String>,
    pub working: std::collections::BTreeSet<String>,
    pub screens: std::collections::BTreeMap<String, Vec<String>>,
    /// The screen a harness started again shows (a login prompt, say).
    pub relaunch_screen: Vec<String>,
    /// `stop:<handle>`, `deliver:<worktree>:<first line>`,
    /// `relaunch:<worktree>:<resumed>`; a resume given up on logs
    /// `resume-exited:<worktree>` before the fresh `relaunch:<worktree>:false`,
    /// and one kept past the wait `resume-unsettled:<worktree>`.
    pub log: Vec<String>,
    /// Every harness started, as `<harness>:<command>`: what a start or a
    /// relaunch would run, for the tests about per-item overrides.
    pub launches: Vec<String>,
    /// The whole text of every start and every delivery (the `log` keeps
    /// only its first line), for the tests about what a session is told.
    pub prompts: Vec<String>,
    /// When set, the next `start` fails with this message: a harness that
    /// cannot be started at all.
    pub start_error: Option<String>,
    /// When set, the next prompt delivery fails with this message. Tests use
    /// this to exercise the daemon's retry bookkeeping without changing a
    /// real driver's delivery semantics.
    pub deliver_error: Option<String>,
    handles: u32,
}

#[cfg(test)]
impl StubDriver {
    pub fn new(kind: DriverKind) -> Self {
        Self {
            kind,
            inner: Default::default(),
        }
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut StubState) -> T) -> T {
        f(&mut self.inner.lock().unwrap())
    }

    /// A workspace with a live, idle agent showing `screen`.
    pub fn seed(&self, worktree_id: &str, handle: &str, screen: &[&str]) {
        self.with(|s| {
            s.worktrees.insert(worktree_id.into());
            s.live.insert(worktree_id.into(), handle.into());
            s.screens.insert(
                handle.into(),
                screen.iter().map(|l| l.to_string()).collect(),
            );
        });
    }

    pub fn log(&self) -> Vec<String> {
        self.with(|s| std::mem::take(&mut s.log))
    }

    /// The harnesses started since the last call, as `<harness>:<command>`.
    pub fn launches(&self) -> Vec<String> {
        self.with(|s| std::mem::take(&mut s.launches))
    }

    /// The first messages of the starts since the last call, whole.
    pub fn prompts(&self) -> Vec<String> {
        self.with(|s| std::mem::take(&mut s.prompts))
    }

    fn ensure_project(&self) -> Result<ProjectSetup> {
        Ok(ProjectSetup {
            repo_id: "stub".into(),
            path: "/stub".into(),
        })
    }

    fn create_worktree(&self, name: &str) -> Result<Worktree> {
        let id = format!("stub::/stub.worktrees/{name}");
        self.with(|s| s.worktrees.insert(id.clone()));
        Ok(Worktree {
            path: format!("/stub.worktrees/{name}"),
            branch: Some(format!("refs/heads/{}", branch_for(name))),
            id,
        })
    }

    fn new_handle(s: &mut StubState, worktree_id: &str) -> String {
        s.handles += 1;
        let h = format!("t{}", s.handles);
        s.live.insert(worktree_id.into(), h.clone());
        let screen = s.relaunch_screen.clone();
        s.screens.insert(h.clone(), screen);
        h
    }

    fn start(&self, worktree_id: &str, command: &str, harness: &str, text: &str) -> Result<String> {
        self.with(|s| {
            if let Some(why) = s.start_error.take() {
                bail!("{why}");
            }
            let h = Self::new_handle(s, worktree_id);
            s.prompts.push(text.to_string());
            s.launches.push(format!("{harness}:{command}"));
            s.log
                .push(format!("start:{worktree_id}:{}", first_line(text)));
            Ok(h)
        })
    }

    fn worktree_exists(&self, id: &str) -> bool {
        self.with(|s| s.worktrees.contains(id))
    }

    fn ps(&self) -> Vec<WorkspaceInfo> {
        self.with(|s| {
            s.worktrees
                .iter()
                .map(|id| WorkspaceInfo {
                    worktree_id: id.clone(),
                    repo_id: "stub".into(),
                    path: repo_root(id).to_string(),
                    agents: s
                        .live
                        .get(id)
                        .map(|_| crate::orca::AgentInfo {
                            state: if s.working.contains(id) {
                                "working".into()
                            } else {
                                "open".into()
                            },
                            ..Default::default()
                        })
                        .into_iter()
                        .collect(),
                    ..Default::default()
                })
                .collect()
        })
    }

    fn remove_worktree(&self, id: &str) {
        self.with(|s| {
            s.worktrees.remove(id);
            s.live.remove(id);
            s.log.push(format!("remove:{id}"));
        });
    }

    fn live_handle(&self, id: &str) -> Option<String> {
        self.with(|s| s.live.get(id).cloned())
    }

    fn screen(&self, handle: &str) -> Vec<String> {
        self.with(|s| s.screens.get(handle).cloned().unwrap_or_default())
    }

    fn stop_agent(&self, worktree_id: &str, handle: &str) {
        self.with(|s| {
            s.live.remove(worktree_id);
            s.working.remove(worktree_id);
            s.log.push(format!("stop:{handle}"));
        });
    }

    fn deliver(&self, worktree_id: &str, relaunch: &Relaunch<'_>, text: &str) -> Result<Delivery> {
        self.with(|s| {
            if let Some(why) = s.deliver_error.take() {
                bail!("{why}");
            }
            if !s.worktrees.contains(worktree_id) {
                bail!("{worktree_id}: no such workspace");
            }
            if let Some(h) = s.live.get(worktree_id).cloned() {
                s.prompts.push(text.to_string());
                s.log
                    .push(format!("deliver:{worktree_id}:{}", first_line(text)));
                return Ok(Delivery {
                    handle: h,
                    relaunched: false,
                    resumed: false,
                });
            }
            let mut resumed = false;
            if let Some(cmd) = relaunch.resume_command {
                s.launches.push(format!("{}:{cmd}", relaunch.harness));
                match s.resume {
                    StubResume::Settles => resumed = true,
                    StubResume::Exits => s.log.push(format!("resume-exited:{worktree_id}")),
                    StubResume::Unsettled => {
                        s.log.push(format!("resume-unsettled:{worktree_id}"));
                        resumed = true;
                    }
                }
            }
            if !resumed {
                s.launches
                    .push(format!("{}:{}", relaunch.harness, relaunch.command));
            }
            let h = Self::new_handle(s, worktree_id);
            s.log.push(format!("relaunch:{worktree_id}:{resumed}"));
            let body = match relaunch.text {
                Some(full) if !resumed => full,
                _ => text,
            };
            s.prompts.push(body.to_string());
            s.log
                .push(format!("deliver:{worktree_id}:{}", first_line(body)));
            Ok(Delivery {
                handle: h,
                relaunched: true,
                resumed,
            })
        })
    }
}

#[cfg(test)]
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").chars().take(60).collect()
}

// ---- the git side shared by the drivers that keep checkouts themselves ----

/// The checkout a repo id names (a plain path for the non-Orca drivers;
/// tolerant of Orca's `<repo>::<path>` form).
pub fn repo_root(id: &str) -> &str {
    id.split_once("::").map(|(r, _)| r).unwrap_or(id)
}

/// Directory a checkout's worktrees go in: next to it, as `<name>.worktrees`.
pub fn worktrees_dir(repo_root: &str) -> PathBuf {
    let root = Path::new(repo_root);
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".into());
    root.parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(format!("{name}.worktrees"))
}

/// The checkout a worktree path belongs to, by ssf's layout: the
/// `<root>` of `<root>.worktrees/<name>`. `None` for a path elsewhere.
pub fn checkout_of_worktree(path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    let dir = p.parent()?;
    let name = dir.file_name()?.to_string_lossy().to_string();
    let base = dir.parent()?;
    name.strip_suffix(".worktrees").map(|n| base.join(n))
}

/// Branch a workspace named `name` works on.
pub fn branch_for(name: &str) -> String {
    format!("bot/{name}")
}

/// The item number a workspace name (`issue-12-...`, `pr-12`) was made
/// for. A `review-12-...` worktree (the reviewer sessions of before #115)
/// is nobody's: it is not taken for the pull request's own workspace.
pub fn number_of_name(name: &str) -> Option<u64> {
    let (prefix, rest) = name.split_once('-')?;
    if !matches!(prefix, "issue" | "pr") {
        return None;
    }
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let after = &rest[digits.len()..];
    if digits.is_empty() || !(after.is_empty() || after.starts_with('-')) {
        return None;
    }
    digits.parse().ok()
}

/// A clone of the repository under `projects_dir` (or `existing_path`),
/// made if missing. The repo id is the checkout path.
pub async fn ensure_local_checkout(
    repo: &str,
    clone_url: &str,
    existing_path: Option<&str>,
    projects_dir: &Path,
) -> Result<ProjectSetup> {
    let path = match existing_path {
        Some(p) => PathBuf::from(p),
        None => projects_dir.join(repo),
    };
    if !path.join(".git").exists() {
        if existing_path.is_some() {
            bail!("{} is not a git checkout", path.display());
        }
        std::fs::create_dir_all(projects_dir)
            .with_context(|| format!("creating {}", projects_dir.display()))?;
        info!(clone_url, dest = %path.display(), "cloning repository");
        let out = tokio::process::Command::new("git")
            .args(["clone", clone_url])
            .arg(&path)
            .output()
            .await
            .context("running git clone")?;
        if !out.status.success() {
            bail!(
                "git clone {clone_url} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    let path = path
        .canonicalize()
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    Ok(ProjectSetup {
        repo_id: path.clone(),
        path,
    })
}

/// The local branch if it exists, else its remote-tracking copy.
pub async fn existing_branch_ref_at(path: &str, branch: &str) -> Result<Option<String>> {
    for (full, short) in [
        (format!("refs/heads/{branch}"), branch.to_string()),
        (
            format!("refs/remotes/origin/{branch}"),
            format!("origin/{branch}"),
        ),
    ] {
        if git(path, &["rev-parse", "--verify", "--quiet", &full])
            .await
            .is_ok()
        {
            return Ok(Some(short));
        }
    }
    Ok(None)
}

/// The repository's default base ref: `origin/HEAD` if known, else the
/// checked-out branch of the main checkout.
pub async fn default_base(repo_root: &str) -> Result<String> {
    if let Ok(r) = git(
        repo_root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .await
        && !r.is_empty()
    {
        return Ok(r);
    }
    let b = git(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    if b.is_empty() || b == "HEAD" {
        bail!("cannot tell the base branch of {repo_root}");
    }
    Ok(b)
}

/// A local worktree named `name` of the checkout at `repo_root`, on branch
/// `bot/<name>`: the branch is made from `base` when it does not exist yet,
/// and checked out as it is when it does (a re-created workspace keeps its
/// history). Returns the worktree's path and full branch ref.
pub async fn add_local_worktree(
    repo_root: &str,
    name: &str,
    base: Option<&str>,
) -> Result<(String, String)> {
    let dir = worktrees_dir(repo_root);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(name);
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    let branch = branch_for(name);
    let path_s = path.to_string_lossy().to_string();
    let _ = git(repo_root, &["worktree", "prune"]).await;
    let have_branch = git(
        repo_root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .await
    .is_ok();
    if have_branch && base.is_none_or(|b| b == branch) {
        git(repo_root, &["worktree", "add", &path_s, &branch]).await?;
    } else {
        let base = match base {
            Some(b) => b.to_string(),
            None => default_base(repo_root).await?,
        };
        if let Some(remote) = base.strip_prefix("origin/") {
            let _ = git(repo_root, &["fetch", "origin", remote]).await;
        }
        let flag = if have_branch { "-B" } else { "-b" };
        git(
            repo_root,
            &["worktree", "add", flag, &branch, &path_s, &base],
        )
        .await?;
    }
    Ok((path_s, format!("refs/heads/{branch}")))
}

/// Remove a local worktree (its branch stays).
pub async fn remove_local_worktree(repo_root: &str, path: &str) -> Result<()> {
    if Path::new(path).exists() {
        git(repo_root, &["worktree", "remove", "--force", path]).await?;
    }
    let _ = git(repo_root, &["worktree", "prune"]).await;
    Ok(())
}

/// Remove a linked worktree no driver has a workspace on any more, asking
/// git which checkout it belongs to (its branch stays).
pub async fn remove_stray_worktree(path: &str) -> Result<()> {
    // Relative on older gits (`--path-format=absolute` needs 2.31), and
    // then relative to the worktree.
    let common = git(path, &["rev-parse", "--git-common-dir"])
        .await
        .with_context(|| format!("{path}: not a git worktree"))?;
    let common = Path::new(path).join(common);
    let root = common
        .parent()
        .with_context(|| format!("{path}: odd git dir {}", common.display()))?
        .to_string_lossy()
        .to_string();
    remove_local_worktree(&root, path).await
}

/// A worktree of the checkout, as `git worktree list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorktree {
    pub path: String,
    /// Full ref (`refs/heads/...`), or `None` when detached.
    pub branch: Option<String>,
    /// The commit checked out.
    pub head: Option<String>,
}

/// Parse `git worktree list --porcelain`.
pub fn parse_worktree_list(text: &str) -> Vec<LocalWorktree> {
    let mut out = Vec::new();
    let mut cur: Option<LocalWorktree> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(c) = cur.take() {
                out.push(c);
            }
            cur = Some(LocalWorktree {
                path: p.to_string(),
                branch: None,
                head: None,
            });
        } else if let Some(b) = line.strip_prefix("branch ")
            && let Some(c) = cur.as_mut()
        {
            c.branch = Some(b.to_string());
        } else if let Some(h) = line.strip_prefix("HEAD ")
            && let Some(c) = cur.as_mut()
        {
            c.head = Some(h.to_string());
        }
    }
    if let Some(c) = cur {
        out.push(c);
    }
    out
}

/// The linked worktrees of the checkout (not the checkout itself).
pub async fn local_worktrees(repo_root: &str) -> Result<Vec<LocalWorktree>> {
    // Said plainly rather than left to git's "cannot change to": the
    // usual way to get here is a repo id another driver wrote (an Orca
    // uuid) taken for a checkout path.
    if !Path::new(repo_root).is_dir() {
        bail!("checkout {repo_root} is not a directory (a repo id from another driver?)");
    }
    let text = git(repo_root, &["worktree", "list", "--porcelain"]).await?;
    let root = Path::new(repo_root)
        .canonicalize()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| repo_root.to_string());
    Ok(parse_worktree_list(&text)
        .into_iter()
        .filter(|w| w.path != root && w.path != repo_root)
        .collect())
}

/// The worktree made for item `number` under `repo_root`, by its name.
pub async fn find_local_worktree(repo_root: &str, number: u64) -> Result<Option<LocalWorktree>> {
    let dir = worktrees_dir(repo_root);
    for w in local_worktrees(repo_root).await? {
        let p = Path::new(&w.path);
        if p.parent() != Some(dir.as_path()) {
            continue;
        }
        let name = p.file_name().map(|n| n.to_string_lossy().to_string());
        if name.as_deref().and_then(number_of_name) == Some(number) {
            return Ok(Some(w));
        }
    }
    Ok(None)
}

pub fn err_no_field(what: &str, v: &serde_json::Value) -> anyhow::Error {
    anyhow!("{what}: {v}")
}

#[cfg(test)]
mod tests;
