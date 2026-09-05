//! Driver for Claude Code cloud sessions (claude.ai/code). Nothing runs on
//! this machine but the `claude` CLI: a session is created from a local
//! worktree on the item's branch (the cloud VM clones the repository's
//! GitHub remote at that branch, so the branch is pushed first), and every
//! later prompt is queued into it with `claude -p ... --cloud <session>`.
//!
//! Workspace ids are the local worktree paths; prompt handles are session
//! ids (`session_...`). The session id of a workspace is kept in the
//! worktree's git directory, so it survives a daemon restart and goes with
//! the worktree.
//!
//! Creating a session without a terminal only works on a self-hosted
//! environment (`cloud.environment`); otherwise `claude --cloud` is run in
//! a pseudo-terminal until it has printed the new session's id.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::config::CloudConfig;
use crate::driver::{
    Relaunch, add_local_worktree, find_local_worktree, local_worktrees, number_of_name,
    remove_local_worktree,
};
use crate::orca::{AgentInfo, Delivery, WorkspaceInfo, Worktree};
use crate::release::git;

const MARKER: &str = "ssf-cloud-session";

#[derive(Clone)]
pub struct Cloud {
    cfg: CloudConfig,
    /// Checkouts of the repositories that use this driver, for `ps`.
    roots: Vec<String>,
}

/// What `claude -p ... --cloud <id> --output-format json` answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendResult {
    Sent(String),
    /// The session is archived or does not exist: a new one is needed.
    Gone(String),
    Failed(String),
}

pub fn parse_send(stdout: &str, stderr: &str, success: bool) -> SendResult {
    let json: Option<Value> = stdout
        .find('{')
        .and_then(|i| serde_json::from_str(&stdout[i..]).ok());
    let text = format!("{stdout}\n{stderr}");
    let lower = text.to_lowercase();
    let gone = lower.contains("archived")
        || lower.contains("session not found")
        || lower.contains("not found:");
    match json {
        Some(v) if v.get("ok").and_then(Value::as_bool) == Some(true) => SendResult::Sent(
            v.get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        Some(v) => {
            let err = v
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string());
            if gone
                || err.to_lowercase().contains("archived")
                || err.to_lowercase().contains("not found")
            {
                SendResult::Gone(err)
            } else {
                SendResult::Failed(err)
            }
        }
        None if success && lower.contains("sent to cloud session") => {
            SendResult::Sent(session_id_in(&text).unwrap_or_default())
        }
        None if gone => SendResult::Gone(text.trim().chars().take(400).collect()),
        None => SendResult::Failed(text.trim().chars().take(400).collect()),
    }
}

