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
    Orca,
    /// The herdr terminal workspace manager (`herdr`); the default since
    /// 2026-09-06 (it was Orca before, see `Config::driver_note`).
    #[default]
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

    /// The driver whose repo ids look like `repo_id`: herdr's is the path
    /// of the checkout, Orca's is a uuid. For records from before the
    /// driver was written down next to the id.
    pub fn of_repo_id(repo_id: &str) -> Option<DriverKind> {
        if std::path::Path::new(repo_id).is_absolute() {
            Some(DriverKind::Herdr)
        } else if !repo_id.is_empty() && !repo_id.contains('/') {
            Some(DriverKind::Orca)
        } else {
            None
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
    /// Driver that repositories use unless they set their own; herdr when
    /// not set (`default_driver`). Kept optional so a file that never set
    /// it stays that way through `ssf config set` / `ssf repo add`, and
    /// `driver_note` can say the default is what is in effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<DriverKind>,
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
    /// Who the agents' commits are by and who pushes them, when not the
    /// bot; a `[[repo]]` can override any key with its own `[repo.git]`.
    #[serde(default, skip_serializing_if = "GitConfig::is_empty")]
    pub git: GitConfig,
    #[serde(default, rename = "repo")]
    pub repos: Vec<RepoConfig>,
}

/// The git identity agents commit with, instance-wide (`[git]`) or per
/// repository (`[repo.git]`). Every key is optional and the two tables are
/// merged key by key, the repository's winning; what is left unset comes
/// from the bot (`github.login`, its noreply email, its enrolled key, its
/// token). `gh` and the GitHub API are the bot whatever stands here.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitConfig {
    /// Author and committer name; set together with `email`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Author and committer email: one the person's GitHub account has
    /// verified, or their `id+login@users.noreply.github.com`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// SSH key to sign commits and tags with (a path), or `false` for
    /// unsigned. Unset: the bot's enrolled key when the identity is the
    /// bot's, unsigned when it is a person's (a signature by a key that
    /// is not registered on the author's account shows as unverified).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_key: Option<SigningKey>,
    /// Who pushes over HTTPS: `bot` (the default), `token:<login>` (the
    /// token gh holds for that account on this machine), `file:<path>` (a
    /// file holding a token), or a git credential helper string used as
    /// `credential.helper` (`!gh auth git-credential`, `store`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

/// `signing_key = "<path>"` or `signing_key = false`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum SigningKey {
    Path(String),
    Off(bool),
}

impl GitConfig {
    pub fn is_empty(&self) -> bool {
        *self == GitConfig::default()
    }

    /// This table with `over` laid on top, key by key.
    pub fn merged(&self, over: &GitConfig) -> GitConfig {
        GitConfig {
            name: over.name.clone().or_else(|| self.name.clone()),
            email: over.email.clone().or_else(|| self.email.clone()),
            signing_key: over
                .signing_key
                .clone()
                .or_else(|| self.signing_key.clone()),
            credential: over.credential.clone().or_else(|| self.credential.clone()),
        }
    }

    /// What a table has to satisfy on its own (the merge is checked again
    /// per repository, since name and email may come from different levels).
    fn validate(&self, where_: &str) -> Result<()> {
        if let Some(SigningKey::Off(true)) = self.signing_key {
            bail!("{where_}.signing_key is a path to an SSH key, or false; `true` says nothing");
        }
        if let Some(SigningKey::Path(p)) = &self.signing_key
            && p.trim().is_empty()
        {
            bail!("{where_}.signing_key is empty; give a path, or false");
        }
        for (key, value) in [("name", &self.name), ("email", &self.email)] {
            if value.as_deref().is_some_and(|v| v.trim().is_empty()) {
                bail!("{where_}.{key} is empty");
            }
        }
        if let Some(c) = &self.credential {
            Credential::parse(c).with_context(|| format!("{where_}.credential"))?;
        }
        Ok(())
    }

