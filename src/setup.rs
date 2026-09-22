//! Explicit per-user setup for the package-owned SSF installation.

use anyhow::{Context, Result, bail};
#[cfg(target_os = "linux")]
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::config::{self, Config};

fn checked(program: &str, args: &[&str], visible: bool) -> Result<()> {
    let mut command = Command::new(program);
    command.args(args);
    if !visible {
        command.stdin(Stdio::null());
    }
    let status = command
        .status()
        .with_context(|| format!("running `{program} {}`", args.join(" ")))?;
    if !status.success() {
        bail!("`{program} {}` exited with {status}", args.join(" "));
    }
    Ok(())
}

fn legacy_runtime() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".local/share/ssf/marketplace"))
}

/// Remove only a runtime carrying the old installer's ownership marker. The
/// old helper performs its own strict manifest validation and preserves work.
fn migrate_legacy_runtime() -> Result<bool> {
    let Some(runtime) = legacy_runtime() else {
        return Ok(false);
    };
    if !runtime.exists() {
        return Ok(false);
    }
    let metadata = runtime.join("install.env");
    let helper = runtime.join("ssf-marketplace");
    for path in [&runtime, &metadata, &helper] {
        if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            bail!("refusing unsafe legacy marketplace path {}", path.display());
        }
    }
    let marker = std::fs::read_to_string(&metadata).with_context(|| {
        format!(
            "legacy runtime at {} has no readable ownership marker; preserved",
            runtime.display()
        )
    })?;
    if !marker
        .lines()
        .any(|line| line == "owner=ssf-marketplace-v1")
        || !helper.is_file()
    {
        bail!(
            "legacy runtime at {} is not positively owned by ssf-marketplace-v1; preserved",
            runtime.display()
        );
    }
    checked(
        helper.to_str().context("legacy helper path is not UTF-8")?,
        &["validate-uninstall"],
        false,
    )?;
    checked(
        helper.to_str().context("legacy helper path is not UTF-8")?,
        &["uninstall"],
        false,
    )?;
    Ok(true)
}

#[cfg(target_os = "linux")]
fn enable_service() -> Result<()> {
    checked("systemctl", &["--user", "daemon-reload"], false)?;
    crate::ui::set_service_enabled_on_error(true, crate::ui::OnServiceError::Fail)
}

