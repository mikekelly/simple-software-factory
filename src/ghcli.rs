//! The GitHub CLI's account store. `gh` keeps one token per account in the
//! system keyring, so the bot can be signed in through gh's own browser flow
//! and its token read back at runtime without ssf storing it.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
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

pub fn token_for(host: &str, login: &str) -> Result<String> {
    let out = Command::new("gh")
        .args(["auth", "token", "--hostname", host, "--user", login])
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
