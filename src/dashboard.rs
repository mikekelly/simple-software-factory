//! Client-only capability HTTP server presenting canonical server status.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{future::Future, io::Read, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    time::{Instant, timeout, timeout_at},
};

const INDEX: &str = include_str!("../dashboard/index.html");
const CSS: &str = include_str!("../dashboard/dashboard.css");
const JS: &str = include_str!("../dashboard/dashboard.js");
const IDLE: Duration = Duration::from_secs(300);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HEADERS: usize = 8192;

pub(crate) async fn run(server: Option<String>, no_browser: bool) -> Result<()> {
    let source = Arc::new(Mutex::new(crate::dashboard_transport::StatusSource::new(
        server,
    )?));
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let token = capability()?;
    let url = format!("http://{}/{token}/", listener.local_addr()?);
    println!("{url}");
    if !no_browser {
        #[cfg(target_os = "macos")]
        let opener = "open";
        #[cfg(not(target_os = "macos"))]
        let opener = "xdg-open";
        let result = timeout(
            REQUEST_TIMEOUT,
            tokio::process::Command::new(opener)
                .arg(&url)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await;
        if !matches!(result, Ok(Ok(status)) if status.success()) {
            eprintln!("Could not open a browser; open the URL above on this machine.");
        }
    }
    serve(listener, token, IDLE, move || {
        let source = source.clone();
        async move { source.lock().await.snapshot().await }
    })
    .await
}

fn capability() -> Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

async fn serve<F, Fut>(
    listener: TcpListener,
    token: String,
    idle: Duration,
    mut load: F,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Value>>,
{
    let host = listener.local_addr()?.to_string();
    let mut deadline = Instant::now() + idle;
    loop {
        let Ok(connection) = timeout_at(deadline, listener.accept()).await else {
            return Ok(());
        };
        let (mut stream, _) = connection?;
        let request = timeout_at(
            deadline.min(Instant::now() + REQUEST_TIMEOUT),
            read_request(&mut stream),
        )
        .await;
        let route = match &request {
            Ok(Ok(request)) => route(request, &host, &token),
            _ => Err(400),
        };
        let (status, kind, body) = match route {
            Ok("api/status") => {
                // Only authenticated status polls extend the browser's lifetime.
                deadline = Instant::now() + idle;
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
        if Instant::now() >= deadline {
            return Ok(());
        }
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

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}
fn issue(row: &Value, fallback: &str) -> Value {
    let id = row["id"].as_str().unwrap_or(fallback);
    let url = row["url"]
        .as_str()
        .and_then(|url| reqwest::Url::parse(url).ok())
        .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some());
    json!({"id":id,"title":row["title"].as_str().unwrap_or(id),"url":url.map(|url|url.to_string()),
        "kind":row["kind"].as_str().unwrap_or("issue"),"active":row["active"] == true})
}

fn presentation(payload: &Value) -> Result<Value> {
    let rows = payload["sessions"]
        .as_array()
        .context("SSF returned status data in an unexpected format")?;
    let active: Vec<_> = rows
        .iter()
        .filter(|row| {
            row["active"] == true
                && row["subscriber_only"] != true
                && !text(row, "owner").is_empty()
        })
        .collect();
    let mut owners = Vec::new();
    let mut cards = Vec::new();
    for row in &active {
        let owner = text(row, "owner");
        if owners.contains(&owner) {
            continue;
        }
        owners.push(owner);
        let primary = rows
            .iter()
            .find(|row| text(row, "id") == owner)
            .unwrap_or(&Value::Null);
        let owned: Vec<_> = active
            .iter()
            .copied()
            .filter(|row| text(row, "owner") == owner)
            .collect();
        let mut candidates = Vec::new();
        if !primary.is_null() {
            candidates.push(primary);
        }
        candidates.extend(owned.iter().copied());
        candidates.sort_by(|a, b| text(b, "last_activity_at").cmp(text(a, "last_activity_at")));
        let runtime = candidates[0];
        let message = candidates
            .iter()
            .map(|row| text(row, "last_assistant_message").trim())
            .find(|message| !message.is_empty())
            .map(|message| message.chars().take(4000).collect::<String>());
        let metadata = |key| {
            let value = text(primary, key);
            if value.is_empty() {
                text(runtime, key).to_owned()
            } else {
                value.to_owned()
            }
        };
        cards.push(json!({"owner":owner,"origin":issue(primary,owner),"additional":owned.iter().filter(|row|text(row,"id") != owner).map(|row|issue(row,owner)).collect::<Vec<_>>(),"agent_state":runtime["agent_state"].as_str().unwrap_or("unknown"),"last_activity_at":runtime["last_activity_at"],"last_assistant_message":message,"harness":metadata("harness"),"model":metadata("model")}));
    }
    let warning = if payload["factory_reachable"] == false {
        let state = text(&payload["host_vm"], "state");
        Some(format!(
            "SSF could not reach the guest factory{}",
            if state.is_empty() {
                String::new()
            } else {
                format!(" (VM {state})")
            }
        ))
    } else if payload["orca"]["available"] == false {
        let detail = text(&payload["orca"], "error").trim();
        Some(
            if detail.is_empty() {
                "SSF could not reach one or more session drivers"
            } else {
                detail
            }
            .to_owned(),
        )
    } else {
        None
    };
    Ok(
        json!({"cards":cards,"warning":warning,"refreshed_at":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs_f64()}),
    )
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
    fn presents_server_ownership_and_latest_message_safely() {
        let snapshot = presentation(&json!({"sessions":[
            {"id":"r#1","title":"Origin","active":false,"harness":"codex","url":"javascript:alert(1)"},
            {"id":"r#2","owner":"r#1","active":true,"agent_state":"working","last_activity_at":"2026-09-12T12:00:00Z","last_assistant_message":" Earlier "},
            {"id":"r#3","owner":"r#1","active":true,"agent_state":"idle","last_activity_at":"2026-09-12T13:00:00Z","last_assistant_message":" <script>latest</script> "},
            {"id":"r#4","owner":"r#4","active":true,"subscriber_only":true}
        ]})).unwrap();
        let cards = snapshot["cards"].as_array().unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["origin"]["title"], "Origin");
        assert!(cards[0]["origin"]["url"].is_null());
        assert_eq!(cards[0]["additional"].as_array().unwrap().len(), 2);
        assert_eq!(cards[0]["agent_state"], "idle");
        assert_eq!(cards[0]["harness"], "codex");
        assert_eq!(
            cards[0]["last_assistant_message"],
            "<script>latest</script>"
        );
        assert!(JS.contains(".textContent = card.last_assistant_message"));
        assert!(!JS.contains("innerHTML"));
    }

    #[test]
    fn distinguishes_unreachable_vm_and_driver_errors_from_empty_factory() {
        let snapshot = presentation(
            &json!({"sessions":[],"factory_reachable":false,"host_vm":{"state":"stopped"}}),
        )
        .unwrap();
        assert!(snapshot["warning"].as_str().unwrap().contains("VM stopped"));
        let snapshot = presentation(
            &json!({"sessions":[],"orca":{"available":false,"error":"driver unavailable"}}),
        )
        .unwrap();
        assert_eq!(snapshot["warning"], "driver unavailable");
        assert!(presentation(&json!({"sessions":[]})).unwrap()["warning"].is_null());
        assert!(presentation(&json!({"error":"not a snapshot"})).is_err());
        assert!(JS.contains("if (body.warning) emptyNode.hidden = true"));
        assert!(JS.contains("catch (error) {\n    emptyNode.hidden = true"));
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
        let task = tokio::spawn(serve(
            listener,
            "secret".into(),
            Duration::from_secs(2),
            move || {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n == 0 {
                        Ok(json!({"sessions":[]}))
                    } else {
                        bail!("remote server unreachable")
                    }
                }
            },
        ));
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
    async fn idle_listener_expires_even_with_incomplete_headers() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(serve(
            listener,
            "secret".into(),
            Duration::from_millis(80),
            || async { Ok(json!({"sessions":[]})) },
        ));
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(b"GET /").await.unwrap();
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(TcpStream::connect(address).await.is_err());
    }

    #[tokio::test]
    async fn only_status_polls_extend_idle_lifetime() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(serve(
            listener,
            "secret".into(),
            Duration::from_millis(250),
            || async { Ok(json!({"sessions":[]})) },
        ));
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            fetch(address, "/secret/api/status")
                .await
                .starts_with("HTTP/1.1 200")
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(fetch(address, "/secret/").await.starts_with("HTTP/1.1 200"));
        timeout(Duration::from_millis(200), task)
            .await
            .unwrap()
            .unwrap()
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
