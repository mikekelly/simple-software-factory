//! Whether a harness is signed in on this machine, checked the way the
//! harness itself would: its own status command where it has one (Claude
//! Code, Codex), else the credential file it writes on login or an API key
//! in the environment. The daemon runs where the harnesses run (on the
//! host, or inside the guest when the factory is in a VM), so "this
//! machine" is the right place to ask; `ssf doctor` is forwarded into the
//! guest for the same reason.
//!
//! A probe never signs anything in or out: a logout on a copied
//! credential revokes the session it was copied from (see `docs/vm.md`).

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// What a probe found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoginState {
    SignedIn,
    SignedOut,
    /// ssf has no way to tell for this harness (a keyring, or a provider
    /// configured in a file ssf does not read).
    Unknown,
}

/// The result of asking where the harness runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Probe {
    pub state: LoginState,
    /// What was checked, for a person: `claude auth status`, or a path.
    pub detail: String,
    /// Identity of the credential on disk (size and mtime), when a file is
    /// known: a new login rewrites it, which is how a blocked session
    /// knows the login is back rather than merely still claimed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"))
}

/// The file a harness writes when a person signs in, if ssf knows it:
/// the `vm::LOGINS` table's path under this home, except where the
/// harness's own environment moves it (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`).
pub fn credential_path(harness: &str) -> Option<PathBuf> {
    let p = match harness {
        "claude" => std::env::var("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home().join(".claude"))
            .join(".credentials.json"),
        "codex" => std::env::var("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home().join(".codex"))
            .join("auth.json"),
        other => home().join(crate::vm::login(other)?.credential),
    };
    Some(p)
}

/// Environment variables that stand in for a login with that harness.
fn api_key_vars(harness: &str) -> &'static [&'static str] {
    match harness {
        "claude" => &["ANTHROPIC_API_KEY"],
        "codex" => &["OPENAI_API_KEY"],
        "gemini" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        "copilot" => &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"],
        "grok" => &["XAI_API_KEY"],
        "pi" | "omp" | "opencode" | "crush" => &[
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "OPENROUTER_API_KEY",
            "GEMINI_API_KEY",
            "XAI_API_KEY",
        ],
        _ => &[],
    }
}

fn key_in_env(harness: &str) -> Option<&'static str> {
    api_key_vars(harness)
        .iter()
        .copied()
        .find(|v| std::env::var(v).is_ok_and(|s| !s.trim().is_empty()))
}

/// Size and mtime of the credential file, or `None` without one.
pub fn fingerprint(harness: &str) -> Option<String> {
    let path = credential_path(harness)?;
    let meta = std::fs::metadata(&path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some(format!("{}:{mtime}", meta.len()))
}

/// Run a harness's status command with a timeout; `None` when it could
/// not be run at all (not installed, hung).
fn run(program: &str, args: &[&str], timeout: Duration) -> Option<(bool, String)> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // A harness started from inside a Claude Code session refuses to nest;
    // the status command does not need the marker.
    cmd.env_remove("CLAUDECODE");
    let mut child = cmd.spawn().ok()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut so) = child.stdout.take() {
                    use std::io::Read;
                    let _ = so.read_to_string(&mut out);
                }
                return Some((status.success(), out));
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

const STATUS_TIMEOUT: Duration = Duration::from_secs(15);

/// Claude Code: `claude auth status --json` prints `{"loggedIn": bool}`
/// (exit 1 when signed out).
fn probe_claude() -> Probe {
    let detail = "claude auth status".to_string();
    match run("claude", &["auth", "status", "--json"], STATUS_TIMEOUT) {
        Some((_, out)) => match serde_json::from_str::<serde_json::Value>(&out) {
            Ok(v) => match v.get("loggedIn").and_then(|b| b.as_bool()) {
                Some(true) => Probe {
                    state: LoginState::SignedIn,
                    detail: format!(
                        "{detail}: signed in{}",
                        v.get("authMethod")
                            .and_then(|m| m.as_str())
                            .map(|m| format!(" ({m})"))
                            .unwrap_or_default()
                    ),
                    fingerprint: fingerprint("claude"),
                },
                Some(false) => Probe {
                    state: LoginState::SignedOut,
                    detail: format!("{detail}: not signed in"),
                    fingerprint: fingerprint("claude"),
                },
                None => Probe {
                    state: LoginState::Unknown,
                    detail: format!("{detail}: no loggedIn field in its output"),
                    fingerprint: fingerprint("claude"),
                },
            },
            Err(_) => Probe {
                state: LoginState::Unknown,
                detail: format!("{detail}: output is not JSON"),
                fingerprint: fingerprint("claude"),
            },
        },
        None => Probe {
            state: LoginState::Unknown,
            detail: format!("{detail}: could not run claude"),
            fingerprint: fingerprint("claude"),
        },
    }
}

