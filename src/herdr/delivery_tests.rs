//! [`Herdr::deliver`] per delivery channel, against a fake herdr: which pane
//! an event goes to, whether it goes through the harness's own channel or the
//! terminal, and what a relaunch does with an earlier attempt on record.
//! Written to pin these branches before they moved behind the delivery
//! channel trait (#446), and kept as its contract.

use super::*;
use crate::config::test_support::{Sandbox, sandbox};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// A herdr that logs every call and answers from files beside it: the agent
/// list before and after a harness is run in pane `w7:p9`, and the pane's
/// process info.
struct Fake {
    _sandbox: Sandbox,
    dir: PathBuf,
    herdr: Herdr,
    mailbox: PathBuf,
}

fn agents(rows: &[(&str, &str)]) -> String {
    let rows: Vec<_> = rows
        .iter()
        .map(|(kind, pane)| {
            json!({"agent": kind, "agent_status": "idle", "pane_id": pane, "workspace_id": "w7"})
        })
        .collect();
    json!({ "agents": rows }).to_string()
}

impl Fake {
    fn new(before: &[(&str, &str)], after: &[(&str, &str)]) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let sandbox = sandbox();
        let dir = sandbox.root().join("herdr-fake");
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("herdr");
        std::fs::write(
            &fake,
            r#"#!/bin/sh
d="$(dirname "$0")"
printf '%s\n' "$*" >> "$d/calls"
case "$1 $2" in
  "agent list")
    if [ -e "$d/ran" ]; then cat "$d/agents-after"; else cat "$d/agents"; fi ;;
  "pane list") echo '{"panes":[{"pane_id":"w7:p9","workspace_id":"w7"}]}' ;;
  "pane run") touch "$d/ran" ;;
  "agent wait") echo '{"agent_status":"idle"}' ;;
  "pane process-info") cat "$d/process-info" ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(dir.join("agents"), agents(before)).unwrap();
        std::fs::write(dir.join("agents-after"), agents(after)).unwrap();
        let herdr = Herdr::new(HerdrConfig {
            command: fake.to_string_lossy().into_owned(),
            tui_idle_timeout_ms: 200,
            ..HerdrConfig::default()
        });
        let mailbox = sandbox.root().join("mailbox");
        std::fs::create_dir_all(&mailbox).unwrap();
        Self {
            _sandbox: sandbox,
            dir,
            herdr,
            mailbox,
        }
    }

    /// The pane's foreground processes, as `pane process-info` reports them.
    fn process_info(&self, processes: Value) {
        std::fs::write(
            self.dir.join("process-info"),
            json!({"process_info": {"foreground_processes": processes}}).to_string(),
        )
        .unwrap();
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.join("calls")).unwrap_or_default()
    }

    fn relaunch<'a>(&'a self, harness: &'a str, journal: bool) -> Relaunch<'a> {
        Relaunch {
            command: "fresh",
            resume_command: None,
            harness,
            title: "title",
            text: Some("the whole story"),
            first_prompt: FirstPrompt::No,
            channel: journal.then_some((self.mailbox.as_path(), 1)),
        }
    }

    /// A confirmed Claude inbox journal for event 1: the event reached the
    /// session in an earlier attempt.
    fn claude_record(&self, text: &str) {
        let id = format!("{:020}-{:x}", 1, Sha256::digest(text.as_bytes()));
        std::fs::write(
            self.mailbox.join(format!("claude-{id}.json")),
            json!({"transcript": "/nowhere.jsonl", "content": text, "confirmed": true})
                .to_string(),
        )
        .unwrap();
    }

    /// A confirmed Codex journal for event 1.
    fn codex_record(&self, text: &str) {
        let name = format!("codex-{:020}-{:x}.json", 1, Sha256::digest(text.as_bytes()));
        std::fs::write(
            self.mailbox.join(name),
            json!({
                "binding": {"socket": "/s", "cwd": "/c", "thread": "t", "transcript": "/t"},
                "id": "ssf-1", "text": text, "confirmed": true
            })
            .to_string(),
        )
        .unwrap();
    }
}

fn delivered(d: &Delivery) -> (&str, bool, bool) {
    (d.handle.as_str(), d.relaunched, d.resumed)
}

