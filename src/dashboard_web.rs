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
/// How long a listing may wait on the factory: the client walks `PATH` and
/// may run a harness's own listing command.
const LISTING_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a write may wait on the factory. An assign's slow part is its
/// GitHub round trip, and the daemon's own client waits three minutes for
/// one; past this the client is given up on, though what the daemon was
/// doing may still land.
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);
const KEEPALIVE: Duration = Duration::from_secs(25);
/// How often a pane mirror reads its pane while someone is watching it: four
/// times a second, measured on #414 at a few percent of a core per pane.
const MIRROR_INTERVAL_MS: &str = "250";
/// Longest session id a pane route takes: `owner/repo#N` or `owner/repo~id`,
/// with room for GitHub's longest names.
const MAX_SESSION: usize = 256;
/// Panes read at once: each is a process reading herdr four times a second.
const MAX_MIRRORS: usize = 16;

/// The pane mirrors being watched, by session: one reader per pane however
/// many viewers it has, started by the first and stopped with the last. The
/// value is the SSE frame to send next, or `None` before the first read.
type Mirrors = std::sync::Arc<
    std::sync::Mutex<
        std::collections::HashMap<
            String,
            std::sync::Arc<tokio::sync::watch::Sender<Option<String>>>,
        >,
    >,
>;

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

/// The capability secret belongs to the factory, not to the run: it is
/// kept under the state directory beside `state.json`, so a restarted
/// server serves the URL a client was configured with. Deleting the file
/// and restarting rotates that URL.
fn capability_path() -> PathBuf {
    crate::config::state_dir().join("dashboard-token")
}

/// 32 random bytes, as the hex a capability path is made of.
const CAPABILITY_BYTES: usize = 32;

/// The whole secret a stored file holds, its newline aside: `classify`
/// compares the path segment byte for byte, so nothing else could be
/// served as a capability.
fn stored_capability(stored: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(stored).ok()?.trim();
    (text.len() == CAPABILITY_BYTES * 2 && text.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(text)
}

fn mint_capability() -> Result<String> {
    let mut bytes = [0u8; CAPABILITY_BYTES];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Whether the file is already readable by its owner alone.
fn is_private(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o777 == 0o600)
}

/// Make the file readable by its owner alone.
fn make_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::Permissions::from_mode(0o600);
    if let Err(error) = std::fs::set_permissions(path, mode) {
        tracing::warn!("could not set the mode on {}: {error}", path.display());
    }
}

/// The capability secret: generated once, then read back from the state
/// directory on every later start.
///
/// Stored content that is not a whole secret is replaced -- it addresses
/// nothing, since only a whole one is served. A file that cannot be read
/// is left alone, and this run serves a fresh secret instead: one that may
/// hold the factory's secret must not be overwritten over a read error,
/// and the dashboard's own file must not stop the factory. A secret that
/// cannot be stored is the same, said in the log rather than thrown.
fn capability() -> Result<String> {
    let path = capability_path();
    match std::fs::read(&path) {
        Ok(stored) => match stored_capability(&stored) {
            Some(token) => {
                // A file this build did not write -- restored from a
                // backup, or made by hand -- still keeps the URL private.
                if !is_private(&path) {
                    make_private(&path);
                }
                return Ok(token.to_string());
            }
            None => tracing::warn!(
                "{} holds {} bytes that are not a capability; writing a new one",
                path.display(),
                stored.len()
            ),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(
                "could not read {} ({error}); serving a new capability for this run and leaving the file as it is",
                path.display()
            );
            return mint_capability();
        }
    }
    let token = mint_capability()?;
    match store_capability(&path, &token) {
        Ok(()) => tracing::info!(
            "Server web dashboard capability stored in {} (kept across restarts; delete it to rotate)",
            path.display()
        ),
        Err(error) => tracing::warn!(
            "could not store the capability in {} ({error:#}); this URL will not survive a restart",
            path.display()
        ),
    }
    Ok(token)
}

/// Write the secret where the next start reads it, readable by its owner
/// alone.
fn store_capability(path: &Path, token: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    crate::config::write_atomic(path, format!("{token}\n").as_bytes(), 0o600)
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
    let mirrors = Mirrors::default();
    loop {
        let (stream, _) = listener.accept().await?;
        let (host, token, latest, client, mirrors) = (
            host.clone(),
            token.clone(),
            latest.clone(),
            client.clone(),
            mirrors.clone(),
        );
        // Each connection gets its own task so a long-lived event stream
        // does not stop the listener from answering anyone else.
        tokio::spawn(async move { handle(stream, &host, &token, latest, &client, &mirrors).await });
    }
}