/// The first cloud session id in some text (`session_...`, or the
/// `cse_...` form the session itself sees).
pub fn session_id_in(text: &str) -> Option<String> {
    for prefix in ["session_", "cse_"] {
        let mut from = 0;
        while let Some(i) = text[from..].find(prefix) {
            let start = from + i;
            let id: String = text[start..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if id.len() > prefix.len() + 8 {
                return Some(id);
            }
            from = start + prefix.len();
        }
    }
    None
}

/// Arguments after the executable in a harness command line such as
/// `claude --model opus --effort high` (the executable is
/// `cloud.command`; only its flags are wanted).
pub fn extra_args(command: &str) -> Vec<String> {
    let mut words = shell_words(command);
    if !words.is_empty() {
        words.remove(0);
    }
    words
        .into_iter()
        .filter(|w| w != "--dangerously-skip-permissions")
        .collect()
}

fn shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                has = true;
            }
            None if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            None => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl Cloud {
    pub fn new(cfg: CloudConfig) -> Self {
        Self {
            cfg,
            roots: Vec::new(),
        }
    }

    /// The checkouts `ps` lists worktrees of.
    pub fn with_roots(mut self, roots: Vec<String>) -> Self {
        self.roots = roots;
        self
    }

    pub fn command(&self) -> &str {
        &self.cfg.command
    }

    fn claude(&self) -> Command {
        let mut c = Command::new(&self.cfg.command);
        // A nested Claude Code refuses to start; the daemon may be one.
        c.env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_ENTRYPOINT");
        c
    }

    /// Signed in to claude.ai (cloud sessions need the account, not a key).
    pub async fn status(&self) -> Result<()> {
        let out = self
            .claude()
            .args(["auth", "status", "--json"])
            .output()
            .await
            .with_context(|| {
                format!("spawning {} (is Claude Code installed?)", self.cfg.command)
            })?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let v: Value = stdout
            .find('{')
            .and_then(|i| serde_json::from_str(&stdout[i..]).ok())
            .with_context(|| {
                format!(
                    "claude auth status produced no JSON: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )
            })?;
        if v.get("loggedIn").and_then(Value::as_bool) != Some(true) {
            bail!("claude is not signed in (run `claude auth login`)");
        }
        let method = v.get("authMethod").and_then(Value::as_str).unwrap_or("");
        if method != "claude.ai" {
            bail!("claude is signed in with {method:?}; cloud sessions need a claude.ai account");
        }
        Ok(())
    }

    fn marker_path(path: &str) -> Option<PathBuf> {
        // A worktree's `.git` is a file naming its git directory.
        let dotgit = Path::new(path).join(".git");
        let text = std::fs::read_to_string(&dotgit).ok()?;
        let dir = text.trim().strip_prefix("gitdir:")?.trim();
        let dir = if Path::new(dir).is_absolute() {
            PathBuf::from(dir)
        } else {
            Path::new(path).join(dir)
        };
        Some(dir.join(MARKER))
    }

    /// The session id recorded for a worktree.
    pub fn session_of(path: &str) -> Option<String> {
        let p = Self::marker_path(path)?;
        std::fs::read_to_string(p)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn record_session(path: &str, id: &str) -> Result<()> {
        let p = Self::marker_path(path).context("worktree has no git directory")?;
        std::fs::write(&p, format!("{id}\n")).with_context(|| format!("writing {}", p.display()))
    }

    fn forget_session(path: &str) {
        if let Some(p) = Self::marker_path(path) {
            let _ = std::fs::remove_file(p);
        }
    }

    async fn root_of(path: &str) -> Result<String> {
        let common = git(
            path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .await?;
        Ok(Path::new(&common)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or(common))
    }

    pub async fn find_worktree_for_issue(
        &self,
        repo_root: &str,
        number: u64,
    ) -> Result<Option<Worktree>> {
        Ok(find_local_worktree(repo_root, number, false)
            .await?
            .map(|w| Worktree {
                id: w.path.clone(),
                path: w.path,
                branch: w.branch,
            }))
    }

    pub async fn create_worktree(
        &self,
        repo_root: &str,
        name: &str,
        base_branch: Option<&str>,
    ) -> Result<Worktree> {
        let (path, branch) = add_local_worktree(repo_root, name, base_branch).await?;
        Ok(Worktree {
            id: path.clone(),
            path,
            branch: Some(branch),
        })
    }

    pub async fn worktree_exists(&self, path: &str) -> Result<bool> {
        Ok(Path::new(path).join(".git").exists())
    }

    pub async fn ps(&self) -> Result<Vec<WorkspaceInfo>> {
        let mut out = Vec::new();
        for root in &self.roots {
            let Ok(list) = local_worktrees(root).await else {
                continue;
            };
            for w in list {
                let name = Path::new(&w.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let item = number_of_name(&name);
                let session = Self::session_of(&w.path);
                let agents = session
                    .iter()
                    .map(|id| AgentInfo {
                        state: "cloud".into(),
                        agent_type: Some("claude".into()),
                        last_assistant_message: Some(format!("https://claude.ai/code/{id}")),
                        ..Default::default()
                    })
                    .collect();
                out.push(WorkspaceInfo {
                    worktree_id: w.path.clone(),
                    repo_id: root.clone(),
                    path: w.path.clone(),
                    display_name: name,
                    branch: w
                        .branch
                        .as_deref()
                        .map(|b| b.strip_prefix("refs/heads/").unwrap_or(b).to_string()),
                    column: None,
                    status: None,
                    is_archived: false,
                    live_terminals: u64::from(session.is_some()),
                    linked_issue: item.filter(|(_, r)| !r).map(|(n, _)| n),
                    linked_pr: None,
                    last_activity_at: None,
                    agents,
                });
            }
        }
        Ok(out)
    }

    /// The CLI cannot ask a cloud session whether it is working.
    pub async fn agent_busy(&self, _path: &str) -> Result<bool> {
        Ok(false)
    }

    pub async fn has_live_agent(&self, path: &str) -> Result<bool> {
        Ok(Self::session_of(path).is_some())
    }

    pub async fn remove_worktree(&self, path: &str) -> Result<()> {
        let root = Self::root_of(path).await?;
        remove_local_worktree(&root, path).await
    }

    /// Push the worktree's branch so the cloud VM can clone it. Runs as the
    /// bot through ssf's credential helper.
    async fn push_branch(&self, path: &str) -> Result<()> {
        let me = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "ssf".into());
        let out = Command::new("git")
            .args(["-C", path, "push", "--set-upstream", "origin", "HEAD"])
            .env("GIT_CONFIG_COUNT", "2")
            .env("GIT_CONFIG_KEY_0", "credential.helper")
            .env("GIT_CONFIG_VALUE_0", "")
            .env("GIT_CONFIG_KEY_1", "credential.helper")
            .env("GIT_CONFIG_VALUE_1", format!("!{me} git-credential"))
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .await
            .context("running git push")?;
        if !out.status.success() {
            bail!(
                "git push failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// Create a cloud session for the worktree with `text` as its task.
    async fn create_session(&self, path: &str, command: &str, text: &str) -> Result<String> {
        self.push_branch(path).await?;
        let extra = extra_args(command);
        let id = match self.cfg.environment.as_deref() {
            Some(env) => self.create_headless(path, &extra, env, text).await?,
            None => self.create_in_pty(path, &extra, text).await?,
        };
        Self::record_session(path, &id)?;
        info!(path, session = id, "created a cloud session");
        Ok(id)
    }

    /// `claude -p <text> --environment <id>`: prints JSON with the session id.
    async fn create_headless(
        &self,
        path: &str,
        extra: &[String],
        environment: &str,
        text: &str,
    ) -> Result<String> {
        let mut cmd = self.claude();
        cmd.current_dir(path)
            .arg("-p")
            .arg(text)
            .args(["--environment", environment, "--output-format", "json"])
            .args(extra)
            .stdin(Stdio::null());
        let out = tokio::time::timeout(
            Duration::from_secs(self.cfg.create_timeout_secs),
            cmd.output(),
        )
        .await
        .context("creating the cloud session timed out")?
        .context("running claude")?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let v: Option<Value> = stdout
            .find('{')
            .and_then(|i| serde_json::from_str(&stdout[i..]).ok());
        v.as_ref()
            .and_then(|v| v.get("session_id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| session_id_in(&format!("{stdout}\n{stderr}")))
            .with_context(|| {
                format!(
                    "claude --environment gave no session id: {} {}",
                    stdout.trim().chars().take(400).collect::<String>(),
                    stderr.trim().chars().take(400).collect::<String>()
                )
            })
    }

    /// `claude --cloud <text>` under `script`, which gives it the terminal
    /// it insists on; stopped once the session id has shown up and the task
    /// has had a moment to go out.
    async fn create_in_pty(&self, path: &str, extra: &[String], text: &str) -> Result<String> {
        let dir = crate::config::state_dir().join("cloud");
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let prompt_file = dir.join(format!("prompt-{}.md", std::process::id()));
        std::fs::write(&prompt_file, text)
            .with_context(|| format!("writing {}", prompt_file.display()))?;
        let mut inner = format!(
            "stty cols 200 rows 50 2>/dev/null; exec {} --cloud \"$(cat {})\"",
            shell_quote(&self.cfg.command),
            shell_quote(&prompt_file.to_string_lossy())
        );
        for a in extra {
            inner.push(' ');
            inner.push_str(&shell_quote(a));
        }
        debug!(path, "claude --cloud in a pty");
        let mut child = Command::new("script")
            .args(["-q", "-e", "-f", "-c", &inner, "/dev/null"])
            .current_dir(path)
            .env("TERM", "xterm-256color")
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_ENTRYPOINT")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("spawning script (util-linux) for claude --cloud")?;
        let mut stdout = child.stdout.take().context("no stdout")?;
        let deadline = Instant::now() + Duration::from_secs(self.cfg.create_timeout_secs);
        let mut seen = String::new();
        let mut buf = [0u8; 4096];
        let mut found: Option<String> = None;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match tokio::time::timeout(left, stdout.read(&mut buf)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(n)) => {
                    seen.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if let Some(id) = session_id_in(&strip_ansi(&seen)) {
                        found = Some(id);
                        break;
                    }
                    if seen.len() > 1 << 20 {
                        seen.drain(..seen.len() / 2);
                    }
                }
                Ok(Err(e)) => {
                    warn!("reading claude --cloud output: {e}");
                    break;
                }
            }
        }
        let _ = std::fs::remove_file(&prompt_file);
        let Some(id) = found else {
            let _ = child.kill().await;
            let tail: String = strip_ansi(&seen)
                .chars()
                .rev()
                .take(600)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            bail!(
                "claude --cloud did not report a session id in time: {}",
                tail.trim()
            );
        };
        // Leave it attached a moment so the task is delivered, then stop it;
        // the session lives on in the cloud.
        let grace = Duration::from_secs(self.cfg.attach_grace_secs);
        let _ = tokio::time::timeout(grace, child.wait()).await;
        let _ = child.kill().await;
        Ok(id)
    }

    /// Queue a message into a session.
    async fn send(&self, session: &str, text: &str) -> Result<SendResult> {
        use tokio::io::AsyncWriteExt;
        let mut child = self
            .claude()
            .args(["-p", "--cloud", session, "--output-format", "json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("running claude")?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.trim_end().as_bytes()).await;
            let _ = stdin.write_all(b"\n").await;
        }
        let out = tokio::time::timeout(Duration::from_secs(120), child.wait_with_output())
            .await
            .context("sending to the cloud session timed out")?
            .context("running claude")?;
        Ok(parse_send(
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
            out.status.success(),
        ))
    }

    /// First prompt of a workspace: creates its session.
    pub async fn start(&self, path: &str, command: &str, text: &str) -> Result<String> {
        self.create_session(path, command, text).await
    }

    pub async fn deliver(
        &self,
        path: &str,
        preferred_handle: Option<&str>,
        relaunch: &Relaunch<'_>,
        text: &str,
    ) -> Result<Delivery> {
        let session = Self::session_of(path).or_else(|| preferred_handle.map(str::to_string));
        if let Some(id) = session {
            match self.send(&id, text).await? {
                SendResult::Sent(_) => {
                    return Ok(Delivery {
                        handle: id,
                        relaunched: false,
                        resumed: false,
                    });
                }
                SendResult::Gone(why) => {
                    warn!(
                        path,
                        session = id,
                        "cloud session is gone ({why}); starting a new one"
                    );
                    Self::forget_session(path);
                }
                SendResult::Failed(why) => bail!("sending to cloud session {id} failed: {why}"),
            }
        }
        let body = relaunch.text.unwrap_or(text);
        let id = self.create_session(path, relaunch.command, body).await?;
        Ok(Delivery {
            handle: id,
            relaunched: true,
            resumed: false,
        })
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Terminal output without escape sequences, so ids can be found in it.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for d in chars.by_ref() {
                        if ('@'..='~').contains(&d) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(d) = chars.next() {
                        if d == '\x07' {
                            break;
                        }
                        if d == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            }
        } else if c == '\r' {
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_session_ids() {
        assert_eq!(
            session_id_in("View: https://claude.ai/code/session_01DiUkqY2kzbUbDmW1w96rfi?from=cli"),
            Some("session_01DiUkqY2kzbUbDmW1w96rfi".into())
        );
        assert_eq!(
            session_id_in("cse_01DiUkqY2kzbUbDmW1w96rfi"),
            Some("cse_01DiUkqY2kzbUbDmW1w96rfi".into())
        );
        assert_eq!(session_id_in("no session_ here"), None);
        assert_eq!(session_id_in("session_abc"), None);
    }

    #[test]
    fn parses_send_results() {
        assert_eq!(
            parse_send(
                r#"{"ok":true,"session_id":"session_01Dabcdefghijklmnop","url":"u"}"#,
                "",
                true
            ),
            SendResult::Sent("session_01Dabcdefghijklmnop".into())
        );
        assert!(matches!(
            parse_send(
                r#"{"ok":false,"session_id":"s","error":"cloud session s is archived and cannot accept new messages"}"#,
                "",
                false
            ),
            SendResult::Gone(_)
        ));
        assert!(matches!(
            parse_send("", "Error: Session not found: session_x", false),
            SendResult::Gone(_)
        ));
        assert!(matches!(
            parse_send(
                "",
                "Error: Couldn't verify your organization's policy",
                false
            ),
            SendResult::Failed(_)
        ));
        assert!(matches!(
            parse_send(
                "Sent to cloud session.\nSession ID: session_01DiUkqY2kzbUbDmW1w96rfi\n",
                "",
                true
            ),
            SendResult::Sent(id) if id == "session_01DiUkqY2kzbUbDmW1w96rfi"
        ));
    }

    #[test]
    fn harness_flags_pass_through() {
        assert_eq!(
            extra_args("claude --model opus --effort high"),
            vec!["--model", "opus", "--effort", "high"]
        );
        assert_eq!(
            extra_args("claude --dangerously-skip-permissions --model 'claude-opus-5'"),
            vec!["--model", "claude-opus-5"]
        );
        assert!(extra_args("claude").is_empty());
    }

    #[test]
    fn strips_escape_sequences() {
        assert_eq!(
            strip_ansi("\x1b[32mok\x1b[0m\r\n\x1b]0;title\x07x"),
            "ok\n\nx"
        );
    }
}
