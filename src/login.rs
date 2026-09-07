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

/// What a harness's status command said: whether it succeeded, its
/// standard output and its standard error. Some of them (Codex) say
/// where they stand on stderr and exit non-zero, so both are kept.
struct Status {
    ok: bool,
    out: String,
    err: String,
}

/// Read one of a child's pipes to the end on a thread of its own, and
/// hand what it read back over a channel: the caller waits for it with a
/// deadline rather than joining, since a reader whose pipe some other
/// process still holds open never finishes (see [`run`]).
fn drain<R: std::io::Read + Send + 'static>(pipe: Option<R>) -> Drained {
    let (tx, rx) = std::sync::mpsc::channel();
    let so_far = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let buf = so_far.clone();
    std::thread::spawn(move || {
        if let Some(mut p) = pipe {
            // Read in pieces so what has arrived is there to be taken
            // when the deadline passes with the pipe still open.
            let mut chunk = [0u8; 4096];
            loop {
                match std::io::Read::read(&mut p, &mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut s) = buf.lock() {
                            s.push_str(&String::from_utf8_lossy(&chunk[..n]));
                        }
                    }
                }
            }
        }
        let _ = tx.send(());
    });
    Drained { done: rx, so_far }
}

/// A pipe being read on its own thread: `done` fires at end of file, and
/// `so_far` holds what has arrived either way.
struct Drained {
    done: std::sync::mpsc::Receiver<()>,
    so_far: std::sync::Arc<std::sync::Mutex<String>>,
}

impl Drained {
    /// What was read, waiting up to `for_` for the end of the pipe; a pipe
    /// still open after that yields what had arrived by then.
    fn take(self, for_: Duration) -> String {
        let _ = self.done.recv_timeout(for_);
        self.so_far.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

/// Run a harness's status command with a timeout; `None` when it could
/// not be run at all (not installed, hung).
///
/// Both pipes are drained by threads of their own while the wait goes on:
/// a program that fills one of them while nothing reads it blocks on the
/// write, and the wait below would then run to its timeout on a command
/// that had nothing left to say.
fn run(program: &str, args: &[&str], timeout: Duration) -> Option<Status> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // A harness started from inside a Claude Code session refuses to nest;
    // the status command does not need the marker.
    cmd.env_remove("CLAUDECODE");
    let mut child = cmd.spawn().ok()?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                // Nothing is waited for here: a grandchild that inherited
                // the pipes holds them open after the child is killed, and
                // waiting for the readers would hang the probe for as
                // long as it lives. The threads end when the pipes close.
                return None;
            }
        }
    };
    // The pipes normally close with the process that exited, so the
    // readers end at once -- but a status command that forks a daemon of
    // its own leaves that daemon holding them, and then the readers never
    // end at all. What is left of the timeout is all they get (a floor,
    // for a child that exited on the deadline itself: reading an exited
    // child's pipe is instant); a reader still holding on is left to end
    // whenever its pipe does.
    let left = || {
        deadline
            .saturating_duration_since(std::time::Instant::now())
            .max(Duration::from_millis(200))
    };
    let out = out.take(left());
    let err = err.take(left());
    Some(Status {
        ok: status.success(),
        out,
        err,
    })
}

const STATUS_TIMEOUT: Duration = Duration::from_secs(15);

/// Claude Code: `claude auth status --json` prints `{"loggedIn": bool}`
/// (exit 1 when signed out).
fn probe_claude() -> Probe {
    let detail = "claude auth status".to_string();
    match run("claude", &["auth", "status", "--json"], STATUS_TIMEOUT) {
        Some(Status { out, .. }) => match serde_json::from_str::<serde_json::Value>(&out) {
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

/// What `codex login status` said. It prints `Logged in using ...` on
/// stdout and exits 0 when it is signed in, and `Not logged in` on
/// **stderr** with exit 1 when it is not, so both streams are read and
/// the "not" is looked for first: `not logged in` contains `logged in`.
fn codex_state(ok: bool, text: &str) -> LoginState {
    let lower = text.to_lowercase();
    if lower.contains("not logged in") {
        LoginState::SignedOut
    } else if ok || lower.contains("logged in") {
        LoginState::SignedIn
    } else {
        LoginState::Unknown
    }
}

/// The last line with anything on it, for the `detail` a person reads.
fn last_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("no output")
}

/// Codex: `codex login status` prints `Logged in using ...` (exit 0) or
/// `Not logged in` on stderr (exit 1).
fn probe_codex() -> Probe {
    codex_probe(run("codex", &["login", "status"], STATUS_TIMEOUT))
}

/// What `codex login status` said, made a probe: both streams are joined
/// before they are read, since the answer can be on either.
fn codex_probe(status: Option<Status>) -> Probe {
    let detail = "codex login status".to_string();
    match status {
        Some(Status { ok, out, err }) => {
            let text = format!("{}\n{}", out.trim(), err.trim());
            let text = text.trim();
            Probe {
                state: codex_state(ok, text),
                detail: format!("{detail}: {}", last_line(text)),
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
        "gemini" => "gemini (pick the Google account option)".to_string(),
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

    /// Codex says it is signed out on stderr and exits 1, so a probe
    /// that reads stdout alone can never refuse a signed-out Codex.
    #[test]
    fn codex_status_is_read_from_both_streams() {
        assert_eq!(codex_state(false, "Not logged in"), LoginState::SignedOut);
        // As it comes out of `run`, with an empty stdout ahead of it.
        assert_eq!(codex_state(false, "\nNot logged in"), LoginState::SignedOut);
        assert_eq!(
            codex_state(true, "Logged in using ChatGPT"),
            LoginState::SignedIn
        );
        // The order matters: "not logged in" contains "logged in".
        assert_eq!(
            codex_state(true, "Not logged in with any account"),
            LoginState::SignedOut
        );
        assert_eq!(codex_state(true, ""), LoginState::SignedIn);
        assert_eq!(codex_state(false, "something else"), LoginState::Unknown);
        assert_eq!(last_line("out\n\nNot logged in\n"), "Not logged in");
        assert_eq!(last_line("  \n"), "no output");
        // And the streams reach `codex_state` joined: the whole answer
        // arrives on stderr with a failing exit code.
        let out = codex_probe(Some(Status {
            ok: false,
            out: String::new(),
            err: "Not logged in".into(),
        }));
        assert_eq!(out.state, LoginState::SignedOut);
        assert!(out.detail.ends_with("Not logged in"), "{}", out.detail);
        let in_ = codex_probe(Some(Status {
            ok: true,
            out: "Logged in using ChatGPT".into(),
            err: String::new(),
        }));
        assert_eq!(in_.state, LoginState::SignedIn);
        assert_eq!(codex_probe(None).state, LoginState::Unknown);
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
