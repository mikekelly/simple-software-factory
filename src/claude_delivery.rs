//! Claude's unofficial peer inbox protocol, verified against 2.1.268.
//! Wire format and token naming follow mikekelly/cc-peer/docs/PROTOCOL.md.
//! No terminal input and no ordinary socket receipt: reconcile ambiguous sends
//! against the target's persistent transcript, never blindly resend them.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

fn root() -> PathBuf {
    #[cfg(test)]
    {
        crate::config::test_support::home().join(".claude")
    }
    #[cfg(not(test))]
    {
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude"))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Registration {
    pid: u32,
    session_id: String,
    cwd: String,
    proc_start: String,
    peer_protocol: u32,
    messaging_socket_path: PathBuf,
}

pub(crate) struct Inbox {
    socket: PathBuf,
    token: String,
    transcript: PathBuf,
}

#[cfg(target_os = "linux")]
fn start_ticks(stat: &str) -> Option<&str> {
    // /proc/PID/stat field 22, after the parenthesized comm (which can contain spaces).
    stat.rsplit_once(") ")?.1.split_whitespace().nth(19)
}

async fn process_start(pid: u32, expected: &str) -> Option<String> {
    #[cfg(not(target_os = "linux"))]
    let _ = expected;
    #[cfg(target_os = "linux")]
    if expected.bytes().all(|b| b.is_ascii_digit()) {
        return start_ticks(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
            .map(str::to_owned);
    }
    let out = tokio::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .await
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn unattended(argv: &[Value]) -> bool {
    argv.iter()
        .any(|arg| arg.as_str() == Some("--dangerously-skip-permissions"))
        && argv.windows(2).any(|pair| {
            pair[0].as_str() == Some("--settings")
                && pair[1]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .is_some_and(|s| s["crossSessionInbound"] == "accept")
        })
}

/// Exact foreground PID, not cwd-based discovery: two sessions can share a checkout.
pub(crate) async fn discover(process_info: &Value) -> Option<Inbox> {
    let processes = process_info
        .pointer("/process_info/foreground_processes")?
        .as_array()?;
    let mut inbox = None;
    for process in processes {
        let argv = process["argv"].as_array()?;
        if !unattended(argv) {
            continue;
        }
        let pid = u32::try_from(process["pid"].as_u64()?).ok()?;
        let body = std::fs::read(root().join("sessions").join(format!("{pid}.json"))).ok()?;
        let reg: Registration = serde_json::from_slice(&body).ok()?;
        if reg.pid != pid
            || reg.peer_protocol != 1
            || reg.proc_start.is_empty()
            || process_start(pid, &reg.proc_start).await? != reg.proc_start
            || reg.session_id.is_empty()
            || !reg
                .session_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return None;
        }
        let socket = std::fs::canonicalize(&reg.messaging_socket_path).ok()?;
        let hash = format!(
            "{:x}",
            Sha256::digest(socket.as_os_str().as_encoded_bytes())
        );
        let key: Value = serde_json::from_slice(
            &std::fs::read(root().join("sessions").join(format!("{pid}.{hash}.key"))).ok()?,
        )
        .ok()?;
        if key["procStart"].as_str()? != reg.proc_start {
            return None;
        }
        let token = key["peerToken"].as_str()?.to_owned();
        if token.is_empty() {
            return None;
        }
        let encoded: String = reg
            .cwd
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let next = Inbox {
            socket,
            token,
            transcript: root()
                .join("projects")
                .join(encoded)
                .join(format!("{}.jsonl", reg.session_id)),
        };
        // Multiple foreground candidates are ambiguous; never select a neighbour.
        if inbox.is_some() {
            return None;
        }
        inbox = Some(next);
    }
    inbox
}

fn event_id(sequence: u64, text: &str) -> String {
    format!("{sequence:020}-{:x}", Sha256::digest(text.as_bytes()))
}

fn record_path(mailbox: &Path, sequence: u64, text: &str) -> PathBuf {
    mailbox.join(format!("claude-{}.json", event_id(sequence, text)))
}

pub(crate) fn has_record(mailbox: &Path, sequence: u64, text: &str) -> bool {
    record_path(mailbox, sequence, text).exists()
}

#[derive(Serialize, Deserialize)]
struct Record {
    transcript: PathBuf,
    content: String,
    confirmed: bool,
}

fn observed(record: &Record) -> bool {
    std::fs::read_to_string(&record.transcript)
        .ok()
        .is_some_and(|body| {
            body.lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|entry| {
                    (entry["type"] == "queue-operation"
                        && entry["operation"] == "enqueue"
                        && entry["content"].as_str() == Some(&record.content))
                        || (entry["type"] == "user"
                            && entry["origin"]["kind"] == "peer"
                            && entry["message"]["content"]
                                .as_str()
                                .is_some_and(|s| s.contains(&record.content)))
                })
        })
}

fn save(path: &Path, record: &Record) -> Result<()> {
    std::fs::create_dir_all(path.parent().context("delivery journal has no parent")?)?;
    let temporary = path.with_extension("tmp");
    let mut file = std::fs::File::create(&temporary)?;
    use std::io::Write;
    file.write_all(&serde_json::to_vec(record)?)?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    std::fs::File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

async fn confirm(path: &Path, mut record: Record) -> Result<()> {
    if !record.confirmed {
        for _ in 0..100 {
            if observed(&record) {
                record.confirmed = true;
                save(path, &record)?;
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        bail!(
            "Claude inbox delivery is ambiguous; retaining {} without resending or terminal fallback. Check its target transcript before resolving the journal",
            path.display()
        );
    }
    Ok(())
}

/// true = native delivery confirmed; false = unavailable before any send (fallback allowed).
pub(crate) async fn deliver(
    inbox: Option<&Inbox>,
    mailbox: &Path,
    sequence: u64,
    text: &str,
) -> Result<bool> {
    let path = record_path(mailbox, sequence, text);
    if path.exists() {
        let record = serde_json::from_slice(&std::fs::read(&path)?)?;
        confirm(&path, record).await?;
        return Ok(true);
    }
    let Some(inbox) = inbox else { return Ok(false) };
    let mut stream = match tokio::time::timeout(
        Duration::from_secs(2),
        UnixStream::connect(&inbox.socket),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        _ => return Ok(false),
    };
    let id = event_id(sequence, text);
    // No reply server is needed for accepted messages. Keep address in the target socket directory.
    let from = format!(
        "uds:{}/ssf-{}.sock",
        inbox
            .socket
            .parent()
            .context("inbox socket has no parent")?
            .display(),
        std::process::id()
    );
    let from: String = from
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b":_/.-\\".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let content = format!(
        "<cross-session-message from=\"{from}\" from-name=\"ssf\" from-mode=\"bypass\" delivery-id=\"{id}\">\n{text}\n</cross-session-message>"
    );
    let record = Record {
        transcript: inbox.transcript.clone(),
        content: content.clone(),
        confirmed: false,
    };
    let auth = json!({"type":"auth","token":inbox.token});
    let message_hash = format!("{:x}", Sha256::digest(id.as_bytes()));
    let frame = json!({"type":"user","message":{"role":"user","content":content},"from":from,"priority":"next","msgV":1,"msg_id":format!("cc-msg-{}", &message_hash[..32])});
    let wire = format!("{auth}\n{frame}\n");
    // Journal before the first byte: a crash or partial write is never retried blindly.
    save(&path, &record)?;
    let _ = tokio::time::timeout(Duration::from_secs(2), stream.write_all(wire.as_bytes())).await;
    let _ = stream.shutdown().await;
    confirm(&path, record).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::UnixListener;

    #[test]
    fn unattended_setting_is_required() {
        assert!(!unattended(&[json!("--dangerously-skip-permissions")]));
        assert!(!unattended(&[
            json!("--settings"),
            json!("{\"crossSessionInbound\":\"accept\"}")
        ]));
        assert!(unattended(&[
            json!("--dangerously-skip-permissions"),
            json!("--settings"),
            json!("{\"crossSessionInbound\":\"accept\"}")
        ]));
        assert!(!unattended(&[
            json!("--dangerously-skip-permissions"),
            json!("--settings"),
            json!("{\"crossSessionInbound\":\"hold\"}")
        ]));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn discovers_only_the_foreground_pid_and_rejects_reuse() {
        let sandbox = crate::config::test_support::sandbox();
        let pid = std::process::id();
        let start = process_start(pid, "0").await.unwrap();
        let socket = sandbox.root().join("inbox.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        let sessions = root().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let hash = format!(
            "{:x}",
            Sha256::digest(socket.as_os_str().as_encoded_bytes())
        );
        std::fs::write(
            sessions.join(format!("{pid}.{hash}.key")),
            json!({"peerToken":"test-token", "procStart":start}).to_string(),
        )
        .unwrap();
        let mut reg = json!({"pid":pid,"sessionId":"test-session","cwd":"/scratch/project","procStart":start,"peerProtocol":1,"messagingSocketPath":socket});
        let path = sessions.join(format!("{pid}.json"));
        std::fs::write(&path, reg.to_string()).unwrap();
        let info = json!({"process_info":{"foreground_processes":[{"pid":pid,"argv":["claude","--dangerously-skip-permissions","--settings","{\"crossSessionInbound\":\"accept\"}"]}]}});
        assert!(discover(&info).await.is_some());
        reg["procStart"] = json!("1");
        std::fs::write(&path, reg.to_string()).unwrap();
        assert!(discover(&info).await.is_none());
    }

    #[tokio::test]
    async fn sends_authenticated_frame_and_reconciles_without_resending() {
        let sandbox = crate::config::test_support::sandbox();
        let socket = sandbox.root().join("inbox.sock");
        let transcript = sandbox.root().join("transcript.jsonl");
        let mailbox = sandbox.root().join("mailbox");
        let listener = UnixListener::bind(&socket).unwrap();
        let inbox = Inbox {
            socket,
            transcript: transcript.clone(),
            token: "test-token".into(),
        };
        let receiver = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut lines = BufReader::new(stream).lines();
            let auth: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(auth, json!({"type":"auth","token":"test-token"}));
            let frame: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(frame["priority"], "next");
            assert_eq!(frame["message"]["role"], "user");
            assert!(
                frame["message"]["content"]
                    .as_str()
                    .unwrap()
                    .contains("[ssf] event")
            );
            std::fs::write(transcript, json!({"type":"queue-operation","operation":"enqueue","content":frame["message"]["content"]}).to_string()).unwrap();
            // No second socket connection is allowed during reconciliation.
            assert!(
                tokio::time::timeout(Duration::from_millis(300), listener.accept())
                    .await
                    .is_err()
            );
        });
        assert!(
            deliver(Some(&inbox), &mailbox, 1, "[ssf] event")
                .await
                .unwrap()
        );
        let path = record_path(&mailbox, 1, "[ssf] event");
        let mut record: Record = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        record.confirmed = false; // Simulate receipt before the daemon saved confirmation.
        save(&path, &record).unwrap();
        assert!(
            deliver(Some(&inbox), &mailbox, 1, "[ssf] event")
                .await
                .unwrap()
        );
        assert!(deliver(None, &mailbox, 1, "[ssf] event").await.unwrap());
        receiver.await.unwrap();
    }

    #[tokio::test]
    async fn unavailable_before_send_allows_fallback_without_journal() {
        let sandbox = crate::config::test_support::sandbox();
        let mailbox = sandbox.root().join("mailbox");
        let inbox = Inbox {
            socket: sandbox.root().join("missing.sock"),
            token: "test".into(),
            transcript: sandbox.root().join("missing.jsonl"),
        };
        assert!(!deliver(Some(&inbox), &mailbox, 1, "event").await.unwrap());
        assert!(!has_record(&mailbox, 1, "event"));
    }

    #[tokio::test]
    async fn ambiguous_journal_never_falls_back_or_resends() {
        let sandbox = crate::config::test_support::sandbox();
        let mailbox = sandbox.root().join("mailbox");
        let path = record_path(&mailbox, 1, "event");
        save(
            &path,
            &Record {
                transcript: sandbox.root().join("missing.jsonl"),
                content: "event".into(),
                confirmed: false,
            },
        )
        .unwrap();
        assert!(
            deliver(None, &mailbox, 1, "event")
                .await
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        assert!(path.exists());
    }

    /// Existing scratch pane only; leaves the harness and its transcript for inspection.
    /// SSF_CLAUDE_TEST_PANE=wN:pN cargo test claude_live_inbox -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn claude_live_inbox() {
        let _machine = crate::config::test_support::the_machine_itself();
        let pane =
            std::env::var("SSF_CLAUDE_TEST_PANE").expect("provide an isolated scratch Claude pane");
        let text = std::env::var("SSF_CLAUDE_TEST_EVENT").unwrap_or_else(|_| {
            "[ssf] Compiled native event 334. Reply COMPILED-334-OK; do not use tools.".into()
        });
        let sequence = std::env::var("SSF_CLAUDE_TEST_SEQUENCE")
            .unwrap_or_else(|_| "1".into())
            .parse()
            .unwrap();
        let mailbox = PathBuf::from(
            std::env::var("SSF_CLAUDE_TEST_MAILBOX").expect("provide a temporary mailbox"),
        );
        let herdr = crate::herdr::Herdr::new(crate::config::HerdrConfig::default());
        let inbox = herdr.claude_inbox(&pane).await.expect("live native inbox");
        let agent = herdr
            .agents()
            .await
            .unwrap()
            .into_iter()
            .find(|agent| agent.pane_id == pane)
            .unwrap();
        let relaunch = crate::driver::Relaunch {
            command: "unused",
            resume_command: None,
            harness: "claude",
            title: "unused",
            text: None,
            first_prompt: crate::driver::FirstPrompt::No,
            channel: Some((&mailbox, sequence)),
        };
        herdr
            .deliver(&agent.workspace_id, Some(&pane), &relaunch, &text)
            .await
            .unwrap();
        herdr
            .deliver(&agent.workspace_id, Some(&pane), &relaunch, &text)
            .await
            .unwrap();
        let body = std::fs::read_to_string(&inbox.transcript).unwrap();
        let count = body
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|entry| {
                entry["type"] == "queue-operation"
                    && entry["operation"] == "enqueue"
                    && entry["content"]
                        .as_str()
                        .is_some_and(|s| s.contains(&event_id(sequence, &text)))
            })
            .count();
        assert_eq!(count, 1, "one persistent enqueue per event");
        assert!(
            herdr
                .run_raw(&["pane", "read", &pane, "--format", "text"])
                .await
                .unwrap()
                .contains("HUMAN-DRAFT-334")
        );
    }
}
