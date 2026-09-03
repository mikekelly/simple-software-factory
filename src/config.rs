//! Configuration: `~/.config/ssf/config.toml` plus a separate 0600 token file.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_ORCA_COMMAND: &str = "/usr/lib/orca-ide/bin/orca-ide";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub github: GithubConfig,
    #[serde(default)]
    pub orca: OrcaConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default, rename = "repo")]
    pub repos: Vec<RepoConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubConfig {
    /// REST API base URL (override for GitHub Enterprise).
    #[serde(default = "default_api_url")]
    pub api_url: String,
    /// Bot token. Prefer the token file (`ssf auth login`) or `SSF_GITHUB_TOKEN`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Bot account login, set by `ssf auth login`; the token must belong to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    /// Author/committer email used for the bot's commits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Private key enrolled on the bot account for SSH auth and commit signing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_key_path: Option<String>,
    /// Ids of the enrolled keys on GitHub, so `ssf auth logout` can revoke them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_key_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_key_id: Option<u64>,
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            api_url: default_api_url(),
            token: None,
            login: None,
            email: None,
            ssh_key_path: None,
            ssh_key_id: None,
            signing_key_id: None,
        }
    }
}

impl GithubConfig {
    /// Host that git talks to (github.com, or the GHE host behind `api_url`).
    pub fn git_host(&self) -> String {
        let host = self
            .api_url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or("api.github.com")
            .to_string();
        if host == "api.github.com" {
            "github.com".to_string()
        } else {
            host
        }
    }
}

fn default_api_url() -> String {
    "https://api.github.com".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrcaConfig {
    /// Path to the Orca CLI binary. Must be the CLI entry point, not the
    /// desktop launcher (`/usr/bin/orca-ide` starts the app).
    #[serde(default = "default_orca_command")]
    pub command: String,
    /// Orca host id to create projects and worktrees on.
    #[serde(default = "default_host")]
    pub host: String,
    /// Parent directory that new project clones are placed in.
    #[serde(default = "default_projects_dir")]
    pub projects_dir: String,
    /// How long to wait for a project clone to become ready.
    #[serde(default = "default_setup_timeout")]
    pub setup_timeout_secs: u64,
    /// How long to wait for a freshly launched agent TUI to become idle.
    #[serde(default = "default_tui_timeout")]
    pub tui_idle_timeout_ms: u64,
}

impl Default for OrcaConfig {
    fn default() -> Self {
        Self {
            command: default_orca_command(),
            host: default_host(),
            projects_dir: default_projects_dir(),
            setup_timeout_secs: default_setup_timeout(),
            tui_idle_timeout_ms: default_tui_timeout(),
        }
    }
}

fn default_orca_command() -> String {
    std::env::var("ORCA_CLI_COMMAND").unwrap_or_else(|_| DEFAULT_ORCA_COMMAND.to_string())
}
fn default_host() -> String {
    "local".to_string()
}
fn default_projects_dir() -> String {
    "~/orca/projects".to_string()
}
fn default_setup_timeout() -> u64 {
    900
}
fn default_tui_timeout() -> u64 {
    90_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    /// Deliver events performed by the bot account itself (normally noise:
    /// the agent's own comments would be echoed back to it).
    #[serde(default)]
    pub include_own_events: bool,
    /// Timeline event types that are never delivered.
    #[serde(default = "default_ignored_events")]
    pub ignored_events: Vec<String>,
    /// Maximum characters of a single comment body included in a prompt.
    #[serde(default = "default_max_body_chars")]
    pub max_body_chars: usize,
    /// Extra instructions appended to every initial prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Remove the Orca workspace after an issue is closed (once the agent has
    /// finished wrapping up). The agent's conversation is kept on disk and the
    /// workspace is re-created, resuming that conversation, if the issue comes
    /// back to life.
    #[serde(default = "default_true")]
    pub cleanup_on_close: bool,
    /// How long to wait for the agent to finish after a close before removing
    /// the workspace anyway.
    #[serde(default = "default_cleanup_grace")]
    pub cleanup_grace_secs: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: default_poll_interval(),
            include_own_events: false,
            ignored_events: default_ignored_events(),
            max_body_chars: default_max_body_chars(),
            instructions: None,
            cleanup_on_close: true,
            cleanup_grace_secs: default_cleanup_grace(),
        }
    }
}

