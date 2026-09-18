use super::prelude::*;
use super::*;

/// Run the `ssf` transport client. With no server configured it execs the
/// adjacent daemon binary directly; `--server HOST` (or `SSF_SERVER`) execs
/// that same command endpoint through ssh.
pub async fn client_main() -> Result<()> {
    // `ssf launch` links the shim directory's `gh`, `git` and `ssf` to this
    // binary; under the two wrapper names this process is that wrapper.
    shim::run_as_shim();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let explicit_server = has_server_argument(&args);
    let configured = std::env::var("SSF_SERVER").ok().filter(|s| !s.is_empty());
    let (servers, args) = client_targets(args, configured)?;

    let cli = Cli::parse_from(std::iter::once("ssf".to_owned()).chain(args.clone()));
    if let Command::Server { command } = cli.command {
        if explicit_server {
            bail!("--server does not apply to `ssf server`");
        }
        return server_catalog_command(command);
    }
    if let Command::Skill { topic } = cli.command {
        return super::skill::print(topic);
    }
    let catalog = server_catalog::Catalog::load()?;
    let routes = if matches!(cli.command, Command::Dashboard) {
        catalog.resolve_dashboard(servers)?
    } else {
        catalog.resolve(servers)?
    };
    validate_command_targets(&catalog, &routes, &cli.command)?;
    if routes.iter().any(|route| {
        route
            .name
            .as_deref()
            .and_then(|name| catalog.get(name))
            .is_some_and(|target| {
                matches!(
                    target,
                    server_catalog::Target::Vm { .. }
                        | server_catalog::Target::Local {
                            config_dir: None,
                            state_dir: None,
                        }
                )
            })
    }) {
        catalog.validate_execution(&routes, &Config::load_from(&config::config_path())?)?;
    }
    if let [route] = routes.as_slice() {
        refuse_unsafe_global_command(route, &cli.command)?;
    }
    if let Command::Dashboard = cli.command {
        return dashboard::run(
            routes
                .into_iter()
                .map(|route| dashboard::ServerRoute {
                    identity: route
                        .name
                        .as_ref()
                        .map(|name| server_catalog::TargetIdentity {
                            name: name.clone(),
                            transport: catalog
                                .get(name)
                                .expect("a resolved route")
                                .transport()
                                .into(),
                        }),
                    label: route.name,
                    destination: route.destination,
                    local_context: route.local_context,
                    vm_context: route.vm_context,
                })
                .collect(),
        )
        .await;
    }

    let route = match routes.as_slice() {
        [route] => route,
        _ => bail!("multiple --server destinations are supported only by `ssf dashboard`"),
    };
    let doctor = matches!(cli.command, Command::Doctor);
    if doctor {
        return run_doctor_client(route, &catalog, &args);
    }

    let err = match &route.destination {
        Some(host) => {
            let command = remote_client_command(&args, None, None);
            std::process::Command::new("ssh")
                .arg("--")
                .arg(host)
                .arg(command)
                .exec()
        }
        None => {
            let mut command = std::process::Command::new(server_executable()?);
            command
                .arg("__client")
                .args(args)
                .env_remove("SSF_SERVER")
                .env_remove(server_catalog::SELECTED_VM_ENV)
                .env_remove(server_catalog::SELECTED_TARGET_ENV);
            if let Some(name) = &route.name {
                let transport = catalog.get(name).expect("a resolved route").transport();
                command.env(
                    server_catalog::SELECTED_TARGET_ENV,
                    serde_json::to_string(&server_catalog::TargetIdentity {
                        name: name.clone(),
                        transport: transport.into(),
                    })?,
                );
            }
            if let Some(context) = &route.local_context {
                command
                    .env("SSF_CONFIG_DIR", &context.config_dir)
                    .env("SSF_STATE_DIR", &context.state_dir);
            }
            if let Some(context) = &route.vm_context {
                command.env(
                    server_catalog::SELECTED_VM_ENV,
                    serde_json::to_string(context)?,
                );
            }
            command.exec()
        }
    };
    Err(anyhow::Error::from(err).context("starting ssf-server"))
}

