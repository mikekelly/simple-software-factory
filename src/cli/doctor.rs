use super::prelude::*;
use super::*;

pub(super) async fn doctor() -> Result<()> {
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
    let place = if factory_vm::in_guest() {
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
    if factory_vm::in_guest() {
        match factory_vm::disk_use(Path::new(factory_vm::GUEST_DATA_DIR)) {
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
            .and_then(|t| factory_vm::parse_meminfo(&t))
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
    let me = client_executable().and_then(std::fs::canonicalize).ok();
    let is_me = |p: &std::path::Path| me.is_some() && std::fs::canonicalize(p).ok() == me;
    // Both links (gh and ssf) have to point at this client binary for agents
    // to post as the bot and run the server's command endpoint.
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
        factory_ui::service_active(),
        format!(
            "{} running{}",
            platform::service_name(),
            if factory_ui::service_enabled() {
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
    if reports_backend_tooling(factory_vm::in_guest()) {
        let vm = factory_vm::Vm::new(&cfg);
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
    }
    // The widget lives on the host; inside the guest there is no Omarchy
    // shell to check.
    if factory_vm::in_guest() {
        println!("note bar widget: checked on the host, not inside the VM");
    } else if !platform::is_omarchy() {
        println!("note bar widget: not on Omarchy, nothing to enable");
    } else {
        check(
            factory_ui::widget_enabled().unwrap_or(false),
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
pub(super) fn checkout_root(cfg: &Config, r: &RepoConfig, state: &state::State) -> Option<String> {
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
pub(super) fn same_path(a: &str, b: &str) -> bool {
    let canon = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
    a == b || canon(a) == canon(b)
}

pub(super) fn which(bin: &str) -> Option<std::path::PathBuf> {
    if bin.contains('/') {
        let p = std::path::PathBuf::from(bin);
        return p.exists().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}
