//! Opt-in delivery to the app-server used by an explicitly attached Codex TUI.
//! The server and terminal lifecycle belong to the launcher/Herdr, not this module.
//! A provider response is admission, not receipt; the durable user-message echo
//! confirms the exact event. Never resend an ambiguous native write.

use anyhow::{Context, Result, bail};
use futures_util::future::BoxFuture;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

use crate::herdr::{Channel, Herdr, Journal};

const TIMEOUT: Duration = Duration::from_secs(5);

struct Rpc {
    stream: WebSocketStream<UnixStream>,
    next: u64,
}

impl Rpc {
    async fn connect(socket: &Path) -> Result<Self> {
        let metadata = std::fs::metadata(socket)?;
        // Only a same-user private local endpoint qualifies. No public TCP listeners.
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            bail!("Codex app-server socket must be owned by this user with mode 0600");
        }
        let stream = tokio::time::timeout(TIMEOUT, UnixStream::connect(socket)).await??;
        if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
            bail!("Codex app-server peer belongs to another user");
        }
        let (stream, _) = tokio::time::timeout(
            TIMEOUT,
            tokio_tungstenite::client_async_with_config(
                "ws://localhost/rpc",
                stream,
                Some(WebSocketConfig::default().max_message_size(Some(16 << 20))),
            ),
        )
        .await??;
        let mut rpc = Self { stream, next: 1 };
        rpc.request(
            "initialize",
            json!({"clientInfo":{"name":"ssf","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}),
        )
        .await?;
        rpc.stream
            .send(Message::Text(
                json!({"method":"initialized"}).to_string().into(),
            ))
            .await?;
        Ok(rpc)
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next;
        self.next += 1;
        tokio::time::timeout(TIMEOUT, async {
            self.stream
                .send(Message::Text(
                    json!({"id":id,"method":method,"params":params})
                        .to_string()
                        .into(),
                ))
                .await?;
            while let Some(message) = self.stream.next().await {
                if let Message::Text(text) = message? {
                    let response: Value = serde_json::from_str(&text)?;
                    if response["id"] == id {
                        if !response["error"].is_null() {
                            bail!("Codex {method} rejected: {}", response["error"]);
                        }
                        return Ok(response["result"].clone());
                    }
                    // Do not answer dialogs or unrelated server requests.
                }
            }
            bail!("Codex app-server disconnected during {method}")
        })
        .await?
    }

    async fn sole_thread(&mut self) -> Result<Value> {
        let loaded = self.request("thread/loaded/list", json!({})).await?;
        if !loaded["nextCursor"].is_null() {
            bail!("Codex endpoint has too many loaded sessions; use an item-specific server");
        }
        let ids = loaded["data"]
            .as_array()
            .context("Codex loaded session list is invalid")?;
        let mut ordinary = Vec::new();
        for id in ids {
            let value = self
                .request("thread/read", json!({"threadId":id,"includeTurns":false}))
                .await?;
            let thread = &value["thread"];
            // Subagents do not own the TUI; multiple ordinary conversations do.
            if thread["parentThreadId"].is_null()
                && !thread["source"]
                    .as_object()
                    .is_some_and(|s| s.contains_key("subAgent"))
            {
                ordinary.push(thread.clone());
            }
        }
        if ordinary.len() != 1 {
            bail!(
                "Codex endpoint must have exactly one ordinary loaded conversation; refusing to guess the TUI session"
            );
        }
        Ok(ordinary.remove(0))
    }
}

/// The configuration overrides ssf itself puts on a codex command line, and
/// the only ones this channel accepts: an override ssf did not write is one it
/// has not checked against the codex that will run, and a native delivery is
/// not the place to find out what it does. Both of these are ssf's own — the
/// effort level (`models::codex_effort`) and the context-compaction threshold
/// (`models::codex_compaction`), which is appended to an operator's
/// `repo.command` too, so a native-delivery launcher carries it.
const VERIFIED_OVERRIDES: &[&str] = &["model_reasoning_effort=", "model_auto_compact_token_limit="];

/// None is a standalone TUI: terminal fallback remains available. An explicit
/// remote launch that fails validation is held, never silently pasted into.
fn endpoint(info: &Value) -> Result<Option<(PathBuf, PathBuf)>> {
    let processes = info
        .pointer("/process_info/foreground_processes")
        .and_then(Value::as_array)
        .context("Codex foreground process information unavailable")?;
    let mut found = None;
    for process in processes {
        let Some(argv) = process["argv"].as_array() else {
            continue;
        };
        let args: Vec<_> = argv.iter().filter_map(Value::as_str).collect();
        if args
            .first()
            .and_then(|arg| Path::new(arg).file_name())
            .is_none_or(|name| name != "codex")
        {
            continue;
        }
        let remote: Vec<_> = args
            .iter()
            .enumerate()
            .filter_map(|(i, arg)| {
                if *arg == "--remote" {
                    Some(args.get(i + 1).copied().unwrap_or(""))
                } else {
                    arg.strip_prefix("--remote=")
                }
            })
            .collect();
        if remote.is_empty() {
            continue;
        }
        if remote.len() != 1 || found.is_some() {
            bail!("Codex remote endpoint is ambiguous");
        }
        if (!args.contains(&"--dangerously-bypass-approvals-and-sandbox")
            && !args
                .windows(2)
                .any(|a| a[0] == "resume" && !a[1].starts_with('-')))
            || !args.contains(&"--dangerously-bypass-hook-trust")
            || args.iter().any(|a| {
                matches!(
                    *a,
                    "--sandbox" | "-s" | "--profile" | "-p" | "--approve-for-me"
                ) || a.starts_with("--sandbox=")
                    || a.starts_with("--profile=")
                    || (a.starts_with("-s") && !a.starts_with("--"))
                    || (a.starts_with("-p") && !a.starts_with("--"))
            })
        {
            bail!("Codex native delivery requires the SSF unattended launch posture");
        }
        for (index, arg) in args.iter().enumerate() {
            let override_value = if *arg == "-c" || *arg == "--config" {
                Some(args.get(index + 1).copied().unwrap_or(""))
            } else if let Some(value) = arg.strip_prefix("--config=") {
                Some(value)
            } else if arg.starts_with("-c") && !arg.starts_with("--") {
                Some(&arg[2..])
            } else {
                None
            };
            if override_value.is_some_and(|v| !VERIFIED_OVERRIDES.iter().any(|k| v.starts_with(k)))
            {
                bail!("Codex native delivery refuses unverified configuration overrides");
            }
        }
        let raw = remote[0].strip_prefix("unix://").context(
            "Codex native delivery currently requires an explicit private Unix endpoint",
        )?;
        if !Path::new(raw).is_absolute() {
            bail!("Codex native delivery requires an explicit absolute Unix socket path");
        }
        let socket = std::fs::canonicalize(raw)?;
        let cwd = std::fs::canonicalize(
            process["cwd"]
                .as_str()
                .context("Codex process cwd unavailable")?,
        )?;
        found = Some((socket, cwd));
    }
    Ok(found)
}

#[derive(Clone, Deserialize, Serialize)]
struct Binding {
    socket: PathBuf,
    cwd: PathBuf,
    thread: String,
    transcript: PathBuf,
}

async fn bind(info: &Value, mailbox: &Path, persist: bool) -> Result<Option<(Binding, Rpc)>> {
    let Some((socket, cwd)) = endpoint(info)? else {
        return Ok(None);
    };
    let mut rpc = Rpc::connect(&socket).await?;
    let thread = rpc.sole_thread().await?;
    let id = thread["id"]
        .as_str()
        .context("Codex conversation has no ID")?;
    let thread_cwd = std::fs::canonicalize(
        thread["cwd"]
            .as_str()
            .context("Codex conversation has no cwd")?,
    )?;
    if thread_cwd != cwd {
        bail!("Codex conversation does not belong to this pane's directory")
    }
    let path = mailbox.join("codex-binding.json");
    let processes = info["process_info"]["foreground_processes"]
        .as_array()
        .unwrap();
    let resumes: Vec<&str> = processes
        .iter()
        .filter_map(|p| p["argv"].as_array())
        .flat_map(|args| args.windows(2))
        .filter(|a| a[0] == "resume")
        .filter_map(|a| a[1].as_str())
        .collect();
    if !resumes.is_empty() && (!path.exists() || resumes != [id]) {
        bail!("Codex remote resume requires the item's existing exact native binding");
    }
    let binding = if path.exists() {
        let binding: Binding = serde_json::from_slice(&std::fs::read(&path)?)?;
        if binding.socket != socket || binding.cwd != cwd || binding.thread != id {
            bail!(
                "Codex endpoint or conversation changed; retaining the item's saved binding without redirecting delivery"
            );
        }
        binding
    } else {
        let transcript = PathBuf::from(
            thread["path"]
                .as_str()
                .context("Codex conversation has no persistent transcript")?,
        );
        if !transcript.is_absolute() {
            bail!("Codex transcript path is not absolute")
        }
        let binding = Binding {
            socket,
            cwd,
            thread: id.into(),
            transcript,
        };
        if persist {
            save(&path, &binding)?
        }
        binding
    };
    Ok(Some((binding, rpc)))
}

pub(crate) async fn available(info: &Value, mailbox: &Path) -> Result<bool> {
    if endpoint(info)?.is_none() && mailbox.join("codex-binding.json").exists() {
        bail!("Codex item's saved native binding no longer has an attached pane");
    }
    Ok(bind(info, mailbox, false).await?.is_some())
}

#[derive(Deserialize, Serialize)]
struct Record {
    binding: Binding,
    id: String,
    text: String,
    confirmed: bool,
}

fn record_path(mailbox: &Path, sequence: u64, text: &str) -> PathBuf {
    mailbox.join(format!(
        "codex-{sequence:020}-{:x}.json",
        Sha256::digest(text.as_bytes())
    ))
}

pub(crate) fn has_record(mailbox: &Path, sequence: u64, text: &str) -> bool {
    record_path(mailbox, sequence, text).exists()
}

pub(crate) fn has_binding(mailbox: &Path) -> bool {
    mailbox.join("codex-binding.json").exists()
}

/// Retire only the active routing binding; event journals retain their own
/// snapshots so ambiguous writes can still be reconciled against the old task.
pub(crate) fn retire_binding(mailbox: &Path) -> Result<()> {
    let path = mailbox.join("codex-binding.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let archive = mailbox.join(format!(
        "codex-binding-retired-{:x}.json",
        Sha256::digest(&bytes)
    ));
    std::fs::rename(path, archive)?;
    std::fs::File::open(mailbox)?.sync_all()?;
    Ok(())
}

fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    std::fs::create_dir_all(path.parent().context("Codex journal has no parent")?)?;
    let temporary = path.with_extension("tmp");
    use std::io::Write;
    let mut file = std::fs::File::create(&temporary)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    std::fs::File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

fn observed(record: &Record) -> bool {
    std::fs::read_to_string(&record.binding.transcript)
        .ok()
        .is_some_and(|body| {
            body.lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|value| {
                    let event = &value["payload"];
                    event["type"] == "item_completed"
                        && event["thread_id"] == record.binding.thread
                        && event["item"]["type"] == "UserMessage"
                        && event["item"]["client_id"] == record.id
                        && event["item"]["content"].as_array().is_some_and(|parts| {
                            parts.len() == 1
                                && parts[0]["type"] == "text"
                                && parts[0]["text"] == record.text
                        })
                })
        })
}

async fn confirm(path: &Path, mut record: Record) -> Result<()> {
    if record.confirmed {
        return Ok(());
    }
    for _ in 0..100 {
        if observed(&record) {
            record.confirmed = true;
            save(path, &record)?;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    bail!(
        "Codex native delivery awaiting its durable user-message echo; retaining {} without resending or terminal fallback",
        path.display()
    )
}

pub(crate) async fn deliver(
    info: Option<&Value>,
    mailbox: &Path,
    sequence: u64,
    text: &str,
) -> Result<bool> {
    let path = record_path(mailbox, sequence, text);
    if path.exists() {
        confirm(&path, serde_json::from_slice(&std::fs::read(&path)?)?).await?;
        return Ok(true);
    }
    let info = info.context("Codex native delivery has no live target")?;
    let Some((binding, mut rpc)) = bind(info, mailbox, true).await? else {
        if mailbox.join("codex-binding.json").exists() {
            bail!(
                "Codex item's native binding exists but its pane no longer has the remote channel"
            )
        }
        return Ok(false);
    };
    let id = format!("ssf-{sequence:020}-{:x}", Sha256::digest(text.as_bytes()));
    let record = Record {
        binding,
        id,
        text: text.into(),
        confirmed: false,
    };
    // Persist intent before the first event byte; every subsequent attempt only
    // reconciles this receipt, including disconnects and daemon/harness restarts.
    save(&path, &record)?;
    let admitted = rpc
        .request(
            "turn/start",
            json!({
                "threadId":record.binding.thread,
                "clientUserMessageId":record.id,
                "input":[{"type":"text","text":text}],
                "approvalPolicy":"never", "sandboxPolicy":{"type":"dangerFullAccess"}
            }),
        )
        .await;
    drop(rpc);
    // Admission during active work is coalesced; it may not reach a model
    // boundary within this pass. The next pass reconciles rather than resends.
    confirm(&path, record)
        .await
        .with_context(|| match admitted {
            Ok(_) => "Codex admitted the event; waiting for durable receipt".to_owned(),
            Err(error) => format!("Codex admission outcome is uncertain: {error:#}"),
        })?;
    Ok(true)
}

/// Codex's delivery channel: the app-server an explicitly attached TUI
/// (`--remote unix://PATH`) runs against, with the terminal as the fallback
/// for a standalone TUI.
pub(crate) struct Codex;

impl Channel for Codex {
    fn session_bound(&self) -> bool {
        true
    }

    fn deliver<'a>(
        &'a self,
        herdr: &'a Herdr,
        pane: &'a str,
        journal: Journal<'a>,
        text: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let (mailbox, sequence) = journal.context("Codex delivery has no journal")?;
            let info = herdr.run(&["pane", "process-info", "--pane", pane]).await?;
            if !deliver(Some(&info), mailbox, sequence, text).await? {
                herdr.send_prompt(pane, text).await?;
            }
            Ok(())
        })
    }

    fn has_record(&self, mailbox: &Path, sequence: u64, text: &str) -> bool {
        has_record(mailbox, sequence, text)
    }

    fn has_binding(&self, mailbox: &Path) -> bool {
        has_binding(mailbox)
    }

    /// An event on record is reconciled only through the resumed session's
    /// native channel. A resumed session with nothing on record is offered
    /// its native channel first, and a standalone one gets the terminal.
    fn relaunched<'a>(
        &'a self,
        herdr: &'a Herdr,
        pane: &'a str,
        journal: Journal<'a>,
        recorded: bool,
        resumed: bool,
        text: &'a str,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            if let Some((mailbox, sequence)) = journal.filter(|_| recorded) {
                if !herdr.codex_channel_available(pane, mailbox).await? {
                    bail!("Codex resumed without its native channel; refusing terminal fallback");
                }
                deliver(None, mailbox, sequence, text).await?;
                return Ok(true);
            }
            if !resumed {
                return Ok(false);
            }
            let (mailbox, sequence) = journal.context("Codex delivery has no journal")?;
            let info = herdr.run(&["pane", "process-info", "--pane", pane]).await?;
            deliver(Some(&info), mailbox, sequence, text).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::UnixListener;

    fn info(socket: &Path, cwd: &Path) -> Value {
        json!({"process_info":{"foreground_processes":[{"pid":std::process::id(),"cwd":cwd,"argv":["codex","--remote",format!("unix://{}",socket.display()),"--dangerously-bypass-approvals-and-sandbox","--dangerously-bypass-hook-trust"]}]}})
    }

    fn fixture() -> (
        crate::config::test_support::Sandbox,
        PathBuf,
        PathBuf,
        UnixListener,
    ) {
        let sandbox = crate::config::test_support::sandbox();
        let socket = sandbox.root().join("app.sock");
        let transcript = sandbox.root().join("rollout.jsonl");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        (sandbox, socket, transcript, listener)
    }

    fn echo(thread: &str, id: &str, text: &str) -> Value {
        json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":thread,"item":{"type":"UserMessage","client_id":id,"content":[{"type":"text","text":text}]}}})
    }

    #[test]
    fn retiring_a_binding_preserves_event_intents() {
        let sandbox = crate::config::test_support::sandbox();
        let path = sandbox.root().join("codex-binding.json");
        std::fs::write(&path, b"old binding").unwrap();
        let journal = sandbox.root().join("codex-event.json");
        std::fs::write(&journal, b"pending event").unwrap();
        retire_binding(sandbox.root()).unwrap();
        retire_binding(sandbox.root()).unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read(&journal).unwrap(), b"pending event");
        assert!(std::fs::read_dir(sandbox.root()).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("codex-binding-retired-")
        }));
    }

    #[tokio::test]
    async fn standalone_falls_back_but_explicit_invalid_channel_is_held() {
        let sandbox = crate::config::test_support::sandbox();
        let standalone = json!({"process_info":{"foreground_processes":[{"argv":["codex"]}]}});
        assert!(
            !deliver(Some(&standalone), sandbox.root(), 1, "event")
                .await
                .unwrap()
        );
        let remote = info(&sandbox.root().join("missing.sock"), sandbox.root());
        assert!(
            deliver(Some(&remote), sandbox.root(), 1, "event")
                .await
                .is_err()
        );
        assert!(!has_record(sandbox.root(), 1, "event"));
    }

    #[tokio::test]
    async fn refuses_permission_overrides_and_nonprivate_endpoints() {
        let (sandbox, socket, _, _) = fixture();
        let good = info(&socket, sandbox.root());
        assert!(endpoint(&good).unwrap().is_some());
        // The overrides ssf itself appends are the ones this channel accepts:
        // the effort level, and the context-compaction threshold that goes on
        // every codex command, a native-delivery launcher included.
        for flag in [
            "-cmodel_reasoning_effort=high",
            "-cmodel_auto_compact_token_limit=300000",
            "--config=model_auto_compact_token_limit=300000",
        ] {
            let mut changed = good.clone();
            changed["process_info"]["foreground_processes"][0]["argv"]
                .as_array_mut()
                .unwrap()
                .push(json!(flag));
            assert!(endpoint(&changed).unwrap().is_some(), "{flag}");
        }
        // The form ssf actually emits: `-c` and its value as separate
        // arguments, the way `models::codex_compaction` writes them.
        let mut changed = good.clone();
        changed["process_info"]["foreground_processes"][0]["argv"]
            .as_array_mut()
            .unwrap()
            .extend([json!("-c"), json!("model_auto_compact_token_limit=300000")]);
        assert!(endpoint(&changed).unwrap().is_some(), "-c <key>=<value>");
        for flag in [
            "--sandbox=workspace-write",
            "-sread-only",
            "--profile=other",
            "--config=approval_policy=on-request",
            "-capproval_policy=on-request",
            // A key ssf does not write, and one that only starts with a key
            // ssf writes.
            "-cmodel_context_window=200000",
            "-cmodel_auto_compact_token_limit_scope=body_after_prefix",
        ] {
            let mut changed = good.clone();
            changed["process_info"]["foreground_processes"][0]["argv"]
                .as_array_mut()
                .unwrap()
                .push(json!(flag));
            assert!(endpoint(&changed).is_err(), "{flag}");
        }
        let mut changed = good;
        changed["process_info"]["foreground_processes"][0]["argv"][2] =
            json!("ws://localhost:5000");
        assert!(endpoint(&changed).is_err());
    }

    #[tokio::test]
    async fn durable_receipt_requires_exact_id_thread_and_content() {
        let (sandbox, socket, transcript, _) = fixture();
        let record = Record {
            binding: Binding {
                socket,
                cwd: sandbox.root().into(),
                thread: "thread1".into(),
                transcript: transcript.clone(),
            },
            id: "event1".into(),
            text: "[ssf] event".into(),
            confirmed: false,
        };
        for value in [
            echo("other", "event1", "[ssf] event"),
            echo("thread1", "other", "[ssf] event"),
            echo("thread1", "event1", "wrong"),
        ] {
            std::fs::write(&transcript, value.to_string()).unwrap();
            assert!(!observed(&record));
        }
        std::fs::write(
            &transcript,
            echo("thread1", "event1", "[ssf] event").to_string(),
        )
        .unwrap();
        assert!(observed(&record));
    }

    #[tokio::test]
    async fn native_message_receipt_and_restart_replay_never_resend() {
        let (sandbox, socket, transcript, listener) = fixture();
        let cwd = sandbox.root().to_owned();
        let transcript_for_server = transcript.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                let method = request["method"].as_str().unwrap();
                if method == "initialized" {
                    continue;
                }
                let result = match method {
                    "initialize" => json!({}),
                    "thread/loaded/list" => json!({"data":["thread1"]}),
                    "thread/read" => {
                        json!({"thread":{"id":"thread1","cwd":cwd,"path":transcript_for_server,"source":"cli"}})
                    }
                    "turn/start" => {
                        assert_eq!(request["params"]["approvalPolicy"], "never");
                        assert_eq!(
                            request["params"]["sandboxPolicy"]["type"],
                            "dangerFullAccess"
                        );
                        let content = request["params"]["input"][0]["text"].as_str().unwrap();
                        let id = request["params"]["clientUserMessageId"].as_str().unwrap();
                        std::fs::write(
                            &transcript_for_server,
                            echo("thread1", id, content).to_string(),
                        )
                        .unwrap();
                        json!({"turn":{"id":"same-active-turn"}})
                    }
                    _ => panic!("unexpected method {method}"),
                };
                ws.send(Message::Text(
                    json!({"id":request["id"],"result":result})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
                if method == "turn/start" {
                    break;
                }
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(300), listener.accept())
                    .await
                    .is_err()
            );
        });
        let mailbox = sandbox.root().join("mailbox");
        assert!(
            deliver(
                Some(&info(&socket, sandbox.root())),
                &mailbox,
                1,
                "[ssf] event"
            )
            .await
            .unwrap()
        );
        let path = record_path(&mailbox, 1, "[ssf] event");
        let mut record: Record = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // Reproduce receipt committed but confirmation lost across process exit.
        record.confirmed = false;
        save(&path, &record).unwrap();
        assert!(deliver(None, &mailbox, 1, "[ssf] event").await.unwrap());
        assert!(deliver(None, &mailbox, 1, "[ssf] event").await.unwrap());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn existing_unconfirmed_intent_is_held_without_reconnecting() {
        let (sandbox, socket, transcript, listener) = fixture();
        let text = "[ssf] unconfirmed";
        save(
            &record_path(sandbox.root(), 1, text),
            &Record {
                binding: Binding {
                    socket: socket.clone(),
                    cwd: sandbox.root().into(),
                    thread: "thread1".into(),
                    transcript,
                },
                id: "event1".into(),
                text: text.into(),
                confirmed: false,
            },
        )
        .unwrap();
        assert!(
            deliver(
                Some(&info(&socket, sandbox.root())),
                sandbox.root(),
                1,
                text
            )
            .await
            .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn multiple_threads_or_changed_binding_never_submit_an_event() {
        for multiple in [true, false] {
            let (sandbox, socket, transcript, listener) = fixture();
            let mailbox = sandbox.root().join("mailbox");
            if !multiple {
                save(
                    &mailbox.join("codex-binding.json"),
                    &Binding {
                        socket: socket.clone(),
                        cwd: sandbox.root().into(),
                        thread: "old-thread".into(),
                        transcript: transcript.clone(),
                    },
                )
                .unwrap();
            }
            let cwd = sandbox.root().to_owned();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(Message::Text(text))) = ws.next().await {
                    let request: Value = serde_json::from_str(&text).unwrap();
                    let result = match request["method"].as_str().unwrap() {
                        "initialized" => continue,
                        "initialize" => json!({}),
                        "thread/loaded/list" => {
                            json!({"data":if multiple {vec!["thread1","thread2"]} else {vec!["thread1"]}})
                        }
                        "thread/read" => {
                            json!({"thread":{"id":request["params"]["threadId"],"cwd":cwd,"path":transcript,"source":"cli"}})
                        }
                        other => panic!("must not submit through {other}"),
                    };
                    ws.send(Message::Text(
                        json!({"id":request["id"],"result":result})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                }
            });
            assert!(
                deliver(
                    Some(&info(&socket, sandbox.root())),
                    &mailbox,
                    1,
                    "[ssf] event"
                )
                .await
                .is_err()
            );
            assert!(!has_record(&mailbox, 1, "[ssf] event"));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn socket_permissions_are_checked_before_connecting() {
        let (_sandbox, socket, _, listener) = fixture();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(Rpc::connect(&socket).await.is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }

    /// Existing isolated scratch pane, with HUMAN-DRAFT-334 unsubmitted.
    #[tokio::test]
    #[ignore]
    async fn codex_live_channel() {
        let _machine = crate::config::test_support::the_machine_itself();
        let pane = std::env::var("SSF_CODEX_TEST_PANE").expect("supply an isolated scratch pane");
        let mailbox = PathBuf::from(
            std::env::var("SSF_CODEX_TEST_MAILBOX").expect("supply a temporary mailbox"),
        );
        let text = std::env::var("SSF_CODEX_TEST_EVENT").unwrap_or_else(|_| {
            "[ssf] Compiled Codex event 334. Reply CODEX-COMPILED-334-OK; do not use tools.".into()
        });
        let sequence = std::env::var("SSF_CODEX_TEST_SEQUENCE")
            .unwrap_or_else(|_| "1".into())
            .parse()
            .unwrap();
        let herdr = crate::herdr::Herdr::new(crate::config::HerdrConfig::default());
        let agent = crate::herdr::parse_agents(&herdr.run(&["agent", "list"]).await.unwrap())
            .into_iter()
            .find(|a| a.pane_id == pane)
            .unwrap();
        let relaunch = crate::driver::Relaunch {
            command: "unused",
            resume_command: None,
            harness: "codex",
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
        let path = record_path(&mailbox, sequence, &text);
        let mut record: Record = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(observed(&record));
        let body = std::fs::read_to_string(&record.binding.transcript).unwrap();
        assert_eq!(
            body.lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|v| v["payload"]["item"]["client_id"] == record.id)
                .count(),
            1
        );
        record.confirmed = false;
        save(&path, &record).unwrap();
        deliver(None, &mailbox, sequence, &text).await.unwrap();
        let screen = herdr
            .run_raw(&["pane", "read", &pane, "--lines", "70", "--format", "text"])
            .await
            .unwrap();
        assert!(screen.contains("HUMAN-DRAFT-334"));
    }
}
