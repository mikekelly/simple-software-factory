//! The web endpoint's terminal (#491, #563, #565): `GET
//! /<capability>/api/term/<session>` upgrades to a WebSocket on a session's
//! herdr pane, an item's or a scratch session's alike.
//!
//! The pane is bridged to `ssf __pane control <session>` over pipes, run
//! through the same client transport as the status stream (so a factory in
//! a VM is reached in the guest): `herdr terminal session control` NDJSON
//! both ways. herdr lets one client control a pane, so this server holds
//! one control stream per pane and shares it among every viewer: each sees
//! the output. One viewer at a time holds control (#574): only its typing,
//! paste and wheel reach the pane, and the pane takes its size; the others'
//! are dropped here, and they see the pane at its size. The first viewer to
//! join a pane no one controls takes control, any viewer can take it
//! (`take`), and when the controller goes no one holds it until a viewer
//! takes it or joins. Only a request allowed to type opens one at all
//! (decided from the request by the caller, and by `item_pane_input` where
//! the config is). See [`bridge_item`] for the messages.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{Message, handshake::derive_accept_key, protocol::Role};

/// Terminals open at once: each is a viewer of a pane's control stream.
const MAX_TERMS: usize = 16;
static OPEN: AtomicUsize = AtomicUsize::new(0);

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

/// Answer the upgrade and bridge the socket and the pane until either
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
    if let Err(error) = bridge_item(&mut ws, session, client).await {
        let _ = ws
            .send(Message::binary(
                format!("\r\nssf: {error:#}\r\n").into_bytes(),
            ))
            .await;
    }
    let _ = ws.close(None).await;
    tracing::debug!(session, "web API terminal closed");
}

/// Lines a wheel notch scrolls herdr's history by. One `terminal.scroll` per
/// notch: in a full-screen TUI with the mouse on, herdr passes each one to
/// the app as one wheel event.
const SCROLL_LINES: u64 = 3;

/// How long a pane that went away, or that someone outside holds, is tried
/// again, one wait after another: a relaunch records its new pane in the
/// state within seconds.
const RETRY_WAITS: [u64; 8] = [1, 2, 4, 8, 10, 10, 10, 15];

/// How often a viewer is pinged, and how long one may say nothing (a pong
/// counts) before it is taken for gone.
const PING_EVERY: std::time::Duration = std::time::Duration::from_secs(20);
const LIVENESS: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a new viewer has to say who it is before it is named for it.
const HELLO_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// What a client's message asks of an item's terminal.
#[derive(Debug, PartialEq)]
pub(crate) enum Ask {
    /// A line for `herdr terminal session control`'s stdin: typed bytes or
    /// a scroll.
    Herdr(String),
    /// The viewer's size, which the pane takes while it holds control.
    Size(u16, u16),
    /// Control of the pane, for this viewer.
    Take,
    /// Who the viewer is (display only, see [`viewer_name`]), and its size.
    Hello(String, Option<(u16, u16)>),
    Nothing,
}

/// What a client's WebSocket message asks: binary frames are typed bytes;
/// text frames are JSON, `hello` (`name`, and `cols`/`rows`), `resize`,
/// `take` (control) or `scroll` (one wheel notch, `up` or `down`).
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
        Some("hello") => {
            let name = value.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let size = resize_of(
                &serde_json::json!({"type": "resize", "cols": value.get("cols"), "rows": value.get("rows")})
                    .to_string(),
            );
            Ask::Hello(viewer_name(name), size)
        }
        Some("take") => Ask::Take,
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

/// The name a viewer is shown by: `@login` for a GitHub login (at most 39
/// of `A-Za-z0-9-`), `dashboard` or `extension`, and `viewer` for anything
/// else. It is for display only: nothing is allowed by it.
pub(crate) fn viewer_name(raw: &str) -> String {
    match raw {
        "dashboard" | "extension" => raw.to_string(),
        _ => match raw.strip_prefix('@') {
            Some(login)
                if (1..=39).contains(&login.len())
                    && login
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-') =>
            {
                raw.to_string()
            }
            _ => "viewer".to_string(),
        },
    }
}

/// What one NDJSON line from herdr means for the viewers.
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

