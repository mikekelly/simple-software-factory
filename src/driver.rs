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
pub fn trust_dialog(screen: &str) -> Option<TrustAnswer> {
    let text = screen.to_lowercase();
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
    HARNESSES.iter().any(|h| login_dialog_in(h, text).is_some())
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
    // What every harness says one way or another.
    let common: &[&str] = &["not logged in"];
    let own: &[&str] = match harness {
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
    };
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
                d.send_prompt(&handle, text).await?;
                Ok(handle)
            }
            #[cfg(test)]
            Driver::Stub(d) => d.start(worktree_id, text),
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

#[cfg(test)]
#[derive(Default)]
pub struct StubState {
    pub worktrees: std::collections::BTreeSet<String>,
    /// worktree id -> handle of its live agent.
    pub live: std::collections::BTreeMap<String, String>,
    pub working: std::collections::BTreeSet<String>,
    pub screens: std::collections::BTreeMap<String, Vec<String>>,
    /// The screen a harness started again shows (a login prompt, say).
    pub relaunch_screen: Vec<String>,
    /// `stop:<handle>`, `deliver:<worktree>:<first line>`, `relaunch:<worktree>:<resumed>`.
    pub log: Vec<String>,
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

    fn start(&self, worktree_id: &str, text: &str) -> Result<String> {
        self.with(|s| {
            let h = Self::new_handle(s, worktree_id);
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
            if !s.worktrees.contains(worktree_id) {
                bail!("{worktree_id}: no such workspace");
            }
            if let Some(h) = s.live.get(worktree_id).cloned() {
                s.log
                    .push(format!("deliver:{worktree_id}:{}", first_line(text)));
                return Ok(Delivery {
                    handle: h,
                    relaunched: false,
                    resumed: false,
                });
            }
            let resumed = relaunch.resume_command.is_some();
            let h = Self::new_handle(s, worktree_id);
            s.log.push(format!("relaunch:{worktree_id}:{resumed}"));
            let body = match relaunch.text {
                Some(full) if !resumed => full,
                _ => text,
            };
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

/// A worktree of the checkout, as `git worktree list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorktree {
    pub path: String,
    /// Full ref (`refs/heads/...`), or `None` when detached.
    pub branch: Option<String>,
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
            });
        } else if let Some(b) = line.strip_prefix("branch ")
            && let Some(c) = cur.as_mut()
        {
            c.branch = Some(b.to_string());
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
mod tests {
    use super::*;

    #[test]
    fn log_lines_never_carry_the_token() {
        let wrapper = "SSF_CONFIG_DIR='/c' SSF_STATE_DIR='/s' SSF_GITHUB_TOKEN='gho_abc123' '/bin/ssf' launch --repo 'o/r' -- 'claude'";
        assert_eq!(
            redacted(wrapper),
            "SSF_CONFIG_DIR='/c' SSF_STATE_DIR='/s' SSF_GITHUB_TOKEN=<redacted> '/bin/ssf' launch --repo 'o/r' -- 'claude'"
        );
        // Unquoted, at the end, twice, and absent.
        assert_eq!(
            redacted("SSF_GITHUB_TOKEN=gho_x ssf"),
            "SSF_GITHUB_TOKEN=<redacted> ssf"
        );
        assert_eq!(
            redacted("A=1 SSF_GITHUB_TOKEN='gho_x'"),
            "A=1 SSF_GITHUB_TOKEN=<redacted>"
        );
        assert_eq!(
            redacted("SSF_GITHUB_TOKEN='a' SSF_GITHUB_TOKEN=b"),
            "SSF_GITHUB_TOKEN=<redacted> SSF_GITHUB_TOKEN=<redacted>"
        );
        assert_eq!(redacted("claude --model haiku"), "claude --model haiku");
        assert!(!redacted(wrapper).contains("gho_"));
    }

    #[tokio::test]
    async fn worktree_listing_refuses_a_repo_id_that_is_no_directory() {
        let err = local_worktrees("1b790ad2-4421-43dc-9f46-f7c09d0c321f")
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("is not a directory"), "{msg}");
        assert!(msg.contains("1b790ad2"), "{msg}");
        assert!(!msg.contains("cannot change to"), "{msg}");
    }

    #[test]
    fn a_repo_id_names_the_driver_that_wrote_it() {
        assert_eq!(
            DriverKind::of_repo_id("1b790ad2-4421-43dc-9f46-f7c09d0c321f"),
            Some(DriverKind::Orca)
        );
        assert_eq!(
            DriverKind::of_repo_id("/home/me/ssf/projects/widgets"),
            Some(DriverKind::Herdr)
        );
        assert_eq!(DriverKind::of_repo_id(""), None);
        assert_eq!(DriverKind::of_repo_id("relative/path"), None);
        let stub = Driver::Stub(StubDriver::new(DriverKind::Herdr));
        assert!(stub.owns_repo_id("stub"));
        assert!(stub.owns_repo_id("/home/me/ssf/projects/widgets"));
        assert!(!stub.owns_repo_id("1b790ad2-4421-43dc-9f46-f7c09d0c321f"));
    }

    #[test]
    fn repo_root_tolerates_orca_ids() {
        assert_eq!(repo_root("/p/widgets::w7"), "/p/widgets");
        assert_eq!(repo_root("/p/widgets"), "/p/widgets");
    }

    #[test]
    fn worktrees_go_next_to_the_checkout() {
        assert_eq!(
            worktrees_dir("/p/widgets"),
            PathBuf::from("/p/widgets.worktrees")
        );
        assert_eq!(branch_for("issue-3-x"), "bot/issue-3-x");
    }

    #[test]
    fn names_tell_their_item() {
        assert_eq!(number_of_name("issue-12"), Some(12));
        assert_eq!(number_of_name("issue-12-fix-it"), Some(12));
        assert_eq!(number_of_name("pr-7-x"), Some(7));
        assert_eq!(
            number_of_name("review-7-x"),
            None,
            "an old reviewer worktree is not the PR's"
        );
        assert_eq!(number_of_name("issue-12x"), None);
        assert_eq!(number_of_name("scratch"), None);
        assert_eq!(number_of_name("issue-"), None);
    }

    #[test]
    fn trust_dialogs_of_each_harness_are_recognised() {
        // Claude Code: "No, exit" comes first.
        let claude = "Quick safety check: Is this a project you created or one you trust?\n\
❯ No, exit\n  Yes, I trust this folder\nEnter to confirm · Esc to cancel";
        assert_eq!(trust_dialog(claude), Some(TrustAnswer::DownEnter));
        // Claude Code's bypass-permissions acceptance, once per machine.
        let bypass = "WARNING: Claude Code running in Bypass Permissions mode\n\
In Bypass Permissions mode, Claude Code will not ask for your approval before running \
potentially dangerous commands.\n❯ No, exit\n  Yes, I accept\nEnter to confirm · Esc to cancel";
        assert_eq!(trust_dialog(bypass), Some(TrustAnswer::DownEnter));
        assert_eq!(
            trust_dialog("⏵⏵ bypass permissions on (shift+tab to cycle)"),
            None
        );
        // Codex: "Yes, continue" comes first.
        let codex = "Do you trust the contents of this directory? Working with untrusted \
contents comes with higher risk of prompt injection.\n› 1. Yes, continue\n  2. No, quit";
        assert_eq!(trust_dialog(codex), Some(TrustAnswer::Enter));
        // Gemini and Pi, when started without --skip-trust / --approve.
        let gemini = "Do you trust the files in this folder?\n● 1. Trust folder (wt)\n  2. Trust parent folder\n  3. Don't trust";
        assert_eq!(trust_dialog(gemini), Some(TrustAnswer::Enter));
        let pi = "Trust project folder?\n/tmp/wt\n→ Trust\n  Trust parent folder";
        assert_eq!(trust_dialog(pi), Some(TrustAnswer::Enter));
        // A ready prompt, or unrelated text, is not a dialog.
        assert_eq!(trust_dialog("❯ \n⏵⏵ bypass permissions on"), None);
        assert_eq!(
            trust_dialog("Folder /tmp/wt has been added to trusted folders."),
            None
        );
    }

    #[test]
    fn login_prompts_of_each_harness_are_recognised() {
        // Claude Code answering a prompt after its token was revoked
        // (seen live on 2026-09-06, issue #81), and its login screen.
        let expired = "❯ [ssf] New activity on #81:\n\n  Login expired · Please run /login\n\n\
❯ \n  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        assert_eq!(
            login_dialog("claude", expired).as_deref(),
            Some("Login expired · Please run /login")
        );
        let screen = "Welcome to Claude Code v2.1.258\n\
 Claude Code can be used with your Claude subscription or billed based on API usage through your Console account.\n\
 Select login method:\n ❯ 1. Claude account with subscription · Pro, Max, Team, or Enterprise\n\
   2. Anthropic Console account · API usage billing\n   3. 3rd-party platform · Amazon Bedrock, Microsoft Foundry, or Vertex AI";
        assert!(login_dialog("claude", screen).is_some());
        assert!(
            login_dialog(
                "claude",
                "API Error: 401 Invalid API key · Please run /login"
            )
            .is_some()
        );
        assert!(
            login_dialog(
                "claude",
                "Your session has expired. Please run /login to sign in again."
            )
            .is_some()
        );
        // The same words far up the screen, quoted by a working agent
        // reading this test, do not count: only the bottom of the screen.
        let mut quoted = vec![
            "⏺ Read(src/driver.rs)".to_string(),
            "  Login expired · Please run /login".to_string(),
        ];
        quoted.extend((0..20).map(|i| format!("  line {i} of the file")));
        quoted.push("❯ ".into());
        assert_eq!(login_dialog("claude", &quoted.join("\n")), None);
        // A ready prompt, the trust dialog, or an agent at work.
        assert_eq!(login_dialog("claude", "❯ \n⏵⏵ bypass permissions on"), None);
        assert_eq!(
            login_dialog("claude", "❯ No, exit\n  Yes, I trust this folder"),
            None
        );
        assert_eq!(
            login_dialog(
                "claude",
                "⏺ Running cargo test…\n  Logging in progress in src/login.rs"
            ),
            None
        );
        // Codex's login screen (0.152.0, empty home).
        let codex = "  Welcome to Codex, OpenAI's command-line coding agent\n\
  Sign in with ChatGPT to use Codex as part of your paid plan\n  or connect an API key for usage-based billing\n\
> 1. Sign in with ChatGPT\n     Usage included with Plus, Pro, Business, and Enterprise plans\n\
  2. Sign in with Device Code\n  3. Provide your own API key\n  Press enter to continue";
        assert!(login_dialog("codex", codex).is_some());
        assert_eq!(
            login_dialog("codex", "› Working on the tests\n  Auth: OAuth"),
            None
        );
        // Gemini (0.57.0), Grok (device login), Pi (0.84.4), Oh My Pi,
        // OpenCode (1.18.25) and Crush (0.92.0) with an empty home.
        let gemini = "│ ? Get started\n│   How would you like to authenticate for this project?\n\
│   ● 1. Sign in with Google\n│     2. Use Gemini API Key\n│     3. Vertex AI\n│   No authentication method selected.";
        assert_eq!(
            login_dialog("gemini", gemini).as_deref(),
            Some("How would you like to authenticate for this project?")
        );
        let grok = "Approve in your browser to finish signing in.\n854F-EX33\nWaiting for approval...\nctrl+q  quit";
        assert!(login_dialog("grok", grok).is_some());
        let pi = " Warning: No models available. Use /login to log into a provider via OAuth or API key. See:\n\
   /home/x/pi/docs/providers.md\n0.0%/0 (auto)     unknown";
        assert!(login_dialog("pi", pi).is_some());
        let omp = "Setup step 1 of 5\nSet up your providers\n╭─ Select provider to login ───╮\n│ ❯ ChatGPT Plus/Pro (Codex Subscription) │";
        assert!(login_dialog("omp", omp).is_some());
        let opencode = "┃  Ask anything... \"Fix a TODO in the codebase\"\n┃  Build auto · Big Pickle OpenCode Zen\n\
● Tip Run /connect to add an AI provider and start coding";
        assert!(login_dialog("opencode", opencode).is_some());
        let crush =
            " To start, let's choose a provider and model.\n > Find your fave\n Charm Hyper";
        assert!(login_dialog("crush", crush).is_some());
        // A harness ssf knows nothing about still gets the common phrases.
        assert!(login_dialog("other", "Error: not logged in").is_some());
        assert_eq!(login_dialog("other", "all good"), None);
        // Echoed `[ssf]` text does not count: a person quoting the phrase
        // in a comment, delivered as activity and still on the screen.
        let quoted = "❯ [ssf] New activity on #5 \"Fix it\" (https://gh/5):\n\n\
- 15:20Z @mike commented (https://gh/c1):\n  > the terminal says Login expired · Please run /login, is that you?\n\
- 15:21Z @mike assigned @bot\n\n⏺ Yes, and I am fine now.\n\n❯ ";
        assert_eq!(login_dialog("claude", quoted), None);
        // But the harness's own answer right after the echo still does.
        assert!(login_dialog("claude", expired).is_some());
        let after_echo = "❯ [ssf] New activity on #5:\n- 15:20Z @mike commented:\n  > hi\n\nLogin expired · Please run /login\n❯ ";
        assert!(login_dialog("claude", after_echo).is_some());
    }

    #[test]
    fn parses_git_worktree_list() {
        let text = "worktree /p/widgets\nHEAD abc\nbranch refs/heads/master\n\n\
worktree /p/widgets.worktrees/issue-3\nHEAD def\nbranch refs/heads/bot/issue-3\n\n\
worktree /p/widgets.worktrees/tmp\nHEAD 123\ndetached\n";
        let list = parse_worktree_list(text);
        assert_eq!(list.len(), 3);
        assert_eq!(list[1].path, "/p/widgets.worktrees/issue-3");
        assert_eq!(list[1].branch.as_deref(), Some("refs/heads/bot/issue-3"));
        assert_eq!(list[2].branch, None);
    }
}
