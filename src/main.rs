//! ssf — Simple Software Factory.
//!
//! Watches GitHub repos for issues assigned to a bot account and turns each one
//! into an Orca workspace running a coding agent, feeding later issue activity
//! into that agent.

mod agents;
mod config;
mod engine;
mod ghcli;
mod github;
mod keys;
mod models;
mod orca;
mod origin;
mod prompt;
mod sessions;
mod shim;
mod state;
mod status;
mod ui;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::json;
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
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
    /// Check that GitHub, Orca and the configured harnesses are usable.
    Doctor,
    /// Omarchy desktop integration: bar widget, menu entries, background service.
    Ui {
        #[command(subcommand)]
        command: UiCommand,
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
        /// Existing local checkout to register in Orca instead of cloning.
        #[arg(long)]
        path: Option<String>,
        /// Clone URL (default https://github.com/owner/name.git).
        #[arg(long)]
        clone_url: Option<String>,
        /// Base ref for issue worktrees.
        #[arg(long)]
        base_branch: Option<String>,
        /// Command that starts the harness (default: the harness id), e.g. "claude --dangerously-skip-permissions".
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
    },
    /// Change some settings of a watched repository, keeping the rest.
    Set {
        name: String,
        #[arg(long)]
        harness: Option<String>,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        clone_url: Option<String>,
        #[arg(long)]
        base_branch: Option<String>,
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
        /// Clear an optional field: path, clone_url, base_branch, command, model, effort, instructions, prompt_file.
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
    Set { key: String, value: String },
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
    // `ssf launch` links `~/.config/ssf/bin/gh` to this binary; invoked under
    // that name we are the gh shim, not the daemon.
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
        Command::Doctor => doctor().await,
        Command::Ui { command } => ui_cmd(command),
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

fn check_harness(harness: &str) {
    if !agents::is_known(harness) {
        eprintln!(
            "note: `{harness}` is not one of the agents Omarchy knows about (see `ssf agents`); Orca must know how to launch it"
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
            path,
            clone_url,
            base_branch,
            command,
            model,
            effort,
            instructions,
            prompt_file,
        } => {
            let (owner, r) = split_repo_name(&name)?;
            let name = format!("{owner}/{r}");
            let path = expand_checkout(path)?;
            check_harness(&harness);
            let entry = RepoConfig {
                name: name.clone(),
                harness,
                command,
                model: model.map(|m| m.trim().to_string()),
                effort: effort.map(|e| e.trim().to_string()),
                clone_url,
                path,
                base_branch,
                instructions,
                prompt_file,
            };
            entry.validate_launch_prefs()?;
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
            path,
            clone_url,
            base_branch,
            command,
            model,
            effort,
            instructions,
            prompt_file,
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
            for field in clear {
                match field.as_str() {
                    "path" => entry.path = None,
                    "clone_url" => entry.clone_url = None,
                    "base_branch" => entry.base_branch = None,
                    "command" => entry.command = None,
                    "model" => entry.model = None,
                    "effort" => entry.effort = None,
                    "instructions" => entry.instructions = None,
                    "prompt_file" => entry.prompt_file = None,
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
            }
            Ok(())
        }
        ConfigCommand::Get { key } => {
            let cfg = Config::load()?;
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
        ConfigCommand::Set { key, value } => {
            if key == "github.token" || key == "github" {
                bail!("credentials are managed with `ssf auth login`, not `config set`");
            }
            if key.starts_with("repo") {
                bail!("repositories are managed with `ssf repo add|set|remove`");
            }
            let path = config::config_path();
            let raw = std::fs::read_to_string(&path).unwrap_or_default();
            let mut table: toml::Table = toml::from_str(&raw).context("parsing config")?;
            let parts: Vec<&str> = key.split('.').collect();
            if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
                bail!("invalid key {key}");
            }
            let parsed = parse_toml_scalar(&value);
            let mut cur: &mut toml::Table = &mut table;
            for part in &parts[..parts.len() - 1] {
                let next = cur
                    .entry(part.to_string())
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                cur = next
                    .as_table_mut()
                    .with_context(|| format!("{part} is not a table"))?;
            }
            cur.insert(parts[parts.len() - 1].to_string(), parsed);
            let text = toml::to_string_pretty(&table)?;
            // Validate before writing so a typo cannot break the daemon.
            let _: Config =
                toml::from_str(&text).with_context(|| format!("{key} is not a valid setting"))?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            config::write_atomic(&path, text.as_bytes(), 0o600)?;
            println!("{key} = {value}");
            Ok(())
        }
    }
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

async fn run(once: bool) -> Result<()> {
    let cfg = Config::load()?;
    if cfg.repos.is_empty() && once {
        bail!("no repositories configured; run `ssf repo add owner/name --harness claude` first");
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
    let me = match (
        std::env::var("SSF_REPO").ok().filter(|r| !r.is_empty()),
        std::env::var("SSF_ISSUE")
            .ok()
            .and_then(|n| n.parse::<u64>().ok()),
    ) {
        (Some(r), Some(n)) => Some(status::session_id(&r, n)),
        _ => None,
    };
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
                "orca_available": snap.orca.is_ok(),
                "orca_error": snap.orca.as_ref().err(),
                "sessions": sessions,
            }))?
        );
        return Ok(());
    }
    if let Err(e) = &snap.orca {
        eprintln!("orca unavailable, agent states unknown: {e}");
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
    let orca = orca::Orca::new(cfg.orca.clone());
    let cli_present =
        std::path::Path::new(orca.command()).exists() || which(orca.command()).is_some();
    check(cli_present, format!("Orca CLI at {}", orca.command()));
    if cli_present {
        match orca.status().await {
            Ok(_) => check(true, "Orca runtime reachable and ready".into()),
            Err(e) => check(false, format!("Orca runtime: {e:#}")),
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
    for r in &cfg.repos {
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
    let shim_ok = shim::target()
        .and_then(|t| std::fs::canonicalize(t).ok())
        .is_some_and(|t| {
            std::env::current_exe()
                .and_then(std::fs::canonicalize)
                .is_ok_and(|me| me == t)
        });
    check(
        shim_ok,
        format!(
            "gh shim at {} {}",
            shim::path().display(),
            if shim_ok {
                "links to this ssf"
            } else {
                "not installed yet (ssf launch creates it when an agent starts)"
            }
        ),
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
                "{} post(s) by the bot arrived without an origin tag (gh shim not in effect): {}",
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
        format!("new clones go under {}", cfg.projects_dir().display()),
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
