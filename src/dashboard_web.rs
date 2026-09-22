//! Optional server-owned capability HTTP endpoint for canonical dashboard status.
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{future::Future, io::Read, path::Path, path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

use crate::ipc::Refused;

const INDEX: &str = include_str!("../dashboard/index.html");
const CSS: &str = include_str!("../dashboard/dashboard.css");
const JS: &str = include_str!("../dashboard/dashboard.js");
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HEADERS: usize = 8192;
/// Longest body the endpoint reads: a write carries a handful of fields, and
/// it is bounded before any of it is read.
const MAX_BODY: usize = 4096;
/// Longest message `POST api/message` carries to an agent: a note a person
/// typed into the overlay, not a document. Checked here rather than by the
/// daemon, since the command a person would use for anything longer is a
/// comment on the item.
///
/// Bytes, not characters, because that is what the request is measured in: a
/// cap in characters would be a promise the 4 KiB body bound could break for
/// text that is not ASCII, and a person refused for the size of a body rather
/// than for a message over the cap. 2 KiB of message leaves the rest of the
/// request ample room.
const MAX_MESSAGE_BYTES: usize = 2048;
/// How long a listing may wait on the factory: the client walks `PATH` and
/// may run a harness's own listing command.
const LISTING_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a write may wait on the factory. An assign's slow part is its
/// GitHub round trip, and the daemon's own client waits three minutes for
/// one; past this the client is given up on, though what the daemon was
/// doing may still land.
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);
const KEEPALIVE: Duration = Duration::from_secs(25);

/// The latest status snapshot, or the error that prevented loading one.
type Latest = tokio::sync::watch::Receiver<Option<std::result::Result<Value, String>>>;

/// Bind before starting the daemon so configuration and port failures are explicit.
pub(crate) async fn bind(config: &crate::config::DashboardConfig) -> Result<Option<TcpListener>> {
    if !config.enabled {
        return Ok(None);
    }
    config.validate()?;
    TcpListener::bind((config.bind, config.port))
        .await
        .context("could not bind server web dashboard")
        .map(Some)
}

