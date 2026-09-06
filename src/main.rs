//! ssf — Simple Software Factory.
//!
//! Watches GitHub repos for issues assigned to a bot account and turns each one
//! into an Orca workspace running a coding agent, feeding later issue activity
//! into that agent.

mod agents;
mod allow;
mod config;
mod driver;
mod engine;
mod ghcli;
mod github;
mod herdr;
mod ipc;
mod keys;
mod login;
mod models;
mod orca;
mod origin;
mod prompt;
mod release;
mod sessions;
mod shim;
mod state;
mod status;
mod ui;
mod vm;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::json;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use tracing_subscriber::EnvFilter;

use config::{Config, RepoConfig, split_repo_name};

#[derive(Parser)]
#[command(
    name = "ssf",
    version,
    about = "Simple Software Factory: GitHub issues -> Orca agent workspaces"
)]
struct Cli {
    /// Log verbosity (also honours RUST_LOG).
    #[arg(long, global = true, default_value = "info", env = "SSF_LOG")]
    log: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Command {
    /// Manage the bot account credentials (meant for humans; see `ssf ui`).
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Print the bot's GitHub token (for agents: `GH_TOKEN="$(ssf token)" gh ...`).
    Token,
    /// Configure which repositories are watched and which agent works on them.
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
    /// List the model ids an agent takes (asking the installed agent when it can tell).
    Models {
        /// Agent id (see `ssf agents`).
        harness: String,
        #[arg(long)]
        json: bool,
    },
    /// List coding agents known to Omarchy and whether they are installed.
    Agents {
        #[arg(long)]
        json: bool,
        /// Only print installed agents.
        #[arg(long)]
        installed: bool,
    },
    /// Read or change daemon settings (dotted keys, e.g. daemon.poll_interval_secs).
    Config {
        #[command(subcommand)]
        command: Option<ConfigCommand>,
    },
    /// Run the daemon: poll GitHub and drive Orca.
    Run {
        /// Do a single pass and exit.
        #[arg(long)]
        once: bool,
    },
    /// Show tracked issues and their workspaces, joined with what Orca
    /// reports about each agent session.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// List the agent sessions on a repository: item, GitHub state, agent
    /// state, branch, last message. Inside a session the repository comes
    /// from SSF_REPO; otherwise every watched repository is listed.
    Peers {
        #[arg(long)]
        json: bool,
        /// Repository (owner/name) to list; defaults to $SSF_REPO, then all.
        #[arg(long, env = "SSF_REPO")]
        repo: Option<String>,
        /// Include retired sessions (closed or unassigned items).
        #[arg(long)]
        all: bool,
    },
    /// Follow an item without working on it: its activity arrives in this
    /// session as `[ssf] FYI` messages. Needs the running daemon.
    Sub {
        /// Item number on this session's repository, or owner/repo#N.
        item: String,
        /// Act as this session (owner/repo#N) instead of $SSF_REPO/$SSF_ISSUE.
        #[arg(long = "as", value_name = "SESSION")]
        r#as: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Stop following an item.
    Unsub {
        item: String,
        #[arg(long = "as", value_name = "SESSION")]
        r#as: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List what this session follows, and who follows its items.
    Subs {
        #[arg(long = "as", value_name = "SESSION")]
        r#as: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Paste a message into the terminal of the agent session on an item,
    /// through the daemon's delivery path (the session is brought back if
    /// its terminal is gone).
    Tell {
        item: String,
        /// The message (read from stdin when omitted).
        message: Option<String>,
        /// Send as this session (owner/repo#N) instead of $SSF_REPO/$SSF_ISSUE.
        #[arg(long = "as", value_name = "SESSION")]
        r#as: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Give a session's workspace back once everything is on origin: the
    /// daemon checks the tree is clean, the branch is on origin with nothing
    /// unpushed and no stash was made on it, and refuses otherwise. Inside a
    /// session it is this session's workspace; from a shell name the item.
    Release {
        /// Item number on this session's repository, or owner/repo#N.
        item: Option<String>,
        /// Act as this session (owner/repo#N) instead of $SSF_REPO/$SSF_ISSUE.
        #[arg(long = "as", value_name = "SESSION")]
        r#as: Option<String>,
        /// Remove it even if the checks fail; work in it is lost. Refused
        /// inside a session unless --as names the session.
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// Remove the workspaces of closed items whose agent is gone: each is
    /// listed with its state, the clean-and-pushed ones are removed, the
    /// rest are left in place. Workspaces of open items, of sessions that
    /// still own open items, and with a running agent are never touched.
    Purge {
        /// Only list; remove nothing.
        #[arg(long)]
        dry_run: bool,
        /// Only workspaces whose item retired more than this many days ago.
        #[arg(long, value_name = "DAYS")]
        older_than: Option<u64>,
        /// Remove the dirty and unpushed ones too (their work is lost).
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print the reference for agents: how sessions, other sessions,
    /// following items, hand-offs and reviewer sessions work. The initial
    /// prompt points here.
    Guide,
    /// Check that GitHub, Orca and the configured harnesses are usable.
    Doctor,
    /// Omarchy desktop integration: bar widget, menu entries, background service.
    Ui {
        #[command(subcommand)]
        command: UiCommand,
    },
    /// Run the whole factory (daemon, herdr, sessions) inside a Firecracker
    /// microVM instead of on this machine: build the image, start, stop
    /// and reach the guest.
    Vm {
        #[command(subcommand)]
        command: VmCommand,
    },
    /// Run a command (normally an agent) with the bot's GitHub credentials in
    /// its environment: GH_TOKEN, GITHUB_TOKEN, a git credential helper, and
    /// SSF_REPO / SSF_ISSUE / SSF_ISSUE_URL for the issue being worked.
    Launch {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        issue: Option<u64>,
        #[arg(long)]
        issue_url: Option<String>,
        /// `reviewer` for the reviewer session of a pull request: exported as
        /// SSF_ROLE, and the gh shim tags posts with `role=reviewer`.
        #[arg(long)]
        role: Option<String>,
        /// Command line to run (through `sh -c`).
        #[arg(trailing_var_arg = true, required = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Git credential helper backed by the bot token (installed by `ssf launch`).
    #[command(name = "git-credential", hide = true)]
    GitCredential {
        /// get | store | erase
        op: String,
    },
}

#[derive(Subcommand)]
enum VmCommand {
    /// Download Firecracker, gvproxy and a guest kernel, make the root
    /// image from the Arch bootstrap tarball and provision it (git, gh,
    /// herdr, the harness CLIs). No root needed.
    Build {
        /// Make a new image even if one exists.
        #[arg(long)]
        force: bool,
    },
    /// Boot the VM (making its disks on first use) and wait for its daemon.
    Start,
    /// Shut the VM down cleanly.
    Stop,
    /// Stop, then start (picks up a new ssf binary and `[vm] files`).
    Restart,
    /// Whether the VM runs and its daemon answers.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Sign a harness in inside the guest: its login runs there in this
    /// terminal (a URL to open here and a code to paste back, or a device
    /// code); the credential is written in the guest, nothing is copied from
    /// this machine. Without a harness, pick one from those installed.
    Login {
        /// `claude`, `codex`, `gemini`, `copilot`, `opencode`, `pi`, `omp`,
        /// `grok` or `crush`.
        harness: Option<String>,
    },
    /// Attach to herdr's session in the guest, in this terminal.
    Attach,
    /// A shell in the guest, or run a command there.
    Ssh {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Run an `ssf` command inside the guest (`ssf vm run -- status --json`).
    Run {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
    /// Push this machine's config and token into the running guest and
    /// restart its daemon.
    Sync,
    /// The guest daemon's journal.
    Logs {
        #[arg(long, short = 'f')]
        follow: bool,
        #[arg(long, short = 'n', default_value_t = 200)]
        lines: u32,
    },
    /// The guest's serial console log (kernel and systemd messages).
    Console {
        #[arg(long, short = 'f')]
        follow: bool,
    },
    /// An `~/.ssh/config` entry for the guest (`herdr --remote ssf-<name>`).
    SshConfig,
    /// Remake the root disk from the image at the next start; state,
    /// clones and worktrees on the data disk stay.
    Reset,
    /// Remove the VM and all its disks.
    Destroy {
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Sign in the bot account through the GitHub CLI (pick an account gh
    /// already knows, or sign in another one in the browser), record its git
    /// identity and enroll a dedicated SSH key for pushes and commit signing.
    Login {
        /// Use this account from gh's keyring without asking.
        #[arg(long, alias = "username")]
        user: Option<String>,
        /// Sign in another account in the browser (gh's device flow).
        #[arg(long)]
        web: bool,
        /// Use a pasted token instead of gh (read from stdin when the value is omitted).
        #[arg(long, num_args = 0..=1, default_missing_value = "-")]
        token: Option<String>,
        /// Do not generate/enroll an SSH key (HTTPS pushes via the token still work).
        #[arg(long)]
        no_keys: bool,
        /// Email to author commits with (default: the account's noreply address).
        #[arg(long)]
        email: Option<String>,
        /// Skip the confirmation prompt.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Show which GitHub account the stored token belongs to.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Remove the stored token and revoke the enrolled keys on GitHub.
    Logout {
        /// Keep the SSH key registered on the bot account and on disk.
        #[arg(long)]
        keep_keys: bool,
    },
}

#[derive(Subcommand)]
enum RepoCommand {
    /// Watch a repository (or replace its settings). Example: ssf repo add owner/name --harness claude
    Add {
        /// GitHub repository as owner/name.
        name: String,
        /// Agent id to run in each issue workspace (see `ssf agents`).
        #[arg(long)]
        harness: String,
        /// Where this repository's sessions run: orca or herdr (default: the top-level `driver`).
        #[arg(long)]
        driver: Option<String>,
        /// Existing local checkout to use instead of cloning.
        #[arg(long)]
        path: Option<String>,
        /// Clone URL (default https://github.com/owner/name.git).
        #[arg(long)]
        clone_url: Option<String>,
        /// Base ref for issue worktrees.
        #[arg(long)]
        base_branch: Option<String>,
        /// Command that starts the harness (default: its permission-free command, shown by `ssf agents --json`).
        #[arg(long)]
        command: Option<String>,
        /// Model the harness runs with, as an Orca model id (e.g. opus, sonnet, gpt-5.5); see `ssf agents --json`.
        #[arg(long)]
        model: Option<String>,
        /// Effort level for the model, as an Orca effort level (e.g. low, medium, high, xhigh, max).
        #[arg(long)]
        effort: Option<String>,
        /// Extra instructions appended to the initial prompt for this repo.
        #[arg(long)]
        instructions: Option<String>,
        /// File appended to the initial prompt, relative to the worktree unless absolute (default: SSF.md).
        #[arg(long, value_name = "PATH")]
        prompt_file: Option<String>,
        /// Logins that may drive this repository, comma-separated, replacing daemon.allowed_users
        /// (default: the collaborators with push access); `*` means anyone and needs --accept-anyone-risk.
        #[arg(long, value_name = "LOGINS")]
        allowed_users: Option<String>,
        /// Accept that `--allowed-users '*'` lets ANYONE on GitHub drive this repository.
        #[arg(long)]
        accept_anyone_risk: bool,
    },
    /// Change some settings of a watched repository, keeping the rest.
    Set {
        name: String,
        #[arg(long)]
        harness: Option<String>,
        /// orca or herdr.
        #[arg(long)]
        driver: Option<String>,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        clone_url: Option<String>,
        #[arg(long)]
        base_branch: Option<String>,
        /// Command that starts the harness (default: its permission-free command, shown by `ssf agents --json`).
        #[arg(long)]
        command: Option<String>,
        /// Model the harness runs with, as an Orca model id (e.g. opus, sonnet, gpt-5.5).
        #[arg(long)]
        model: Option<String>,
        /// Effort level for the model, as an Orca effort level (e.g. low, medium, high, xhigh, max).
        #[arg(long)]
        effort: Option<String>,
        #[arg(long)]
        instructions: Option<String>,
        /// File appended to the initial prompt, relative to the worktree unless absolute (default: SSF.md).
        #[arg(long, value_name = "PATH")]
        prompt_file: Option<String>,
        /// Logins that may drive this repository, comma-separated, replacing daemon.allowed_users;
        /// `*` means anyone and needs --accept-anyone-risk.
        #[arg(long, value_name = "LOGINS")]
        allowed_users: Option<String>,
        /// Accept that `--allowed-users '*'` lets ANYONE on GitHub drive this repository.
        #[arg(long)]
        accept_anyone_risk: bool,
        /// Clear an optional field: driver, path, clone_url, base_branch, command, model, effort, instructions, prompt_file, allowed_users.
        #[arg(long, value_name = "FIELD")]
        clear: Vec<String>,
    },
    /// Stop watching a repository.
    #[command(alias = "rm")]
    Remove { name: String },
    /// List watched repositories.
    #[command(alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Print the config file path.
    Path,
    /// Print the effective configuration (token redacted).
    Show {
        #[arg(long)]
        json: bool,
    },
    /// Read one setting, e.g. `ssf config get daemon.poll_interval_secs`.
    Get { key: String },
    /// Change one setting, e.g. `ssf config set daemon.poll_interval_secs 60`.
    Set {
        key: String,
        value: String,
        /// Accept that `daemon.allowed_users '["*"]'` lets ANYONE on GitHub drive the factory.
        #[arg(long)]
        accept_anyone_risk: bool,
    },
}

#[derive(Subcommand)]
enum UiCommand {
    /// Link the bar widget into ~/.config/omarchy/plugins, enable it, add menu entries.
    Install {
        #[arg(long)]
        quiet: bool,
    },
    /// Remove the bar widget and menu entries.
    Uninstall,
    /// Control the background service (`ssf.service` user unit).
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
}

#[derive(Subcommand)]
enum ServiceCommand {
    /// Allow the service to run at login and start it now.
    Enable,
    /// Stop the service and keep it from starting at login.
    Disable,
    /// Flip between enabled and disabled.
    Toggle,
    /// Exit 0 when the service is enabled (for menu `checked:` conditions).
    IsEnabled,
    /// Show service state.
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // `ssf launch` links `~/.config/ssf/bin/gh` (and `ssf`) to this binary;
    // invoked under the gh name we are the gh shim, not the daemon.
    if shim::invoked_as_gh() {
        shim::run();
    }
    let cli = Cli::parse();
    let filter = EnvFilter::try_from_env("RUST_LOG")
        .or_else(|_| EnvFilter::try_new(&cli.log))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();
    // With the factory in a VM, the commands that talk to the daemon run
    // inside the guest, where the daemon is. With the VM down, `status`
    // says so the way it says the service is stopped on bare metal (the
    // bar widget polls it); the others cannot do anything.
    if let Some(name) = forwarded_name(&cli.command)
        && !vm::in_guest()
        && let Ok(cfg) = Config::load()
        && cfg.vm.enabled
    {
        let vm = vm::Vm::new(&cfg);
        if !vm.running() {
            match cli.command {
                Command::Status { json: true } => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "vm": "stopped", "service_active": false,
                            "service_enabled": ui::service_enabled(),
                            "sessions": [], "repos": [],
                        })
                    );
                    return Ok(());
                }
                Command::Status { json: false } => {
                    println!(
                        "vm:      {} is not running (`ssf vm start`, or `ssf ui service enable`)",
                        cfg.vm.name
                    );
                    return Ok(());
                }
                _ => bail!(
                    "the factory runs in VM {}, which is not running; `ssf vm start` first",
                    cfg.vm.name
                ),
            }
        }
        let args: Vec<String> = std::env::args().skip(1).collect();
        let st = vm
            .exec_ssf(&args)
            .with_context(|| format!("running `ssf {name}` in the VM"))?;
        std::process::exit(st.code().unwrap_or(1));
    }

    match cli.command {
        Command::Auth { command } => auth(command).await,
        Command::Token => {
            let cfg = Config::load()?;
            println!("{}", cfg.github_token()?);
            Ok(())
        }
        Command::Repo { command } => repo(command),
        Command::Models { harness, json } => {
            let ids = models::available_models(&harness)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&ids)?);
            } else {
                for id in ids {
                    println!("{id}");
                }
            }
            Ok(())
        }
        Command::Agents { json, installed } => {
            let mut list = agents::list();
            if installed {
                list.retain(|a| a.installed);
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else {
                for a in list {
                    println!(
                        "{:<10} {:<16} {}{}",
                        a.id,
                        a.name,
                        if a.installed {
                            "installed"
                        } else {
                            "not installed"
                        },
                        if a.default { "  (omarchy default)" } else { "" }
                    );
                }
            }
            Ok(())
        }
        Command::Config { command } => {
            config_cmd(command.unwrap_or(ConfigCommand::Show { json: false }))
        }
        Command::Run { once } => run(once).await,
        Command::Status { json } => status(json).await,
        Command::Peers { json, repo, all } => peers(json, repo, all).await,
        Command::Sub { item, r#as, json } => sub(&item, r#as.as_deref(), json, true).await,
        Command::Unsub { item, r#as, json } => sub(&item, r#as.as_deref(), json, false).await,
        Command::Subs { r#as, json } => subs(r#as.as_deref(), json),
        Command::Tell {
            item,
            message,
            r#as,
            json,
        } => tell(&item, message, r#as.as_deref(), json).await,
        Command::Release {
            item,
            r#as,
            force,
            json,
        } => release(item.as_deref(), r#as.as_deref(), force, json).await,
        Command::Purge {
            dry_run,
            older_than,
            force,
            json,
        } => purge(dry_run, older_than, force, json).await,
        Command::Guide => {
            let bot = std::env::var("SSF_BOT")
                .ok()
                .filter(|b| !b.is_empty())
                .or_else(|| state::State::load().ok().and_then(|s| s.bot_login))
                .unwrap_or_else(|| "<bot>".into());
            let daemon = Config::load().map(|c| c.daemon).unwrap_or_default();
            print!(
                "{}",
                prompt::guide(&bot, daemon.review_label(), vm::in_guest())
            );
            Ok(())
        }
        Command::Doctor => doctor().await,
        Command::Vm { command } => vm_cmd(command).await,
        Command::Ui { command } => ui_cmd(command),
        Command::Launch {
            repo,
            issue,
            issue_url,
            command,
            role,
        } => launch(repo, issue, issue_url, role, command),
        Command::GitCredential { op } => git_credential(&op),
    }
}

fn launch(
    repo: Option<String>,
    issue: Option<u64>,
    issue_url: Option<String>,
    role: Option<String>,
    command: Vec<String>,
) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let cfg = Config::load().unwrap_or_default();
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(command.join(" "));
    // Git configuration is injected through GIT_CONFIG_* so it beats the
    // human's ~/.gitconfig (identity, signing key, credential helpers) inside
    // the agent's shell only.
    let mut git: Vec<(String, String)> = Vec::new();
    match cfg.github_token() {
        Ok(token) => {
            cmd.env("GH_TOKEN", &token).env("GITHUB_TOKEN", &token);
            // Our helper first and any configured ones dropped, so HTTPS pushes
            // go out as the bot rather than as whoever is logged into gh.
            let me = std::env::current_exe()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "ssf".into());
            git.push(("credential.helper".into(), String::new()));
            git.push(("credential.helper".into(), format!("!{me} git-credential")));
        }
        Err(e) => eprintln!("ssf launch: no bot credentials exported ({e:#})"),
    }
    // gh must only ever see the bot. Its own config dir would expose every
    // account in the human's keyring to `gh auth token --user ...`, so point
    // it at an ssf-owned one that lists none; GH_TOKEN carries the identity.
    let gh_dir = config::config_dir().join("gh");
    if std::fs::create_dir_all(&gh_dir).is_ok() {
        cmd.env("GH_CONFIG_DIR", &gh_dir);
    }
    if let Some(login) = cfg.github.login.as_deref() {
        let email = cfg
            .github
            .email
            .clone()
            .unwrap_or_else(|| format!("{login}@users.noreply.github.com"));
        for var in ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"] {
            cmd.env(var, login);
        }
        for var in ["GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"] {
            cmd.env(var, &email);
        }
        git.push(("user.name".into(), login.to_string()));
        git.push(("user.email".into(), email));
    }
    match cfg
        .github
        .ssh_key_path
        .as_deref()
        .filter(|p| std::path::Path::new(p).exists())
    {
        Some(key) => {
            cmd.env(
                "GIT_SSH_COMMAND",
                format!("ssh -i {} -o IdentitiesOnly=yes", shell_quote(key)),
            );
            if cfg.github.signing_key_id.is_some() {
                let pubkey = keys::public_path(std::path::Path::new(key));
                git.push(("gpg.format".into(), "ssh".into()));
                git.push((
                    "user.signingkey".into(),
                    pubkey.to_string_lossy().to_string(),
                ));
                git.push(("commit.gpgsign".into(), "true".into()));
                git.push(("tag.gpgsign".into(), "true".into()));
            } else {
                git.push(("commit.gpgsign".into(), "false".into()));
            }
        }
        None => {
            // No bot key: make sure commits are not signed with the human's key.
            git.push(("commit.gpgsign".into(), "false".into()));
            git.push(("tag.gpgsign".into(), "false".into()));
        }
    }
    let base: usize = std::env::var("GIT_CONFIG_COUNT")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    cmd.env("GIT_CONFIG_COUNT", (base + git.len()).to_string());
    for (i, (k, v)) in git.iter().enumerate() {
        cmd.env(format!("GIT_CONFIG_KEY_{}", base + i), k)
            .env(format!("GIT_CONFIG_VALUE_{}", base + i), v);
    }
    if let Ok(st) = state::State::load() {
        if let Some(login) = st.bot_login {
            cmd.env("SSF_BOT", login);
        }
    }
    if let Some(r) = repo {
        cmd.env("SSF_REPO", r);
    }
    if let Some(n) = issue {
        cmd.env("SSF_ISSUE", n.to_string());
    }
    if let Some(u) = issue_url {
        cmd.env("SSF_ISSUE_URL", u);
    }
    match role.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) if r == origin::REVIEWER => {
            cmd.env("SSF_ROLE", r);
        }
        Some(r) => bail!("--role {r}: the only role is `{}`", origin::REVIEWER),
        None => {
            cmd.env_remove("SSF_ROLE");
        }
    }
    // A `gh` shim first on PATH stamps everything the agent posts with the
    // origin tag for this issue (see src/shim.rs).
    match std::env::current_exe().and_then(std::fs::canonicalize) {
        Ok(me) => match shim::install(&me) {
            Ok(dir) => match shim::prepend_to_path(&dir, std::env::var_os("PATH").as_deref()) {
                Some(path) => {
                    cmd.env("PATH", path);
                }
                None => eprintln!(
                    "ssf launch: {} cannot go on PATH, posts will not carry origin tags",
                    dir.display()
                ),
            },
            Err(e) => eprintln!(
                "ssf launch: gh shim not installed, posts will not carry origin tags ({e:#})"
            ),
        },
        Err(e) => {
            eprintln!("ssf launch: gh shim not installed, posts will not carry origin tags ({e:#})")
        }
    }
    let err = cmd.exec();
    Err(anyhow::Error::from(err).context("exec failed"))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn git_credential(op: &str) -> Result<()> {
    if op != "get" {
        return Ok(());
    }
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let mut protocol = "";
    let mut host = "";
    for line in input.lines() {
        if let Some(v) = line.strip_prefix("protocol=") {
            protocol = v;
        } else if let Some(v) = line.strip_prefix("host=") {
            host = v;
        }
    }
    let cfg = Config::load().unwrap_or_default();
    let wanted = cfg.github.git_host();
    if protocol != "https" || !host.eq_ignore_ascii_case(&wanted) {
        return Ok(());
    }
    let Ok(token) = cfg.github_token() else {
        return Ok(());
    };
    println!("username=x-access-token\npassword={token}");
    Ok(())
}

async fn auth(command: AuthCommand) -> Result<()> {
    match command {
        AuthCommand::Login {
            user,
            web,
            token,
            no_keys,
            email,
            yes,
        } => {
            let mut cfg = Config::load()?;
            let host = cfg.github.git_host();
            let interactive = std::io::stdin().is_terminal();
            let mut previous_active: Option<String> = None;
            let (login, token, source) = if let Some(t) = token {
                let t = if t == "-" {
                    read_stdin_token(interactive)?
                } else {
                    t.trim().to_string()
                };
                if t.is_empty() {
                    bail!("no token provided");
                }
                let gh = github::GitHub::new(&cfg.github.api_url, &t)?;
                let me = gh.whoami().await.context("token rejected by GitHub")?;
                if let Some(u) = user.as_deref() {
                    if !me.login.eq_ignore_ascii_case(u.trim_start_matches('@')) {
                        bail!("that token belongs to @{}, not @{u}", me.login);
                    }
                }
                let path = config::save_token(&t)?;
                println!("Stored token for @{} in {}", me.login, path.display());
                (me.login, t, "token file")
            } else {
                if !ghcli::available() {
                    bail!(
                        "the GitHub CLI (gh) is not installed; install github-cli or pass --token"
                    );
                }
                let mut accounts = ghcli::accounts(&host)?;
                previous_active = accounts.iter().find(|a| a.active).map(|a| a.login.clone());
                let chosen = if web {
                    None
                } else if let Some(u) = user.as_deref() {
                    let u = u.trim_start_matches('@');
                    match accounts.iter().find(|a| a.login.eq_ignore_ascii_case(u)) {
                        Some(a) => Some(a.login.clone()),
                        None => bail!(
                            "gh does not know @{u}; run `ssf auth login --web` and sign in as @{u} in the browser"
                        ),
                    }
                } else if !interactive {
                    bail!(
                        "no terminal to pick an account in; pass --user <login> (an account gh knows) or --web"
                    );
                } else {
                    pick_account(&accounts)?
                };
                let chosen = match chosen {
                    Some(c) => c,
                    None => {
                        println!(
                            "Signing in another account in the browser. Use a private window so GitHub does not reuse your own session."
                        );
                        ghcli::login_web(&host, ghcli::REQUIRED_SCOPES)?;
                        accounts = ghcli::accounts(&host)?;
                        let now_active =
                            accounts.iter().find(|a| a.active).map(|a| a.login.clone());
                        match now_active {
                            Some(l) if previous_active.as_deref() != Some(l.as_str()) => l,
                            Some(l) => bail!(
                                "the browser sign-in did not add a new account (gh is still on @{l}); sign in as the bot in a private window"
                            ),
                            None => bail!("gh reports no active account after sign-in"),
                        }
                    }
                };
                // gh's own flows act on the active account, so the bot is active only briefly.
                let restore = |host: &str, chosen: &str| {
                    if let Some(prev) = previous_active.as_deref() {
                        if prev != chosen {
                            if let Err(e) = ghcli::switch_to(host, prev) {
                                eprintln!("warning: could not switch gh back to @{prev}: {e:#}");
                            }
                        }
                    }
                };
                if let Some(acc) = accounts.iter().find(|a| a.login == chosen) {
                    let needed: Vec<&str> = acc
                        .missing_scopes()
                        .into_iter()
                        .filter(|s| !no_keys || *s == "repo")
                        .collect();
                    if !needed.is_empty() {
                        if interactive {
                            println!(
                                "@{chosen}'s gh token lacks {}; asking gh to add them.",
                                needed.join(", ")
                            );
                            ghcli::switch_to(&host, &chosen)?;
                            let r = ghcli::refresh_scopes(&host, &needed);
                            restore(&host, &chosen);
                            r?;
                        } else {
                            eprintln!(
                                "warning: @{chosen}'s gh token lacks {}; key enrollment may fail",
                                needed.join(", ")
                            );
                        }
                    }
                }
                restore(&host, &chosen);
                let t = ghcli::token_for(&host, &chosen)?;
                let gh = github::GitHub::new(&cfg.github.api_url, &t)?;
                let me = gh
                    .whoami()
                    .await
                    .context("gh's token for the bot was rejected by GitHub")?;
                if !me.login.eq_ignore_ascii_case(&chosen) {
                    bail!("gh's token for @{chosen} belongs to @{}", me.login);
                }
                if interactive
                    && !yes
                    && user.is_none()
                    && !confirm(&format!("Use @{} as the bot account?", me.login))?
                {
                    bail!("cancelled");
                }
                // A pasted token from an earlier sign-in would shadow gh's.
                let _ = std::fs::remove_file(config::token_path());
                (me.login, t, "gh keyring")
            };

            let gh = github::GitHub::new(&cfg.github.api_url, &token)?;
            let me = gh.whoami().await?;
            let mut st = state::State::load().unwrap_or_default();
            st.bot_login = Some(login.clone());
            let _ = st.save();
            cfg.github.login = Some(login.clone());
            cfg.github.email = Some(
                email
                    .or_else(|| me.email.clone())
                    .unwrap_or_else(|| me.noreply_email()),
            );
            println!("Bot account: @{login} ({source})");
            println!(
                "Commits will be authored as {login} <{}>",
                cfg.github.email.as_deref().unwrap_or("")
            );
            if !no_keys {
                let host_name = hostname();
                let key_path = cfg
                    .github
                    .ssh_key_path
                    .clone()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| config::default_key_path(&login));
                let pair = keys::ensure(&key_path, &format!("ssf:{login}@{host_name}"))?;
                let title = format!("ssf on {host_name}");
                match gh.add_key("keys", &title, &pair.public_key).await {
                    Ok(id) => {
                        cfg.github.ssh_key_id = Some(id);
                        println!("Enrolled SSH key for pushes ({})", pair.public.display());
                    }
                    Err(e) => eprintln!("warning: could not enroll the SSH key for pushes: {e:#}"),
                }
                match gh
                    .add_key("ssh_signing_keys", &title, &pair.public_key)
                    .await
                {
                    Ok(id) => {
                        cfg.github.signing_key_id = Some(id);
                        println!("Enrolled the same key for commit signing");
                    }
                    Err(e) => eprintln!("warning: could not enroll the signing key: {e:#}"),
                }
                cfg.github.ssh_key_path = Some(pair.private.to_string_lossy().to_string());
            }
            cfg.save()?;
            if let Some(prev) = previous_active.as_deref() {
                if prev != login {
                    println!(
                        "gh stays on @{prev}; ssf reads @{login}'s token from gh when it needs it."
                    );
                }
            }
            if !cfg.repos.is_empty() {
                println!("Issues assigned to @{login} in the watched repos will now be picked up.");
            }
            Ok(())
        }
        AuthCommand::Status { json } => {
            let cfg = Config::load()?;
            let token = cfg.github_token()?;
            let gh = github::GitHub::new(&cfg.github.api_url, &token)?;
            let me = gh.whoami().await?;
            let key_ok = cfg
                .github
                .ssh_key_path
                .as_deref()
                .is_some_and(|p| std::path::Path::new(p).exists());
            if json {
                println!(
                    "{}",
                    json!({
                        "login": me.login, "type": me.kind, "id": me.id,
                        "email": cfg.github.email,
                        "ssh_key": cfg.github.ssh_key_path, "ssh_key_present": key_ok,
                        "ssh_key_id": cfg.github.ssh_key_id, "signing_key_id": cfg.github.signing_key_id
                    })
                );
                return Ok(());
            }
            println!(
                "Bot account: @{} ({}, id {}), token from {}",
                me.login,
                me.kind,
                me.id,
                cfg.token_source()
            );
            if let Some(l) = &cfg.github.login {
                if !l.eq_ignore_ascii_case(&me.login) {
                    println!(
                        "warning: config says @{l} but the token belongs to @{}; run `ssf auth login --user {l}`",
                        me.login
                    );
                }
            }
            println!(
                "Commit identity: {} <{}>",
                me.login,
                cfg.github
                    .email
                    .as_deref()
                    .unwrap_or("(not set; run `ssf auth login`)")
            );
            let id_or = |v: Option<u64>| {
                v.map(|i| i.to_string())
                    .unwrap_or_else(|| "not enrolled".into())
            };
            match (&cfg.github.ssh_key_path, key_ok) {
                (Some(p), true) => println!(
                    "SSH key: {p} (auth key id {}, signing key id {})",
                    id_or(cfg.github.ssh_key_id),
                    id_or(cfg.github.signing_key_id)
                ),
                (Some(p), false) => println!("SSH key: {p} is missing; run `ssf auth login` again"),
                (None, _) => println!(
                    "SSH key: none (pushes use the token over HTTPS; commits are unsigned)"
                ),
            }
            Ok(())
        }
        AuthCommand::Logout { keep_keys } => {
            let mut cfg = Config::load()?;
            if !keep_keys {
                if let Ok(token) = cfg.github_token() {
                    if let Ok(gh) = github::GitHub::new(&cfg.github.api_url, &token) {
                        for (kind, id) in [
                            ("keys", cfg.github.ssh_key_id),
                            ("ssh_signing_keys", cfg.github.signing_key_id),
                        ] {
                            if let Some(id) = id {
                                match gh.delete_key(kind, id).await {
                                    Ok(()) => println!("Revoked {kind} entry {id} on GitHub"),
                                    Err(e) => eprintln!(
                                        "warning: could not revoke {kind} entry {id}: {e:#}"
                                    ),
                                }
                            }
                        }
                    }
                }
                if let Some(p) = cfg.github.ssh_key_path.take() {
                    keys::remove(std::path::Path::new(&p));
                    println!("Removed {p}");
                }
                cfg.github.ssh_key_id = None;
                cfg.github.signing_key_id = None;
            }
            let path = config::token_path();
            match std::fs::remove_file(&path) {
                Ok(()) => println!("Removed {}", path.display()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
            }
            if let Some(l) = &cfg.github.login {
                println!(
                    "Forgot @{l} as the bot. Its gh sign-in is untouched; remove it with `gh auth logout --user {l}` if you want."
                );
            }
            cfg.github.login = None;
            cfg.github.email = None;
            cfg.save()?;
            let mut st = state::State::load().unwrap_or_default();
            st.bot_login = None;
            let _ = st.save();
            Ok(())
        }
    }
}

fn read_stdin_token(interactive: bool) -> Result<String> {
    if interactive {
        eprint!("Paste the bot account's GitHub token: ");
        std::io::stderr().flush()?;
    }
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("reading token from stdin")?;
    Ok(buf.trim().to_string())
}

fn confirm(question: &str) -> Result<bool> {
    eprint!("{question} [Y/n] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let a = line.trim().to_lowercase();
    Ok(a.is_empty() || a == "y" || a == "yes")
}

/// `claude logged in, codex not logged in (omp not installed)`.
fn login_summary(states: &[vm::LoginState]) -> String {
    let mut parts: Vec<String> = states
        .iter()
        .filter(|s| s.installed)
        .map(|s| {
            format!(
                "{} {}",
                s.harness,
                if s.logged_in {
                    "logged in"
                } else {
                    "not logged in"
                }
            )
        })
        .collect();
    let missing: Vec<&str> = states
        .iter()
        .filter(|s| !s.installed)
        .map(|s| s.harness.as_str())
        .collect();
    if !missing.is_empty() {
        parts.push(format!("({} not installed)", missing.join(", ")));
    }
    parts.join(", ")
}

/// Terminal picker over the harnesses installed in the guest; `None` when
/// the person picks nothing.
fn pick_login(states: &[vm::LoginState]) -> Result<Option<&'static vm::Login>> {
    let installed: Vec<&vm::LoginState> = states.iter().filter(|s| s.installed).collect();
    if installed.is_empty() {
        bail!("no harness CLI is installed in the guest (`ssf vm build --force` for a new image)");
    }
    println!("Which harness to sign in inside the VM?");
    for (i, s) in installed.iter().enumerate() {
        println!(
            "  {}) {}{}",
            i + 1,
            s.harness,
            if s.logged_in { "  (logged in)" } else { "" }
        );
    }
    println!("  q) nothing");
    loop {
        eprint!("> ");
        std::io::stderr().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let a = line.trim();
        if a.is_empty() || a.eq_ignore_ascii_case("q") {
            return Ok(None);
        }
        let chosen = match a.parse::<usize>() {
            Ok(n) if (1..=installed.len()).contains(&n) => Some(installed[n - 1].harness.as_str()),
            _ => installed
                .iter()
                .map(|s| s.harness.as_str())
                .find(|h| h.eq_ignore_ascii_case(a)),
        };
        if let Some(l) = chosen.and_then(vm::login) {
            return Ok(Some(l));
        }
        eprintln!("a number from the list, a harness name, or q");
    }
}

/// Terminal picker over gh's accounts; `None` means "sign in another one".
fn pick_account(accounts: &[ghcli::Account]) -> Result<Option<String>> {
    if accounts.is_empty() {
        println!("gh has no accounts on this host yet.");
        return Ok(None);
    }
    println!("Which GitHub account is the bot?");
    for (i, a) in accounts.iter().enumerate() {
        println!(
            "  {}) @{}{}",
            i + 1,
            a.login,
            if a.active {
                "  (your active gh account)"
            } else {
                ""
            }
        );
    }
    println!("  w) sign in another account in the browser");
    loop {
        eprint!("> ");
        std::io::stderr().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let a = line.trim();
        if a.eq_ignore_ascii_case("w") {
            return Ok(None);
        }
        if let Ok(n) = a.parse::<usize>() {
            if (1..=accounts.len()).contains(&n) {
                return Ok(Some(accounts[n - 1].login.clone()));
            }
        }
        if let Some(acc) = accounts
            .iter()
            .find(|x| x.login.eq_ignore_ascii_case(a.trim_start_matches('@')))
        {
            return Ok(Some(acc.login.clone()));
        }
        println!("Enter a number, an account name, or w.");
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "omarchy".to_string())
}

/// herdr starts and reads only the agents it can recognise in a pane.
fn check_herdr_harness(harness: &str) {
    let known = std::process::Command::new(
        std::env::var("HERDR_COMMAND").unwrap_or_else(|_| "herdr".into()),
    )
    .args(["agent", "start", "--help"])
    .output()
    .ok()
    .map(|o| String::from_utf8_lossy(&o.stdout).to_string());
    if let Some(text) = known
        && text.contains("possible values")
        && !text
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|w| w == harness)
    {
        eprintln!(
            "warning: herdr does not list `{harness}` among the agents it detects (see `herdr agent start --help`); sessions would wait for it and give up"
        );
    }
}

fn check_harness(harness: &str) {
    if !agents::is_known(harness) {
        eprintln!(
            "note: `{harness}` is not one of the agents Omarchy knows about (see `ssf agents`); the driver must know how to launch it"
        );
    } else if !agents::list()
        .iter()
        .any(|a| a.id == harness && a.installed)
    {
        eprintln!("warning: `{harness}` does not appear to be installed (see `ssf agents`)");
    }
}

fn repo(command: RepoCommand) -> Result<()> {
    let mut cfg = Config::load()?;
    match command {
        RepoCommand::Add {
            name,
            harness,
            driver,
            path,
            clone_url,
            base_branch,
            command,
            model,
            effort,
            instructions,
            prompt_file,
            allowed_users,
            accept_anyone_risk,
        } => {
            let (owner, r) = split_repo_name(&name)?;
            let name = format!("{owner}/{r}");
            let path = expand_checkout(path)?;
            check_harness(&harness);
            let driver = driver.map(|d| d.parse()).transpose()?;
            if driver == Some(config::DriverKind::Herdr) {
                check_herdr_harness(&harness);
            }
            let mut entry = RepoConfig {
                name: name.clone(),
                harness,
                driver,
                command,
                model: model.map(|m| m.trim().to_string()),
                effort: effort.map(|e| e.trim().to_string()),
                clone_url,
                path,
                base_branch,
                instructions,
                prompt_file,
                allowed_users: None,
                accepted_anyone_risk: false,
            };
            entry.validate_launch_prefs()?;
            if let Some(list) = allowed_users {
                set_repo_allowed_users(&mut entry, &list, accept_anyone_risk)?;
            }
            let action = if let Some(pos) = cfg
                .repos
                .iter()
                .position(|x| x.name.eq_ignore_ascii_case(&name))
            {
                cfg.repos[pos] = entry;
                "Updated"
            } else {
                cfg.repos.push(entry);
                "Added"
            };
            cfg.save()?;
            println!("{action} {name} in {}", config::config_path().display());
            Ok(())
        }
        RepoCommand::Set {
            name,
            harness,
            driver,
            path,
            clone_url,
            base_branch,
            command,
            model,
            effort,
            instructions,
            prompt_file,
            allowed_users,
            accept_anyone_risk,
            clear,
        } => {
            let pos = cfg
                .repos
                .iter()
                .position(|x| x.name.eq_ignore_ascii_case(&name))
                .with_context(|| format!("{name} is not configured; use `ssf repo add`"))?;
            let entry = &mut cfg.repos[pos];
            if let Some(h) = harness {
                check_harness(&h);
                if h != entry.harness {
                    // Model ids and effort levels belong to a harness; a new
                    // harness starts from its defaults unless told otherwise.
                    if (entry.model.is_some() && model.is_none())
                        || (entry.effort.is_some() && effort.is_none())
                    {
                        eprintln!(
                            "note: model/effort reset to the defaults of {h}; set them again with --model/--effort"
                        );
                    }
                    entry.model = None;
                    entry.effort = None;
                }
                entry.harness = h;
            }
            if let Some(d) = driver {
                entry.driver = Some(d.parse()?);
            }
            if entry.driver == Some(config::DriverKind::Herdr) {
                check_herdr_harness(&entry.harness);
            }
            if let Some(p) = expand_checkout(path)? {
                entry.path = Some(p);
            }
            if clone_url.is_some() {
                entry.clone_url = clone_url;
            }
            if base_branch.is_some() {
                entry.base_branch = base_branch;
            }
            if command.is_some() {
                entry.command = command;
            }
            if let Some(m) = model {
                entry.model = Some(m.trim().to_string());
            }
            if let Some(e) = effort {
                entry.effort = Some(e.trim().to_string());
            }
            if instructions.is_some() {
                entry.instructions = instructions;
            }
            if prompt_file.is_some() {
                entry.prompt_file = prompt_file;
            }
            if let Some(list) = allowed_users {
                set_repo_allowed_users(entry, &list, accept_anyone_risk)?;
            }
            for field in clear {
                match field.as_str() {
                    "driver" => entry.driver = None,
                    "path" => entry.path = None,
                    "clone_url" => entry.clone_url = None,
                    "base_branch" => entry.base_branch = None,
                    "command" => entry.command = None,
                    "model" => entry.model = None,
                    "effort" => entry.effort = None,
                    "instructions" => entry.instructions = None,
                    "prompt_file" => entry.prompt_file = None,
                    "allowed_users" => {
                        entry.allowed_users = None;
                        entry.accepted_anyone_risk = false;
                    }
                    other => bail!("cannot clear unknown field {other}"),
                }
            }
            entry.validate_launch_prefs()?;
            let updated = entry.name.clone();
            cfg.save()?;
            println!("Updated {updated}");
            Ok(())
        }
        RepoCommand::Remove { name } => {
            let before = cfg.repos.len();
            cfg.repos.retain(|x| !x.name.eq_ignore_ascii_case(&name));
            if cfg.repos.len() == before {
                bail!("{name} is not configured");
            }
            cfg.save()?;
            println!("Removed {name}");
            Ok(())
        }
        RepoCommand::List { json } => {
            if json {
                println!("{}", serde_json::to_string_pretty(&cfg.repos)?);
                return Ok(());
            }
            if cfg.repos.is_empty() {
                println!(
                    "No repositories configured. Add one with: ssf repo add owner/name --harness claude"
                );
            }
            for r in &cfg.repos {
                let mut extra = Vec::new();
                if let Some(m) = &r.model {
                    extra.push(format!("model={m}"));
                }
                if let Some(e) = &r.effort {
                    extra.push(format!("effort={e}"));
                }
                if let Some(p) = &r.path {
                    extra.push(format!("path={p}"));
                }
                if let Some(u) = &r.clone_url {
                    extra.push(format!("clone_url={u}"));
                }
                if let Some(b) = &r.base_branch {
                    extra.push(format!("base={b}"));
                }
                if let Some(a) = &r.allowed_users {
                    extra.push(format!("allowed_users={}", a.join(",")));
                }
                println!(
                    "{:<40} harness={}{}",
                    r.name,
                    r.harness,
                    if extra.is_empty() {
                        String::new()
                    } else {
                        format!("  {}", extra.join(" "))
                    }
                );
            }
            Ok(())
        }
    }
}

fn expand_checkout(path: Option<String>) -> Result<Option<String>> {
    let Some(p) = path else { return Ok(None) };
    let p = config::expand_tilde(&p);
    if !p.join(".git").exists() {
        bail!("{} is not a git checkout", p.display());
    }
    Ok(Some(p.to_string_lossy().to_string()))
}

/// Parse a comma-separated `--allowed-users` value: logins, or `*`.
fn parse_allowed_users(list: &str) -> Vec<String> {
    list.split([',', ' '])
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| l.trim_start_matches('@').to_string())
        .collect()
}

/// Set a repository's allow-list, with the wildcard only after consent.
fn set_repo_allowed_users(entry: &mut RepoConfig, list: &str, accepted: bool) -> Result<()> {
    let logins = parse_allowed_users(list);
    if allow::is_wildcard(&logins) {
        confirm_anyone_risk(accepted, &format!("repository {}", entry.name))?;
        entry.accepted_anyone_risk = true;
    } else {
        entry.accepted_anyone_risk = false;
    }
    entry.allowed_users = Some(logins);
    Ok(())
}

/// The one affordance for opening the factory to everyone: an explicit
/// flag, or a yes typed at a terminal after the risk is spelled out. Anything
/// else (a script, a pipe) is refused with the flag named.
fn confirm_anyone_risk(accepted: bool, what: &str) -> Result<()> {
    anyone_risk_decision(accepted, std::io::stdin().is_terminal(), what, || {
        eprint!("Type yes to accept that risk: ");
        std::io::stderr().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        Ok(line.trim().eq_ignore_ascii_case("yes"))
    })
}

/// The decision behind `confirm_anyone_risk`, with the terminal factored
/// out: `ask` is only consulted at an interactive terminal, after the risk
/// has been printed.
fn anyone_risk_decision(
    accepted: bool,
    interactive: bool,
    what: &str,
    ask: impl FnOnce() -> Result<bool>,
) -> Result<()> {
    if accepted {
        return Ok(());
    }
    let risk = format!(
        "allowed_users \"*\" lets ANYONE with a GitHub account drive {what}: \
         every assignment, mention, review request, label and comment reaches an unattended \
         agent running with the bot's credentials, so anyone on the internet can make it act \
         and can put text in front of it."
    );
    if !interactive {
        bail!("{risk}\nRefusing without --accept-anyone-risk.");
    }
    eprintln!("{risk}");
    if ask()? {
        Ok(())
    } else {
        bail!("not accepted; nothing changed")
    }
}

fn config_cmd(command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Path => {
            println!("{}", config::config_path().display());
            Ok(())
        }
        ConfigCommand::Show { json } => {
            let mut cfg = Config::load()?;
            if cfg.github.token.is_some() {
                cfg.github.token = Some("<redacted>".into());
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&cfg)?);
            } else {
                println!("# {}", config::config_path().display());
                print!("{}", toml::to_string_pretty(&cfg)?);
                if let Some(note) = cfg.driver_note() {
                    println!();
                    println!("# driver in effect: {}", cfg.default_driver());
                    println!("#   {note}");
                }
                // Who may drive each repository, resolved from the file
                // alone (the collaborator default is fetched by the daemon;
                // `ssf doctor` shows it).
                if !cfg.repos.is_empty() {
                    println!();
                    println!("# who can drive ssf (allowed_users):");
                    for r in &cfg.repos {
                        println!("#   {}: {}", r.name, cfg.access_summary(r));
                    }
                }
            }
            Ok(())
        }
        ConfigCommand::Get { key } => {
            let cfg = Config::load()?;
            if key == "driver" && cfg.driver.is_none() {
                println!("{}", cfg.default_driver());
                return Ok(());
            }
            let value: toml::Value = toml::Value::try_from(&cfg)?;
            let mut cur = &value;
            for part in key.split('.') {
                cur = cur
                    .get(part)
                    .with_context(|| format!("unknown setting {key}"))?;
            }
            if key.starts_with("github.token") {
                bail!("the token is not readable through `config get`; use `ssf token`");
            }
            match cur {
                toml::Value::String(s) => println!("{s}"),
                other => println!("{other}"),
            }
            Ok(())
        }
        ConfigCommand::Set {
            key,
            value,
            accept_anyone_risk,
        } => config_set_at(&config::config_path(), &key, &value, accept_anyone_risk),
    }
}

/// `ssf config set`: one key in the file at `path`, validated before it is
/// written. `daemon.allowed_users` is special: a wildcard is written only
/// with consent (`accepted`, or a yes at the terminal) and gets its marker;
/// any other list drops the marker.
fn config_set_at(path: &Path, key: &str, value: &str, accepted: bool) -> Result<()> {
    if key == "github.token" || key == "github" {
        bail!("credentials are managed with `ssf auth login`, not `config set`");
    }
    if key.starts_with("repo") {
        bail!("repositories are managed with `ssf repo add|set|remove`");
    }
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    let mut table: toml::Table = toml::from_str(&raw).context("parsing config")?;
    let parts: Vec<&str> = key.split('.').collect();
    if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
        bail!("invalid key {key}");
    }
    let parsed = parse_toml_scalar(value);
    let mut cur: &mut toml::Table = &mut table;
    for part in &parts[..parts.len() - 1] {
        let next = cur
            .entry(part.to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        cur = next
            .as_table_mut()
            .with_context(|| format!("{part} is not a table"))?;
    }
    if key == "daemon.allowed_users" {
        let logins: Vec<String> = parsed
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .or_else(|| parsed.as_str().map(parse_allowed_users))
            .with_context(|| {
                format!("{key} takes a list of logins, e.g. '[\"alice\", \"bob\"]'")
            })?;
        let marker = if allow::is_wildcard(&logins) {
            confirm_anyone_risk(accepted, "every repository this daemon watches")?;
            true
        } else {
            false
        };
        // The marker stands only next to a wildcard, so a later
        // hand edit that adds one is refused again.
        if marker {
            cur.insert(allow::RISK_KEY.to_string(), toml::Value::Boolean(true));
        } else {
            cur.remove(allow::RISK_KEY);
        }
        cur.insert(
            parts[parts.len() - 1].to_string(),
            toml::Value::Array(logins.into_iter().map(toml::Value::String).collect()),
        );
    } else {
        cur.insert(parts[parts.len() - 1].to_string(), parsed);
    }
    let text = toml::to_string_pretty(&table)?;
    // Validate before writing so a typo cannot break the daemon.
    let checked: Config =
        toml::from_str(&text).with_context(|| format!("{key} is not a valid setting"))?;
    checked.validate()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    config::write_atomic(path, text.as_bytes(), 0o600)?;
    println!("{key} = {value}");
    Ok(())
}

fn parse_toml_scalar(value: &str) -> toml::Value {
    if let Ok(b) = value.parse::<bool>() {
        return toml::Value::Boolean(b);
    }
    if let Ok(i) = value.parse::<i64>() {
        return toml::Value::Integer(i);
    }
    if value.starts_with('[') {
        if let Ok(v) = toml::from_str::<toml::Table>(&format!("v = {value}")) {
            if let Some(x) = v.get("v") {
                return x.clone();
            }
        }
    }
    toml::Value::String(value.to_string())
}

/// The name of a command that runs in the guest when the factory is in a VM.
fn forwarded_name(cmd: &Command) -> Option<&'static str> {
    let name = match cmd {
        Command::Status { .. } => "status",
        Command::Peers { .. } => "peers",
        Command::Sub { .. } => "sub",
        Command::Unsub { .. } => "unsub",
        Command::Subs { .. } => "subs",
        Command::Tell { .. } => "tell",
        Command::Release { .. } => "release",
        Command::Purge { .. } => "purge",
        Command::Doctor => "doctor",
        Command::Run { once: true } => "run",
        _ => return None,
    };
    vm::forwards(name).then_some(name)
}

async fn vm_cmd(command: VmCommand) -> Result<()> {
    let cfg = Config::load()?;
    let vm = vm::Vm::new(&cfg);
    match command {
        VmCommand::Build { force } => vm.build(force).await,
        VmCommand::Start => {
            let orca = vm::orca_repos(&cfg);
            if !orca.is_empty() {
                eprintln!(
                    "note: {} run in herdr inside the VM (Orca needs a desktop)",
                    orca.join(", ")
                );
            }
            vm.start(&cfg).await
        }
        VmCommand::Stop => vm.stop().await,
        VmCommand::Restart => {
            vm.stop().await?;
            vm.start(&cfg).await
        }
        VmCommand::Status { json } => {
            let st = vm.status().await;
            if json {
                println!("{}", serde_json::to_string_pretty(&st)?);
            } else {
                println!(
                    "vm:       {} ({}){}",
                    st.name,
                    st.dir,
                    if st.enabled {
                        ""
                    } else {
                        "  [vm] enabled = false"
                    }
                );
                println!(
                    "image:    {}",
                    if st.image {
                        "built"
                    } else {
                        "missing (ssf vm build)"
                    }
                );
                println!(
                    "state:    {}",
                    match (st.running, st.firecracker_pid) {
                        (true, Some(p)) => format!("running (firecracker pid {p})"),
                        _ => "stopped".to_string(),
                    }
                );
                println!(
                    "ssh:      {}",
                    if st.ssh {
                        format!("127.0.0.1:{} answers", st.ssh_port)
                    } else {
                        "not reachable".to_string()
                    }
                );
                println!("daemon:   {}", st.daemon.as_deref().unwrap_or("unknown"));
                if !st.logins.is_empty() {
                    println!("logins:   {}", login_summary(&st.logins));
                }
            }
            Ok(())
        }
        VmCommand::Login { harness } => {
            let login = match harness {
                Some(h) => vm::login(&h).with_context(|| {
                    format!(
                        "no login flow for `{h}`; one of {}",
                        vm::LOGINS
                            .iter()
                            .map(|l| l.harness)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?,
                None => {
                    let states = vm
                        .logins()
                        .context("asking the guest (is the VM up? `ssf vm status`)")?;
                    match pick_login(&states)? {
                        Some(l) => l,
                        None => return Ok(()),
                    }
                }
            };
            if vm.login(login)? {
                println!("{}: logged in inside the VM", login.harness);
                Ok(())
            } else {
                bail!(
                    "{}: no credential at ~/{} in the guest; see the output above",
                    login.harness,
                    login.credential
                )
            }
        }
        VmCommand::Attach => exit_with(vm.attach()?),
        VmCommand::Ssh { command } => exit_with(vm.shell(&command)?),
        VmCommand::Run { args } => exit_with(vm.exec_ssf(&args)?),
        VmCommand::Sync => vm.sync(&cfg),
        VmCommand::Logs { follow, lines } => exit_with(vm.logs(follow, lines)?),
        VmCommand::Console { follow } => {
            let log = vm.console_log();
            let mut cmd = std::process::Command::new("tail");
            cmd.arg("-n").arg("200");
            if follow {
                cmd.arg("-f");
            }
            exit_with(cmd.arg(&log).status()?)
        }
        VmCommand::SshConfig => {
            print!("{}", vm.ssh_config());
            Ok(())
        }
        VmCommand::Reset => vm.reset().await,
        VmCommand::Destroy { yes } => {
            if !yes {
                bail!(
                    "this removes {} and everything in it; pass --yes",
                    vm.dir.display()
                );
            }
            vm.destroy().await
        }
    }
}

fn exit_with(st: std::process::ExitStatus) -> Result<()> {
    std::process::exit(st.code().unwrap_or(1));
}

async fn run(once: bool) -> Result<()> {
    let cfg = Config::load()?;
    if cfg.vm.enabled {
        // The factory lives in the VM: start it and stay with it.
        return vm::Vm::new(&cfg).supervise(&cfg).await;
    }
    if cfg.repos.is_empty() && once {
        bail!("no repositories configured; run `ssf repo add owner/name --harness claude` first");
    }
    // An install from before herdr became the default, still without a
    // `driver` line, changes driver on this upgrade: say so once, where
    // Orca is around to have been the one in use.
    if let Some(note) = cfg.driver_note()
        && std::path::Path::new(&cfg.orca.command).exists()
    {
        tracing::warn!("{note}");
    }
    let engine = engine::Engine::new(cfg).await?;
    if once {
        let mut engine = engine;
        engine.tick().await;
        return Ok(());
    }
    engine.run_forever().await
}

async fn status(json: bool) -> Result<()> {
    let snap = status::Snapshot::collect(Config::load()?).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&snap.to_json())?);
    } else {
        print!("{}", status::render_status(&snap));
    }
    Ok(())
}

async fn peers(json: bool, repo: Option<String>, all: bool) -> Result<()> {
    let snap = status::Snapshot::collect(Config::load()?).await?;
    let repo = repo.filter(|r| !r.is_empty());
    if let Some(r) = &repo {
        if !snap
            .cfg
            .repos
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(r))
        {
            bail!("{r} is not a watched repository (see `ssf repo list`)");
        }
    }
    let me = identity(None)?.map(|(o, reviewer)| o.session(reviewer));
    let sessions: Vec<status::Session> = snap
        .sessions()
        .into_iter()
        .filter(|s| repo.as_ref().is_none_or(|r| s.repo.eq_ignore_ascii_case(r)))
        .filter(|s| all || s.active)
        .collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "me": me,
                "orca_available": snap.available(),
                "orca_error": snap.error(),
                "sessions": sessions,
            }))?
        );
        return Ok(());
    }
    if let Some(e) = snap.error() {
        eprintln!("driver unavailable, its agent states unknown: {e}");
    }
    if sessions.is_empty() {
        println!(
            "no {}sessions{}",
            if all { "" } else { "active " },
            repo.map(|r| format!(" on {r}")).unwrap_or_default()
        );
        return Ok(());
    }
    print!("{}", status::render_peers(&sessions, me.as_deref()));
    Ok(())
}

