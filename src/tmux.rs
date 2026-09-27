//! Scratch sessions left in tmux: from #491 to #565 a scratch session ran
//! in a detached tmux session of its own (see [`session_name`]). They run
//! in herdr panes now, as item sessions do; one found still running in tmux
//! is left there and told there (pasted: `load-buffer` + `paste-buffer`,
//! then Enter) until it stops, and ended there when it is released. It
//! starts in a herdr pane the next time it starts. Nothing here starts a
//! tmux session, and tmux need not be installed: without it no session is
//! running in it.
//!
//! Commands never inherit `TMUX`: the daemon may itself run inside a tmux
//! pane, and every command has to reach the same (default, per-user)
//! server whoever runs it.

use anyhow::{Context, Result, bail};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tracing::debug;

/// What a scratch session's terminal handle starts with when it is a tmux
/// session (`tmux:<name>`); a handle without it is a legacy herdr pane.
pub const HANDLE_PREFIX: &str = "tmux:";

/// The tmux session a scratch session (`repo` `owner/name`, `id`) runs in.
/// tmux takes neither `.` nor `:` in a name, so `owner/repo~id` is escaped:
/// `_` is `__`, `.` `_d`, `/` `_s`, `~` `_t`, anything else outside
/// letters, digits and `-` `_x<hex>_`. No two sessions share a name.
pub fn session_name(repo: &str, id: &str) -> String {
    // A prefix-free escape, so distinct sessions never share a name.
    let mut out = String::from("ssf-");
    for c in format!("{repo}~{id}").chars() {
        match c {
            c if c.is_ascii_alphanumeric() || c == '-' => out.push(c),
            '_' => out.push_str("__"),
            '.' => out.push_str("_d"),
            '/' => out.push_str("_s"),
            '~' => out.push_str("_t"),
            c => out.push_str(&format!("_x{:x}_", c as u32)),
        }
    }
    out
}

/// `tmux:<name>`: the terminal handle recorded for a scratch session.
pub fn handle(name: &str) -> String {
    format!("{HANDLE_PREFIX}{name}")
}

/// The tmux session a recorded handle names; `None` for a herdr pane.
pub fn name_of(handle: &str) -> Option<&str> {
    handle.strip_prefix(HANDLE_PREFIX)
}

/// The target of a session, matched exactly (a bare name is a prefix match).
fn session_target(name: &str) -> String {
    format!("={name}")
}

/// The active pane of a session, matched exactly.
fn pane_target(name: &str) -> String {
    format!("={name}:")
}

/// A way to reach tmux: the real server (the default one, or `-L socket`
/// in tests), or a stub for the engine's tests.
#[derive(Clone, Default)]
pub struct Tmux {
    /// `-L` socket name; the default server when `None`.
    socket: Option<String>,
    #[cfg(test)]
    stub: Option<StubTmux>,
}

impl Tmux {
    /// The default server, which a person reaches with a plain `tmux`.
    pub fn new() -> Self {
        Self::default()
    }

    /// A server of its own (`tmux -L socket`), for tests.
    #[cfg(test)]
    pub fn with_socket(socket: &str) -> Self {
        Self {
            socket: Some(socket.to_string()),
            stub: None,
        }
    }

    #[cfg(test)]
    pub fn stub(stub: StubTmux) -> Self {
        Self {
            socket: None,
            stub: Some(stub),
        }
    }

