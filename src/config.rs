//! Configuration: `~/.config/ssf/config.toml` plus a separate 0600 token file.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_ORCA_COMMAND: &str = "/usr/lib/orca-ide/bin/orca-ide";

/// What runs the agents: the multiplexer that holds the workspaces and
/// terminals ssf creates and delivers prompts into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DriverKind {
    /// The Orca desktop app and CLI (`orca-ide`).
    #[default]
    Orca,
    /// The herdr terminal workspace manager (`herdr`).
    Herdr,
}

impl DriverKind {
    #[cfg(test)]
    pub const ALL: [DriverKind; 2] = [DriverKind::Orca, DriverKind::Herdr];

    /// The config value (`orca`, `herdr`).
    pub fn id(self) -> &'static str {
        match self {
            DriverKind::Orca => "orca",
            DriverKind::Herdr => "herdr",
        }
    }

    /// How the driver is called in messages.
    pub fn label(self) -> &'static str {
        match self {
            DriverKind::Orca => "Orca",
            DriverKind::Herdr => "herdr",
        }
    }
}

impl std::str::FromStr for DriverKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "orca" => Ok(DriverKind::Orca),
            "herdr" => Ok(DriverKind::Herdr),
            other => bail!("unknown driver {other:?}; use orca or herdr"),
        }
    }
}

impl std::fmt::Display for DriverKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Driver that repositories use unless they set their own.
    #[serde(default)]
    pub driver: DriverKind,
    #[serde(default)]
    pub github: GithubConfig,
    #[serde(default)]
    pub orca: OrcaConfig,
    #[serde(default)]
    pub herdr: HerdrConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    /// Running the whole factory inside a Firecracker microVM (see `ssf vm`).
    #[serde(default)]
    pub vm: VmConfig,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HerdrConfig {
    /// The herdr CLI; it talks to the running herdr server over its socket.
    #[serde(default = "default_herdr_command")]
    pub command: String,
    /// Parent directory that ssf clones repositories into (herdr has no
    /// project registry; ssf keeps the checkouts itself). Worktrees go next
    /// to the clone, in `<name>.worktrees/`.
    #[serde(default = "default_ssf_projects_dir")]
    pub projects_dir: String,
    /// How long to wait for a freshly launched agent to be detected in its
    /// pane and become ready for input.
    #[serde(default = "default_tui_timeout")]
    pub tui_idle_timeout_ms: u64,
}

impl Default for HerdrConfig {
    fn default() -> Self {
        Self {
            command: default_herdr_command(),
            projects_dir: default_ssf_projects_dir(),
            tui_idle_timeout_ms: default_tui_timeout(),
        }
    }
}

fn default_herdr_command() -> String {
    std::env::var("HERDR_COMMAND").unwrap_or_else(|_| "herdr".to_string())
}
fn default_ssf_projects_dir() -> String {
    "~/ssf/projects".to_string()
}

fn default_tui_timeout() -> u64 {
    90_000
}

/// `[vm]`: the daemon, herdr and the sessions inside a Firecracker microVM
/// instead of on this machine. The host keeps only what builds, starts,
/// stops and reaches the guest (`ssf vm ...`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmConfig {
    /// Run the factory in the VM: `ssf run` on the host starts and watches
    /// the guest instead of polling GitHub itself, and the daemon-facing
    /// commands (`status`, `tell`, `peers`, ...) run inside the guest.
    #[serde(default)]
    pub enabled: bool,
    /// Name of the VM (its disks live in `<dir>/<name>/`).
    #[serde(default = "default_vm_name")]
    pub name: String,
    /// Where the image, kernel, binaries and the VMs are kept.
    #[serde(default = "default_vm_dir")]
    pub dir: String,
    /// The Firecracker binary; `<dir>/firecracker` (downloaded by `ssf vm build`) when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firecracker: Option<String>,
    /// The gvproxy binary (user-mode networking); `<dir>/gvproxy` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gvproxy: Option<String>,
    /// The guest kernel; `<dir>/vmlinux` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    /// The root image `ssf vm build` makes; `<dir>/rootfs.ext4` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rootfs: Option<String>,
    #[serde(default = "default_vm_vcpus")]
    pub vcpus: u32,
    #[serde(default = "default_vm_mem_mib")]
    pub mem_mib: u32,
    /// Size of the persistent data disk (state, clones and worktrees), made sparse.
    #[serde(default = "default_vm_data_gib")]
    pub data_gib: u32,
    /// Size of the root image `ssf vm build` makes.
    #[serde(default = "default_vm_root_gib")]
    pub root_gib: u32,
    /// Port on 127.0.0.1 where the guest's sshd is reachable.
    #[serde(default = "default_vm_ssh_port")]
    pub ssh_port: u16,
    /// Host files copied into the guest at every start, as `src` or
    /// `src:dest` (`~` allowed; a relative `dest` is under the guest user's
    /// home, and a bare `src` under the host home lands at the same place
    /// there). This is how a harness login gets in, for example
    /// `~/.claude/.credentials.json`.
    #[serde(default)]
    pub files: Vec<String>,
}