/// This session's identity: `--as owner/repo#N` (or `owner/repo#N:reviewer`),
/// else the environment `ssf launch` set up (`SSF_ROLE=reviewer` makes it
/// the reviewer session of the item).
fn identity(as_: Option<&str>) -> Result<Option<(origin::Origin, bool)>> {
    match as_ {
        Some(a) => origin::parse_session(a)
            .map(Some)
            .with_context(|| format!("--as {a}: expected owner/repo#N")),
        None => Ok(origin::Origin::from_env().map(|o| (o, origin::Origin::reviewer_from_env()))),
    }
}

/// An item or session argument: `owner/repo#N`, or a bare number on `me`'s
/// repository; either with a `:reviewer` suffix for a reviewer session.
fn item_ref(item: &str, me: Option<&origin::Origin>) -> Result<String> {
    let item = item.trim().trim_start_matches('#');
    let (bare, suffix) = match item.strip_suffix(&format!(":{}", origin::REVIEWER)) {
        Some(b) => (b, format!(":{}", origin::REVIEWER)),
        None => (item, String::new()),
    };
    if let Ok(n) = bare.parse::<u64>() {
        return match me {
            Some(o) => Ok(format!("{}#{n}{suffix}", o.repo)),
            None => {
                bail!("{item}: pass owner/repo#{item}, or --as owner/repo#N to name the repository")
            }
        };
    }
    match origin::Origin::parse(bare) {
        Some(o) => Ok(format!("{o}{suffix}")),
        None => bail!("{item}: expected an item number or owner/repo#N"),
    }
}

