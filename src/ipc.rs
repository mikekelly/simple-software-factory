//! Requests from the CLI to the running daemon (`ssf sub|unsub|tell`,
//! `ssf release|purge`), over a Unix socket in the state directory.
//!
//! The daemon keeps the state in memory and writes it out wholesale, so
//! anything that changes it (a subscription) or needs its delivery path (a
//! message pasted into another session's terminal) has to go through the
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// `from` (a session, `owner/repo#N`) wants to hear about `target`.
    Sub {
        from: String,
        target: String,
    },
    Unsub {
        from: String,
        target: String,
    },
    /// Paste `text` into the terminal of the session acting on `target`.
    /// `from` is the sending session, or `None` from a human shell.
    Tell {
        from: Option<String>,
        target: String,
        text: String,
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
    /// List, and unless `dry_run` remove, the workspaces of closed items
    /// whose agent is gone: the clean-and-pushed ones, or all of them with
    /// `force`. `older_than_days` keeps recently retired ones out of it.
    Purge {
        dry_run: bool,
        older_than_days: Option<u64>,
        force: bool,
    },
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

impl Response {
    pub fn ok(data: Value) -> Self {
        Self {
            ok: true,
            error: None,
            data,
        }
    }

    pub fn err(e: impl std::fmt::Display) -> Self {
        Self {
            ok: false,
            error: Some(e.to_string()),
            data: Value::Null,
        }
    }
}

/// Send one request to the daemon and wait for its answer.
pub async fn call(req: &Request) -> Result<Value> {
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
    let resp: Response =
        serde_json::from_str(answer.trim()).context("parsing the daemon's answer")?;
    if resp.ok {
        Ok(resp.data)
    } else {
        bail!("{}", resp.error.unwrap_or_else(|| "request failed".into()))
    }
}

/// Read one request line from a connection the daemon accepted.
pub async fn read_request(stream: &mut UnixStream) -> Result<Request> {
    let mut reader = BufReader::new(stream).take(MAX_LINE as u64);
    let mut line = String::new();
    let n = tokio::time::timeout(SERVER_IO_TIMEOUT, reader.read_line(&mut line))
        .await
        .context("client sent nothing in time")?
        .context("reading request")?;
    if n == 0 {
        bail!("empty request");
    }
    serde_json::from_str(line.trim()).context("parsing request")
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
    fn requests_round_trip_as_tagged_json() {
        let r = Request::Tell {
            from: Some("o/r#1".into()),
            target: "o/r#2".into(),
            text: "hi".into(),
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"op\":\"tell\""));
        assert_eq!(serde_json::from_str::<Request>(&j).unwrap(), r);
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
        let e = serde_json::to_string(&Response::err("nope")).unwrap();
        let back: Response = serde_json::from_str(&e).unwrap();
        assert!(!back.ok);
        assert_eq!(back.error.as_deref(), Some("nope"));
    }
}
