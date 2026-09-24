//! Scratch sessions' terminals (#491): each runs in a detached tmux session
//! of its own (see [`session_name`]), rather than in a herdr pane. An
//! item's session stays in herdr.
//!
//! tmux knows nothing about agents, so there is no agent state here: a
//! scratch session is live exactly while its tmux session exists (its
//! harness is the session's only command, so the session ends with it).
//! Deliveries are pasted (`load-buffer` + `paste-buffer`, then Enter), and
//! the web dashboard attaches to the session through a PTY (`api/term`).
//!
//! Commands never inherit `TMUX`: the daemon may itself run inside a tmux
//! pane, and every command has to reach the same (default, per-user)
//! server whoever runs it.

use anyhow::{Context, Result, bail};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

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

/// A key as `tmux send-keys` names it, from the names the pane mirror sends
/// (herdr's: `enter`, `esc`, `ctrl+c`, `shift+tab`, `f5`). `None` for a
/// name tmux has no key for; a single character is typed literally by the
/// caller, not here.
pub fn key_name(key: &str) -> Option<String> {
    let lower = key.to_ascii_lowercase();
    let named = match lower.as_str() {
        "enter" | "return" => "Enter",
        "esc" | "escape" => "Escape",
        "tab" => "Tab",
        "shift+tab" | "backtab" => "BTab",
        "backspace" | "bspace" => "BSpace",
        "space" => "Space",
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "home" => "Home",
        "end" => "End",
        "pageup" | "pgup" => "PPage",
        "pagedown" | "pgdn" => "NPage",
        "delete" | "del" => "DC",
        "insert" | "ins" => "IC",
        _ => {
            if let Some(n) = lower.strip_prefix('f')
                && let Ok(n) = n.parse::<u8>()
                && (1..=12).contains(&n)
            {
                return Some(format!("F{n}"));
            }
            for (prefix, tmux) in [("ctrl+", "C-"), ("alt+", "M-"), ("meta+", "M-")] {
                if let Some(rest) = lower.strip_prefix(prefix) {
                    let inner = if rest.chars().count() == 1 {
                        rest.to_string()
                    } else {
                        key_name(rest)?
                    };
                    return Some(format!("{tmux}{inner}"));
                }
            }
            return None;
        }
    };
    Some(named.to_string())
}

/// The target of a session, matched exactly (a bare name is a prefix match).
fn session_target(name: &str) -> String {
    format!("={name}")
}