async fn handle(
    mut stream: TcpStream,
    host: &str,
    token: &str,
    mut latest: Latest,
    client: &Path,
    mirrors: &Mirrors,
) {
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
        Ok(Routed::Read(relative)) if relative.starts_with("api/pane/") => {
            match pane_session(&relative["api/pane/".len()..]) {
                Ok(session) => {
                    let mut frames = mirror(mirrors, &session, client);
                    pane_events(&mut stream, &mut frames, KEEPALIVE, REQUEST_TIMEOUT).await;
                    return;
                }
                Err(answer) => answer,
            }
        }
        Ok(Routed::Read(relative)) => read(relative, &mut latest, client).await,
        Ok(Routed::Write(accepted)) => write(&mut stream, accepted, client).await,
        Err(status) => rejected(status),
    };
    let _ = timeout(REQUEST_TIMEOUT, respond(&mut stream, status, kind, &body)).await;
}

/// What a request refused before anything was read is told. Most of these are
/// about the shape of the request, which the client that made it already knows;
/// the body bound is the one whose number a client can act on, so it is the one
/// worth naming.
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
        Route::Scratch => scratch(&body, &write, client).await,
        Route::ScratchRelease => scratch_release(&body, &write, client).await,
        Route::ScratchResume => scratch_resume(&body, &write, client).await,
        Route::PaneInput => pane_input(&body, &write, client).await,
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

/// `POST api/scratch`: `ssf scratch create`'s own request, answered with its
/// `--json` result. Shared without `for`; that GitHub user's with it.
async fn scratch(body: &[u8], write: &Write<'_>, client: &Path) -> (u16, &'static str, String) {
    let request: ScratchRequest = match parse(body, "scratch") {
        Ok(request) => request,
        Err(answer) => return answer,
    };
    if let Some(login) = &request.r#for
        && !is_login(login)
    {
        return bad("for must be a GitHub login");
    }
    tracing::info!(
        origin = write.origin,
        repo = request.repo,
        harness = request.harness,
        model = request.model.as_deref().unwrap_or(""),
        effort = request.effort.as_deref().unwrap_or(""),
        owner_login = request.r#for.as_deref().unwrap_or(""),
        "web API scratch create"
    );
    carry(
        client,
        crate::ipc::Request::ScratchCreate {
            repo: request.repo,
            harness: request.harness,
            model: request.model,
            effort: request.effort,
            owner_login: request.r#for,
        },
        "the scratch request",
    )
    .await
}

/// `POST api/scratch/release`: kill a scratch session -- `ssf release` for it,
/// with the same checks. Unlike an item's release this one can be forced,
/// because a scratch session's workspace is all there is of it and the person
/// who made it is the one who says it can go: a refusal answers `409` with the
/// daemon's whole answer (the `check` included), so the overlay can show what
/// would be lost and ask again, and only a second, explicit request carries
/// `force`.
async fn scratch_release(
    body: &[u8],
    write: &Write<'_>,
    client: &Path,
) -> (u16, &'static str, String) {
    let request: ScratchReleaseRequest = match parse(body, "scratch release") {
        Ok(request) => request,
        Err(answer) => return answer,
    };
    if crate::origin::Scratch::parse(&request.session).is_none() {
        return bad("session must be a scratch session, owner/repo~id");
    }
    tracing::info!(
        origin = write.origin,
        session = request.session,
        force = request.force,
        "web API scratch release"
    );
    let (status, kind, body) = carry(
        client,
        crate::ipc::Request::Release {
            session: request.session,
            force: request.force,
        },
        "the release request",
    )
    .await;
    if status != 200 {
        return (status, kind, body);
    }
    let mut answer: Value = match serde_json::from_str(&body) {
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
    let message = crate::cli::release_refused_text(session, path, &problems);
    answer["error"] = message.into();
    (409, "application/json", answer.to_string())
}

/// `POST api/scratch/resume`: `ssf scratch resume` for a killed scratch
/// session.
async fn scratch_resume(
    body: &[u8],
    write: &Write<'_>,
    client: &Path,
) -> (u16, &'static str, String) {
    let request: SessionRequest = match parse(body, "scratch resume") {
        Ok(request) => request,
        Err(answer) => return answer,
    };
    if crate::origin::Scratch::parse(&request.session).is_none() {
        return bad("session must be a scratch session, owner/repo~id");
    }
    tracing::info!(
        origin = write.origin,
        session = request.session,
        "web API scratch resume"
    );
    carry(
        client,
        crate::ipc::Request::ScratchResume {
            session: request.session,
        },
        "the resume request",
    )
    .await
}

/// `POST api/pane/input`: type into a session's agent pane, from the pane
/// mirror's terminal. `text` is what the terminal sent (Enter, Backspace and
/// the arrows as the bytes a terminal sends for them); `keys` are named keys
/// herdr presses (`enter`, `ctrl+c`).
async fn pane_input(body: &[u8], write: &Write<'_>, client: &Path) -> (u16, &'static str, String) {
    let request: PaneInputRequest = match parse(body, "pane input") {
        Ok(request) => request,
        Err(answer) => return answer,
    };
    if !is_session(&request.session) {
        return bad("session must be owner/repo#N or owner/repo~id");
    }
    // Whether an item's pane takes typing is the factory's setting
    // (`item_pane_input`, off by default: #439), decided where the config
    // is -- in the guest, for a factory in a VM -- by `ssf __pane send`.
    if request
        .text
        .as_deref()
        .is_some_and(|text| text.contains('\0'))
    {
        return bad("text cannot carry a NUL byte");
    }
    if !request.keys.iter().all(|key| is_key(key)) {
        return bad("keys are key names such as enter, esc or ctrl+c");
    }
    let text = request.text.filter(|text| !text.is_empty());
    if text.is_none() && request.keys.is_empty() {
        return bad("nothing to type: give text or keys");
    }
    // What was typed is not logged: it is the person's, and may be a secret
    // they were asked for.
    tracing::debug!(
        origin = write.origin,
        session = request.session,
        chars = text.as_deref().map(|t| t.chars().count()).unwrap_or(0),
        keys = request.keys.len(),
        "web API pane input"
    );
    let mut args = vec!["__pane".to_string(), "send".into(), request.session];
    if let Some(text) = text {
        // One argument, `=`-joined, so text that starts with a dash is text.
        args.push(format!("--text={text}"));
    }
    for key in request.keys {
        args.push(format!("--key={key}"));
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match ask(client, &args, WRITE_TIMEOUT).await {
        Ok(output) if output.status.success() => {
            (200, "application/json", json!({"sent": true}).to_string())
        }
        Ok(output) if output.status.code() == Some(crate::pane::INPUT_REFUSED) => {
            bad(client_error(&output, "the input"))
        }
        Ok(output) => failure(
            Some(crate::ipc::RefusalKind::Conflict),
            client_error(&output, "the input"),
        ),
        Err(error) => failure_of(&error),
    }
}

/// A GitHub login: letters, digits and single hyphens, as GitHub allows.
fn is_login(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 39
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// A key name herdr presses: `enter`, `esc`, `ctrl+c`, `f5`.
fn is_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 32
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'_'))
}