async fn sub(item: &str, as_: Option<&str>, json: bool, subscribe: bool) -> Result<()> {
    let (me, reviewer) = identity(as_)?.context(
        "not inside an agent session (SSF_REPO/SSF_ISSUE unset); pass --as owner/repo#N",
    )?;
    let target = item_ref(item, Some(&me))?;
    let me = me.session(reviewer);
    let req = if subscribe {
        ipc::Request::Sub {
            from: me.clone(),
            target: target.clone(),
        }
    } else {
        ipc::Request::Unsub {
            from: me.clone(),
            target: target.clone(),
        }
    };
    let v = ipc::call(&req).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let title = v.get("title").and_then(|t| t.as_str()).unwrap_or("");
    let who = v.get("subscriber").and_then(|t| t.as_str()).unwrap_or("");
    if subscribe {
        let owner = match v.get("owner").and_then(|o| o.as_str()) {
            Some(o) => format!("owned by {o}"),
            None => "no session of its own; polled for you".into(),
        };
        let added = v.get("added").and_then(|a| a.as_bool()).unwrap_or(true);
        println!(
            "{who} {} {target} \"{title}\" ({owner}); new activity on it will arrive as [ssf] FYI messages.",
            if added {
                "subscribed to"
            } else {
                "was already subscribed to"
            }
        );
    } else {
        let removed = v.get("removed").and_then(|a| a.as_bool()).unwrap_or(true);
        println!(
            "{who} {} {target} \"{title}\"{}",
            if removed {
                "unsubscribed from"
            } else {
                "was not subscribed to"
            },
            if v.get("untracked")
                .and_then(|a| a.as_bool())
                .unwrap_or(false)
            {
                "; nobody follows it now, so it is no longer polled"
            } else {
                ""
            }
        );
    }
    Ok(())
}

