//! Optional server-owned capability HTTP endpoint for canonical dashboard status.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{future::Future, io::Read, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

const INDEX: &str = include_str!("../dashboard/index.html");
const CSS: &str = include_str!("../dashboard/dashboard.css");
const JS: &str = include_str!("../dashboard/dashboard.js");
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HEADERS: usize = 8192;
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

/// The listener and daemon share a lifetime. Dropping either future closes its
/// resources; there is no detached web task or browser idle expiration.
pub(crate) async fn with_daemon<F>(listener: Option<TcpListener>, daemon: F) -> Result<()>
where
    F: Future<Output = Result<()>>,
{
    let Some(listener) = listener else {
        return daemon.await;
    };
    let token = capability()?;
    tracing::info!(
        "Server web dashboard: http://{}/{token}/",
        listener.local_addr()?
    );
    let mut source = crate::dashboard_transport::StatusSource::new(None)?;
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
        result = serve(listener, token, latest_rx) => result.context("server web dashboard stopped"),
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

async fn serve(listener: TcpListener, token: String, latest: Latest) -> Result<()> {
    let host: std::sync::Arc<str> = listener.local_addr()?.to_string().into();
    let token: std::sync::Arc<str> = token.into();
    loop {
        let (stream, _) = listener.accept().await?;
        let (host, token, latest) = (host.clone(), token.clone(), latest.clone());
        // Each connection gets its own task so a long-lived event stream
        // does not stop the listener from answering anyone else.
        tokio::spawn(async move { handle(stream, &host, &token, latest).await });
    }
}

async fn handle(mut stream: TcpStream, host: &str, token: &str, mut latest: Latest) {
    let request = timeout(REQUEST_TIMEOUT, read_request(&mut stream)).await;
    let route = match &request {
        Ok(Ok(request)) => route(request, host, token),
        _ => Err(400),
    };
    if let Ok("api/events") = route {
        // Ends quietly when the client goes away or the daemon stops.
        let _ = events(&mut stream, &mut latest, KEEPALIVE).await;
        return;
    }
    let (status, kind, body) = match route {
        Ok("api/status") => {
            let snapshot = match timeout(Duration::from_secs(30), load(&mut latest)).await {
                Ok(result) => result.and_then(|value| presentation(&value)),
                Err(_) => Err(anyhow::anyhow!("SSF status timed out after 30 seconds")),
            };
            match snapshot {
                Ok(value) => (200, "application/json", value.to_string()),
                Err(error) => (
                    502,
                    "application/json",
                    json!({"error": format!("{error:#}")}).to_string(),
                ),
            }
        }
        Ok("" | "index.html") => (200, "text/html; charset=utf-8", INDEX.to_owned()),
        Ok("dashboard.css") => (200, "text/css; charset=utf-8", CSS.to_owned()),
        Ok("dashboard.js") => (200, "text/javascript; charset=utf-8", JS.to_owned()),
        Ok(_) => (
            404,
            "application/json",
            json!({"error":"not found"}).to_string(),
        ),
        Err(status) => (
            status,
            "application/json",
            json!({"error":"request rejected"}).to_string(),
        ),
    };
    let _ = timeout(REQUEST_TIMEOUT, respond(&mut stream, status, kind, &body)).await;
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

/// Server-sent events: the current snapshot, then every later one, with a
/// comment line while nothing changes so idle connections stay open.
async fn events(stream: &mut TcpStream, latest: &mut Latest, keepalive: Duration) -> Result<()> {
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n{SECURITY_HEADERS}\r\n"
            )
            .as_bytes(),
        )
        .await?;
    stream.flush().await?;
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
                stream.write_all(frame.as_bytes()).await?;
                stream.flush().await?;
            }
        }
        match next_tick(latest, keepalive).await {
            Tick::Changed => pending = true,
            Tick::Idle => {
                stream.write_all(b": keepalive\n\n").await?;
                stream.flush().await?;
            }
            // The daemon stopping ends its event streams instead of leaving
            // them re-sending the snapshot it can no longer refresh.
            Tick::Closed => return Ok(()),
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

fn route<'a>(request: &'a str, host: &str, token: &str) -> std::result::Result<&'a str, u16> {
    let mut lines = request.split("\r\n");
    let parts: Vec<_> = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    if parts.len() != 3 || !matches!(parts[2], "HTTP/1.0" | "HTTP/1.1") {
        return Err(400);
    }
    if parts[0] != "GET" {
        return Err(405);
    }
    let mut hosts = Vec::new();
    let mut origins = Vec::new();
    for line in lines.take_while(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or(400u16)?;
        if name.eq_ignore_ascii_case("host") {
            hosts.push(value.trim());
        }
        if name.eq_ignore_ascii_case("origin") {
            origins.push(value.trim());
        }
        if name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case("content-length")
        {
            return Err(400);
        }
    }
    if hosts != [host]
        || origins.len() > 1
        || origins.first().is_some_and(|origin| {
            // A Chrome MV3 extension's service worker sends its own origin.
            *origin != format!("http://{host}") && !origin.starts_with("chrome-extension://")
        })
    {
        return Err(403);
    }
    let path = parts[1].strip_prefix('/').ok_or(404u16)?;
    let (provided, relative) = path.split_once('/').ok_or(404u16)?;
    // Compare all capability bytes, without exposing matching prefixes.
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
    #[test]
    fn checks_capability_host_origin_and_method() {
        let good = request("/secret/api/status", "127.0.0.1:123", "");
        assert_eq!(route(&good, "127.0.0.1:123", "secret"), Ok("api/status"));
        for extra in [
            "Origin: http://127.0.0.1:123\r\n",
            "Origin: chrome-extension://abcdefghijklmnopabcdefghijklmnop\r\n",
        ] {
            let request = request("/secret/api/events", "127.0.0.1:123", extra);
            assert_eq!(route(&request, "127.0.0.1:123", "secret"), Ok("api/events"));
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
            (good.replacen("GET", "POST", 1), 405),
            (
                request(
                    "/secret/api/status",
                    "127.0.0.1:123",
                    "Content-Length: 10\r\n",
                ),
                400,
            ),
        ] {
            assert_eq!(route(&input, "127.0.0.1:123", "secret"), Err(expected));
        }
        assert_eq!(capability().unwrap().len(), 64);
        assert_ne!(capability().unwrap(), capability().unwrap());
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

    #[tokio::test]
    async fn http_serves_assets_status_and_honest_errors() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        assert!(address.ip().is_loopback());
        let (tx, rx) = tokio::sync::watch::channel(Some(Ok(json!({"dashboard":{"cards":[]}}))));
        let task = tokio::spawn(serve(listener, "secret".into(), rx));
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
        let task = tokio::spawn(serve(listener, "secret".into(), rx));
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
        let task = tokio::spawn(serve(listener, "secret".into(), rx));
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
        let writer = tokio::spawn(async move { events(&mut server, &mut rx, SHORT).await });
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
        writer.await.unwrap().unwrap();
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