/// A session id a route takes: an item's (`owner/repo#N`) or a scratch
/// session's (`owner/repo~id`).
fn is_session(session: &str) -> bool {
    session.len() <= MAX_SESSION
        && (crate::origin::Origin::parse(session).is_some()
            || crate::origin::Scratch::parse(session).is_some())
}

/// The session a pane stream names, percent-decoded (`#` cannot be in a URL
/// path as itself), or the `400` for one that is not a session.
fn pane_session(encoded: &str) -> std::result::Result<String, (u16, &'static str, String)> {
    // Named as ssf names it, so two spellings of one session share a reader.
    percent_decode(encoded)
        .filter(|session| session.len() <= MAX_SESSION)
        .and_then(|session| {
            crate::origin::Scratch::parse(&session)
                .map(|s| s.to_string())
                .or_else(|| crate::origin::Origin::parse(&session).map(|o| o.to_string()))
        })
        .ok_or_else(|| bad("the pane route takes a session, owner/repo#N or owner/repo~id"))
}

/// `%XX`-decoding of a path segment, or `None` for one that is not valid
/// UTF-8 or carries a broken escape.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A viewer of `session`'s pane: the running mirror's frames, or a mirror
/// started for this viewer. The map is only touched under its lock, and a
/// mirror leaves it under the same lock, so a viewer never joins a mirror
/// that is on its way out.
fn mirror(
    mirrors: &Mirrors,
    session: &str,
    client: &Path,
) -> tokio::sync::watch::Receiver<Option<String>> {
    let mut map = mirrors
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(sender) = map.get(session) {
        return sender.subscribe();
    }
    if map.len() >= MAX_MIRRORS {
        // The sender goes at once, so the viewer gets this frame and the end.
        let (_, frames) = tokio::sync::watch::channel(Some(error_frame(&format!(
            "{MAX_MIRRORS} panes are already being watched; close one and try again"
        ))));
        return frames;
    }
    let (sender, frames) = tokio::sync::watch::channel(None);
    let sender = std::sync::Arc::new(sender);
    map.insert(session.to_string(), sender.clone());
    let command = match mirror_command(client, session) {
        Ok(command) => Some(command),
        Err(error) => {
            sender.send_replace(Some(error_frame(&format!("{error:#}"))));
            None
        }
    };
    tokio::spawn(run_mirror(
        mirrors.clone(),
        session.to_string(),
        sender,
        command,
    ));
    frames
}