/// A harness with no native channel takes the event through the terminal, in
/// the saved pane when it is live and otherwise in any live agent's.
#[tokio::test]
async fn terminal_delivery_goes_to_any_live_agent_when_the_saved_pane_is_gone() {
    let f = Fake::new(&[("claude", "w7:p2"), ("gemini", "w7:p3")], &[]);
    let d = f
        .herdr
        .deliver("w7", Some("w7:p1"), &f.relaunch("gemini", false), "event")
        .await
        .unwrap();
    assert_eq!(delivered(&d), ("w7:p2", false, false));
    assert!(f.calls().contains("agent prompt w7:p2 event"), "{}", f.calls());
    assert!(!f.calls().contains("process-info"), "{}", f.calls());
}

#[tokio::test]
async fn claude_without_an_inbox_takes_the_event_through_the_terminal() {
    let f = Fake::new(&[("claude", "w7:p1")], &[]);
    f.process_info(json!([]));
    let d = f
        .herdr
        .deliver("w7", Some("w7:p1"), &f.relaunch("claude", true), "event")
        .await
        .unwrap();
    assert_eq!(delivered(&d), ("w7:p1", false, false));
    let calls = f.calls();
    assert!(calls.contains("pane process-info --pane w7:p1"), "{calls}");
    assert!(calls.contains("agent prompt w7:p1 event"), "{calls}");
}

#[tokio::test]
async fn claude_event_on_record_is_not_sent_again() {
    let f = Fake::new(&[("claude", "w7:p1")], &[]);
    f.process_info(json!([]));
    f.claude_record("event");
    let d = f
        .herdr
        .deliver("w7", Some("w7:p1"), &f.relaunch("claude", true), "event")
        .await
        .unwrap();
    assert_eq!(delivered(&d), ("w7:p1", false, false));
    assert!(!f.calls().contains("agent prompt"), "{}", f.calls());
}

/// A standalone Codex TUI (no `--remote`) has no native channel yet.
#[tokio::test]
async fn standalone_codex_takes_the_event_through_the_terminal() {
    let f = Fake::new(&[("codex", "w7:p1")], &[]);
    f.process_info(json!([{"pid": 1, "argv": ["codex"]}]));
    let d = f
        .herdr
        .deliver("w7", Some("w7:p1"), &f.relaunch("codex", true), "event")
        .await
        .unwrap();
    assert_eq!(delivered(&d), ("w7:p1", false, false));
    let calls = f.calls();
    assert!(calls.contains("pane process-info --pane w7:p1"), "{calls}");
    assert!(calls.contains("agent prompt w7:p1 event"), "{calls}");
}

#[tokio::test]
async fn codex_event_on_record_is_not_sent_again() {
    let f = Fake::new(&[("codex", "w7:p1")], &[]);
    f.process_info(json!([]));
    f.codex_record("event");
    let d = f
        .herdr
        .deliver("w7", Some("w7:p1"), &f.relaunch("codex", true), "event")
        .await
        .unwrap();
    assert_eq!(delivered(&d), ("w7:p1", false, false));
    assert!(!f.calls().contains("agent prompt"), "{}", f.calls());
}

/// A codex pane whose process info cannot be read is an error, not a paste.
#[tokio::test]
async fn codex_without_process_info_is_not_pasted_into() {
    let f = Fake::new(&[("codex", "w7:p1")], &[]);
    f.herdr
        .deliver("w7", Some("w7:p1"), &f.relaunch("codex", true), "event")
        .await
        .unwrap_err();
    assert!(!f.calls().contains("agent prompt"), "{}", f.calls());
}

#[tokio::test]
async fn a_native_channel_without_its_journal_is_an_error() {
    for (harness, error) in [
        ("claude", "Claude delivery has no journal"),
        ("codex", "Codex delivery has no journal"),
        ("omp", "native harness delivery has no mailbox"),
        ("pi", "native harness delivery has no mailbox"),
    ] {
        let f = Fake::new(&[(harness, "w7:p1")], &[]);
        let e = f
            .herdr
            .deliver("w7", Some("w7:p1"), &f.relaunch(harness, false), "event")
            .await
            .unwrap_err();
        assert_eq!(e.to_string(), error);
        assert!(!f.calls().contains("process-info"), "{}", f.calls());
        assert!(!f.calls().contains("agent prompt"), "{}", f.calls());
    }
}