    /// Name and email go together (author and committer are one person):
    /// the base `[git]` table has both or neither, and a `[repo.git]` that
    /// sets one over an empty base has to set the other too.
    fn validate_merged(&self, where_: &str) -> Result<()> {
        let (set, missing) = match (&self.name, &self.email) {
            (Some(_), None) => ("name", "email"),
            (None, Some(_)) => ("email", "name"),
            _ => return Ok(()),
        };
        bail!(
            "{where_}: git.{set} is set without git.{missing}; a commit identity needs both, e.g. \
             `ssf config set git '{{ name = \"Ann Person\", email = \"ann@example.com\" }}'` \
             or `ssf repo set <owner/name> --git-name ... --git-email ...`"
        )
    }
}

/// Who pushes over HTTPS, parsed from `git.credential`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// The bot's token through `ssf git-credential` (the default).
    Bot,
    /// The token gh holds for this login on the machine the agent runs on.
    Token(String),
    /// A file holding a token, read by `ssf git-credential` at push time.
    File(PathBuf),
    /// A git credential helper string, set as `credential.helper` verbatim.
    Helper(String),
}

impl Credential {
    pub fn parse(s: &str) -> Result<Credential> {
        let s = s.trim();
        if s.is_empty() {
            bail!(
                "credential is empty; use bot, token:<login>, file:<path> or a credential helper"
            );
        }
        if s.eq_ignore_ascii_case("bot") {
            return Ok(Credential::Bot);
        }
        if let Some(login) = s.strip_prefix("token:") {
            let login = login.trim().trim_start_matches('@');
            if login.is_empty() {
                bail!("token: needs the gh login whose token to use, e.g. token:alice");
            }
            return Ok(Credential::Token(login.to_string()));
        }
        if let Some(path) = s.strip_prefix("file:") {
            let path = path.trim();
            if path.is_empty() {
                bail!("file: needs the path of a file holding the token");
            }
            return Ok(Credential::File(expand_tilde(path)));
        }
        // A helper: a command (`!...`), a program path, or a bare helper
        // name with options (`store`, `cache --timeout=3600`). A word with
        // a colon in it is a misspelt `token:`/`file:` rather than any of
        // those.
        let word = s.split_whitespace().next().unwrap_or("");
        if !(s.starts_with('!') || s.starts_with('/')) && word.contains(':') {
            bail!(
                "`{s}` is not bot, token:<login> or file:<path>; a credential helper starts with `!` or `/` or is a helper name such as `store`"
            );
        }
        Ok(Credential::Helper(s.to_string()))
    }

    /// The config value that names this credential.
    pub fn to_config(&self) -> String {
        match self {
            Credential::Bot => "bot".into(),
            Credential::Token(l) => format!("token:{l}"),
            Credential::File(p) => format!("file:{}", p.display()),
            Credential::Helper(h) => h.clone(),
        }
    }

    /// Who `git push` acts as, for the first prompt, when not the bot.
    pub fn prompt_pusher(&self) -> Option<String> {
        match self {
            Credential::Bot => None,
            Credential::Token(l) => Some(format!("@{l}")),
            Credential::File(p) => Some(format!("the account whose token is in `{}`", p.display())),
            Credential::Helper(h) => {
                Some(format!("whoever the credential helper `{h}` answers for"))
            }
        }
    }

    /// A line for `ssf doctor` and `ssf config show`.
    pub fn describe(&self, bot: &str) -> String {
        match self {
            Credential::Bot => format!("pushes as @{bot} (the bot)"),
            Credential::Token(l) => format!("pushes as @{l} (gh keyring token)"),
            Credential::File(p) => format!("pushes with the token in {}", p.display()),
            Credential::Helper(h) => format!("pushes through credential helper `{h}`"),
        }
    }
}

/// Where the effective identity's name and email come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentitySource {
    /// The bot's own login and email (nothing configured).
    Bot,
    /// `[git]`.
    Instance,
    /// `[repo.git]` (at least one of name/email).
    Repo,
}

/// The git identity `ssf launch` gives an agent, after the merge and the
/// bot defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIdentity {
    /// Author and committer; `None` when neither the bot nor `[git]` is
    /// known (a fresh install before `ssf auth login`).
    pub name: Option<String>,
    pub email: Option<String>,
    pub source: IdentitySource,
    /// Private key to sign with; `None` means unsigned. The file may be
    /// missing: `ssf launch` then leaves signing off and says so.
    pub signing_key: Option<PathBuf>,
    pub credential: Credential,
}

impl GitIdentity {
    pub fn is_bot(&self) -> bool {
        self.source == IdentitySource::Bot
    }