/// What a pane's shared stream sends every viewer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Out {
    /// Terminal bytes.
    Bytes(Vec<u8>),
    /// A JSON message: `size` or `notice`.
    Text(String),
    /// Who is watching, in the order they joined, and which of them (by
    /// id) holds control: each viewer is told whether it is that one.
    Viewers(Vec<(u64, String)>, Option<u64>),
    /// The stream is over, and why.
    End(String),
}

/// What a viewer asks of the stream.
#[derive(Debug)]
enum In {
    Line(String),
    Size(u16, u16),
    /// Draw the whole screen again, for a viewer that joined late.
    Redraw,
    /// The factory keeps the pane view-only now: end the stream for all.
    Refuse(String),
}

/// Runs `ssf __pane input-check` for a pane: whether it still takes typing.
pub(crate) type Check = std::sync::Arc<dyn Fn() -> Result<tokio::process::Command> + Send + Sync>;

/// How often a running stream asks again whether its pane takes typing.
const RECHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

/// Why the factory keeps the pane view-only, where `check` says it does.
/// A check that fails otherwise (a slow VM, say) is not a refusal.
async fn refusal(check: &Check) -> Option<String> {
    let mut command = check().ok()?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(std::time::Duration::from_secs(15), command.output())
        .await
        .ok()?
        .ok()?;
    (out.status.code() == Some(crate::pane::INPUT_REFUSED))
        .then(|| String::from_utf8_lossy(&out.stderr).trim().to_string())
}

/// End `session`'s running stream, if there is one, for every viewer.
fn refuse(session: &str, why: &str) {
    if let Some(stream) = STREAMS
        .lock()
        .unwrap()
        .get(session)
        .and_then(std::sync::Weak::upgrade)
    {
        let _ = stream.input.send(In::Refuse(why.to_string()));
    }
}

/// Starts `ssf __pane control` for a pane, at a size or the pane's own.
pub(crate) type Spawn =
    std::sync::Arc<dyn Fn(Option<(u16, u16)>) -> Result<tokio::process::Command> + Send + Sync>;

/// A viewer on a stream's list: its id, name, and the size it last asked for.
type ViewerEntry = (u64, String, Option<(u16, u16)>);

/// One pane's control stream (#563), shared by every viewer of it: herdr
/// lets one client control a pane, so this server holds one and fans it out.
/// It is released when the last viewer goes (its input channel closes).
pub(crate) struct Stream {
    input: tokio::sync::mpsc::UnboundedSender<In>,
    output: tokio::sync::broadcast::Sender<Out>,
    /// Each viewer: its id, name, and the size it last asked for.
    viewers: std::sync::Mutex<Vec<ViewerEntry>>,
    /// The viewer holding control: its input reaches the pane, and the
    /// pane takes its size. Locked after `viewers` where both are.
    controller: std::sync::Mutex<Option<u64>>,
}

impl Stream {
    fn tell_viewers(&self) {
        let viewers: Vec<(u64, String)> = self
            .viewers
            .lock()
            .unwrap()
            .iter()
            .map(|(id, n, _)| (*id, n.clone()))
            .collect();
        let controller = *self.controller.lock().unwrap();
        let _ = self.output.send(Out::Viewers(viewers, controller));
    }
}

/// The `viewers` message for viewer `me`: the names, the controller's name
/// (or null), and whether `me` holds control.
fn viewers_message(viewers: &[(u64, String)], controller: Option<u64>, me: u64) -> String {
    let names: Vec<&str> = viewers.iter().map(|(_, n)| n.as_str()).collect();
    let holder = viewers
        .iter()
        .find(|(id, _)| Some(*id) == controller)
        .map(|(_, n)| n.as_str());
    serde_json::json!({
        "type": "viewers",
        "names": names,
        "controller": holder,
        "control": controller == Some(me),
    })
    .to_string()
}

type Streams = std::collections::HashMap<String, std::sync::Weak<Stream>>;
static STREAMS: std::sync::LazyLock<std::sync::Mutex<Streams>> =
    std::sync::LazyLock::new(Default::default);
static VIEWER_IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One viewer of a shared stream; going (drop) takes it off the list.
pub(crate) struct Viewer {
    stream: std::sync::Arc<Stream>,
    id: u64,
}

impl Viewer {
    fn ask(&self, ask: In) {
        let _ = self.stream.input.send(ask);
    }