/// The listener and daemon share a lifetime. Dropping either future closes the
/// listener, and every connection it accepted ends with the status stream the
/// handler reads; there is no browser idle expiration.
pub(crate) async fn with_daemon<F>(listener: Option<TcpListener>, daemon: F) -> Result<()>
where
    F: Future<Output = Result<()>>,
{
    let Some(listener) = listener else {
        return daemon.await;
    };
    let token = capability()?;
    // The factory this listener answers for is asked through its own client,
    // the same way the status stream below is: for a factory in a VM the
    // daemon and the harnesses are in the guest, and the client is what
    // forwards there.
    let client = crate::server_executable()?;
    tracing::info!(
        "Server web dashboard: http://{}/{token}/",
        listener.local_addr()?
    );
    // The status the listener serves is the factory this server answers
    // for: started as a catalog target, the stream has to know which one,
    // or it would read the default factory's configuration and label every
    // card with the machine's hostname (`StatusSource::new_with_context`,
    // which the TUI's own client uses the same way).
    let mut source = crate::dashboard_transport::StatusSource::new_with_context(
        None,
        crate::server_catalog::service_local_context().cloned(),
        crate::server_catalog::selected_vm_context()?,
        crate::server_catalog::selected_target_identity()?,
    )?;
    let (latest_tx, latest_rx) =
        tokio::sync::watch::channel(None::<std::result::Result<Value, String>>);
    let stream = async move {
        loop {
            let snapshot = source
                .next_snapshot()
                .await
                .map_err(|error| format!("{error:#}"));
            let failed = snapshot.is_err();
            if latest_tx.send(Some(snapshot)).is_err() {
                return Ok(());
            }
            if failed {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    };
    tokio::select! {
        result = daemon => result,
        result = stream => result,
        result = serve(listener, token, latest_rx, client) => result.context("server web dashboard stopped"),
    }
}

/// One snapshot from the watch channel, waiting for the first one if the
/// stream has not produced anything yet.
async fn load(latest: &mut Latest) -> Result<Value> {
    if latest.borrow().is_none() {
        latest
            .changed()
            .await
            .context("SSF status stream stopped")?;
    }
    match latest
        .borrow()
        .clone()
        .context("SSF status stream has not started")?
    {
        Ok(value) => Ok(value),
        Err(error) => bail!(error),
    }
}

fn capability() -> Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

async fn serve(
    listener: TcpListener,
    token: String,
    latest: Latest,
    client: PathBuf,
) -> Result<()> {
    let host: std::sync::Arc<str> = listener.local_addr()?.to_string().into();
    let token: std::sync::Arc<str> = token.into();
    let client: std::sync::Arc<PathBuf> = client.into();
    loop {
        let (stream, _) = listener.accept().await?;
        let (host, token, latest, client) =
            (host.clone(), token.clone(), latest.clone(), client.clone());
        // Each connection gets its own task so a long-lived event stream
        // does not stop the listener from answering anyone else.
        tokio::spawn(async move { handle(stream, &host, &token, latest, &client).await });
    }
}

async fn handle(mut stream: TcpStream, host: &str, token: &str, mut latest: Latest, client: &Path) {
    let request = timeout(REQUEST_TIMEOUT, read_request(&mut stream)).await;
    let routed = match &request {
        Ok(Ok(request)) => classify(request, host, token),
        _ => Err(400),
    };
    let (status, kind, body) = match routed {
        Ok(Routed::Read("api/events")) => {
            // Ends quietly when the client goes away or the daemon stops.
            events(&mut stream, &mut latest, KEEPALIVE, REQUEST_TIMEOUT).await;
            return;
        }
        Ok(Routed::Read(relative)) => read(relative, &mut latest, client).await,
        Ok(Routed::Write(accepted)) => write(&mut stream, accepted, client).await,
        Err(status) => rejected(status),
    };
    let _ = timeout(REQUEST_TIMEOUT, respond(&mut stream, status, kind, &body)).await;
}

/// What a request refused before anything was read is told. Most of these are
/// about the shape of the request, which the client that made it already knows;
/// the body bound is the one a person can reach through the overlay's own
/// message box, so it is the one worth naming.
fn rejected(status: u16) -> (u16, &'static str, String) {
    let message = match status {
        413 => format!("the request body is longer than the {MAX_BODY} bytes this endpoint reads"),
        _ => "request rejected".to_string(),
    };
    (
        status,
        "application/json",
        json!({ "error": message }).to_string(),
    )
}

/// Answer a write route: read its body and make the one `ssf` request that
/// route stands for, answered with the daemon's own result or refusal.
async fn write(
    stream: &mut TcpStream,
    write: Write<'_>,
    client: &Path,
) -> (u16, &'static str, String) {
    let body = match timeout(REQUEST_TIMEOUT, read_body(stream, write.length)).await {
        Ok(Ok(body)) => body,
        // A body the client never finished sending, or one that never
        // arrived: nothing has been read and nothing has been done.
        Ok(Err(_)) | Err(_) => return bad("the request body was not read in full"),
    };
    match write.route {
        Route::Assign => assign(&body, &write, client).await,
        Route::Handover => handover(&body, &write, client).await,
        Route::Release => release(&body, &write, client).await,
        Route::Message => message(&body, &write, client).await,
    }
}

/// Answer a read route: the status snapshot, the browser assets, what
/// `ssf agents` lists, or one harness's model ids.
async fn read(relative: &str, latest: &mut Latest, client: &Path) -> (u16, &'static str, String) {
    match relative {
        "api/status" => snapshot(latest).await,
        "" | "index.html" => (200, "text/html; charset=utf-8", INDEX.to_owned()),
        "dashboard.css" => (200, "text/css; charset=utf-8", CSS.to_owned()),
        "dashboard.js" => (200, "text/javascript; charset=utf-8", JS.to_owned()),
        "api/agents" => agents(client).await,
        _ => match relative.strip_prefix("api/models/") {
            Some(harness) => models(harness, client).await,
            None => not_found(),
        },
    }
}

/// The current snapshot as `/api/status` has always answered it: presented
/// the same way the event stream presents it, or the honest error.
async fn snapshot(latest: &mut Latest) -> (u16, &'static str, String) {
    let snapshot = match timeout(Duration::from_secs(30), load(latest)).await {
        Ok(result) => result.and_then(|value| presentation(&value)),
        Err(_) => Err(anyhow::anyhow!("SSF status timed out after 30 seconds")),
    };
    match snapshot {
        Ok(value) => (200, "application/json", value.to_string()),
        Err(error) => failure_of(&error),
    }
}

/// What `ssf agents` lists, as `ssf agents --json` prints it. The command is
/// the factory's own: it walks `PATH`, asks `mise` what it has installed and
/// reads the machine's default agent, so it has to run where the sessions
/// run rather than in this process.
async fn agents(client: &Path) -> (u16, &'static str, String) {
    match ask(client, &["agents", "--json"], LISTING_TIMEOUT).await {
        Ok(output) => listed(output, "the agent list"),
        Err(error) => failure_of(&error),
    }
}

/// One harness's model ids, as `ssf models <harness> --json` prints them. A
/// harness ssf does not know is `404`, and one that takes no model setting is
/// `400` (nothing is wrong with a request that asks what a harness offers;
/// there is simply nothing to list). Both are facts of ssf's own tables
/// rather than of any one machine, so they are settled here; only the ids
/// themselves come from the factory.
async fn models(harness: &str, client: &Path) -> (u16, &'static str, String) {
    if !crate::agents::is_known(harness) {
        return not_found();
    }
    if !crate::models::supports_model(harness) {
        return bad(crate::models::no_model_setting(harness));
    }
    match ask(client, &["models", harness, "--json"], LISTING_TIMEOUT).await {
        Ok(output) => listed(output, "the model list"),
        Err(error) => failure_of(&error),
    }
}

/// `POST api/assign`: `ssf assign`'s own code path for the item, answered
/// with its `--json` result or with the refusal that stopped it, verbatim.
async fn assign(body: &[u8], write: &Write<'_>, client: &Path) -> (u16, &'static str, String) {
    let request: AssignRequest = match parse(body, "assign") {
        Ok(request) => request,
        Err(answer) => return answer,
    };
    if request.number == 0 {
        return bad("number must be the item's number, 1 or more");
    }
    // What `ssf assign owner/repo#N --harness …` sends the daemon, with no
    // `--as`: a person is asking, not a session.
    let item = format!("{}#{}", request.repo, request.number);
    tracing::info!(
        origin = write.origin,
        item,
        harness = request.harness,
        model = request.model.as_deref().unwrap_or(""),
        effort = request.effort.as_deref().unwrap_or(""),
        "web API assign"
    );
    carry(
        client,
        crate::ipc::Request::Assign {
            item,
            harness: request.harness,
            model: request.model,
            effort: request.effort,
            by: None,
        },
        "the assign request",
    )
    .await
}

/// `POST api/handover`: `ssf handover`'s own code path for the item's session,
/// answered with its `--json` result. The optional `note` is the handover's
/// summary — what the new session is told before the item's story — and is
/// checked by the daemon exactly as `ssf handover --summary` is.
async fn handover(body: &[u8], write: &Write<'_>, client: &Path) -> (u16, &'static str, String) {
    let request: HandoverRequest = match parse(body, "handover") {
        Ok(request) => request,
        Err(answer) => return answer,
    };
    if request.number == 0 {
        return bad("number must be the item's number, 1 or more");
    }
    let item = format!("{}#{}", request.repo, request.number);
    tracing::info!(
        origin = write.origin,
        item,
        harness = request.harness,
        model = request.model.as_deref().unwrap_or(""),
        effort = request.effort.as_deref().unwrap_or(""),
        note_chars = request
            .note
            .as_deref()
            .map(|n| n.chars().count())
            .unwrap_or(0),
        "web API handover"
    );
    carry(
        client,
        crate::ipc::Request::Handover {
            session: item,
            harness: request.harness,
            model: request.model,
            effort: request.effort,
            summary: request.note,
            by: None,
        },
        "the handover request",
    )
    .await
}

/// `POST api/release`: `ssf release` for the item, never forced — the
/// workspace checks are the whole point of doing it from a browser, and a
/// person who has looked at the workspace passes `--force` at a shell.
///
/// The daemon answers a refused release as a *result* rather than an error
/// (`{"released": false, "check": …}`), because `ssf release` prints the
/// checks itself; here that becomes the same refusal the command prints.
async fn release(body: &[u8], write: &Write<'_>, client: &Path) -> (u16, &'static str, String) {
    let request: ReleaseRequest = match parse(body, "release") {
        Ok(request) => request,
        Err(answer) => return answer,
    };
    if request.number == 0 {
        return bad("number must be the item's number, 1 or more");
    }
    let item = format!("{}#{}", request.repo, request.number);
    tracing::info!(origin = write.origin, item, "web API release");
    let (status, kind, body) = carry(
        client,
        crate::ipc::Request::Release {
            session: item,
            force: false,
        },
        "the release request",
    )
    .await;
    if status != 200 {
        return (status, kind, body);
    }
    let answer: Value = match serde_json::from_str(&body) {
        Ok(answer) => answer,
        Err(error) => {
            return failure(
                None,
                format!("the release answer could not be read: {error}"),
            );
        }
    };
    if answer.get("released").and_then(Value::as_bool) == Some(true) {
        return (status, kind, body);
    }
    let session = answer.get("session").and_then(Value::as_str).unwrap_or("?");
    let path = answer.get("path").and_then(Value::as_str).unwrap_or("");
    let problems: Vec<&str> = answer
        .pointer("/check/problems")
        .and_then(Value::as_array)
        .map(|problems| problems.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    failure(
        Some(crate::ipc::RefusalKind::Conflict),
        crate::cli::release_refused_text(session, path, &problems),
    )
}

/// `POST api/message`: `text` reaches the agent that acts on the item the way
/// the item's own activity does, through the daemon's delivery path — which
/// brings a gone workspace and agent back first, and holds a prompt for a
/// session that is at its sign-in prompt instead of losing it.
async fn message(body: &[u8], write: &Write<'_>, client: &Path) -> (u16, &'static str, String) {
    let request: MessageRequest = match parse(body, "message") {
        Ok(request) => request,
        Err(answer) => return answer,
    };
    if request.number == 0 {
        return bad("number must be the item's number, 1 or more");
    }
    if request.text.trim().is_empty() {
        return bad("the message is empty: write something to send");
    }
    let bytes = request.text.len();
    if bytes > MAX_MESSAGE_BYTES {
        return bad(format!(
            "the message is {bytes} bytes; the most a message carries is {MAX_MESSAGE_BYTES}"
        ));
    }
    let item = format!("{}#{}", request.repo, request.number);
    tracing::info!(origin = write.origin, item, bytes, "web API message");
    carry(
        client,
        crate::ipc::Request::Message {
            item,
            text: request.text,
        },
        "the message request",
    )
    .await
}

/// One write's body as its own request, or the `400` that names what about it
/// could not be read. An unknown field is refused rather than ignored, so a
/// misspelled one cannot silently ask for something nobody meant.
fn parse<T: serde::de::DeserializeOwned>(
    body: &[u8],
    route: &str,
) -> std::result::Result<T, (u16, &'static str, String)> {
    serde_json::from_slice(body).map_err(|error| bad(format!("bad {route} request: {error}")))
}

/// One request taken to this factory's daemon, answered with what it said: its
/// result as the `200` body, or its refusal in its own words. The request is
/// exactly the one the matching `ssf` command sends, so the daemon's code path
/// and the command's are the same one.
async fn carry(
    client: &Path,
    request: crate::ipc::Request,
    what: &str,
) -> (u16, &'static str, String) {
    let Ok(line) = serde_json::to_string(&request) else {
        return failure(None, "the request could not be written down");
    };
    let output = match ask(client, &["__request", &line], WRITE_TIMEOUT).await {
        Ok(output) => output,
        Err(error) => return failure_of(&error),
    };
    // The client prints the daemon's answer and exits with whether it
    // agreed, so the answer itself is what decides the response here.
    match serde_json::from_slice::<crate::ipc::Response>(&output.stdout) {
        Ok(response) => answer(response),
        Err(_) => failure(None, client_error(&output, what)),
    }
}

/// Ask this factory one client command and take its output. The transport is
/// the status stream's own: a client run in this process's service identity,
/// which forwards into the guest when the factory is in a VM. Without it a
/// listener bound by a VM's host — the topology `ssf setup` creates — would
/// answer from the host, where neither the daemon nor the harnesses are.
async fn ask(client: &Path, args: &[&str], limit: Duration) -> Result<std::process::Output> {
    let mut command = crate::dashboard_transport::local_client_command(
        client,
        args,
        crate::server_catalog::service_local_context(),
        crate::server_catalog::selected_vm_context()?.as_ref(),
        crate::server_catalog::selected_target_identity()?.as_ref(),
    );
    match timeout(limit, command.output()).await {
        // A write the factory never answered is not a write that did not
        // happen: the daemon may still be working on it, and the message
        // says that rather than the tidy lie.
        Err(_) => bail!("the factory did not answer in time; what it was doing may still land"),
        Ok(output) => output.context("running the ssf client"),
    }
}

fn not_found() -> (u16, &'static str, String) {
    (
        404,
        "application/json",
        json!({"error":"not found"}).to_string(),
    )
}

/// A client command's output as the `200` body it printed, or the factory's
/// failure with its own words.
fn listed(output: std::process::Output, what: &str) -> (u16, &'static str, String) {
    if output.status.success() {
        return (
            200,
            "application/json",
            String::from_utf8_lossy(&output.stdout).into_owned(),
        );
    }
    failure(None, client_error(&output, what))
}

/// Why a client command stopped, as it printed it: a command tells a person
/// on stderr, and that is the message this endpoint reports.
fn client_error(output: &std::process::Output, what: &str) -> String {
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if detail.is_empty() {
        format!("{what} could not be read")
    } else {
        detail
    }
}

/// The daemon's answer to a write, as HTTP: its result is the `200` the
/// command prints, a refusal carries the daemon's own words in `error` with
/// the status that goes with it, and a failure the request cannot be blamed
/// for is `502`.
fn answer(response: crate::ipc::Response) -> (u16, &'static str, String) {
    if response.ok {
        return (200, "application/json", response.data.to_string());
    }
    failure(
        response.kind,
        response.error.unwrap_or_else(|| "request failed".into()),
    )
}

/// A `400` with the reason: the request itself is what ssf refused.
fn bad(message: impl Into<String>) -> (u16, &'static str, String) {
    failure(Some(crate::ipc::RefusalKind::BadInput), message)
}

/// [`failure`] for an error: a [`Refused`] anywhere in its chain decides the
/// status, and everything else is the factory failing to serve the request.
fn failure_of(error: &anyhow::Error) -> (u16, &'static str, String) {
    failure(
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<Refused>())
            .map(|refused| refused.kind),
        format!("{error:#}"),
    )
}

/// What a refusal is worth in HTTP: the request's own fault (`400`) or the
/// item's state (`409`), and `502` when it is neither and the factory simply
/// failed. The message goes in `error` as the daemon wrote it.
fn failure(
    kind: Option<crate::ipc::RefusalKind>,
    message: impl Into<String>,
) -> (u16, &'static str, String) {
    let status = match kind {
        Some(crate::ipc::RefusalKind::BadInput) => 400,
        Some(crate::ipc::RefusalKind::Conflict) => 409,
        None => 502,
    };
    (
        status,
        "application/json",
        json!({"error": message.into()}).to_string(),
    )
}

/// Read the body of a write, at the length its headers declared and within
/// the bound [`classify`] already applied.
async fn read_body(stream: &mut TcpStream, length: usize) -> Result<Vec<u8>> {
    let mut body = vec![0u8; length];
    stream
        .read_exact(&mut body[..])
        .await
        .context("reading the request body")?;
    Ok(body)
}

/// What an event stream wakes on: a newer snapshot, a keepalive that is due,
/// or the status stream going away.
enum Tick {
    Changed,
    Idle,
    Closed,
}

async fn next_tick(latest: &mut Latest, keepalive: Duration) -> Tick {
    match timeout(keepalive, latest.changed()).await {
        Ok(Ok(())) => Tick::Changed,
        Ok(Err(_)) => Tick::Closed,
        Err(_) => Tick::Idle,
    }
}

/// Writes one chunk of an event stream, bounded the way the ordinary response
/// is: a client that stops reading must not pin this task, so the stream ends
/// instead of blocking on a socket nobody drains. Reports whether to go on.
async fn write_frame(stream: &mut TcpStream, frame: &[u8], write_timeout: Duration) -> bool {
    matches!(
        timeout(write_timeout, stream.write_all(frame)).await,
        Ok(Ok(()))
    )
}

/// Server-sent events: the current snapshot, then every later one, with a
/// comment line while no snapshot arrives so idle connections stay open.
async fn events(
    stream: &mut TcpStream,
    latest: &mut Latest,
    keepalive: Duration,
    write_timeout: Duration,
) {
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n{SECURITY_HEADERS}\r\n"
    );
    if !write_frame(stream, headers.as_bytes(), write_timeout).await {
        return;
    }
    let mut pending = true;
    loop {
        // Only the first snapshot and then each change is a frame: the channel
        // keeps its value, so an idle tick must not re-send the last one.
        if pending {
            pending = false;
            // The guard is dropped before the write: it is not `Send`.
            let snapshot = latest.borrow_and_update().clone();
            if let Some(snapshot) = snapshot {
                let frame = match snapshot {
                    Ok(value) => match presentation(&value) {
                        Ok(value) => status_frame(&value),
                        Err(error) => error_frame(&format!("{error:#}")),
                    },
                    Err(error) => error_frame(&error),
                };
                if !write_frame(stream, frame.as_bytes(), write_timeout).await {
                    return;
                }
            }
        }
        match next_tick(latest, keepalive).await {
            Tick::Changed => pending = true,
            Tick::Idle => {
                if !write_frame(stream, b": keepalive\n\n", write_timeout).await {
                    return;
                }
            }
            // The daemon stopping ends its event streams instead of leaving
            // them re-sending the snapshot it can no longer refresh.
            Tick::Closed => return,
        }
    }
}

fn status_frame(value: &Value) -> String {
    format!("event: status\ndata: {value}\n\n")
}

fn error_frame(error: &str) -> String {
    format!("event: error\ndata: {}\n\n", json!({"error": error}))
}

async fn read_request(stream: &mut TcpStream) -> Result<String> {
    let mut bytes = Vec::new();
    while bytes.len() < MAX_HEADERS {
        let byte = stream.read_u8().await?;
        bytes.push(byte);
        if bytes.ends_with(b"\r\n\r\n") {
            return String::from_utf8(bytes).context("invalid HTTP headers");
        }
    }
    bail!("HTTP headers too large")
}

/// What a request is, once its method, host, origin, capability and (for a
/// write) media type and body bound have been checked.
enum Routed<'a> {
    /// A read: the path under the capability.
    Read(&'a str),
    /// One of the writes.
    Write(Write<'a>),
}

/// An accepted write: which of them, and the request's own facts.
struct Write<'a> {
    /// The extension origin, which every accepted write is logged with.
    origin: &'a str,
    /// What `ssf` request this route makes.
    route: Route,
    /// Declared body length, within [`MAX_BODY`].
    length: usize,
}

/// The writes the endpoint accepts, each standing for one `ssf` request on
/// the daemon. Nothing else is a write: a route that is not one of these is
/// `405`, however it is posted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Route {
    Assign,
    Handover,
    Release,
    Message,
}