    /// `Name <email>` or a note that nothing is recorded.
    pub fn who(&self) -> String {
        match (&self.name, &self.email) {
            (Some(n), Some(e)) => format!("{n} <{e}>"),
            _ => "(no identity recorded; run `ssf auth login` or set [git])".to_string(),
        }
    }

    /// One line: who commits, signed how, who pushes.
    pub fn describe(&self, bot: &str) -> String {
        let signed = match &self.signing_key {
            Some(k) => format!("signed with {}", k.display()),
            None => "unsigned".to_string(),
        };
        let source = match self.source {
            IdentitySource::Bot => "the bot",
            IdentitySource::Instance => "[git]",
            IdentitySource::Repo => "[repo.git]",
        };
        format!(
            "commits as {} ({source}), {signed}, {}",
            self.who(),
            self.credential.describe(bot)
        )
    }
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
    /// The guest's vCPUs. Unset: chosen from this machine (its logical
    /// CPUs minus one, at least 2) and written here by `ssf vm build`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcpus: Option<u32>,
    /// The guest's memory in MiB. Unset: chosen from this machine (half
    /// its RAM, at least 4096) and written here by `ssf vm build`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_mib: Option<u32>,
    /// Size of the persistent data disk (state, clones and worktrees) in
    /// GiB. The file is sparse, so this reserves nothing on the host. Unset:
    /// chosen from this machine (half the free space of the filesystem
    /// holding `dir`, at least 20) and written here by `ssf vm build`; `ssf
    /// vm grow` enlarges an existing disk and updates this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_gib: Option<u32>,
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
            vcpus: None,
            mem_mib: None,
            data_gib: None,
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
    /// No longer used: it timed the reviewer sessions out, and those went
    /// with #115 (ssf runs one session per item). Accepted so old config
    /// files still load; never written back.
    #[serde(default, skip_serializing)]
    pub cleanup_grace_secs: Option<u64>,
    /// No longer used: the label started a reviewer session until #115;
    /// ssf no longer reacts to any label. Accepted so old config files
    /// still load; never written back.
    #[serde(default, skip_serializing)]
    pub review_label: Option<String>,
    /// Resume interrupted sessions when the daemon starts. After a machine
    /// restart Orca's terminals are gone: every active session whose
    /// workspace still exists but has no live agent is started again
    /// (resuming its conversation when possible) with a note that it was
    /// interrupted. Sessions that are still running are never touched, so a
    /// plain daemon restart changes nothing.
    #[serde(default = "default_true")]
    pub resume_on_start: bool,
    /// How long to wait for the driver at daemon start (checking every ten
    /// seconds) before polling begins, since herdr or Orca may still be coming
    /// up in the same login. If it is not ready by then, polling starts anyway
    /// and the startup pass runs on the first poll that finds the driver ready.
    /// (`startup_orca_wait_secs`, its name from when Orca was the only
    /// driver, is still read.)
    #[serde(
        default = "default_startup_driver_wait",
        alias = "startup_orca_wait_secs"
    )]
    pub startup_driver_wait_secs: u64,
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
    /// Keys still in the file that ssf no longer reads, for `ssf doctor`
    /// to mention: `review_label` and `cleanup_grace_secs` belonged to the
    /// reviewer sessions removed in #115.
    pub fn retired_keys(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.review_label.is_some() {
            out.push("daemon.review_label");
        }
        if self.cleanup_grace_secs.is_some() {
            out.push("daemon.cleanup_grace_secs");
        }
        out
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
            cleanup_grace_secs: None,
            review_label: None,
            resume_on_start: true,
            startup_driver_wait_secs: default_startup_driver_wait(),
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
fn default_startup_driver_wait() -> u64 {
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
    /// Model the harness runs with: an Orca model id (`opus`, `gpt-5.5`, ...)
    /// for claude, codex, gemini and grok, `provider/model` for pi, omp,
    /// opencode and copilot; appended to the command as the harness's model
    /// flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Effort (reasoning) level for the model, one the harness accepts
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
    /// Git identity for this repository's agents, key by key over `[git]`.
    #[serde(default, skip_serializing_if = "GitConfig::is_empty")]
    pub git: GitConfig,
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
        self.git.validate("git")?;
        self.git.validate_merged("[git]")?;
        for r in &self.repos {
            r.split()?;
            if r.harness.trim().is_empty() {
                bail!("repo {}: harness must not be empty", r.name);
            }
            r.validate_launch_prefs()?;
            r.git.validate(&format!("repo {}: git", r.name))?;
            self.git
                .merged(&r.git)
                .validate_merged(&format!("repo {}", r.name))?;
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

    /// The `[git]` settings in effect for a repository: `[repo.git]` over
    /// `[git]`, key by key (`[git]` alone without a repository).
    pub fn git_settings(&self, repo: Option<&RepoConfig>) -> GitConfig {
        match repo {
            Some(r) => self.git.merged(&r.git),
            None => self.git.clone(),
        }
    }

    /// The identity `ssf launch` gives an agent working on `repo`: the
    /// merged `[git]` settings with the bot filling in whatever is unset.
    pub fn git_identity(&self, repo: Option<&RepoConfig>) -> GitIdentity {
        let settings = self.git_settings(repo);
        let source = match (&settings.name, &settings.email) {
            (None, None) => IdentitySource::Bot,
            _ if repo.is_some_and(|r| r.git.name.is_some() || r.git.email.is_some()) => {
                IdentitySource::Repo
            }
            _ => IdentitySource::Instance,
        };
        let (name, email) = match source {
            IdentitySource::Bot => {
                let name = self.github.login.clone();
                let email = name.as_ref().map(|login| {
                    self.github
                        .email
                        .clone()
                        .unwrap_or_else(|| format!("{login}@users.noreply.github.com"))
                });
                (name, email)
            }
            _ => (settings.name.clone(), settings.email.clone()),
        };
        let bot_key = self
            .github
            .ssh_key_path
            .as_deref()
            .map(expand_tilde)
            .filter(|_| self.github.signing_key_id.is_some());
        let signing_key = match &settings.signing_key {
            Some(SigningKey::Path(p)) => Some(expand_tilde(p)),
            Some(SigningKey::Off(_)) => None,
            None if source == IdentitySource::Bot => bot_key,
            None => None,
        };
        let credential = settings
            .credential
            .as_deref()
            .and_then(|c| Credential::parse(c).ok())
            .unwrap_or(Credential::Bot);
        GitIdentity {
            name,
            email,
            source,
            signing_key,
            credential,
        }
    }

    /// Where a driver clones repositories that have no checkout yet.
    pub fn projects_dir(&self, driver: DriverKind) -> PathBuf {
        expand_tilde(match driver {
            DriverKind::Orca => &self.orca.projects_dir,
            DriverKind::Herdr => &self.herdr.projects_dir,
        })
    }

    /// The instance-wide driver: the top-level `driver`, or herdr when the
    /// file does not set one.
    pub fn default_driver(&self) -> DriverKind {
        self.driver.unwrap_or_default()
    }

    /// The driver a repository's sessions run under.
    pub fn driver_for(&self, repo: &RepoConfig) -> DriverKind {
        repo.driver.unwrap_or_else(|| self.default_driver())
    }

    /// Every driver some repository uses (the default one when there are
    /// no repositories, so `ssf doctor` has something to check).
    pub fn drivers_in_use(&self) -> Vec<DriverKind> {
        let mut out: Vec<DriverKind> = self.repos.iter().map(|r| self.driver_for(r)).collect();
        if out.is_empty() {
            out.push(self.default_driver());
        }
        out.sort();
        out.dedup();
        out
    }

    /// Why repositories run where they do when the file leaves `driver`
    /// unset: the default moved from Orca to herdr on 2026-09-06, so an
    /// install that relied on the old default changes driver on upgrade
    /// without any edit of its own. `None` when `driver` is set or every
    /// repository picks its own.
    pub fn driver_note(&self) -> Option<String> {
        if self.driver.is_some() {
            return None;
        }
        let relying: Vec<&str> = self
            .repos
            .iter()
            .filter(|r| r.driver.is_none())
            .map(|r| r.name.as_str())
            .collect();
        if relying.is_empty() {
            return None;
        }
        Some(format!(
            "`driver` is not set in config.toml, so {} run{} in {} (the default; it was Orca until 2026-09-06). \
             Keep Orca with `ssf config set driver orca`, or make herdr explicit with `ssf config set driver herdr`.",
            relying.join(", "),
            if relying.len() == 1 { "s" } else { "" },
            DriverKind::default().label()
        ))
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
    fn vm_sizes_stay_unset_until_written_and_old_files_pin_them() {
        // Unset: not in the file, so a later `ssf vm build` chooses them.
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert!(!text.contains("vcpus"), "{text}");
        assert!(!text.contains("mem_mib"), "{text}");
        assert!(!text.contains("data_gib"), "{text}");
        assert!(text.contains("root_gib = 8"), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.vm.vcpus, None);
        assert_eq!(back.vm.data_gib, None);
        // Set: written and read back.
        let mut cfg = Config::default();
        cfg.vm.vcpus = Some(3);
        cfg.vm.mem_mib = Some(15872);
        cfg.vm.data_gib = Some(80);
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert!(text.contains("data_gib = 80"), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(
            (back.vm.vcpus, back.vm.mem_mib, back.vm.data_gib),
            (Some(3), Some(15872), Some(80))
        );
        // A file from before, with the old constants written out, keeps them.
        let old: Config =
            toml::from_str("[vm]\nvcpus = 2\nmem_mib = 4096\ndata_gib = 20\n").unwrap();
        assert_eq!(old.vm.data_gib, Some(20));
    }

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
        assert_eq!(cfg.driver, Some(DriverKind::Herdr));
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
        assert_eq!(empty.driver, None);
        assert_eq!(empty.default_driver(), DriverKind::Herdr);
        assert_eq!(empty.drivers_in_use(), vec![DriverKind::Herdr]);
        assert_eq!("Herdr".parse::<DriverKind>().unwrap(), DriverKind::Herdr);
        assert!("tmux".parse::<DriverKind>().is_err());
        // The per-repo choice round-trips through the file.
        let text = toml::to_string(&cfg).unwrap();
        assert!(text.contains("driver = \"orca\""));
        let again: Config = toml::from_str(&text).unwrap();
        assert_eq!(again.repos[1].driver, Some(DriverKind::Orca));
        assert_eq!(again.repos[0].driver, None);
    }

    #[test]
    fn startup_wait_reads_its_old_orca_name_and_writes_the_new_one() {
        let old: Config = toml::from_str("[daemon]\nstartup_orca_wait_secs = 7\n").unwrap();
        assert_eq!(old.daemon.startup_driver_wait_secs, 7);
        let new: Config = toml::from_str("[daemon]\nstartup_driver_wait_secs = 9\n").unwrap();
        assert_eq!(new.daemon.startup_driver_wait_secs, 9);
        assert_eq!(Config::default().daemon.startup_driver_wait_secs, 120);
        let text = toml::to_string(&old).unwrap();
        assert!(text.contains("startup_driver_wait_secs = 7"));
        assert!(!text.contains("startup_orca_wait_secs"));
    }

    #[test]
    fn herdr_is_the_default_driver_and_repos_fall_back_to_it() {
        let cfg: Config = toml::from_str(
            r#"
[orca]
projects_dir = "~/orca/projects"
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
        assert_eq!(DriverKind::default(), DriverKind::Herdr);
        assert_eq!(cfg.driver, None);
        assert_eq!(cfg.default_driver(), DriverKind::Herdr);
        assert_eq!(cfg.driver_for(&cfg.repos[0]), DriverKind::Herdr);
        assert_eq!(cfg.driver_for(&cfg.repos[1]), DriverKind::Orca);
        assert_eq!(
            cfg.drivers_in_use(),
            vec![DriverKind::Orca, DriverKind::Herdr]
        );
        // The unset key stays unset through a save, so a later
        // `ssf repo add` does not silently pin the new default.
        let text = toml::to_string(&cfg).unwrap();
        assert!(!text.starts_with("driver"), "{text}");
        assert!(!text.contains("\ndriver = \"herdr\""), "{text}");
        let again: Config = toml::from_str(&text).unwrap();
        assert_eq!(again.driver, None);
    }

    #[test]
    fn driver_note_only_when_a_repo_relies_on_the_unset_default() {
        let mut cfg: Config = toml::from_str(
            r#"
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
        let note = cfg.driver_note().expect("a/b relies on the default");
        assert!(note.contains("a/b runs in herdr"), "{note}");
        assert!(!note.contains("c/d"), "{note}");
        assert!(note.contains("ssf config set driver orca"), "{note}");
        // Set explicitly (either way): nothing to say.
        cfg.driver = Some(DriverKind::Orca);
        assert_eq!(cfg.driver_note(), None);
        cfg.driver = Some(DriverKind::Herdr);
        assert_eq!(cfg.driver_note(), None);
        // Unset, but every repository picks its own: nothing to say.
        cfg.driver = None;
        cfg.repos[0].driver = Some(DriverKind::Herdr);
        assert_eq!(cfg.driver_note(), None);
        // No repositories at all: nothing runs anywhere yet.
        assert_eq!(Config::default().driver_note(), None);
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
    fn the_old_reviewer_keys_still_load_and_are_not_written_back() {
        let cfg = parse(
            r#"
[daemon]
review_label = "review"
cleanup_grace_secs = 900

[[repo]]
name = "acme/widgets"
harness = "claude"
"#,
        )
        .unwrap();
        assert_eq!(cfg.daemon.review_label.as_deref(), Some("review"));
        assert_eq!(cfg.daemon.cleanup_grace_secs, Some(900));
        assert_eq!(
            cfg.daemon.retired_keys(),
            vec!["daemon.review_label", "daemon.cleanup_grace_secs"]
        );
        assert!(DaemonConfig::default().retired_keys().is_empty());
        let out = toml::to_string(&cfg).unwrap();
        assert!(!out.contains("review_label"), "{out}");
        assert!(!out.contains("cleanup_grace_secs"), "{out}");
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

    fn bot_config() -> Config {
        let mut cfg = Config::default();
        cfg.github.login = Some("acme-bot".into());
        cfg.github.email = Some("1+acme-bot@users.noreply.github.com".into());
        cfg.github.ssh_key_path = Some("/keys/acme-bot_ed25519".into());
        cfg.github.signing_key_id = Some(7);
        cfg.repos.push(RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            ..RepoConfig::default()
        });
        cfg
    }

    #[test]
    fn git_identity_is_the_bot_unless_configured() {
        let cfg = bot_config();
        let id = cfg.git_identity(Some(&cfg.repos[0]));
        assert!(id.is_bot());
        assert_eq!(id.name.as_deref(), Some("acme-bot"));
        assert_eq!(
            id.email.as_deref(),
            Some("1+acme-bot@users.noreply.github.com")
        );
        assert_eq!(
            id.signing_key.as_deref(),
            Some(Path::new("/keys/acme-bot_ed25519"))
        );
        assert_eq!(id.credential, Credential::Bot);
        // Nothing recorded at all: no identity, still the bot credential.
        let empty = Config::default();
        let id = empty.git_identity(None);
        assert!(id.name.is_none() && id.email.is_none() && id.signing_key.is_none());
        assert!(id.who().contains("no identity"));
        // A bot without a signing key enrolled signs nothing.
        let mut cfg = bot_config();
        cfg.github.signing_key_id = None;
        assert!(cfg.git_identity(None).signing_key.is_none());
    }

    #[test]
    fn git_tables_merge_key_by_key_with_the_repo_winning() {
        let mut cfg = bot_config();
        cfg.git = GitConfig {
            name: Some("Ann Person".into()),
            email: Some("ann@example.com".into()),
            signing_key: None,
            credential: Some("token:ann".into()),
        };
        // Instance identity: a person, unsigned by default, pushing as herself.
        let id = cfg.git_identity(Some(&cfg.repos[0]));
        assert_eq!(id.source, IdentitySource::Instance);
        assert_eq!(id.who(), "Ann Person <ann@example.com>");
        assert!(
            id.signing_key.is_none(),
            "a person is unsigned unless a key is given"
        );
        assert_eq!(id.credential, Credential::Token("ann".into()));
        // The repo overrides one key and adds a signing key.
        cfg.repos[0].git = GitConfig {
            email: Some("ann@work.example".into()),
            signing_key: Some(SigningKey::Path("~/.ssh/id_ed25519".into())),
            credential: Some("bot".into()),
            ..GitConfig::default()
        };
        let id = cfg.git_identity(Some(&cfg.repos[0]));
        assert_eq!(id.source, IdentitySource::Repo);
        assert_eq!(id.who(), "Ann Person <ann@work.example>");
        assert_eq!(
            id.signing_key,
            Some(expand_tilde("~/.ssh/id_ed25519")),
            "signing key comes from the repo table"
        );
        assert_eq!(id.credential, Credential::Bot);
        // `[git]` alone still applies with no repository given.
        assert_eq!(
            cfg.git_identity(None).credential,
            Credential::Token("ann".into())
        );
        // Turning the bot's signing off without changing who commits.
        let mut cfg = bot_config();
        cfg.git.signing_key = Some(SigningKey::Off(false));
        let id = cfg.git_identity(Some(&cfg.repos[0]));
        assert!(id.is_bot());
        assert!(id.signing_key.is_none());
        assert!(
            id.describe("acme-bot").contains("unsigned"),
            "{}",
            id.describe("acme-bot")
        );
    }

    #[test]
    fn credential_values_parse() {
        assert_eq!(Credential::parse("bot").unwrap(), Credential::Bot);
        assert_eq!(Credential::parse(" Bot ").unwrap(), Credential::Bot);
        assert_eq!(
            Credential::parse("token:@ann").unwrap(),
            Credential::Token("ann".into())
        );
        assert_eq!(
            Credential::parse("file:/run/secrets/gh").unwrap(),
            Credential::File(PathBuf::from("/run/secrets/gh"))
        );
        assert_eq!(
            Credential::parse("!gh auth git-credential").unwrap(),
            Credential::Helper("!gh auth git-credential".into())
        );
        assert!(Credential::parse("").is_err());
        assert!(Credential::parse("token:").is_err());
        assert!(Credential::parse("file: ").is_err());
        assert!(
            Credential::parse("tokn:ann").is_err(),
            "a misspelt kind is not a helper"
        );
        assert_eq!(
            Credential::parse("cache --timeout=3600").unwrap(),
            Credential::Helper("cache --timeout=3600".into())
        );
        assert_eq!(
            Credential::parse("/usr/lib/git-core/git-credential-libsecret").unwrap(),
            Credential::Helper("/usr/lib/git-core/git-credential-libsecret".into())
        );
        for c in [
            Credential::Bot,
            Credential::Token("ann".into()),
            Credential::File(PathBuf::from("/t")),
            Credential::Helper("store".into()),
        ] {
            assert_eq!(Credential::parse(&c.to_config()).unwrap(), c);
        }
    }

    #[test]
    fn git_tables_are_validated_at_load() {
        let load = |text: &str| -> Result<Config> {
            let cfg: Config = toml::from_str(text)?;
            cfg.validate()?;
            Ok(cfg)
        };
        let ok = load(
            "[git]\nname = \"Ann\"\nemail = \"ann@example.com\"\nsigning_key = false\n\n\
             [[repo]]\nname = \"o/r\"\nharness = \"claude\"\n[repo.git]\ncredential = \"token:ann\"\n",
        )
        .unwrap();
        assert_eq!(ok.git.signing_key, Some(SigningKey::Off(false)));
        assert_eq!(ok.repos[0].git.credential.as_deref(), Some("token:ann"));
        // Name without email, at one level or across two.
        assert!(load("[git]\nname = \"Ann\"\n").is_err());
        let err =
            load("[[repo]]\nname = \"o/r\"\nharness = \"claude\"\n[repo.git]\nemail = \"a@b\"\n")
                .unwrap_err();
        assert!(
            format!("{err:#}").contains("git.email is set without git.name"),
            "{err:#}"
        );
        // Email at the instance and name on the repo is a whole identity.
        assert!(
            load(
                "[git]\nemail = \"a@b\"\n[[repo]]\nname = \"o/r\"\nharness = \"claude\"\n[repo.git]\nname = \"Ann\"\n"
            )
            .is_err(),
            "the instance table alone is still half an identity"
        );
        assert!(load("[git]\nsigning_key = true\n").is_err());
        assert!(load("[git]\ncredential = \"token:\"\n").is_err());
        assert!(load("[git]\nunknown = 1\n").is_err());
        // An empty table round-trips to nothing.
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(!text.contains("[git]"), "{text}");
    }
}