impl Default for VmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            name: default_vm_name(),
            dir: default_vm_dir(),
            firecracker: None,
            gvproxy: None,
            kernel: None,
            rootfs: None,
            vcpus: default_vm_vcpus(),
            mem_mib: default_vm_mem_mib(),
            data_gib: default_vm_data_gib(),
            root_gib: default_vm_root_gib(),
            ssh_port: default_vm_ssh_port(),
            files: Vec::new(),
        }
    }
}

fn default_vm_name() -> String {
    "default".to_string()
}
fn default_vm_dir() -> String {
    "~/.local/share/ssf/vm".to_string()
}
fn default_vm_vcpus() -> u32 {
    2
}
fn default_vm_mem_mib() -> u32 {
    4096
}
fn default_vm_data_gib() -> u32 {
    20
}
fn default_vm_root_gib() -> u32 {
    8
}
fn default_vm_ssh_port() -> u16 {
    2222
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    /// Deliver the bot account's own commits and cross-references too
    /// (normally noise), and every session's posts back to it. The bot's
    /// comments are otherwise sorted per session by their origin tag, and
    /// untagged ones (a person typing as the bot) are always delivered.
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
    /// No longer used: an item's workspace is never removed on close. The
    /// agent releases it with `ssf release` once everything is on origin,
    /// and `ssf purge` handles what is left. Accepted so old config files
    /// still load.
    #[serde(default = "default_true")]
    pub cleanup_on_close: bool,
    /// How long a reviewer session gets to finish after its review is done
    /// or its pull request closes before its workspace (a read-only
    /// checkout) is removed anyway. Item workspaces are not affected.
    #[serde(default = "default_cleanup_grace")]
    pub cleanup_grace_secs: u64,
    /// Label that asks for a review of a pull request one of the bot's own
    /// sessions wrote: GitHub refuses a review request from a pull request's
    /// author, so a human (or the author's agent) adds this label instead,
    /// the reviewer session starts, and ssf removes the label once the
    /// review is posted. Empty disables the label trigger.
    #[serde(default = "default_review_label")]
    pub review_label: String,
    /// Resume interrupted sessions when the daemon starts. After a machine
    /// restart Orca's terminals are gone: every active session whose
    /// workspace still exists but has no live agent is started again
    /// (resuming its conversation when possible) with a note that it was
    /// interrupted. Sessions that are still running are never touched, so a
    /// plain daemon restart changes nothing.
    #[serde(default = "default_true")]
    pub resume_on_start: bool,
    /// How long to wait for Orca at daemon start (checking every ten
    /// seconds) before polling begins, since Orca may still be coming up in
    /// the same login. If it is not ready by then, polling starts anyway and
    /// the startup pass runs on the first poll that finds Orca ready.
    #[serde(default = "default_startup_orca_wait")]
    pub startup_orca_wait_secs: u64,
    /// GitHub logins whose assignments, mentions, review requests, labels
    /// and posts ssf acts on, for every repository that has no list of its
    /// own (case-insensitive; the bot itself is always accepted). Unset:
    /// each repository's collaborators with push access. `"*"` means
    /// anyone on GitHub and needs `accepted_anyone_risk = true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_users: Option<Vec<String>>,
    /// The operator has accepted that `allowed_users = ["*"]` lets anyone
    /// on GitHub drive the factory. Written by `ssf config set` after a
    /// confirmation or `--accept-anyone-risk`; a wildcard without it is
    /// refused at load.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accepted_anyone_risk: bool,
}