/// The body of an assign write: `ssf assign`'s own arguments, with the item
/// named as the repository and the number. An unknown field is refused
/// rather than ignored, so a misspelled one cannot silently assign a stack
/// nobody asked for.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssignRequest {
    /// Watched repository, `owner/name`.
    repo: String,
    /// Issue or pull request number.
    number: u64,
    /// Harness the session runs (`ssf agents` lists the ids).
    harness: String,
    /// Model for the session; the harness's own default when absent.
    #[serde(default)]
    model: Option<String>,
    /// Effort level for the session; the harness's own default when absent.
    #[serde(default)]
    effort: Option<String>,
}

/// The body of a handover write: `ssf handover`'s own arguments for an item
/// that has a session, with `note` as the summary the new session reads
/// before the item's story.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandoverRequest {
    /// Watched repository, `owner/name`.
    repo: String,
    /// Issue or pull request number.
    number: u64,
    /// Harness the new session runs (`ssf agents` lists the ids).
    harness: String,
    /// Model for the new session; the harness's own default when absent.
    #[serde(default)]
    model: Option<String>,
    /// Effort level for the new session; the harness's own default when absent.
    #[serde(default)]
    effort: Option<String>,
    /// What the new session is told before the item's story.
    #[serde(default)]
    note: Option<String>,
}

