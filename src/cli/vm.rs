use super::prelude::*;
use super::*;
#[derive(Debug, PartialEq)]
pub(super) enum Gate {
    /// Send it to the guest; `Some` is what to say on stderr first, when
    /// the answer was that there is no answer.
    Send(Option<String>),
    /// The VM is not running: this is why the command cannot run.
    Refuse(String),
}

/// The gate every command the host forwards into the guest goes through.
/// `probe` is [`factory_vm::Vm::running_now`]'s answer or the reason there is
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
pub(super) fn forwarding_gate(
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
pub(super) fn probe_word(probe: &Result<bool, String>) -> &'static str {
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
pub(super) fn vm_status_for_guest(vm: &str) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "vm": vm, "service_active": false, "service_enabled": factory_ui::service_enabled(),
        "factory_location": "guest", "factory_reachable": false,
        "host_vm": { "state": vm },
        "sessions": [], "repos": [],
    });
    payload["dashboard"] =
        status::dashboard_presentation(&payload).expect("VM status always contains sessions");
    payload
}

/// The name of a command that runs in the guest when the factory is in a VM.
pub(super) fn forwarded_name(cmd: &Command) -> Option<&'static str> {
    let name = match cmd {
        Command::Auth { .. } => "auth",
        Command::Token => "token",
        Command::Repo { .. } => "repo",
        Command::Agents { .. } => "agents",
        Command::Models { .. } => "models",
        Command::Config { command } => match command {
            Some(ConfigCommand::Get { key } | ConfigCommand::Set { key, .. })
                if key == "vm"
                    || key.starts_with("vm.")
                    || key == "dashboard"
                    || key.starts_with("dashboard.") =>
            {
                return None;
            }
            _ => "config",
        },
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
        _ => return None,
    };
    factory_vm::forwards(name).then_some(name)
}

/// A guest's stderr already says why its command failed. For the one-shot
/// engine command, name the guest as well: from the host a person otherwise
/// cannot tell which daemon owns the refused state directory.
pub(super) async fn vm_cmd(command: VmCommand) -> Result<()> {
    let cfg = Config::load()?;
    let vm = factory_vm::Vm::new(&cfg);
    match command {
        VmCommand::Build {
            force,
            vcpus,
            mem_mib,
            data_gib,
        } => {
            let mut cfg = cfg;
            size_vm(&mut cfg, &vm.base, [vcpus, mem_mib, data_gib])?;
            factory_vm::Vm::new(&cfg).build(&cfg, force).await
        }
        VmCommand::Grow { data_gib } => {
            if let Some(n) = vm.grow(data_gib)? {
                let mut cfg = cfg;
                cfg.vm.data_gib = Some(n);
                cfg.save_vm_settings()?;
                println!(
                    "[vm] data_gib = {n} written to {}",
                    config::config_path().display()
                );
            }
            Ok(())
        }
        VmCommand::Start => {
            let orca = factory_vm::orca_repos(&cfg);
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
                println!("backend:  {}", st.backend);
                if let Some(t) = &st.tooling {
                    println!("tooling:  {}", t.detail);
                }
                match &st.instance {
                    // A `limactl list` that failed is not "no such
                    // instance": saying "missing (ssf vm build)" over a
                    // VM lima could not be asked about sends people to
                    // rebuild one that is already there.
                    Some(inst) => println!(
                        "instance: {inst}{}",
                        match (&st.probe_error, &st.lima_dir, st.image) {
                            (Some(e), ..) => format!(" unknown: {e}"),
                            (None, Some(d), _) => format!(" ({d})"),
                            (None, None, false) => " missing (ssf vm build)".to_string(),
                            (None, None, true) => String::new(),
                        }
                    ),
                    None => println!(
                        "image:    {}",
                        if st.image {
                            "built"
                        } else {
                            "missing (ssf vm build)"
                        }
                    ),
                }
                println!(
                    "state:    {}",
                    match (&st.probe_error, st.running, st.firecracker_pid) {
                        (Some(_), ..) => "unknown (lima did not answer)".to_string(),
                        (None, Some(true), Some(p)) => format!("running (firecracker pid {p})"),
                        (None, Some(true), None) => "running".to_string(),
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
                println!(
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
                    println!("logins:   {}", login_summary(&st.logins));
                }
            }
            Ok(())
        }
        VmCommand::Login { harness } => {
            let login = match harness {
                Some(h) => factory_vm::login(&h).with_context(|| {
                    format!(
                        "no login flow for `{h}`; one of {}",
                        factory_vm::LOGINS
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
        VmCommand::Tailscale => exit_with(vm.tailscale()?),
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
                        factory_vm::BackendKind::Lima => format!(
                            ", the lima instance {} and its disk {}",
                            vm.lima_name(),
                            vm.lima_disk_name()
                        ),
                        factory_vm::BackendKind::Firecracker => String::new(),
                    }
                );
            }
            vm.destroy().await
        }
    }
}

pub(super) fn exit_with(st: std::process::ExitStatus) -> Result<()> {
    std::process::exit(st.code().unwrap_or(1));
}

/// `ssf vm build`'s sizing: a `--vcpus/--mem-mib/--data-gib` flag is
/// written to `[vm]`; a key set there stays; a key set nowhere gets the
/// rule for this machine and is written too. The choice is printed with
/// where each value came from. `[vm] backend` is settled the same way
/// (the platform's default, written once).
pub(super) fn size_vm(cfg: &mut Config, base: &Path, flags: [Option<u32>; 3]) -> Result<()> {
    let (backend, backend_from, backend_changed) =
        factory_vm::choose_backend(&mut cfg.vm, factory_vm::BackendKind::platform_default());
    println!("VM backend: {backend} ({backend_from})");
    // The backend decides which filesystem the data disk will fill, so it
    // has to be settled before the machine is measured.
    let (dir, what) = factory_vm::sizing_dir(backend, base);
    let facts = factory_vm::HostFacts::probe(&dir)?;
    let mut chosen = factory_vm::choose_sizes(&mut cfg.vm, flags, factory_vm::sizes_for(&facts));
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
        cfg.save_vm_settings()?;
        println!(
            "written to {} under [vm] (backend, vcpus, mem_mib, data_gib); edit them there. The data disk itself is made once and only enlarged by `ssf vm grow`",
            config::config_path().display()
        );
    }
    Ok(())
}
