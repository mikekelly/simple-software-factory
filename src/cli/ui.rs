use super::prelude::*;
use super::*;

pub(super) fn ui_cmd(command: UiCommand) -> Result<()> {
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
                Ok(())
            }
            ServiceCommand::Toggle => {
                let next = !factory_ui::service_enabled();
                factory_ui::set_service_enabled(next)?;
                println!("service {}", if next { "enabled" } else { "disabled" });
                Ok(())
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
                        json!({"enabled": enabled, "active": active, "failed": failed, "configured": configured})
                    );
                } else {
                    println!(
                        "configured: {configured}\nenabled:    {enabled}\nactive:     {active}\nfailed:     {failed}"
                    );
                }
                Ok(())
            }
        },
    }
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
