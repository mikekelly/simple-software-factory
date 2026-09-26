//! The web endpoint's terminal (#491): `GET /<capability>/api/term/<session>`
//! upgrades to a WebSocket attached to a scratch session's tmux session.
//!
//! The attach is `ssf __pane attach <session>` run through the same client
//! transport as the pane mirror (so a factory in a VM is attached to in the
//! guest, over `ssh -t`), inside a PTY this process opens. The protocol:
//!
//! - binary frames, both ways, are terminal bytes: what the PTY printed, and
//!   what the person typed;
//! - a text frame from the client is JSON control, of which there is one,
//!   `{"type":"resize","cols":N,"rows":N}`, which resizes the PTY (and so,
//!   with `window-size latest`, the tmux window);
//! - the socket closes when the attach ends (the session ended, or could not
//!   be attached to: what it said is the last output), or the client goes.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::AsyncWriteExt;
use tokio::io::unix::AsyncFd;
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{Message, handshake::derive_accept_key, protocol::Role};

/// Terminals open at once: each is an attach process and a PTY.
const MAX_TERMS: usize = 16;
static OPEN: AtomicUsize = AtomicUsize::new(0);

/// The size a terminal starts at, until the client says its own.
const START_COLS: u16 = 80;
const START_ROWS: u16 = 24;

/// A client control message: a resize to `cols` x `rows`, or nothing this
/// endpoint knows.
pub(crate) fn resize_of(text: &str) -> Option<(u16, u16)> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if value.get("type")?.as_str()? != "resize" {
        return None;
    }
    let dimension = |key: &str| {
        value
            .get(key)?
            .as_u64()
            .filter(|n| (1..=1000).contains(n))
            .map(|n| n as u16)
    };
    Some((dimension("cols")?, dimension("rows")?))
}

/// The `101` that accepts the upgrade whose key is `key`.
pub(crate) fn switching_protocols(key: &str) -> String {
    format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        derive_accept_key(key.as_bytes())
    )
}

/// Held while a terminal is open; counts it against [`MAX_TERMS`].
struct Slot;

impl Slot {
    fn take() -> Option<Self> {
        OPEN.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
            (n < MAX_TERMS).then_some(n + 1)
        })
        .ok()
        .map(|_| Slot)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        OPEN.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Answer the upgrade and bridge the socket and the attach until either
/// ends.
pub(crate) async fn serve(mut stream: TcpStream, session: &str, key: &str, client: &Path) {
    let Some(_slot) = Slot::take() else {
        let body = serde_json::json!({
            "error": format!("{MAX_TERMS} terminals are already open; close one and try again")
        })
        .to_string();
        let _ = stream
            .write_all(
                format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        return;
    };
    if stream
        .write_all(switching_protocols(key).as_bytes())
        .await
        .is_err()
    {
        return;
    }
    let mut ws = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
    tracing::debug!(session, "web API terminal opened");
    let bridged = if crate::origin::Origin::parse(session).is_some() {
        bridge_herdr(&mut ws, session, client).await
    } else {
        bridge(&mut ws, session, client).await
    };
    if let Err(error) = bridged {
        let _ = ws
            .send(Message::binary(
                format!("\r\nssf: {error:#}\r\n").into_bytes(),
            ))
            .await;
    }
    let _ = ws.close(None).await;
    tracing::debug!(session, "web API terminal closed");
}

/// A PTY: the master this process keeps, and the slave the attach gets.
fn open_pty(cols: u16, rows: u16) -> Result<(OwnedFd, OwnedFd)> {
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty writes two descriptors it opened into the out
    // parameters; a null name and termios are allowed.
    let done = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::addr_of_mut!(size),
        )
    };
    if done != 0 {
        return Err(std::io::Error::last_os_error()).context("opening a PTY");
    }
    // SAFETY: both descriptors were just opened for us and are owned here.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    // SAFETY: fcntl on a descriptor we own.
    unsafe {
        let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
        libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        // Neither end may leak into another child spawned meanwhile: a
        // stray copy of the slave would keep the master from ever seeing
        // the attach end. The child gets the slave through `Stdio` (dup2,
        // which clears the flag on its copy).
        libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(slave.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
    }
    Ok((master, slave))
}

fn resize(master: &OwnedFd, cols: u16, rows: u16) {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCSWINSZ on the PTY master we own; the kernel tells the
    // attach (the foreground group of the slave) with SIGWINCH.
    unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
}

