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
//!
//! An item session's pane (#563) is herdr's, and is bridged instead to `ssf
//! __pane control <session> [--observe]` over pipes: `herdr terminal session`
//! NDJSON both ways. It opens view only (`observe`), and takes control only
//! when the client asks (`{"type":"control"}`) and this server allows it
//! (`may_control`, decided from the request) and the factory does
//! (`item_pane_input`, decided where the config is); `{"type":"release"}`
//! gives it back. See [`bridge_item`] for the messages.

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
pub(crate) async fn serve(
    mut stream: TcpStream,
    session: &str,
    key: &str,
    client: &Path,
    may_control: bool,
) {
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
        bridge_item(&mut ws, session, client, may_control).await
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

/// Lines a wheel notch scrolls herdr's history by. One `terminal.scroll` per
/// notch: in a full-screen TUI with the mouse on, herdr passes each one to
/// the app as one wheel event.
const SCROLL_LINES: u64 = 3;

/// How long a pane that went away is looked for again, one wait after
/// another: a relaunch records its new pane in the state within seconds.
const RELOCATE_WAITS: [u64; 8] = [1, 2, 4, 8, 10, 10, 10, 15];

/// What a client's message asks of an item's terminal.
#[derive(Debug, PartialEq)]
pub(crate) enum Ask {
    /// A line for `herdr terminal session control`'s stdin: typed bytes, a
    /// resize or a scroll. Only ever written while in control.
    Herdr(String),
    /// The browser's size, which control starts at.
    Size(u16, u16),
    Control,
    Release,
    Nothing,
}

/// What a client's WebSocket message asks: binary frames are typed bytes;
/// text frames are JSON, `resize`, `scroll` (one wheel notch, `up` or
/// `down`), `control` or `release`.
pub(crate) fn ask_of(message: &Message) -> Ask {
    use base64::Engine;
    let line = |command: serde_json::Value| Ask::Herdr(format!("{command}\n"));
    let text = match message {
        Message::Binary(bytes) if !bytes.is_empty() => {
            return line(serde_json::json!({
                "type": "terminal.input",
                "bytes": base64::engine::general_purpose::STANDARD.encode(bytes),
            }));
        }
        Message::Text(text) => text.as_str(),
        _ => return Ask::Nothing,
    };
    if let Some((cols, rows)) = resize_of(text) {
        return Ask::Size(cols, rows);
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Ask::Nothing;
    };
    match value.get("type").and_then(|t| t.as_str()) {
        Some("control") => Ask::Control,
        Some("release") => Ask::Release,
        Some("scroll") => match value.get("direction").and_then(|d| d.as_str()) {
            Some(direction @ ("up" | "down")) => line(serde_json::json!({
                "type": "terminal.scroll",
                "lines": SCROLL_LINES,
                "direction": direction,
            })),
            _ => Ask::Nothing,
        },
        _ => Ask::Nothing,
    }
}

/// What one NDJSON line from herdr means for the socket.
#[derive(Debug, PartialEq)]
pub(crate) enum HerdrLine {
    /// Terminal bytes to draw, at the size herdr drew them.
    Frame {
        bytes: Vec<u8>,
        size: Option<(u64, u64)>,
    },
    /// The stream ended, and why.
    Closed(String),
    Other,
}

pub(crate) fn herdr_line(line: &str) -> HerdrLine {
    use base64::Engine;
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return HerdrLine::Other;
    };
    match value.get("type").and_then(|t| t.as_str()) {
        Some("terminal.frame") => {
            let Some(bytes) = value
                .get("bytes")
                .and_then(|b| b.as_str())
                .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
            else {
                return HerdrLine::Other;
            };
            let dimension = |key: &str| value.get(key).and_then(|n| n.as_u64());
            HerdrLine::Frame {
                bytes,
                size: dimension("width").zip(dimension("height")),
            }
        }
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

/// Whether a stream that ended for `reason` is worth starting again: the
/// harness exited, or the pane is gone, and a relaunch has a new one. A pane
/// taken over by someone else is not: this view does not take it back.
pub(crate) fn relocatable(reason: &str) -> bool {
    reason.contains("exited") || reason.contains("not found") || reason.contains("no agent")
}

/// Why this server gives a terminal no control.
const NO_CONTROL: &str = "this server takes typing into an item's pane only from the Chrome \
                          extension, or from its own page where dashboard.terminal_input is on";

/// How one run of `ssf __pane control` ended.
enum End {
    /// The client went.
    Gone,
    /// The client asked to control (`true`) or release (`false`).
    Switch(bool),
    /// herdr's `terminal.closed`, or what the command said when it stopped.
    Closed(String),
    /// Control was refused where the factory's config is.
    Refused(String),
}

async fn say(ws: &mut WebSocketStream<TcpStream>, message: serde_json::Value) -> bool {
    ws.send(Message::text(message.to_string())).await.is_ok()
}

/// Bridge the socket to an item's pane (#563), through `ssf __pane control`
/// run by the factory's client (so a factory in a VM streams from the
/// guest). Text frames to the client are JSON:
///
/// - `{"type":"mode","control":bool,"may_control":bool}` when the stream
///   (re)starts (control once herdr has drawn for it): whether it is control,
///   and whether this server would let it be;
/// - `{"type":"size","cols":N,"rows":N}` when herdr's frames change size;
/// - `{"type":"refused","reason":"…"}` for control that was not allowed;
/// - `{"type":"notice","text":"…"}` for anything else worth saying.
///
/// Typed bytes, resizes and scrolls are written only while in control, so a
/// view-only terminal is view only whatever its page sends.
async fn bridge_item(
    ws: &mut WebSocketStream<TcpStream>,
    session: &str,
    client: &Path,
    may_control: bool,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let mut control = false;
    let mut size: Option<(u16, u16)> = None;
    let mut waits = RELOCATE_WAITS.iter();
    loop {
        let mut args = vec!["__pane".to_string(), "control".into(), session.to_string()];
        if !control {
            args.push("--observe".into());
        } else if let Some((cols, rows)) = size {
            args.extend([
                "--cols".into(),
                cols.to_string(),
                "--rows".into(),
                rows.to_string(),
            ]);
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut command = crate::dashboard_transport::local_client_command(
            client,
            &args,
            crate::server_catalog::service_local_context(),
            crate::server_catalog::selected_vm_context()?.as_ref(),
            crate::server_catalog::selected_target_identity()?.as_ref(),
        );
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().context("starting the pane stream")?;
        let mut stdin = child.stdin.take().context("the pane stream's stdin")?;
        let mut lines =
            BufReader::new(child.stdout.take().context("the pane stream's stdout")?).lines();
        let mut stderr = child.stderr.take().context("the pane stream's stderr")?;
        let mode =
            serde_json::json!({"type": "mode", "control": control, "may_control": may_control});
        // Control is said once herdr has drawn for it: until then it may
        // yet be refused.
        let mut announced = !control;
        if announced && !say(ws, mode.clone()).await {
            return Ok(());
        }
        let mut drawn: Option<(u64, u64)> = None;
        let end = loop {
            tokio::select! {
                line = lines.next_line() => match line {
                    Ok(Some(line)) => match herdr_line(&line) {
                        HerdrLine::Frame { bytes, size: at } => {
                            waits = RELOCATE_WAITS.iter();
                            if !announced {
                                announced = true;
                                if !say(ws, mode.clone()).await {
                                    break End::Gone;
                                }
                            }
                            if let Some((cols, rows)) = at.filter(|at| Some(*at) != drawn) {
                                drawn = at;
                                if !say(ws, serde_json::json!({"type": "size", "cols": cols, "rows": rows})).await {
                                    break End::Gone;
                                }
                            }
                            if ws.send(Message::binary(bytes)).await.is_err() {
                                break End::Gone;
                            }
                        }
                        HerdrLine::Closed(reason) => break End::Closed(reason),
                        HerdrLine::Other => {}
                    },
                    Ok(None) | Err(_) => {
                        let mut said = String::new();
                        let _ = stderr.read_to_string(&mut said).await;
                        let status = child.wait().await.ok().and_then(|s| s.code());
                        let said = said.trim().to_string();
                        break if control && status == Some(crate::pane::INPUT_REFUSED) {
                            End::Refused(said)
                        } else if said.is_empty() {
                            End::Closed("the pane stream ended".into())
                        } else {
                            End::Closed(said)
                        };
                    }
                },
                message = ws.next() => match message {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break End::Gone,
                    Some(Ok(message)) => match ask_of(&message) {
                        Ask::Control if !control && !may_control => {
                            if !say(ws, serde_json::json!({"type": "refused", "reason": NO_CONTROL})).await {
                                break End::Gone;
                            }
                        }
                        Ask::Control if !control => break End::Switch(true),
                        Ask::Release if control => break End::Switch(false),
                        Ask::Size(cols, rows) => {
                            size = Some((cols, rows));
                            if control {
                                let line = serde_json::json!({"type": "terminal.resize", "cols": cols, "rows": rows});
                                if stdin.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                                    break End::Closed("the pane stream stopped taking input".into());
                                }
                            }
                        }
                        Ask::Herdr(line)
                            if control && stdin.write_all(line.as_bytes()).await.is_err() =>
                        {
                            break End::Closed("the pane stream stopped taking input".into());
                        }
                        _ => {}
                    },
                },
            }
        };
        // Closing stdin detaches a controller cleanly, which gives the pane
        // back its own size; an observer takes that as its cue to stop.
        drop(stdin);
        if tokio::time::timeout(std::time::Duration::from_secs(2), child.wait())
            .await
            .is_err()
        {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        match end {
            End::Gone => return Ok(()),
            End::Switch(wanted) => control = wanted,
            End::Refused(reason) => {
                control = false;
                if !say(ws, serde_json::json!({"type": "refused", "reason": reason})).await {
                    return Ok(());
                }
            }
            End::Closed(reason) if relocatable(&reason) => {
                let Some(&wait) = waits.next() else {
                    anyhow::bail!("{reason}; the pane did not come back");
                };
                let text = format!("{reason}; looking for the pane again in {wait}s");
                if !say(ws, serde_json::json!({"type": "notice", "text": text})).await {
                    return Ok(());
                }
                // The client may go, or change its mind, meanwhile.
                let pause = tokio::time::sleep(std::time::Duration::from_secs(wait));
                tokio::pin!(pause);
                loop {
                    tokio::select! {
                        _ = &mut pause => break,
                        message = ws.next() => match message {
                            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return Ok(()),
                            Some(Ok(message)) => match ask_of(&message) {
                                Ask::Control => control = may_control,
                                Ask::Release => control = false,
                                Ask::Size(cols, rows) => size = Some((cols, rows)),
                                _ => {}
                            },
                        },
                    }
                }
            }
            End::Closed(reason) => {
                // Taken over elsewhere, or a failure: say so, and go on
                // watching rather than take the pane back.
                control = false;
                let text = format!("{reason}; watching the pane again");
                if !say(ws, serde_json::json!({"type": "notice", "text": text})).await {
                    return Ok(());
                }
                let Some(&wait) = waits.next() else {
                    anyhow::bail!("{reason}");
                };
                tokio::time::sleep(std::time::Duration::from_secs(wait.min(2))).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn herdr_lines_are_read() {
        assert_eq!(
            herdr_line(
                r#"{"type":"terminal.frame","seq":1,"bytes":"aGk=","width":120,"height":40}"#
            ),
            HerdrLine::Frame {
                bytes: b"hi".to_vec(),
                size: Some((120, 40))
            }
        );
        assert_eq!(
            herdr_line(r#"{"type":"terminal.closed","reason":"terminal t exited"}"#),
            HerdrLine::Closed("terminal t exited".into())
        );
        assert_eq!(
            herdr_line(r#"{"type":"terminal.frame","bytes":"%%"}"#),
            HerdrLine::Other
        );
        assert_eq!(herdr_line("not json"), HerdrLine::Other);
    }

    #[test]
    fn client_messages_become_herdr_commands() {
        assert_eq!(
            ask_of(&Message::binary(b"hi".to_vec())),
            Ask::Herdr("{\"bytes\":\"aGk=\",\"type\":\"terminal.input\"}\n".into())
        );
        assert_eq!(
            ask_of(&Message::text(r#"{"type":"resize","cols":80,"rows":24}"#)),
            Ask::Size(80, 24)
        );
        // One wheel notch is one scroll, whatever else the message says.
        let Ask::Herdr(line) = ask_of(&Message::text(
            r#"{"type":"scroll","direction":"up","lines":999}"#,
        )) else {
            panic!("a scroll is a herdr command");
        };
        assert!(
            line.contains("\"terminal.scroll\"")
                && line.contains("\"lines\":3")
                && line.contains("\"up\""),
            "{line}"
        );
        assert_eq!(
            ask_of(&Message::text(r#"{"type":"scroll","direction":"left"}"#)),
            Ask::Nothing
        );
        assert_eq!(
            ask_of(&Message::text(r#"{"type":"control"}"#)),
            Ask::Control
        );
        assert_eq!(
            ask_of(&Message::text(r#"{"type":"release"}"#)),
            Ask::Release
        );
        // Nothing a client says becomes a takeover or a raw herdr command.
        assert_eq!(
            ask_of(&Message::text(r#"{"type":"terminal.release"}"#)),
            Ask::Nothing
        );
        assert_eq!(
            ask_of(&Message::text(r#"{"type":"takeover"}"#)),
            Ask::Nothing
        );
    }

    #[test]
    fn only_a_pane_that_went_away_is_looked_for_again() {
        assert!(relocatable("terminal term_1 exited"));
        assert!(relocatable("terminal target w1:p1 not found"));
        assert!(relocatable("no agent is running in o/r#1's workspace"));
        assert!(!relocatable("terminal attach taken over"));
        assert!(!relocatable("detached"));
    }

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