/// A session-bound channel addresses one session: with two live and no saved
/// pane to tell them apart, neither is guessed at.
#[tokio::test]
async fn a_session_bound_channel_does_not_guess_between_live_sessions() {
    for harness in ["claude", "codex"] {
        let f = Fake::new(&[(harness, "w7:p1"), (harness, "w7:p2")], &[]);
        let e = f
            .herdr
            .deliver("w7", None, &f.relaunch(harness, true), "event")
            .await
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            format!("{harness} delivery has multiple live sessions and no saved pane")
        );
    }
    // A mailbox channel takes the first, as the terminal does.
    let f = Fake::new(&[("omp", "w7:p1"), ("omp", "w7:p2")], &[]);
    let e = f
        .herdr
        .deliver("w7", None, &f.relaunch("omp", true), "event")
        .await
        .unwrap_err();
    assert!(e.to_string().contains("bridge is unavailable"), "{e}");
}

/// A saved pane hosting another harness is not this session's, so the event
/// goes to a relaunch rather than into it.
#[tokio::test]
async fn a_session_bound_channel_skips_a_saved_pane_hosting_another_harness() {
    let f = Fake::new(&[("gemini", "w7:p1")], &[("codex", "w7:p9")]);
    f.process_info(json!([]));
    let d = f
        .herdr
        .deliver("w7", Some("w7:p1"), &f.relaunch("codex", true), "event")
        .await
        .unwrap();
    assert_eq!(delivered(&d), ("w7:p9", true, false));
    let calls = f.calls();
    assert!(calls.contains("pane run w7:p9 fresh"), "{calls}");
    assert!(!calls.contains("agent prompt w7:p1"), "{calls}");
}

#[tokio::test]
async fn a_bound_codex_session_is_only_resumed_never_started_afresh() {
    let f = Fake::new(&[], &[]);
    std::fs::write(f.mailbox.join("codex-binding.json"), "{}").unwrap();
    let e = f
        .herdr
        .deliver("w7", None, &f.relaunch("codex", true), "event")
        .await
        .unwrap_err();
    assert_eq!(
        e.to_string(),
        "codex has an outstanding native delivery journal but no saved session to resume (no terminal fallback)"
    );
    assert!(!f.calls().contains("pane run"), "{}", f.calls());
}

#[tokio::test]
async fn an_event_on_record_is_not_resubmitted_when_the_resume_fails() {
    let f = Fake::new(&[], &[]);
    f.claude_record("event");
    let relaunch = Relaunch {
        resume_command: Some("resume"),
        ..f.relaunch("claude", true)
    };
    let e = f
        .herdr
        .deliver("w7", None, &relaunch, "event")
        .await
        .unwrap_err();
    assert_eq!(
        e.to_string(),
        "claude could not resume its outstanding native delivery; refusing a fresh terminal submission"
    );
    let calls = f.calls();
    assert!(calls.contains("pane run w7:p9 resume"), "{calls}");
    assert!(!calls.contains("pane run w7:p9 fresh"), "{calls}");
    assert!(!calls.contains("agent prompt"), "{calls}");
}

#[tokio::test]
async fn claude_event_on_record_is_settled_in_the_resumed_session() {
    let f = Fake::new(&[], &[("claude", "w7:p9")]);
    f.claude_record("event");
    let relaunch = Relaunch {
        resume_command: Some("resume"),
        ..f.relaunch("claude", true)
    };
    let d = f
        .herdr
        .deliver("w7", None, &relaunch, "event")
        .await
        .unwrap();
    assert_eq!(delivered(&d), ("w7:p9", true, true));
    let calls = f.calls();
    assert!(calls.contains("pane run w7:p9 resume"), "{calls}");
    assert!(!calls.contains("process-info"), "{calls}");
    assert!(!calls.contains("agent prompt"), "{calls}");
}