impl DaemonConfig {
    /// The review label, unless the trigger is disabled.
    pub fn review_label(&self) -> Option<&str> {
        let l = self.review_label.trim();
        (!l.is_empty()).then_some(l)
    }
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
            review_label: default_review_label(),
            resume_on_start: true,
            startup_orca_wait_secs: default_startup_orca_wait(),
            allowed_users: None,
            accepted_anyone_risk: false,
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
fn default_review_label() -> String {
    "review".into()
}

fn default_cleanup_grace() -> u64 {
    900
}
fn default_startup_orca_wait() -> u64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    /// `owner/name` on GitHub.
    pub name: String,
    /// Agent id launched in each issue workspace (`claude`, `codex`, ...).
    pub harness: String,
    /// Driver for this repository's sessions; the top-level `driver` when
    /// not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<DriverKind>,
    /// Shell command that starts the harness (run through `ssf launch`, which
    /// exports the bot credentials). Defaults to the harness's permission-free
    /// command (`models::default_command`): sessions are unattended, so the
    /// harness must never stop to ask.
    #[serde(
        default,
        alias = "relaunch_command",
        skip_serializing_if = "Option::is_none"
    )]
    pub command: Option<String>,
    /// Model the harness runs with, as an Orca model id (`opus`, `gpt-5.5`,
    /// ...); appended to the command as the harness's model flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Effort (reasoning) level for the model, as an Orca effort level
    /// (`low`, `medium`, `high`, `xhigh`, ...); see `ssf agents --json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
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
    /// File whose contents are appended to the initial prompt, relative to
    /// the worktree unless absolute or `~/`-prefixed. Defaults to `SSF.md`
    /// in the repository; a missing file is simply not mentioned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<String>,
    /// Logins that may drive this repository, replacing
    /// `daemon.allowed_users` (an empty list is nobody but the bot). Unset:
    /// the instance list, else the collaborators with push access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_users: Option<Vec<String>>,
    /// See `DaemonConfig::accepted_anyone_risk`; needed for `"*"` here.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accepted_anyone_risk: bool,
}

/// Name of the per-project prompt file when `repo.prompt_file` is not set.
pub const DEFAULT_PROMPT_FILE: &str = "SSF.md";

impl RepoConfig {
    pub fn split(&self) -> Result<(&str, &str)> {
        split_repo_name(&self.name)
    }

    pub fn clone_url(&self) -> String {
        self.clone_url
            .clone()
            .unwrap_or_else(|| format!("https://github.com/{}.git", self.name))
    }

    /// The configured prompt file, as given (`SSF.md` by default).
    pub fn prompt_file(&self) -> &str {
        self.prompt_file
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or(DEFAULT_PROMPT_FILE)
    }

    /// Where the prompt file lives for a checkout at `worktree`: an absolute
    /// or `~/` path stands on its own, anything else is inside the worktree.
    pub fn prompt_file_path(&self, worktree: &Path) -> PathBuf {
        let p = expand_tilde(self.prompt_file());
        if p.is_absolute() { p } else { worktree.join(p) }
    }

    /// The command that starts the harness, with the configured model and
    /// effort level applied: `command` when set, else the harness's
    /// permission-free default.
    pub fn harness_command(&self) -> String {
        let base = self
            .command
            .clone()
            .unwrap_or_else(|| crate::models::default_command(&self.harness));
        crate::models::apply_to_command(
            &base,
            &self.harness,
            self.model.as_deref(),
            self.effort.as_deref(),
        )
    }

    /// Check that the model and effort settings fit the harness.
    pub fn validate_launch_prefs(&self) -> Result<()> {
        crate::models::validate(&self.harness, self.model.as_deref(), self.effort.as_deref())
            .with_context(|| format!("repo {}", self.name))
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
        cfg.validate()?;
        Ok(cfg)
    }