async fn pty_read(master: &AsyncFd<OwnedFd>, buffer: &mut [u8]) -> std::io::Result<usize> {
    loop {
        let mut guard = master.readable().await?;
        match guard.try_io(|fd| {
            // SAFETY: a read into a buffer we own, of at most its length.
            let n = unsafe {
                libc::read(
                    fd.get_ref().as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if n < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(n as usize)
            }
        }) {
            Ok(result) => return result,
            Err(_would_block) => continue,
        }
    }
}

async fn pty_write(master: &AsyncFd<OwnedFd>, mut bytes: &[u8]) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let mut guard = master.writable().await?;
        match guard.try_io(|fd| {
            // SAFETY: a write from a buffer we own, of at most its length.
            let n = unsafe {
                libc::write(fd.get_ref().as_raw_fd(), bytes.as_ptr().cast(), bytes.len())
            };
            if n < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(n as usize)
            }
        }) {
            Ok(Ok(n)) => bytes = &bytes[n..],
            Ok(Err(error)) => return Err(error),
            Err(_would_block) => continue,
        }
    }
    Ok(())
}

async fn bridge(ws: &mut WebSocketStream<TcpStream>, session: &str, client: &Path) -> Result<()> {
    let (master, slave) = open_pty(START_COLS, START_ROWS)?;
    let mut command = crate::dashboard_transport::local_client_command(
        client,
        &["__pane", "attach", session],
        crate::server_catalog::service_local_context(),
        crate::server_catalog::selected_vm_context()?.as_ref(),
        crate::server_catalog::selected_target_identity()?.as_ref(),
    );
    command
        .env("TERM", "xterm-256color")
        .stdin(slave.try_clone()?)
        .stdout(slave.try_clone()?)
        .stderr(slave);
    // SAFETY: only async-signal-safe calls between fork and exec: a session
    // of its own, with the PTY as its controlling terminal, so the attach
    // gets SIGWINCH on a resize and SIGHUP when the PTY closes.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().context("starting the terminal attach")?;
    // The slave is the child's now; dropping `command` closes this
    // process's copies, so the master reads EOF (EIO) once the attach ends.
    drop(command);
    let master = AsyncFd::new(master).context("watching the PTY")?;
    let mut buffer = vec![0u8; 16 * 1024];
    let outcome: Result<()> = loop {
        tokio::select! {
            read = pty_read(&master, &mut buffer) => match read {
                // EIO is how Linux says the other side of the PTY closed.
                Ok(0) | Err(_) => break Ok(()),
                Ok(n) => {
                    if ws.send(Message::binary(buffer[..n].to_vec())).await.is_err() {
                        break Ok(());
                    }
                }
            },
            message = ws.next() => match message {
                Some(Ok(Message::Binary(bytes))) => {
                    if let Err(error) = pty_write(&master, &bytes).await {
                        break Err(error).context("typing into the terminal");
                    }
                }
                Some(Ok(Message::Text(text))) => {
                    if let Some((cols, rows)) = resize_of(text.as_str()) {
                        resize(master.get_ref(), cols, rows);
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break Ok(()),
                Some(Ok(_)) => {}
            },
        }
    };
    if let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
        // SAFETY: a hangup to the session the attach leads, as a closed
        // terminal would send; with a VM that is ssh and what it started.
        unsafe { libc::killpg(pid, libc::SIGHUP) };
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
    outcome
}

/// Spike (#561): what a client's WebSocket message asks of `herdr terminal
/// session control`, as its NDJSON command line. Binary frames are typed
/// bytes; text frames are `resize`, `scroll` or `release` control.
pub(crate) fn herdr_command_of(message: &Message) -> Option<String> {
    use base64::Engine;
    let command = match message {
        Message::Binary(bytes) => serde_json::json!({
            "type": "terminal.input",
            "bytes": base64::engine::general_purpose::STANDARD.encode(bytes),
        }),
        Message::Text(text) => {
            if let Some((cols, rows)) = resize_of(text.as_str()) {
                serde_json::json!({"type": "terminal.resize", "cols": cols, "rows": rows})
            } else {
                let value: serde_json::Value = serde_json::from_str(text.as_str()).ok()?;
                match value.get("type")?.as_str()? {
                    "scroll" => {
                        let lines = value.get("lines")?.as_u64()?.clamp(1, 1000);
                        let direction = match value.get("direction")?.as_str()? {
                            "up" => "up",
                            _ => "down",
                        };
                        serde_json::json!({"type": "terminal.scroll", "lines": lines, "direction": direction})
                    }
                    "release" => serde_json::json!({"type": "terminal.release"}),
                    _ => return None,
                }
            }
        }
        _ => return None,
    };
    Some(format!("{command}\n"))
}

/// Spike (#561): what one NDJSON line from herdr means for the socket.
#[derive(Debug, PartialEq)]
pub(crate) enum HerdrLine {
    /// Terminal bytes to draw.
    Frame(Vec<u8>),
    /// The stream ended, and why.
    Closed(String),
    /// Anything else.
    Other,
}

pub(crate) fn herdr_line(line: &str) -> HerdrLine {
    use base64::Engine;
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return HerdrLine::Other;
    };
    match value.get("type").and_then(|t| t.as_str()) {
        Some("terminal.frame") => value
            .get("bytes")
            .and_then(|b| b.as_str())
            .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
            .map_or(HerdrLine::Other, HerdrLine::Frame),
        Some("terminal.closed") => HerdrLine::Closed(
            value
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("closed")
                .to_string(),
        ),
        _ => HerdrLine::Other,
    }
}

/// Spike (#561): bridge the socket to `ssf __pane control <session>`, which
/// runs `herdr terminal session control` on the item's pane. NDJSON both
/// ways, over pipes: a PTY's line discipline would mangle it.
async fn bridge_herdr(
    ws: &mut WebSocketStream<TcpStream>,
    session: &str,
    client: &Path,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut command = crate::dashboard_transport::local_client_command(
        client,
        &["__pane", "control", session],
        crate::server_catalog::service_local_context(),
        crate::server_catalog::selected_vm_context()?.as_ref(),
        crate::server_catalog::selected_target_identity()?.as_ref(),
    );
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().context("starting the herdr bridge")?;
    let mut stdin = child.stdin.take().context("the bridge's stdin")?;
    let mut lines = BufReader::new(child.stdout.take().context("the bridge's stdout")?).lines();
    let mut stderr = child.stderr.take().context("the bridge's stderr")?;
    // Start at the browser's size once it says it; until then herdr keeps the
    // pane's own.
    let outcome: Result<()> = loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => match herdr_line(&line) {
                    HerdrLine::Frame(bytes) => {
                        if ws.send(Message::binary(bytes)).await.is_err() {
                            break Ok(());
                        }
                    }
                    HerdrLine::Closed(reason) => {
                        let _ = ws
                            .send(Message::binary(format!("\r\nssf: {reason}\r\n").into_bytes()))
                            .await;
                        break Ok(());
                    }
                    HerdrLine::Other => {}
                },
                Ok(None) | Err(_) => {
                    let mut said = String::new();
                    let _ = tokio::io::AsyncReadExt::read_to_string(&mut stderr, &mut said).await;
                    let said = said.trim();
                    break if said.is_empty() { Ok(()) } else { Err(anyhow::anyhow!("{said}")) };
                }
            },
            message = ws.next() => match message {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break Ok(()),
                Some(Ok(message)) => {
                    if let Some(command) = herdr_command_of(&message)
                        && let Err(error) = stdin.write_all(command.as_bytes()).await
                    {
                        break Err(error).context("typing into the terminal");
                    }
                }
            },
        }
    };
    // Closing stdin ends herdr's stream cleanly, and releases the pane.
    drop(stdin);
    if tokio::time::timeout(std::time::Duration::from_secs(2), child.wait())
        .await
        .is_err()
    {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_messages_are_read_and_bounded() {
        assert_eq!(
            resize_of(r#"{"type":"resize","cols":120,"rows":40}"#),
            Some((120, 40))
        );
        assert_eq!(resize_of(r#"{"type":"resize","cols":0,"rows":40}"#), None);
        assert_eq!(resize_of(r#"{"type":"resize","cols":120}"#), None);
        assert_eq!(resize_of(r#"{"type":"other"}"#), None);
        assert_eq!(resize_of("not json"), None);
    }

    #[test]
    fn herdr_lines_and_commands_translate() {
        assert_eq!(
            herdr_line(r#"{"type":"terminal.frame","seq":1,"bytes":"aGk="}"#),
            HerdrLine::Frame(b"hi".to_vec())
        );
        assert_eq!(
            herdr_line(r#"{"type":"terminal.closed","reason":"gone"}"#),
            HerdrLine::Closed("gone".into())
        );
        assert_eq!(
            herdr_command_of(&Message::binary(b"hi".to_vec())).unwrap(),
            "{\"bytes\":\"aGk=\",\"type\":\"terminal.input\"}\n"
        );
        assert!(
            herdr_command_of(&Message::text(r#"{"type":"resize","cols":80,"rows":24}"#))
                .unwrap()
                .contains("terminal.resize")
        );
    }

    /// RFC 6455's own example key and answer.
    #[test]
    fn the_upgrade_is_accepted_with_the_derived_key() {
        let answer = switching_protocols("dGhlIHNhbXBsZSBub25jZQ==");
        assert!(answer.starts_with("HTTP/1.1 101 "), "{answer}");
        assert!(
            answer.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"),
            "{answer}"
        );
        assert!(answer.ends_with("\r\n\r\n"));
    }

    #[tokio::test]
    async fn a_pty_carries_bytes_to_its_reader() {
        let (master, slave) = open_pty(80, 24).unwrap();
        let master = AsyncFd::new(master).unwrap();
        // Write on the slave side as a program would; the master reads it.
        let mut writer = std::fs::File::from(slave);
        std::io::Write::write_all(&mut writer, b"hello").unwrap();
        let mut buffer = [0u8; 64];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            pty_read(&master, &mut buffer),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(String::from_utf8_lossy(&buffer[..n]).contains("hello"));
        resize(master.get_ref(), 100, 30);
    }
}
