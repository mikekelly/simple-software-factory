//! Driver for herdr (<https://herdr.dev>): a terminal workspace manager
//! whose CLI talks to its running server. A workspace here is a herdr
//! workspace opened on a git worktree ssf made next to its clone; the agent
//! runs in the workspace's root pane, where herdr recognises it and reports
//! its state (`idle`, `working`, `blocked`, `done`).
//!
//! Workspace ids are herdr's (`w7`); prompt handles are pane ids (`w7:p1`).

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::config::HerdrConfig;
use crate::driver::{
    self, Relaunch, add_local_worktree, find_local_worktree, number_of_name, remove_local_worktree,
};
use crate::orca::{AgentInfo, Delivery, WorkspaceInfo, Worktree};

const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

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
}

/// One row of `herdr pane list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub pane_id: String,
    pub cwd: Option<String>,
    pub agent: Option<String>,
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
                cwd: s(p, "cwd"),
                agent: s(p, "agent"),
            })
        })
        .collect()
}

/// A workspace's checkout root and item number, from the cwd of its panes:
/// ssf's worktrees live in `<root>.worktrees/<name>`.
fn root_and_item(cwd: &str) -> (Option<String>, Option<(u64, bool)>) {
    let p = Path::new(cwd);
    let name = p.file_name().map(|n| n.to_string_lossy().to_string());
    let parent = p
        .parent()
        .and_then(|d| d.file_name())
        .map(|n| n.to_string_lossy().to_string());
    let root = match (p.parent().and_then(Path::parent), parent) {
        (Some(base), Some(dir)) => dir
            .strip_suffix(".worktrees")
            .map(|n| base.join(n).to_string_lossy().to_string()),
        _ => None,
    };
    let item = if root.is_some() {
        name.as_deref().and_then(number_of_name)
    } else {
        None
    };
    (root, item)
}