    /// What a config has to satisfy beyond parsing: repository names and
    /// launch settings, and a wildcard allow-list only with its marker.
    pub fn validate(&self) -> Result<()> {
        if self
            .daemon
            .allowed_users
            .as_deref()
            .is_some_and(crate::allow::is_wildcard)
            && !self.daemon.accepted_anyone_risk
        {
            bail!(
                "daemon.allowed_users contains \"*\", which lets ANYONE on GitHub drive the factory, \
                 without daemon.{} = true; run `ssf config set daemon.allowed_users '[\"*\"]' --accept-anyone-risk` \
                 to accept that, or list the logins instead",
                crate::allow::RISK_KEY
            );
        }
        for r in &self.repos {
            r.split()?;
            if r.harness.trim().is_empty() {
                bail!("repo {}: harness must not be empty", r.name);
            }
            r.validate_launch_prefs()?;
            if r.allowed_users
                .as_deref()
                .is_some_and(crate::allow::is_wildcard)
                && !r.accepted_anyone_risk
            {
                bail!(
                    "repo {}: allowed_users contains \"*\", which lets ANYONE on GitHub drive the factory there, \
                     without {} = true on the repo; run `ssf repo set {} --allowed-users '*' --accept-anyone-risk` \
                     to accept that, or list the logins instead",
                    r.name,
                    crate::allow::RISK_KEY,
                    r.name
                );
            }
        }
        Ok(())
    }

