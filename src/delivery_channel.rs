//! Out-of-band delivery shared with harness-side bridges.

use anyhow::{Context, Result, bail};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const ACK_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn mailbox(repo: &str, number: u64) -> PathBuf {
    let mut path = crate::config::state_dir().join("delivery");
    for component in repo.split('/') {
        path.push(match component {
            "" => "_empty_",
            "." => "_dot_",
            ".." => "_dot-dot_",
            other => other,
        });
    }
    path.join(number.to_string())
}

pub(crate) fn bridge() -> PathBuf {
    crate::platform::share_file("harness/ssf-delivery.ts")
}

pub(crate) fn supports(harness: &str) -> bool {
    matches!(harness, "omp" | "pi")
}

pub(crate) fn available(path: &Path) -> bool {
    let pid = std::fs::read(path.join("ready.json"))
        .ok()
        .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).ok())
        .and_then(|ready| ready.get("pid")?.as_i64())
        .and_then(|pid| i32::try_from(pid).ok());
    let Some(pid) = pid else { return false };
    // SAFETY: signal 0 changes no process state; it only checks whether the
    // extension process named by its own ready marker still exists.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn fingerprint(text: &str) -> u64 {
    // FNV-1a is stable across daemon restarts; the sequence makes equal
    // messages in one session distinct.
    text.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

fn event_paths(path: &Path, sequence: u64, text: &str) -> (PathBuf, PathBuf) {
    let stem = format!("{sequence:020}-{:016x}", fingerprint(text));
    (
        path.join(format!("{stem}.json")),
        path.join(format!("{stem}.json.ack")),
    )
}

pub(crate) fn has_record(path: &Path, sequence: u64, text: &str) -> bool {
    let (pending, ack) = event_paths(path, sequence, text);
    pending.exists() || ack.exists()
}

pub(crate) async fn deliver(path: &Path, sequence: u64, text: &str) -> Result<()> {
    if !available(path) {
        bail!(
            "the harness delivery bridge is unavailable at {}; restart the session to load it",
            path.display()
        );
    }
    std::fs::create_dir_all(path)
        .with_context(|| format!("creating delivery mailbox {}", path.display()))?;
    let (pending, ack) = event_paths(path, sequence, text);
    if ack.exists() {
        return Ok(());
    }
    if !pending.exists() {
        let stem = pending
            .file_stem()
            .and_then(|stem| stem.to_str())
            .context("delivery path has no UTF-8 file stem")?;
        let temporary = path.join(format!(".{stem}.tmp-{}", std::process::id()));
        let bytes = serde_json::to_vec(&json!({ "text": text }))?;
        std::fs::write(&temporary, bytes)
            .with_context(|| format!("writing delivery {}", temporary.display()))?;
        std::fs::rename(&temporary, &pending)
            .with_context(|| format!("publishing delivery {}", pending.display()))?;
    }
    let started = Instant::now();
    while started.elapsed() < ACK_TIMEOUT {
        if ack.exists() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    bail!(
        "the harness did not acknowledge out-of-band delivery through {}",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_is_scoped_to_repository_and_session() {
        let sandbox = crate::config::test_support::sandbox();
        assert_eq!(
            mailbox("owner/repo", 42),
            sandbox.state_dir().join("delivery/owner/repo/42")
        );
        assert!(mailbox("../repo", 42).starts_with(sandbox.state_dir().join("delivery")));
    }

    #[tokio::test]
    async fn delivery_waits_for_the_bridge_acknowledgement() {
        let sandbox = crate::config::test_support::sandbox();
        let root = sandbox.root().join("mailbox");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("ready.json"),
            format!("{{\"pid\":{}}}", std::process::id()),
        )
        .unwrap();
        let path = root.clone();
        let bridge = tokio::spawn(async move {
            loop {
                let entry = std::fs::read_dir(&path)
                    .unwrap()
                    .flatten()
                    .map(|entry| entry.path())
                    .find(|path| {
                        path.extension().is_some_and(|ext| ext == "json")
                            && path.file_name().unwrap() != "ready.json"
                    });
                if let Some(pending) = entry {
                    let text: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&pending).unwrap()).unwrap();
                    std::fs::rename(&pending, format!("{}.ack", pending.display())).unwrap();
                    return text["text"].as_str().unwrap().to_string();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        deliver(&root, 7, "[ssf] hello").await.unwrap();
        assert_eq!(bridge.await.unwrap(), "[ssf] hello");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
        // A retry after the handoff observes the durable acknowledgement and
        // cannot publish a second copy.
        deliver(&root, 7, "[ssf] hello").await.unwrap();
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);

        std::fs::write(event_paths(&root, 8, "[ssf] later").0, b"pending").unwrap();
        assert!(has_record(&root, 8, "[ssf] later"));
        assert_eq!(
            std::fs::read_dir(&root)
                .unwrap()
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json.ack"))
                .count(),
            1,
            "a later event must not delete the earlier durable acknowledgement"
        );
    }
}