fn run_doctor_client(
    route: &server_catalog::Route,
    catalog: &server_catalog::Catalog,
    args: &[String],
) -> Result<()> {
    let client_version =
        std::env::var(CLIENT_VERSION_ENV).unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_owned());
    let already_reported = std::env::var_os(VERSION_REPORTED_ENV).is_some();
    let identity = route
        .name
        .as_ref()
        .map(|name| server_catalog::TargetIdentity {
            name: name.clone(),
            transport: route
                .name
                .as_deref()
                .and_then(|name| catalog.get(name))
                .map_or("ssh", server_catalog::Target::transport)
                .into(),
        });
    // An SSH catalog name belongs to this client. Sending it to the remote
    // host would make doctor inspect an unrelated `ssf@NAME` service there.
    let endpoint_identity = if route.destination.is_none() {
        identity.as_ref()
    } else {
        None
    };
    let incompatible = if already_reported {
        false
    } else {
        let server_version = probe_target_version(route, endpoint_identity)?;
        super::doctor::report_versions(
            Some(&client_version),
            server_version.as_deref(),
            route.name.as_deref(),
        )
    };
    let status = match &route.destination {
        Some(host) => std::process::Command::new("ssh")
            .arg("--")
            .arg(host)
            .arg(remote_client_command(args, Some(&client_version), None))
            .status(),
        None => {
            let mut command = std::process::Command::new(server_executable()?);
            command.arg("__client").args(args);
            apply_local_route(&mut command, route, endpoint_identity)?;
            command
                .env(CLIENT_VERSION_ENV, &client_version)
                .env(VERSION_REPORTED_ENV, "1")
                .status()
        }
    }
    .context("running doctor on the selected server")?;
    std::process::exit(if incompatible {
        1
    } else {
        status.code().unwrap_or(1)
    });
}

fn probe_target_version(
    route: &server_catalog::Route,
    identity: Option<&server_catalog::TargetIdentity>,
) -> Result<Option<String>> {
    let output = match &route.destination {
        Some(host) => std::process::Command::new("ssh")
            .arg("--")
            .arg(host)
            .arg(remote_version_command(identity, true))
            .output(),
        None => {
            let mut command = std::process::Command::new(server_executable()?);
            command.arg("__target-version");
            apply_local_route(&mut command, route, identity)?;
            command.output()
        }
    }
    .context("asking the selected server for its version")?;
    if output.status.success()
        && let Some(version) = parse_program_version(&output.stdout)
    {
        return Ok(Some(version));
    }
    let unsupported = output.status.code() == Some(2)
        && String::from_utf8_lossy(&output.stderr).contains("__target-version");
    if !unsupported {
        return Ok(None);
    }

    let fallback = match &route.destination {
        Some(host) => std::process::Command::new("ssh")
            .arg("--")
            .arg(host)
            .arg(remote_version_command(None, false))
            .output(),
        None => std::process::Command::new(server_executable()?)
            .arg("--version")
            .output(),
    }
    .context("asking the server executable for its version")?;
    Ok(fallback
        .status
        .success()
        .then(|| parse_program_version(&fallback.stdout))
        .flatten())
}

fn apply_local_route(
    command: &mut std::process::Command,
    route: &server_catalog::Route,
    identity: Option<&server_catalog::TargetIdentity>,
) -> Result<()> {
    command
        .env_remove("SSF_SERVER")
        .env_remove(server_catalog::SELECTED_VM_ENV)
        .env_remove(server_catalog::SELECTED_TARGET_ENV);
    if let Some(identity) = identity {
        command.env(
            server_catalog::SELECTED_TARGET_ENV,
            serde_json::to_string(identity)?,
        );
    }
    if let Some(context) = &route.local_context {
        command
            .env("SSF_CONFIG_DIR", &context.config_dir)
            .env("SSF_STATE_DIR", &context.state_dir);
    }
    if let Some(context) = &route.vm_context {
        command.env(
            server_catalog::SELECTED_VM_ENV,
            serde_json::to_string(context)?,
        );
    }
    Ok(())
}