    fn controls(&self) -> bool {
        *self.stream.controller.lock().unwrap() == Some(self.id)
    }

    /// Typing, paste or a scroll: it reaches the pane only from the
    /// controller, and is dropped from anyone else.
    fn input(&self, line: String) {
        if self.controls() {
            self.ask(In::Line(line));
        }
    }

    /// This viewer's size, kept for when it takes control; the pane takes
    /// it now only if it holds control.
    fn resize(&self, cols: u16, rows: u16) {
        for viewer in self.stream.viewers.lock().unwrap().iter_mut() {
            if viewer.0 == self.id {
                viewer.2 = Some((cols, rows));
            }
        }
        if self.controls() {
            self.ask(In::Size(cols, rows));
        }
    }

    /// Take control from whoever holds it: the pane takes this viewer's
    /// size, and every viewer hears who holds it now.
    fn take(&self) {
        {
            let viewers = self.stream.viewers.lock().unwrap();
            let mut controller = self.stream.controller.lock().unwrap();
            if *controller == Some(self.id) {
                return;
            }
            *controller = Some(self.id);
            // Sized under the lock, so two takes at once leave the pane at
            // the size of whichever holds control last.
            let size = viewers
                .iter()
                .find(|(id, _, _)| *id == self.id)
                .and_then(|(_, _, size)| *size);
            if let Some((cols, rows)) = size {
                self.ask(In::Size(cols, rows));
            }
        }
        self.stream.tell_viewers();
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let mut viewers = self.stream.viewers.lock().unwrap();
        viewers.retain(|(id, _, _)| *id != self.id);
        // Control does not pass on by itself: no one holds it until a
        // viewer takes it, or joins, and the pane keeps its size meanwhile.
        let mut controller = self.stream.controller.lock().unwrap();
        if *controller == Some(self.id) {
            *controller = None;
        }
        drop((viewers, controller));
        self.stream.tell_viewers();
    }
}

/// Join `session`'s shared stream as `name`, starting it (at `size`) if no
/// one else is watching. The receiver hears everything from now on; the
/// viewer asks for the screen to be drawn again, so it starts with it.
pub(crate) fn join(
    session: &str,
    name: String,
    size: Option<(u16, u16)>,
    spawn: Spawn,
    check: Check,
) -> (Viewer, tokio::sync::broadcast::Receiver<Out>) {
    let mut streams = STREAMS.lock().unwrap();
    let stream = match streams.get(session).and_then(std::sync::Weak::upgrade) {
        Some(stream) => stream,
        None => {
            let (input, asks) = tokio::sync::mpsc::unbounded_channel();
            let (output, _) = tokio::sync::broadcast::channel(256);
            let stream = std::sync::Arc::new(Stream {
                input,
                output: output.clone(),
                viewers: Default::default(),
                controller: Default::default(),
            });
            let weak = std::sync::Arc::downgrade(&stream);
            streams.insert(session.to_string(), weak.clone());
            tokio::spawn(pump(
                session.to_string(),
                weak,
                asks,
                output,
                // At the pane's own size: no one controls it yet.
                None,
                spawn,
                check,
            ));
            stream
        }
    };
    let receiver = stream.output.subscribe();
    let id = VIEWER_IDS.fetch_add(1, Ordering::SeqCst);
    stream.viewers.lock().unwrap().push((id, name, size));
    // Every viewer joins view only (#606): control is only ever taken.
    drop(streams);
    stream.tell_viewers();
    let _ = stream.input.send(In::Redraw);
    (Viewer { stream, id }, receiver)
}

/// How one run of `ssf __pane control` ended.
enum End {
    /// Every viewer went.
    Gone,
    /// herdr's `terminal.closed`, or what the command said when it stopped:
    /// the pane went, or someone outside holds it.
    Closed(String),
    /// The factory keeps the pane view-only (`item_pane_input`).
    Refused(String),
}