fn subs(as_: Option<&str>, json: bool) -> Result<()> {
    let cfg = Config::load()?;
    let st = state::State::load()?;
    let (me, reviewer) = identity(as_)?.context(
        "not inside an agent session (SSF_REPO/SSF_ISSUE unset); pass --as owner/repo#N",
    )?;
    // The subscriber is always the owning session (a reviewer is its own).
    let me_id = if reviewer {
        me.session(true)
    } else {
        st.repos
            .get(&me.repo)
            .map(|rs| {
                let mut cur = me.number;
                let mut hops = 0;
                while let Some(next) = rs.issues.get(&cur).and_then(|s| s.shares_workspace_of) {
                    if next == cur || hops > 16 {
                        break;
                    }
                    cur = next;
                    hops += 1;
                }
                status::session_id(&me.repo, cur)
            })
            .unwrap_or_else(|| me.to_string())
    };
    let mut following = Vec::new();
    let mut followers = Vec::new();
    for repo in &cfg.repos {
        let Some(rs) = st.repos.get(&repo.name) else {
            continue;
        };
        for item in rs.issues.values() {
            let id = status::session_id(&repo.name, item.number);
            let owner = if item.subscriber_only {
                None
            } else {
                Some(status::session_id(
                    &repo.name,
                    item.shares_workspace_of.unwrap_or(item.number),
                ))
            };
            if item
                .subscribers
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&me_id))
            {
                following.push(json!({
                    "item": id,
                    "title": item.title,
                    "kind": item.kind,
                    "github_state": item.github_state,
                    "owner": owner,
                }));
            }
            if owner
                .as_deref()
                .is_some_and(|o| o.eq_ignore_ascii_case(&me_id))
                && !item.subscribers.is_empty()
            {
                followers.push(json!({
                    "item": id,
                    "title": item.title,
                    "subscribers": item.subscribers,
                }));
            }
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "me": me_id,
                "subscribed_to": following,
                "subscribers": followers,
            }))?
        );
        return Ok(());
    }
    println!("{me_id} follows:");
    if following.is_empty() {
        println!("  (nothing; `ssf sub <n>` to follow an item)");
    }
    for f in &following {
        println!(
            "  {:<24} {:<7} {}  ({})",
            f["item"].as_str().unwrap_or(""),
            f["github_state"].as_str().unwrap_or("?"),
            f["title"].as_str().unwrap_or(""),
            match f["owner"].as_str() {
                Some(o) => format!("owned by {o}"),
                None => "no session".into(),
            }
        );
    }
    println!("followed by other sessions:");
    if followers.is_empty() {
        println!("  (nobody)");
    }
    for f in &followers {
        let subs: Vec<&str> = f["subscribers"]
            .as_array()
            .map(|a| a.iter().filter_map(|s| s.as_str()).collect())
            .unwrap_or_default();
        println!(
            "  {:<24} {}  <- {}",
            f["item"].as_str().unwrap_or(""),
            f["title"].as_str().unwrap_or(""),
            subs.join(", ")
        );
    }
    Ok(())
}