fn remote_version_command(
    identity: Option<&server_catalog::TargetIdentity>,
    target: bool,
) -> String {
    let command = if target {
        "ssf-server __target-version"
    } else {
        "ssf-server --version"
    };
    identity.map_or_else(
        || command.into(),
        |identity| {
            format!(
                "{}={} {command}",
                server_catalog::SELECTED_TARGET_ENV,
                shell_quote(&serde_json::to_string(identity).expect("target identity serializes"))
            )
        },
    )
}

fn parse_program_version(stdout: &[u8]) -> Option<String> {
    String::from_utf8_lossy(stdout)
        .split_whitespace()
        .last()
        .filter(|version| !version.is_empty())
        .map(str::to_owned)
}

fn validate_command_targets(
    catalog: &server_catalog::Catalog,
    routes: &[server_catalog::Route],
    command: &Command,
) -> Result<()> {
    if !matches!(command, Command::Vm { .. }) {
        return Ok(());
    }
    for route in routes {
        let Some(name) = route.name.as_deref() else {
            continue;
        };
        let target = catalog.get(name).expect("a resolved named route");
        if !matches!(target, server_catalog::Target::Vm { .. }) {
            bail!(
                "server {name:?} uses the {} transport, not a managed VM",
                target.transport()
            );
        }
    }
    Ok(())
}

fn refuse_unsafe_global_command(route: &server_catalog::Route, command: &Command) -> Result<()> {
    if route.local_context.is_some()
        && matches!(
            command,
            Command::VmInit { .. } | Command::Vm { .. } | Command::Uninstall { .. }
        )
    {
        bail!(
            "this command still manages the installation-wide service or VM; it is not yet supported for a namespaced local server"
        );
    }
    if route.vm_context.is_some() && matches!(command, Command::Uninstall { .. }) {
        bail!(
            "uninstall is not yet target-aware; refusing to apply installation-wide removal to a named VM server"
        );
    }
    Ok(())
}

fn has_server_argument(args: &[String]) -> bool {
    args.iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| arg == "--server" || arg.starts_with("--server="))
}

fn server_catalog_command(command: ServerCommand) -> Result<()> {
    let catalog = server_catalog::Catalog::load()?;
    match command {
        ServerCommand::List { json } => {
            if json {
                let rows: Vec<_> = catalog
                    .list()
                    .map(|(name, target)| server_catalog_json(name, target, catalog.len() == 1))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else if catalog.list().next().is_none() {
                println!("No configured servers (unqualified commands use the local server).")
            } else {
                for (name, target) in catalog.list() {
                    println!("{name}\t{}", target.transport());
                }
            }
            Ok(())
        }
        ServerCommand::Show { name, json } => {
            let target = catalog
                .get(&name)
                .with_context(|| format!("unknown SSF server {name:?}"))?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&server_catalog_json(
                        &name,
                        target,
                        catalog.len() == 1,
                    ))?
                );
            } else {
                println!("name:       {name}");
                println!("transport:  {}", target.transport());
                match target {
                    server_catalog::Target::Vm {
                        runtime_name,
                        backend,
                        config,
                    } => {
                        println!("runtime:    {runtime_name}");
                        if let Some(backend) = backend {
                            println!("backend:    {backend}");
                        }
                        if let Some(config) = config {
                            println!("settings:   catalog");
                            println!("directory:  {}", config.dir);
                            println!("ssh port:   {}", config.ssh_port);
                        } else {
                            println!("settings:   legacy [vm]");
                        }
                    }
                    server_catalog::Target::Ssh { destination } => {
                        println!("destination: {destination}");
                    }
                    server_catalog::Target::Local {
                        config_dir,
                        state_dir,
                    } => {
                        if let Some(config_dir) = config_dir {
                            println!("config:     {config_dir}");
                        }
                        if let Some(state_dir) = state_dir {
                            println!("state:      {state_dir}");
                        }
                    }
                }
            }
            Ok(())
        }
        ServerCommand::Add {
            name,
            local,
            vm,
            ssh,
            config_dir,
            state_dir,
            runtime_name,
            vm_dir,
            ssh_port,
        } => {
            if !local && (config_dir.is_some() || state_dir.is_some()) {
                bail!("--config-dir and --state-dir require --local");
            }
            if !vm && (runtime_name.is_some() || vm_dir.is_some() || ssh_port.is_some()) {
                bail!("--runtime-name, --vm-dir and --ssh-port require --vm");
            }
            let target = if local {
                server_catalog::Catalog::add_local(&name, config_dir, state_dir)?
            } else if vm {
                server_catalog::Catalog::add_vm(&name, runtime_name, vm_dir, ssh_port)?
            } else {
                server_catalog::Catalog::add_ssh(
                    &name,
                    ssh.context("an SSH destination is required")?,
                )?
            };
            println!("Added SSF server {name:?} ({}).", target.transport());
            let count = server_catalog::Catalog::load()?.len();
            if count == 1 {
                println!("It is selected automatically while it is the only configured server.");
            } else {
                println!(
                    "There are now {count} servers; target-scoped commands require `--server NAME`."
                );
            }
            Ok(())
        }
        ServerCommand::Remove { name } => {
            if platform::named_service_enabled_or_active(&name)? {
                bail!(
                    "server {name:?} still has an enabled or active service; run `ssf --server {name} ui service disable` first"
                );
            }
            let target = server_catalog::Catalog::remove(&name)?;
            println!(
                "Removed SSF server {name:?} from the catalog; its {} data was not deleted.",
                target.transport()
            );
            match target {
                server_catalog::Target::Local {
                    config_dir: Some(config),
                    state_dir: Some(state),
                } => println!("Retained {config} and {state}."),
                server_catalog::Target::Vm {
                    runtime_name,
                    config: Some(config),
                    ..
                } => println!(
                    "Retained VM runtime {runtime_name:?} and its resources under {}.",
                    config.dir
                ),
                _ => {}
            }
            Ok(())
        }
        ServerCommand::MigrateVm { name } => {
            if server_catalog::Catalog::migrate_legacy_vm(&name)? {
                println!(
                    "Migrated the existing VM in place as server {name:?}; its runtime resources and guest data were not moved."
                );
            } else {
                println!("VM server {name:?} was already migrated; nothing changed.");
            }
            Ok(())
        }
    }
}

