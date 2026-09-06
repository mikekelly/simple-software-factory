//! Thin wrapper around the Orca CLI (`orca-ide ... --json`).

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::config::OrcaConfig;

/// Bracketed-paste markers so multi-line prompts land in an agent TUI as one
/// message instead of being submitted line by line.
const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

#[derive(Clone)]
pub struct Orca {
    cfg: OrcaConfig,
}

#[derive(Debug, Clone)]
pub struct ProjectSetup {
    pub repo_id: String,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct Worktree {
    pub id: String,
    pub path: String,
    pub branch: Option<String>,
}

/// Outcome of delivering a prompt: which terminal took it and whether the
/// harness had to be started again for it.
#[derive(Debug, Clone)]
pub struct Delivery {
    pub handle: String,
    pub relaunched: bool,
    pub resumed: bool,
}

/// One row of `orca worktree ps`: a workspace and the agents running in it.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct WorkspaceInfo {
    pub worktree_id: String,
    pub repo_id: String,
    pub path: String,
    pub display_name: String,
    /// Branch name without the `refs/heads/` prefix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Board column (`in-progress`, `completed`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    /// Orca's own rollup (`working`, `active`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub is_archived: bool,
    pub live_terminals: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_issue: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_pr: Option<u64>,
    /// Most recent of the workspace's activity, output and agent timestamps (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
    pub agents: Vec<AgentInfo>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct AgentInfo {
    /// `working`, `done`, `open`, `waiting`, ... as Orca reports it.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<String>,
    pub interrupted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_since: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl WorkspaceInfo {
    /// The agent whose state best describes the workspace: a working one
    /// wins, otherwise the most recently updated.
    pub fn primary_agent(&self) -> Option<&AgentInfo> {
        self.agents
            .iter()
            .find(|a| a.state == "working")
            .or_else(|| self.agents.iter().max_by_key(|a| a.updated_at.clone()))
    }

    pub fn is_working(&self) -> bool {
        self.agents.iter().any(|a| a.state == "working")
            || self.status.as_deref() == Some("working")
    }
}

/// Milliseconds since the epoch (how Orca reports times) as RFC 3339.
pub fn ms_to_iso(ms: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

fn value_ms(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(Value::as_i64)
}

fn value_string(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn linked_number(v: Option<&Value>) -> Option<u64> {
    v.and_then(|l| {
        l.as_u64()
            .or_else(|| l.get("number").and_then(Value::as_u64))
    })
}

/// Parse the `result` of `orca worktree ps --json`.
pub fn parse_ps(v: &Value) -> Vec<WorkspaceInfo> {
    let list = v.get("worktrees").and_then(Value::as_array);
    let Some(list) = list else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|w| {
            let worktree_id = w
                .get("worktreeId")
                .or_else(|| w.get("id"))
                .and_then(Value::as_str)?
                .to_string();
            let agents: Vec<AgentInfo> = w
                .get("agents")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|x| AgentInfo {
                            state: value_string(x, "state").unwrap_or_else(|| "unknown".into()),
                            agent_type: value_string(x, "agentType"),
                            last_assistant_message: value_string(x, "lastAssistantMessage"),
                            tool_name: value_string(x, "toolName"),
                            tool_input: value_string(x, "toolInput"),
                            interrupted: x
                                .get("interrupted")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            state_since: value_ms(x, "stateStartedAt").and_then(ms_to_iso),
                            updated_at: value_ms(x, "updatedAt").and_then(ms_to_iso),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut latest = [value_ms(w, "lastActivityAt"), value_ms(w, "lastOutputAt")]
                .into_iter()
                .flatten()
                .max();
            for a in w
                .get("agents")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(t) = value_ms(a, "updatedAt") {
                    latest = Some(latest.map_or(t, |l| l.max(t)));
                }
            }
            Some(WorkspaceInfo {
                worktree_id,
                repo_id: value_string(w, "repoId").unwrap_or_default(),
                path: value_string(w, "path").unwrap_or_default(),
                display_name: value_string(w, "displayName").unwrap_or_default(),
                branch: value_string(w, "branch").map(|b| {
                    b.strip_prefix("refs/heads/")
                        .map(str::to_string)
                        .unwrap_or(b)
                }),
                column: value_string(w, "workspaceStatus"),
                status: value_string(w, "status"),
                is_archived: w
                    .get("isArchived")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                live_terminals: w
                    .get("liveTerminalCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                linked_issue: linked_number(w.get("linkedIssue")),
                linked_pr: linked_number(w.get("linkedPR")),
                last_activity_at: latest.and_then(ms_to_iso),
                agents,
            })
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct Terminal {
    pub handle: String,
    pub agent_identity: Option<String>,
    pub connected: bool,
    pub writable: bool,
}

impl Orca {
    pub fn new(cfg: OrcaConfig) -> Self {
        Self { cfg }
    }

    pub fn command(&self) -> &str {
        &self.cfg.command
    }

    /// Run an Orca CLI command with `--json` and return `result`.
    pub async fn run(&self, args: &[&str]) -> Result<Value> {
        let mut full: Vec<&str> = args.to_vec();
        full.push("--json");
        debug!(cmd = %self.cfg.command, args = ?crate::driver::redacted_args(args), "orca");
        let out = Command::new(&self.cfg.command)
            .args(&full)
            .env_remove("ORCA_TERMINAL_ID")
            .output()
            .await
            .with_context(|| format!("spawning {} (is Orca installed?)", self.cfg.command))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let json_start = stdout.find('{');
        let parsed: Option<Value> =
            json_start.and_then(|i| serde_json::from_str(&stdout[i..]).ok());
        let Some(v) = parsed else {
            bail!(
                "orca {} produced no JSON (exit {:?}): {} {}",
                crate::driver::redacted_args(args).join(" "),
                out.status.code(),
                stdout.trim().chars().take(400).collect::<String>(),
                stderr.trim().chars().take(400).collect::<String>()
            );
        };
        if v.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
        let err = v.get("error").cloned().unwrap_or(Value::Null);
        let code = err.get("code").and_then(Value::as_str).unwrap_or("error");
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| err.to_string());
        bail!(
            "orca {} failed [{code}]: {msg}",
            crate::driver::redacted_args(args).join(" ")
        )
    }

    pub async fn status(&self) -> Result<Value> {
        let v = self.run(&["status"]).await?;
        let state = v
            .pointer("/runtime/state")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if state != "ready" {
            bail!("Orca runtime is not ready (state: {state}); is the Orca app running?");
        }
        Ok(v)
    }

    // ---- projects -------------------------------------------------------

    /// Find the Orca project for a GitHub repo, if any.
    pub async fn find_project(&self, owner: &str, repo: &str) -> Result<Option<String>> {
        let v = self.run(&["project", "list"]).await?;
        let wanted = format!("github:{}/{}", owner, repo).to_lowercase();
        let projects = v
            .get("projects")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for p in projects {
            let id = p.get("id").and_then(Value::as_str).unwrap_or("");
            if id.to_lowercase() == wanted {
                return Ok(Some(id.to_string()));
            }
            let po = p.pointer("/providerIdentity/owner").and_then(Value::as_str);
            let pr = p.pointer("/providerIdentity/repo").and_then(Value::as_str);
            let prov = p
                .pointer("/providerIdentity/provider")
                .and_then(Value::as_str);
            if prov == Some("github")
                && po.is_some_and(|o| o.eq_ignore_ascii_case(owner))
                && pr.is_some_and(|r| r.eq_ignore_ascii_case(repo))
            {
                return Ok(Some(id.to_string()));
            }
            let key = p
                .pointer("/gitRemoteIdentity/canonicalKey")
                .and_then(Value::as_str);
            if key.is_some_and(|k| k.eq_ignore_ascii_case(&format!("github.com/{owner}/{repo}"))) {
                return Ok(Some(id.to_string()));
            }
        }
        Ok(None)
    }

    /// A ready setup for `project_id` on the configured host, if any.
    pub async fn find_ready_setup(&self, project_id: &str) -> Result<Option<ProjectSetup>> {
        let v = self
            .run(&["project", "setups", "--project", project_id])
            .await?;
        let setups = v
            .get("setups")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut pending = None;
        for s in setups {
            let host = s.get("hostId").and_then(Value::as_str).unwrap_or("");
            if host != self.cfg.host {
                continue;
            }
            let state = s.get("setupState").and_then(Value::as_str).unwrap_or("");
            let repo_id = s.get("repoId").and_then(Value::as_str);
            let path = s.get("path").and_then(Value::as_str).unwrap_or("");
            if state == "ready" {
                if let Some(repo_id) = repo_id {
                    return Ok(Some(ProjectSetup {
                        repo_id: repo_id.to_string(),
                        path: path.to_string(),
                    }));
                }
            } else {
                pending = Some(state.to_string());
            }
        }
        if let Some(state) = pending {
            debug!(project_id, state, "project setup not ready yet");
        }
        Ok(None)
    }

    /// Ensure an Orca project + local setup exists for the repo, creating it
    /// by clone or by importing `existing_path` when necessary.
    pub async fn ensure_project(
        &self,
        owner: &str,
        repo: &str,
        clone_url: &str,
        existing_path: Option<&str>,
        projects_dir: &Path,
    ) -> Result<ProjectSetup> {
        let project_id = format!("github:{owner}/{repo}");
        let existing = self.find_project(owner, repo).await?;
        let project_id = existing.unwrap_or(project_id);
        if let Some(setup) = self.find_ready_setup(&project_id).await? {
            return Ok(setup);
        }
        match existing_path {
            Some(path) => {
                info!(project_id, path, "importing existing checkout into Orca");
                self.run(&[
                    "project",
                    "setup-existing-folder",
                    "--project",
                    &project_id,
                    "--host",
                    &self.cfg.host,
                    "--path",
                    path,
                    "--kind",
                    "git",
                    "--display-name",
                    repo,
                ])
                .await?;
            }
            None => {
                std::fs::create_dir_all(projects_dir)
                    .with_context(|| format!("creating {}", projects_dir.display()))?;
                let dest = projects_dir.to_string_lossy().to_string();
                info!(
                    project_id,
                    clone_url, dest, "cloning repository into Orca project"
                );
                self.run(&[
                    "project",
                    "setup-clone",
                    "--project",
                    &project_id,
                    "--host",
                    &self.cfg.host,
                    "--url",
                    clone_url,
                    "--destination",
                    &dest,
                    "--display-name",
                    repo,
                ])
                .await?;
            }
        }
        let deadline = Instant::now() + Duration::from_secs(self.cfg.setup_timeout_secs);
        loop {
            if let Some(setup) = self.find_ready_setup(&project_id).await? {
                info!(
                    project_id,
                    repo_id = setup.repo_id,
                    path = setup.path,
                    "project ready"
                );
                return Ok(setup);
            }
            if Instant::now() > deadline {
                bail!("timed out waiting for Orca project {project_id} to become ready");
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }

    // ---- worktrees ------------------------------------------------------

    /// Existing worktree already linked to this issue number, if any.
    pub async fn find_worktree_for_issue(
        &self,
        repo_id: &str,
        number: u64,
    ) -> Result<Option<Worktree>> {
        let sel = format!("id:{repo_id}");
        let v = self
            .run(&["worktree", "list", "--repo", &sel, "--limit", "500"])
            .await?;
        let list = v
            .get("worktrees")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for w in list {
            if w.get("isArchived").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let linked = w.get("linkedIssue");
            let n = linked.and_then(|l| {
                l.as_u64()
                    .or_else(|| l.get("number").and_then(Value::as_u64))
            });
            if n == Some(number) {
                let id = w
                    .get("id")
                    .or_else(|| w.get("worktreeId"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("worktree entry without id: {w}"))?;
                let path = w
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let branch = w.get("branch").and_then(Value::as_str).map(str::to_string);
                return Ok(Some(Worktree {
                    id: id.to_string(),
                    path,
                    branch,
                }));
            }
        }
        Ok(None)
    }

    /// Create a workspace for an issue. With `agent` the harness is launched in
    /// the first terminal and given the prompt; without it, only the checkout
    /// is made (the caller launches something itself).
    pub async fn create_worktree(
        &self,
        repo_id: &str,
        name: &str,
        issue: u64,
        agent: Option<(&str, &str)>,
        comment: &str,
        base_branch: Option<&str>,
    ) -> Result<Worktree> {
        let sel = format!("id:{repo_id}");
        let issue_s = issue.to_string();
        let mut args: Vec<&str> = vec![
            "worktree",
            "create",
            "--repo",
            &sel,
            "--name",
            name,
            "--issue",
            &issue_s,
            "--comment",
            comment,
            "--no-parent",
        ];
        if let Some((a, p)) = agent {
            args.extend(["--agent", a, "--prompt", p]);
        }
        if let Some(b) = base_branch {
            args.push("--base-branch");
            args.push(b);
        }
        let v = self.run(&args).await?;
        let wt = v.get("worktree").unwrap_or(&v);
        let id = wt
            .get("id")
            .or_else(|| wt.get("worktreeId"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("worktree create returned no worktree id: {v}"))?
            .to_string();
        let path = wt
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let branch = wt.get("branch").and_then(Value::as_str).map(str::to_string);
        Ok(Worktree { id, path, branch })
    }

    /// Filesystem path of the repo's main checkout.
    pub async fn repo_path(&self, repo_id: &str) -> Result<String> {
        let sel = format!("id:{repo_id}");
        let v = self.run(&["repo", "show", "--repo", &sel]).await?;
        v.pointer("/repo/path")
            .or_else(|| v.get("path"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("repo show returned no path: {v}"))
    }

    /// Start the harness in a workspace that has no agent yet: create its
    /// terminal, wait for the TUI, answer first-run dialogs, and close the
    /// placeholder shell Orca opened with the checkout.
    pub async fn launch_in_worktree(
        &self,
        worktree_id: &str,
        command: &str,
        title: &str,
        harness: &str,
    ) -> Result<String> {
        let before = self.list_terminals(worktree_id).await.unwrap_or_default();
        let handle = self.create_terminal(worktree_id, command, title).await?;
        self.settle_harness(&handle, harness).await?;
        for t in before {
            if t.agent_identity.is_none() && t.handle != handle {
                let _ = self
                    .run(&["terminal", "close", "--terminal", &t.handle])
                    .await;
            }
        }
        Ok(handle)
    }

    /// Whether the workspace still exists (and is not archived).
    pub async fn worktree_exists(&self, worktree_id: &str) -> Result<bool> {
        let sel = format!("id:{worktree_id}");
        match self.run(&["worktree", "show", "--worktree", &sel]).await {
            Ok(v) => {
                let wt = v.get("worktree").unwrap_or(&v);
                Ok(wt.get("isArchived").and_then(Value::as_bool) != Some(true))
            }
            Err(e) if e.to_string().contains("selector_not_found") => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Every workspace Orca knows about, with the agents running in it.
    pub async fn ps(&self) -> Result<Vec<WorkspaceInfo>> {
        let v = self.run(&["worktree", "ps", "--limit", "500"]).await?;
        Ok(parse_ps(&v))
    }

    /// Is the agent in this workspace still busy?
    pub async fn agent_busy(&self, worktree_id: &str) -> Result<bool> {
        Ok(self
            .ps()
            .await?
            .iter()
            .any(|w| w.worktree_id == worktree_id && w.is_working()))
    }

    /// Stop the workspace's terminals and remove it from Orca and git.
    pub async fn remove_worktree(&self, worktree_id: &str) -> Result<()> {
        let sel = format!("id:{worktree_id}");
        let _ = self.run(&["terminal", "stop", "--worktree", &sel]).await;
        self.run(&["worktree", "rm", "--worktree", &sel, "--force"])
            .await?;
        Ok(())
    }

    pub async fn set_comment(&self, worktree_id: &str, comment: &str) -> Result<()> {
        let sel = format!("id:{worktree_id}");
        self.run(&["worktree", "set", "--worktree", &sel, "--comment", comment])
            .await?;
        Ok(())
    }

    pub async fn set_status(&self, worktree_id: &str, status: &str) -> Result<()> {
        let sel = format!("id:{worktree_id}");
        self.run(&[
            "worktree",
            "set",
            "--worktree",
            &sel,
            "--workspace-status",
            status,
        ])
        .await?;
        Ok(())
    }

    // ---- terminals ------------------------------------------------------

    pub async fn list_terminals(&self, worktree_id: &str) -> Result<Vec<Terminal>> {
        let sel = format!("id:{worktree_id}");
        let v = self
            .run(&["terminal", "list", "--worktree", &sel, "--limit", "100"])
            .await?;
        let list = v
            .get("terminals")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(list
            .iter()
            .filter_map(|t| {
                Some(Terminal {
                    handle: t.get("handle")?.as_str()?.to_string(),
                    agent_identity: t
                        .get("agentIdentity")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    connected: t.get("connected").and_then(Value::as_bool).unwrap_or(true),
                    writable: t.get("writable").and_then(Value::as_bool).unwrap_or(true),
                })
            })
            .collect())
    }

    /// Is there a connected, writable terminal running an agent in the
    /// workspace, i.e. would [`Orca::deliver`] paste into one rather than
    /// relaunch the harness?
    pub async fn has_live_agent(&self, worktree_id: &str) -> Result<bool> {
        Ok(self
            .list_terminals(worktree_id)
            .await?
            .iter()
            .any(|t| t.agent_identity.is_some() && t.connected && t.writable))
    }

    pub async fn create_terminal(
        &self,
        worktree_id: &str,
        command: &str,
        title: &str,
    ) -> Result<String> {
        let sel = format!("id:{worktree_id}");
        let v = self
            .run(&[
                "terminal",
                "create",
                "--worktree",
                &sel,
                "--command",
                command,
                "--title",
                title,
            ])
            .await?;
        v.pointer("/terminal/handle")
            .or_else(|| v.get("handle"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("terminal create returned no handle: {v}"))
    }

    pub async fn wait_tui_idle(&self, handle: &str, timeout_ms: u64) -> Result<bool> {
        let t = timeout_ms.to_string();
        let v = self
            .run(&[
                "terminal",
                "wait",
                "--terminal",
                handle,
                "--for",
                "tui-idle",
                "--timeout-ms",
                &t,
            ])
            .await?;
        Ok(v.pointer("/wait/satisfied")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    /// Rendered screen of a terminal, as lines.
    pub async fn screen(&self, handle: &str) -> Result<Vec<String>> {
        let v = self
            .run(&["terminal", "read", "--terminal", handle, "--screen"])
            .await?;
        Ok(v.pointer("/terminal/tail")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|l| l.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Wait for a freshly launched harness to become idle and click through
    /// first-run dialogs that would otherwise swallow the prompt: Claude Code
    /// and Codex ask whether to trust a folder the first time they run in a
    /// new worktree (see `driver::trust_dialog` for who preselects what).
    pub async fn settle_harness(&self, handle: &str, harness: &str) -> Result<()> {
        let idle = self
            .wait_tui_idle(handle, self.cfg.tui_idle_timeout_ms)
            .await?;
        if !idle {
            bail!("{harness} in {handle} did not become idle in time");
        }
        for _ in 0..3 {
            let screen = self.screen(handle).await?;
            if let Some(answer) = crate::driver::trust_dialog(&screen.join("\n")) {
                info!(handle, "accepting the folder trust dialog");
                if answer == crate::driver::TrustAnswer::DownEnter {
                    self.run(&["terminal", "send", "--terminal", handle, "--text", "\x1b[B"])
                        .await?;
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                self.run(&[
                    "terminal",
                    "send",
                    "--terminal",
                    handle,
                    "--text",
                    "",
                    "--enter",
                ])
                .await?;
                tokio::time::sleep(Duration::from_millis(1500)).await;
                self.wait_tui_idle(handle, self.cfg.tui_idle_timeout_ms)
                    .await?;
                continue;
            }
            break;
        }
        Ok(())
    }

    /// The live agent terminal a delivery would paste into: `preferred`
    /// if it is still one, else any.
    pub async fn live_handle(
        &self,
        worktree_id: &str,
        preferred: Option<&str>,
    ) -> Result<Option<String>> {
        let terminals = self.list_terminals(worktree_id).await?;
        let alive = |t: &Terminal| t.agent_identity.is_some() && t.connected && t.writable;
        Ok(preferred
            .and_then(|h| terminals.iter().find(|t| t.handle == h && alive(t)))
            .or_else(|| terminals.iter().find(|t| alive(t)))
            .map(|t| t.handle.clone()))
    }

    /// Close the agent's terminal (Orca ends the process with it) and wait
    /// until the workspace has no live agent, so the next delivery starts
    /// the harness again rather than pasting into a dead one.
    pub async fn stop_agent(&self, worktree_id: &str, handle: &str) -> Result<()> {
        self.run(&["terminal", "close", "--terminal", handle])
            .await?;
        for _ in 0..10 {
            if !self.has_live_agent(worktree_id).await? {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        bail!("{handle} is still a live agent terminal after being closed")
    }

    /// Paste `text` into the terminal as one bracketed-paste block, then press Enter.
    pub async fn send_prompt(&self, handle: &str, text: &str) -> Result<()> {
        let pasted = format!("{PASTE_START}{}{PASTE_END}", text.trim_end());
        self.run(&["terminal", "send", "--terminal", handle, "--text", &pasted])
            .await?;
        tokio::time::sleep(Duration::from_millis(400)).await;
        self.run(&[
            "terminal",
            "send",
            "--terminal",
            handle,
            "--text",
            "",
            "--enter",
        ])
        .await?;
        Ok(())
    }

    /// Locate (or relaunch) the harness terminal in a worktree and deliver a
    /// prompt. When the harness has to be started again and `resume_command`
    /// is given, that is tried first so the agent keeps its memory; if the
    /// resume visibly fails, a fresh harness gets `relaunch_text` instead
    /// (the whole story rather than the delta).
    pub async fn deliver(
        &self,
        worktree_id: &str,
        preferred_handle: Option<&str>,
        relaunch_command: &str,
        resume_command: Option<&str>,
        harness: &str,
        title: &str,
        text: &str,
        relaunch_text: Option<&str>,
    ) -> Result<Delivery> {
        let terminals = self.list_terminals(worktree_id).await?;
        let alive = |t: &Terminal| t.agent_identity.is_some() && t.connected && t.writable;
        let target = preferred_handle
            .and_then(|h| terminals.iter().find(|t| t.handle == h && alive(t)))
            .or_else(|| terminals.iter().find(|t| alive(t)))
            .map(|t| t.handle.clone());
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
        if let Some(cmd) = resume_command {
            warn!(
                worktree_id,
                cmd = crate::driver::redacted(cmd),
                "no live agent terminal; resuming harness session"
            );
            let h = self.create_terminal(worktree_id, cmd, title).await?;
            match self.settle_harness(&h, harness).await {
                Ok(()) if !crate::sessions::resume_failed(&self.screen(&h).await?) => {
                    for t in &terminals {
                        if t.agent_identity.is_none() {
                            let _ = self
                                .run(&["terminal", "close", "--terminal", &t.handle])
                                .await;
                        }
                    }
                    resumed = true;
                    handle = Some(h);
                }
                Ok(()) => {
                    warn!(
                        worktree_id,
                        "harness could not resume its session; starting fresh"
                    );
                    let _ = self.run(&["terminal", "close", "--terminal", &h]).await;
                }
                Err(e) => {
                    warn!(
                        worktree_id,
                        "resumed harness did not settle ({e:#}); starting fresh"
                    );
                    let _ = self.run(&["terminal", "close", "--terminal", &h]).await;
                }
            }
        }
        let handle = match handle {
            Some(h) => h,
            None => {
                warn!(
                    worktree_id,
                    relaunch_command = crate::driver::redacted(relaunch_command),
                    "no live agent terminal; relaunching harness"
                );
                self.launch_in_worktree(worktree_id, relaunch_command, title, harness)
                    .await?
            }
        };
        let body = match relaunch_text {
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

    #[test]
    fn parses_worktree_ps_rows() {
        let v = json!({"worktrees": [{
            "worktreeId": "repo::/w/issue-5",
            "repoId": "repo",
            "path": "/w/issue-5",
            "branch": "refs/heads/bot/issue-5",
            "displayName": "issue-5",
            "workspaceStatus": "in-progress",
            "status": "working",
            "isArchived": false,
            "liveTerminalCount": 1,
            "linkedIssue": 5,
            "linkedPR": null,
            "lastActivityAt": 1788435090763i64,
            "lastOutputAt": 1788435157645i64,
            "agents": [{
                "state": "working",
                "agentType": "claude",
                "lastAssistantMessage": null,
                "toolName": "Bash",
                "toolInput": "cargo test",
                "interrupted": false,
                "stateStartedAt": 1788435092830i64,
                "updatedAt": 1788435160000i64
            }]
        }, {"id": "repo::/w/other", "branch": "main", "agents": []}]});
        let rows = parse_ps(&v);
        assert_eq!(rows.len(), 2);
        let w = &rows[0];
        assert_eq!(w.worktree_id, "repo::/w/issue-5");
        assert_eq!(w.branch.as_deref(), Some("bot/issue-5"));
        assert_eq!(w.column.as_deref(), Some("in-progress"));
        assert_eq!(w.linked_issue, Some(5));
        assert_eq!(w.linked_pr, None);
        assert!(w.is_working());
        // The newest of the workspace and agent timestamps wins.
        assert_eq!(w.last_activity_at.as_deref(), Some("2026-09-03T11:32:40Z"));
        let a = w.primary_agent().unwrap();
        assert_eq!(a.state, "working");
        assert_eq!(a.tool_name.as_deref(), Some("Bash"));
        assert_eq!(a.last_assistant_message, None);
        assert_eq!(a.state_since.as_deref(), Some("2026-09-03T11:31:32Z"));
        let o = &rows[1];
        assert_eq!(o.branch.as_deref(), Some("main"));
        assert!(!o.is_working());
        assert!(o.primary_agent().is_none());
    }

    #[test]
    fn primary_agent_prefers_working_then_newest() {
        let w = WorkspaceInfo {
            agents: vec![
                AgentInfo {
                    state: "done".into(),
                    updated_at: Some("2026-01-01T00:00:02Z".into()),
                    ..Default::default()
                },
                AgentInfo {
                    state: "open".into(),
                    updated_at: Some("2026-01-01T00:00:01Z".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(w.primary_agent().unwrap().state, "done");
        let w = WorkspaceInfo {
            agents: vec![
                AgentInfo {
                    state: "done".into(),
                    updated_at: Some("2026-01-01T00:00:02Z".into()),
                    ..Default::default()
                },
                AgentInfo {
                    state: "working".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(w.primary_agent().unwrap().state, "working");
    }
}
