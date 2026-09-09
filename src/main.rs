//! ssf — Simple Software Factory.
//!
//! Watches GitHub repos for issues assigned to a bot account and turns each one
//! into a workspace (in herdr or Orca) running a coding agent, feeding later issue activity
//! into that agent.

mod agents;
mod allow;
mod config;
mod driver;
mod engine;
mod events;
mod ghcli;
mod github;
mod herdr;
mod ipc;
mod keys;
mod login;
mod models;
mod orca;
mod origin;
mod platform;
mod prompt;
mod release;
mod sessions;
mod shim;
mod state;
mod status;
mod ui;
mod uninstall;
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
    about = "Simple Software Factory: GitHub issues -> agent workspaces in herdr or Orca"
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
    /// Manage the bot account credentials (for people, from a terminal, or the agent setting ssf up for them; factory sessions never run it).
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
    /// Read or change settings (dotted keys, e.g. daemon.poll_interval_secs, daemon.startup_driver_wait_secs, vm.enabled, driver).
    Config {
        #[command(subcommand)]
        command: Option<ConfigCommand>,
    },
    /// Run the daemon: poll GitHub and drive the workspaces (herdr or Orca).
    Run {
        /// Do a single pass and exit.
        #[arg(long)]
        once: bool,
    },
    /// Show tracked issues and their workspaces, joined with what the driver
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
    /// Hand this session's item to a new session on another harness,
    /// model or effort, in the same workspace: the daemon ends this
    /// session on its next pass and starts the new one there, with the
    /// summary written here ahead of the item's story. The harness,
    /// model and effort stay with the item until its workspace is
    /// released. Inside a session it is this session's item; from a shell
    /// name the item.
    #[command(group(
        clap::ArgGroup::new("summary_form").args(["summary", "summary_file", "no_summary"])
    ))]
    Handover {
        /// Item number on this session's repository, or owner/repo#N.
        item: Option<String>,
        /// Drop a handover the daemon has not carried out yet; the
        /// session that is there keeps the item and is told to carry on.
        #[arg(long, conflicts_with_all = ["harness", "model", "effort", "summary_form"])]
        cancel: bool,
        /// Harness the new session runs (`ssf agents` lists the ids).
        #[arg(long, value_name = "ID", required_unless_present = "cancel")]
        harness: Option<String>,
        /// Model for the new session (`ssf models <harness>` lists them);
        /// the harness's own default when not given.
        #[arg(long, value_name = "ID")]
        model: Option<String>,
        /// Effort level for the new session; the harness's own default
        /// when not given.
        #[arg(long, value_name = "LEVEL")]
        effort: Option<String>,
        /// What the new session is told before the item's story: what the
        /// item is about, what is done, what is left, where things are.
        #[arg(long, value_name = "TEXT")]
        summary: Option<String>,
        /// Read the summary from a file instead.
        #[arg(long, value_name = "PATH")]
        summary_file: Option<PathBuf>,
        /// Hand over with no summary: the new session reads the item itself.
        #[arg(long)]
        no_summary: bool,
        /// Act as this session (owner/repo#N) instead of $SSF_REPO/$SSF_ISSUE.
        #[arg(long = "as", value_name = "SESSION")]
        r#as: Option<String>,
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
    /// following items, hand-offs and second opinions work. The initial
    /// prompt points here.
    Guide,
    /// Check that GitHub, the drivers in use and the configured harnesses are usable.
    Doctor,
    /// Omarchy desktop integration: bar widget, menu entries, background service.
    Ui {
        #[command(subcommand)]
        command: UiCommand,
    },
    /// Take this machine back to just the package: purge closed workspaces,
    /// stop and disable the service, remove the bar widget and menu entries,
    /// sign the bot out (revoking its keys on GitHub), destroy the microVM.
    /// Reports first and asks once. Leaves the package (`sudo pacman -R ssf`,
    /// `apt remove` or `dnf remove`; the command prints the one for this
    /// machine), the projects directory (clones and worktrees), and, without
    /// `--data`, the config and state directories.
    Uninstall {
        /// Skip the confirmation (scripted use).
        #[arg(long, short = 'y')]
        yes: bool,
        /// Go ahead even when a workspace holds uncommitted or unpushed work
        /// (kept on the host; destroyed with the VM's disks in VM mode), or
        /// the VM is stopped so its workspaces cannot be checked.
        #[arg(long)]
        force: bool,
        /// Also remove ~/.config/ssf (config, the bot's key) and
        /// ~/.local/state/ssf (state, and the disabled-service marker).
        #[arg(long)]
        data: bool,
        /// Only print the report (what would be stopped, removed and revoked),
        /// as JSON. Used by the host to ask the guest in VM mode.
        #[arg(long, hide = true)]
        report: bool,
    },
    /// Run the whole factory (daemon, herdr, sessions) inside a VM instead
    /// of on this machine: build the guest, start, stop and reach it.
    /// `[vm] backend` picks what runs it: a Firecracker microVM (the
    /// default on Linux) or a lima instance (the default on macOS, and
    /// Linux with qemu).
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
    /// Size the VM from this machine, then make the guest and provision
    /// it (git, gh, herdr, the harness CLIs). Firecracker (`[vm] backend`,
    /// the default on Linux): downloads Firecracker, gvproxy and a guest
    /// kernel and makes the root image from the Arch bootstrap tarball.
    /// lima (the default on macOS): creates the `ssf-<name>` instance and
    /// its data disk from a cloud image and boots it once. No root needed.
    ///
    /// Every `[vm]` size key left unset is chosen from the host, printed
    /// and written to config.toml: `vcpus` is the CPUs minus one (at least
    /// 2), `mem_mib` half the RAM (at least 4096), `data_gib` half the
    /// free space of the filesystem the data disk lands on (at least 20;
    /// the disk is sparse, so this reserves nothing) -- `[vm] dir` under
    /// Firecracker, lima's own disk directory under lima, and the line
    /// printed says which was measured. A key already in `[vm]` is kept;
    /// a flag below writes a value of your own.
    Build {
        /// Make a new image even if one exists.
        #[arg(long)]
        force: bool,
        /// The guest's vCPUs, written to config.toml instead of the rule.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
        vcpus: Option<u32>,
        /// The guest's memory in MiB, written to config.toml instead of the rule.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
        mem_mib: Option<u32>,
        /// The data disk's size in GiB, written to config.toml instead of the rule.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
        data_gib: Option<u32>,
    },
    /// Enlarge an existing VM's data disk, keeping what is on it (the VM
    /// must be stopped).
    ///
    /// Grows to the size given, or to the rule for today's free space
    /// (half of it, at least 20 GiB): Firecracker runs `e2fsck -f`,
    /// lengthens the file and `resize2fs`; lima runs `limactl disk resize`
    /// and the guest grows the filesystem at its next boot. Then `[vm]
    /// data_gib` is updated. Never shrinks; a smaller disk means a new VM.
    Grow {
        /// The new size in GiB (at least the current size).
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
        data_gib: Option<u32>,
    },
    /// Boot the VM (making its disks on first use) and wait for its daemon.
    Start,
    /// Shut the VM down cleanly.
    Stop,
    /// Stop, then start (picks up a new ssf binary and `[vm] files`).
    Restart,
    /// Whether the VM runs and its daemon answers, its size, and how full
    /// the data disk is.
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
    /// A fresh root at the next start (Firecracker: the root disk remade
    /// from the image; lima: the instance re-created, provisioned again
    /// on its first boot); state, clones and worktrees on the data disk
    /// stay.
    Reset,
    /// Remove the VM and all its disks (lima: the instance and its data
    /// disk too).
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
        /// Model the harness runs with: an Orca model id (e.g. opus, sonnet, gpt-5.5) for claude, codex, gemini
        /// and grok; provider/model for pi, omp and opencode; `auto` or a model
        /// name for copilot. `ssf models <harness>` lists them.
        #[arg(long)]
        model: Option<String>,
        /// Effort level for the model, one the harness accepts (e.g. low, medium, high, xhigh, max; `ssf agents --json` lists them).
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
        /// Post the daemon's events (session attached, resumed, held, given up, released) on this
        /// repository's items as short `ssf` blocks, overriding daemon.event_comments (default: on).
        #[arg(long, value_name = "true|false")]
        event_comments: Option<bool>,
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
        /// Model the harness runs with: an Orca model id (e.g. opus, sonnet, gpt-5.5) for claude, codex, gemini
        /// and grok; provider/model for pi, omp and opencode; `auto` or a model
        /// name for copilot. `ssf models <harness>` lists them.
        #[arg(long)]
        model: Option<String>,
        /// Effort level for the model, one the harness accepts (e.g. low, medium, high, xhigh, max; `ssf agents --json` lists them).
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
        /// Post the daemon's events (session attached, resumed, held, given up, released) on this
        /// repository's items as short `ssf` blocks, overriding daemon.event_comments.
        #[arg(long, value_name = "true|false")]
        event_comments: Option<bool>,
        /// Commit author and committer name for this repository's agents (with --git-email); default: the [git] table, else the bot.
        #[arg(long, value_name = "NAME")]
        git_name: Option<String>,
        /// Commit author and committer email: one the person's GitHub account has verified.
        #[arg(long, value_name = "EMAIL")]
        git_email: Option<String>,
        /// SSH key to sign commits with, or `false` for unsigned (default: the bot's key for the bot, unsigned for a person).
        #[arg(long, value_name = "PATH|false")]
        git_signing_key: Option<String>,
        /// Who pushes over HTTPS: bot, token:<gh login>, file:<token file>, or a git credential helper string.
        #[arg(long, value_name = "WHO")]
        git_credential: Option<String>,
        /// Clear an optional field: driver, path, clone_url, base_branch, command, model, effort, instructions, prompt_file, allowed_users,
        /// event_comments, git (the whole [repo.git] table) or git.name, git.email, git.signing_key, git.credential.
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
    /// Copy the bar widget into ~/.config/omarchy/plugins, enable it, add menu entries.
    Install {
        #[arg(long)]
        quiet: bool,
    },
    /// Remove the bar widget and menu entries.
    Uninstall,
    /// Control the background service (the `ssf.service` user unit on Linux, the Homebrew service on macOS).
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
        // Is the guest up? "No" and "could not ask" are different
        // answers: under lima the question forks `limactl`, and a fork
        // that fails is not a factory that has stopped.
        let probe = vm.running_now().map_err(|e| format!("{e:#}"));
        // What the backend needs and this host has not got, when the
        // answer is anything but "it is running". The check is PATH and
        // file lookups, no fork of its own; it is the same one `ssf vm
        // status` and `ssf doctor` print, and this is where a person on
        // a machine without the backend installed meets it first.
        let missing = (probe != Ok(true))
            .then(|| vm.tooling())
            .filter(|t| !t.ok)
            .map(|t| t.detail);
        let backend = vm.backend().to_string();
        match forwarding_gate(&probe, &cfg.vm.name, &backend, name, missing.as_deref()) {
            Gate::Refuse(why) => match cli.command {
                Command::Status { json: true } => {
                    println!("{}", vm_status_for_guest(probe_word(&probe)));
                    return Ok(());
                }
                Command::Status { json: false } => {
                    println!(
                        "vm:      {} is not running (`ssf vm start`, or `ssf ui service enable`)",
                        cfg.vm.name
                    );
                    return Ok(());
                }
                _ => bail!(why),
            },
            Gate::Send(note) => {
                if let Some(note) = note {
                    eprintln!("{note}");
                }
                let args: Vec<String> = std::env::args().skip(1).collect();
                // `status --json` is answered even when the guest does
                // not answer it: an ssh that fails -- the VM down behind
                // an unanswerable probe, or the window after `limactl
                // start` where lima says Running before sshd does --
                // would otherwise print nothing at all. What that buys
                // is a document to parse, whose `service_enabled` and
                // `vm` are read from this host and true: the bar widget
                // coerces anything it cannot parse to an empty object,
                // where its own service toggle reads as disabled, and a
                // `jq` over this command gets a field rather than a
                // parse error. The guest's own answer is passed through
                // untouched, with its exit status; silence is what gets
                // a document made for it, saying what the probe saw of
                // the VM, nothing of the sessions it could not ask
                // after, and exiting 0 the way a stopped VM's answer
                // above does.
                if matches!(cli.command, Command::Status { json: true }) {
                    let out = vm.capture_ssf(&args);
                    if let Err(e) = &out {
                        eprintln!("running `ssf {name}` in the VM: {e:#}");
                    }
                    let answer = out
                        .as_ref()
                        .ok()
                        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                        .filter(|s| !s.trim().is_empty());
                    match answer {
                        Some(answer) => {
                            print!("{answer}");
                            std::process::exit(
                                out.map(|o| o.status.code().unwrap_or(1)).unwrap_or(1),
                            );
                        }
                        None => {
                            println!("{}", vm_status_for_guest(probe_word(&probe)));
                            return Ok(());
                        }
                    }
                }
                let st = vm
                    .exec_ssf(&args)
                    .with_context(|| format!("running `ssf {name}` in the VM"))?;
                if let Some(note) = forwarded_failure_note(&cli.command, &cfg.vm.name, st.success())
                {
                    eprintln!("{note}");
                }
                std::process::exit(st.code().unwrap_or(1));
            }
        }
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
        Command::Handover {
            item,
            cancel,
            harness,
            model,
            effort,
            summary,
            summary_file,
            no_summary,
            r#as,
            json,
        } => {
            handover(
                item.as_deref(),
                cancel,
                harness.as_deref(),
                model.as_deref(),
                effort.as_deref(),
                summary,
                summary_file.as_deref(),
                no_summary,
                r#as.as_deref(),
                json,
            )
            .await
        }
        Command::Purge {
            dry_run,
            older_than,
            force,
            json,
        } => purge(dry_run, older_than, force, json).await,
        Command::Guide => {
            let state_bot = state::State::load().ok().and_then(|state| state.bot_login);
            let config_bot = Config::load().ok().and_then(|cfg| cfg.github.login);
            let bot = configured_bot_login(
                std::env::var("SSF_BOT").ok().as_deref(),
                state_bot.as_deref(),
                config_bot.as_deref(),
            )
            .unwrap_or_else(|| "<bot>".into());
            print!("{}", prompt::guide(&bot, vm::in_guest()));
            Ok(())
        }
        Command::Doctor => doctor().await,
        Command::Vm { command } => vm_cmd(command).await,
        Command::Ui { command } => ui_cmd(command),
        Command::Uninstall {
            yes,
            force,
            data,
            report,
        } => {
            if report {
                uninstall::print_report().await
            } else if vm::in_guest() {
                bail!("`ssf uninstall` runs on the host, which owns the VM")
            } else {
                uninstall::run(yes, force, data).await
            }
        }
        Command::Launch {
            repo,
            issue,
            issue_url,
            command,
        } => launch(repo, issue, issue_url, command),
        Command::GitCredential { op } => git_credential(&op),
    }
}

