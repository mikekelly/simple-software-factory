use super::prelude::*;

pub(super) fn launch(
    repo: Option<String>,
    issue: Option<u64>,
    issue_url: Option<String>,
    command: Vec<String>,
) -> Result<()> {
    let cfg = Config::load().unwrap_or_default();
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(command.join(" "));
    let me = client_executable()
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
    // A session is already on the server machine; do not send its own ssf
    // commands back through a remote transport selected by the operator.
    cmd.env_remove("SSF_SERVER");
    // A `gh` shim first on PATH stamps everything the agent posts with the
    // origin tag for this issue (see src/shim.rs).
    match client_executable().and_then(std::fs::canonicalize) {
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

pub(crate) fn server_executable() -> Result<PathBuf> {
    Ok(factory_vm::companion_server_path(
        &std::env::current_exe().context("locating the ssf client")?,
    ))
}

pub(crate) fn client_executable() -> std::io::Result<PathBuf> {
    std::env::current_exe().map(|p| companion_client_path(&p))
}

pub(super) fn companion_client_path(server: &Path) -> PathBuf {
    let name = server
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let client = if name == "ssf-server" {
        "ssf".to_string()
    } else if let Some(suffix) = name.strip_prefix("ssf-server-") {
        format!("ssf-{suffix}")
    } else {
        "ssf".to_string()
    };
    server.with_file_name(client)
}

/// A running daemon's last authenticated identity is retained in state for
/// token-only setups. A session's own identity wins, then that cache, then
/// the configured account for a factory that has not started yet.
pub(super) fn configured_bot_login(
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
pub(super) struct LaunchEnv {
    pub(super) env: Vec<(String, String)>,
    pub(super) git: Vec<(String, String)>,
    pub(super) notes: Vec<String>,
}

/// The git side of an agent's environment for `repo` (`None`: `[git]`
/// alone): the effective identity as author and committer, signing with
/// its key or off, and who pushes. `gh` is not touched here: it is the bot
/// through `GH_TOKEN`, whatever the identity. `have_token` says whether
/// the bot token could be resolved, which the bot credential needs.
pub(super) fn launch_env(
    cfg: &Config,
    repo: Option<&RepoConfig>,
    me: &str,
    have_token: bool,
) -> LaunchEnv {
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
pub(super) fn push_credential(cfg: &Config, session_repo: Option<&str>) -> config::Credential {
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
pub(super) fn parse_signing_key(value: &str) -> config::SigningKey {
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

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The credential helper `ssf launch` configures (and the VM guest's
/// `.gitconfig` names): answers HTTPS requests for the GitHub host with
/// the token of whoever the config says pushes for `SSF_REPO` (the bot by
/// default), and with the bot's outside a session. For another host it
/// answers nothing and git moves on.
pub(super) fn git_credential(op: &str) -> Result<()> {
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