fn server_catalog_json(
    name: &str,
    target: &server_catalog::Target,
    implicit: bool,
) -> serde_json::Value {
    match target {
        server_catalog::Target::Local {
            config_dir,
            state_dir,
        } => serde_json::json!({
            "name": name,
            "transport": "local",
            "implicit": implicit,
            "config_dir": config_dir,
            "state_dir": state_dir,
        }),
        server_catalog::Target::Vm {
            runtime_name,
            backend,
            config,
        } => serde_json::json!({
            "name": name,
            "transport": "vm",
            "implicit": implicit,
            "runtime_name": runtime_name,
            "backend": backend,
            "config": config,
        }),
        server_catalog::Target::Ssh { destination } => serde_json::json!({
            "name": name,
            "transport": "ssh",
            "implicit": implicit,
            "destination": destination,
        }),
    }
}

pub(crate) fn remote_client_command(
    args: &[String],
    client_version: Option<&str>,
    identity: Option<&server_catalog::TargetIdentity>,
) -> String {
    let mut command = String::from("ssf-server __client");
    if let Some(version) = client_version {
        command = format!("{CLIENT_VERSION_ENV}={} {command}", shell_quote(version));
        command = format!("{VERSION_REPORTED_ENV}=1 {command}");
    }
    if let Some(identity) = identity {
        command = format!(
            "{}={} {command}",
            server_catalog::SELECTED_TARGET_ENV,
            shell_quote(&serde_json::to_string(identity).expect("target identity serializes"))
        );
    }
    for arg in args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    command
}

/// Passed through the command transport so `doctor`, which runs at the
/// selected factory, can compare the binary answering there with the binary
/// that the person invoked. This is deliberately not a public configuration
/// variable.
pub(crate) const CLIENT_VERSION_ENV: &str = "SSF_INTERNAL_CLIENT_VERSION";
pub(crate) const VERSION_REPORTED_ENV: &str = "SSF_INTERNAL_VERSION_REPORTED";

