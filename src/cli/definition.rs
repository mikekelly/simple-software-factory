use super::prelude::*;

#[derive(Parser)]
#[command(
    name = "ssf",
    bin_name = "ssf",
    version,
    about = "Simple Software Factory: GitHub issues -> agent workspaces in herdr or Orca"
)]
pub(super) struct Cli {
    /// Run on this SSH destination; repeat to group servers in the dashboard.
    #[arg(long, global = true, env = "SSF_SERVER")]
    pub(super) _server: Option<String>,
    /// Log verbosity (also honours RUST_LOG).
    #[arg(long, global = true, default_value = "info", env = "SSF_LOG")]
    pub(super) log: String,
    #[command(subcommand)]
    pub(super) command: Command,
}

#[derive(Parser)]
#[command(name = "ssf-server", version, about = "Simple Software Factory daemon")]
pub(super) struct ServerCli {
    /// Log verbosity (also honours RUST_LOG).
    #[arg(long, default_value = "info", env = "SSF_LOG")]
    pub(super) log: String,
    /// Do a single engine pass and exit.
    #[arg(long)]
    pub(super) once: bool,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub(super) enum Command {
    /// Check local harness credentials without printing their contents.
    #[command(hide = true)]
    LoginProbe { harness: String },
    /// Initialize persistent guest factory state (called by the guest boot service).
    #[command(hide = true)]
    VmInit { seed: PathBuf },
    /// Prepare this user account and enable the packaged background service.
    Setup,
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
    /// Show tracked issues and their workspaces, joined with what the driver
    /// reports about each agent session.
    Status {
        #[arg(long)]
        json: bool,
        /// Keep the connection open and emit one JSON snapshot per line.
        #[arg(long, requires = "json")]
        watch: bool,
    },
    /// Show a live terminal dashboard (arrows/j/k, Enter to focus in Herdr, q to quit).
    Dashboard,
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
        /// Remove it even if the checks fail or the retired session owns open
        /// follow-ups; work in it is lost. An active owner or pending handover
        /// still blocks release. Refused inside a session unless --as names it.
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
    /// stop and disable the service, sign the bot out (revoking its keys on
    /// GitHub), and destroy the microVM. Omarchy owns its widget and menu.
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
        /// ~/.local/state/ssf (state and setup readiness).
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
pub(super) enum VmCommand {
    /// Size the VM from this machine, then make the guest and provision
    /// it (git, gh, herdr, the harness CLIs). Firecracker (`[vm] backend`,
    /// the default on Linux): downloads Firecracker, gvproxy and a guest
    /// kernel and makes the root image from an Ubuntu 24.04 LTS root tarball.
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
    /// Install Tailscale inside the guest on demand and enrol it in this
    /// terminal. The requested hostname is `ssf-vm`; Tailscale adds a numeric
    /// suffix when that name is already present in the tailnet.
    Tailscale,
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
    /// Finish an interrupted ownership migration; ordinary edits already
    /// operate in the guest and need no sync.
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
pub(super) enum AuthCommand {
    /// Sign in the bot and enroll an SSH key for pushes and signing.
    /// VM mode uses device authorization inside the guest; host mode can use gh's keyring.
    Login {
        /// Expected bot login in VM mode; choose this gh account in host mode.
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
pub(super) enum RepoCommand {
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
        /// SSF agent guidance appended to the main session, relative to the worktree unless absolute (default: SSF.md).
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
        /// SSF agent guidance appended to the main session, relative to the worktree unless absolute (default: SSF.md).
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
pub(super) enum ConfigCommand {
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
pub(super) enum UiCommand {
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
pub(super) enum ServiceCommand {
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
