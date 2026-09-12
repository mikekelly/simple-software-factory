//! Installed-client PTY coverage; all servers and Herdr commands are isolated fakes.
#![cfg(unix)]
use std::io::{Read, Write};
use std::os::{
    fd::{AsRawFd, FromRawFd},
    unix::fs::PermissionsExt,
};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
struct Temp(std::path::PathBuf);
impl Temp {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("ssf-dashboard-{name}-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_ssf"), path.join("ssf")).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Pty {
    master: std::fs::File,
    slave: std::fs::File,
    child: std::process::Child,
    output: String,
    original_flags: libc::tcflag_t,
}
impl Pty {
    fn spawn(mut command: Command) -> Self {
        let mut master = -1;
        let mut slave = -1;
        let size = libc::winsize {
            ws_row: 30,
            ws_col: 140,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &size,
                )
            },
            0
        );
        let master = unsafe { std::fs::File::from_raw_fd(master) };
        let slave = unsafe { std::fs::File::from_raw_fd(slave) };
        let mut attributes = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut attributes) },
            0
        );
        unsafe {
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
        }
        let child = command
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .spawn()
            .unwrap();
        Self {
            master,
            slave,
            child,
            output: String::new(),
            original_flags: attributes.c_lflag,
        }
    }
    fn wait_for(&mut self, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let mut buffer = [0; 16384];
            if let Ok(n) = self.master.read(&mut buffer) {
                self.output.push_str(&String::from_utf8_lossy(&buffer[..n]));
            }
            if self.output.contains(text) {
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "client exited: {}",
                self.output
            );
            assert!(
                Instant::now() < deadline,
                "missing {text:?}: {}",
                self.output
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn key(&mut self, key: &[u8]) {
        self.master.write_all(key).unwrap();
    }
    fn wait_for_file(&mut self, path: &Path, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            if std::fs::read_to_string(path).is_ok_and(|content| content.contains(text)) {
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "client exited: {}",
                self.output
            );
            assert!(Instant::now() < deadline, "missing {text:?} in {path:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn quit(&mut self) {
        self.key(b"q");
        self.finished();
    }
    fn finished(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "client did not exit promptly");
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut attributes = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(self.slave.as_raw_fd(), &mut attributes) },
            0
        );
        assert_eq!(
            attributes.c_lflag, self.original_flags,
            "raw mode was not restored"
        );
        let mut buffer = [0; 16384];
        while let Ok(n) = self.master.read(&mut buffer) {
            if n == 0 {
                break;
            }
            self.output.push_str(&String::from_utf8_lossy(&buffer[..n]));
        }
        assert!(
            self.output.contains("\u{1b}[?1049l"),
            "alternate screen was not restored"
        );
    }
}
impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn client(root: &Path) -> Command {
    let mut command = Command::new(root.join("ssf"));
    command
        .env("PATH", root)
        .env("TEST_ROOT", root)
        .env("TERM", "xterm-256color")
        .env_remove("SSF_SERVER")
        .env_remove("HERDR_ENV")
        .env("SSF_CONFIG_DIR", root.join("config"))
        .env("SSF_STATE_DIR", root.join("state"));
    command
}
const SNAPSHOT: &str = r#"{"dashboard":{"cards":[{"owner":"r#1","origin":{"id":"r#1","title":"Origin issue"},"additional":[{"id":"r#2","title":"Another issue"}],"agent_state":"working","last_activity_at":"2026-09-12","last_assistant_message":"Latest summary","agent_session_id":"session-1","harness":"codex"}],"monitored_items":[],"warning":null}}"#;

#[test]
fn terminal_refreshes_for_local_remote_and_environment_routes_without_browser() {
    let root = Temp::new("routing");
    script(
        &root.0.join("ssf-server"),
        &format!(
            r#"printf '%s\n' "$@" >> "$TEST_ROOT/local-args"
while :; do printf '%s\n' '{SNAPSHOT}'; /usr/bin/sleep 1; done"#
        ),
    );
    script(
        &root.0.join("ssh"),
        &format!(
            r#"printf '%s\n' "$@" >> "$TEST_ROOT/ssh-args"
while :; do printf '%s\n' '{SNAPSHOT}'; /usr/bin/sleep 1; done"#
        ),
    );
    for route in ["local", "remote", "environment"] {
        let mut command = client(&root.0);
        if route == "remote" {
            command.args(["--server", "customer@cloud.example"]);
        }
        if route == "environment" {
            command.env("SSF_SERVER", "environment-host");
        }
        command.arg("dashboard");
        let mut terminal = Pty::spawn(command);
        terminal.wait_for("Origin issue");
        terminal.wait_for("SSF FACTORY");
        assert!(terminal.child.try_wait().unwrap().is_none());
        terminal.quit();
    }
    assert_eq!(
        std::fs::read_to_string(root.0.join("local-args")).unwrap(),
        "__client\nstatus\n--json\n--watch\n"
    );
    let ssh = std::fs::read_to_string(root.0.join("ssh-args")).unwrap();
    assert!(
        ssh.contains("customer@cloud.example\nssf-server __client 'status' '--json' '--watch'")
    );
    assert!(ssh.contains("environment-host\nssf-server __client 'status' '--json' '--watch'"));
    let paths: Vec<_> = ssh
        .lines()
        .filter(|line| line.starts_with("ControlPath="))
        .collect();
    assert_eq!(paths.len(), 2);
    assert_ne!(paths[0], paths[1]);
    assert_eq!(ssh.matches("ControlMaster=auto").count(), 2);
}

