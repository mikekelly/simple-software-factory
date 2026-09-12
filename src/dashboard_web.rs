//! Optional server-owned capability HTTP endpoint for canonical dashboard status.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{future::Future, io::Read, sync::Arc, time::Duration};
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
    let source = Arc::new(tokio::sync::Mutex::new(
        crate::dashboard_transport::StatusSource::new(None)?,
    ));
    tokio::select! {
        result = daemon => result,
        result = serve(listener, token, move || {
            let source = source.clone();
            async move { source.lock().await.snapshot().await }
        }) => result.context("server web dashboard stopped"),
    }
}

fn capability() -> Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

async fn serve<F, Fut>(listener: TcpListener, token: String, mut load: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Value>>,
{
    let host = listener.local_addr()?.to_string();
    loop {
        let (mut stream, _) = listener.accept().await?;
        let request = timeout(REQUEST_TIMEOUT, read_request(&mut stream)).await;
        let route = match &request {
            Ok(Ok(request)) => route(request, &host, &token),
            _ => Err(400),
        };
        let (status, kind, body) = match route {
            Ok("api/status") => {
                let snapshot = match timeout(Duration::from_secs(30), load()).await {
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
        || origins
            .first()
            .is_some_and(|origin| *origin != format!("http://{host}"))
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
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'\r\n\r\n",
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn request(path: &str, host: &str, extra: &str) -> String {
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{extra}\r\n")
    }
    #[test]
    fn checks_capability_host_origin_and_method() {
        let good = request("/secret/api/status", "127.0.0.1:123", "");
        assert_eq!(route(&good, "127.0.0.1:123", "secret"), Ok("api/status"));
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
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let task = tokio::spawn(serve(listener, "secret".into(), move || {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            async move {
                if n == 0 {
                    Ok(json!({"dashboard":{"cards":[]}}))
                } else {
                    bail!("remote server unreachable")
                }
            }
        }));
        let rejected = fetch(address, "/api/status").await;
        assert!(rejected.starts_with("HTTP/1.1 404"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
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
        assert!(TcpStream::connect(address).await.is_err());
    }

    #[tokio::test]
    async fn daemon_error_is_preserved_and_closes_listener() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let result = with_daemon(Some(listener), async { bail!("daemon failed") }).await;
        assert!(result.unwrap_err().to_string().contains("daemon failed"));
        assert!(TcpStream::connect(address).await.is_err());
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