/// Join `herdr workspace list`, the panes of each workspace and
/// `herdr agent list` into the engine's view.
pub fn join_ps(
    workspaces: &Value,
    panes: &[(String, Vec<Pane>)],
    agents: &[Agent],
) -> Vec<WorkspaceInfo> {
    workspaces
        .get("workspaces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|w| {
            let id = s(w, "workspace_id")?;
            let ws_panes = panes
                .iter()
                .find(|(ws, _)| *ws == id)
                .map(|(_, p)| p.as_slice())
                .unwrap_or_default();
            let cwd = ws_panes.iter().find_map(|p| p.cwd.clone());
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
                worktree_id: id,
                repo_id: root.unwrap_or_default(),
                path: cwd.unwrap_or_default(),
                display_name: s(w, "label").unwrap_or_default(),
                branch: None,
                column: None,
                status: s(w, "agent_status"),
                is_archived: false,
                live_terminals: ws_panes.len() as u64,
                linked_issue: item.filter(|(_, r)| !r).map(|(n, _)| n),
                linked_pr: None,
                last_activity_at: None,
                agents: ws_agents,
            })
        })
        .collect()
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
                args.join(" "),
                stdout.trim().chars().take(400).collect::<String>()
            );
        };
        Ok(v.get("result").cloned().unwrap_or(v))
    }

    /// Run a herdr command and return its stdout (`read --format text`
    /// prints the screen as it is).
    pub async fn run_raw(&self, args: &[&str]) -> Result<String> {
        debug!(cmd = %self.cfg.command, ?args, "herdr");
        let out = Command::new(&self.cfg.command)
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
            bail!("herdr {} failed: {msg}", args.join(" "));
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

    /// The herdr workspace open on `path`, if any.
    async fn workspace_for_path(&self, repo_root: &str, path: &str) -> Result<Option<String>> {
        let v = self.run(&["worktree", "list", "--cwd", repo_root]).await?;
        Ok(v.get("worktrees")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|w| s(w, "path").as_deref() == Some(path))
            .and_then(|w| s(w, "open_workspace_id")))
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
        number: u64,
    ) -> Result<Option<Worktree>> {
        let Some(w) = find_local_worktree(repo_root, number, false).await? else {
            return Ok(None);
        };
        let label = Path::new(&w.path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("issue-{number}"));
        let ws = self.open(repo_root, &w.path, &label).await?;
        Ok(Some(Worktree {
            id: ws,
            path: w.path,
            branch: w.branch,
        }))
    }

    pub async fn create_worktree(
        &self,
        repo_root: &str,
        name: &str,
        comment: &str,
        base_branch: Option<&str>,
    ) -> Result<Worktree> {
        let (path, branch) = add_local_worktree(repo_root, name, base_branch).await?;
        let ws = match self.open(repo_root, &path, name).await {
            Ok(ws) => ws,
            Err(e) => {
                let _ = remove_local_worktree(repo_root, &path).await;
                return Err(e);
            }
        };
        let _ = self.set_comment(&ws, comment).await;
        Ok(Worktree {
            id: ws,
            path,
            branch: Some(branch),
        })
    }

    pub async fn worktree_exists(&self, workspace_id: &str) -> Result<bool> {
        match self.run(&["workspace", "get", workspace_id]).await {
            Ok(_) => Ok(true),
            Err(e) => {
                let msg = e.to_string().to_lowercase();
                if msg.contains("not_found")
                    || msg.contains("not found")
                    || msg.contains("unknown workspace")
                {
                    Ok(false)
                } else {
                    Err(e)
                }
            }
        }
    }

    pub async fn ps(&self) -> Result<Vec<WorkspaceInfo>> {
        let workspaces = self.run(&["workspace", "list"]).await?;
        let agents = self.agents().await?;
        let mut panes = Vec::new();
        for w in workspaces
            .get("workspaces")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(id) = s(w, "workspace_id") {
                let p = self.panes(&id).await.unwrap_or_default();
                panes.push((id, p));
            }
        }
        Ok(join_ps(&workspaces, &panes, &agents))
    }

    pub async fn agent_busy(&self, workspace_id: &str) -> Result<bool> {
        Ok(self
            .agents()
            .await?
            .iter()
            .any(|a| a.workspace_id == workspace_id && a.status == "working"))
    }

    pub async fn has_live_agent(&self, workspace_id: &str) -> Result<bool> {
        Ok(self
            .agents()
            .await?
            .iter()
            .any(|a| a.workspace_id == workspace_id))
    }

    /// Close the workspace and remove its checkout.
    pub async fn remove_worktree(&self, workspace_id: &str) -> Result<()> {
        let panes = self.panes(workspace_id).await.unwrap_or_default();
        let cwd = panes.iter().find_map(|p| p.cwd.clone());
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
        if let Some(cwd) = cwd {
            let (root, _) = root_and_item(&cwd);
            if let Some(root) = root {
                let _ = remove_local_worktree(&root, &cwd).await;
            }
        }
        Ok(())
    }

    pub async fn set_comment(&self, workspace_id: &str, comment: &str) -> Result<()> {
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

    pub async fn set_status(&self, workspace_id: &str, status: &str) -> Result<()> {
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
        let text = self
            .run_raw(&[
                "pane", "read", pane_id, "--source", "visible", "--format", "text",
            ])
            .await?;
        Ok(text.lines().map(str::to_string).collect())
    }

    /// Wait until herdr sees an agent in the pane and it is ready for input,
    /// answering the folder-trust dialog Claude Code shows on a new
    /// worktree.
    pub async fn settle_harness(&self, pane_id: &str, harness: &str) -> Result<()> {
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
        for _ in 0..4 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("{harness} in {pane_id} did not become idle in time");
            }
            let t = left.as_millis().to_string();
            let v = self
                .run(&["agent", "wait", pane_id, "--timeout", &t])
                .await?;
            let state = v
                .get("agent_status")
                .or_else(|| v.pointer("/agent/agent_status"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if state != "blocked" {
                return Ok(());
            }
            let text = self.screen(pane_id).await?.join("\n").to_lowercase();
            if text.contains("trust this folder") {
                info!(pane_id, "accepting the folder trust dialog");
                self.run(&["agent", "send-keys", pane_id, "down"]).await?;
                tokio::time::sleep(Duration::from_millis(300)).await;
                self.run(&["agent", "send-keys", pane_id, "enter"]).await?;
                tokio::time::sleep(Duration::from_millis(1500)).await;
                continue;
            }
            // Some other question: the prompt goes in anyway, as with Orca.
            return Ok(());
        }
        Ok(())
    }

    /// Run the harness in a shell pane of the workspace and wait for it.
    pub async fn launch(
        &self,
        workspace_id: &str,
        command: &str,
        title: &str,
        harness: &str,
    ) -> Result<String> {
        let pane = self.shell_pane(workspace_id).await?;
        self.run(&["pane", "run", &pane, command]).await?;
        self.settle_harness(&pane, harness).await?;
        let _ = self.run(&["pane", "rename", &pane, title]).await;
        Ok(pane)
    }

    /// Give the agent in a pane a prompt. `agent prompt` pastes for us; if
    /// it refuses because the agent is at a question, the text is pasted
    /// raw like Orca does (the harness queues it).
    pub async fn send_prompt(&self, pane_id: &str, text: &str) -> Result<()> {
        match self
            .run(&["agent", "prompt", pane_id, text.trim_end()])
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("agent_blocked") => {
                warn!(
                    pane_id,
                    "agent is blocked on a question; pasting the prompt raw"
                );
                let pasted = format!("{PASTE_START}{}{PASTE_END}", text.trim_end());
                self.run(&["pane", "send-text", pane_id, &pasted]).await?;
                tokio::time::sleep(Duration::from_millis(400)).await;
                self.run(&["pane", "send-keys", pane_id, "enter"]).await?;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    pub async fn deliver(
        &self,
        workspace_id: &str,
        preferred_handle: Option<&str>,
        relaunch: &Relaunch<'_>,
        text: &str,
    ) -> Result<Delivery> {
        let agents = self.agents().await?;
        let live: Vec<&Agent> = agents
            .iter()
            .filter(|a| a.workspace_id == workspace_id)
            .collect();
        let target = preferred_handle
            .and_then(|h| live.iter().find(|a| a.pane_id == h))
            .or_else(|| live.first())
            .map(|a| a.pane_id.clone());
        if let Some(handle) = target {
            self.send_prompt(&handle, text).await?;
            return Ok(Delivery {
                handle,
                relaunched: false,
                resumed: false,
            });
        }
        let mut resumed = false;
        let mut handle = None;
        if let Some(cmd) = relaunch.resume_command {
            warn!(workspace_id, cmd, "no live agent; resuming harness session");
            match self
                .launch(workspace_id, cmd, relaunch.title, relaunch.harness)
                .await
            {
                Ok(h) => {
                    if crate::sessions::resume_failed(&self.screen(&h).await.unwrap_or_default()) {
                        warn!(
                            workspace_id,
                            "harness could not resume its session; starting fresh"
                        );
                        let _ = self.run(&["pane", "send-keys", &h, "ctrl+c"]).await;
                        tokio::time::sleep(Duration::from_millis(1000)).await;
                    } else {
                        resumed = true;
                        handle = Some(h);
                    }
                }
                Err(e) => {
                    warn!(
                        workspace_id,
                        "resumed harness did not settle ({e:#}); starting fresh"
                    );
                }
            }
        }
        let handle = match handle {
            Some(h) => h,
            None => {
                warn!(
                    workspace_id,
                    command = relaunch.command,
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
        let body = match relaunch.text {
            Some(full) if !resumed => full,
            _ => text,
        };
        self.send_prompt(&handle, body).await?;
        Ok(Delivery {
            handle,
            relaunched: true,
            resumed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Against a running herdr server: makes a scratch repo, opens a
    /// workspace, runs Claude Code in it, sends a prompt, removes it all.
    /// `cargo test herdr_live -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn herdr_live() {
        let base = std::env::temp_dir().join(format!("ssf-herdr-live-{}", std::process::id()));
        let root = base.join("widgets").to_string_lossy().to_string();
        std::fs::create_dir_all(&root).unwrap();
        for args in [
            vec!["init", "-q", "-b", "master"],
            vec![
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        ] {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(&args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let h = Herdr::new(HerdrConfig::default());
        h.status().await.unwrap();
        let wt = h
            .create_worktree(&root, "issue-3-x", "ssf: test", None)
            .await
            .unwrap();
        eprintln!("workspace {} at {}", wt.id, wt.path);
        assert!(h.worktree_exists(&wt.id).await.unwrap());
        assert!(!h.has_live_agent(&wt.id).await.unwrap());
        let found = h.find_worktree_for_issue(&root, 3).await.unwrap().unwrap();
        assert_eq!(found.id, wt.id);
        let handle = h
            .launch(&wt.id, "claude --model haiku", "claude · #3", "claude")
            .await
            .unwrap();
        eprintln!("launched in {handle}");
        assert!(h.has_live_agent(&wt.id).await.unwrap());
        h.send_prompt(&handle, "Reply with the single word pong and nothing else.")
            .await
            .unwrap();
        // The prompt must have been submitted, not just pasted.
        tokio::time::sleep(Duration::from_secs(8)).await;
        let v = h
            .run(&["agent", "wait", &handle, "--timeout", "60000"])
            .await
            .unwrap();
        eprintln!("wait: {v}");
        let screen = h.screen(&handle).await.unwrap();
        eprintln!("screen:\n{}", screen.join("\n"));
        let text = screen.join("\n").to_lowercase();
        assert!(text.contains("pong"), "the prompt was not answered");
        let rows = h.ps().await.unwrap();
        let row = rows.iter().find(|r| r.worktree_id == wt.id).unwrap();
        eprintln!("ps row: {row:?}");
        assert_eq!(row.linked_issue, Some(3));
        assert_eq!(row.repo_id, root);
        h.set_status(&wt.id, "completed").await.unwrap();
        h.remove_worktree(&wt.id).await.unwrap();
        assert!(!h.worktree_exists(&wt.id).await.unwrap());
        assert!(!std::path::Path::new(&wt.path).exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parses_agent_and_pane_lists() {
        let agents = parse_agents(&json!({"agents": [
            {"agent": "claude", "agent_status": "working", "cwd": "/p/w.worktrees/issue-3-x",
             "pane_id": "w2:p1", "workspace_id": "w2", "terminal_title_stripped": "Running tests"},
            {"agent": "omp", "agent_status": "idle", "pane_id": "w3:p1", "workspace_id": "w3"}
        ]}));
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].pane_id, "w2:p1");
        assert_eq!(agents[0].status, "working");
        assert_eq!(agents[0].title.as_deref(), Some("Running tests"));
        let panes = parse_panes(&json!({"panes": [
            {"pane_id": "w2:p1", "cwd": "/p/w.worktrees/issue-3-x", "agent": "claude"},
            {"pane_id": "w2:p2", "cwd": "/p/w.worktrees/issue-3-x"}
        ]}));
        assert_eq!(panes[1].agent, None);
        assert_eq!(panes[0].agent.as_deref(), Some("claude"));
    }

    #[test]
    fn workspace_cwd_tells_root_and_item() {
        assert_eq!(
            root_and_item("/p/widgets.worktrees/issue-3-x"),
            (Some("/p/widgets".into()), Some((3, false)))
        );
        assert_eq!(
            root_and_item("/p/widgets.worktrees/review-9"),
            (Some("/p/widgets".into()), Some((9, true)))
        );
        assert_eq!(root_and_item("/home/me/code/thing"), (None, None));
    }

    #[test]
    fn joins_workspaces_panes_and_agents() {
        let ws = json!({"workspaces": [
            {"workspace_id": "w2", "label": "issue-3-x", "agent_status": "working"},
            {"workspace_id": "w5", "label": "scratch", "agent_status": "idle"}
        ]});
        let panes = vec![
            (
                "w2".to_string(),
                vec![Pane {
                    pane_id: "w2:p1".into(),
                    cwd: Some("/p/widgets.worktrees/issue-3-x".into()),
                    agent: Some("claude".into()),
                }],
            ),
            ("w5".to_string(), vec![]),
        ];
        let agents = vec![Agent {
            pane_id: "w2:p1".into(),
            workspace_id: "w2".into(),
            kind: "claude".into(),
            status: "working".into(),
            cwd: None,
            title: Some("cargo test".into()),
        }];
        let rows = join_ps(&ws, &panes, &agents);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].worktree_id, "w2");
        assert_eq!(rows[0].repo_id, "/p/widgets");
        assert_eq!(rows[0].linked_issue, Some(3));
        assert!(rows[0].is_working());
        assert_eq!(
            rows[0]
                .primary_agent()
                .unwrap()
                .last_assistant_message
                .as_deref(),
            Some("cargo test")
        );
        assert_eq!(rows[1].linked_issue, None);
        assert!(!rows[1].is_working());
        assert!(rows[1].primary_agent().is_none());
    }
}