/// The body of a release write: the item whose session's workspace goes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseRequest {
    /// Watched repository, `owner/name`.
    repo: String,
    /// Issue or pull request number.
    number: u64,
}

/// The body of a message write: what the item's agent is told.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageRequest {
    /// Watched repository, `owner/name`.
    repo: String,
    /// Issue or pull request number.
    number: u64,
    /// The message, delivered the way the item's own activity is.
    text: String,
}

/// Check one request and say what it is. A read answers on the rules it
/// always has. A write is held to the stricter ones: only a Chrome
/// extension's own origin, so a page the tailnet can load cannot post, and
/// never the bind host's; a JSON media type; and a body bounded before a
/// byte of it is read. Every answer here is a status, so a refusal has done
/// nothing.
fn classify<'a>(request: &'a str, host: &str, token: &str) -> std::result::Result<Routed<'a>, u16> {
    let mut lines = request.split("\r\n");
    let parts: Vec<_> = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    if parts.len() != 3 || !matches!(parts[2], "HTTP/1.0" | "HTTP/1.1") {
        return Err(400);
    }
    let write = match parts[0] {
        "GET" => false,
        "POST" => true,
        _ => return Err(405),
    };
    let mut hosts = Vec::new();
    let mut origins = Vec::new();
    let mut content_type = None;
    let mut length = None;
    for line in lines.take_while(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or(400u16)?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("host") {
            hosts.push(value);
        } else if name.eq_ignore_ascii_case("origin") {
            origins.push(value);
        } else if name.eq_ignore_ascii_case("content-type") {
            content_type = Some(value);
        } else if name.eq_ignore_ascii_case("content-length") {
            // A body with two lengths has two readings; refuse it rather
            // than pick one.
            if length.is_some() {
                return Err(400);
            }
            length = Some(value.parse::<usize>().map_err(|_| 400u16)?);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            // A body is read at the length its headers declare, or not at
            // all: a chunked one would need its own framing.
            return Err(400);
        }
    }
    if hosts != [host] || origins.len() > 1 {
        return Err(403);
    }
    let origin = origins.first().copied();
    let relative = under_capability(token, parts[1])?;
    if !write {
        // A read carries no body.
        if length.is_some() {
            return Err(400);
        }
        if origin.is_some_and(|origin| {
            // A Chrome MV3 extension's service worker sends its own origin.
            origin != format!("http://{host}") && !origin.starts_with("chrome-extension://")
        }) {
            return Err(403);
        }
        return Ok(Routed::Read(relative));
    }
    let Some(origin) = origin.filter(|origin| origin.starts_with("chrome-extension://")) else {
        return Err(403);
    };
    let route = match relative {
        "api/assign" => Route::Assign,
        "api/handover" => Route::Handover,
        "api/release" => Route::Release,
        "api/message" => Route::Message,
        // A write is only for the write routes, however the request is
        // shaped; every other path under the capability is a read.
        _ => return Err(405),
    };
    if !content_type.is_some_and(is_json) {
        return Err(415);
    }
    match length {
        Some(length) if length > MAX_BODY => Err(413),
        Some(length) => Ok(Routed::Write(Write {
            origin,
            route,
            length,
        })),
        None => Err(400),
    }
}

/// Whether a `Content-Type` names JSON: its media type, whatever parameters
/// follow it.
fn is_json(value: &str) -> bool {
    value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("application/json")
}

/// The path under the capability, or `404` when the request did not carry
/// this server's: every capability byte is compared, without exposing
/// matching prefixes.
fn under_capability<'a>(token: &str, target: &'a str) -> std::result::Result<&'a str, u16> {
    let path = target.strip_prefix('/').ok_or(404u16)?;
    let (provided, relative) = path.split_once('/').ok_or(404u16)?;
    let mismatch = provided.len() ^ token.len();
    let mismatch = provided
        .bytes()
        .zip(token.bytes())
        .fold(mismatch, |acc, (a, b)| acc | usize::from(a ^ b));
    if mismatch != 0 {
        return Err(404);
    }
    Ok(relative)
}

const SECURITY_HEADERS: &str = "X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'\r\n";

async fn respond(stream: &mut TcpStream, status: u16, kind: &str, body: &str) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        _ => "Bad Gateway",
    };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n{SECURITY_HEADERS}\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