#[cfg(target_os = "linux")]
fn verify_binary() -> Result<()> {
    checked("/usr/bin/ssf", &["--version"], false)
        .context("the package-owned /usr/bin/ssf is not usable")?;
    checked("/usr/bin/ssf-server", &["--version"], false)
        .context("the package-owned /usr/bin/ssf-server is not usable")?;
    let unit = std::fs::read_to_string("/usr/lib/systemd/user/ssf.service")
        .context("reading the package-owned /usr/lib/systemd/user/ssf.service")?;
    if !unit
        .lines()
        .any(|line| line.trim() == "ExecStart=/usr/bin/ssf-server")
    {
        bail!("the package-owned ssf.service does not launch `/usr/bin/ssf-server`");
    }
    let template = std::fs::read_to_string("/usr/lib/systemd/user/ssf@.service")
        .context("reading the package-owned /usr/lib/systemd/user/ssf@.service")?;
    if !template
        .lines()
        .any(|line| line.trim() == "ExecStart=/usr/bin/ssf-server --target %i")
    {
        bail!("the package-owned ssf@.service does not launch one explicit target");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_package() -> Result<()> {
    checked("systemctl", &["--user", "daemon-reload"], false)?;
    let unit = crate::platform::service_unit();
    let expected_fragment = if crate::platform::service_target().is_some() {
        "/usr/lib/systemd/user/ssf@.service"
    } else {
        "/usr/lib/systemd/user/ssf.service"
    };
    let output = Command::new("systemctl")
        .args(["--user", "show", "-p", "FragmentPath", "--value", &unit])
        .stdin(Stdio::null())
        .output()
        .context("finding ssf.service")?;
    if !output.status.success()
        || String::from_utf8_lossy(&output.stdout).trim() != expected_fragment
    {
        bail!("{unit} does not resolve to the package-owned {expected_fragment}");
    }
    let exec = Command::new("systemctl")
        .args(["--user", "show", "-p", "ExecStart", "--value", &unit])
        .stdin(Stdio::null())
        .output()
        .context("checking ssf.service ExecStart")?;
    let exec = String::from_utf8_lossy(&exec.stdout);
    if !effective_exec_is_owned(&exec, crate::platform::service_target().as_deref()) {
        bail!("{unit} does not effectively launch the selected package-owned server");
    }
    Ok(())
}

#[cfg(any(target_os = "linux", test))]
fn effective_exec_is_owned(exec: &str, target: Option<&str>) -> bool {
    exec.matches("path=").count() == 1
        && exec.contains("path=/usr/bin/ssf-server ;")
        && exec.contains(&match target {
            Some(target) => format!("argv[]=/usr/bin/ssf-server --target {target} ;"),
            None => "argv[]=/usr/bin/ssf-server ;".into(),
        })
}

#[cfg(not(target_os = "linux"))]
fn verify_binary() -> Result<()> {
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn verify_package() -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn enable_service() -> Result<()> {
    crate::ui::set_service_enabled_on_error(true, crate::ui::OnServiceError::Fail)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn enable_service() -> Result<()> {
    bail!("ssf setup does not support this operating system")
}

#[cfg(target_os = "linux")]
fn enable_linger() -> Result<()> {
    let user = std::env::var("USER").context("USER is required to enable linger")?;
    let output = Command::new("loginctl")
        .args(["show-user", &user, "-p", "Linger", "--value"])
        .stdin(Stdio::null())
        .output()
        .context("checking login linger")?;
    if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "yes" {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        bail!(
            "login linger is disabled; run `sudo loginctl enable-linger {user}`, then run `ssf setup` again"
        );
    }
    eprintln!("Enabling login linger so ssf keeps running across logout and starts at boot.");
    checked("sudo", &["loginctl", "enable-linger", &user], true)?;
    let verify = Command::new("loginctl")
        .args(["show-user", &user, "-p", "Linger", "--value"])
        .output()?;
    if !verify.status.success() || String::from_utf8_lossy(&verify.stdout).trim() != "yes" {
        bail!("login linger is still disabled after `sudo loginctl enable-linger {user}`");
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn enable_linger() -> Result<()> {
    Ok(())
}

pub fn run() -> Result<()> {
    verify_existing_config()?;
    verify_binary()?;
    if migrate_legacy_runtime()? {
        println!("migrated the legacy marketplace runtime");
    }
    let target_already_selected = crate::server_catalog::selected_target_identity()?.is_some();
    let target = prepare_target()?;
    activate_prepared_target(
        target.as_deref(),
        target_already_selected,
        crate::server_catalog::activate_service_target,
    )?;
    verify_package()?;
    if target.is_some() {
        stop_legacy_service()?;
    }
    finish_setup()
}

fn activate_prepared_target(
    target: Option<&str>,
    already_selected: bool,
    activate: impl FnOnce(&str) -> Result<()>,
) -> Result<()> {
    if let Some(name) = target
        && !already_selected
    {
        activate(name)?;
    }
    Ok(())
}

fn prepare_target() -> Result<Option<String>> {
    if let Some(identity) = crate::server_catalog::selected_target_identity()? {
        return Ok(Some(identity.name));
    }
    let catalog = crate::server_catalog::Catalog::load()?;
    match catalog.len() {
        1 => return Ok(catalog.list().next().map(|(name, _)| name.to_owned())),
        count if count > 1 => {
            bail!("multiple SSF servers are configured; select the one to set up with --server")
        }
        _ => {}
    }
    if crate::config::Config::legacy_vm_settings()?.is_some_and(|vm| vm.enabled) {
        crate::server_catalog::Catalog::migrate_legacy_vm("ssf-server")?;
        println!(
            "Migrated the existing VM in place as server `ssf-server`; its runtime resources and guest data were not moved."
        );
        return Ok(Some("ssf-server".into()));
    }
    if crate::config::config_path().is_file() {
        crate::server_catalog::Catalog::migrate_legacy_local()?;
        println!(
            "Registered the existing host factory as server `local`; its configuration and state were not moved."
        );
        return Ok(Some("local".into()));
    }
    crate::server_catalog::Catalog::add_conventional_vm()?;
    println!("Created the conventional VM server `ssf-server`.");
    println!("It is selected automatically while it is the only configured server.");
    Ok(Some("ssf-server".into()))
}

#[cfg(target_os = "linux")]
fn stop_legacy_service() -> Result<()> {
    if crate::ui::legacy_service_enabled_or_active()? {
        checked(
            "systemctl",
            &["--user", "disable", "--now", crate::platform::SERVICE],
            false,
        )?;
        println!("stopped the legacy singleton service");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn stop_legacy_service() -> Result<()> {
    let plist =
        dirs::home_dir().map(|home| home.join("Library/LaunchAgents/homebrew.mxcl.ssf.plist"));
    if crate::platform::legacy_service_active() || plist.is_some_and(|path| path.is_file()) {
        checked("brew", &["services", "stop", "ssf"], false)?;
        println!("stopped the legacy singleton service");
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn stop_legacy_service() -> Result<()> {
    Ok(())
}

fn verify_existing_config() -> Result<()> {
    if config::config_path().exists() {
        Config::load()
            .context("validating the existing SSF configuration (preserved unchanged)")?;
    }
    Ok(())
}

fn finish_setup() -> Result<()> {
    enable_linger()?;
    // The bar widget the package used to ship is gone (#413); this is the
    // per-user setup step, so an upgraded installation that runs it again is
    // cleaned up without anyone remembering to. Nothing is there on a fresh
    // install, and nothing here is Omarchy-specific beyond the widget itself.
    match crate::ui::remove_superseded_widget() {
        Ok(crate::ui::RemovedWidget::Removed) => {
            println!("disabled and removed the superseded Omarchy bar widget")
        }
        Ok(crate::ui::RemovedWidget::Disabled) => println!(
            "disabled the superseded Omarchy bar widget; `omarchy plugin remove ssf.factory` removes the checkout"
        ),
        Ok(crate::ui::RemovedWidget::Nothing) => {}
        Err(e) => eprintln!("warning: could not remove the superseded Omarchy bar widget: {e:#}"),
    }
    std::fs::create_dir_all(config::state_dir()).context("creating the SSF state directory")?;
    let vm_target = crate::server_catalog::selected_target_identity()?
        .is_some_and(|identity| identity.transport == "vm");
    if !vm_target && !config::config_path().exists() {
        Config::default().save()?;
    }
    Config::load().context("validating the existing SSF configuration (preserved unchanged)")?;
    enable_service()?;
    let marker = completion_marker();
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent).context("creating the setup marker directory")?;
    }
    crate::config::write_atomic(&marker, b"ssf-setup-v1\n", 0o600)?;
    println!("ssf setup complete; selected service enabled");
    if vm_target {
        println!("next: `ssf vm build`, then `ssf auth login --web` in the guest");
    } else {
        println!("next: `ssf auth login --web`, then add a repository with `ssf repo add`");
    }
    Ok(())
}

pub fn completion_marker() -> PathBuf {
    match crate::server_catalog::selected_target_identity() {
        Ok(Some(identity)) if identity.transport == "vm" => config::client_state_dir()
            .join("setup")
            .join(format!("{}-complete", identity.name)),
        _ => config::state_dir().join("setup-complete"),
    }
}

pub fn complete() -> bool {
    let vm_target = crate::server_catalog::selected_target_identity()
        .ok()
        .flatten()
        .is_some_and(|identity| identity.transport == "vm");
    Config::load().is_ok()
        && (vm_target || config::config_path().is_file())
        && std::fs::read(completion_marker()).is_ok_and(|body| body == b"ssf-setup-v1\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_requires_the_final_setup_marker() {
        let sandbox = crate::config::test_support::sandbox();
        Config::default().save().unwrap();
        assert!(config::config_path().is_file());
        assert!(!complete(), "a config alone must still offer setup retry");
        std::fs::create_dir_all(sandbox.state_dir()).unwrap();
        crate::config::write_atomic(&completion_marker(), b"ssf-setup-v1\n", 0o600).unwrap();
        assert!(complete());
        std::fs::write(config::config_path(), "not = [valid toml").unwrap();
        assert!(!complete(), "a malformed config must offer setup repair");
        Config::default().save().unwrap();
        assert!(complete());
        std::fs::write(completion_marker(), "wrong\n").unwrap();
        assert!(!complete(), "a malformed marker must not claim readiness");
        crate::config::write_atomic(&completion_marker(), b"ssf-setup-v1\n", 0o600).unwrap();
        std::fs::remove_file(config::config_path()).unwrap();
        assert!(
            !complete(),
            "a marker without its config must offer setup repair"
        );
    }

    #[test]
    fn fresh_setup_creates_the_conventional_vm_target() {
        let _sandbox = crate::config::test_support::sandbox();
        assert!(!config::config_path().exists());
        assert_eq!(prepare_target().unwrap().as_deref(), Some("ssf-server"));
        let catalog = crate::server_catalog::Catalog::load().unwrap();
        let crate::server_catalog::Target::Vm {
            runtime_name,
            config: Some(vm),
            ..
        } = catalog.get("ssf-server").unwrap()
        else {
            panic!("conventional target is not an owned VM")
        };
        assert_eq!(runtime_name, "default");
        assert!(vm.enabled);
        assert!(vm.ssh_port >= 2222);
    }

    #[test]
    fn setup_registers_an_established_host_factory_without_moving_it() {
        let sandbox = crate::config::test_support::sandbox();
        Config::default().save().unwrap();
        let before = std::fs::read(config::config_path()).unwrap();
        assert_eq!(prepare_target().unwrap().as_deref(), Some("local"));
        let catalog = crate::server_catalog::Catalog::load().unwrap();
        assert!(matches!(
            catalog.get("local"),
            Some(crate::server_catalog::Target::Local {
                config_dir: None,
                state_dir: None
            })
        ));
        assert_eq!(std::fs::read(config::config_path()).unwrap(), before);
        assert!(!sandbox.state_dir().join("setup-complete").exists());
    }

    #[test]
    fn setup_adopts_an_established_vm_without_changing_its_resources() {
        let _sandbox = crate::config::test_support::sandbox();
        let mut config = Config::default();
        config.vm.enabled = true;
        config.vm.name = "kept".into();
        config.vm.dir = "/tmp/ssf-established-vm".into();
        config.vm.ssh_port = 43222;
        config.save().unwrap();
        let expected = config.vm.clone();
        assert_eq!(prepare_target().unwrap().as_deref(), Some("ssf-server"));
        let catalog = crate::server_catalog::Catalog::load().unwrap();
        let Some(crate::server_catalog::Target::Vm {
            runtime_name,
            config: Some(actual),
            ..
        }) = catalog.get("ssf-server")
        else {
            panic!("migrated target is not an owned VM")
        };
        assert_eq!(runtime_name, "kept");
        assert_eq!(actual.as_ref(), &expected);
        assert!(Config::legacy_vm_settings().unwrap().is_none());
    }

    #[test]
    fn setup_does_not_reopen_the_client_catalog_after_a_target_is_selected() {
        let mut activated = false;
        activate_prepared_target(Some("local"), true, |_| {
            activated = true;
            Ok(())
        })
        .unwrap();
        assert!(!activated);

        activate_prepared_target(Some("ssf-server"), false, |_| {
            activated = true;
            Ok(())
        })
        .unwrap();
        assert!(activated);
    }

    #[test]
    fn malformed_existing_config_is_refused_and_preserved() {
        let _sandbox = crate::config::test_support::sandbox();
        let malformed = b"not = [valid toml";
        crate::config::write_atomic(&config::config_path(), malformed, 0o600).unwrap();
        let error = verify_existing_config().unwrap_err();
        assert!(format!("{error:#}").contains("preserved unchanged"));
        assert_eq!(std::fs::read(config::config_path()).unwrap(), malformed);
        assert!(!complete());
    }

    #[test]
    fn effective_service_has_exactly_the_one_owned_command() {
        assert!(effective_exec_is_owned(
            "{ path=/usr/bin/ssf-server ; argv[]=/usr/bin/ssf-server ; ignore_errors=no ; }",
            None,
        ));
        assert!(!effective_exec_is_owned(
            "{ path=/usr/bin/ssf-server ; argv[]=/usr/bin/ssf-server --other ; }",
            None,
        ));
        assert!(!effective_exec_is_owned(
            "{ path=/usr/bin/ssf-server ; argv[]=/usr/bin/ssf-server ; } ; { path=/usr/bin/other ; argv[]=/usr/bin/other ; }",
            None,
        ));
        assert!(effective_exec_is_owned(
            "{ path=/usr/bin/ssf-server ; argv[]=/usr/bin/ssf-server --target ssf-server ; }",
            Some("ssf-server"),
        ));
    }
}