/// `ssf __pane watch <session>`, through the status stream's own transport.
fn mirror_command(client: &Path, session: &str) -> Result<tokio::process::Command> {
    Ok(crate::dashboard_transport::local_client_command(
        client,
        &[
            "__pane",
            "watch",
            session,
            "--interval-ms",
            MIRROR_INTERVAL_MS,
        ],
        crate::server_catalog::service_local_context(),
        crate::server_catalog::selected_vm_context()?.as_ref(),
        crate::server_catalog::selected_target_identity()?.as_ref(),
    ))
}

/// One pane's reader: runs the watch while anyone is looking, passing on each
/// frame it prints (it prints one only when the screen changed), and stops it
/// -- the child goes with the command -- once the last viewer has gone.
async fn run_mirror(
    mirrors: Mirrors,
    session: String,
    sender: std::sync::Arc<tokio::sync::watch::Sender<Option<String>>>,
    command: Option<tokio::process::Command>,
) {
    use tokio::io::AsyncBufReadExt;
    let leave = |mirrors: &Mirrors| {
        let mut map = mirrors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if map
            .get(&session)
            .is_some_and(|held| std::sync::Arc::ptr_eq(held, &sender))
        {
            map.remove(&session);
        }
    };
    let Some(mut command) = command else {
        leave(&mirrors);
        return;
    };
    // A group of its own, so that stopping it stops everything it started:
    // with the factory in a VM the client is a wrapper around ssh, and the
    // watch runs in the guest behind it.
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            sender.send_replace(Some(error_frame(&format!(
                "could not start the pane reader: {error}"
            ))));
            leave(&mirrors);
            return;
        }
    };
    let group = child.id();
    // Read as it comes, so a chatty reader cannot fill the pipe and stall;
    // the start of it is what says why a reader stopped.
    let mut stderr = child.stderr.take().expect("piped");
    let mut stderr = tokio::spawn(async move {
        let mut kept = Vec::new();
        let mut buffer = [0u8; 1024];
        while let Ok(read @ 1..) = stderr.read(&mut buffer).await {
            let room = 4096usize.saturating_sub(kept.len());
            kept.extend_from_slice(&buffer[..read.min(room)]);
        }
        String::from_utf8_lossy(&kept).into_owned()
    });
    let mut lines = tokio::io::BufReader::new(child.stdout.take().expect("piped")).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                // The watch's heartbeat.
                Ok(Some(line)) if line.is_empty() => {}
                Ok(Some(line)) => {
                    let frame = pane_frame(&line);
                    sender.send_if_modified(|last| {
                        if last.as_deref() == Some(frame.as_str()) {
                            return false;
                        }
                        *last = Some(frame);
                        true
                    });
                }
                _ => {
                    // A watch that ended on an error said it as its last
                    // frame; one that did not says why here, if anywhere.
                    let detail = timeout(REQUEST_TIMEOUT, &mut stderr).await;
                    let detail = detail.ok().and_then(|d| d.ok()).unwrap_or_default();
                    let detail = detail.trim();
                    if !detail.is_empty() {
                        sender.send_replace(Some(error_frame(detail)));
                    } else if !sender.borrow().as_deref().is_some_and(|f| f.starts_with("event: error")) {
                        sender.send_replace(Some(error_frame("the pane reader stopped")));
                    }
                    break;
                }
            },
            _ = sender.closed() => {
                // Nobody is watching. A viewer that arrived since holds the
                // lock to join, so the count is read under it too.
                let map = mirrors.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                if sender.receiver_count() == 0 {
                    drop(map);
                    break;
                }
            }
        }
    }
    leave(&mirrors);
    if let Some(group) = group.and_then(|pid| i32::try_from(pid).ok()) {
        // SAFETY: a signal to the group this reader was started in.
        unsafe { libc::killpg(group, libc::SIGKILL) };
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// A watch line as the frame a viewer gets: `event: screen` with the
/// `{"screen": …}` object, or `event: error` with the `{"error": …}` one.
fn pane_frame(line: &str) -> String {
    match serde_json::from_str::<Value>(line) {
        Ok(value) if value.get("screen").is_some() => format!("event: screen\ndata: {value}\n\n"),
        Ok(value) if value.get("error").is_some() => format!("event: error\ndata: {value}\n\n"),
        _ => error_frame("the pane reader said something that is not a frame"),
    }
}

/// Server-sent events for a pane mirror: the latest frame, then each new one,
/// with a keepalive comment while the screen stands still. Ends when the
/// viewer goes -- noticed at once, since a viewer is what keeps the pane
/// being read -- or the mirror stops.
async fn pane_events(
    stream: &mut TcpStream,
    frames: &mut tokio::sync::watch::Receiver<Option<String>>,
    keepalive: Duration,
    write_timeout: Duration,
) {
    let (mut reader, mut writer) = stream.split();
    // A viewer sends nothing after its request, so a read that returns is
    // the viewer going away.
    let gone = async {
        let mut byte = [0u8; 1];
        loop {
            match reader.read(&mut byte).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    };
    let send = async {
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n{SECURITY_HEADERS}\r\n"
        );
        if !write_frame(&mut writer, headers.as_bytes(), write_timeout).await {
            return;
        }
        let mut pending = true;
        loop {
            // Only the first frame and then each change is sent: the channel
            // keeps its value, so a keepalive must not resend the last one.
            if pending {
                pending = false;
                let frame = frames.borrow_and_update().clone();
                if let Some(frame) = frame
                    && !write_frame(&mut writer, frame.as_bytes(), write_timeout).await
                {
                    return;
                }
            }
            match timeout(keepalive, frames.changed()).await {
                Ok(Ok(())) => pending = true,
                // The mirror stopped; its last frame has been sent.
                Ok(Err(_)) => return,
                Err(_) => {
                    if !write_frame(&mut writer, b": keepalive\n\n", write_timeout).await {
                        return;
                    }
                }
            }
        }
    };
    tokio::select! {
        () = gone => {}
        () = send => {}
    }
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
async fn write_frame(
    stream: &mut (impl AsyncWriteExt + Unpin),
    frame: &[u8],
    write_timeout: Duration,
) -> bool {
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
///
/// The item writes are the `ssf` commands that act on an item without being a
/// comment on it -- assign, hand over and release -- and a client that wants
/// to say something to an item's agent says it on the item, where the
/// exchange is part of the item's record (#439). The scratch writes are `ssf
/// scratch` and the kill of one. The one route that types at an agent is the
/// pane mirror's input (#414): a terminal on the agent's own pane, the same
/// thing a person attached to the factory's herdr has, not a message box.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Route {
    Assign,
    Handover,
    Release,
    Scratch,
    ScratchRelease,
    ScratchResume,
    PaneInput,
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

/// The body of a scratch create: `ssf scratch create`'s own arguments, with
/// `for` as `--for`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScratchRequest {
    /// Watched repository, `owner/name`.
    repo: String,
    /// Harness the session runs (`ssf agents` lists the ids).
    harness: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    /// GitHub login of the person the session is for; shared when absent.
    #[serde(default)]
    r#for: Option<String>,
}

/// The body of a scratch kill: the session, and `force` for the second,
/// explicit request after the checks found work.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScratchReleaseRequest {
    session: String,
    #[serde(default)]
    force: bool,
}

/// A body that names one session.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionRequest {
    session: String,
}

