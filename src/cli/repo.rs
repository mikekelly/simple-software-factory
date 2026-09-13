use super::prelude::*;
use super::*;

pub(super) fn repo(command: RepoCommand) -> Result<()> {
    repo_at(&config::config_path(), command)
}

/// `ssf repo ...` against the config file at `path`.
pub(super) fn repo_at(config_file: &Path, command: RepoCommand) -> Result<()> {
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
                github_id: None,
                aliases: Vec::new(),
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
                entry.github_id = cfg.repos[pos].github_id;
                entry.aliases = cfg.repos[pos].aliases.clone();
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

pub(super) fn expand_checkout(path: Option<String>) -> Result<Option<String>> {
    let Some(p) = path else { return Ok(None) };
    let p = config::expand_tilde(&p);
    if !p.join(".git").exists() {
        bail!("{} is not a git checkout", p.display());
    }
    Ok(Some(p.to_string_lossy().to_string()))
}

/// Parse a comma-separated `--allowed-users` value: logins, or `*`.
pub(super) fn parse_allowed_users(list: &str) -> Vec<String> {
    list.split([',', ' '])
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| l.trim_start_matches('@').to_string())
        .collect()
}

/// Set a repository's allow-list, with the wildcard only after consent.
pub(super) fn set_repo_allowed_users(
    entry: &mut RepoConfig,
    list: &str,
    accepted: bool,
) -> Result<()> {
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
pub(super) fn confirm_anyone_risk(accepted: bool, what: &str) -> Result<()> {
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
pub(super) fn anyone_risk_decision(
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

pub(super) fn config_cmd(command: ConfigCommand) -> Result<()> {
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
pub(super) fn config_set_at(path: &Path, key: &str, value: &str, accepted: bool) -> Result<()> {
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

pub(super) fn parse_toml_scalar(value: &str) -> toml::Value {
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