    /// The configured allow-list for a repository, with where it comes
    /// from; `None` when neither the repo nor the instance sets one (the
    /// collaborators with push access stand in, fetched by the daemon).
    pub fn allowed_users<'a>(
        &'a self,
        repo: &'a RepoConfig,
    ) -> Option<(&'a [String], crate::allow::Source)> {
        if let Some(l) = &repo.allowed_users {
            return Some((l, crate::allow::Source::Repo));
        }
        self.daemon
            .allowed_users
            .as_deref()
            .map(|l| (l, crate::allow::Source::Instance))
    }

    /// Whether the wildcard is in effect for a repository.
    pub fn anyone_allowed(&self, repo: &RepoConfig) -> bool {
        self.allowed_users(repo)
            .is_some_and(|(l, _)| crate::allow::is_wildcard(l))
    }

    /// Whether any repository is open to anyone (with no repositories, the
    /// instance list decides).
    pub fn anyone_allowed_anywhere(&self) -> bool {
        if self.repos.is_empty() {
            return self
                .daemon
                .allowed_users
                .as_deref()
                .is_some_and(crate::allow::is_wildcard);
        }
        self.repos.iter().any(|r| self.anyone_allowed(r))
    }

    /// One line on who may drive a repository, from the config alone.
    pub fn access_summary(&self, repo: &RepoConfig) -> String {
        match self.allowed_users(repo) {
            Some((l, source)) => crate::allow::AllowList::new(
                self.github.login.as_deref().unwrap_or("bot"),
                l.iter().map(String::as_str),
                source,
            )
            .describe(),
            None => "collaborators with push access (default)".to_string(),
        }
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

    /// Where a driver clones repositories that have no checkout yet.
    pub fn projects_dir(&self, driver: DriverKind) -> PathBuf {
        expand_tilde(match driver {
            DriverKind::Orca => &self.orca.projects_dir,
            DriverKind::Herdr => &self.herdr.projects_dir,
        })
    }

    /// The driver a repository's sessions run under.
    pub fn driver_for(&self, repo: &RepoConfig) -> DriverKind {
        repo.driver.unwrap_or(self.driver)
    }

    /// Every driver some repository uses (the default one when there are
    /// no repositories, so `ssf doctor` has something to check).
    pub fn drivers_in_use(&self) -> Vec<DriverKind> {
        let mut out: Vec<DriverKind> = self.repos.iter().map(|r| self.driver_for(r)).collect();
        if out.is_empty() {
            out.push(self.driver);
        }
        out.sort();
        out.dedup();
        out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drivers_come_from_the_top_level_and_per_repo() {
        let cfg: Config = toml::from_str(
            r#"
driver = "herdr"
[herdr]
projects_dir = "~/work"
[[repo]]
name = "a/b"
harness = "claude"
[[repo]]
name = "c/d"
harness = "claude"
driver = "orca"
"#,
        )
        .unwrap();
        assert_eq!(cfg.driver, DriverKind::Herdr);
        assert_eq!(cfg.driver_for(&cfg.repos[0]), DriverKind::Herdr);
        assert_eq!(cfg.driver_for(&cfg.repos[1]), DriverKind::Orca);
        assert_eq!(
            cfg.drivers_in_use(),
            vec![DriverKind::Orca, DriverKind::Herdr]
        );
        assert!(cfg.projects_dir(DriverKind::Herdr).ends_with("work"));
        assert!(
            cfg.projects_dir(DriverKind::Orca)
                .ends_with("orca/projects")
        );
        let empty = Config::default();
        assert_eq!(empty.driver, DriverKind::Orca);
        assert_eq!(empty.drivers_in_use(), vec![DriverKind::Orca]);
        assert_eq!("Herdr".parse::<DriverKind>().unwrap(), DriverKind::Herdr);
        assert!("tmux".parse::<DriverKind>().is_err());
        // The per-repo choice round-trips through the file.
        let text = toml::to_string(&cfg).unwrap();
        assert!(text.contains("driver = \"orca\""));
        let again: Config = toml::from_str(&text).unwrap();
        assert_eq!(again.repos[1].driver, Some(DriverKind::Orca));
        assert_eq!(again.repos[0].driver, None);
    }

    fn parse(toml_src: &str) -> Result<Config> {
        let dir = std::env::temp_dir().join(format!("ssf-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!(
            "{}.toml",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, toml_src).unwrap();
        let r = Config::load_from(&path);
        let _ = std::fs::remove_file(&path);
        r
    }

    #[test]
    fn the_review_label_defaults_to_review_and_can_be_disabled() {
        let cfg = parse(
            r#"
[[repo]]
name = "acme/widgets"
harness = "claude"
"#,
        )
        .unwrap();
        assert_eq!(cfg.daemon.review_label(), Some("review"));
        let cfg = parse(
            r#"
[daemon]
review_label = " needs-review "

[[repo]]
name = "acme/widgets"
harness = "claude"
"#,
        )
        .unwrap();
        assert_eq!(cfg.daemon.review_label(), Some("needs-review"));
        let cfg = parse(
            r#"
[daemon]
review_label = ""

[[repo]]
name = "acme/widgets"
harness = "claude"
"#,
        )
        .unwrap();
        assert_eq!(cfg.daemon.review_label(), None);
    }

    #[test]
    fn model_and_effort_are_applied_to_the_command() {
        let cfg = parse(
            r#"
[[repo]]
name = "acme/widgets"
harness = "claude"
command = "claude --dangerously-skip-permissions"
model = "opus"
effort = "high"
"#,
        )
        .unwrap();
        assert_eq!(
            cfg.repos[0].harness_command(),
            "claude --dangerously-skip-permissions --model opus --effort high"
        );
        let cfg = parse(
            r#"
[[repo]]
name = "acme/widgets"
harness = "codex"
model = "gpt-5.5"
"#,
        )
        .unwrap();
        assert_eq!(
            cfg.repos[0].harness_command(),
            "codex --dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust -m gpt-5.5"
        );
    }

    #[test]
    fn default_command_is_permission_free_and_command_overrides_it() {
        let cfg = parse(
            r#"
[[repo]]
name = "acme/widgets"
harness = "claude"

[[repo]]
name = "acme/gadgets"
harness = "claude"
command = "claude --permission-mode acceptEdits"

[[repo]]
name = "acme/other"
harness = "aider"
"#,
        )
        .unwrap();
        assert_eq!(
            cfg.repos[0].harness_command(),
            "claude --dangerously-skip-permissions --disallowedTools AskUserQuestion"
        );
        assert_eq!(
            cfg.repos[1].harness_command(),
            "claude --permission-mode acceptEdits"
        );
        assert_eq!(cfg.repos[2].harness_command(), "aider");
        // Resuming builds on the same base.
        assert_eq!(
            crate::sessions::resume_command("claude", &cfg.repos[0].harness_command(), "abc")
                .unwrap(),
            "claude --dangerously-skip-permissions --disallowedTools AskUserQuestion --resume abc"
        );
    }

    #[test]
    fn bad_effort_is_rejected_at_load() {
        let err = parse(
            r#"
[[repo]]
name = "acme/widgets"
harness = "claude"
effort = "ultra"
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("acme/widgets"), "{err:#}");
        assert!(format!("{err:#}").contains("ultra"), "{err:#}");
    }

    #[test]
    fn model_for_a_harness_without_model_support_is_rejected() {
        assert!(
            parse(
                r#"
[[repo]]
name = "acme/widgets"
harness = "crush"
model = "x"
"#,
            )
            .is_err()
        );
    }

    #[test]
    fn settings_round_trip_through_toml() {
        let cfg = parse(
            r#"
[[repo]]
name = "acme/widgets"
harness = "claude"
model = "sonnet"
effort = "low"
"#,
        )
        .unwrap();
        let out = toml::to_string_pretty(&cfg).unwrap();
        assert!(out.contains("model = \"sonnet\""), "{out}");
        assert!(out.contains("effort = \"low\""), "{out}");
    }
    #[test]
    fn a_wildcard_allow_list_needs_its_marker() {
        let err = parse(
            r#"
[daemon]
allowed_users = ["*"]
"#,
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("--accept-anyone-risk"),
            "{err:#}"
        );
        assert!(
            format!("{err:#}").contains("daemon.allowed_users"),
            "{err:#}"
        );
        let cfg = parse(
            r#"
[daemon]
allowed_users = ["*"]
accepted_anyone_risk = true
"#,
        )
        .unwrap();
        assert!(cfg.anyone_allowed_anywhere());
        let err = parse(
            r#"
[[repo]]
name = "acme/widgets"
harness = "claude"
allowed_users = ["alice", "*"]
"#,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("acme/widgets"), "{err:#}");
        assert!(
            format!("{err:#}").contains("ssf repo set acme/widgets"),
            "{err:#}"
        );
        let cfg = parse(
            r#"
[[repo]]
name = "acme/widgets"
harness = "claude"
allowed_users = ["alice", "*"]
accepted_anyone_risk = true

[[repo]]
name = "acme/gadgets"
harness = "claude"
allowed_users = ["alice"]
"#,
        )
        .unwrap();
        assert!(cfg.anyone_allowed(&cfg.repos[0]));
        assert!(!cfg.anyone_allowed(&cfg.repos[1]));
        assert!(cfg.anyone_allowed_anywhere());
        // The marker round-trips only when set.
        let out = toml::to_string_pretty(&cfg).unwrap();
        assert_eq!(
            out.matches("accepted_anyone_risk = true").count(),
            1,
            "{out}"
        );
        assert!(!out.contains("accepted_anyone_risk = false"), "{out}");
    }

    #[test]
    fn the_repo_list_replaces_the_instance_list() {
        let cfg = parse(
            r#"
[github]
login = "bot"

[daemon]
allowed_users = ["Alice"]

[[repo]]
name = "acme/a"
harness = "claude"

[[repo]]
name = "acme/b"
harness = "claude"
allowed_users = ["bob"]

[[repo]]
name = "acme/c"
harness = "claude"
allowed_users = []
"#,
        )
        .unwrap();
        use crate::allow::Source;
        let (a, b, c) = (&cfg.repos[0], &cfg.repos[1], &cfg.repos[2]);
        assert_eq!(
            cfg.allowed_users(a).map(|(l, s)| (l.to_vec(), s)),
            Some((vec!["Alice".to_string()], Source::Instance))
        );
        assert_eq!(
            cfg.allowed_users(b).map(|(l, s)| (l.to_vec(), s)),
            Some((vec!["bob".to_string()], Source::Repo))
        );
        assert_eq!(
            cfg.allowed_users(c).map(|(l, s)| (l.to_vec(), s)),
            Some((vec![], Source::Repo))
        );
        assert_eq!(cfg.access_summary(a), "@alice (instance list)");
        assert_eq!(cfg.access_summary(b), "@bob (repo list)");
        assert_eq!(
            cfg.access_summary(c),
            "nobody but the bot (repo list: empty)"
        );
        assert!(!cfg.anyone_allowed_anywhere());
        // Neither set: the collaborators, fetched by the daemon.
        let cfg = parse(
            r#"
[[repo]]
name = "acme/a"
harness = "claude"
"#,
        )
        .unwrap();
        assert!(cfg.allowed_users(&cfg.repos[0]).is_none());
        assert_eq!(
            cfg.access_summary(&cfg.repos[0]),
            "collaborators with push access (default)"
        );
        assert!(!cfg.anyone_allowed_anywhere());
    }
}