/// Run the pane's control, again after it ends while viewers remain (with
/// [`RETRY_WAITS`]), until the last viewer goes, the factory refuses it, or
/// the pane does not come back. Every viewer hears what it says.
async fn pump(
    session: String,
    stream: std::sync::Weak<Stream>,
    mut asks: tokio::sync::mpsc::UnboundedReceiver<In>,
    output: tokio::sync::broadcast::Sender<Out>,
    mut size: Option<(u16, u16)>,
    spawn: Spawn,
    check: Check,
) {
    let reason = run_pump(&mut asks, &output, &mut size, &spawn, &check).await;
    // Off the list first, and under its lock, so a viewer joining now
    // either hears the end or starts a stream of its own.
    let mut streams = STREAMS.lock().unwrap();
    if streams
        .get(&session)
        .is_some_and(|held| held.ptr_eq(&stream))
    {
        streams.remove(&session);
    }
    if let Some(reason) = reason {
        let _ = output.send(Out::End(reason));
    }
}

/// [`pump`]'s loop: `None` once every viewer went, or why it stopped.
async fn run_pump(
    asks: &mut tokio::sync::mpsc::UnboundedReceiver<In>,
    output: &tokio::sync::broadcast::Sender<Out>,
    size: &mut Option<(u16, u16)>,
    spawn: &Spawn,
    check: &Check,
) -> Option<String> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let say = |message: serde_json::Value| {
        let _ = output.send(Out::Text(message.to_string()));
    };
    let mut waits = RETRY_WAITS.iter();
    loop {
        let started = spawn(*size).and_then(|mut command| {
            command
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true);
            command.spawn().context("starting the pane stream")
        });
        let mut child = match started {
            Ok(child) => child,
            Err(error) => return Some(format!("{error:#}")),
        };
        let (Some(mut stdin), Some(stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Some("the pane stream has no pipes".into());
        };
        let mut lines = BufReader::new(stdout).lines();
        let mut drawn: Option<(u64, u64)> = None;
        let mut recheck =
            tokio::time::interval_at(tokio::time::Instant::now() + RECHECK_EVERY, RECHECK_EVERY);
        let end = loop {
            tokio::select! {
                // `item_pane_input` turned off meanwhile ends the stream.
                _ = recheck.tick() => {
                    if let Some(why) = refusal(check).await {
                        break End::Refused(why);
                    }
                }
                line = lines.next_line() => match line {
                    Ok(Some(line)) => match herdr_line(&line) {
                        HerdrLine::Frame { bytes, size: at } => {
                            waits = RETRY_WAITS.iter();
                            if let Some((cols, rows)) = at.filter(|at| Some(*at) != drawn) {
                                drawn = at;
                                say(serde_json::json!({"type": "size", "cols": cols, "rows": rows}));
                            }
                            let _ = output.send(Out::Bytes(bytes));
                        }
                        HerdrLine::Closed(reason) => break End::Closed(reason),
                        HerdrLine::Other => {}
                    },
                    Ok(None) | Err(_) => {
                        let mut said = String::new();
                        let _ = stderr.read_to_string(&mut said).await;
                        let status = child.wait().await.ok().and_then(|s| s.code());
                        let said = said.trim().to_string();
                        break if status == Some(crate::pane::INPUT_REFUSED) {
                            End::Refused(said)
                        } else if said.is_empty() {
                            End::Closed("the pane stream ended".into())
                        } else {
                            End::Closed(said)
                        };
                    }
                },
                ask = asks.recv() => {
                    let line = match ask {
                        None => break End::Gone,
                        Some(In::Refuse(why)) => break End::Refused(why),
                        Some(In::Line(line)) => line,
                        Some(In::Size(cols, rows)) => {
                            *size = Some((cols, rows));
                            format!("{}\n", serde_json::json!({"type": "terminal.resize", "cols": cols, "rows": rows}))
                        }
                        // The same size again draws the whole screen, which
                        // is what a new viewer needs; before the first frame
                        // there is nothing to draw again: that one is whole.
                        Some(In::Redraw) => match drawn.take() {
                            Some((cols, rows)) => format!("{}\n", serde_json::json!({"type": "terminal.resize", "cols": cols, "rows": rows})),
                            None => continue,
                        },
                    };
                    if stdin.write_all(line.as_bytes()).await.is_err() {
                        break End::Closed("the pane stream stopped taking input".into());
                    }
                }
            }
        };
        // Closing stdin detaches the controller cleanly, which gives the
        // pane back its own size.
        drop(stdin);
        if tokio::time::timeout(std::time::Duration::from_secs(2), child.wait())
            .await
            .is_err()
        {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        let reason = match end {
            End::Gone => return None,
            End::Refused(reason) => return Some(reason),
            End::Closed(reason) => reason,
        };
        let Some(&wait) = waits.next() else {
            return Some(format!("{reason}; the pane did not come back"));
        };
        say(
            serde_json::json!({"type": "notice", "text": format!("{reason}; trying the pane again in {wait}s")}),
        );
        // Viewers may go, or resize, meanwhile; what they type is dropped.
        let pause = tokio::time::sleep(std::time::Duration::from_secs(wait));
        tokio::pin!(pause);
        loop {
            tokio::select! {
                _ = &mut pause => break,
                ask = asks.recv() => match ask {
                    None => return None,
                    Some(In::Refuse(why)) => return Some(why),
                    Some(In::Size(cols, rows)) => *size = Some((cols, rows)),
                    Some(_) => {}
                },
            }
        }
    }
}