fn launch(
    repo: Option<String>,
    issue: Option<u64>,
    issue_url: Option<String>,
    command: Vec<String>,
) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let cfg = Config::load().unwrap_or_default();
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(command.join(" "));
    let me = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "ssf".into());
    let repo_cfg = repo
        .as_deref()
        .and_then(|name| cfg.repos.iter().find(|r| r.name.eq_ignore_ascii_case(name)));
    let token = cfg.github_token();
    if let Err(e) = &token {
        eprintln!("ssf launch: no bot credentials exported ({e:#})");
    }
    let plan = launch_env(&cfg, repo_cfg, &me, token.is_ok());
    for note in &plan.notes {
        eprintln!("ssf launch: {note}");
    }
    if let Ok(token) = &token {
        cmd.env("GH_TOKEN", token).env("GITHUB_TOKEN", token);
    }
    for (k, v) in &plan.env {
        cmd.env(k, v);
    }
    // gh must only ever see the bot. Its own config dir would expose every
    // account in the human's keyring to `gh auth token --user ...`, so point
    // it at an ssf-owned one that lists none; GH_TOKEN carries the identity.
    let gh_dir = config::config_dir().join("gh");
    if std::fs::create_dir_all(&gh_dir).is_ok() {
        cmd.env("GH_CONFIG_DIR", &gh_dir);
    }
    // Git configuration is injected through GIT_CONFIG_* so it beats the
    // human's ~/.gitconfig (identity, signing key, credential helpers) inside
    // the agent's shell only.
    let base: usize = std::env::var("GIT_CONFIG_COUNT")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    cmd.env("GIT_CONFIG_COUNT", (base + plan.git.len()).to_string());
    for (i, (k, v)) in plan.git.iter().enumerate() {
        cmd.env(format!("GIT_CONFIG_KEY_{}", base + i), k)
            .env(format!("GIT_CONFIG_VALUE_{}", base + i), v);
    }
    let state_bot = state::State::load().ok().and_then(|state| state.bot_login);
    if let Some(login) =
        configured_bot_login(None, state_bot.as_deref(), cfg.github.login.as_deref())
    {
        cmd.env("SSF_BOT", login);
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
    // `SSF_ROLE` marked the reviewer sessions of before #115; an old one
    // in the environment must not reach the agent.
    cmd.env_remove("SSF_ROLE");
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

/// A running daemon's last authenticated identity is retained in state for
/// token-only setups. A session's own identity wins, then that cache, then
/// the configured account for a factory that has not started yet.
fn configured_bot_login(
    session_bot: Option<&str>,
    state_bot: Option<&str>,
    config_bot: Option<&str>,
) -> Option<String> {
    session_bot
        .filter(|login| !login.is_empty())
        .or_else(|| state_bot.filter(|login| !login.is_empty()))
        .or_else(|| config_bot.filter(|login| !login.is_empty()))
        .map(str::to_owned)
}

/// What `ssf launch` puts in the agent's environment for git: variables,
/// `GIT_CONFIG_*` entries (in order) and notes for stderr.
#[derive(Debug, Default, PartialEq, Eq)]
struct LaunchEnv {
    env: Vec<(String, String)>,
    git: Vec<(String, String)>,
    notes: Vec<String>,
}

/// The git side of an agent's environment for `repo` (`None`: `[git]`
/// alone): the effective identity as author and committer, signing with
/// its key or off, and who pushes. `gh` is not touched here: it is the bot
/// through `GH_TOKEN`, whatever the identity. `have_token` says whether
/// the bot token could be resolved, which the bot credential needs.
fn launch_env(cfg: &Config, repo: Option<&RepoConfig>, me: &str, have_token: bool) -> LaunchEnv {
    use config::Credential;
    let mut out = LaunchEnv::default();
    let identity = cfg.git_identity(repo);
    // Pushes: our helper first and any configured ones dropped, so HTTPS
    // pushes go out as who the config says rather than as whoever is
    // logged into gh. The helper reads the config (and SSF_REPO) itself.
    match &identity.credential {
        Credential::Bot if !have_token => {}
        Credential::Bot | Credential::Token(_) | Credential::File(_) => {
            out.git.push(("credential.helper".into(), String::new()));
            out.git
                .push(("credential.helper".into(), format!("!{me} git-credential")));
        }
        Credential::Helper(h) => {
            out.git.push(("credential.helper".into(), String::new()));
            out.git.push(("credential.helper".into(), h.clone()));
        }
    }
    if let (Some(name), Some(email)) = (&identity.name, &identity.email) {
        for var in ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"] {
            out.env.push((var.into(), name.clone()));
        }
        for var in ["GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"] {
            out.env.push((var.into(), email.clone()));
        }
        out.git.push(("user.name".into(), name.clone()));
        out.git.push(("user.email".into(), email.clone()));
    }
    // SSH remotes always use the bot's enrolled key: a person's credential
    // is for HTTPS. The key doubles as the bot's signing key.
    if let Some(key) = cfg
        .github
        .ssh_key_path
        .as_deref()
        .map(config::expand_tilde)
        .filter(|p| p.exists())
    {
        out.env.push((
            "GIT_SSH_COMMAND".into(),
            format!(
                "ssh -i {} -o IdentitiesOnly=yes",
                shell_quote(&key.to_string_lossy())
            ),
        ));
    }
    match &identity.signing_key {
        Some(key) if key.exists() => {
            // git hands user.signingkey to ssh-keygen: the public key file
            // when there is one (the private key next to it, or the agent,
            // does the signing), else the private key itself.
            let pubkey = keys::public_path(key);
            let signing = if pubkey.exists() { pubkey } else { key.clone() };
            out.git.push(("gpg.format".into(), "ssh".into()));
            out.git.push((
                "user.signingkey".into(),
                signing.to_string_lossy().to_string(),
            ));
            out.git.push(("commit.gpgsign".into(), "true".into()));
            out.git.push(("tag.gpgsign".into(), "true".into()));
        }
        Some(key) => {
            out.notes.push(format!(
                "signing key {} is missing; commits go out unsigned",
                key.display()
            ));
            out.git.push(("commit.gpgsign".into(), "false".into()));
            out.git.push(("tag.gpgsign".into(), "false".into()));
        }
        None => {
            // Nothing to sign with: make sure commits are not signed with
            // the human's key either.
            out.git.push(("commit.gpgsign".into(), "false".into()));
            out.git.push(("tag.gpgsign".into(), "false".into()));
        }
    }
    out
}

/// Whose token `ssf git-credential` answers with: the effective identity's
/// for the session's repository (`SSF_REPO`; a repository the config does
/// not list gets `[git]` alone). The identity's credential is for the
/// agents' pushes, so outside a session (no `SSF_REPO`: the daemon's own
/// clones and fetches, a shell in the VM guest) it is the bot, whatever
/// `[git]` says.
fn push_credential(cfg: &Config, session_repo: Option<&str>) -> config::Credential {
    match session_repo {
        Some(name) => {
            let repo = cfg.repos.iter().find(|r| r.name.eq_ignore_ascii_case(name));
            cfg.git_identity(repo).credential
        }
        None => config::Credential::Bot,
    }
}

