//! Requests from the CLI to the running daemon (`ssf sub|unsub`,
//! `ssf release|purge`), over a Unix socket in the state directory.
//!
//! The daemon keeps the state in memory and writes it out wholesale, so
//! anything that changes it (a subscription) or needs its delivery path (a
//! handover summary, an assignment's overrides) has to go through the
//! daemon rather than edit the state file. The protocol is one JSON line
//! each way; the daemon answers between polls.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::config::state_dir;

/// Longest a client waits for the daemon's answer: a tick that has to
/// relaunch a harness can take a while.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(180);
/// Longest the daemon waits on a client to send or take a line.
pub const SERVER_IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_LINE: usize = 1 << 20;

/// Longest summary `ssf handover` carries to the new session: enough for
/// a handover note, short of a prompt nothing can read.
pub const MAX_SUMMARY_CHARS: usize = 8_000;

/// Longest socket path the kernel takes (`sun_path`), with room for the NUL.
const MAX_SOCKET_PATH: usize = 107;

/// `ssf.sock` in the state directory, so it sits next to the state it
/// guards; when that path is too long for a Unix socket, a name derived
/// from the state directory under `$XDG_RUNTIME_DIR` instead.
pub fn socket_path() -> PathBuf {
    let dir = state_dir();
    let p = dir.join("ssf.sock");
    if p.as_os_str().len() <= MAX_SOCKET_PATH {
        return p;
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    dir.hash(&mut h);
    runtime.join(format!("ssf-{:016x}.sock", h.finish()))
}

/// Whether a daemon answers on this factory's socket. A connect is enough:
/// only a live listener accepts one, and the socket a dead daemon left
/// behind refuses it. This is the daemon's own liveness, the one fact about
/// the factory that does not depend on whatever supervises it -- a systemd
/// unit, another supervisor, or a person at a shell (#463).
pub fn daemon_reachable() -> bool {
    std::os::unix::net::UnixStream::connect(socket_path()).is_ok()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// List pre-existing allocations waiting for explicit adoption.
    Candidates {
        repo: Option<String>,
    },
    /// Adopt explicitly named pre-existing allocations on this factory.
    Adopt {
        items: Vec<String>,
    },
    /// `from` (a session, `owner/repo#N`) wants to hear about `target`, at
    /// this level (following it again changes the level).
    Sub {
        from: String,
        target: String,
        #[serde(default)]
        events: crate::state::Events,
    },
    Unsub {
        from: String,
        target: String,
    },
    /// Remove the workspace of `session` (`owner/repo#N`) once the checks
    /// in `crate::release` pass; `force` skips them.
    Release {
        session: String,
        force: bool,
    },
    /// Hand `session`'s item to a new session on `harness` (with `model`
    /// and `effort` when given) in the same workspace, with `summary` as
    /// the new agent's first message. `by` is the session that asked, or
    /// `None` for a person at a shell.
    Handover {
        session: String,
        harness: String,
        model: Option<String>,
        effort: Option<String>,
        summary: Option<String>,
        by: Option<String>,
    },
    /// Drop the handover recorded on `session`'s item before the daemon
    /// has carried it out; the session that is there keeps the item.
    CancelHandover {
        session: String,
    },
    /// Assign the bot to `item` (`owner/repo#N`) on GitHub and write the
    /// item's launch overrides, so the session that onboards it comes up
    /// on `harness`/`model`/`effort` rather than on the repository's
    /// stack. The inverse of `Handover`, which wants an item that already
    /// has a session. `by` is the session that asked, or `None` for a
    /// person at a shell.
    Assign {
        item: String,
        harness: String,
        model: Option<String>,
        effort: Option<String>,
        by: Option<String>,
    },
    /// List, and unless `dry_run` remove, the workspaces of closed items
    /// whose agent is gone: the clean-and-pushed ones, or all of them with
    /// `force`. `older_than_days` keeps recently retired ones out of it.
    Purge {
        dry_run: bool,
        older_than_days: Option<u64>,
        force: bool,
    },
    /// Start a scratch session on `repo` (`owner/name`): an agent session
    /// that works on no item, in a worktree of its own, on this stack.
    /// Shared without `owner_login`; that GitHub user's with it.
    ScratchCreate {
        repo: String,
        harness: String,
        model: Option<String>,
        effort: Option<String>,
        owner_login: Option<String>,
    },
    /// Bring a killed scratch session (`owner/repo~id`) back: its workspace
    /// re-created and its harness conversation resumed.
    ScratchResume {
        session: String,
    },
    Ping,
}

/// What a refusal is about, for a caller that has its own vocabulary for it
/// rather than only the daemon's message: the web API answers `bad_input`
/// with `400` and `conflict` with `409` (`src/dashboard_web.rs`). `ssf`'s own
/// commands print the message like any other error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalKind {
    /// The request named something ssf does not have: about the input
    /// itself, and the same request with other input could work.
    BadInput,
    /// The request was understood and the item is not in a state it can be
    /// applied to.
    Conflict,
}

