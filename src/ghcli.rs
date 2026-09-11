//! The GitHub CLI's account store. `gh` keeps one token per account in the
//! system keyring, so the bot can be signed in through gh's own browser flow
//! and its token read back at runtime without ssf storing it.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Scopes the bot token needs: issues/PRs/pushes, Projects boards, plus key
/// enrollment.
pub const REQUIRED_SCOPES: &[&str] = &[
    "repo",
    "project",
    "admin:public_key",
    "admin:ssh_signing_key",
];

#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    pub login: String,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub scopes: String,
}

impl Account {
    pub fn scopes(&self) -> Vec<String> {
        self.scopes
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    pub fn missing_scopes(&self) -> Vec<&'static str> {
        let have = self.scopes();
        REQUIRED_SCOPES
            .iter()
            .copied()
            .filter(|s| !have.iter().any(|h| h == s))
            .collect()
    }
}

#[derive(Deserialize)]
struct StatusJson {
    #[serde(default)]
    hosts: BTreeMap<String, Vec<Account>>,
}

pub fn available() -> bool {
    Command::new("gh")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Accounts gh knows for `host`, active one flagged.
pub fn accounts(host: &str) -> Result<Vec<Account>> {
    let out = Command::new("gh")
        .args(["auth", "status", "--hostname", host, "--json", "hosts"])
        .output()
        .context("running gh auth status (is github-cli installed?)")?;
    if !out.status.success() {
        // gh exits 1 when nobody is logged in, with an empty body.
        let text = String::from_utf8_lossy(&out.stdout);
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
    }
    let parsed: StatusJson =
        serde_json::from_slice(&out.stdout).context("decoding gh auth status")?;
    Ok(parsed.hosts.get(host).cloned().unwrap_or_default())
}

/// The token gh's own store holds for `login`. Inside an agent session
/// `GH_CONFIG_DIR` points at ssf's empty gh directory and `GH_TOKEN` is the
/// bot, so both are dropped: this is the one place that reads another
/// account's token on purpose (`git.credential = "token:<login>"`).
pub fn token_for(host: &str, login: &str) -> Result<String> {
    let out = Command::new("gh")
        .args(["auth", "token", "--hostname", host, "--user", login])
        .env_remove("GH_CONFIG_DIR")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .output()
        .context("running gh auth token")?;
    if !out.status.success() {
        bail!(
            "gh has no token for @{login} on {host}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if token.is_empty() {
        bail!("gh returned an empty token for @{login}");
    }
    Ok(token)
}

pub fn switch_to(host: &str, login: &str) -> Result<()> {
    let out = Command::new("gh")
        .args(["auth", "switch", "--hostname", host, "--user", login])
        .output()
        .context("running gh auth switch")?;
    if !out.status.success() {
        bail!(
            "gh auth switch --user {login} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Current `git_protocol` preference for `host` in gh's config, if any.
fn git_protocol(host: &str) -> Option<String> {
    let out = Command::new("gh")
        .args(["config", "get", "--host", host, "git_protocol"])
        .output()
        .ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !v.is_empty() {
        Some(v)
    } else {
        None
    }
}

/// Interactive browser sign-in; the account is whoever signs in. gh makes
/// the new account active, so callers switch back afterwards. gh also
/// records a git protocol for the host as part of login; ssf does not use
/// it, so the human's existing preference is put back.
pub fn login_web(host: &str, scopes: &[&str]) -> Result<()> {
    let previous_protocol = git_protocol(host);
    let status = Command::new("gh")
        .args([
            "auth",
            "login",
            "--hostname",
            host,
            "--web",
            "--git-protocol",
            previous_protocol.as_deref().unwrap_or("https"),
            "--skip-ssh-key",
            "--scopes",
            &scopes.join(","),
        ])
        .status()
        .context("running gh auth login")?;
    if let Some(prev) = previous_protocol {
        let _ = Command::new("gh")
            .args(["config", "set", "--host", host, "git_protocol", &prev])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    if !status.success() {
        bail!("gh auth login did not complete");
    }
    Ok(())
}

/// Run GitHub's device flow inside the guest. The caller must validate the
/// account and save the returned token with `config::save_token` before using it.
/// No system keyring or existing gh account is read or changed.
pub fn login_device(host: &str, scopes: &[&str]) -> Result<String> {
    login_device_using(host, scopes, &crate::config::config_dir(), Path::new("gh"))
}

struct DeviceLoginDir(PathBuf);

impl DeviceLoginDir {
    fn create(root: &Path) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;

        std::fs::create_dir_all(root).context("creating guest credential directory")?;
        // Keep even the temporary OAuth result on the persistent guest data disk.
        // create_dir fails closed if an interrupted attempt with this PID remains.
        let path = root.join(format!(".device-login-{}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .with_context(|| {
                format!(
                    "creating private device-login directory {}; if an interrupted login left it behind, remove that directory and retry",
                    path.display()
                )
            })?;
        Ok(Self(path))
    }
}

impl Drop for DeviceLoginDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn device_command(program: &Path, config_dir: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_ENTERPRISE_TOKEN")
        .env_remove("GITHUB_ENTERPRISE_TOKEN")
        .env_remove("GH_DEBUG")
        .env_remove("DEBUG")
        .env_remove("GH_DEBUG_API_REQUESTS")
        .env_remove("GH_FORCE_TTY")
        .env("GH_CONFIG_DIR", config_dir)
        .env("GH_BROWSER", "false")
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null());
    command
}

fn login_device_using(host: &str, scopes: &[&str], root: &Path, program: &Path) -> Result<String> {
    let directory = DeviceLoginDir::create(root)?;
    let status = device_command(program, &directory.0)
        .args([
            "auth",
            "login",
            "--hostname",
            host,
            "--web",
            "--git-protocol",
            "https",
            "--skip-ssh-key",
            "--insecure-storage",
            "--scopes",
            &scopes.join(","),
        ])
        .status()
        .context("running guest GitHub device login (is github-cli installed?)")?;
    if !status.success() {
        bail!(
            "guest GitHub device login did not complete; existing bot credentials were not changed"
        );
    }
    let out = device_command(program, &directory.0)
        .args(["auth", "token", "--hostname", host])
        .output()
        .context("reading guest device-login token")?;
    if !out.status.success() {
        bail!("could not read the token from the guest device login; retry `ssf auth login`");
    }
    let token = String::from_utf8(out.stdout).context("decoding guest device-login token")?;
    let token = token.trim().to_string();
    if token.is_empty() {
        bail!("guest device login returned an empty token");
    }
    Ok(token)
}

/// Interactive scope upgrade for the *active* account.
pub fn refresh_scopes(host: &str, scopes: &[&str]) -> Result<()> {
    let status = Command::new("gh")
        .args([
            "auth",
            "refresh",
            "--hostname",
            host,
            "--scopes",
            &scopes.join(","),
        ])
        .status()
        .context("running gh auth refresh")?;
    if !status.success() {
        bail!("gh auth refresh did not complete");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn device_login_isolates_credentials_and_cleans_temporary_store() {
        let root =
            std::env::temp_dir().join(format!("ssf-device-login-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let cleanup = DeviceLoginDir(root.clone());
        let private = DeviceLoginDir::create(&root).unwrap();
        assert_eq!(
            std::fs::metadata(&private.0).unwrap().permissions().mode() & 0o777,
            0o700
        );
        drop(private);
        let program = root.join("gh");
        std::fs::write(
            &program,
            r#"#!/bin/sh
set -eu
test "$GH_PROMPT_DISABLED" = 1
test "$GH_BROWSER" = false
if [ "$2" = login ]; then
    case " $* " in *" --insecure-storage "*) ;; *) exit 9 ;; esac
    test ! -t 0
    printf 'device-token\n' > "$GH_CONFIG_DIR/token"
else
    cat "$GH_CONFIG_DIR/token"
fi
"#,
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            login_device_using("github.com", REQUIRED_SCOPES, &root, &program).unwrap(),
            "device-token"
        );
        let temporary = root.join(format!(".device-login-{}", std::process::id()));
        assert!(!temporary.exists());
        std::fs::write(&program, "#!/bin/sh\nexit 1\n").unwrap();
        assert!(login_device_using("github.com", REQUIRED_SCOPES, &root, &program).is_err());
        assert!(!temporary.exists());
        // An interrupted attempt is never silently reused or deleted.
        std::fs::create_dir(&temporary).unwrap();
        std::fs::write(temporary.join("token"), "interrupted-token").unwrap();
        assert!(login_device_using("github.com", REQUIRED_SCOPES, &root, &program).is_err());
        assert_eq!(
            std::fs::read_to_string(temporary.join("token")).unwrap(),
            "interrupted-token"
        );
        drop(cleanup);
    }

    #[test]
    fn device_command_removes_inherited_authentication_and_debug_settings() {
        let command = device_command(Path::new("gh"), Path::new("/guest/private"));
        let env: BTreeMap<_, _> = command.get_envs().collect();
        for name in [
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_ENTERPRISE_TOKEN",
            "GH_DEBUG",
            "DEBUG",
            "GH_DEBUG_API_REQUESTS",
            "GH_FORCE_TTY",
        ] {
            assert_eq!(env.get(std::ffi::OsStr::new(name)), Some(&None));
        }
        assert_eq!(
            env.get(std::ffi::OsStr::new("GH_CONFIG_DIR")),
            Some(&Some(std::ffi::OsStr::new("/guest/private")))
        );
    }
}