/// Bridge the socket to an item's pane (#563) as one viewer of its shared
/// stream: `ssf __pane control` run by the factory's client (so a factory
/// in a VM streams from the guest). The client says `hello` first (its
/// name, and size); text frames to it are JSON:
///
/// - `{"type":"size","cols":N,"rows":N}` when the pane's size changes (and
///   once on joining);
/// - `{"type":"viewers","names":[…],"controller":name|null,"control":bool}`
///   whenever a viewer joins or goes, or control moves: `control` says
///   whether this viewer holds it. Only the controller's typing, `scroll`
///   and `resize` reach the pane; `{"type":"take"}` takes control;
/// - `{"type":"notice","text":"…"}` for anything else worth saying.
async fn bridge_item(
    ws: &mut WebSocketStream<TcpStream>,
    session: &str,
    client: &Path,
) -> Result<()> {
    // A first message that is not `hello` is handled as any other, once
    // the viewer has joined.
    let mut first = None;
    let (name, size) = match tokio::time::timeout(HELLO_WAIT, ws.next()).await {
        Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) | Ok(None) => return Ok(()),
        Ok(Some(Ok(message))) => match ask_of(&message) {
            Ask::Hello(name, size) => (name, size),
            _ => {
                first = Some(message);
                (viewer_name(""), None)
            }
        },
        Err(_) => (viewer_name(""), None),
    };
    let vm = crate::server_catalog::selected_vm_context()?;
    let identity = crate::server_catalog::selected_target_identity()?;
    let (client, owned) = (client.to_path_buf(), session.to_string());
    let check: Check = {
        let (client, owned, vm, identity) =
            (client.clone(), owned.clone(), vm.clone(), identity.clone());
        std::sync::Arc::new(move || {
            Ok(crate::dashboard_transport::local_client_command(
                &client,
                &["__pane", "input-check", &owned],
                crate::server_catalog::service_local_context(),
                vm.as_ref(),
                identity.as_ref(),
            ))
        })
    };
    // Every viewer is checked as it joins, and a pane the factory keeps
    // view-only now ends a running stream for everyone.
    if let Some(why) = refusal(&check).await {
        refuse(session, &why);
        anyhow::bail!("{why}");
    }
    let spawn: Spawn = std::sync::Arc::new(move |size: Option<(u16, u16)>| {
        let mut args = vec!["__pane".to_string(), "control".into(), owned.clone()];
        if let Some((cols, rows)) = size {
            args.extend([
                "--cols".into(),
                cols.to_string(),
                "--rows".into(),
                rows.to_string(),
            ]);
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        Ok(crate::dashboard_transport::local_client_command(
            &client,
            &args,
            crate::server_catalog::service_local_context(),
            vm.as_ref(),
            identity.as_ref(),
        ))
    });
    let (viewer, mut heard) = join(session, name, size, spawn, check);
    let apply = |message: &Message| match ask_of(message) {
        Ask::Herdr(line) => viewer.input(line),
        Ask::Size(cols, rows) => viewer.resize(cols, rows),
        Ask::Take => viewer.take(),
        Ask::Hello(..) | Ask::Nothing => {}
    };
    if let Some(message) = first {
        apply(&message);
    }
    // A viewer that says nothing, not even a pong, for LIVENESS is gone
    // (asleep, say, with no FIN): it holds the pane no longer.
    let mut ping = tokio::time::interval(PING_EVERY);
    let mut heard_at = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if heard_at.elapsed() >= LIVENESS {
                    return Ok(());
                }
                if ws.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return Ok(());
                }
            },
            out = heard.recv() => match out {
                Ok(Out::Bytes(bytes)) => {
                    if ws.send(Message::binary(bytes)).await.is_err() {
                        return Ok(());
                    }
                }
                Ok(Out::Viewers(viewers, controller)) => {
                    let text = viewers_message(&viewers, controller, viewer.id);
                    if ws.send(Message::text(text)).await.is_err() {
                        return Ok(());
                    }
                }
                Ok(Out::Text(text)) => {
                    if ws.send(Message::text(text)).await.is_err() {
                        return Ok(());
                    }
                }
                Ok(Out::End(reason)) => anyhow::bail!("{reason}"),
                // Fell behind: what it missed is drawn again whole.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => viewer.ask(In::Redraw),
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
            },
            message = ws.next() => match message {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return Ok(()),
                Some(Ok(message)) => {
                    heard_at = tokio::time::Instant::now();
                    apply(&message);
                }
            },
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
            ask_of(&Message::text(
                r#"{"type":"hello","name":"@mikekelly","cols":100,"rows":30}"#
            )),
            Ask::Hello("@mikekelly".into(), Some((100, 30)))
        );
        assert_eq!(
            ask_of(&Message::text(r#"{"type":"hello"}"#)),
            Ask::Hello("viewer".into(), None)
        );
        assert_eq!(ask_of(&Message::text(r#"{"type":"take"}"#)), Ask::Take);
        // Nothing a client says becomes a takeover or a raw herdr command.
        assert_eq!(
            ask_of(&Message::text(r#"{"type":"control"}"#)),
            Ask::Nothing
        );
        assert_eq!(
            ask_of(&Message::text(r#"{"type":"takeover"}"#)),
            Ask::Nothing
        );
    }

    /// A viewer's name is for display: a login, or one of the fixed names,
    /// and nothing else gets through as it was sent.
    #[test]
    fn viewer_names_are_sanitized() {
        assert_eq!(viewer_name("@mikekelly"), "@mikekelly");
        assert_eq!(viewer_name("@a-b-1"), "@a-b-1");
        assert_eq!(viewer_name("dashboard"), "dashboard");
        assert_eq!(viewer_name("extension"), "extension");
        for bad in [
            "",
            "@",
            "mikekelly",
            "@<script>",
            "@a b",
            "@\u{1b}[31m",
            &format!("@{}", "a".repeat(40)),
        ] {
            assert_eq!(viewer_name(bad), "viewer", "{bad:?}");
        }
        assert_eq!(viewer_name(&format!("@{}", "a".repeat(39))).len(), 40);
    }

    /// A stand-in for `ssf __pane control`: a frame saying it started (at
    /// the size it was given), then every stdin line back as a frame, and a
    /// mark in `gone` when its stdin closes (the pane released).
    fn fake_pane(gone: &Path) -> Spawn {
        let gone = gone.to_path_buf();
        std::sync::Arc::new(move |size: Option<(u16, u16)>| {
            let size = size.map_or("none".to_string(), |(c, r)| format!("{c}x{r}"));
            let script = format!(
                r#"frame() {{ printf '{{"type":"terminal.frame","bytes":"%s","width":80,"height":24}}\n' "$(printf '%s' "$1" | base64 -w0)"; }}
frame "started {size}"
while IFS= read -r line; do frame "$line"; done
echo released > '{}'"#,
                gone.display()
            );
            let mut command = tokio::process::Command::new("sh");
            command.args(["-c", &script]);
            Ok(command)
        })
    }

    /// A check that always finds the pane taking typing.
    fn allowed() -> Check {
        std::sync::Arc::new(|| Ok(tokio::process::Command::new("true")))
    }

    /// The next bytes a viewer is sent, skipping JSON messages.
    async fn next_bytes(heard: &mut tokio::sync::broadcast::Receiver<Out>) -> String {
        loop {
            let out = tokio::time::timeout(std::time::Duration::from_secs(5), heard.recv())
                .await
                .expect("a frame in time")
                .expect("the stream is open");
            if let Out::Bytes(bytes) = out {
                return String::from_utf8_lossy(&bytes).into_owned();
            }
        }
    }

    /// The next list of viewers a viewer is sent, and who holds control.
    async fn next_list(
        heard: &mut tokio::sync::broadcast::Receiver<Out>,
    ) -> (Vec<String>, Option<String>) {
        loop {
            let out = tokio::time::timeout(std::time::Duration::from_secs(5), heard.recv())
                .await
                .expect("a message in time")
                .expect("the stream is open");
            if let Out::Viewers(viewers, controller) = out {
                let value: serde_json::Value =
                    serde_json::from_str(&viewers_message(&viewers, controller, u64::MAX)).unwrap();
                return (
                    serde_json::from_value(value["names"].clone()).unwrap(),
                    value["controller"].as_str().map(str::to_string),
                );
            }
        }
    }

    async fn next_viewers(heard: &mut tokio::sync::broadcast::Receiver<Out>) -> Vec<String> {
        next_list(heard).await.0
    }

    async fn next_control(heard: &mut tokio::sync::broadcast::Receiver<Out>) -> Option<String> {
        next_list(heard).await.1
    }

    /// Each viewer is told the controller's name, and whether it is it.
    #[test]
    fn the_viewers_message_says_who_controls() {
        let viewers = [(1, "@a".to_string()), (2, "dashboard".to_string())];
        let of = |controller, me| -> serde_json::Value {
            serde_json::from_str(&viewers_message(&viewers, controller, me)).unwrap()
        };
        assert_eq!(
            of(Some(2), 2),
            serde_json::json!({"type": "viewers", "names": ["@a", "dashboard"], "controller": "dashboard", "control": true})
        );
        assert_eq!(of(Some(2), 1)["control"], false);
        assert_eq!(of(None, 1)["controller"], serde_json::Value::Null);
        assert_eq!(of(None, 1)["control"], false);
    }

    /// Viewers share one stream: all see its output, only the controller's
    /// typing and size reach the pane, control moves when taken and goes
    /// with its holder, a late joiner has the screen drawn again, everyone
    /// hears who is watching and who controls, and the pane is released
    /// only when the last one goes.
    #[tokio::test]
    async fn viewers_share_one_stream_until_the_last_goes() {
        let gone = std::env::temp_dir().join(format!("ssf-shared-{}", std::process::id()));
        let _ = std::fs::remove_file(&gone);
        let session = "o/shared#1";
        let (a, mut heard_a) = join(
            session,
            "@alice".into(),
            Some((100, 30)),
            fake_pane(&gone),
            allowed(),
        );
        assert_eq!(next_viewers(&mut heard_a).await, ["@alice"]);
        assert_eq!(next_bytes(&mut heard_a).await, "started none");
        // The second viewer joins the same stream (no second start), and
        // asks for the screen again: a same-size resize, drawn to both.
        let (b, mut heard_b) = join(
            session,
            "dashboard".into(),
            Some((50, 10)),
            fake_pane(&gone),
            allowed(),
        );
        assert_eq!(next_viewers(&mut heard_a).await, ["@alice", "dashboard"]);
        assert_eq!(next_viewers(&mut heard_b).await, ["@alice", "dashboard"]);
        let redraw = next_bytes(&mut heard_b).await;
        assert!(
            redraw.contains("terminal.resize") && redraw.contains("\"cols\":80"),
            "{redraw}"
        );
        assert_eq!(next_bytes(&mut heard_a).await, redraw);
        // Joining never takes control (#606): no one's typing or size
        // reaches the pane until someone takes it.
        a.input("dropped\n".into());
        a.resize(60, 12);
        a.take();
        assert_eq!(next_control(&mut heard_b).await, Some("@alice".into()));
        assert_eq!(next_control(&mut heard_a).await, Some("@alice".into()));
        assert!(next_bytes(&mut heard_a).await.contains("\"cols\":60"));
        assert!(next_bytes(&mut heard_b).await.contains("\"cols\":60"));
        // Only the controller's typing reaches the pane, and only its
        // size; the other's are dropped.
        a.input("from a\n".into());
        b.input("from b\n".into());
        a.input("again a\n".into());
        for text in ["from a", "again a"] {
            assert_eq!(next_bytes(&mut heard_a).await, text);
            assert_eq!(next_bytes(&mut heard_b).await, text);
        }
        b.resize(120, 40);
        a.resize(90, 20);
        assert!(next_bytes(&mut heard_a).await.contains("\"cols\":90"));
        assert!(next_bytes(&mut heard_b).await.contains("\"cols\":90"));
        // Taking control moves it, with the taker's size, and everyone
        // hears who holds it.
        b.take();
        assert_eq!(next_control(&mut heard_a).await, Some("dashboard".into()));
        assert!(next_bytes(&mut heard_a).await.contains("\"cols\":120"));
        a.input("dropped\n".into());
        b.input("from b\n".into());
        assert_eq!(next_bytes(&mut heard_a).await, "from b");
        // When the controller goes no one holds control, and what is
        // typed is dropped, until a viewer takes it: joining does not.
        drop(b);
        assert_eq!(next_viewers(&mut heard_a).await, ["@alice"]);
        a.input("dropped\n".into());
        let (d, mut heard_d) = join(
            session,
            "extension".into(),
            Some((70, 15)),
            fake_pane(&gone),
            allowed(),
        );
        assert_eq!(next_viewers(&mut heard_a).await, ["@alice", "extension"]);
        assert_eq!(next_control(&mut heard_d).await, None);
        a.take();
        assert_eq!(next_control(&mut heard_d).await, Some("@alice".into()));
        assert_eq!(next_control(&mut heard_a).await, Some("@alice".into()));
        a.input("mine again\n".into());
        loop {
            let bytes = next_bytes(&mut heard_d).await;
            assert!(!bytes.contains("dropped"), "{bytes}");
            if bytes == "mine again" {
                break;
            }
        }
        drop(d);
        assert_eq!(next_viewers(&mut heard_a).await, ["@alice"]);
        let b = a;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!gone.exists(), "released while a viewer remained");
        drop(b);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !gone.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the pane is released when the last viewer goes");
        // The next viewer starts a stream of its own.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (_c, mut heard_c) = join(
            session,
            "extension".into(),
            None,
            fake_pane(&gone),
            allowed(),
        );
        assert_eq!(next_viewers(&mut heard_c).await, ["extension"]);
        assert_eq!(next_bytes(&mut heard_c).await, "started none");
        let _ = std::fs::remove_file(&gone);
    }

    /// A refusal found when someone joins (`item_pane_input` turned off)
    /// ends a running stream for every viewer, and a refusing check says why.
    #[tokio::test]
    async fn a_refusal_on_joining_ends_the_running_stream() {
        let gone = std::env::temp_dir().join(format!("ssf-refuse-{}", std::process::id()));
        let (_a, mut heard) = join("o/later#3", "@a".into(), None, fake_pane(&gone), allowed());
        assert_eq!(next_bytes(&mut heard).await, "started none");
        let check: Check = std::sync::Arc::new(|| {
            let mut command = tokio::process::Command::new("sh");
            command.args(["-c", "echo 'o/later#3 is view-only now' >&2; exit 2"]);
            Ok(command)
        });
        let why = refusal(&check).await.unwrap();
        assert_eq!(why, "o/later#3 is view-only now");
        assert_eq!(refusal(&allowed()).await, None);
        refuse("o/later#3", &why);
        let end = loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), heard.recv())
                .await
                .unwrap()
                .unwrap()
            {
                Out::End(reason) => break reason,
                _ => continue,
            }
        };
        assert_eq!(end, why);
        let _ = std::fs::remove_file(&gone);
    }

    /// A pane the factory keeps view-only ends the stream for every viewer
    /// with the factory's words, and is not tried again.
    #[tokio::test]
    async fn a_refused_pane_ends_the_stream_for_its_viewers() {
        let refused: Spawn = std::sync::Arc::new(|_| {
            let mut command = tokio::process::Command::new("sh");
            command.args(["-c", "echo \"o/r#7's pane is view-only\" >&2; exit 2"]);
            Ok(command)
        });
        let (_a, mut heard) = join("o/refused#7", "@a".into(), None, refused, allowed());
        let end = loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), heard.recv())
                .await
                .unwrap()
                .unwrap()
            {
                Out::End(reason) => break reason,
                _ => continue,
            }
        };
        assert!(end.contains("view-only"), "{end}");
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
}