/// A refusal carrying its [`RefusalKind`] up through `anyhow`, so the
/// connection that answers a request can tell it from a failure.
#[derive(Debug)]
pub struct Refused {
    pub kind: RefusalKind,
    pub message: String,
}

impl Refused {
    pub fn bad_input(message: impl Into<String>) -> Self {
        Self {
            kind: RefusalKind::BadInput,
            message: message.into(),
        }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            kind: RefusalKind::Conflict,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refused {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Whether the error was a [`Refused`], and what it is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<RefusalKind>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

impl Response {
    pub fn ok(data: Value) -> Self {
        Self {
            ok: true,
            error: None,
            kind: None,
            data,
        }
    }

    pub fn err(e: impl std::fmt::Display) -> Self {
        Self {
            ok: false,
            error: Some(e.to_string()),
            kind: None,
            data: Value::Null,
        }
    }

    /// [`Response::err`] for an error that carries a [`Refused`] anywhere in
    /// its chain.
    pub fn refused(e: &anyhow::Error) -> Self {
        Self {
            ok: false,
            error: Some(format!("{e:#}")),
            kind: e
                .chain()
                .find_map(|cause| cause.downcast_ref::<Refused>())
                .map(|refused| refused.kind),
            data: Value::Null,
        }
    }
}

/// Send one request to the daemon and wait for its answer.
pub async fn call(req: &Request) -> Result<Value> {
    let resp = exchange(req).await?;
    if resp.ok {
        Ok(resp.data)
    } else {
        bail!("{}", resp.error.unwrap_or_else(|| "request failed".into()))
    }
}

/// [`call`] with the daemon's answer as it came: a refusal is an answer
/// rather than a transport failure, so a caller that acts on what the refusal
/// was about reads it here.
pub async fn exchange(req: &Request) -> Result<Response> {
    let path = socket_path();
    let stream = match UnixStream::connect(&path).await {
        Ok(s) => s,
        Err(e) => bail!(
            "the ssf daemon is not running ({}: {e}); start it with `ssf ui service enable` or `ssf-server`",
            path.display()
        ),
    };
    let (rd, mut wr) = stream.into_split();
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    wr.write_all(line.as_bytes())
        .await
        .context("sending the request to the daemon")?;
    let mut reader = BufReader::new(rd).take(MAX_LINE as u64);
    let mut answer = String::new();
    let n = tokio::time::timeout(CLIENT_TIMEOUT, reader.read_line(&mut answer))
        .await
        .context("the daemon did not answer in time")?
        .context("reading the daemon's answer")?;
    if n == 0 {
        bail!("the daemon closed the connection without answering");
    }
    serde_json::from_str(answer.trim()).context("parsing the daemon's answer")
}

/// Read one request line from a connection the daemon accepted. `None` is a
/// peer that connected and closed without sending anything, which is what a
/// liveness probe is ([`daemon_reachable`]): there is no request to answer
/// and nothing to report.
pub async fn read_request(stream: &mut UnixStream) -> Result<Option<Request>> {
    let mut reader = BufReader::new(stream).take(MAX_LINE as u64);
    let mut line = String::new();
    let n = tokio::time::timeout(SERVER_IO_TIMEOUT, reader.read_line(&mut line))
        .await
        .context("client sent nothing in time")?
        .context("reading request")?;
    if n == 0 {
        return Ok(None);
    }
    serde_json::from_str(line.trim())
        .context("parsing request")
        .map(Some)
}

pub async fn write_response(stream: &mut UnixStream, resp: &Response) -> Result<()> {
    let mut line = serde_json::to_string(resp)?;
    line.push('\n');
    tokio::time::timeout(SERVER_IO_TIMEOUT, stream.write_all(line.as_bytes()))
        .await
        .context("client did not take the answer in time")?
        .context("writing response")?;
    let _ = stream.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_state_dirs_move_the_socket_to_the_runtime_dir() {
        let short = PathBuf::from("/tmp/x").join("ssf.sock");
        assert!(short.as_os_str().len() <= MAX_SOCKET_PATH);
        // The real check needs the env; just make sure the fallback name is
        // stable and short.
        let a = PathBuf::from("/run/user/1000").join(format!("ssf-{:016x}.sock", 1u64));
        assert!(a.as_os_str().len() <= MAX_SOCKET_PATH);
    }

    #[test]
    fn a_listening_socket_is_reachable_and_a_stale_one_is_not() {
        let _sandbox = crate::config::test_support::sandbox();
        // Nothing is listening yet.
        assert!(!daemon_reachable());
        let path = socket_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(daemon_reachable());
        // A daemon that stops leaves its socket file behind, and connecting
        // to that is refused rather than answered.
        // Another test thread that forks while the listener is open hands
        // the child a copy until it execs, so give that copy time to close.
        drop(listener);
        assert!(path.exists());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while daemon_reachable() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!daemon_reachable());
    }

    /// A peer that connects and closes without a request is how the
    /// dashboards ask whether the daemon is there ([`daemon_reachable`]):
    /// the daemon reads no request and answers nothing, so the probe
    /// neither logs a refusal nor blocks the next real client.
    #[tokio::test]
    async fn a_connection_that_sends_nothing_is_not_a_request() {
        let dir = std::env::temp_dir().join(format!("ssf-ipc-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("probe.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        drop(std::os::unix::net::UnixStream::connect(&path).unwrap());
        let (mut peer, _) = listener.accept().await.unwrap();
        assert!(read_request(&mut peer).await.unwrap().is_none());
        // A client that does send a request is read as one.
        let mut client = tokio::net::UnixStream::connect(&path).await.unwrap();
        client.write_all(b"{\"op\":\"ping\"}\n").await.unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        assert_eq!(read_request(&mut peer).await.unwrap(), Some(Request::Ping));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn requests_round_trip_as_tagged_json() {
        let p = Request::Purge {
            dry_run: true,
            older_than_days: Some(7),
            force: false,
        };
        let j = serde_json::to_string(&p).unwrap();
        assert!(j.contains("\"op\":\"purge\""));
        assert_eq!(serde_json::from_str::<Request>(&j).unwrap(), p);
        let h = Request::Handover {
            session: "o/r#1".into(),
            harness: "pi".into(),
            model: Some("openai/gpt-6".into()),
            effort: None,
            summary: Some("what is done, what is left".into()),
            by: Some("o/r#1".into()),
        };
        let j = serde_json::to_string(&h).unwrap();
        assert!(j.contains("\"op\":\"handover\""));
        assert_eq!(serde_json::from_str::<Request>(&j).unwrap(), h);
        let c = Request::CancelHandover {
            session: "o/r#1".into(),
        };
        let j = serde_json::to_string(&c).unwrap();
        assert!(j.contains("\"op\":\"cancel_handover\""));
        assert_eq!(serde_json::from_str::<Request>(&j).unwrap(), c);
        let s = Request::Assign {
            item: "o/r#2".into(),
            harness: "pi".into(),
            model: Some("openai/gpt-6".into()),
            effort: Some("high".into()),
            by: Some("o/r#1".into()),
        };
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("\"op\":\"assign\""));
        assert_eq!(serde_json::from_str::<Request>(&j).unwrap(), s);
        let a = Request::Adopt {
            items: vec!["o/r#3".into(), "x/y#4".into()],
        };
        let j = serde_json::to_string(&a).unwrap();
        assert!(j.contains("\"op\":\"adopt\""));
        assert_eq!(serde_json::from_str::<Request>(&j).unwrap(), a);
        // A follow says what it wants to hear; a request from a client that
        // predates the field still means the default level (#453).
        let sub = Request::Sub {
            from: "o/r#1".into(),
            target: "o/r#3".into(),
            events: crate::state::Events::All,
        };
        let j = serde_json::to_string(&sub).unwrap();
        assert!(j.contains("\"op\":\"sub\"") && j.contains("\"events\":\"all\""));
        assert_eq!(serde_json::from_str::<Request>(&j).unwrap(), sub);
        let older: Request =
            serde_json::from_str(r#"{"op":"sub","from":"o/r#1","target":"o/r#3"}"#).unwrap();
        assert_eq!(
            older,
            Request::Sub {
                from: "o/r#1".into(),
                target: "o/r#3".into(),
                events: crate::state::Events::State,
            }
        );
        let e = serde_json::to_string(&Response::err("nope")).unwrap();
        let back: Response = serde_json::from_str(&e).unwrap();
        assert!(!back.ok);
        assert_eq!(back.error.as_deref(), Some("nope"));
        // A refusal carries what it is about, and one answered by a daemon
        // that predates the field still reads.
        let refused =
            Response::refused(&anyhow::anyhow!(Refused::conflict("already has a session")));
        assert_eq!(refused.error.as_deref(), Some("already has a session"));
        assert_eq!(refused.kind, Some(RefusalKind::Conflict));
        let back: Response =
            serde_json::from_str(&serde_json::to_string(&refused).unwrap()).unwrap();
        assert_eq!(back.kind, Some(RefusalKind::Conflict));
        let older: Response = serde_json::from_str(r#"{"ok":false,"error":"nope"}"#).unwrap();
        assert_eq!(older.kind, None);
        assert_eq!(
            Response::refused(&anyhow::anyhow!("broken")).kind,
            None,
            "only a Refused is a refusal"
        );
    }
}