    /// `tmux` with this server's socket and none of the caller's tmux
    /// environment.
    fn base(&self, program: &str) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(program);
        if let Some(socket) = &self.socket {
            command.args(["-L", socket]);
        }
        command
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .kill_on_drop(true);
        command
    }

    /// Run tmux, returning its stdout.
    async fn run(&self, args: &[&str], input: Option<&[u8]>) -> Result<String> {
        debug!(args = ?crate::driver::redacted_args(args), "tmux");
        let mut command = self.base("tmux");
        command
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(if input.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            });
        let mut child = command
            .spawn()
            .context("spawning tmux (is tmux installed?)")?;
        if let Some(input) = input {
            let mut stdin = child.stdin.take().context("tmux has no stdin")?;
            stdin.write_all(input).await.context("writing to tmux")?;
            drop(stdin);
        }
        let out = child.wait_with_output().await.context("waiting for tmux")?;
        if !out.status.success() {
            bail!(
                "tmux {} failed: {}",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Whether session `name` exists. No server at all is "no".
    pub async fn has_session(&self, name: &str) -> Result<bool> {
        #[cfg(test)]
        if let Some(stub) = &self.stub {
            return Ok(stub.with(|s| s.live.contains(name)));
        }
        let status = self
            .base("tmux")
            .args(["has-session", "-t", &session_target(name)])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
        match status {
            Ok(status) => Ok(status.success()),
            // No tmux installed: nothing is running in it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(anyhow::Error::new(e).context("spawning tmux")),
        }
    }

    /// End session `name`, and its harness with it. One already gone is
    /// fine.
    pub async fn kill_session(&self, name: &str) -> Result<()> {
        #[cfg(test)]
        if let Some(stub) = &self.stub {
            return stub.with(|s| {
                if let Some(why) = s.kill_error.take() {
                    bail!("{why}");
                }
                s.live.remove(name);
                s.log.push(format!("kill:{name}"));
                Ok(())
            });
        }
        if !self.has_session(name).await? {
            return Ok(());
        }
        self.run(&["kill-session", "-t", &session_target(name)], None)
            .await
            .map(|_| ())
    }

    /// Paste `text` into the session's pane as one bracketed paste (when
    /// the program there asked for those) and submit it with Enter.
    pub async fn paste(&self, name: &str, text: &str) -> Result<()> {
        let text = text.trim_end();
        #[cfg(test)]
        if let Some(stub) = &self.stub {
            return stub.with(|s| {
                if !s.live.contains(name) {
                    bail!("no tmux session {name}");
                }
                s.prompts.push(text.to_string());
                s.log.push(format!(
                    "paste:{name}:{}",
                    text.lines().next().unwrap_or("")
                ));
                Ok(())
            });
        }
        // A buffer of the session's own, deleted by the paste, so two
        // deliveries at once cannot paste each other's text.
        let buffer = format!("{name}-delivery");
        self.run(&["load-buffer", "-b", &buffer, "-"], Some(text.as_bytes()))
            .await?;
        self.run(
            &[
                "paste-buffer",
                "-d",
                "-p",
                "-b",
                &buffer,
                "-t",
                &pane_target(name),
            ],
            None,
        )
        .await?;
        // A harness that gets Enter in the same read as the paste may take
        // it as a newline in the paste.
        tokio::time::sleep(Duration::from_millis(400)).await;
        self.run(&["send-keys", "-t", &pane_target(name), "Enter"], None)
            .await?;
        Ok(())
    }
}

/// A tmux for the engine's tests: sessions are names in a set, and every
/// operation is logged.
#[cfg(test)]
#[derive(Clone, Default)]
pub struct StubTmux {
    inner: std::sync::Arc<std::sync::Mutex<StubTmuxState>>,
}

#[cfg(test)]
#[derive(Default)]
pub struct StubTmuxState {
    pub live: std::collections::BTreeSet<String>,
    /// `paste:<name>:<first line>`, `kill:<name>`.
    pub log: Vec<String>,
    /// Every text pasted, whole.
    pub prompts: Vec<String>,
    /// When set, the next kill fails with this message.
    pub kill_error: Option<String>,
}

#[cfg(test)]
impl StubTmux {
    pub fn with<T>(&self, f: impl FnOnce(&mut StubTmuxState) -> T) -> T {
        f(&mut self.inner.lock().unwrap())
    }

    pub fn log(&self) -> Vec<String> {
        self.with(|s| std::mem::take(&mut s.log))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_names_are_safe_for_tmux_and_keep_repositories_apart() {
        assert_eq!(session_name("o/r", "k3f9"), "ssf-o_sr_tk3f9");
        let name = session_name("my-org/site.io", "ab12");
        assert_eq!(name, "ssf-my-org_ssite_dio_tab12");
        assert_ne!(session_name("o/a.b", "x"), session_name("o/a_b", "x"));
        assert!(!name.contains('.') && !name.contains(':'));
        assert_ne!(session_name("a/b", "x1"), session_name("a/c", "x1"));
        assert_eq!(name_of(&handle(&name)), Some(name.as_str()));
        assert_eq!(name_of("w7:p1"), None);
    }

    /// A session left from before #565 is pasted to and killed, exactly by
    /// name. Against a real tmux on a socket of the test's own, never the
    /// person's server; skipped where tmux is not installed.
    #[tokio::test]
    async fn a_real_session_takes_a_paste_and_is_killed() {
        if std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .is_err()
        {
            eprintln!("tmux is not installed; skipping");
            return;
        }
        let socket = format!("ssf-test-{}", std::process::id());
        let tmux = Tmux::with_socket(&socket);
        let name = session_name("o/r", "t3st");
        let dir = std::env::temp_dir().to_string_lossy().into_owned();
        assert!(!tmux.has_session(&name).await.unwrap());
        // `cat` echoes what it is given, so the paste shows on the screen.
        // A second session whose name the first is a prefix of is left be.
        let other = format!("{name}x");
        for session in [&name, &other] {
            tmux.run(
                &["new-session", "-d", "-s", session, "-c", &dir, "cat"],
                None,
            )
            .await
            .unwrap();
        }
        assert!(tmux.has_session(&name).await.unwrap());
        tmux.paste(&name, "hello from ssf").await.unwrap();
        let mut seen = String::new();
        for _ in 0..20 {
            seen = tmux
                .run(&["capture-pane", "-p", "-t", &pane_target(&name)], None)
                .await
                .unwrap();
            if seen.contains("hello from ssf") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(seen.contains("hello from ssf"), "{seen:?}");
        tmux.kill_session(&name).await.unwrap();
        assert!(!tmux.has_session(&name).await.unwrap());
        assert!(tmux.has_session(&other).await.unwrap());
        // A session already gone is fine to kill.
        tmux.kill_session(&name).await.unwrap();
        let _ = std::process::Command::new("tmux")
            .args(["-L", &socket, "kill-server"])
            .output();
    }
}