async fn tell(item: &str, message: Option<String>, as_: Option<&str>, json: bool) -> Result<()> {
    let me = identity(as_)?;
    let target = item_ref(item, me.as_ref().map(|(o, _)| o))?;
    let text = match message {
        Some(m) => m,
        None => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
    };
    if text.trim().is_empty() {
        bail!("nothing to say (pass the message, or pipe it in)");
    }
    let v = ipc::call(&ipc::Request::Tell {
        from: me.map(|(o, reviewer)| o.session(reviewer)),
        target: target.clone(),
        text,
    })
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!(
        "delivered to the session on {target} ({}){}",
        v.get("session").and_then(|s| s.as_str()).unwrap_or("?"),
        if v.get("relaunched")
            .and_then(|b| b.as_bool())
            .unwrap_or(false)
        {
            ", whose agent had to be relaunched for it"
        } else {
            ""
        }
    );
    Ok(())
}

async fn release(item: Option<&str>, as_: Option<&str>, force: bool, json: bool) -> Result<()> {
    let me = identity(as_)?;
    let session = match item {
        Some(i) => item_ref(i, me.as_ref().map(|(o, _)| o))?,
        None => me
            .as_ref()
            .map(|(o, reviewer)| o.session(*reviewer))
            .context("not inside an agent session (SSF_REPO/SSF_ISSUE unset); name the item, or pass --as owner/repo#N")?,
    };
    // Inside a session `--force` is not the agent's to use: the checks are
    // the whole point. A person passes --as, or runs it from a plain shell.
    if force && as_.is_none() && origin::Origin::from_env().is_some() {
        bail!(
            "--force is for a person who has looked at the workspace: run `ssf release --as {session} --force` from a shell"
        );
    }
    let v = ipc::call(&ipc::Request::Release { session, force }).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        if v.get("released").and_then(|b| b.as_bool()) != Some(true) {
            std::process::exit(1);
        }
        return Ok(());
    }
    let session = v.get("session").and_then(|s| s.as_str()).unwrap_or("?");
    let path = v.get("path").and_then(|s| s.as_str()).unwrap_or("");
    let problems: Vec<&str> = v
        .pointer("/check/problems")
        .and_then(|p| p.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    if v.get("released").and_then(|b| b.as_bool()) != Some(true) {
        let mut msg = format!(
            "not released: the workspace of {session} ({path}) holds work that is not on origin:\n"
        );
        for p in &problems {
            msg.push_str(&format!("  - {p}\n"));
        }
        msg.push_str(
            "nothing was removed. Commit, push and try again; a kept workspace costs nothing.",
        );
        bail!("{msg}");
    }
    if v.get("already_gone").and_then(|b| b.as_bool()) == Some(true) {
        println!("{session}: the workspace was already gone; recorded as released.");
        return Ok(());
    }
    let secs = v
        .get("poll_interval_secs")
        .and_then(|n| n.as_u64())
        .unwrap_or(10);
    if v.get("forced").and_then(|b| b.as_bool()) == Some(true) {
        println!("{session}: release forced despite:");
        for p in &problems {
            println!("  - {p}");
        }
    } else {
        println!("{session}: clean and on origin.");
    }
    println!(
        "The workspace ({path}) is removed on the daemon's next pass (within {secs}s), with its terminal. Stop here."
    );
    Ok(())
}