#[tokio::test]
async fn codex_event_on_record_needs_its_native_channel_after_the_resume() {
    let f = Fake::new(&[], &[("codex", "w7:p9")]);
    f.process_info(json!([]));
    f.codex_record("event");
    let relaunch = Relaunch {
        resume_command: Some("resume"),
        ..f.relaunch("codex", true)
    };
    let e = f
        .herdr
        .deliver("w7", None, &relaunch, "event")
        .await
        .unwrap_err();
    assert_eq!(
        e.to_string(),
        "Codex resumed without its native channel; refusing terminal fallback"
    );
    assert!(!f.calls().contains("agent prompt"), "{}", f.calls());
}

/// A resumed Codex with nothing on record tries its native channel, and a
/// standalone one takes the event as a first prompt: the event, not the story.
#[tokio::test]
async fn resumed_codex_tries_its_native_channel_before_the_terminal() {
    let f = Fake::new(&[], &[("codex", "w7:p9")]);
    f.process_info(json!([]));
    let relaunch = Relaunch {
        resume_command: Some("resume"),
        ..f.relaunch("codex", true)
    };
    let d = f
        .herdr
        .deliver("w7", None, &relaunch, "event")
        .await
        .unwrap();
    assert_eq!(delivered(&d), ("w7:p9", true, true));
    let calls = f.calls();
    let native = calls.find("pane process-info --pane w7:p9").expect(&calls);
    let terminal = calls.find("agent prompt w7:p9 event --wait").expect(&calls);
    assert!(native < terminal, "{calls}");
}

/// A resumed Claude with nothing on record is not offered its inbox: the
/// event goes in as a first prompt.
#[tokio::test]
async fn resumed_claude_takes_the_event_through_the_terminal() {
    let f = Fake::new(&[], &[("claude", "w7:p9")]);
    let relaunch = Relaunch {
        resume_command: Some("resume"),
        ..f.relaunch("claude", true)
    };
    let d = f
        .herdr
        .deliver("w7", None, &relaunch, "event")
        .await
        .unwrap();
    assert_eq!(delivered(&d), ("w7:p9", true, true));
    let calls = f.calls();
    assert!(!calls.contains("process-info"), "{calls}");
    assert!(calls.contains("agent prompt w7:p9 event --wait"), "{calls}");
}

/// A mailbox event already on record is handed to the relaunched harness's
/// bridge, which reconciles it, and never pasted -- even into a fresh launch.
#[tokio::test]
async fn mailbox_event_on_record_goes_back_through_the_mailbox_after_a_relaunch() {
    let f = Fake::new(&[], &[("omp", "w7:p9")]);
    std::fs::write(
        f.mailbox.join("ready.json"),
        json!({"pid": std::process::id()}).to_string(),
    )
    .unwrap();
    // Publish event 1 and have the "bridge" acknowledge it.
    let mailbox = f.mailbox.clone();
    let ack = tokio::spawn(async move {
        loop {
            let pending = std::fs::read_dir(&mailbox).unwrap().flatten().find(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.starts_with(&format!("{:020}-", 1)) && name.ends_with(".json")
            });
            if let Some(pending) = pending {
                let path = pending.path();
                std::fs::rename(&path, format!("{}.ack", path.display())).unwrap();
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    crate::delivery_channel::deliver(&f.mailbox, 1, "event")
        .await
        .unwrap();
    ack.await.unwrap();
    let d = f
        .herdr
        .deliver("w7", None, &f.relaunch("omp", true), "event")
        .await
        .unwrap();
    assert_eq!(delivered(&d), ("w7:p9", true, true));
    let calls = f.calls();
    assert!(calls.contains("pane run w7:p9 fresh"), "{calls}");
    assert!(!calls.contains("agent prompt"), "{calls}");
}

/// A fresh harness with no native channel gets the whole story as its first
/// prompt.
#[tokio::test]
async fn a_fresh_terminal_harness_gets_the_whole_story() {
    let f = Fake::new(&[], &[("gemini", "w7:p9")]);
    let d = f
        .herdr
        .deliver("w7", None, &f.relaunch("gemini", false), "event")
        .await
        .unwrap();
    assert_eq!(delivered(&d), ("w7:p9", true, false));
    let calls = f.calls();
    assert!(calls.contains("pane run w7:p9 fresh"), "{calls}");
    assert!(
        calls.contains("agent prompt w7:p9 the whole story --wait"),
        "{calls}"
    );
}
