//! Out-of-band delivery shared with harness-side bridges.
//!
//! The daemon publishes one file per event; the bridge injects it into the
//! session and acknowledges the file once the session's transcript records the
//! injected message.  Acknowledged therefore means "the agent has the record
//! of it", but publication is what makes the event durable: the file stays in
//! the mailbox until that record exists, so a harness that exits inside the
//! injection window takes the event again when it is relaunched, exactly as it
//! already reconciles an event whose record is in the resumed transcript
//! (#390).  A delivery that publishes without a receipt is queued in the
//! harness's own mailbox, not lost, and it stays pending for the relaunch path
//! to find.

use anyhow::{Context, Result, bail};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::Instant;

/// How long one attempt waits for the harness to record the event before the
/// pass moves on.  The record lands at the agent's next step boundary, so a
/// session inside a long tool call will not reach it in this window; the wait
/// is long enough to catch an idle session's injection and short enough that a
/// pass is not held behind a busy one.
const RECORD_TIMEOUT: Duration = Duration::from_secs(2);

/// What an attempt got: the transcript holds the event, or the mailbox does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Receipt {
    /// The session's transcript records the injected message.
    Recorded,
    /// Published to the mailbox and not recorded yet.  The bridge keeps the
    /// file until it is, and a relaunch injects it again if the harness went
    /// away first.
    Published,
}

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

/// Does the mailbox hold this event?  A pending file was published and may
/// already have been handed to the harness; only an acknowledged one is in the
/// session's transcript.  Either way the event must not be submitted twice, by
/// the bridge or through the terminal.
pub(crate) fn has_record(path: &Path, sequence: u64, text: &str) -> bool {
    let (pending, ack) = event_paths(path, sequence, text);
    pending.exists() || ack.exists()
}

/// Another attempt for this event sequence that the harness has not
/// acknowledged.  Its text carries the events this one carries too -- the
/// caller renders the delta since a watermark that has not moved -- so
/// publishing beside it would have the session record them twice.
fn unacknowledged(path: &Path, sequence: u64, here: &Path) -> bool {
    let prefix = format!("{sequence:020}-");
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .any(|candidate| {
            candidate != here
                && candidate.extension().is_some_and(|ext| ext == "json")
                && candidate
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
}

pub(crate) async fn deliver(path: &Path, sequence: u64, text: &str) -> Result<Receipt> {
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
        return Ok(Receipt::Recorded);
    }
    // One attempt per sequence is in flight at a time.  An earlier one that
    // the session has not recorded yet must land first: a pass that finds more
    // activity renders it into the same sequence, and both files would be
    // injected, leaving the session with the earlier events twice.  Whatever
    // the earlier file carries is published and stays published, so this
    // attempt reports what it can: the mailbox holds the events.
    let started = Instant::now();
    while unacknowledged(path, sequence, &pending) {
        if started.elapsed() >= RECORD_TIMEOUT {
            return Ok(Receipt::Published);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
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
    while started.elapsed() < RECORD_TIMEOUT {
        if ack.exists() {
            return Ok(Receipt::Recorded);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(Receipt::Published)
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

    /// The window #390 closes: a bridge that has published the event but has
    /// no session record of it yet.  The attempt must report the mailbox, not
    /// the transcript, and leave the event there for the harness to take and
    /// for a relaunch to find.
    #[tokio::test(start_paused = true)]
    async fn an_event_the_harness_has_not_recorded_stays_pending() {
        let sandbox = crate::config::test_support::sandbox();
        let root = sandbox.root().join("mailbox");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("ready.json"),
            format!("{{\"pid\":{}}}", std::process::id()),
        )
        .unwrap();
        assert_eq!(
            deliver(&root, 3, "[ssf] busy").await.unwrap(),
            Receipt::Published
        );
        let (pending, ack) = event_paths(&root, 3, "[ssf] busy");
        assert!(
            !ack.exists(),
            "an event the transcript does not hold was acknowledged"
        );
        let body: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&pending).unwrap()).unwrap();
        assert_eq!(
            body["text"], "[ssf] busy",
            "the event is no longer in the mailbox for the harness to take"
        );

        // The record lands at the agent's next step boundary, however long
        // that takes: the next attempt observes it and reports the record.
        std::fs::rename(&pending, &ack).unwrap();
        assert_eq!(
            deliver(&root, 3, "[ssf] busy").await.unwrap(),
            Receipt::Recorded
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
    }

    /// A second attempt for one sequence renders the events the first one
    /// already carries (the item's watermark has not moved), so it must not
    /// publish beside a file the session has not recorded: the session would
    /// see those events twice.
    #[tokio::test(start_paused = true)]
    async fn an_unrecorded_attempt_is_not_joined_by_a_second_one() {
        let sandbox = crate::config::test_support::sandbox();
        let root = sandbox.root().join("mailbox");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("ready.json"),
            format!("{{\"pid\":{}}}", std::process::id()),
        )
        .unwrap();
        let first = deliver(&root, 4, "[ssf] one").await.unwrap();
        assert_eq!(first, Receipt::Published);
        let second = deliver(&root, 4, "[ssf] one and two").await.unwrap();
        assert_eq!(second, Receipt::Published);
        assert!(
            !event_paths(&root, 4, "[ssf] one and two").0.exists(),
            "a superset of an unrecorded event was published beside it"
        );
        // Once the first is recorded, the fuller text goes out on its own.
        std::fs::rename(
            event_paths(&root, 4, "[ssf] one").0,
            event_paths(&root, 4, "[ssf] one").1,
        )
        .unwrap();
        assert_eq!(
            deliver(&root, 4, "[ssf] one and two").await.unwrap(),
            Receipt::Published
        );
        assert!(event_paths(&root, 4, "[ssf] one and two").0.exists());
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
