use super::prelude::*;
use super::*;

/// Run the `ssf` transport client. With no server configured it execs the
/// adjacent daemon binary directly; `--server HOST` (or `SSF_SERVER`) execs
/// that same command endpoint through ssh.
/// adjacent daemon binary directly; `--server HOST` (or `SSF_SERVER`) execs
/// that same command endpoint through ssh.
pub async fn client_main() -> Result<()> {
    if shim::invoked_as_gh() {
        shim::run();
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let configured = std::env::var("SSF_SERVER").ok().filter(|s| !s.is_empty());
    let (server, args) = client_target(args, configured)?;

    let cli = Cli::parse_from(std::iter::once("ssf".to_owned()).chain(args.clone()));
    if let Command::Dashboard = cli.command {
        return dashboard::run(server).await;
    }

    let err = match server {
        Some(host) => {
            let command = remote_client_command(&args);
            std::process::Command::new("ssh")
                .arg("--")
                .arg(host)
                .arg(command)
                .exec()
        }
        None => std::process::Command::new(server_executable()?)
            .arg("__client")
            .args(args)
            .exec(),
    };
    Err(anyhow::Error::from(err).context("starting ssf-server"))
}

pub(crate) fn remote_client_command(args: &[String]) -> String {
    let mut command = String::from("ssf-server __client");
    for arg in args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    command
}

pub(super) fn client_target(
    mut args: Vec<String>,
    configured: Option<String>,
) -> Result<(Option<String>, Vec<String>)> {
    let mut server = configured;
    let options_end = args.iter().position(|a| a == "--").unwrap_or(args.len());
    if let Some(i) = args[..options_end].iter().position(|a| a == "--server") {
        if i + 1 >= args.len() {
            bail!("--server needs an SSH destination");
        }
        server = Some(args.remove(i + 1));
        args.remove(i);
    } else if let Some((i, value)) = args[..options_end]
        .iter()
        .enumerate()
        .find_map(|(i, a)| a.strip_prefix("--server=").map(|v| (i, v.to_owned())))
    {
        server = Some(value);
        args.remove(i);
    }
    Ok((server, args))
}

pub(super) async fn command_main(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<()> {
    // `ssf launch` links `~/.config/ssf/bin/gh` (and `ssf`) to this binary;
    // invoked under the gh name we are the gh shim, not the daemon.
    if shim::invoked_as_gh() {
        shim::run();
    }
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
                            println!("{}", vm_status_for_guest(probe_word(&probe)));
                            std::io::Write::flush(&mut std::io::stdout())?;
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        }
                    }
                    println!("{}", vm_status_for_guest(probe_word(&probe)));
                    return Ok(());
                }
                Command::Status { json: false, .. } => {
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
                if !matches!(cli.command, Command::Status { .. } | Command::Doctor) {
                    vm.ensure_factory_ownership(&cfg)?;
                }
                if matches!(
                    cli.command,
                    Command::Doctor | Command::Status { json: false, .. }
                ) {
                    eprintln!(
                        "host VM: {} ({backend}, {}); inspecting guest factory",
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
                            answer["host_vm"] = serde_json::json!({
                                "name": cfg.vm.name, "backend": backend,
                                "state": probe_word(&probe),
                                "service_enabled": factory_ui::service_enabled(),
                            });
                            println!("{answer}");
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
        Command::Setup => setup::run(),
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
        Command::Status { json, watch } => status(json, watch).await,
        Command::Dashboard => bail!("run `ssf dashboard` on the client computer"),
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
    if args.get(1).is_some_and(|arg| arg == "__client") {
        let command_args =
            std::iter::once(std::ffi::OsString::from("ssf")).chain(args.into_iter().skip(2));
        return command_main(command_args).await;
    }
    let cli = ServerCli::parse();
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