async fn purge(dry_run: bool, older_than: Option<u64>, force: bool, json: bool) -> Result<()> {
    let v = ipc::call(&ipc::Request::Purge {
        dry_run,
        older_than_days: older_than,
        force,
    })
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let rows = v
        .get("workspaces")
        .and_then(|w| w.as_array())
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        println!(
            "no workspaces to purge: none belongs to a closed item{}",
            older_than
                .map(|d| format!(" retired more than {d} days ago"))
                .unwrap_or_default()
        );
        return Ok(());
    }
    let mut removed = 0;
    let mut kept = 0;
    let mut forceable = 0;
    for r in &rows {
        let s = |k: &str| r.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let did = r.get("removed").and_then(|b| b.as_bool()).unwrap_or(false);
        let state = s("state");
        let gone = state == "already gone";
        let verb = if did {
            removed += 1;
            if gone { "forgot" } else { "removed" }
        } else if dry_run {
            if gone {
                "would forget"
            } else if state == "clean and pushed" || (force && state != "agent running") {
                "would remove"
            } else {
                "would keep"
            }
        } else {
            kept += 1;
            if state != "agent running" {
                forceable += 1;
            }
            "kept"
        };
        let title = s("title");
        let given_up = r
            .get("release_given_up")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        println!(
            "{:<13} {} \"{}\"  [{}]{}  {}",
            verb,
            s("session"),
            status::one_line(&title, 50),
            s("state"),
            if given_up { " (release given up)" } else { "" },
            s("path")
        );
        if let Some(problems) = r.get("problems").and_then(|p| p.as_array()) {
            for p in problems.iter().filter_map(|p| p.as_str()) {
                println!("              - {p}");
            }
        }
        if let Some(e) = r.get("error").and_then(|e| e.as_str()) {
            println!("              error: {e}");
        }
    }
    if dry_run {
        println!("dry run: nothing was removed.");
    } else {
        println!(
            "{removed} removed, {kept} kept{}.",
            if forceable > 0 && !force {
                "; `ssf purge --force` removes the kept ones too, losing what is in them"
            } else {
                ""
            }
        );
    }
    Ok(())
}