fn presentation(payload: &Value) -> Result<Value> {
    let dashboard = &payload["dashboard"];
    if !dashboard["cards"].is_array() {
        bail!("SSF returned status without canonical dashboard data; update ssf-server");
    }
    Ok(dashboard.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    async fn assert_listener_closed(address: std::net::SocketAddr) {
        timeout(Duration::from_secs(1), async {
            while TcpStream::connect(address).await.is_ok() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dashboard listener remained open after the daemon stopped");
    }

    fn request(path: &str, host: &str, extra: &str) -> String {
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{extra}\r\n")
    }

    /// A `POST` as the extension's service worker sends it: the capability
    /// path, an extension origin, a JSON media type and a body.
    fn post(path: &str, host: &str, extra: &str, body: &str) -> String {
        format!(
            "POST {path} HTTP/1.1\r\nHost: {host}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    fn read_route(routed: std::result::Result<Routed<'_>, u16>) -> std::result::Result<&str, u16> {
        match routed {
            Ok(Routed::Read(relative)) => Ok(relative),
            Ok(Routed::Write(_)) => panic!("expected a read"),
            Err(status) => Err(status),
        }
    }

    /// The assign body the extension sends, and the headers that carry it.
    fn assign_body() -> String {
        json!({"repo":"o/r","number":7,"harness":"claude","model":"opus","effort":"low"})
            .to_string()
    }

    const EXTENSION_ORIGIN: &str =
        "Origin: chrome-extension://abcdefghijklmnopabcdefghijklmnop\r\n";

    #[test]
    fn checks_capability_host_origin_and_method() {
        let good = request("/secret/api/status", "127.0.0.1:123", "");
        assert_eq!(
            read_route(classify(&good, "127.0.0.1:123", "secret")),
            Ok("api/status")
        );
        for extra in ["Origin: http://127.0.0.1:123\r\n", EXTENSION_ORIGIN] {
            let request = request("/secret/api/events", "127.0.0.1:123", extra);
            assert_eq!(
                read_route(classify(&request, "127.0.0.1:123", "secret")),
                Ok("api/events")
            );
        }
        for (input, expected) in [
            (request("/wrong/api/status", "127.0.0.1:123", ""), 404),
            (request("/secret/api/status", "attacker.example", ""), 403),
            (
                request(
                    "/secret/api/status",
                    "127.0.0.1:123",
                    "Origin: https://attacker.example\r\n",
                ),
                403,
            ),
            (
                request(
                    "/secret/api/status",
                    "127.0.0.1:123",
                    "Host: 127.0.0.1:123\r\n",
                ),
                403,
            ),
            (
                request(
                    "/secret/api/status",
                    "127.0.0.1:123",
                    "Origin: https://chrome-extension://abc\r\n",
                ),
                403,
            ),
            (good.replacen("GET", "POST", 1), 403),
            (
                request(
                    "/secret/api/status",
                    "127.0.0.1:123",
                    "Content-Length: 10\r\n",
                ),
                400,
            ),
        ] {
            assert_eq!(
                read_route(classify(&input, "127.0.0.1:123", "secret")),
                Err(expected)
            );
        }
        assert_eq!(capability().unwrap().len(), 64);
        assert_ne!(capability().unwrap(), capability().unwrap());
    }

    /// The write rules, one rejection at a time: only an extension's own
    /// origin, only JSON, and only a body within the bound. Each is decided
    /// from the request line and the headers, so none of them reads a byte
    /// of the body — and none of them can have done anything.
    #[test]
    fn accepts_only_an_extension_write_within_the_body_bound() {
        let body = assign_body();
        let headers = format!("{EXTENSION_ORIGIN}Content-Type: application/json\r\n");
        let accepted = post("/secret/api/assign", "127.0.0.1:123", &headers, &body);
        assert!(
            matches!(
                classify(&accepted, "127.0.0.1:123", "secret"),
                Ok(Routed::Write(write))
                    if write.origin == "chrome-extension://abcdefghijklmnopabcdefghijklmnop"
                        && write.length == body.len()
            ),
            "an extension's own POST is the write"
        );
        // A media type may carry parameters.
        let with_charset = post(
            "/secret/api/assign",
            "127.0.0.1:123",
            &format!("{EXTENSION_ORIGIN}Content-Type: application/json; charset=utf-8\r\n"),
            &body,
        );
        assert!(matches!(
            classify(&with_charset, "127.0.0.1:123", "secret"),
            Ok(Routed::Write(_))
        ));
        // Every write route is one, and each is the request it stands for.
        for (path, route) in [
            ("/secret/api/assign", Route::Assign),
            ("/secret/api/handover", Route::Handover),
            ("/secret/api/release", Route::Release),
            ("/secret/api/message", Route::Message),
        ] {
            assert!(
                matches!(
                    classify(&post(path, "127.0.0.1:123", &headers, &body), "127.0.0.1:123", "secret"),
                    Ok(Routed::Write(write)) if write.route == route
                ),
                "{path} is {route:?}"
            );
        }
        for (input, expected) in [
            // The bind host's own origin is a page the tailnet can load.
            (
                post(
                    "/secret/api/assign",
                    "127.0.0.1:123",
                    "Origin: http://127.0.0.1:123\r\nContent-Type: application/json\r\n",
                    &body,
                ),
                403,
            ),
            // No origin at all: an ordinary client, not the extension.
            (
                post(
                    "/secret/api/assign",
                    "127.0.0.1:123",
                    "Content-Type: application/json\r\n",
                    &body,
                ),
                403,
            ),
            (
                post(
                    "/secret/api/assign",
                    "127.0.0.1:123",
                    "Origin: https://github.com\r\nContent-Type: application/json\r\n",
                    &body,
                ),
                403,
            ),
            (
                post(
                    "/secret/api/assign",
                    "127.0.0.1:123",
                    &format!("{EXTENSION_ORIGIN}Content-Type: text/plain\r\n"),
                    &body,
                ),
                415,
            ),
            (
                post(
                    "/secret/api/assign",
                    "127.0.0.1:123",
                    EXTENSION_ORIGIN,
                    &body,
                ),
                415,
            ),
            (
                format!(
                    "POST /secret/api/assign HTTP/1.1\r\nHost: 127.0.0.1:123\r\n{EXTENSION_ORIGIN}\
Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    MAX_BODY + 1
                ),
                413,
            ),
            // A body longer than the bound is refused for its length alone;
            // two lengths at all have two readings and are refused outright.
            (
                post(
                    "/secret/api/assign",
                    "127.0.0.1:123",
                    &format!(
                        "{EXTENSION_ORIGIN}Content-Type: application/json\r\nContent-Length: 1\r\n"
                    ),
                    &body,
                ),
                400,
            ),
            // A write is only for the write routes, and only by POST: a read
            // path is not one however it is posted, and neither is a path
            // that is not there at all.
            (
                post("/secret/api/status", "127.0.0.1:123", &headers, &body),
                405,
            ),
            (
                post("/secret/api/events", "127.0.0.1:123", &headers, &body),
                405,
            ),
            (
                post("/secret/api/agents", "127.0.0.1:123", &headers, &body),
                405,
            ),
            (
                post("/secret/api/nothing", "127.0.0.1:123", &headers, &body),
                405,
            ),
            (
                post(
                    "/secret/api/assign",
                    "127.0.0.1:123",
                    &format!(
                        "{EXTENSION_ORIGIN}Content-Type: application/json\r\nTransfer-Encoding: chunked\r\n"
                    ),
                    &body,
                ),
                400,
            ),
        ] {
            assert_eq!(
                classify(&input, "127.0.0.1:123", "secret").err(),
                Some(expected),
                "{input}"
            );
        }
        assert!(is_json("APPLICATION/JSON"));
        assert!(!is_json("application/jsonx"));
    }

    #[test]
    fn serves_canonical_model_without_reinterpreting_ownership() {
        let dashboard = json!({"cards":[{"owner":"r#1"}],"warning":"VM stopped","refreshed_at":42});
        assert_eq!(
            presentation(&json!({"dashboard":dashboard})).unwrap(),
            dashboard
        );
        assert!(presentation(&json!({"sessions":[]})).is_err());
        assert!(!JS.contains("innerHTML"));
    }

    async fn fetch(address: std::net::SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(request(path, &address.to_string(), "").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    /// One write over a real socket, as the extension's service worker sends
    /// it.
    async fn write(address: std::net::SocketAddr, path: &str, extra: &str, body: &str) -> String {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(post(path, &address.to_string(), extra, body).as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    /// One write as the extension's service worker sends it: the capability
    /// path, an extension origin, a JSON media type and a body.
    async fn post_json(address: std::net::SocketAddr, path: &str, body: &str) -> String {
        write(
            address,
            path,
            &format!("{EXTENSION_ORIGIN}Content-Type: application/json\r\n"),
            body,
        )
        .await
    }

    /// A stand-in for the `ssf` client the endpoint runs commands with: the
    /// endpoint asks the factory through a client process — the status
    /// stream's own transport, which forwards into a VM — so this is what a
    /// factory looks like to it. The script records the arguments it was
    /// given and prints one canned answer.
    struct Client {
        root: PathBuf,
    }

    impl Client {
        fn new(name: &str, answer: &str, exit: u8) -> Self {
            let root = std::env::temp_dir().join(format!(
                "ssf-web-client-{name}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            let script = format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{args}'\nprintf '%s' '{answer}'\nexit {exit}\n",
                args = root.join("args").display(),
                // The canned answer is a shell single-quoted string: no
                // fixture contains a quote of its own.
                answer = answer.replace('\'', "'\\''"),
                exit = exit,
            );
            let program = root.join("ssf");
            std::fs::write(&program, script).unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self { root }
        }

        fn program(&self) -> PathBuf {
            self.root.join("ssf")
        }

        /// What the client was last run as, or nothing when it has not been
        /// run at all: one argument per line, as it wrote them.
        fn args(&self) -> Vec<String> {
            std::fs::read_to_string(self.root.join("args"))
                .map(|text| text.lines().map(str::to_string).collect())
                .unwrap_or_default()
        }
    }

    impl Drop for Client {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// A listener serving one factory's client.
    async fn served(
        client: &Client,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<Result<()>>) {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::watch::channel(Some(Ok(json!({"dashboard":{"cards":[]}}))));
        std::mem::forget(tx);
        (
            address,
            tokio::spawn(serve(listener, "secret".into(), rx, client.program())),
        )
    }

    /// The pickers the extension's assign form is built from: what `ssf
    /// agents` lists, and what `ssf models <harness>` lists — asked of the
    /// factory as those commands are. A harness ssf does not know is `404`;
    /// one that takes no model setting has nothing to list and says so with
    /// `400` without asking anyone.
    #[tokio::test]
    async fn serves_the_agent_and_model_listings_the_cli_prints() {
        let agents = json!([{"id":"claude","name":"Claude Code","installed":true}]);
        let models =
            json!({"harness":"claude","models":["opus"],"source":{"kind":"table","detail":""}});
        let client = Client::new("listings", &agents.to_string(), 0);
        let (address, task) = served(&client).await;
        let response = fetch(address, "/secret/api/agents").await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(
            response.split("\r\n\r\n").nth(1).unwrap(),
            agents.to_string(),
            "the listing is the command's own output"
        );
        assert_eq!(client.args(), ["__client", "agents", "--json"]);
        // The model ids come from the factory too: its catalogues and its
        // harnesses' listing commands are there, not here.
        let client = Client::new("models", &models.to_string(), 0);
        let (address, task2) = served(&client).await;
        let response = fetch(address, "/secret/api/models/claude").await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(
            response.split("\r\n\r\n").nth(1).unwrap(),
            models.to_string()
        );
        assert_eq!(client.args(), ["__client", "models", "claude", "--json"]);
        // A harness ssf does not know, and one that takes no model setting:
        // both are ssf's own tables, so neither asks the factory anything.
        for (path, expected) in [
            ("/secret/api/models/not-a-harness", "HTTP/1.1 404"),
            ("/secret/api/models/crush", "HTTP/1.1 400"),
        ] {
            let asked = client.args();
            let response = fetch(address, path).await;
            assert!(response.starts_with(expected), "{response}");
            assert_eq!(client.args(), asked, "the factory was asked for {path}");
        }
        assert!(
            fetch(address, "/secret/api/models/crush")
                .await
                .contains("does not take a model setting"),
            "the refusal says why"
        );
        // A factory that cannot answer is the factory's failure, reported
        // with its own words.
        let broken = Client::new("broken", "", 1);
        // stderr is where a command says why it stopped; the script's own
        // is empty, so the endpoint names what it could not read.
        let (address, task3) = served(&broken).await;
        let response = fetch(address, "/secret/api/agents").await;
        assert!(response.starts_with("HTTP/1.1 502"), "{response}");
        assert!(
            response.contains("the agent list could not be read"),
            "{response}"
        );
        task.abort();
        task2.abort();
        task3.abort();
    }

    /// The write itself: the factory is asked with exactly the request `ssf
    /// assign` sends its daemon, and the daemon's answer is the `200` body.
    #[tokio::test]
    async fn the_assign_write_runs_the_request_ssf_assign_sends() {
        let result = json!({"session":"o/r#7","title":"Fix it","assigned":true,
            "overrides_written":true,"open":true,"poll_interval_secs":10});
        let answer = json!({"ok":true,"data":result}).to_string();
        let client = Client::new("assign", &answer, 0);
        let (address, task) = served(&client).await;
        let response = write(
            address,
            "/secret/api/assign",
            &format!("{EXTENSION_ORIGIN}Content-Type: application/json\r\n"),
            &assign_body(),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let body: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body, result);
        // The client is run as `ssf __client __request '<json>'`, and that
        // JSON is the request `ssf assign` sends: the same code path on the
        // daemon, reached the way every command reaches a factory in a VM.
        let args = client.args();
        assert_eq!(args.len(), 3, "{args:?}");
        assert_eq!(&args[..2], ["__client", "__request"]);
        assert_eq!(
            serde_json::from_str::<crate::ipc::Request>(&args[2]).unwrap(),
            crate::ipc::Request::Assign {
                item: "o/r#7".into(),
                harness: "claude".into(),
                model: Some("opus".into()),
                effort: Some("low".into()),
                // A person at the overlay, not a session.
                by: None,
            }
        );
        task.abort();
    }

    /// Each of the four writes is the request its own `ssf` command sends the
    /// daemon — the same code path, reached the way every command reaches a
    /// factory in a VM — answered with the daemon's own result.
    #[tokio::test]
    async fn each_write_route_runs_the_request_its_command_sends() {
        let assign_result = json!({"session":"o/r#7","title":"Fix it","assigned":true,
            "overrides_written":true,"open":true,"poll_interval_secs":10});
        let handover_result = json!({"session":"o/r#7","title":"Fix it",
            "summary_chars":8,"poll_interval_secs":10});
        let release_result = json!({"session":"o/r#7","title":"Fix it","released":true,
            "pending":true,"poll_interval_secs":10});
        let message_result = json!({"session":"o/r#7","title":"Fix it","delivered":true});
        let handover_body = json!({"repo":"o/r","number":7,"harness":"omp",
            "model":"deepseek/deepseek-flash","effort":"high","note":"carry on"})
        .to_string();
        let release_body = json!({"repo":"o/r","number":7}).to_string();
        let message_body =
            json!({"repo":"o/r","number":7,"text":"the test is red again"}).to_string();
        for (name, path, body, result, expected) in [
            (
                "assign",
                "/secret/api/assign",
                assign_body(),
                assign_result,
                crate::ipc::Request::Assign {
                    item: "o/r#7".into(),
                    harness: "claude".into(),
                    model: Some("opus".into()),
                    effort: Some("low".into()),
                    by: None,
                },
            ),
            (
                "handover",
                "/secret/api/handover",
                handover_body,
                handover_result,
                crate::ipc::Request::Handover {
                    session: "o/r#7".into(),
                    harness: "omp".into(),
                    model: Some("deepseek/deepseek-flash".into()),
                    effort: Some("high".into()),
                    summary: Some("carry on".into()),
                    by: None,
                },
            ),
            (
                "release",
                "/secret/api/release",
                release_body,
                release_result,
                // Never forced: the workspace checks are the point of doing
                // this from a browser.
                crate::ipc::Request::Release {
                    session: "o/r#7".into(),
                    force: false,
                },
            ),
            (
                "message",
                "/secret/api/message",
                message_body,
                message_result,
                crate::ipc::Request::Message {
                    item: "o/r#7".into(),
                    text: "the test is red again".into(),
                },
            ),
        ] {
            let client = Client::new(name, &json!({"ok":true,"data":result}).to_string(), 0);
            let (address, task) = served(&client).await;
            let response = post_json(address, path, &body).await;
            assert!(response.starts_with("HTTP/1.1 200"), "{name}: {response}");
            assert_eq!(
                serde_json::from_str::<Value>(response.split("\r\n\r\n").nth(1).unwrap()).unwrap(),
                result,
                "{name}: the daemon's result is the body"
            );
            // The client is run as `ssf __client __request '<json>'`, and that
            // JSON is the request the matching `ssf` command sends.
            let args = client.args();
            assert_eq!(&args[..2], ["__client", "__request"], "{name}: {args:?}");
            assert_eq!(
                serde_json::from_str::<crate::ipc::Request>(&args[2]).unwrap(),
                expected,
                "{name}"
            );
            task.abort();
        }
    }

    /// A release the workspace's own state refuses is the refusal `ssf release`
    /// prints, in the same words: the daemon answers it as a *result* — it is
    /// the command that words it — and the endpoint is the command here.
    #[tokio::test]
    async fn a_release_the_workspace_refuses_is_the_commands_own_refusal() {
        let refused = json!({"released":false,"session":"o/r#7","title":"Fix it","path":"/w/7",
            "check":{"state":"dirty","safe":false,
            "problems":["2 files are not committed","the branch is not pushed"]}});
        let client = Client::new("dirty", &json!({"ok":true,"data":refused}).to_string(), 0);
        let (address, task) = served(&client).await;
        let body = json!({"repo":"o/r","number":7}).to_string();
        let response = post_json(address, "/secret/api/release", &body).await;
        assert!(response.starts_with("HTTP/1.1 409"), "{response}");
        let error =
            serde_json::from_str::<Value>(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let message = error["error"].as_str().unwrap();
        assert!(
            message.starts_with("not released: the workspace of o/r#7 (/w/7)"),
            "{message}"
        );
        for problem in ["2 files are not committed", "the branch is not pushed"] {
            assert!(message.contains(problem), "{message}");
        }
        assert!(message.contains("nothing was removed"), "{message}");
        task.abort();
    }

    /// A body a route cannot read is refused for it, with the factory never
    /// asked: the item's number, the message's own bounds, and a field the
    /// route does not take.
    #[tokio::test]
    async fn refuses_write_bodies_a_route_cannot_read() {
        let client = Client::new("unread", "", 0);
        std::fs::remove_file(client.program()).unwrap();
        let (address, task) = served(&client).await;
        let bad_json = "{ not json".to_string();
        let zero_stack = json!({"repo":"o/r","number":0,"harness":"claude"}).to_string();
        let zero_item = json!({"repo":"o/r","number":0}).to_string();
        let zero_message = json!({"repo":"o/r","number":0,"text":"hi"}).to_string();
        let no_number = json!({"repo":"o/r","harness":"claude"}).to_string();
        let extra_field =
            json!({"repo":"o/r","number":7,"harness":"claude","branch":"x"}).to_string();
        let empty_text = json!({"repo":"o/r","number":7,"text":"   "}).to_string();
        let long_text = json!({"repo":"o/r","number":7,
            "text":"x".repeat(MAX_MESSAGE_BYTES + 1)})
        .to_string();
        // The cap counts bytes, so text that is not ASCII is refused for the
        // cap rather than slipping through it: 700 characters (2,100 bytes) is
        // under any character count, and over this cap only because of the
        // bytes it takes to write.
        let long_multibyte = json!({"repo":"o/r","number":7,
            "text":"漢".repeat(700)})
        .to_string();
        assert!(long_multibyte.len() < MAX_BODY);
        for (path, body, needle) in [
            (
                "/secret/api/assign",
                bad_json.as_str(),
                "bad assign request",
            ),
            (
                "/secret/api/handover",
                bad_json.as_str(),
                "bad handover request",
            ),
            (
                "/secret/api/release",
                bad_json.as_str(),
                "bad release request",
            ),
            (
                "/secret/api/message",
                bad_json.as_str(),
                "bad message request",
            ),
            (
                "/secret/api/assign",
                zero_stack.as_str(),
                "number must be the item's number",
            ),
            (
                "/secret/api/handover",
                zero_stack.as_str(),
                "number must be the item's number",
            ),
            (
                "/secret/api/release",
                zero_item.as_str(),
                "number must be the item's number",
            ),
            (
                "/secret/api/message",
                zero_message.as_str(),
                "number must be the item's number",
            ),
            (
                "/secret/api/message",
                no_number.as_str(),
                "bad message request",
            ),
            (
                "/secret/api/handover",
                extra_field.as_str(),
                "unknown field",
            ),
            (
                "/secret/api/message",
                empty_text.as_str(),
                "the message is empty",
            ),
            (
                "/secret/api/message",
                long_text.as_str(),
                "the most a message carries is 2048",
            ),
            (
                "/secret/api/message",
                long_multibyte.as_str(),
                "the message is 2100 bytes; the most a message carries is 2048",
            ),
        ] {
            let response = post_json(address, path, body).await;
            assert!(
                response.starts_with("HTTP/1.1 400"),
                "{path} {body}: {response}"
            );
            assert!(response.contains(needle), "{path} {body}: {response}");
        }
        assert!(client.args().is_empty(), "a refused write ran the client");
        // The write routes are not reads: a GET of one is a `404` from the
        // read side, which is what keeps a handover out of a link.
        for path in [
            "/secret/api/assign",
            "/secret/api/handover",
            "/secret/api/release",
            "/secret/api/message",
        ] {
            let response = fetch(address, path).await;
            assert!(response.starts_with("HTTP/1.1 404"), "{path}: {response}");
        }
        assert!(client.args().is_empty(), "a refused write ran the client");
        task.abort();
    }

    /// What the daemon refuses comes back as the command prints it, with the
    /// status the extension needs to tell the two apart.
    #[tokio::test]
    async fn a_refusal_is_answered_with_the_daemons_own_words() {
        for (answer, expected) in [
            (
                json!({"ok":false,"error":"o/r#7 already has a session",
                       "kind":"conflict"}),
                "HTTP/1.1 409 Conflict",
            ),
            (
                // A factory failure carries no kind: it was not the request
                // that stopped it.
                json!({"ok":false,"error":"claude is not installed where the daemon runs"}),
                "HTTP/1.1 502 Bad Gateway",
            ),
            (
                json!({"ok":false,"error":"o/r is not a watched repository",
                       "kind":"bad_input"}),
                "HTTP/1.1 400 Bad Request",
            ),
        ] {
            let message = answer["error"].as_str().unwrap();
            let client = Client::new("refusal", &answer.to_string(), 1);
            let (address, task) = served(&client).await;
            let response = write(
                address,
                "/secret/api/assign",
                &format!("{EXTENSION_ORIGIN}Content-Type: application/json\r\n"),
                &assign_body(),
            )
            .await;
            assert!(response.starts_with(expected), "{response}");
            // The daemon's message verbatim, as `{"error": …}`.
            assert_eq!(
                serde_json::from_str::<Value>(response.split("\r\n\r\n").nth(1).unwrap()).unwrap(),
                json!({"error": message}),
                "{response}"
            );
            task.abort();
        }
    }

    /// Every write rule, over a real socket: each refused for what the
    /// request carried, with the client never run at all — which is what
    /// says nothing was done.
    #[tokio::test]
    async fn refuses_writes_that_break_a_rule_before_asking_the_factory() {
        // The program is one no shell could run: if a rule let a request
        // through, the answer would be the `502` of failing to start it.
        let client = Client::new("unreached", "", 0);
        std::fs::remove_file(client.program()).unwrap();
        let (address, task) = served(&client).await;
        let body = assign_body();
        let own = format!("Origin: http://{address}\r\nContent-Type: application/json\r\n");
        for (path, extra, expected) in [
            ("/secret/api/assign", own.as_str(), "HTTP/1.1 403"),
            (
                "/secret/api/assign",
                "Content-Type: application/json\r\n",
                "HTTP/1.1 403",
            ),
            (
                "/secret/api/assign",
                &format!("{EXTENSION_ORIGIN}Content-Type: text/plain\r\n"),
                "HTTP/1.1 415",
            ),
        ] {
            let response = write(address, path, extra, &body).await;
            assert!(response.starts_with(expected), "{response}");
        }
        assert!(client.args().is_empty(), "a refused write ran the client");
        // A body over the bound is refused for its length alone, before any of
        // it is read: the connection is closed with the answer rather than
        // waiting for 4 KiB that will not come.
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(
                format!(
                    "POST /secret/api/assign HTTP/1.1\r\nHost: {address}\r\n{EXTENSION_ORIGIN}\
Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    MAX_BODY + 1
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 413"), "{response}");
        // The one rejection a person can reach from the overlay's message box
        // names the bound rather than saying only that the request was refused.
        assert!(
            response.contains(&format!(
                "longer than the {MAX_BODY} bytes this endpoint reads"
            )),
            "{response}"
        );
        // A bad body is the request's own fault, and a well-formed one that
        // names nothing is the daemon's answer to give.
        assert!(
            write(
                address,
                "/secret/api/assign",
                &format!("{EXTENSION_ORIGIN}Content-Type: application/json\r\n"),
                "{ not json"
            )
            .await
            .starts_with("HTTP/1.1 400")
        );
        assert!(
            write(
                address,
                "/secret/api/assign",
                &format!("{EXTENSION_ORIGIN}Content-Type: application/json\r\n"),
                &json!({"repo":"o/r","number":0,"harness":"claude"}).to_string()
            )
            .await
            .starts_with("HTTP/1.1 400")
        );
        // The write's route is not a read, and nothing else is a write.
        let as_read = fetch(address, "/secret/api/assign").await;
        assert!(as_read.starts_with("HTTP/1.1 404"), "{as_read}");
        let misplaced = write(
            address,
            "/secret/api/status",
            &format!("{EXTENSION_ORIGIN}Content-Type: application/json\r\n"),
            &body,
        )
        .await;
        assert!(misplaced.starts_with("HTTP/1.1 405"), "{misplaced}");
        assert!(client.args().is_empty(), "a refused write ran the client");
        task.abort();
    }

    #[tokio::test]
    async fn http_serves_assets_status_and_honest_errors() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        assert!(address.ip().is_loopback());
        let (tx, rx) = tokio::sync::watch::channel(Some(Ok(json!({"dashboard":{"cards":[]}}))));
        let task = tokio::spawn(serve(listener, "secret".into(), rx, "unused".into()));
        let rejected = fetch(address, "/api/status").await;
        assert!(rejected.starts_with("HTTP/1.1 404"));
        let html = fetch(address, "/secret/").await;
        assert!(html.contains("Active agents"));
        assert!(html.contains("Content-Security-Policy: default-src 'self'"));
        assert!(html.contains("Referrer-Policy: no-referrer"));
        assert!(
            fetch(address, "/secret/dashboard.css")
                .await
                .starts_with("HTTP/1.1 200")
        );
        assert!(
            fetch(address, "/secret/dashboard.js")
                .await
                .contains("emptyNode.hidden = true")
        );
        let status = fetch(address, "/secret/api/status").await;
        assert!(status.starts_with("HTTP/1.1 200"));
        assert!(status.contains("\"cards\":[]"));
        tx.send(Some(Err("remote server unreachable".into())))
            .unwrap();
        let error = fetch(address, "/secret/api/status").await;
        assert!(error.starts_with("HTTP/1.1 502"));
        assert!(error.contains("remote server unreachable"));
        task.abort();
    }

    #[tokio::test]
    async fn disabled_listener_does_not_bind_even_an_occupied_port() {
        let occupied = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let mut config = crate::config::DashboardConfig {
            port: occupied.local_addr().unwrap().port(),
            ..Default::default()
        };
        assert!(bind(&config).await.unwrap().is_none());
        config.enabled = true;
        assert!(
            bind(&config)
                .await
                .unwrap_err()
                .to_string()
                .contains("could not bind")
        );
    }

    #[tokio::test]
    async fn listener_lives_until_daemon_finishes_and_closes_with_it() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (finish, done) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(with_daemon(Some(listener), async move {
            done.await.unwrap();
            Ok(())
        }));
        assert!(
            fetch(address, "/unknown/")
                .await
                .starts_with("HTTP/1.1 404")
        );
        assert!(!task.is_finished());
        finish.send(()).unwrap();
        task.await.unwrap().unwrap();
        assert_listener_closed(address).await;
    }

    #[tokio::test]
    async fn daemon_error_is_preserved_and_closes_listener() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let result = with_daemon(Some(listener), async { bail!("daemon failed") }).await;
        assert!(result.unwrap_err().to_string().contains("daemon failed"));
        assert_listener_closed(address).await;
    }

    /// Reads until `needle` appears, leaving the stream and what has been read
    /// so far in place, so an event stream can be inspected frame by frame.
    async fn read_until(stream: &mut TcpStream, buffer: &mut Vec<u8>, needle: &str) -> String {
        let mut chunk = [0u8; 1024];
        loop {
            let seen = String::from_utf8_lossy(buffer).to_string();
            if seen.contains(needle) {
                return seen;
            }
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0, "event stream closed before {needle}");
            buffer.extend_from_slice(&chunk[..n]);
        }
    }

    /// Connects and switches to events, returning the reading end.
    async fn open_events(
        address: std::net::SocketAddr,
        buffer: &mut Vec<u8>,
    ) -> (TcpStream, String) {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(request("/secret/api/events", &address.to_string(), "").as_bytes())
            .await
            .unwrap();
        let headers = read_until(&mut stream, buffer, "event: status").await;
        (stream, headers)
    }

    #[tokio::test]
    async fn event_stream_sends_the_snapshot_then_every_change() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::watch::channel(Some(Ok(json!({"dashboard":{"cards":[]}}))));
        let task = tokio::spawn(serve(listener, "secret".into(), rx, "unused".into()));
        let mut buffer = Vec::new();
        let (mut stream, headers) =
            timeout(Duration::from_secs(5), open_events(address, &mut buffer))
                .await
                .unwrap();
        assert!(headers.starts_with("HTTP/1.1 200 OK"), "{headers}");
        assert!(headers.contains("Content-Type: text/event-stream"));
        assert!(headers.contains("Cache-Control: no-cache"));
        assert!(headers.contains("X-Content-Type-Options: nosniff"));
        // The first frame is the current snapshot, without waiting for a change.
        assert!(
            headers.contains("event: status\ndata: {\"cards\":[]}\n\n"),
            "{headers}"
        );
        tx.send(Some(Ok(json!({"dashboard":{"cards":[{"owner":"r#1"}]}}))))
            .unwrap();
        let second = timeout(
            Duration::from_secs(5),
            read_until(&mut stream, &mut buffer, "r#1"),
        )
        .await
        .unwrap();
        assert_eq!(second.matches("event: status").count(), 2);
        tx.send(Some(Err("remote server unreachable".into())))
            .unwrap();
        let third = timeout(
            Duration::from_secs(5),
            read_until(&mut stream, &mut buffer, "event: error"),
        )
        .await
        .unwrap();
        assert!(third.contains("remote server unreachable"));
        task.abort();
    }

    /// A tool window can hold an event stream open for as long as it is
    /// visible, so requests on other connections must still be answered.
    #[tokio::test]
    async fn an_open_event_stream_does_not_block_other_requests() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::watch::channel(Some(Ok(json!({"dashboard":{"cards":[]}}))));
        let task = tokio::spawn(serve(listener, "secret".into(), rx, "unused".into()));
        let mut buffer = Vec::new();
        let (_stream, headers) = timeout(Duration::from_secs(5), open_events(address, &mut buffer))
            .await
            .unwrap();
        assert!(headers.contains("event: status"), "{headers}");
        let status = timeout(Duration::from_secs(5), fetch(address, "/secret/api/status"))
            .await
            .unwrap();
        assert!(status.starts_with("HTTP/1.1 200"), "{status}");
        assert!(status.contains("\"cards\":[]"), "{status}");
        let html = timeout(Duration::from_secs(5), fetch(address, "/secret/"))
            .await
            .unwrap();
        assert!(html.starts_with("HTTP/1.1 200"), "{html}");
        drop(tx);
        task.abort();
    }

    /// The stream's own contract, against a real socket with a short keepalive
    /// so the test does not wait out the 25 seconds production uses. Ending the
    /// status stream must also end the connections reading it.
    #[tokio::test]
    async fn event_stream_keeps_an_idle_stream_open_and_ends_with_the_status_stream() {
        const SHORT: Duration = Duration::from_millis(50);
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, mut rx) = tokio::sync::watch::channel(Some(Ok(json!({"dashboard":{"cards":[]}}))));
        let mut stream = TcpStream::connect(address).await.unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        let writer =
            tokio::spawn(async move { events(&mut server, &mut rx, SHORT, REQUEST_TIMEOUT).await });
        let mut buffer = Vec::new();
        let first = timeout(
            Duration::from_secs(5),
            read_until(&mut stream, &mut buffer, "\"cards\":[]"),
        )
        .await
        .unwrap();
        assert!(first.starts_with("HTTP/1.1 200 OK"), "{first}");
        assert!(first.contains("Content-Type: text/event-stream"));
        assert!(first.contains("Cache-Control: no-cache"));
        assert!(first.contains("X-Content-Type-Options: nosniff"));
        assert!(
            first.contains("event: status\ndata: {\"cards\":[]}\n\n"),
            "{first}"
        );
        // An idle tick is a comment line, never the snapshot over again.
        let idle = timeout(
            Duration::from_secs(5),
            read_until(&mut stream, &mut buffer, ": keepalive"),
        )
        .await
        .unwrap();
        assert_eq!(idle.matches("event: status").count(), 1, "{idle}");
        tx.send(Some(Ok(json!({"dashboard":{"cards":[{"owner":"r#1"}]}}))))
            .unwrap();
        let second = timeout(
            Duration::from_secs(5),
            read_until(&mut stream, &mut buffer, "r#1"),
        )
        .await
        .unwrap();
        assert_eq!(second.matches("event: status").count(), 2, "{second}");
        tx.send(Some(Err("remote server unreachable".into())))
            .unwrap();
        let third = timeout(
            Duration::from_secs(5),
            read_until(&mut stream, &mut buffer, "event: error"),
        )
        .await
        .unwrap();
        assert!(third.contains("remote server unreachable"), "{third}");
        drop(tx);
        let mut rest = Vec::new();
        timeout(Duration::from_secs(5), stream.read_to_end(&mut rest))
            .await
            .expect("an event stream outlived the status stream")
            .unwrap();
        // Only keepalive comments may follow the last frame the test saw.
        assert!(
            !String::from_utf8_lossy(&rest).contains("event: "),
            "{}",
            String::from_utf8_lossy(&rest)
        );
        writer.await.unwrap();
    }

    /// A tool window can leave a stream open and stop draining it, and a
    /// suspended host does the same. The frame then cannot be written at all,
    /// so the task must give up rather than park in the write for good.
    #[tokio::test]
    async fn a_stream_nobody_reads_ends_instead_of_blocking() {
        // Larger than the loopback socket and receive buffers together, so the
        // write cannot finish however the kernel sizes them.
        let big = "x".repeat(4 * 1024 * 1024);
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (_tx, mut rx) =
            tokio::sync::watch::channel(Some(Ok(json!({"dashboard":{"cards":[{"owner":big}]}}))));
        let _reader = TcpStream::connect(address).await.unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        let writer = tokio::spawn(async move {
            events(
                &mut server,
                &mut rx,
                Duration::from_secs(30),
                Duration::from_millis(1),
            )
            .await
        });
        // `_reader` is held open and never read, so the only way out of the
        // write is the bound. Without it the task stays parked and this times
        // out instead.
        timeout(Duration::from_secs(5), writer)
            .await
            .expect("a stream nobody read left its task parked in the write")
            .unwrap();
    }

    #[tokio::test]
    async fn oversized_headers_are_bounded() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(address).await.unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        client.write_all(&vec![b'A'; MAX_HEADERS]).await.unwrap();
        assert!(
            timeout(REQUEST_TIMEOUT, read_request(&mut server))
                .await
                .unwrap()
                .is_err()
        );
    }
}
