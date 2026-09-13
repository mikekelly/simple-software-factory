use super::prelude::*;
use super::*;

pub(super) async fn auth(command: AuthCommand) -> Result<()> {
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
                (me.login, t, "token file")
            } else if factory_vm::in_guest() {
                let t = ghcli::login_device(&host, ghcli::REQUIRED_SCOPES)?;
                let gh = github::GitHub::new(&cfg.github.api_url, &t)?;
                let me = gh
                    .whoami()
                    .await
                    .context("device token rejected by GitHub")?;
                if let Some(u) = user.as_deref()
                    && !me.login.eq_ignore_ascii_case(u.trim_start_matches('@'))
                {
                    bail!(
                        "that sign-in belongs to @{}, not @{u}; no bot credentials were changed",
                        me.login
                    );
                }
                (me.login, t, "guest token file")
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
            // The newly authenticated credential must replace a legacy inline token.
            cfg.github.token = None;
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
            if source == "token file" || source == "guest token file" {
                let path = config::save_token(&token)?;
                println!("Stored token for @{login} in {}", path.display());
            }
            cfg.save()?;
            refresh_guest_auth()?;
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
        AuthCommand::Logout { keep_keys } => {
            auth_logout(keep_keys).await?;
            refresh_guest_auth()
        }
    }
}

/// The daemon holds its API token for its lifetime. Authentication changes
/// restart only the guest service, whose workspaces live on the data disk.
pub(super) fn refresh_guest_auth() -> Result<()> {
    // Unit tests authenticate against local fixtures and must never control
    // the real guest service, even when cargo test itself runs inside a VM.
    #[cfg(not(test))]
    if factory_vm::in_guest() {
        if config::config_dir() != PathBuf::from(factory_vm::GUEST_HOME).join(".config/ssf") {
            println!(
                "Credentials saved in {}; restart the daemon using that configuration to apply them.",
                config::config_dir().display()
            );
            return Ok(());
        }
        let status = std::process::Command::new("sudo")
            .args(["systemctl", "restart", "ssf.service"])
            .status()
            .context("credentials saved in the guest; restarting its daemon")?;
        if !status.success() {
            bail!(
                "credentials saved in the guest, but its daemon could not restart; run `ssf vm ssh -- sudo systemctl restart ssf.service`"
            );
        }
    }
    Ok(())
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
    cfg.github.token = None;
    cfg.save()?;
    Ok(())
}

pub(super) fn read_stdin_token(interactive: bool) -> Result<String> {
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

pub(super) fn confirm(question: &str) -> Result<bool> {
    eprint!("{question} [Y/n] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let a = line.trim().to_lowercase();
    Ok(a.is_empty() || a == "y" || a == "yes")
}

/// `claude logged in, codex not logged in (omp not installed)`.
pub(super) fn login_summary(states: &[factory_vm::LoginState]) -> String {
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
pub(super) fn pick_login(
    states: &[factory_vm::LoginState],
) -> Result<Option<&'static factory_vm::Login>> {
    let installed: Vec<&factory_vm::LoginState> = states.iter().filter(|s| s.installed).collect();
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
        if let Some(l) = chosen.and_then(factory_vm::login) {
            return Ok(Some(l));
        }
        eprintln!("a number from the list, a harness name, or q");
    }
}

/// Terminal picker over gh's accounts; `None` means "sign in another one".
pub(super) fn pick_account(accounts: &[ghcli::Account]) -> Result<Option<String>> {
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
pub(crate) fn hostname() -> String {
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
pub(super) fn pick_hostname(
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
pub(super) fn check_herdr_harness(harness: &str) {
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

pub(super) fn check_harness(harness: &str) {
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