/// Codex: `codex login status` prints `Logged in using ...` (exit 0) or
/// `Not logged in` (exit 1).
fn probe_codex() -> Probe {
    let detail = "codex login status".to_string();
    match run("codex", &["login", "status"], STATUS_TIMEOUT) {
        Some((ok, out)) => {
            let text = out.trim();
            let lower = text.to_lowercase();
            let state = if lower.contains("not logged in") {
                LoginState::SignedOut
            } else if ok || lower.contains("logged in") {
                LoginState::SignedIn
            } else {
                LoginState::Unknown
            };
            Probe {
                state,
                detail: format!("{detail}: {}", text.lines().last().unwrap_or("no output")),
                fingerprint: fingerprint("codex"),
            }
        }
        None => Probe {
            state: LoginState::Unknown,
            detail: format!("{detail}: could not run codex"),
            fingerprint: fingerprint("codex"),
        },
    }
}

/// A harness without a status command: signed in when its credential
/// file has content (and, where the file exists before any login, the
/// token in it) or an API key is in the environment.
fn probe_file(harness: &str) -> Probe {
    let key = key_in_env(harness);
    let path = credential_path(harness);
    let must_contain = crate::vm::login(harness).and_then(|l| l.must_contain);
    let present = path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .is_some_and(|text| {
            text.trim().len() > 2 && must_contain.is_none_or(|needle| text.contains(needle))
        });
    let shown = path
        .as_deref()
        .map(tilde)
        .unwrap_or_else(|| "no credential file known".into());
    let (state, detail) = match (present, key) {
        (true, _) => (LoginState::SignedIn, format!("{shown} present")),
        (false, Some(var)) => (LoginState::SignedIn, format!("{var} set")),
        (false, None) if path.is_some() => (
            LoginState::SignedOut,
            format!(
                "no {shown}{}",
                match api_key_vars(harness) {
                    [] => String::new(),
                    vars => format!(" and no {}", vars.join("/")),
                }
            ),
        ),
        (false, None) => (LoginState::Unknown, shown),
    };
    Probe {
        state,
        detail,
        fingerprint: fingerprint(harness),
    }
}

fn tilde(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    match home().to_str() {
        Some(h) if s.starts_with(h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

/// Is `harness` signed in where this process runs?
pub fn probe(harness: &str) -> Probe {
    match harness {
        "claude" => probe_claude(),
        "codex" => probe_codex(),
        // Copilot keeps its login in the keyring where there is one (the
        // host); the file in the table is its fallback (the guest).
        "copilot" => match (probe_file("copilot"), crate::vm::in_guest()) {
            (p, _) if p.state == LoginState::SignedIn => p,
            (p, true) => p,
            (p, false) => Probe {
                state: LoginState::Unknown,
                detail: format!(
                    "{}; on the host copilot may hold its login in the keyring",
                    p.detail
                ),
                fingerprint: p.fingerprint,
            },
        },
        // Crush's providers live in its config; the table's file is only
        // its Copilot login.
        "crush" => Probe {
            state: LoginState::Unknown,
            detail: "crush keeps its providers in its config; `crush login` or an API key".into(),
            fingerprint: None,
        },
        other if crate::vm::login(other).is_some() => probe_file(other),
        _ => Probe {
            state: LoginState::Unknown,
            detail: format!("ssf has no login check for {harness}"),
            fingerprint: None,
        },
    }
}

/// The command that signs `harness` in, as a person would run it on the
/// host; inside the VM the guest needs its own login, which `ssf vm login
/// <harness>` (or `ssf vm ssh` and the same command) provides.
pub fn how_to_sign_in(harness: &str) -> String {
    let host = match harness {
        "gemini" => "gemini (pick \"Sign in with Google\")".to_string(),
        "pi" | "omp" => format!("{harness}, then /login"),
        other => match crate::vm::login(other) {
            Some(l) => l.argv.join(" "),
            None => format!("sign {other} in"),
        },
    };
    if crate::vm::in_guest() {
        format!("`ssf vm login {harness}` on the host (or `ssf vm ssh`, then `{host}`)")
    } else {
        format!("`{host}` on the host")
    }
}

/// The harness's display name for people.
pub fn display_name(harness: &str) -> String {
    match harness {
        "claude" => "Claude Code".into(),
        "codex" => "Codex".into(),
        "gemini" => "Gemini CLI".into(),
        "copilot" => "GitHub Copilot".into(),
        "opencode" => "OpenCode".into(),
        "pi" => "Pi".into(),
        "omp" => "Oh My Pi".into(),
        "grok" => "Grok".into(),
        "crush" => "Crush".into(),
        other => other.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_paths_follow_the_harness_homes() {
        assert!(
            credential_path("claude")
                .unwrap()
                .ends_with(".credentials.json")
        );
        assert!(
            credential_path("codex")
                .unwrap()
                .ends_with(".codex/auth.json")
        );
        assert!(
            credential_path("pi")
                .unwrap()
                .ends_with(".pi/agent/auth.json")
        );
        // The rest follow the VM login table.
        assert!(
            credential_path("copilot")
                .unwrap()
                .ends_with(".copilot/config.json")
        );
        assert!(credential_path("nope").is_none());
    }

    #[test]
    fn unknown_harnesses_cannot_be_told() {
        let p = probe("nope");
        assert_eq!(p.state, LoginState::Unknown);
        assert!(p.fingerprint.is_none());
        assert!(how_to_sign_in("claude").contains("claude auth login"));
        assert!(how_to_sign_in("pi").contains("/login"));
        assert_eq!(display_name("claude"), "Claude Code");
    }
}
