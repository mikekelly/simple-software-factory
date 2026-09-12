//! Explicit per-user setup for the package-owned SSF installation.

use anyhow::{Context, Result, bail};
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
    checked(
        "systemctl",
        &["--user", "enable", "--now", "ssf.service"],
        false,
    )
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
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_package() -> Result<()> {
    checked("systemctl", &["--user", "daemon-reload"], false)?;
    let output = Command::new("systemctl")
        .args([
            "--user",
            "show",
            "-p",
            "FragmentPath",
            "--value",
            "ssf.service",
        ])
        .stdin(Stdio::null())
        .output()
        .context("finding ssf.service")?;
    if !output.status.success()
        || String::from_utf8_lossy(&output.stdout).trim() != "/usr/lib/systemd/user/ssf.service"
    {
        bail!(
            "ssf.service does not resolve to the package-owned /usr/lib/systemd/user/ssf.service"
        );
    }
    let exec = Command::new("systemctl")
        .args([
            "--user",
            "show",
            "-p",
            "ExecStart",
            "--value",
            "ssf.service",
        ])
        .stdin(Stdio::null())
        .output()
        .context("checking ssf.service ExecStart")?;
    let exec = String::from_utf8_lossy(&exec.stdout);
    if !effective_exec_is_owned(&exec) {
        bail!("ssf.service does not effectively launch the package-owned `/usr/bin/ssf-server`");
    }
    Ok(())
}

fn effective_exec_is_owned(exec: &str) -> bool {
    exec.matches("path=").count() == 1
        && exec.contains("path=/usr/bin/ssf-server ;")
        && exec.contains("argv[]=/usr/bin/ssf-server ;")
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
    checked("brew", &["services", "start", "ssf"], false)
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
    run_verified_setup(
        verify_binary,
        migrate_legacy_runtime,
        verify_package,
        finish_setup,
    )
}

fn verify_existing_config() -> Result<()> {
    if config::config_path().exists() {
        Config::load()
            .context("validating the existing SSF configuration (preserved unchanged)")?;
    }
    Ok(())
}

fn run_verified_setup(
    verify_files: impl FnOnce() -> Result<()>,
    migrate: impl FnOnce() -> Result<bool>,
    verify_effective_unit: impl FnOnce() -> Result<()>,
    finish: impl FnOnce() -> Result<()>,
) -> Result<()> {
    verify_files()?;
    if migrate()? {
        println!("migrated the legacy marketplace runtime");
    }
    verify_effective_unit()?;
    finish()
}

fn finish_setup() -> Result<()> {
    enable_linger()?;
    std::fs::create_dir_all(config::state_dir()).context("creating the SSF state directory")?;
    if !config::config_path().exists() {
        Config::default().save()?;
    }
    Config::load().context("validating the existing SSF configuration (preserved unchanged)")?;
    enable_service()?;
    crate::config::write_atomic(&completion_marker(), b"ssf-setup-v1\n", 0o600)?;
    println!("ssf setup complete; service enabled and running");
    println!("next: `ssf auth login --web`, then add a repository with `ssf repo add`");
    Ok(())
}

pub fn completion_marker() -> PathBuf {
    config::state_dir().join("setup-complete")
}

pub fn complete() -> bool {
    Config::load().is_ok()
        && config::config_path().is_file()
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
    fn a_legacy_shadow_is_migrated_before_effective_unit_verification() {
        use std::cell::RefCell;
        let calls = RefCell::new(Vec::new());
        run_verified_setup(
            || {
                calls.borrow_mut().push("files");
                Ok(())
            },
            || {
                calls.borrow_mut().push("migrate-shadow");
                Ok(true)
            },
            || {
                calls.borrow_mut().push("effective-package-unit");
                Ok(())
            },
            || {
                calls.borrow_mut().push("finish");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            *calls.borrow(),
            [
                "files",
                "migrate-shadow",
                "effective-package-unit",
                "finish"
            ]
        );
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
            "{ path=/usr/bin/ssf-server ; argv[]=/usr/bin/ssf-server ; ignore_errors=no ; }"
        ));
        assert!(!effective_exec_is_owned(
            "{ path=/usr/bin/ssf-server ; argv[]=/usr/bin/ssf-server --other ; }"
        ));
        assert!(!effective_exec_is_owned(
            "{ path=/usr/bin/ssf-server ; argv[]=/usr/bin/ssf-server ; } ; { path=/usr/bin/other ; argv[]=/usr/bin/other ; }"
        ));
    }
}