/// `--git-signing-key`: `false` (or `off`, `none`) means unsigned, anything
/// else is the key's path.
fn parse_signing_key(value: &str) -> config::SigningKey {
    let v = value.trim();
    if ["false", "off", "none", "no"]
        .iter()
        .any(|w| v.eq_ignore_ascii_case(w))
    {
        config::SigningKey::Off(false)
    } else {
        config::SigningKey::Path(v.to_string())
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The credential helper `ssf launch` configures (and the VM guest's
/// `.gitconfig` names): answers HTTPS requests for the GitHub host with
/// the token of whoever the config says pushes for `SSF_REPO` (the bot by
/// default), and with the bot's outside a session. For another host it
/// answers nothing and git moves on.
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
    let token = match push_credential(&cfg, std::env::var("SSF_REPO").ok().as_deref()) {
        config::Credential::Bot => cfg.github_token(),
        config::Credential::Token(login) => ghcli::token_for(&wanted, &login),
        config::Credential::File(path) => std::fs::read_to_string(&path)
            .map(|t| t.trim().to_string())
            .with_context(|| format!("reading the token file {}", path.display())),
        // A helper string is set as credential.helper itself; we are not
        // in the chain then.
        config::Credential::Helper(_) => return Ok(()),
    };
    match token {
        Ok(t) if !t.trim().is_empty() => println!("username=x-access-token\npassword={t}"),
        Ok(_) => eprintln!("ssf git-credential: the token is empty"),
        Err(e) => eprintln!("ssf git-credential: {e:#}"),
    }
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
            let identity = cfg.git_identity(None);
            if json {
                println!(
                    "{}",
                    json!({
                        "login": me.login, "type": me.kind, "id": me.id,
                        "email": cfg.github.email,
                        "ssh_key": cfg.github.ssh_key_path, "ssh_key_present": key_ok,
                        "ssh_key_id": cfg.github.ssh_key_id, "signing_key_id": cfg.github.signing_key_id,
                        "git": {
                            "name": identity.name, "email": identity.email,
                            "source": match identity.source {
                                config::IdentitySource::Bot => "bot",
                                config::IdentitySource::Instance => "git",
                                config::IdentitySource::Repo => "repo.git",
                            },
                            "signing_key": identity.signing_key,
                            "credential": identity.credential.to_config(),
                        }
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
                "Bot commit identity: {} <{}>",
                me.login,
                cfg.github
                    .email
                    .as_deref()
                    .unwrap_or("(not set; run `ssf auth login`)")
            );
            if !identity.is_bot() || identity.credential != config::Credential::Bot {
                println!(
                    "Git identity ([git]; repositories may override, see `ssf doctor`): {}",
                    identity.describe(&me.login)
                );
            }
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
        AuthCommand::Logout { keep_keys } => auth_logout(keep_keys).await,
    }
}

/// `ssf auth logout`: revoke the bot's keys on GitHub and remove them here
/// (unless `keep_keys`), remove its token, and forget it as the bot. Also
/// the sign-out step of `ssf uninstall`.
pub async fn auth_logout(keep_keys: bool) -> Result<()> {
    let mut cfg = Config::load()?;
    if !keep_keys {
        if let Ok(token) = cfg.github_token()
            && let Ok(gh) = github::GitHub::new(&cfg.github.api_url, &token)
        {
            for (kind, id) in [
                ("keys", cfg.github.ssh_key_id),
                ("ssh_signing_keys", cfg.github.signing_key_id),
            ] {
                if let Some(id) = id {
                    match gh.delete_key(kind, id).await {
                        Ok(()) => println!("Revoked {kind} entry {id} on GitHub"),
                        Err(e) => {
                            eprintln!("warning: could not revoke {kind} entry {id}: {e:#}")
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
    Ok(())
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

/// This machine's name, for the label on the bot's GitHub key. Linux has
/// `/etc/hostname`; macOS does not, and answers `scutil --get
/// ComputerName` (the name a person gave the Mac) or `hostname`.
fn hostname() -> String {
    let ran = |program: &str, args: &[&str]| {
        std::process::Command::new(program)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
    };
    pick_hostname(
        std::fs::read_to_string("/etc/hostname").ok(),
        || ran("scutil", &["--get", "ComputerName"]),
        || ran("hostname", &[]),
    )
}

/// The first of `/etc/hostname`, `scutil --get ComputerName` and
/// `hostname` that answers with something, trimmed; "localhost" when none
/// does. The file comes first, so a Linux host keeps the name it had.
fn pick_hostname(
    file: Option<String>,
    computer_name: impl Fn() -> Option<String>,
    hostname: impl Fn() -> Option<String>,
) -> String {
    let clean = |s: String| {
        let s = s.trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    file.and_then(clean)
        .or_else(|| computer_name().and_then(clean))
        .or_else(|| hostname().and_then(clean))
        .unwrap_or_else(|| "localhost".to_string())
}

/// herdr starts and reads only the agents it can recognise in a pane.
fn check_herdr_harness(harness: &str) {
    let known = std::process::Command::new(config::herdr_command_path(
        &std::env::var("HERDR_COMMAND").unwrap_or_else(|_| "herdr".into()),
    ))
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
    repo_at(&config::config_path(), command)
}

/// `ssf repo ...` against the config file at `path`.
fn repo_at(config_file: &Path, command: RepoCommand) -> Result<()> {
    let mut cfg = Config::load_from(config_file)?;
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
            event_comments,
        } => {
            let (owner, r) = split_repo_name(&name)?;
            let name = format!("{owner}/{r}");
            let path = expand_checkout(path)?;
            check_harness(&harness);
            let driver = driver.map(|d| d.parse()).transpose()?;
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
                conflict_check_interval_secs: None,
                instructions,
                prompt_file,
                allowed_users: None,
                accepted_anyone_risk: false,
                event_comments,
                git: config::GitConfig::default(),
            };
            entry.validate_launch_prefs()?;
            // herdr runs only the agents it recognises, so warn for a
            // repository that ends up there, by its own choice or the default.
            if cfg.driver_for(&entry) == config::DriverKind::Herdr {
                check_herdr_harness(&entry.harness);
            }
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
            cfg.save_to(config_file)?;
            println!("{action} {name} in {}", config_file.display());
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
            event_comments,
            git_name,
            git_email,
            git_signing_key,
            git_credential,
            clear,
        } => {
            let pos = cfg
                .repos
                .iter()
                .position(|x| x.name.eq_ignore_ascii_case(&name))
                .with_context(|| format!("{name} is not configured; use `ssf repo add`"))?;
            let default_driver = cfg.default_driver();
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
            if entry.driver.unwrap_or(default_driver) == config::DriverKind::Herdr {
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
            if event_comments.is_some() {
                entry.event_comments = event_comments;
            }
            if let Some(n) = git_name {
                entry.git.name = Some(n.trim().to_string());
            }
            if let Some(e) = git_email {
                entry.git.email = Some(e.trim().to_string());
            }
            if let Some(k) = git_signing_key {
                entry.git.signing_key = Some(parse_signing_key(&k));
            }
            if let Some(c) = git_credential {
                entry.git.credential = Some(c.trim().to_string());
            }
            for field in clear {
                match field.as_str() {
                    "git" => entry.git = config::GitConfig::default(),
                    "git.name" => entry.git.name = None,
                    "git.email" => entry.git.email = None,
                    "git.signing_key" => entry.git.signing_key = None,
                    "git.credential" => entry.git.credential = None,
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
                    "event_comments" => entry.event_comments = None,
                    other => bail!("cannot clear unknown field {other}"),
                }
            }
            entry.validate_launch_prefs()?;
            let updated = entry.name.clone();
            cfg.validate()?;
            let identity = cfg.git_identity(cfg.repos.get(pos));
            cfg.save_to(config_file)?;
            println!("Updated {updated}");
            if !identity.is_bot() || identity.credential != config::Credential::Bot {
                println!(
                    "{updated}: {}",
                    identity.describe(cfg.github.login.as_deref().unwrap_or("bot"))
                );
            }
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
                // The git identity per repository: [repo.git] over [git]
                // over the bot (`ssf doctor` checks the key and token).
                let bot = cfg.github.login.as_deref().unwrap_or("bot");
                println!();
                println!("# git identity (commits and pushes; gh is always the bot):");
                if cfg.repos.is_empty() {
                    println!("#   {}", cfg.git_identity(None).describe(bot));
                }
                for r in &cfg.repos {
                    println!(
                        "#   {}: {}",
                        r.name,
                        cfg.git_identity(Some(r)).describe(bot)
                    );
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
    // The startup wait was renamed; the file may hold either spelling, and
    // serde reads them as one field, so write the new name and drop the old.
    let parts: Vec<&str> = if key == "daemon.startup_orca_wait_secs" {
        vec!["daemon", "startup_driver_wait_secs"]
    } else {
        parts
    };
    let key = parts.join(".");
    let key = key.as_str();
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
        if key == "daemon.startup_driver_wait_secs" {
            cur.remove("startup_orca_wait_secs");
        }
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
    if (value.starts_with('[') || value.starts_with('{'))
        && let Ok(v) = toml::from_str::<toml::Table>(&format!("v = {value}"))
        && let Some(x) = v.get("v")
    {
        return x.clone();
    }
    toml::Value::String(value.to_string())
}

/// What the gate in front of a forwarded command does with it.
#[derive(Debug, PartialEq)]
enum Gate {
    /// Send it to the guest; `Some` is what to say on stderr first, when
    /// the answer was that there is no answer.
    Send(Option<String>),
    /// The VM is not running: this is why the command cannot run.
    Refuse(String),
}

/// The gate every command the host forwards into the guest goes through.
/// `probe` is [`vm::Vm::running_now`]'s answer or the reason there is
/// none, and `missing` what the backend needs and this host has not got.
///
/// Only a definite "not running" refuses. "Could not ask" is not an
/// answer to guess from: under lima the probe forks `limactl`, and one
/// fork that failed refused `tell`, `release`, `purge` and `doctor` over
/// a factory that was up, and had `status --json` -- the bar widget's
/// source -- report an idle one. The command goes to the guest instead,
/// to succeed or fail on its own terms, having said first why ssf cannot
/// tell and what the host is missing: otherwise a person whose `limactl`
/// is not installed at all would get nothing but an ssh error.
fn forwarding_gate(
    probe: &Result<bool, String>,
    vm_name: &str,
    backend: &str,
    cmd: &str,
    missing: Option<&str>,
) -> Gate {
    match probe {
        Ok(true) => Gate::Send(None),
        Ok(false) => Gate::Refuse(match missing {
            None => format!(
                "the factory runs in VM {vm_name}, which is not running; `ssf vm start` first"
            ),
            Some(detail) => format!(
                "the factory runs in VM {vm_name}, which is not running, and {backend} cannot start it: {detail}"
            ),
        }),
        Err(why) => {
            let mut note = format!("could not tell whether VM {vm_name} is running: {why}");
            if let Some(detail) = missing {
                note.push_str(&format!(
                    "\nand if it is down, {backend} cannot start it: {detail}"
                ));
            }
            // The refusal this replaces said what to do about a VM that
            // is down. An ssh failure says nothing of the sort, so the
            // advice comes here instead, before the command that may be
            // about to hit one.
            note.push_str(&format!(
                "\nsending `ssf {cmd}` to it anyway; if that fails on ssh the VM is down: `ssf vm start` starts it, `ssf vm status` says what the host can see"
            ));
            Gate::Send(Some(note))
        }
    }
}

/// The VM's state as the host knows it, for a `status --json` the guest
/// did not answer: what the probe said, including that it said nothing.
fn probe_word(probe: &Result<bool, String>) -> &'static str {
    match probe {
        Ok(true) => "running",
        Ok(false) => "stopped",
        Err(_) => "unknown",
    }
}

/// What `status --json` says for a guest the host could not reach. The
/// bar widget parses this and has no other source, so it is answered
/// rather than left empty; `vm` is the one field the host can still fill
/// in, and the sessions and repositories it could not ask after are
/// empty rather than invented.
fn vm_status_for_guest(vm: &str) -> serde_json::Value {
    serde_json::json!({
        "vm": vm, "service_active": false, "service_enabled": ui::service_enabled(),
        "sessions": [], "repos": [],
    })
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
        Command::Handover { .. } => "handover",
        Command::Purge { .. } => "purge",
        Command::Doctor => "doctor",
        Command::Run { once: true } => "run",
        _ => return None,
    };
    vm::forwards(name).then_some(name)
}

/// A guest's stderr already says why its command failed. For the one-shot
/// engine command, name the guest as well: from the host a person otherwise
/// cannot tell which daemon owns the refused state directory.
fn forwarded_failure_note(cmd: &Command, vm_name: &str, success: bool) -> Option<String> {
    (!success && matches!(cmd, Command::Run { once: true }))
        .then(|| format!("`ssf run --once` failed in VM {vm_name}"))
}

async fn vm_cmd(command: VmCommand) -> Result<()> {
    let cfg = Config::load()?;
    let vm = vm::Vm::new(&cfg);
    match command {
        VmCommand::Build {
            force,
            vcpus,
            mem_mib,
            data_gib,
        } => {
            let mut cfg = cfg;
            size_vm(&mut cfg, &vm.base, [vcpus, mem_mib, data_gib])?;
            vm::Vm::new(&cfg).build(&cfg, force).await
        }
        VmCommand::Grow { data_gib } => {
            if let Some(n) = vm.grow(data_gib)? {
                let mut cfg = cfg;
                cfg.vm.data_gib = Some(n);
                cfg.save()?;
                println!(
                    "[vm] data_gib = {n} written to {}",
                    config::config_path().display()
                );
            }
            Ok(())
        }
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
                print!("{}", render_vm_status(&st));
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
            let log = vm.console_path()?;
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
                    "this removes {} and everything in it{}; pass --yes",
                    vm.dir.display(),
                    match vm.backend() {
                        vm::BackendKind::Lima => format!(
                            ", the lima instance {} and its disk {}",
                            vm.lima_name(),
                            vm.lima_disk_name()
                        ),
                        vm::BackendKind::Firecracker => String::new(),
                    }
                );
            }
            vm.destroy().await
        }
    }
}

/// `ssf doctor`'s lines about what this configuration does not name.
///
/// A function for the reason `render_vm_status` is one: `main` is not
/// reachable from a test, and the twin of these three lines in
/// `ssf vm status` hid a must-fix in three consecutive rounds. This copy
/// then hid the same one -- naming `[vm] dir` for a fact about lima's
/// home -- because only the other copy had been extracted.
///
/// What that pins is the text. The `print!` that puts it on a terminal
/// is in `doctor` and is reachable by nothing: removing it makes
/// `ssf doctor` silent about a stray it found, with the suite green.
/// The same is true at four sibling sites: `uninstall::run`'s three
/// prints and `render_vm_status`'s. No issue number here on purpose --
/// #188 tracked it and was consolidated, #158 tracks it and closes
/// with this change, and a pointer that closes reads as a gap that was
/// fixed. The gap is the sentence above, which stays true until a seam
/// makes these bodies reachable.
pub fn stray_notes(strays: &[vm::Stray]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for stray in strays {
        let _ = writeln!(out, "note {}", stray.describe());
    }
    out
}

/// `ssf vm status` as a person reads it.
///
/// A function, because `main` is not reachable from a test and this
/// block has hidden a must-fix in three separate gauntlet rounds: a
/// missing ordering caveat, a sentence that did not use the shared one,
/// and a line naming the wrong directory. The words are pinned where
/// they can be -- but not the `print!` that shows them, which can be
/// removed for a silent `ssf vm status` without a test going red.
pub fn render_vm_status(st: &vm::VmStatus) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "vm:       {} ({}){}",
        st.name,
        st.dir,
        if st.enabled {
            ""
        } else {
            "  [vm] enabled = false"
        }
    );
    let _ = writeln!(out, "backend:  {}", st.backend);
    if let Some(t) = &st.tooling {
        let _ = writeln!(out, "tooling:  {}", t.detail);
    }
    match &st.instance {
        // A `limactl list` that failed is not "no such
        // instance": saying "missing (ssf vm build)" over a
        // VM lima could not be asked about sends people to
        // rebuild one that is already there.
        Some(inst) => {
            let _ = writeln!(
                out,
                "instance: {inst}{}",
                match (&st.probe_error, &st.lima_dir, st.image) {
                    (Some(e), ..) => format!(" unknown: {e}"),
                    (None, Some(d), _) => format!(" ({d})"),
                    (None, None, false) => " missing (ssf vm build)".to_string(),
                    (None, None, true) => String::new(),
                }
            );
        }
        None => {
            let _ = writeln!(
                out,
                "image:    {}",
                if st.image {
                    "built"
                } else {
                    "missing (ssf vm build)"
                }
            );
        }
    }
    let _ = writeln!(
        out,
        "state:    {}",
        match (&st.probe_error, st.running, st.firecracker_pid) {
            (Some(_), ..) => "unknown (lima did not answer)".to_string(),
            (None, Some(true), Some(p)) => format!("running (firecracker pid {p})"),
            (None, Some(true), None) => "running".to_string(),
            _ => "stopped".to_string(),
        }
    );
    for stray in &st.strays {
        let _ = writeln!(out, "stray:    {}", stray.describe());
    }
    let _ = writeln!(
        out,
        "ssh:      {}",
        if st.ssh {
            format!("127.0.0.1:{} answers", st.ssh_port)
        } else {
            "not reachable".to_string()
        }
    );
    let _ = writeln!(
        out,
        "daemon:   {}",
        st.daemon.as_deref().unwrap_or("unknown")
    );
    let _ = writeln!(
        out,
        "size:     {} vCPUs, {} MiB; data disk {} GiB{}",
        st.vcpus,
        st.mem_mib,
        st.data_gib,
        match &st.data {
            Some(d) => format!(
                ", {}{}",
                d.describe(),
                if d.is_full() { "; `ssf vm grow`" } else { "" }
            ),
            None => String::new(),
        }
    );
    if !st.logins.is_empty() {
        let _ = writeln!(out, "logins:   {}", login_summary(&st.logins));
    }
    out
}

fn exit_with(st: std::process::ExitStatus) -> Result<()> {
    std::process::exit(st.code().unwrap_or(1));
}

/// `ssf vm build`'s sizing: a `--vcpus/--mem-mib/--data-gib` flag is
/// written to `[vm]`; a key set there stays; a key set nowhere gets the
/// rule for this machine and is written too. The choice is printed with
/// where each value came from. `[vm] backend` is settled the same way
/// (the platform's default, written once).
fn size_vm(cfg: &mut Config, base: &Path, flags: [Option<u32>; 3]) -> Result<()> {
    let (backend, backend_from, backend_changed) =
        vm::choose_backend(&mut cfg.vm, vm::BackendKind::platform_default());
    println!("VM backend: {backend} ({backend_from})");
    // The backend decides which filesystem the data disk will fill, so it
    // has to be settled before the machine is measured.
    let (dir, what) = vm::sizing_dir(backend, base);
    let facts = vm::HostFacts::probe(&dir)?;
    let mut chosen = vm::choose_sizes(&mut cfg.vm, flags, vm::sizes_for(&facts));
    chosen.changed |= backend_changed;
    println!(
        "this machine: {} CPUs, {} MiB RAM, {} GiB free on {} (measured at {}, {what})",
        facts.cpus,
        facts.mem_mib,
        facts.free_bytes >> 30,
        facts.mount,
        dir.display(),
    );
    println!(
        "VM size: {} vCPUs ({}), {} MiB RAM ({}), {} GiB data disk ({}; sparse, so it takes host space only as the guest writes)",
        chosen.sizes.vcpus,
        chosen.sources[0],
        chosen.sizes.mem_mib,
        chosen.sources[1],
        chosen.sizes.data_gib,
        chosen.sources[2],
    );
    if chosen.changed {
        cfg.save()?;
        println!(
            "written to {} under [vm] (backend, vcpus, mem_mib, data_gib); edit them there. The data disk itself is made once and only enlarged by `ssf vm grow`",
            config::config_path().display()
        );
    }
    Ok(())
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
    let me = identity(None)?.map(|o| o.to_string());
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

/// This session's identity: `--as owner/repo#N`, else the environment
/// `ssf launch` set up.
fn identity(as_: Option<&str>) -> Result<Option<origin::Origin>> {
    match as_ {
        Some(a) => origin::Origin::parse(a)
            .map(Some)
            .with_context(|| format!("--as {a}: expected owner/repo#N")),
        None => Ok(origin::Origin::from_env()),
    }
}

/// An item or session argument: `owner/repo#N`, or a bare number on `me`'s
/// repository.
fn item_ref(item: &str, me: Option<&origin::Origin>) -> Result<String> {
    let item = item.trim().trim_start_matches('#');
    if let Ok(n) = item.parse::<u64>() {
        return match me {
            Some(o) => Ok(format!("{}#{n}", o.repo)),
            None => {
                bail!("{item}: pass owner/repo#{item}, or --as owner/repo#N to name the repository")
            }
        };
    }
    match origin::Origin::parse(item) {
        Some(o) => Ok(o.to_string()),
        None => bail!("{item}: expected an item number or owner/repo#N"),
    }
}

async fn sub(item: &str, as_: Option<&str>, json: bool, subscribe: bool) -> Result<()> {
    let me = identity(as_)?.context(
        "not inside an agent session (SSF_REPO/SSF_ISSUE unset); pass --as owner/repo#N",
    )?;
    let target = item_ref(item, Some(&me))?;
    let me = me.to_string();
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
    let me = identity(as_)?.context(
        "not inside an agent session (SSF_REPO/SSF_ISSUE unset); pass --as owner/repo#N",
    )?;
    // The subscriber is always the owning session.
    let me_id = st
        .repos
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
        .unwrap_or_else(|| me.to_string());
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
    let target = item_ref(item, me.as_ref())?;
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
        from: me.map(|o| o.to_string()),
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
        Some(i) => item_ref(i, me.as_ref())?,
        None => me
            .as_ref()
            .map(|o| o.to_string())
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

/// `1,234`: a count as the messages write it.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What `ssf handover` prints once the daemon has recorded it. The second
/// paragraph is what the outgoing agent acts on, so it says plainly that
/// this session is over. Worded, like every text ssf puts on a screen,
/// without the phrases `driver::login_dialog` looks for.
#[allow(clippy::too_many_arguments)]
pub fn handover_recorded_text(
    session: &str,
    title: &str,
    harness_name: &str,
    model: Option<&str>,
    effort: Option<&str>,
    command: Option<&str>,
    summary_chars: Option<usize>,
    secs: u64,
) -> String {
    // What decides an unset model or effort, in the words the
    // `handed-over` post uses for it (`events::Launch`).
    let default = match command {
        Some(_) => "the command's",
        None => "the harness's default",
    };
    let model = match model {
        Some(m) => format!("model {m}"),
        None => format!("{default} model"),
    };
    let effort = match effort {
        Some(e) => format!("effort {e}"),
        None => format!("{default} effort"),
    };
    let summary = match summary_chars {
        Some(n) => format!("with a summary of {} chars", thousands(n)),
        None => "without a summary".to_string(),
    };
    format!(
        "Handover of {session} (\"{title}\") recorded: to {harness_name} ({model}, {effort}), \
{summary}.\nThe daemon ends this session on its next pass (within {secs}s) and starts the new \
one in the same workspace. Stop working now: do not start anything else, and do not run this \
command again."
    )
}

/// Why a summary that quotes a harness's sign-in screen is refused. The
/// summary is pasted into the new session's terminal, where ssf reads
/// the bottom of the screen for exactly those phrases, so such a summary
/// would hold the new session's deliveries for the whole backoff. `line`
/// is the offending line with the phrases already redacted
/// (`driver::redact_login_phrases`), so this text is safe on a screen
/// itself.
pub fn summary_quotes_a_sign_in_screen_text(line: &str) -> String {
    format!(
        "the summary would read as a harness's own sign-in screen where it says \"{line}\" (the \
phrase is left out here): pasted into the new session's terminal it would hold that session's \
deliveries. Reword that line -- name the command in prose rather than quoting the screen -- and \
hand over again."
    )
}

/// The summary a handover carries: `--summary`, the contents of
/// `--summary-file`, or nothing for `--no-summary`. Checked here, where
/// the person or agent that wrote it can fix it, rather than in the daemon.
fn handover_summary(
    summary: Option<String>,
    file: Option<&Path>,
    no_summary: bool,
) -> Result<Option<String>> {
    let text = match (summary, file) {
        (Some(t), _) => t,
        (None, Some(p)) => std::fs::read_to_string(p)
            .with_context(|| format!("reading the summary from {}", p.display()))?,
        (None, None) if no_summary => return Ok(None),
        (None, None) => bail!(
            "say what the new session is told: --summary \"<text>\", --summary-file <path>, or \
--no-summary"
        ),
    };
    if text.trim().is_empty() {
        bail!("the summary is empty: write a summary or pass --no-summary");
    }
    let n = text.chars().count();
    if n > ipc::MAX_SUMMARY_CHARS {
        bail!(
            "the summary is {} characters; the most a handover carries is {}. Shorten it, or say \
the rest on the item",
            thousands(n),
            thousands(ipc::MAX_SUMMARY_CHARS)
        );
    }
    if let Some(line) = driver::login_prompt_line(&text) {
        bail!(
            "{}",
            summary_quotes_a_sign_in_screen_text(&driver::redact_login_phrases(&line))
        );
    }
    Ok(Some(text))
}

#[allow(clippy::too_many_arguments)]
async fn handover(
    item: Option<&str>,
    cancel: bool,
    harness: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
    summary: Option<String>,
    summary_file: Option<&Path>,
    no_summary: bool,
    as_: Option<&str>,
    json: bool,
) -> Result<()> {
    let me = identity(as_)?;
    let session = match item {
        Some(i) => item_ref(i, me.as_ref())?,
        None => me
            .as_ref()
            .map(|o| o.to_string())
            .context("not inside an agent session (SSF_REPO/SSF_ISSUE unset); name the item, or pass --as owner/repo#N")?,
    };
    if cancel {
        return cancel_handover(&session, json).await;
    }
    let harness = harness.context("--harness is required")?.trim();
    if !agents::is_known(harness) {
        bail!(
            "{harness} is not a harness ssf knows; `ssf agents` lists the ids ({})",
            agents::list()
                .iter()
                .map(|a| a.id.clone())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    models::validate(harness, model, effort)?;
    let summary = handover_summary(summary, summary_file, no_summary)?;
    let v = ipc::call(&ipc::Request::Handover {
        session,
        harness: harness.to_string(),
        model: model.map(str::to_string),
        effort: effort.map(str::to_string),
        summary,
        by: me.map(|o| o.to_string()),
    })
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let s = |p: &str| v.pointer(p).and_then(|x| x.as_str()).map(str::to_string);
    println!(
        "{}",
        handover_recorded_text(
            v.get("session").and_then(|x| x.as_str()).unwrap_or("?"),
            v.get("title").and_then(|x| x.as_str()).unwrap_or(""),
            &login::display_name(s("/to/harness").as_deref().unwrap_or(harness)),
            s("/to/model").as_deref(),
            s("/to/effort").as_deref(),
            s("/to/command").as_deref(),
            v.get("summary_chars")
                .and_then(|x| x.as_u64())
                .map(|n| n as usize),
            v.get("poll_interval_secs")
                .and_then(|n| n.as_u64())
                .unwrap_or(10),
        )
    );
    Ok(())
}

/// `ssf handover --cancel`: the pending handover is dropped and the
/// session that is there keeps the item.
async fn cancel_handover(session: &str, json: bool) -> Result<()> {
    let v = ipc::call(&ipc::Request::CancelHandover {
        session: session.to_string(),
    })
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    println!(
        "{}",
        handover_cancelled_text(
            &s("session"),
            &s("title"),
            &s("harness_name"),
            v.get("told").and_then(|x| x.as_bool()).unwrap_or(false),
        )
    );
    Ok(())
}

/// What `ssf handover --cancel` prints. Worded, like every text ssf puts
/// on a screen, without the phrases `driver::login_dialog` looks for.
pub fn handover_cancelled_text(session: &str, title: &str, harness: &str, told: bool) -> String {
    let told = if told {
        " The session on it has been told to carry on."
    } else {
        ""
    };
    format!(
        "Handover of {session} (\"{title}\") to {harness} cancelled; nothing about the item \
changed.{told}"
    )
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
        let workspace_gone = s("workspace") == "gone";
        println!(
            "{:<13} {} \"{}\"  [{}]{}{}  {}",
            verb,
            s("session"),
            status::one_line(&title, 50),
            s("state"),
            if workspace_gone {
                " (workspace gone, checkout still on disk)"
            } else {
                ""
            },
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

/// Does this doctor report the VM backend's host tooling?
///
/// Only a host doctor does, and only as a note. `doctor` is a forwarded
/// command ([`forwarded_name`]), so with `[vm] enabled` and the VM
/// running it is the guest that answers -- and the guest has no limactl
/// or `/dev/kvm` of its own to report on -- while with the VM stopped
/// `main` bails before doctor runs, naming the missing tooling itself
/// ("... cannot start it: ..."). So a doctor that reaches this line is
/// running the factory here, on this machine, where the backend's tooling
/// is not in use: nothing is broken by its absence, it is what turning
/// `[vm] enabled` on would need. The ok/FAIL judgement on it belongs to
/// the two places that do depend on it, `ssf vm status` and that bail.
fn reports_backend_tooling(in_guest: bool) -> bool {
    !in_guest
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
        let herdr = d.kind() == config::DriverKind::Herdr;
        // herdr may live in ~/.local/bin (the herdr.dev installer's default),
        // which the systemd user manager's PATH does not include.
        let cmd = if herdr {
            config::herdr_command_path(d.command())
        } else {
            std::path::PathBuf::from(d.command())
        };
        let cmd = cmd.to_string_lossy().into_owned();
        let cli_present = std::path::Path::new(&cmd).exists() || which(&cmd).is_some();
        check(
            cli_present,
            if !cli_present && herdr {
                format!(
                    "{} driver: CLI `{}` not found; install it: {}",
                    d.label(),
                    d.command(),
                    platform::herdr_install_hint()
                )
            } else {
                format!("{} driver: CLI at {}", d.label(), cmd)
            },
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
    let retired = cfg.daemon.retired_keys();
    if !retired.is_empty() {
        println!(
            "note {} in config.toml no longer {} anything: the `review` label and reviewer sessions went with one session per item; remove the line",
            retired.join(" and "),
            if retired.len() == 1 { "does" } else { "do" }
        );
    }
    let state = state::State::load().unwrap_or_default();
    // Harnesses no repository is configured with, because an item was
    // handed over to one (`ssf handover`): its session runs that harness
    // where the daemon runs, so it is checked like the configured ones,
    // and the line says which item put it there.
    let mut handed_over: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for (name, rs) in &state.repos {
        for it in rs.issues.values().filter(|i| i.active) {
            let Some(o) = it.overrides.as_ref() else {
                continue;
            };
            if cfg.repos.iter().any(|r| r.harness == o.harness) {
                continue;
            }
            handed_over
                .entry(o.harness.clone())
                .or_default()
                .push(format!("{name}#{}", it.number));
        }
    }
    let used_by = |h: &str| match handed_over.get(h) {
        Some(items) => format!("; used by {} after a handover", items.join(", ")),
        None => String::new(),
    };
    // Each harness a repository uses, signed in where this runs (the host,
    // or the guest: with the factory in a VM `ssf doctor` is forwarded
    // there, so the check happens where the agents are).
    let mut harnesses: Vec<String> = cfg.repos.iter().map(|r| r.harness.clone()).collect();
    harnesses.extend(handed_over.keys().cloned());
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
            login::LoginState::SignedIn => check(
                true,
                format!("{name} signed in {place} ({}{})", probe.detail, used_by(h)),
            ),
            login::LoginState::SignedOut => check(
                false,
                format!(
                    "{name} not signed in {place} ({}{}); sign in with {}, or sessions on it stall at its login prompt",
                    probe.detail,
                    used_by(h),
                    login::how_to_sign_in(h)
                ),
            ),
            login::LoginState::Unknown => {
                println!(
                    "note {name}: cannot tell whether it is signed in {place} ({}{})",
                    probe.detail,
                    used_by(h)
                )
            }
        }
    }
    // In the guest: the data disk and the memory, which only show from
    // inside (the data disk is a sparse file on the host; the guest has
    // no swap, so short memory means OOM kills, not slowness).
    if vm::in_guest() {
        match vm::disk_use(Path::new(vm::GUEST_DATA_DIR)) {
            Ok(d) => check(
                !d.is_full(),
                format!(
                    "data disk {}{}",
                    d.describe(),
                    if d.is_full() {
                        // This runs in the guest, which does not know the
                        // host's OS: both hints.
                        format!(
                            "; grow it from the host: stop the VM (`{}`, `{}` on macOS, or `ssf vm stop` when it was started by hand), `ssf vm grow`, start it again",
                            platform::service_hint_for("linux", "stop"),
                            platform::service_hint_for("macos", "stop")
                        )
                    } else {
                        String::new()
                    }
                ),
            ),
            Err(e) => println!("note data disk: {e:#}"),
        }
        if let Some(m) = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|t| vm::parse_meminfo(&t))
        {
            check(
                !m.is_short(),
                format!(
                    "guest memory: {}{}",
                    m.describe(),
                    if m.is_short() {
                        "; raise vm.mem_mib in config.toml on the host and `ssf vm restart`"
                    } else {
                        ""
                    }
                ),
            );
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
    // The harnesses handovers put on items, installed where the daemon
    // runs: no repository names them, so nothing else here would look.
    for (h, items) in &handed_over {
        let bin = models::default_command(h);
        let bin = bin.split_whitespace().next().unwrap_or("");
        let ok = which(bin).is_some() || installed.iter().any(|a| a.id == *h && a.installed);
        check(
            ok,
            format!(
                "harness `{h}` installed (used by {} after a handover)",
                items.join(", ")
            ),
        );
    }
    // What each driver has open, once, for the worktree lines below: a
    // worktree with no agent in its workspace (or no workspace at all)
    // is what those report.
    let mut open_workspaces: std::collections::BTreeMap<
        config::DriverKind,
        Result<Vec<orca::WorkspaceInfo>>,
    > = Default::default();
    for d in driver::Drivers::from_config(&cfg).iter() {
        open_workspaces.insert(d.kind(), d.ps().await);
    }
    // Where a person starts their SSF.md from: the packages put it in
    // /usr/share/ssf, Homebrew under its own prefix.
    let example_notes = platform::share_file("SSF.example.md");
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
        // The project notes (`SSF.md`, or `repo.prompt_file`): looked for
        // on GitHub, on the branch the agents start from (`repo.base_branch`,
        // else the default branch), so no clone is needed; a machine path
        // is looked for here.
        let notes = r.prompt_file();
        let notes_path = config::expand_tilde(notes);
        if notes_path.is_absolute() {
            let present = notes_path.exists();
            check(
                present,
                format!(
                    "{}: project notes at {} {}",
                    r.name,
                    notes_path.display(),
                    if present {
                        "present".to_string()
                    } else {
                        format!("missing; start from {}", example_notes.display())
                    }
                ),
            );
        } else {
            match (&gh, r.split()) {
                (Some(gh), Ok((owner, name))) => {
                    match gh
                        .has_file(owner, name, notes, r.base_branch.as_deref())
                        .await
                    {
                        Ok(true) => check(true, format!("{}: project notes ({notes})", r.name)),
                        Ok(false) => check(
                            false,
                            format!(
                                "no {notes} in {}{}; start from {}",
                                r.name,
                                match &r.base_branch {
                                    Some(b) => format!(" on branch {b} (does the branch exist?)"),
                                    None => String::new(),
                                },
                                example_notes.display()
                            ),
                        ),
                        Err(e) => check(
                            false,
                            format!(
                                "{}: project notes ({notes}) could not be checked: {e:#}",
                                r.name
                            ),
                        ),
                    }
                }
                _ => check(
                    false,
                    format!(
                        "{}: project notes ({notes}) cannot be checked without a token",
                        r.name
                    ),
                ),
            }
        }
        // The configured checkout, `~` expanded as the daemon expands it.
        let configured_missing = r
            .path
            .as_deref()
            .is_some_and(|p| !config::expand_tilde(p).join(".git").exists());
        if let Some(p) = &r.path {
            check(!configured_missing, format!("{}: checkout at {p}", r.name));
        }
        // The worktrees its sessions work in, and what each holds that is
        // on no other branch and not on origin, when no agent is on it: a
        // workspace closed by hand leaves the checkout behind, and nothing
        // else says that removing it would lose work.
        // A configured path that is missing was flagged just above.
        match checkout_root(&cfg, r, &state) {
            None if configured_missing => {}
            None => check(
                true,
                format!(
                    "{}: no checkout yet (the first session clones it under {})",
                    r.name,
                    cfg.projects_dir(cfg.driver_for(r)).display()
                ),
            ),
            Some(root) => match release::held_work(&root, r.base_branch.as_deref()).await {
                Err(e) => println!(
                    "note {}: worktrees of {} could not be checked: {e:#}",
                    r.name, root
                ),
                Ok(report) => {
                    let stale = match &report.fetch_error {
                        Some(e) => format!(
                            " (origin not fetched, so the counts may be stale: {})",
                            status::one_line(e, 80)
                        ),
                        None => String::new(),
                    };
                    // Every driver's workspaces, not only this repository's
                    // driver's: after a driver switch the old driver may
                    // still have an agent on a worktree here.
                    let rows: Vec<&orca::WorkspaceInfo> = open_workspaces
                        .values()
                        .filter_map(|r| r.as_ref().ok())
                        .flatten()
                        .collect();
                    let driver_down = open_workspaces.values().any(|r| r.is_err());
                    // (line, item is active)
                    let stranded: Vec<(String, bool)> = report
                        .worktrees
                        .iter()
                        .filter(|h| h.at_risk())
                        .filter_map(|h| {
                            let ws = match rows.iter().find(|w| same_path(&w.path, &h.path)) {
                                Some(w) if !w.agents.is_empty() => return None,
                                Some(_) => "workspace open, no agent in it",
                                None if driver_down => {
                                    "cannot tell whether an agent is on it (a driver is not answering)"
                                }
                                None => "no workspace",
                            };
                            let record = driver::number_of_name(&h.name).map(|n| {
                                (
                                    n,
                                    state
                                        .repos
                                        .get(&r.name)
                                        .and_then(|rs| rs.issues.get(&n))
                                        .map(|it| it.active),
                                )
                            });
                            let item = match record {
                                Some((n, Some(true))) => format!("#{n} active"),
                                Some((n, Some(false))) => format!("#{n} retired"),
                                Some((n, None)) => format!("#{n} not on record"),
                                None => "no item".to_string(),
                            };
                            Some((
                                format!("{}: {}; {ws}; {item}", h.name, h.describe(&report.base)),
                                matches!(record, Some((_, Some(true)))),
                            ))
                        })
                        .collect();
                    let total = report.worktrees.len();
                    let plural = |n: usize| if n == 1 { "" } else { "s" };
                    if stranded.is_empty() {
                        check(
                            true,
                            if total == 0 {
                                format!("{}: no worktrees under {}{stale}", r.name, report.dir)
                            } else {
                                format!(
                                    "{}: {total} worktree{} under {}; none holds work that is only there without an agent on it{stale}",
                                    r.name,
                                    plural(total),
                                    report.dir
                                )
                            },
                        );
                    } else {
                        println!(
                            "WARN {}: {} of {total} worktree{} under {} hold{} work only it has (commits on no other branch and not on origin, uncommitted changes, a stash), with no agent on it{stale}:",
                            r.name,
                            stranded.len(),
                            plural(total),
                            report.dir,
                            if stranded.len() == 1 { "s" } else { "" }
                        );
                        for (line, _) in &stranded {
                            println!("              - {line}");
                        }
                        // What to do depends on the item: a tell reaches an
                        // active one and brings its session back in the
                        // checkout; a retired one refuses a tell, so its
                        // branch is pushed by hand.
                        if stranded.iter().any(|(_, active)| *active) {
                            println!(
                                "              an active item: `ssf tell <item> \"...\"` brings its session back in that checkout"
                            );
                        }
                        if stranded.iter().any(|(_, active)| !*active) {
                            println!(
                                "              a retired item, or none: push the branch by hand (`git -C <checkout> push -u origin <branch>`), or look and decide"
                            );
                        }
                        println!(
                            "              `ssf purge --force` or removing the directory loses the uncommitted changes and leaves the commits on a local branch nothing lists (a detached HEAD's go too)"
                        );
                    }
                }
            },
        }
        // The git identity its agents commit and push with, and whether
        // what it needs (a key, a token) is here where the agents run.
        let identity = cfg.git_identity(Some(r));
        check(
            identity.name.is_some(),
            format!("{}: {}", r.name, identity.describe(&bot)),
        );
        if let Some(key) = &identity.signing_key {
            check(
                key.exists(),
                format!(
                    "{}: signing key {} {}",
                    r.name,
                    key.display(),
                    if key.exists() {
                        "present"
                    } else {
                        "missing (commits would go out unsigned)"
                    }
                ),
            );
        }
        match &identity.credential {
            config::Credential::Token(login) => {
                let host = cfg.github.git_host();
                match ghcli::token_for(&host, login) {
                    Ok(_) => check(
                        true,
                        format!("{}: gh holds a token for @{login} {place}", r.name),
                    ),
                    Err(e) => check(
                        false,
                        format!(
                            "{}: no token for @{login} {place} ({e:#}); sign @{login} in to gh here, or use file:<path>",
                            r.name
                        ),
                    ),
                }
            }
            config::Credential::File(path) => check(
                path.exists(),
                format!(
                    "{}: token file {} {}",
                    r.name,
                    path.display(),
                    if path.exists() { "present" } else { "missing" }
                ),
            ),
            _ => {}
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
    let st = &state;
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
            platform::service_name(),
            if ui::service_enabled() {
                ""
            } else {
                " (disabled by `ssf ui service disable`)"
            }
        ),
    );
    // The tooling the VM backend needs, on the host that would run it.
    // Nothing here uses it (see `reports_backend_tooling`), so a missing
    // limactl is not a failure: it is what to install before turning the
    // VM on.
    if reports_backend_tooling(vm::in_guest()) {
        let vm = vm::Vm::new(&cfg);
        println!(
            "note {} backend: {}{}",
            vm.backend(),
            vm.tooling().detail,
            if cfg.vm.enabled {
                ""
            } else {
                "; [vm] enabled is false, so nothing here needs it until you turn the VM on"
            }
        );
        // What a changed `[vm] name` left behind. Nothing ssf runs will
        // remove one -- it cannot tell one kept on purpose from one
        // abandoned -- so being named here is the only way they stop
        // being invisible.
        //
        // Off the filesystem, always. Asking the backend would add two
        // `limactl` forks, each bounded at `SURVEY_LIMIT` -- a minute
        // apiece, so two of silence for the person whose lima is
        // wedged, who is exactly the person running `doctor`. It forks
        // the service manager already -- `systemctl` on Linux,
        // `launchctl` on the Mac this sentence is about -- so the point
        // is not that it forks nothing; it is that it has never waited
        // on lima.
        // What the listing would buy under lima is the suppression of a
        // directory lima has disowned, a cost `strays_on_disk_read`
        // already accepts in its own doc: naming one costs a line, not
        // a VM. Under Firecracker the two are the same answer by
        // construction.
        print!("{}", stray_notes(&vm.strays_on_filesystem()));
    }
    // The widget lives on the host; inside the guest there is no Omarchy
    // shell to check.
    if vm::in_guest() {
        println!("note bar widget: checked on the host, not inside the VM");
    } else if !platform::is_omarchy() {
        println!("note bar widget: not on Omarchy, nothing to enable");
    } else {
        check(
            ui::widget_enabled().unwrap_or(false),
            "bar widget enabled in ~/.config/omarchy/shell.json".into(),
        );
    }
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

/// The checkout `ssf doctor` looks for a repository's worktrees next to:
/// the configured path, else what the state remembers of its sessions
/// (herdr's repo id is the checkout; any driver's worktree path sits
/// under `<checkout>.worktrees/`), else where the driver would clone it.
fn checkout_root(cfg: &Config, r: &RepoConfig, state: &state::State) -> Option<String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = &r.path {
        candidates.push(config::expand_tilde(p));
    }
    if let Some(rs) = state.repos.get(&r.name) {
        for it in rs.issues.values() {
            if let Some(p) = &it.worktree_path
                && let Some(root) = driver::checkout_of_worktree(p)
            {
                candidates.push(root);
            }
            if let Some(id) = &it.repo_id
                && id.starts_with('/')
            {
                candidates.push(PathBuf::from(id));
            }
        }
    }
    if let Ok((_, name)) = r.split() {
        candidates.push(cfg.projects_dir(cfg.driver_for(r)).join(name));
    }
    candidates
        .into_iter()
        .find(|p| p.join(".git").exists())
        .map(|p| p.to_string_lossy().to_string())
}

/// The same directory, whichever way each side spells it.
fn same_path(a: &str, b: &str) -> bool {
    let canon = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
    a == b || canon(a) == canon(b)
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

    /// A `VmStatus` with nothing interesting in it, to vary one field
    /// at a time.
    fn status() -> vm::VmStatus {
        vm::VmStatus {
            enabled: true,
            name: "new".into(),
            dir: "/v/new".into(),
            backend: "lima".into(),
            instance: Some("ssf-new".into()),
            lima_dir: None,
            image: false,
            running: Some(false),
            firecracker_pid: None,
            gvproxy_pid: None,
            ssh_port: 2222,
            ssh: false,
            daemon: None,
            logins: Vec::new(),
            vcpus: 2,
            mem_mib: 4096,
            data_gib: 20,
            data: None,
            tooling: None,
            probe_error: None,
            strays: Vec::new(),
        }
    }

    #[test]
    fn vm_status_says_a_stray_is_left_alone() {
        // The half that went missing when this printer had its own
        // wording: "ssf leaves it alone" is what the design turns on,
        // and `config.example.toml` and `docs/vm.md` both promise it.
        //
        // Two of them, because the canonical #158 case leaves exactly
        // two -- an instance and its disk -- and `sort_strays` puts
        // disks last. Printing only the first kept the debris and
        // dropped the data disk, which is the one holding the clones
        // and the only one whose line says so. One stray in the fixture
        // could not tell the difference.
        let st = vm::VmStatus {
            strays: vec![
                vm::Stray::lima_instance("ssf-old".into(), &Default::default()),
                vm::Stray::lima_disk("ssf-old".into(), &Default::default()),
            ],
            ..status()
        };
        let text = render_vm_status(&st);
        assert!(text.contains("ssf leaves it alone"), "{text}");
        assert!(text.contains("limactl delete ssf-old"), "{text}");
        assert!(text.contains("limactl disk delete ssf-old"), "{text}");
        assert!(text.contains("after its instance"), "{text}");
        // `ssf vm status` is a column of `label:   value` lines; a
        // stray without its label reads as part of the row above it.
        assert!(text.contains("\nstray:    lima also holds"), "{text}");
        assert!(
            text.contains("(its clones and worktrees are in it)"),
            "the disk's own line, which is the one that says what is at stake: {text}"
        );
    }

    #[test]
    fn a_failed_forwarded_one_shot_names_its_guest() {
        let once = Command::Run { once: true };
        assert_eq!(
            forwarded_failure_note(&once, "factory", false).as_deref(),
            Some("`ssf run --once` failed in VM factory")
        );
        assert!(forwarded_failure_note(&once, "factory", true).is_none());
        assert!(forwarded_failure_note(&Command::Doctor, "factory", false).is_none());
    }

    #[test]
    fn doctor_notes_name_every_stray_not_just_the_first() {
        // The twin of `render_vm_status`'s lines. Only that copy was
        // extracted last round, so this one went on naming `[vm] dir`
        // for a fact about lima's home with nothing to catch it.
        // Both strays, for the reason `render_vm_status`'s twin takes
        // both: the disk sorts last, so a printer that stops after one
        // drops the clones and keeps the debris.
        let text = stray_notes(&[
            vm::Stray::lima_instance("ssf-old".into(), &Default::default()),
            vm::Stray::lima_disk("ssf-old".into(), &Default::default()),
        ]);
        assert!(text.contains("ssf leaves it alone"), "{text}");
        assert!(text.contains("limactl delete ssf-old"), "{text}");
        assert!(text.contains("limactl disk delete ssf-old"), "{text}");
        assert!(
            text.contains("(its clones and worktrees are in it)"),
            "{text}"
        );
        // `doctor`'s own prefix: its output is a list of `note`/`warn`
        // lines and a stray that arrives without one reads as prose.
        assert!(text.starts_with("note "), "{text}");
        assert_eq!(stray_notes(&[]), "");
    }

    #[test]
    fn the_backend_tooling_is_a_note_on_the_host_and_nothing_in_the_guest() {
        // Why it is only ever a note: `doctor` is forwarded, so a factory
        // in a running VM answers doctor from the guest...
        assert_eq!(forwarded_name(&Command::Doctor), Some("doctor"));
        assert!(vm::forwards("doctor"));
        assert!(!reports_backend_tooling(true));
        // ...and a factory in a stopped VM never gets here at all: `main`
        // bails on any forwarded command but `status`, and that bail is
        // itself what names the tooling a host has not got. So the doctor
        // that prints this line is one running the factory on this
        // machine, where the backend is not in use and cannot fail.
        assert!(reports_backend_tooling(false));
    }

    #[test]
    fn the_machine_name_comes_from_whichever_source_this_os_has() {
        // Linux: /etc/hostname, exactly as before.
        assert_eq!(pick_hostname(Some("box\n".into()), || None, || None), "box");
        // macOS has no /etc/hostname; the name a person gave the Mac
        // comes first, `hostname` after it. Neither must be allowed to
        // leave the key labelled "ssf on localhost".
        assert_eq!(
            pick_hostname(
                None,
                || Some("Mike's MacBook Pro\n".into()),
                || Some("mikes-mbp.local\n".into())
            ),
            "Mike's MacBook Pro"
        );
        assert_eq!(
            pick_hostname(None, || None, || Some("mikes-mbp.local\n".into())),
            "mikes-mbp.local"
        );
        // Empty answers count as no answer.
        assert_eq!(
            pick_hostname(
                Some("  \n".into()),
                || Some("".into()),
                || Some(" mac \n".into())
            ),
            "mac"
        );
        assert_eq!(pick_hostname(None, || None, || None), "localhost");
    }

    #[test]
    fn only_a_definite_no_keeps_a_command_out_of_the_guest() {
        // A guest that is up takes the command, with nothing said.
        assert_eq!(
            forwarding_gate(&Ok(true), "default", "lima", "tell", None),
            Gate::Send(None)
        );
        // A guest that is down does not, and the refusal names what the
        // host has not got when that is why it cannot be started.
        assert_eq!(
            forwarding_gate(&Ok(false), "default", "lima", "tell", None),
            Gate::Refuse(
                "the factory runs in VM default, which is not running; `ssf vm start` first".into()
            )
        );
        let Gate::Refuse(why) = forwarding_gate(
            &Ok(false),
            "default",
            "lima",
            "doctor",
            Some("limactl not installed; install lima"),
        ) else {
            panic!("a stopped VM refuses a command that needs it")
        };
        assert!(
            why.contains("lima cannot start it: limactl not installed"),
            "{why}"
        );

        // "The probe could not be made" is neither answer. Under lima it
        // forks `limactl`, and one fork that fails -- or is cut off by
        // LIVENESS_LIMIT -- must not refuse every forwarded command over
        // a factory that is running, nor report a stopped VM to the bar
        // widget. The command goes to the guest, and says why first: the
        // reason is not in the log at every log level.
        let probe = Err("asking lima whether ssf-default is running: fork/exec: \
resource temporarily unavailable"
            .to_string());
        let Gate::Send(Some(note)) = forwarding_gate(&probe, "default", "lima", "tell", None)
        else {
            panic!("an unanswered probe forwards the command")
        };
        assert!(
            note.starts_with("could not tell whether VM default is running: asking lima"),
            "{note}"
        );
        assert!(note.contains("sending `ssf tell` to it anyway"), "{note}");
        assert!(!note.contains("is not running,"), "{note}");
        // A host with no `limactl` at all cannot answer the probe, so
        // the refusal that names the missing tooling is never reached:
        // the note carries it instead, or the person sees nothing but
        // ssh refusing a connection.
        let Gate::Send(Some(note)) = forwarding_gate(
            &probe,
            "default",
            "lima",
            "status",
            Some("limactl not installed; install lima"),
        ) else {
            panic!("an unanswered probe forwards the command")
        };
        assert!(
            note.contains("if it is down, lima cannot start it: limactl not installed"),
            "{note}"
        );
        // And the note carries the advice the refusal used to give: the
        // ssh failure that may follow it says nothing about ssf.
        assert!(note.contains("`ssf vm start` starts it"), "{note}");
    }

    #[test]
    fn the_widget_gets_an_answer_for_a_guest_that_did_not_give_one() {
        // The document names the host service, which is read from the
        // state directory: a test's must be its own (#140).
        let _sandbox = crate::config::test_support::sandbox();
        // The host cannot fill in what only the guest knows, so the
        // sessions and repositories are empty rather than invented; what
        // it can fill in is the VM, and each of the three answers the
        // probe can give reaches the document as itself. An ssh failure
        // with nothing put in its place is the case this exists to stop:
        // the widget parses that as a factory with nothing in it.
        assert_eq!(probe_word(&Ok(true)), "running");
        assert_eq!(probe_word(&Ok(false)), "stopped");
        assert_eq!(probe_word(&Err("no answer".into())), "unknown");
        for state in ["running", "stopped", "unknown"] {
            let v = vm_status_for_guest(state);
            assert_eq!(v["vm"], state);
            assert_eq!(v["service_active"], false);
            assert_eq!(v["sessions"], serde_json::json!([]));
            assert_eq!(v["repos"], serde_json::json!([]));
        }
    }

    #[test]
    fn item_refs_accept_numbers_and_sessions() {
        let me = origin::Origin::new("o/r", 3).unwrap();
        assert_eq!(item_ref("7", Some(&me)).unwrap(), "o/r#7");
        assert_eq!(item_ref("#7", Some(&me)).unwrap(), "o/r#7");
        assert_eq!(item_ref("x/y#7", None).unwrap(), "x/y#7");
        assert!(item_ref("7", None).is_err());
        // The reviewer sessions of before #115 had a suffix of their own.
        assert!(item_ref("7:reviewer", Some(&me)).is_err());
        assert!(item_ref("x/y#7:reviewer", None).is_err());
        assert!(item_ref("nonsense", Some(&me)).is_err());
    }
    #[test]
    fn the_handover_message_names_the_new_stack_and_ends_the_session() {
        assert_eq!(
            handover_recorded_text(
                "o/r#5",
                "Fix the widget",
                "Pi",
                Some("openai/gpt-6"),
                Some("high"),
                None,
                Some(1234),
                10,
            ),
            "Handover of o/r#5 (\"Fix the widget\") recorded: to Pi (model openai/gpt-6, effort \
high), with a summary of 1,234 chars.\nThe daemon ends this session on its next pass (within \
10s) and starts the new one in the same workspace. Stop working now: do not start anything \
else, and do not run this command again."
        );
        let plain = handover_recorded_text("o/r#5", "T", "Codex", None, None, None, None, 30);
        assert!(
            plain.starts_with(
                "Handover of o/r#5 (\"T\") recorded: to Codex (the harness's default model, the \
harness's default effort), without a summary."
            ),
            "{plain}"
        );
        assert!(plain.contains("within 30s"), "{plain}");
        // With a repository command configured it is the command that
        // decides an unset model or effort, as the `handed-over` post
        // says of the same handover.
        let by_command = handover_recorded_text(
            "o/r#5",
            "T",
            "Claude Code",
            None,
            None,
            Some("claude --dangerously-skip-permissions"),
            None,
            10,
        );
        assert!(
            by_command.starts_with(
                "Handover of o/r#5 (\"T\") recorded: to Claude Code (the command's model, the \
command's effort), without a summary."
            ),
            "{by_command}"
        );
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000_000), "1,000,000");
    }

    #[test]
    fn a_handover_summary_comes_from_the_flag_or_the_file() {
        let dir = std::env::temp_dir().join(format!("ssf-summary-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("summary.md");
        std::fs::write(&path, "what is left").unwrap();
        assert_eq!(
            handover_summary(None, Some(&path), false)
                .unwrap()
                .as_deref(),
            Some("what is left")
        );
        assert_eq!(
            handover_summary(Some("inline".into()), None, false)
                .unwrap()
                .as_deref(),
            Some("inline")
        );
        assert!(handover_summary(None, None, true).unwrap().is_none());
        // No summary form at all: the clap group cannot require one, since
        // `--cancel` takes none either, so the check is here.
        let e = handover_summary(None, None, false).unwrap_err().to_string();
        assert!(e.contains("say what the new session is told"), "{e}");
        // Empty, and over the cap, are the writer's to fix.
        std::fs::write(&path, "   \n").unwrap();
        let e = handover_summary(None, Some(&path), false)
            .unwrap_err()
            .to_string();
        assert!(e.contains("write a summary or pass --no-summary"), "{e}");
        assert!(
            handover_summary(Some(String::new()), None, false)
                .unwrap_err()
                .to_string()
                .contains("the summary is empty")
        );
        let long = "x".repeat(ipc::MAX_SUMMARY_CHARS + 1);
        let e = handover_summary(Some(long), None, false)
            .unwrap_err()
            .to_string();
        assert!(e.contains("8,001 characters") && e.contains("8,000"), "{e}");
        assert!(
            handover_summary(None, Some(&dir.join("nope.md")), false)
                .unwrap_err()
                .to_string()
                .contains("reading the summary from")
        );
        // A summary that quotes a sign-in screen would block the session
        // it starts: refused here, where the author can reword it, and
        // the refusal itself does not repeat the phrase.
        let e = handover_summary(
            Some("Blocked all afternoon: the pane kept saying Please run /login".into()),
            None,
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(
            e.contains("would read as a harness's own sign-in screen"),
            "{e}"
        );
        assert!(!driver::quotes_login_prompt(&e), "{e}");
        assert!(
            e.contains("[\u{2026}]"),
            "the phrase is redacted, not dropped: {e}"
        );
        // Wherever the phrase stands in it: a long summary that quotes one
        // in its third line, and one that has an `[ssf]` marker of its own.
        let mut long =
            String::from("Handing over.\n\nThe pane kept saying \"Please run /login\" at me.\n");
        for i in 0..40 {
            long.push_str(&format!("- step {i}: done\n"));
        }
        assert!(
            handover_summary(Some(long), None, false)
                .unwrap_err()
                .to_string()
                .contains("would read as a harness's own sign-in screen")
        );
        assert!(
            handover_summary(
                Some("[ssf] the note said:\n- not logged in, it said".into()),
                None,
                false
            )
            .unwrap_err()
            .to_string()
            .contains("would read as a harness's own sign-in screen")
        );
        let _ = std::fs::remove_dir_all(&dir);
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
    fn config_set_replaces_the_old_startup_wait_key_with_the_new_one() {
        let dir = std::env::temp_dir().join(format!(
            "ssf-config-set-rename-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[daemon]\nstartup_orca_wait_secs = 60\n").unwrap();
        // The new name over a file holding the old one.
        config_set_at(&path, "daemon.startup_driver_wait_secs", "30", false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("startup_orca_wait_secs"), "{text}");
        assert_eq!(
            Config::load_from(&path)
                .unwrap()
                .daemon
                .startup_driver_wait_secs,
            30
        );
        // The old name is still accepted and lands under the new one.
        config_set_at(&path, "daemon.startup_orca_wait_secs", "45", false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("startup_driver_wait_secs = 45"), "{text}");
        assert!(!text.contains("startup_orca_wait_secs"), "{text}");
        assert_eq!(
            Config::load_from(&path)
                .unwrap()
                .daemon
                .startup_driver_wait_secs,
            45
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn config_set_switches_event_comments_through_the_generic_path() {
        let dir = std::env::temp_dir().join(format!(
            "ssf-config-set-events-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "").unwrap();
        assert!(Config::load_from(&path).unwrap().daemon.event_comments);
        config_set_at(&path, "daemon.event_comments", "false", false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("event_comments = false"), "{text}");
        assert!(!Config::load_from(&path).unwrap().daemon.event_comments);
        config_set_at(&path, "daemon.event_comments", "true", false).unwrap();
        assert!(Config::load_from(&path).unwrap().daemon.event_comments);
        // Per-repository values go through `ssf repo set`, not here.
        assert!(config_set_at(&path, "repo.event_comments", "false", false).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn repo_add_and_set_switch_event_comments_and_clear_puts_it_back() {
        let dir = std::env::temp_dir().join(format!(
            "ssf-repo-events-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let add = |event_comments: Option<bool>| RepoCommand::Add {
            name: "o/r".into(),
            harness: "claude".into(),
            driver: None,
            path: None,
            clone_url: None,
            base_branch: None,
            command: None,
            model: None,
            effort: None,
            instructions: None,
            prompt_file: None,
            allowed_users: None,
            accept_anyone_risk: false,
            event_comments,
        };
        let set = |event_comments: Option<bool>, clear: Vec<String>| RepoCommand::Set {
            name: "o/r".into(),
            harness: None,
            driver: None,
            path: None,
            clone_url: None,
            base_branch: None,
            command: None,
            model: None,
            effort: None,
            instructions: None,
            prompt_file: None,
            allowed_users: None,
            accept_anyone_risk: false,
            event_comments,
            git_name: None,
            git_email: None,
            git_signing_key: None,
            git_credential: None,
            clear,
        };
        let loaded = || Config::load_from(&path).unwrap();
        // Unset by default: the instance decides, and nothing is written.
        repo_at(&path, add(None)).unwrap();
        assert_eq!(loaded().repos[0].event_comments, None);
        assert!(loaded().event_comments(&loaded().repos[0]));
        let text = std::fs::read_to_string(&path).unwrap();
        let repo_table = text.split("[[repo]]").nth(1).unwrap();
        assert!(!repo_table.contains("event_comments"), "{text}");
        // Set off, then on, then cleared.
        repo_at(&path, set(Some(false), vec![])).unwrap();
        let cfg = loaded();
        assert_eq!(cfg.repos[0].event_comments, Some(false));
        assert!(!cfg.event_comments(&cfg.repos[0]));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("event_comments = false")
        );
        repo_at(&path, set(Some(true), vec![])).unwrap();
        assert_eq!(loaded().repos[0].event_comments, Some(true));
        repo_at(&path, set(None, vec![])).unwrap();
        assert_eq!(loaded().repos[0].event_comments, Some(true), "left alone");
        repo_at(&path, set(None, vec!["event_comments".into()])).unwrap();
        assert_eq!(loaded().repos[0].event_comments, None);
        // `repo add` over an existing entry takes the flag too.
        repo_at(&path, add(Some(false))).unwrap();
        assert_eq!(loaded().repos[0].event_comments, Some(false));
        std::fs::remove_dir_all(&dir).ok();
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

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ssf-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git_value<'a>(plan: &'a LaunchEnv, key: &str) -> Vec<&'a str> {
        plan.git
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    fn env_value<'a>(plan: &'a LaunchEnv, key: &str) -> Option<&'a str> {
        plan.env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn launch_env_is_the_bot_by_default() {
        let dir = scratch_dir("launch-bot");
        let key = dir.join("bot_ed25519");
        std::fs::write(&key, "k").unwrap();
        std::fs::write(keys::public_path(&key), "p").unwrap();
        let mut cfg = Config::default();
        cfg.github.login = Some("acme-bot".into());
        cfg.github.ssh_key_path = Some(key.to_string_lossy().to_string());
        cfg.github.signing_key_id = Some(1);
        cfg.repos.push(RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            ..RepoConfig::default()
        });
        let plan = launch_env(&cfg, cfg.repos.first(), "/opt/ssf", true);
        assert_eq!(env_value(&plan, "GIT_AUTHOR_NAME"), Some("acme-bot"));
        assert_eq!(env_value(&plan, "GIT_COMMITTER_NAME"), Some("acme-bot"));
        assert_eq!(
            env_value(&plan, "GIT_AUTHOR_EMAIL"),
            Some("acme-bot@users.noreply.github.com")
        );
        assert_eq!(
            env_value(&plan, "GIT_COMMITTER_EMAIL"),
            env_value(&plan, "GIT_AUTHOR_EMAIL")
        );
        assert_eq!(
            git_value(&plan, "credential.helper"),
            vec!["", "!/opt/ssf git-credential"]
        );
        assert_eq!(git_value(&plan, "gpg.format"), vec!["ssh"]);
        assert_eq!(
            git_value(&plan, "user.signingkey"),
            vec![keys::public_path(&key).to_string_lossy().as_ref()]
        );
        assert_eq!(git_value(&plan, "commit.gpgsign"), vec!["true"]);
        assert_eq!(git_value(&plan, "tag.gpgsign"), vec!["true"]);
        assert!(
            env_value(&plan, "GIT_SSH_COMMAND")
                .unwrap()
                .contains("IdentitiesOnly=yes")
        );
        assert!(plan.notes.is_empty(), "{:?}", plan.notes);
        // Without a token there is no bot helper to configure.
        let plan = launch_env(&cfg, cfg.repos.first(), "/opt/ssf", false);
        assert!(git_value(&plan, "credential.helper").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn launch_env_commits_as_the_configured_person() {
        let dir = scratch_dir("launch-person");
        let bot_key = dir.join("bot_ed25519");
        std::fs::write(&bot_key, "k").unwrap();
        let person_key = dir.join("id_ed25519");
        std::fs::write(&person_key, "k").unwrap();
        let mut cfg = Config::default();
        cfg.github.login = Some("acme-bot".into());
        cfg.github.ssh_key_path = Some(bot_key.to_string_lossy().to_string());
        cfg.github.signing_key_id = Some(1);
        cfg.git.name = Some("Ann Person".into());
        cfg.git.email = Some("ann@example.com".into());
        cfg.repos.push(RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            ..RepoConfig::default()
        });
        // A person with nothing else: unsigned, bot pushes the commits.
        let plan = launch_env(&cfg, cfg.repos.first(), "/opt/ssf", true);
        assert_eq!(env_value(&plan, "GIT_AUTHOR_NAME"), Some("Ann Person"));
        assert_eq!(env_value(&plan, "GIT_COMMITTER_NAME"), Some("Ann Person"));
        assert_eq!(
            env_value(&plan, "GIT_AUTHOR_EMAIL"),
            Some("ann@example.com")
        );
        assert_eq!(git_value(&plan, "user.name"), vec!["Ann Person"]);
        assert_eq!(git_value(&plan, "user.email"), vec!["ann@example.com"]);
        assert!(git_value(&plan, "gpg.format").is_empty());
        assert_eq!(git_value(&plan, "commit.gpgsign"), vec!["false"]);
        assert_eq!(git_value(&plan, "tag.gpgsign"), vec!["false"]);
        assert_eq!(
            git_value(&plan, "credential.helper"),
            vec!["", "!/opt/ssf git-credential"]
        );
        // SSH remotes still go through the bot's key.
        assert!(
            env_value(&plan, "GIT_SSH_COMMAND")
                .unwrap()
                .contains("bot_ed25519")
        );
        // Their own key (no .pub next to it: the private path is used) and
        // their own token, set on the repository.
        cfg.repos[0].git.signing_key = Some(config::SigningKey::Path(
            person_key.to_string_lossy().to_string(),
        ));
        cfg.repos[0].git.credential = Some("token:ann".into());
        let plan = launch_env(&cfg, cfg.repos.first(), "/opt/ssf", true);
        assert_eq!(git_value(&plan, "gpg.format"), vec!["ssh"]);
        assert_eq!(
            git_value(&plan, "user.signingkey"),
            vec![person_key.to_string_lossy().as_ref()]
        );
        assert_eq!(git_value(&plan, "commit.gpgsign"), vec!["true"]);
        assert_eq!(
            git_value(&plan, "credential.helper"),
            vec!["", "!/opt/ssf git-credential"],
            "the token is looked up by the helper, per SSF_REPO"
        );
        // A helper string replaces ours; a missing key means unsigned, with a note.
        cfg.repos[0].git.credential = Some("!gh auth git-credential".into());
        cfg.repos[0].git.signing_key = Some(config::SigningKey::Path(
            dir.join("gone").to_string_lossy().to_string(),
        ));
        let plan = launch_env(&cfg, cfg.repos.first(), "/opt/ssf", true);
        assert_eq!(
            git_value(&plan, "credential.helper"),
            vec!["", "!gh auth git-credential"]
        );
        assert_eq!(git_value(&plan, "commit.gpgsign"), vec!["false"]);
        assert!(
            plan.notes.iter().any(|n| n.contains("gone")),
            "{:?}",
            plan.notes
        );
        // Another repository without an override is the instance identity.
        let other = RepoConfig {
            name: "o/s".into(),
            harness: "claude".into(),
            ..RepoConfig::default()
        };
        let plan = launch_env(&cfg, Some(&other), "/opt/ssf", true);
        assert_eq!(env_value(&plan, "GIT_AUTHOR_NAME"), Some("Ann Person"));
        assert_eq!(git_value(&plan, "commit.gpgsign"), vec!["false"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_set_writes_the_git_table_whole() {
        let dir = scratch_dir("config-set-git");
        let path = dir.join("config.toml");
        // One half of an identity is refused, with the way to set both.
        let err = config_set_at(&path, "git.name", "Ann", false).unwrap_err();
        assert!(
            format!("{err:#}").contains("ssf config set git '{"),
            "{err:#}"
        );
        assert!(!path.exists());
        config_set_at(
            &path,
            "git",
            r#"{ name = "Ann Person", email = "ann@example.com" }"#,
            false,
        )
        .unwrap();
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(cfg.git.name.as_deref(), Some("Ann Person"));
        assert_eq!(cfg.git.email.as_deref(), Some("ann@example.com"));
        // With both there, one key at a time is fine.
        config_set_at(&path, "git.email", "ann@work.example", false).unwrap();
        config_set_at(&path, "git.signing_key", "false", false).unwrap();
        config_set_at(&path, "git.credential", "token:ann", false).unwrap();
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(cfg.git.email.as_deref(), Some("ann@work.example"));
        assert_eq!(cfg.git.signing_key, Some(config::SigningKey::Off(false)));
        assert_eq!(
            cfg.git_identity(None).credential,
            config::Credential::Token("ann".into())
        );
        assert!(config_set_at(&path, "git.credential", "token:", false).is_err());
        let err = config_set_at(&path, "git.credential", "tokn:ann", false).unwrap_err();
        assert!(format!("{err:#}").contains("token:"), "{err:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_helper_answers_as_the_bot_outside_a_session() {
        let mut cfg = Config::default();
        cfg.github.login = Some("acme-bot".into());
        cfg.git.credential = Some("token:ann".into());
        cfg.repos.push(RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            git: config::GitConfig {
                credential: Some("file:/t".into()),
                ..Default::default()
            },
            ..RepoConfig::default()
        });
        assert_eq!(
            push_credential(&cfg, Some("o/r")),
            config::Credential::File(PathBuf::from("/t"))
        );
        assert_eq!(
            push_credential(&cfg, Some("O/R")),
            config::Credential::File(PathBuf::from("/t"))
        );
        assert_eq!(
            push_credential(&cfg, Some("o/other")),
            config::Credential::Token("ann".into())
        );
        // The daemon's clones and a guest shell: never the person.
        assert_eq!(push_credential(&cfg, None), config::Credential::Bot);
    }

    #[test]
    fn signing_key_flag_reads_false_as_off() {
        assert_eq!(parse_signing_key("false"), config::SigningKey::Off(false));
        assert_eq!(parse_signing_key(" OFF "), config::SigningKey::Off(false));
        assert_eq!(
            parse_signing_key("~/.ssh/id_ed25519"),
            config::SigningKey::Path("~/.ssh/id_ed25519".into())
        );
    }

    #[test]
    fn guide_and_launch_identity_prefer_session_then_daemon_then_config() {
        assert_eq!(
            configured_bot_login(
                Some("session-bot"),
                Some("daemon-bot"),
                Some("configured-bot")
            ),
            Some("session-bot".into())
        );
        assert_eq!(
            configured_bot_login(Some(""), Some("daemon-bot"), Some("configured-bot")),
            Some("daemon-bot".into())
        );
        assert_eq!(
            configured_bot_login(None, None, Some("configured-bot")),
            Some("configured-bot".into())
        );
        assert_eq!(configured_bot_login(None, None, Some("")), None);
        assert_eq!(configured_bot_login(None, None, None), None);
    }

    async fn auth_test_api() -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut request = [0; 1024];
                let _ = socket.read(&mut request).await;
                let body = r#"{"login":"new-bot","id":42,"type":"User"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        base
    }

    #[tokio::test]
    async fn auth_does_not_rewrite_the_live_daemon_snapshot() {
        // Auth used to load and then save this whole file. The fixture holds
        // an active session and a daemon snapshot, including the old cache,
        // so a byte-for-byte check catches either login or logout doing that.
        let _sandbox = config::test_support::sandbox();
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut cfg = Config::default();
        cfg.github.api_url = auth_test_api().await;
        cfg.save().unwrap();
        let live = r#"{
  "bot_login": "daemon-bot",
  "last_poll_at": "2026-09-09T01:00:00Z",
  "last_error": "driver was restarting",
  "repos": {
    "acme/widgets": {
      "issues": {
        "183": {
          "number": 183,
          "title": "Keep this session",
          "worktree_path": "/scratch/widgets.worktrees/issue-183",
          "terminal_handle": "live-terminal",
          "agent_session_id": "live-conversation",
          "active": true
        }
      }
    }
  }
}
"#;
        let state_path = state::state_path();
        std::fs::write(&state_path, live).unwrap();
        // This is the daemon's in-memory snapshot while auth runs.
        let daemon_state = state::State::load().unwrap();

        auth(AuthCommand::Login {
            user: None,
            web: false,
            token: Some("test-token".into()),
            no_keys: true,
            email: None,
            yes: true,
        })
        .await
        .unwrap();
        assert_eq!(
            Config::load().unwrap().github.login.as_deref(),
            Some("new-bot")
        );
        assert_eq!(std::fs::read_to_string(&state_path).unwrap(), live);

        // The daemon may save its snapshot after auth. It keeps the live
        // binding, and it cannot overwrite the separate auth configuration.
        daemon_state.save().unwrap();
        assert_eq!(
            Config::load().unwrap().github.login.as_deref(),
            Some("new-bot")
        );

        let after_daemon_save = std::fs::read_to_string(&state_path).unwrap();
        auth_logout(true).await.unwrap();
        assert!(Config::load().unwrap().github.login.is_none());
        assert_eq!(
            std::fs::read_to_string(&state_path).unwrap(),
            after_daemon_save
        );

        // A blank inline token is not a credential either; it must not make
        // the retained daemon cache look like a live sign-in after logout.
        let mut logged_out = Config::load().unwrap();
        logged_out.github.token = Some(" \t\n ".into());
        logged_out.save().unwrap();

        let status = status::Snapshot {
            cfg: Config::load().unwrap(),
            state: state::State::load().unwrap(),
            workspaces: Vec::new(),
            down: Vec::new(),
            errors: Vec::new(),
        };
        assert!(status.to_json()["bot_login"].is_null());
        assert!(status::render_status(&status).contains("bot:     (not signed in)"));

        let state = state::State::load().unwrap();
        let session = &state.repos["acme/widgets"].issues[&183];
        assert_eq!(session.terminal_handle.as_deref(), Some("live-terminal"));
        assert_eq!(
            session.agent_session_id.as_deref(),
            Some("live-conversation")
        );
        assert!(session.active);
    }
}