fn ui_cmd(command: UiCommand) -> Result<()> {
    match command {
        UiCommand::Install { quiet } => ui::install_all(quiet),
        UiCommand::Uninstall => ui::uninstall_all(),
        UiCommand::Service { command } => match command {
            ServiceCommand::Enable => {
                ui::set_service_enabled(true)?;
                println!("service enabled");
                Ok(())
            }
            ServiceCommand::Disable => {
                ui::set_service_enabled(false)?;
                println!("service disabled");
                Ok(())
            }
            ServiceCommand::Toggle => {
                let next = !ui::service_enabled();
                ui::set_service_enabled(next)?;
                println!("service {}", if next { "enabled" } else { "disabled" });
                Ok(())
            }
            ServiceCommand::IsEnabled => {
                if ui::service_enabled() {
                    Ok(())
                } else {
                    std::process::exit(1)
                }
            }
            ServiceCommand::Status { json } => {
                let enabled = ui::service_enabled();
                let active = ui::service_active();
                if json {
                    println!("{}", json!({"enabled": enabled, "active": active}));
                } else {
                    println!("enabled: {enabled}\nactive:  {active}");
                }
                Ok(())
            }
        },
    }
}

async fn doctor() -> Result<()> {
    let mut problems = 0;
    let mut check = |ok: bool, msg: String| {
        println!("{} {}", if ok { "ok  " } else { "FAIL" }, msg);
        if !ok {
            problems += 1;
        }
    };
    let cfg = match Config::load() {
        Ok(c) => {
            check(
                true,
                format!("config readable at {}", config::config_path().display()),
            );
            c
        }
        Err(e) => {
            check(false, format!("config: {e:#}"));
            Config::default()
        }
    };
    match cfg.github_token() {
        Ok(token) => match github::GitHub::new(&cfg.github.api_url, &token) {
            Ok(gh) => match gh.whoami().await {
                Ok(me) => {
                    check(true, format!("GitHub token belongs to @{}", me.login));
                    match &cfg.github.login {
                        Some(l) if l.eq_ignore_ascii_case(&me.login) => {}
                        Some(l) => check(
                            false,
                            format!("config expects @{l}; run `ssf auth login --user {l}`"),
                        ),
                        None => check(
                            false,
                            "bot identity not recorded; run `ssf auth login`".into(),
                        ),
                    }
                    let key_ok = cfg
                        .github
                        .ssh_key_path
                        .as_deref()
                        .is_some_and(|p| std::path::Path::new(p).exists());
                    check(
                        key_ok,
                        format!(
                            "bot SSH key {}",
                            cfg.github
                                .ssh_key_path
                                .as_deref()
                                .unwrap_or("(none; commits unsigned, HTTPS pushes only)")
                        ),
                    );
                }
                Err(e) => check(false, format!("GitHub token rejected: {e:#}")),
            },
            Err(e) => check(false, format!("HTTP client: {e:#}")),
        },
        Err(e) => check(false, format!("{e:#}")),
    }
    for d in driver::Drivers::from_config(&cfg).iter() {
        let cli_present =
            std::path::Path::new(d.command()).exists() || which(d.command()).is_some();
        check(
            cli_present,
            format!("{} driver: CLI at {}", d.label(), d.command()),
        );
        if cli_present {
            match d.status().await {
                Ok(()) => check(true, format!("{} reachable and ready", d.label())),
                Err(e) => check(false, format!("{}: {e:#}", d.label())),
            }
        }
    }
    if let Some(note) = cfg.driver_note() {
        println!("note {note}");
    }
    // Each harness a repository uses, signed in where this runs (the host,
    // or the guest: with the factory in a VM `ssf doctor` is forwarded
    // there, so the check happens where the agents are).
    let mut harnesses: Vec<String> = cfg.repos.iter().map(|r| r.harness.clone()).collect();
    harnesses.sort();
    harnesses.dedup();
    let place = if vm::in_guest() {
        "inside the VM"
    } else {
        "on the host"
    };
    for h in &harnesses {
        let probe = login::probe(h);
        let name = login::display_name(h);
        match probe.state {
            login::LoginState::SignedIn => {
                check(true, format!("{name} signed in {place} ({})", probe.detail))
            }
            login::LoginState::SignedOut => check(
                false,
                format!(
                    "{name} not signed in {place} ({}); sign in with {}, or sessions on it stall at its login prompt",
                    probe.detail,
                    login::how_to_sign_in(h)
                ),
            ),
            login::LoginState::Unknown => {
                println!(
                    "note {name}: cannot tell whether it is signed in {place} ({})",
                    probe.detail
                )
            }
        }
    }
    match ipc::call(&ipc::Request::Ping).await {
        Ok(v) => check(
            true,
            format!(
                "daemon answering on {} as @{}",
                ipc::socket_path().display(),
                v.get("login").and_then(|l| l.as_str()).unwrap_or("?")
            ),
        ),
        Err(e) => {
            println!("note the daemon is not answering (`ssf sub|unsub|tell` need it): {e:#}")
        }
    }
    check(
        !cfg.repos.is_empty(),
        format!(
            "{} repositor{} configured",
            cfg.repos.len(),
            if cfg.repos.len() == 1 { "y" } else { "ies" }
        ),
    );
    let installed = agents::list();
    let gh = cfg
        .github_token()
        .ok()
        .and_then(|t| github::GitHub::new(&cfg.github.api_url, &t).ok());
    let bot = cfg.github.login.clone().unwrap_or_else(|| "the bot".into());
    for r in &cfg.repos {
        // Who may drive it: the configured list, or the collaborators with
        // push access fetched the way the daemon does.
        match cfg.allowed_users(r) {
            Some((list, source)) => {
                let l = allow::AllowList::new(&bot, list.iter().map(String::as_str), source);
                if l.is_anyone() {
                    println!("WARN {}: allowed users: {}", r.name, l.describe());
                } else {
                    check(true, format!("{}: allowed users: {}", r.name, l.describe()));
                }
            }
            None => match (&gh, r.split()) {
                (Some(gh), Ok((owner, name))) => match gh.collaborators(owner, name, None).await {
                    Ok(github::Conditional::Modified { value, .. }) => {
                        let l = allow::AllowList::new(
                            &bot,
                            allow::pushers(&value).iter().map(String::as_str),
                            allow::Source::Collaborators,
                        );
                        check(true, format!("{}: allowed users: {}", r.name, l.describe()));
                    }
                    Ok(github::Conditional::NotModified) => {}
                    Err(e) => check(
                        false,
                        format!(
                            "{}: allowed users: collaborators could not be fetched, so nothing is acted on; \
                             list them with `ssf repo set {} --allowed-users alice,bob` (or daemon.allowed_users): {e:#}",
                            r.name, r.name
                        ),
                    ),
                },
                _ => check(
                    false,
                    format!(
                        "{}: allowed users: the collaborators with push access (cannot be fetched without a token)",
                        r.name
                    ),
                ),
            },
        }
        let cmd = r.harness_command();
        let bin = cmd.split_whitespace().next().unwrap_or("");
        let ok = which(bin).is_some() || installed.iter().any(|a| a.id == r.harness && a.installed);
        check(ok, format!("{}: harness `{}` installed", r.name, r.harness));
        if let Some(p) = &r.path {
            check(
                std::path::Path::new(p).join(".git").exists(),
                format!("{}: checkout at {p}", r.name),
            );
        }
    }
    match shim::real_gh() {
        Some(gh) => check(true, format!("GitHub CLI at {}", gh.display())),
        None => check(
            false,
            "GitHub CLI (gh) not installed; agents cannot post as the bot".into(),
        ),
    }
    let me = std::env::current_exe().and_then(std::fs::canonicalize).ok();
    let is_me = |p: &std::path::Path| me.is_some() && std::fs::canonicalize(p).ok() == me;
    // Both links (gh and ssf) have to point at this binary for agents to
    // post as the bot and run this daemon's CLI.
    let links: Vec<(&str, Option<PathBuf>)> = shim::LINKS
        .iter()
        .map(|name| (*name, std::fs::read_link(shim::dir().join(name)).ok()))
        .collect();
    let links_ok = links.iter().all(|(_, t)| t.as_deref().is_some_and(is_me));
    let where_ = format!(
        "{} links in {}",
        shim::LINKS.join(" and "),
        shim::dir().display()
    );
    if links_ok {
        check(true, format!("{where_} point at this ssf"));
    } else if links.iter().all(|(_, t)| t.is_none()) {
        check(
            false,
            format!("{where_} not installed yet (ssf launch creates them when an agent starts)"),
        );
    } else {
        let odd = links
            .iter()
            .map(|(name, t)| match t {
                Some(t) if is_me(t) => format!("{name} ok"),
                Some(t) => format!("{name} -> {}", t.display()),
                None => format!("{name} missing"),
            })
            .collect::<Vec<_>>()
            .join(", ");
        check(
            false,
            format!(
                "{where_} do not all point at this ssf ({odd}); ssf launch relinks them when an agent starts"
            ),
        );
    }
    // Informational: the shim directory goes first on an agent's PATH, so
    // another ssf on PATH only matters to a person typing in their shell.
    check(
        true,
        match (me.is_some(), shim::ssf_on_path()) {
            (true, Some(p)) if is_me(&p) => {
                format!("ssf on PATH at {} is this binary", p.display())
            }
            (true, Some(p)) => format!(
                "ssf on PATH at {} is not this binary; commands typed in a shell run that one, agents run this one",
                p.display()
            ),
            (false, Some(p)) => format!(
                "ssf on PATH at {}; cannot tell whether it is this binary",
                p.display()
            ),
            (_, None) => {
                "no ssf on PATH; agents run this one through the shim directory".to_string()
            }
        },
    );
    let st = state::State::load().unwrap_or_default();
    let untagged: Vec<String> = st
        .repos
        .values()
        .flat_map(|r| r.issues.values())
        .flat_map(|i| i.untagged.values().cloned())
        .collect();
    check(
        untagged.is_empty(),
        if untagged.is_empty() {
            "every post by the bot carried an origin tag".to_string()
        } else {
            format!(
                "{} post(s) by the bot arrived without an origin tag (a person posting as the bot, or the gh shim not in effect): {}",
                untagged.len(),
                untagged
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    );
    check(
        ui::service_active(),
        format!(
            "{} running{}",
            ui::SERVICE,
            if ui::service_enabled() {
                ""
            } else {
                " (disabled by `ssf ui service disable`)"
            }
        ),
    );
    check(
        ui::widget_enabled().unwrap_or(false),
        "bar widget enabled in ~/.config/omarchy/shell.json".into(),
    );
    check(
        true,
        format!(
            "new clones go under {}",
            cfg.projects_dir(cfg.default_driver()).display()
        ),
    );
    if problems > 0 {
        bail!("{problems} problem(s) found");
    }
    println!("all good");
    Ok(())
}

fn which(bin: &str) -> Option<std::path::PathBuf> {
    if bin.contains('/') {
        let p = std::path::PathBuf::from(bin);
        return p.exists().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_refs_accept_numbers_sessions_and_reviewers() {
        let me = origin::Origin::new("o/r", 3).unwrap();
        assert_eq!(item_ref("7", Some(&me)).unwrap(), "o/r#7");
        assert_eq!(item_ref("#7", Some(&me)).unwrap(), "o/r#7");
        assert_eq!(item_ref("7:reviewer", Some(&me)).unwrap(), "o/r#7:reviewer");
        assert_eq!(item_ref("x/y#7", None).unwrap(), "x/y#7");
        assert_eq!(item_ref("x/y#7:reviewer", None).unwrap(), "x/y#7:reviewer");
        assert!(item_ref("7", None).is_err());
        assert!(item_ref("7:author", Some(&me)).is_err());
        assert!(item_ref("nonsense", Some(&me)).is_err());
    }
    #[test]
    fn allowed_users_flags_parse_logins_and_the_wildcard() {
        assert_eq!(
            parse_allowed_users("Alice, @bob,carol"),
            vec!["Alice", "bob", "carol"]
        );
        assert_eq!(parse_allowed_users("*"), vec!["*"]);
        assert!(parse_allowed_users("").is_empty());
        let mut entry = RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            ..Default::default()
        };
        set_repo_allowed_users(&mut entry, "alice,bob", false).unwrap();
        assert_eq!(
            entry.allowed_users.as_deref(),
            Some(&["alice".to_string(), "bob".to_string()][..])
        );
        assert!(!entry.accepted_anyone_risk);
        set_repo_allowed_users(&mut entry, "*", true).unwrap();
        assert!(entry.accepted_anyone_risk);
        // Back to a list: the marker goes, so a later hand edit is refused.
        set_repo_allowed_users(&mut entry, "alice", false).unwrap();
        assert!(!entry.accepted_anyone_risk);
    }

    #[test]
    fn the_wildcard_is_refused_without_consent() {
        assert!(anyone_risk_decision(true, false, "x", || unreachable!()).is_ok());
        let err = anyone_risk_decision(false, false, "x", || unreachable!()).unwrap_err();
        assert!(err.to_string().contains("--accept-anyone-risk"), "{err}");
        assert!(err.to_string().contains("ANYONE"), "{err}");
        assert!(anyone_risk_decision(false, true, "x", || Ok(false)).is_err());
        assert!(anyone_risk_decision(false, true, "x", || Ok(true)).is_ok());
    }

    #[test]
    fn config_set_writes_the_wildcard_only_with_its_marker() {
        let dir = std::env::temp_dir().join(format!(
            "ssf-config-set-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        config_set_at(&path, "daemon.allowed_users", r#"["*"]"#, true).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("accepted_anyone_risk = true"), "{text}");
        let cfg = Config::load_from(&path).unwrap();
        assert!(cfg.anyone_allowed_anywhere());
        // A list again: the marker goes with the wildcard.
        config_set_at(&path, "daemon.allowed_users", r#"["Alice", "bob"]"#, false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("accepted_anyone_risk"), "{text}");
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(
            cfg.daemon.allowed_users.as_deref(),
            Some(&["Alice".to_string(), "bob".to_string()][..])
        );
        // A bare login list works too.
        config_set_at(&path, "daemon.allowed_users", "carol", false).unwrap();
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(
            cfg.daemon.allowed_users.as_deref(),
            Some(&["carol".to_string()][..])
        );
        // A hand edit that adds the wildcard without the marker is refused
        // at load, and by any later `config set` of another key.
        std::fs::write(&path, "[daemon]\nallowed_users = [\"*\"]\n").unwrap();
        assert!(Config::load_from(&path).is_err());
        let err = config_set_at(&path, "daemon.poll_interval_secs", "5", false).unwrap_err();
        assert!(
            format!("{err:#}").contains("--accept-anyone-risk"),
            "{err:#}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