fn default_poll_interval() -> u64 {
    10
}
fn default_ignored_events() -> Vec<String> {
    ["mentioned", "subscribed", "unsubscribed"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}
fn default_max_body_chars() -> usize {
    8000
}
fn default_true() -> bool {
    true
}
fn default_cleanup_grace() -> u64 {
    900
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    /// `owner/name` on GitHub.
    pub name: String,
    /// Orca agent id launched in each issue workspace (`claude`, `codex`, ...).
    pub harness: String,
    /// Shell command that starts the harness (run through `ssf launch`, which
    /// exports the bot credentials). Defaults to the harness id.
    #[serde(
        default,
        alias = "relaunch_command",
        skip_serializing_if = "Option::is_none"
    )]
    pub command: Option<String>,
    /// Clone URL used when Orca has no project for this repo yet.
    /// Defaults to `https://github.com/owner/name.git`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_url: Option<String>,
    /// Existing local checkout to import instead of cloning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Base ref for issue worktrees (defaults to the repo's Orca base ref).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    /// Repo-specific instructions appended to the initial prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

impl RepoConfig {
    pub fn split(&self) -> Result<(&str, &str)> {
        split_repo_name(&self.name)
    }

    pub fn clone_url(&self) -> String {
        self.clone_url
            .clone()
            .unwrap_or_else(|| format!("https://github.com/{}.git", self.name))
    }

    pub fn harness_command(&self) -> String {
        self.command.clone().unwrap_or_else(|| self.harness.clone())
    }
}

pub fn split_repo_name(name: &str) -> Result<(&str, &str)> {
    match name.split_once('/') {
        Some((o, r)) if !o.is_empty() && !r.is_empty() && !r.contains('/') => Ok((o, r)),
        _ => bail!("repo name must be of the form owner/name, got {name:?}"),
    }
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SSF_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("ssf")
}

pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SSF_STATE_DIR") {
        return PathBuf::from(dir);
    }
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("~/.local/state"))
        .join("ssf")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn token_path() -> PathBuf {
    config_dir().join("token")
}

pub fn default_key_path(login: &str) -> PathBuf {
    config_dir().join("keys").join(format!("{login}_ed25519"))
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        for r in &cfg.repos {
            r.split()?;
            if r.harness.trim().is_empty() {
                bail!("repo {}: harness must not be empty", r.name);
            }
        }
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let body = toml::to_string_pretty(self).context("serialising config")?;
        write_atomic(&path, body.as_bytes(), 0o600)
    }

    /// Resolve the bot token: env var, then config, then a pasted token file,
    /// then the GitHub CLI's keyring for the signed-in bot account.
    pub fn github_token(&self) -> Result<String> {
        if let Ok(t) = std::env::var("SSF_GITHUB_TOKEN") {
            if !t.trim().is_empty() {
                return Ok(t.trim().to_string());
            }
        }
        if let Some(t) = &self.github.token {
            if !t.trim().is_empty() {
                return Ok(t.trim().to_string());
            }
        }
        let path = token_path();
        match std::fs::read_to_string(&path) {
            Ok(t) if !t.trim().is_empty() => return Ok(t.trim().to_string()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        }
        if let Some(login) = &self.github.login {
            return crate::ghcli::token_for(&self.github.git_host(), login).with_context(|| {
                format!("bot @{login} is not signed in to gh any more; run `ssf auth login`")
            });
        }
        bail!("no bot account signed in; run `ssf auth login`")
    }

    /// Where the token comes from, for status output.
    pub fn token_source(&self) -> &'static str {
        if std::env::var("SSF_GITHUB_TOKEN").is_ok_and(|t| !t.trim().is_empty()) {
            "SSF_GITHUB_TOKEN"
        } else if self.github.token.is_some() {
            "config.toml"
        } else if token_path().exists() {
            "token file"
        } else if self.github.login.is_some() {
            "gh keyring"
        } else {
            "none"
        }
    }

    pub fn projects_dir(&self) -> PathBuf {
        expand_tilde(&self.orca.projects_dir)
    }
}

pub fn save_token(token: &str) -> Result<PathBuf> {
    let path = token_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    write_atomic(&path, format!("{}\n", token.trim()).as_bytes(), 0o600)?;
    Ok(path)
}

pub fn write_atomic(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    Ok(())
}