pub(super) fn client_targets(
    mut args: Vec<String>,
    configured: Option<String>,
) -> Result<(Vec<String>, Vec<String>)> {
    let mut servers = Vec::new();
    let mut i = 0;
    while i < args.iter().position(|a| a == "--").unwrap_or(args.len()) {
        if args[i] == "--server" {
            if i + 1 >= args.len() {
                bail!("--server needs an SSH destination");
            }
            servers.push(args.remove(i + 1));
            args.remove(i);
        } else if let Some(value) = args[i].strip_prefix("--server=") {
            servers.push(value.to_owned());
            args.remove(i);
        } else {
            i += 1;
        }
    }
    if servers.is_empty()
        && let Some(configured) = configured
    {
        servers.push(configured);
    }
    Ok((servers, args))
}

pub(super) async fn command_main(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<()> {
    // `ssf launch` links `~/.config/ssf/bin/gh` (and `git`, and `ssf`) to this
    // binary; invoked under a wrapper's name we are that wrapper, not the
    // daemon.
    shim::run_as_shim();
    // This internal value selects service identities. Refuse malformed
    // inherited input before a service helper could fall back to the singleton.
    server_catalog::selected_target_identity()?;
    let args: Vec<std::ffi::OsString> = args.into_iter().collect();
    let forwarded_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let cli = Cli::parse_from(args);
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
        && !factory_vm::in_guest()
        && let cfg = Config::load()?
        && cfg.vm.enabled
    {
        let selected_server = server_catalog::selected_vm_context()?.map(|context| context.name);
        let vm = factory_vm::Vm::new(&cfg);
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
                Command::Status { json: true, watch } => {
                    if watch {
                        loop {
                            println!(
                                "{}",
                                vm_status_for_guest(probe_word(&probe), selected_server.as_deref())
                            );
                            std::io::Write::flush(&mut std::io::stdout())?;
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        }
                    }
                    println!(
                        "{}",
                        vm_status_for_guest(probe_word(&probe), selected_server.as_deref())
                    );
                    return Ok(());
                }
                Command::Status { json: false, .. } => {
                    let identity = match selected_server.as_deref() {
                        Some(server) => format!("server {server} (runtime {})", cfg.vm.name),
                        None => cfg.vm.name.clone(),
                    };
                    println!(
                        "vm:      {identity} is not running (`ssf vm start`, or `ssf ui service enable`)"
                    );
                    return Ok(());
                }
                _ => bail!(why),
            },
            Gate::Send(note) => {
                if let Some(note) = note {
                    eprintln!("{note}");
                }
                if !matches!(cli.command, Command::Status { .. } | Command::Doctor) {
                    vm.ensure_factory_ownership(&cfg)?;
                }
                if matches!(
                    cli.command,
                    Command::Doctor | Command::Status { json: false, .. }
                ) {
                    eprintln!(
                        "host VM{}: {} ({backend}, {}); inspecting guest factory",
                        selected_server
                            .as_deref()
                            .map(|server| format!(" server {server}"))
                            .unwrap_or_default(),
                        cfg.vm.name,
                        probe_word(&probe)
                    );
                }
                let args = forwarded_args;
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
                if matches!(
                    cli.command,
                    Command::Status {
                        json: true,
                        watch: false
                    }
                ) {
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
                            let mut answer: serde_json::Value = serde_json::from_str(&answer)
                                .context("guest status returned invalid JSON")?;
                            answer["factory_location"] = "guest".into();
                            answer["factory_reachable"] = true.into();
                            if let Some(server) = &selected_server {
                                answer["server"] = server.clone().into();
                                answer["transport"] = "vm".into();
                            }
                            answer["host_vm"] = serde_json::json!({
                                "name": cfg.vm.name, "backend": backend,
                                "state": probe_word(&probe),
                                "service_enabled": factory_ui::service_enabled(),
                                "server": selected_server,
                            });
                            println!("{answer}");
                            std::process::exit(
                                out.map(|o| o.status.code().unwrap_or(1)).unwrap_or(1),
                            );
                        }
                        None => {
                            println!(
                                "{}",
                                vm_status_for_guest(probe_word(&probe), selected_server.as_deref())
                            );
                            return Ok(());
                        }
                    }
                }
                let st = vm
                    .exec_ssf(&args)
                    .with_context(|| format!("running `ssf {name}` in the VM"))?;
                std::process::exit(st.code().unwrap_or(1));
            }
        }
    }

    match cli.command {
        Command::LoginProbe { harness } => {
            std::process::exit(i32::from(
                login::probe(&harness).state != login::LoginState::SignedIn,
            ));
        }
        Command::VmInit { seed } => {
            factory_vm::initialize_guest_factory(&seed, &config::config_dir())
        }
        Command::Server { .. } => bail!("run `ssf server` on the client computer"),
        Command::Setup => setup::run(),
        Command::Auth { command } => auth(command).await,
        Command::Token => {
            let cfg = Config::load()?;
            println!("{}", cfg.github_token()?);
            Ok(())
        }
        Command::Repo { command } => repo(command),
        Command::Models { harness, json } => {
            let available = models::available(&harness)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&available)?);
            } else {
                for id in &available.models {
                    println!("{id}");
                }
                eprintln!("source: {}", available.source.describe(&harness));
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
        Command::Status { json, watch } => status(json, watch).await,
        Command::Dashboard => bail!("run `ssf dashboard` on the client computer"),
        Command::Peers { json, repo, all } => peers(json, repo, all).await,
        Command::Candidates { json, repo } => candidates(repo, json).await,
        Command::Adopt { items, json } => adopt(items, json).await,
        Command::Sub { item, r#as, json } => sub(&item, r#as.as_deref(), json, true).await,
        Command::Unsub { item, r#as, json } => sub(&item, r#as.as_deref(), json, false).await,
        Command::Subs { r#as, json } => subs(r#as.as_deref(), json),
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
        Command::Assign {
            item,
            harness,
            model,
            effort,
            r#as,
            json,
        } => {
            assign(
                &item,
                &harness,
                model.as_deref(),
                effort.as_deref(),
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
        Command::Skill { topic } => super::skill::print(topic),
        Command::Guide => {
            let state_bot = state::State::load().ok().and_then(|state| state.bot_login);
            let config_bot = Config::load().ok().and_then(|cfg| cfg.github.login);
            let bot = configured_bot_login(
                std::env::var("SSF_BOT").ok().as_deref(),
                state_bot.as_deref(),
                config_bot.as_deref(),
            )
            .unwrap_or_else(|| "<bot>".into());
            print!("{}", prompt::guide(&bot, factory_vm::in_guest()));
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
            } else if factory_vm::in_guest() {
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

/// Run the `ssf-server` daemon.
pub async fn server_main() -> Result<()> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args.get(1).is_some_and(|arg| arg == "__target-version") {
        server_catalog::selected_target_identity()?;
        let cfg = Config::load()?;
        if !factory_vm::in_guest() && cfg.vm.enabled {
            let vm = factory_vm::Vm::new(&cfg);
            println!(
                "ssf-server {}",
                vm.ssh_output(&["ssf-server", "--version"])?
            );
        } else {
            println!("ssf-server {}", env!("CARGO_PKG_VERSION"));
        }
        return Ok(());
    }
    if args.get(1).is_some_and(|arg| arg == "__client") {
        let command_args =
            std::iter::once(std::ffi::OsString::from("ssf")).chain(args.into_iter().skip(2));
        return command_main(command_args).await;
    }
    let cli = ServerCli::parse();
    if let Some(target) = &cli.target {
        server_catalog::activate_service_target(target)?;
    }
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

    if cli.once && !factory_vm::in_guest() {
        let cfg = Config::load()?;
        if cfg.vm.enabled {
            let vm = factory_vm::Vm::new(&cfg);
            if !vm.running_now()? {
                bail!(
                    "the factory runs in VM {}, which is not running; `ssf vm start` first",
                    cfg.vm.name
                );
            }
            let status = vm.exec_server_once()?;
            if !status.success() {
                eprintln!("`ssf-server --once` failed in VM {}", cfg.vm.name);
            }
            std::process::exit(status.code().unwrap_or(1));
        }
    }
    run(cli.once).await
}