/// The body of a pane input: what to type into the session's agent pane.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PaneInputRequest {
    session: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    keys: Vec<String>,
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
        "api/scratch" => Route::Scratch,
        "api/scratch/release" => Route::ScratchRelease,
        "api/scratch/resume" => Route::ScratchResume,
        "api/pane/input" => Route::PaneInput,
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
    }

    /// The secret is minted once and then kept: a restart serves the URL the
    /// extension was configured with, which is the whole point of storing it.
    /// Content that is not a whole secret is replaced, never served as a path;
    /// a file that cannot be read at all is left alone.
    #[test]
    fn the_capability_is_generated_once_and_kept() {
        let _sandbox = crate::config::test_support::sandbox();
        let first = capability().unwrap();
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let path = capability_path();
        let mode = || std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let chmod =
            |mode| std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(mode(), 0o600);
        assert_eq!(capability().unwrap(), first, "a restart reuses the secret");
        // A truncated or hand-edited file addresses nothing, so it is
        // replaced with a whole secret rather than served as a path.
        std::fs::write(&path, "not a secret").unwrap();
        let replaced = capability().unwrap();
        assert_eq!(replaced.len(), 64);
        assert_ne!(replaced, first);
        assert_eq!(capability().unwrap(), replaced);
        // A secret this build did not write — restored from a backup, or made
        // by hand — is still kept private.
        let hand = "a".repeat(64);
        std::fs::write(&path, format!("{hand}\n")).unwrap();
        chmod(0o644);
        assert_eq!(capability().unwrap(), hand);
        assert_eq!(mode(), 0o600);
        // One that cannot be read may still be the factory's secret, so it is
        // left as it is, and this run serves a fresh secret rather than taking
        // the factory down over the dashboard's own file.
        chmod(0o000);
        let unreadable = capability().unwrap();
        assert_eq!(unreadable.len(), 64);
        assert_ne!(unreadable, hand);
        chmod(0o600);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            hand,
            "an unreadable file is not overwritten"
        );
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
            ("/secret/api/scratch", Route::Scratch),
            ("/secret/api/scratch/release", Route::ScratchRelease),
            ("/secret/api/scratch/resume", Route::ScratchResume),
            ("/secret/api/pane/input", Route::PaneInput),
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

    /// Each write is the request its own `ssf` command sends the
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
        let handover_body = json!({"repo":"o/r","number":7,"harness":"omp",
            "model":"deepseek/deepseek-flash","effort":"high","note":"carry on"})
        .to_string();
        let release_body = json!({"repo":"o/r","number":7}).to_string();
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
    /// asked: the item's number, and a field the route does not take.
    #[tokio::test]
    async fn refuses_write_bodies_a_route_cannot_read() {
        let client = Client::new("unread", "", 0);
        std::fs::remove_file(client.program()).unwrap();
        let (address, task) = served(&client).await;
        let bad_json = "{ not json".to_string();
        let zero_stack = json!({"repo":"o/r","number":0,"harness":"claude"}).to_string();
        let zero_item = json!({"repo":"o/r","number":0}).to_string();
        let extra_field =
            json!({"repo":"o/r","number":7,"harness":"claude","branch":"x"}).to_string();
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
                "/secret/api/handover",
                extra_field.as_str(),
                "unknown field",
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
        ] {
            let response = fetch(address, path).await;
            assert!(response.starts_with("HTTP/1.1 404"), "{path}: {response}");
        }
        assert!(client.args().is_empty(), "a refused write ran the client");
        task.abort();
    }

    /// The scratch writes are `ssf scratch create`, `ssf release` and `ssf
    /// scratch resume`'s own requests. A scratch session has no item number:
    /// it is named by its session id, and the item routes are not touched.
    #[tokio::test]
    async fn the_scratch_writes_run_the_requests_their_commands_send() {
        let created = json!({"session":"o/r~ab12","poll_interval_secs":10});
        for (name, path, body, expected) in [
            (
                "create",
                "/secret/api/scratch",
                json!({"repo":"o/r","harness":"claude","model":"opus","for":"alice"}),
                crate::ipc::Request::ScratchCreate {
                    repo: "o/r".into(),
                    harness: "claude".into(),
                    model: Some("opus".into()),
                    effort: None,
                    owner_login: Some("alice".into()),
                },
            ),
            (
                "shared",
                "/secret/api/scratch",
                json!({"repo":"o/r","harness":"codex"}),
                crate::ipc::Request::ScratchCreate {
                    repo: "o/r".into(),
                    harness: "codex".into(),
                    model: None,
                    effort: None,
                    owner_login: None,
                },
            ),
            (
                "resume",
                "/secret/api/scratch/resume",
                json!({"session":"o/r~ab12"}),
                crate::ipc::Request::ScratchResume {
                    session: "o/r~ab12".into(),
                },
            ),
        ] {
            let client = Client::new(name, &json!({"ok":true,"data":created}).to_string(), 0);
            let (address, task) = served(&client).await;
            let response = post_json(address, path, &body.to_string()).await;
            assert!(response.starts_with("HTTP/1.1 200"), "{name}: {response}");
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

    /// Killing a scratch session is two opt-ins when its workspace holds work:
    /// the first request is never forced and answers `409` with the checks,
    /// so the overlay can say what would be lost; only a second request that
    /// says `force` removes it anyway. A clean workspace goes on the first.
    #[tokio::test]
    async fn killing_a_scratch_session_with_work_in_it_takes_a_second_request() {
        let refused = json!({"released":false,"session":"o/r~ab12","path":"/w/s",
            "check":{"state":"dirty","safe":false,"problems":["1 file is not committed"]}});
        let client = Client::new("kill", &json!({"ok":true,"data":refused}).to_string(), 0);
        let (address, task) = served(&client).await;
        let response = post_json(
            address,
            "/secret/api/scratch/release",
            &json!({"session":"o/r~ab12"}).to_string(),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 409"), "{response}");
        let answer: Value =
            serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(answer["check"]["problems"][0], "1 file is not committed");
        assert!(
            answer["error"]
                .as_str()
                .unwrap()
                .contains("1 file is not committed")
        );
        assert_eq!(
            serde_json::from_str::<crate::ipc::Request>(&client.args()[2]).unwrap(),
            crate::ipc::Request::Release {
                session: "o/r~ab12".into(),
                force: false,
            }
        );
        task.abort();

        let released = json!({"released":true,"session":"o/r~ab12","pending":true});
        let client = Client::new("force", &json!({"ok":true,"data":released}).to_string(), 0);
        let (address, task) = served(&client).await;
        let response = post_json(
            address,
            "/secret/api/scratch/release",
            &json!({"session":"o/r~ab12","force":true}).to_string(),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(
            serde_json::from_str::<crate::ipc::Request>(&client.args()[2]).unwrap(),
            crate::ipc::Request::Release {
                session: "o/r~ab12".into(),
                force: true,
            }
        );
        task.abort();
    }

    /// What a scratch or pane write cannot read is refused without asking the
    /// factory: a force on an item (an item's release is never forced from a
    /// browser), a login that is not one, a key that is not a key name.
    #[tokio::test]
    async fn refuses_scratch_and_pane_bodies_a_route_cannot_read() {
        let client = Client::new("scratch-unread", "", 0);
        std::fs::remove_file(client.program()).unwrap();
        let (address, task) = served(&client).await;
        for (path, body, needle) in [
            (
                "/secret/api/scratch/release",
                json!({"session":"o/r#7","force":true}),
                "must be a scratch session",
            ),
            (
                "/secret/api/scratch/resume",
                json!({"session":"o/r#7"}),
                "must be a scratch session",
            ),
            (
                "/secret/api/scratch",
                json!({"repo":"o/r","harness":"claude","for":"not a login"}),
                "for must be a GitHub login",
            ),
            (
                "/secret/api/scratch",
                json!({"repo":"o/r","harness":"claude","number":0}),
                "unknown field",
            ),
            (
                "/secret/api/pane/input",
                json!({"session":"o/r~ab12","keys":["enter; rm -rf /"]}),
                "key names",
            ),
            (
                "/secret/api/pane/input",
                json!({"session":"o/r~ab12"}),
                "nothing to type",
            ),
            (
                "/secret/api/pane/input",
                json!({"session":"nonsense","text":"x"}),
                "session must be",
            ),
        ] {
            let response = post_json(address, path, &body.to_string()).await;
            assert!(
                response.starts_with("HTTP/1.1 400"),
                "{path} {body}: {response}"
            );
            assert!(response.contains(needle), "{path} {body}: {response}");
        }
        // Input is a write: a page's own origin, or none, is refused before
        // anything is read.
        let body = json!({"session":"o/r~ab12","text":"y"}).to_string();
        for extra in [
            "Content-Type: application/json\r\n".to_string(),
            format!("Origin: http://{address}\r\nContent-Type: application/json\r\n"),
            "Origin: https://github.com\r\nContent-Type: application/json\r\n".to_string(),
        ] {
            let response = write(address, "/secret/api/pane/input", &extra, &body).await;
            assert!(response.starts_with("HTTP/1.1 403"), "{extra}: {response}");
        }
        assert!(client.args().is_empty(), "a refused write ran the client");
        // A pane stream names a session; anything else is refused unread.
        let response = fetch(address, "/secret/api/pane/nonsense").await;
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        task.abort();
    }

    /// Typing into a scratch session's pane is `ssf __pane send`, with the text as one
    /// `--text=` argument so text that starts with a dash is still text.
    #[tokio::test]
    async fn pane_input_types_through_the_factorys_client() {
        let client = Client::new("input", "", 0);
        let (address, task) = served(&client).await;
        let body = json!({"session":"o/r~t414","text":"-y\u{1b}[A","keys":["ctrl+c"]}).to_string();
        let response = post_json(address, "/secret/api/pane/input", &body).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(
            client.args(),
            [
                "__client",
                "__pane",
                "send",
                "o/r~t414",
                "--text=-y\u{1b}[A",
                "--key=ctrl+c"
            ]
        );
        task.abort();
    }

    /// An item's pane the factory keeps view-only (`item_pane_input` off,
    /// decided by `ssf __pane send` where the config is) is the request's
    /// fault, `400`, in the factory's words -- not a failure to type.
    #[tokio::test]
    async fn a_view_only_pane_refuses_typing_as_the_factory_words_it() {
        let client = Client::new("viewonly", "", 0);
        std::fs::write(
            client.program(),
            "#!/bin/sh\necho \"o/r#7's pane is view-only: speak to an item's agent by commenting on the item\" >&2\nexit 2\n",
        )
        .unwrap();
        let (address, task) = served(&client).await;
        let body = json!({"session":"o/r#7","text":"y"}).to_string();
        let response = post_json(address, "/secret/api/pane/input", &body).await;
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        assert!(response.contains("commenting on the item"), "{response}");
        task.abort();
    }

    /// Reads one pane stream until it has `frames` screen frames, or fails
    /// the test.
    async fn screens(address: std::net::SocketAddr, path: &str, frames: usize) -> Vec<String> {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(request(path, &address.to_string(), "").as_bytes())
            .await
            .unwrap();
        let mut seen = String::new();
        timeout(Duration::from_secs(10), async {
            let mut buffer = [0u8; 4096];
            while seen.matches("event: screen").count() < frames {
                let read = stream.read(&mut buffer).await.unwrap();
                assert!(read > 0, "the stream ended: {seen}");
                seen.push_str(&String::from_utf8_lossy(&buffer[..read]));
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no {frames} frames: {seen}"));
        seen.lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(str::to_string)
            .collect()
    }

    /// Two viewers of one pane share one reader, and a screen that did not
    /// change is not sent again. When the last viewer goes the reader stops,
    /// and the next viewer starts a new one.
    #[tokio::test]
    async fn one_reader_per_pane_however_many_watch_it() {
        let client = Client::new("mirror", "", 0);
        let log = client.root.join("runs");
        // A reader that prints the same screen twice, then another, and then
        // waits the way a reader of an idle pane does.
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{log}'\n\
             echo '{{\"screen\":\"one\"}}'\necho '{{\"screen\":\"one\"}}'\nsleep 0.3\n\
             echo '{{\"screen\":\"two\"}}'\nexec sleep 30\n",
            log = log.display()
        );
        std::fs::write(client.program(), script).unwrap();
        let (address, task) = served(&client).await;
        let path = "/secret/api/pane/o%2Fr~ab12";
        let (first, second) = tokio::join!(screens(address, path, 2), screens(address, path, 2));
        for frames in [&first, &second] {
            assert_eq!(
                frames.last().map(String::as_str),
                Some("{\"screen\":\"two\"}"),
                "{frames:?}"
            );
            assert!(
                !frames.windows(2).any(|pair| pair[0] == pair[1]),
                "a frame was sent twice: {frames:?}"
            );
        }
        let runs = std::fs::read_to_string(&log).unwrap();
        assert_eq!(runs.lines().count(), 1, "{runs}");
        assert_eq!(
            runs.trim(),
            "__client __pane watch o/r~ab12 --interval-ms 250"
        );
        // Both viewers have gone; the reader goes with them, and a new
        // viewer starts it again.
        timeout(Duration::from_secs(5), async {
            loop {
                let _ = screens(address, path, 1).await;
                if std::fs::read_to_string(&log).unwrap().lines().count() == 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the reader outlived its last viewer");
        task.abort();
    }

    /// Stopping a reader stops everything it started: with the factory in a
    /// VM the client is a wrapper whose ssh child would otherwise outlive it,
    /// still reading the pane in the guest.
    #[tokio::test]
    async fn a_reader_that_stops_takes_its_children_with_it() {
        let client = Client::new("group", "", 0);
        let pid = client.root.join("child");
        let script = format!(
            "#!/bin/sh
sleep 30 &
echo $! > '{pid}'
             echo '{{\"screen\":\"one\"}}'
wait
",
            pid = pid.display()
        );
        std::fs::write(client.program(), script).unwrap();
        let (address, task) = served(&client).await;
        let _ = screens(address, "/secret/api/pane/o%2Fr%7Eab12", 1).await;
        let child: i32 = std::fs::read_to_string(&pid)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        timeout(Duration::from_secs(5), async {
            while still_running(child) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the reader's child outlived its last viewer");
        task.abort();
    }

    /// Whether `pid` is still running: field 3 of `/proc/<pid>/stat`, after
    /// the parenthesized comm, which can hold spaces. A `Z` process has
    /// exited and only waits to be reaped, which is what this test meets: the
    /// group kill takes the reader and its child at once, so nothing in this
    /// process is left to reap the child — where the namespace's init reaps
    /// what it did not start (a machine's does; a test job's container need
    /// not) it is gone, and where it does not, it sits as a zombie. Exited
    /// either way, and not still reading a pane, though `kill(pid, 0)` counts
    /// a zombie as present.
    #[cfg(target_os = "linux")]
    fn still_running(pid: i32) -> bool {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        stat.rsplit_once(") ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z")
    }

    /// Without `/proc`, whether the process is there at all; the platforms
    /// that have none reap orphaned children themselves.
    #[cfg(not(target_os = "linux"))]
    fn still_running(pid: i32) -> bool {
        // SAFETY: signal 0 only asks whether the process is there.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// Two spellings of one session are one pane, read once.
    #[test]
    fn a_pane_is_named_as_ssf_names_it() {
        assert_eq!(pane_session("o%2Fr%7Eab12").unwrap(), "o/r~ab12");
        assert_eq!(pane_session("o%2Fr%237").unwrap(), "o/r#7");
        assert!(pane_session("nonsense").is_err());
    }

    /// The endpoint has no message route: `api/message` was removed with the
    /// overlay's message box (#439), and a POST to it is a path under the
    /// capability that is not a write route -- the same `405` any other path
    /// gets. The only input is the pane mirror's terminal (#414), which the
    /// factory's `item_pane_input` keeps off item sessions by default.
    #[tokio::test]
    async fn there_is_no_route_that_types_at_an_agent() {
        let client = Client::new("gone", "", 0);
        std::fs::remove_file(client.program()).unwrap();
        let (address, task) = served(&client).await;
        let body = json!({"repo":"o/r","number":7,"text":"hello"}).to_string();
        let response = post_json(address, "/secret/api/message", &body).await;
        assert!(response.starts_with("HTTP/1.1 405"), "{response}");
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
        // The 413 names the bound rather than saying only that the request was
        // refused.
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
                .contains("document.querySelector(\"#cards\")")
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
        let _sandbox = crate::config::test_support::sandbox();
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
        let _sandbox = crate::config::test_support::sandbox();
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