#[test]
fn repeated_server_routes_share_one_dashboard_and_keep_streams_separate() {
    let root = Temp::new("multiple-servers");
    script(
        &root.0.join("ssh"),
        r#"while [ "$1" != "--" ]; do shift; done
shift
route="$1"
if [ "$route" = "factory-one" ]; then
 snapshot='{"server":{"hostname":"factory-one"},"dashboard":{"cards":[{"owner":"r#1","origin":{"id":"r#1","title":"First factory agent"},"additional":[],"agent_state":"working","last_activity_at":"now","last_assistant_message":"one","agent_session_id":"one","harness":"codex"}],"monitored_items":[],"warning":null}}'
else
 snapshot='{"server":{"hostname":"factory-two"},"dashboard":{"cards":[{"owner":"r#2","origin":{"id":"r#2","title":"Second factory agent"},"additional":[],"agent_state":"blocked","last_activity_at":"now","last_assistant_message":"two","agent_session_id":"two","harness":"codex"}],"monitored_items":[],"warning":null}}'
fi
while :; do printf '%s\n' "$snapshot"; /usr/bin/sleep 1; done"#,
    );
    let mut command = client(&root.0);
    command.args([
        "--server",
        "factory-one",
        "--server",
        "factory-two",
        "dashboard",
    ]);
    let mut terminal = Pty::spawn(command);
    terminal.wait_for("First factory agent");
    terminal.wait_for("Second factory agent");
    terminal.wait_for("2 servers");
    terminal.quit();
}

#[test]
fn herdr_selection_resolves_cross_workspace_pane_and_stale_panes_leave_dashboard_alive() {
    let root = Temp::new("focus");
    script(
        &root.0.join("ssf-server"),
        &format!("printf '%s\\n' '{SNAPSHOT}'"),
    );
    script(
        &root.0.join("herdr"),
        r#"printf '%s\n' "$@" >> "$TEST_ROOT/herdr-args"
if [ "$2" = list ]; then
 printf '%s\n' '{"result":{"agents":[{"agent":"codex","pane_id":"w9:p7","agent_session":{"kind":"id","value":"session-1"}}]}}'
elif [ -f "$TEST_ROOT/closed" ]; then
 echo 'pane no longer exists' >&2; exit 1
else
 printf '%s\n' '{"result":{}}'
fi"#,
    );
    let mut command = client(&root.0);
    command
        .env("HERDR_ENV", "1")
        .env("HERDR_PANE_ID", "w1:p1")
        .arg("dashboard");
    let mut terminal = Pty::spawn(command);
    terminal.wait_for("Origin issue");
    terminal.key(b"\r");
    terminal.wait_for_file(&root.0.join("herdr-args"), "focus\nw9:p7");
    assert!(terminal.child.try_wait().unwrap().is_none());
    std::fs::write(root.0.join("closed"), "").unwrap();
    // SGR left click on the first visible card.
    terminal.key(b"\x1b[<0;5;4M");
    terminal.wait_for_file(
        &root.0.join("herdr-args"),
        "focus\nw9:p7\nagent\nlist\nagent\nfocus\nw9:p7",
    );
    assert!(terminal.child.try_wait().unwrap().is_none());
    terminal.quit();
    assert_eq!(
        std::fs::read_to_string(root.0.join("herdr-args")).unwrap(),
        "agent\nlist\nagent\nfocus\nw9:p7\nagent\nlist\nagent\nfocus\nw9:p7\n"
    );
}

#[test]
fn errors_are_visible_and_quit_restores_terminal_while_request_is_pending() {
    let root = Temp::new("errors");
    script(
        &root.0.join("ssf-server"),
        "echo 'connection refused' >&2; exit 1",
    );
    let mut command = client(&root.0);
    command.arg("dashboard");
    let mut terminal = Pty::spawn(command);
    terminal.wait_for("TRANSPORT ERROR");
    terminal.wait_for("connection refused");
    terminal.quit();
    script(&root.0.join("ssf-server"), "while :; do :; done");
    let mut command = client(&root.0);
    command.arg("dashboard");
    let mut terminal = Pty::spawn(command);
    terminal.wait_for("Connecting to SSF");
    terminal.quit();
    let mut command = client(&root.0);
    command.arg("dashboard");
    let mut terminal = Pty::spawn(command);
    terminal.wait_for("Connecting to SSF");
    assert_eq!(
        unsafe { libc::kill(terminal.child.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    terminal.finished();
}

#[test]
fn non_terminal_is_rejected_with_a_scriptable_alternative() {
    let root = Temp::new("nonterminal");
    let output = client(&root.0).arg("dashboard").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("ssf status --json"));
}