/// The arguments of [`Tmux::attach_command`]: one tmux command sequence, so
/// a session that is gone fails the first and nothing is attached.
pub fn attach_args(name: &str) -> Vec<String> {
    let pane = pane_target(name);
    let session = session_target(name);
    [
        "set-option",
        "-t",
        &pane,
        "detach-on-destroy",
        "on",
        ";",
        "attach-session",
        "-t",
        &session,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// The active pane of a session, matched exactly.
fn pane_target(name: &str) -> String {
    format!("={name}:")
}

/// The arguments that start `command` in a new detached session `name`,
/// in `cwd`, with `env` set in it. The window starts at a size a mirror
/// can read; `window-size latest` (set once it exists) then follows
/// whichever client attached last.
pub fn new_session_args(
    name: &str,
    cwd: &str,
    command: &str,
    env: &[(String, String)],
) -> Vec<String> {
    let mut args: Vec<String> = [
        "new-session",
        "-d",
        "-s",
        name,
        "-c",
        cwd,
        "-x",
        "160",
        "-y",
        "48",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    for (key, value) in env {
        args.push("-e".into());
        args.push(format!("{key}={value}"));
    }
    args.push(command.to_string());
    args
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
            .await
            .context("spawning tmux (is tmux installed?)")?;
        Ok(status.success())
    }

    /// Whether a tmux server is running on this socket.
    async fn server_running(&self) -> bool {
        self.run(&["list-sessions"], None).await.is_ok()
    }

    /// Start `command` in a new detached session `name` in `cwd`, with the
    /// daemon's `PATH`, and let its window follow the latest client's size.
    ///
    /// Under systemd, a server started here would be in the service's
    /// cgroup and killed with it on every restart of the daemon, taking
    /// every scratch session with it; when no server is running, it is
    /// therefore started in a scope of its own (`systemd-run --user
    /// --scope`) where systemd-run is there and works, and plainly
    /// otherwise.
    pub async fn new_session(&self, name: &str, cwd: &str, command: &str) -> Result<()> {
        #[cfg(test)]
        if let Some(stub) = &self.stub {
            return stub.with(|s| {
                if let Some(why) = s.start_error.take() {
                    bail!("{why}");
                }
                s.live.insert(name.to_string());
                s.log.push(format!("new:{name}"));
                s.launches.push(command.to_string());
                Ok(())
            });
        }
        let env: Vec<(String, String)> = ["PATH", "LANG"]
            .into_iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
            .collect();
        let args = new_session_args(name, cwd, command, &env);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut started = false;
        if !self.server_running().await && crate::platform::which("systemd-run").is_some() {
            let mut full: Vec<String> = ["--user", "--scope", "--quiet", "--collect", "--", "tmux"]
                .into_iter()
                .map(str::to_string)
                .collect();
            if let Some(socket) = &self.socket {
                full.extend(["-L".to_string(), socket.clone()]);
            }
            full.extend(args.iter().map(|a| a.to_string()));
            let mut scoped = tokio::process::Command::new("systemd-run");
            scoped
                .args(&full)
                .env_remove("TMUX")
                .env_remove("TMUX_PANE")
                .kill_on_drop(true);
            match scoped.stdin(std::process::Stdio::null()).output().await {
                Ok(out) if out.status.success() => started = true,
                Ok(out) => warn!(
                    session = name,
                    "starting tmux in a systemd scope failed ({}); starting it plainly",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Err(e) => debug!(
                    session = name,
                    "no systemd-run ({e}); starting tmux plainly"
                ),
            }
        }
        if !started {
            self.run(&args, None).await?;
        }
        // `set-option -t` takes a pane: a bare `=name` is no target at all.
        let target = pane_target(name);
        // A terminal attached to the session is detached when it ends, rather
        // than moved to another session (as a tmux.conf setting it off would).
        for (option, value) in [("window-size", "latest"), ("detach-on-destroy", "on")] {
            if let Err(e) = self
                .run(&["set-option", "-t", &target, option, value], None)
                .await
            {
                // Only a session whose command has already exited gets here.
                debug!(session = name, "setting {option}: {e:#}");
            }
        }
        info!(session = name, cwd, "started tmux session");
        Ok(())
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

    /// Type into the session's pane as a person would: `text` literally,
    /// then each named key.
    pub async fn type_input(&self, name: &str, text: Option<&str>, keys: &[String]) -> Result<()> {
        let target = pane_target(name);
        if let Some(text) = text.filter(|t| !t.is_empty()) {
            self.run(&["send-keys", "-t", &target, "-l", "--", text], None)
                .await?;
        }
        for key in keys {
            if key.chars().count() == 1 {
                self.run(&["send-keys", "-t", &target, "-l", "--", key], None)
                    .await?;
            } else {
                let Some(named) = key_name(key) else {
                    bail!("tmux has no key named {key}");
                };
                self.run(&["send-keys", "-t", &target, &named], None)
                    .await?;
            }
        }
        Ok(())
    }

    /// The session's visible screen with its colours, rows joined by
    /// `\r\n`; with `history`, that many rows above it too.
    pub async fn capture(&self, name: &str, history: Option<u32>) -> Result<String> {
        #[cfg(test)]
        if let Some(stub) = &self.stub {
            return Ok(stub.with(|s| s.screen.join("\r\n")));
        }
        let target = pane_target(name);
        let start = history.map(|h| format!("-{h}"));
        let mut args = vec!["capture-pane", "-p", "-e", "-t", &target];
        if let Some(start) = start.as_deref() {
            args.extend(["-S", start]);
        }
        let out = self.run(&args, None).await?;
        let out = out.strip_suffix('\n').unwrap_or(&out);
        Ok(out.split('\n').collect::<Vec<_>>().join("\r\n"))
    }

    /// Wait for a harness just started in session `name` to settle: its
    /// screen unchanged for a couple of reads, answering the folder-trust
    /// dialog a harness shows on a new worktree on the way. `false` when
    /// the session ended (a resume the harness could not do, say).
    pub async fn settle(&self, name: &str, harness: &str, timeout: Duration) -> Result<bool> {
        #[cfg(test)]
        if let Some(stub) = &self.stub {
            let _ = (harness, timeout);
            return Ok(stub.with(|s| {
                if s.resume_exits {
                    s.resume_exits = false;
                    s.live.remove(name);
                }
                s.live.contains(name)
            }));
        }
        let deadline = tokio::time::Instant::now() + timeout;
        let mut last = String::new();
        let mut steady = 0;
        let mut answered = 0;
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(1000)).await;
            if !self.has_session(name).await? {
                return Ok(false);
            }
            let screen = self.capture(name, None).await.unwrap_or_default();
            let plain = strip_ansi(&screen).replace("\r\n", "\n");
            if answered < 4
                && let Some(answer) = crate::driver::trust_dialog(&plain)
            {
                info!(
                    session = name,
                    "{harness}: accepting the folder trust dialog"
                );
                let target = pane_target(name);
                if answer == crate::driver::TrustAnswer::DownEnter {
                    self.run(&["send-keys", "-t", &target, "Down"], None)
                        .await?;
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                self.run(&["send-keys", "-t", &target, "Enter"], None)
                    .await?;
                answered += 1;
                steady = 0;
                continue;
            }
            if !plain.trim().is_empty() && plain == last {
                steady += 1;
                if steady >= 2 {
                    return Ok(true);
                }
            } else {
                steady = 0;
            }
            last = plain;
        }
        warn!(session = name, "{harness} did not settle in time; going on");
        self.has_session(name).await
    }

    /// `tmux attach-session` to exactly `name`, for a terminal (`ssf __pane
    /// attach`, behind the dashboard's `api/term`). `detach-on-destroy` is
    /// set first (for a session started before ssf set it itself), so when
    /// the session ends the terminal ends with it instead of being moved to
    /// another session on the server.
    pub fn attach_command(&self, name: &str) -> std::process::Command {
        let mut command = std::process::Command::new("tmux");
        if let Some(socket) = &self.socket {
            command.args(["-L", socket]);
        }
        command
            .args(attach_args(name))
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        command
    }
}

/// `text` without its ANSI escape sequences (CSI and OSC), for matching
/// what a screen says.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
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
    /// `new:<name>`, `paste:<name>:<first line>`, `kill:<name>`.
    pub log: Vec<String>,
    /// Every command a session was started with.
    pub launches: Vec<String>,
    /// Every text pasted, whole.
    pub prompts: Vec<String>,
    pub screen: Vec<String>,
    /// The next settle finds the session gone (a resume that exited).
    pub resume_exits: bool,
    pub start_error: Option<String>,
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

    #[test]
    fn new_sessions_are_detached_in_the_worktree_with_the_environment() {
        let args = new_session_args(
            "ssf-o_r-k3f9",
            "/w/scratch-k3f9",
            "ssf launch --session 'o/r~k3f9' -- claude",
            &[("PATH".into(), "/usr/bin".into())],
        );
        assert_eq!(
            args,
            [
                "new-session",
                "-d",
                "-s",
                "ssf-o_r-k3f9",
                "-c",
                "/w/scratch-k3f9",
                "-x",
                "160",
                "-y",
                "48",
                "-e",
                "PATH=/usr/bin",
                "ssf launch --session 'o/r~k3f9' -- claude",
            ]
        );
    }

    #[test]
    fn attaching_names_the_session_exactly_and_detaches_when_it_ends() {
        assert_eq!(
            attach_args("ssf-o_sr_tab12"),
            [
                "set-option",
                "-t",
                "=ssf-o_sr_tab12:",
                "detach-on-destroy",
                "on",
                ";",
                "attach-session",
                "-t",
                "=ssf-o_sr_tab12",
            ]
        );
    }

    #[test]
    fn keys_map_to_tmux_names() {
        for (ours, theirs) in [
            ("enter", "Enter"),
            ("esc", "Escape"),
            ("ctrl+c", "C-c"),
            ("alt+b", "M-b"),
            ("shift+tab", "BTab"),
            ("backspace", "BSpace"),
            ("f5", "F5"),
            ("ctrl+up", "C-Up"),
            ("pageup", "PPage"),
        ] {
            assert_eq!(key_name(ours).as_deref(), Some(theirs), "{ours}");
        }
        assert_eq!(key_name("f13"), None);
        assert_eq!(key_name("nonsense"), None);
    }

    #[test]
    fn ansi_is_stripped_for_matching() {
        assert_eq!(
            strip_ansi("\x1b[1mTrust\x1b[0m this \x1b]0;title\x07folder"),
            "Trust this folder"
        );
    }

    /// Ctrl+C in a scratch terminal ends the harness and so its session: the
    /// attached terminal must end too, not move to another ssf session on
    /// the same server (#491). Real tmux on a socket of the test's own,
    /// attached in a PTY by util-linux `script`; skipped without either.
    #[tokio::test]
    async fn an_attached_terminal_ends_with_its_session() {
        let have = |cmd: &str, arg: &str| {
            std::process::Command::new(cmd)
                .arg(arg)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if !cfg!(target_os = "linux") || !have("tmux", "-V") || !have("script", "--version") {
            eprintln!("tmux or util-linux script is not installed; skipping");
            return;
        }
        let socket = format!("ssf-test-attach-{}", std::process::id());
        let tmux = Tmux::with_socket(&socket);
        let dir = std::env::temp_dir().to_string_lossy().into_owned();
        let name = session_name("o/r", "a1");
        // A second session whose name the first is a prefix of.
        let other = format!("{name}x");
        tmux.new_session(&other, &dir, "cat").await.unwrap();
        // As a person's tmux.conf may set it, which is how #491 met it.
        tmux.run(&["set-option", "-g", "detach-on-destroy", "off"], None)
            .await
            .unwrap();
        tmux.new_session(&name, &dir, "cat").await.unwrap();
        let attach = tmux.attach_command(&name);
        let line = std::iter::once(attach.get_program())
            .chain(attach.get_args())
            .map(|a| format!("'{}'", a.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ");
        let mut client = tokio::process::Command::new("script")
            .args(["-qec", &line, "/dev/null"])
            .env_remove("TMUX")
            .env("TERM", "xterm-256color")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        // Held open: `wait` would close it, and `script` ends at its EOF.
        let _keys = client.stdin.take();
        // Wait for the client to attach, then end its session.
        let mut attached = false;
        for _ in 0..50 {
            let clients = tmux
                .run(&["list-clients", "-F", "#{session_name}"], None)
                .await
                .unwrap_or_default();
            if clients.lines().any(|l| l == name) {
                attached = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(attached, "the terminal never attached to {name}");
        // Settled in, as a person's terminal is by the time they type.
        tokio::time::sleep(Duration::from_millis(500)).await;
        tmux.kill_session(&name).await.unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(5), client.wait()).await;
        let clients = tmux
            .run(&["list-clients", "-F", "#{session_name}"], None)
            .await
            .unwrap_or_default();
        let _ = std::process::Command::new("tmux")
            .args(["-L", &socket, "kill-server"])
            .output();
        assert!(
            ended.is_ok(),
            "the terminal outlived its session: {clients:?}"
        );
        assert!(!clients.lines().any(|l| l == other), "moved to {other}");
    }

    /// Against a real tmux on a socket of the test's own, never the
    /// person's server; skipped where tmux is not installed.
    #[tokio::test]
    async fn a_real_session_starts_takes_a_paste_and_is_killed() {
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
        let dir = std::env::temp_dir();
        assert!(!tmux.has_session(&name).await.unwrap());
        // `cat` echoes what it is given, so the paste shows on the screen.
        tmux.new_session(&name, &dir.to_string_lossy(), "cat")
            .await
            .unwrap();
        assert!(tmux.has_session(&name).await.unwrap());
        // An attached terminal leaves with the session, never for another.
        let option = tmux
            .run(
                &[
                    "show-options",
                    "-v",
                    "-t",
                    &pane_target(&name),
                    "detach-on-destroy",
                ],
                None,
            )
            .await
            .unwrap();
        assert_eq!(option.trim(), "on");
        let size = tmux
            .run(
                &[
                    "show-options",
                    "-wv",
                    "-t",
                    &pane_target(&name),
                    "window-size",
                ],
                None,
            )
            .await
            .unwrap();
        assert_eq!(size.trim(), "latest");
        tmux.paste(&name, "hello from ssf").await.unwrap();
        tmux.type_input(&name, Some("typed"), &["enter".into()])
            .await
            .unwrap();
        let mut seen = String::new();
        for _ in 0..20 {
            seen = tmux.capture(&name, Some(100)).await.unwrap();
            if seen.contains("hello from ssf") && seen.contains("typed") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(seen.contains("hello from ssf"), "{seen:?}");
        assert!(seen.contains("typed"), "{seen:?}");
        tmux.kill_session(&name).await.unwrap();
        assert!(!tmux.has_session(&name).await.unwrap());
        // A session already gone is fine to kill.
        tmux.kill_session(&name).await.unwrap();
        let _ = std::process::Command::new("tmux")
            .args(["-L", &socket, "kill-server"])
            .output();
    }
}
