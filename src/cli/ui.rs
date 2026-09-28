use super::prelude::*;
use super::*;

pub(super) async fn ui_cmd(command: UiCommand) -> Result<()> {
    match command {
        UiCommand::Install { quiet } => factory_ui::install_all(quiet),
        UiCommand::Uninstall => factory_ui::uninstall_all(),
        UiCommand::Service { command } => match command {
            ServiceCommand::Enable => {
                factory_ui::set_service_enabled(true)?;
                println!("service enabled");
                Ok(())
            }
            ServiceCommand::Disable => {
                factory_ui::set_service_enabled(false)?;
                println!("service disabled");
                stop_supervised_vm().await
            }
            ServiceCommand::Toggle => {
                let next = !factory_ui::service_enabled();
                factory_ui::set_service_enabled(next)?;
                println!("service {}", if next { "enabled" } else { "disabled" });
                if next {
                    Ok(())
                } else {
                    stop_supervised_vm().await
                }
            }
            ServiceCommand::IsEnabled => {
                if factory_ui::service_enabled() {
                    Ok(())
                } else {
                    std::process::exit(1)
                }
            }
            ServiceCommand::Status { json } => {
                let enabled = factory_ui::service_enabled();
                let active = factory_ui::service_active();
                let failed = factory_ui::service_failed();
                let configured = setup::complete();
                if json {
                    println!(
                        "{}",
                        json!({"server": server_catalog::selected_target_name(), "unit": platform::service_unit(), "enabled": enabled, "active": active, "failed": failed, "configured": configured})
                    );
                } else {
                    if let Some(server) = server_catalog::selected_target_name() {
                        println!("server:     {server}");
                    }
                    println!(
                        "unit:       {}\nconfigured: {configured}\nenabled:    {enabled}\nactive:     {active}\nfailed:     {failed}",
                        platform::service_unit()
                    );
                }
                Ok(())
            }
        },
    }
}

/// Stopping the service ends only the supervisor: the VM has a lifetime of
/// its own, so that restarting the service (a package upgrade does) leaves
/// the guest running. Disabling the service is the explicit "turn the
/// factory off", so it stops the VM as well.
async fn stop_supervised_vm() -> Result<()> {
    if factory_vm::in_guest() {
        return Ok(());
    }
    let cfg = Config::load()?;
    if !cfg.vm.enabled {
        return Ok(());
    }
    factory_vm::Vm::new(&cfg).stop().await
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
pub(super) fn reports_backend_tooling(in_guest: bool) -> bool {
    !in_guest
}

/// What `ssf doctor` says on a VM host about a `[dashboard]` of its own: the
/// guest daemon serves the dashboard with the guest's `[dashboard]` (#653),
/// so the host's is not read. Nothing migrates it.
pub(super) fn unused_host_dashboard_note(host: &Config) -> Option<&'static str> {
    (host.vm.enabled && host.dashboard.enabled).then_some(
        "[dashboard] in this host's config is unused in VM mode: the guest daemon serves the dashboard with the guest's own [dashboard] (`ssf config set dashboard.enabled true` sets it there)",
    )
}
