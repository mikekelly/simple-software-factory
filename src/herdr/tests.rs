use super::*;
use serde_json::json;

/// The pending mailbox file for one event sequence, once the daemon has
/// published it.  The file is named by sequence and content fingerprint, which
/// only the channel computes, so this finds it the way the bridge does.
async fn wait_for_pending(mailbox: &std::path::Path, sequence: u64) -> std::path::PathBuf {
    let prefix = format!("{sequence:020}-");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        for entry in std::fs::read_dir(mailbox).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // An event the harness recorded at once is already acknowledged,
            // and one the Grok bridge is sending is claimed as `.handed`.
            if let Some(stem) = name
                .strip_suffix(".ack")
                .or(name.strip_suffix(".handed"))
                .or(Some(&name))
                && name.starts_with(&prefix)
                && stem.ends_with(".json")
            {
                return mailbox.join(stem);
            }
        }
        assert!(
            Instant::now() < deadline,
            "event {sequence} never reached the mailbox at {}",
            mailbox.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// End the live harness in `handle` the way a crash or a closed terminal
/// would. Grok takes its first ctrl+c as "cancel the turn", which would run a
/// queued event before it exits, so its TUI -- the pid Grok's registry gives
/// for the conversation the bridge pinned -- is sent SIGTERM instead.
async fn stop_harness(h: &Herdr, handle: &str, harness: &str, mailbox: &std::path::Path) {
    if harness != "grok" {
        h.run(&["pane", "send-keys", handle, "ctrl+d"])
            .await
            .unwrap();
        return;
    }
    let id = std::fs::read_to_string(mailbox.join("session/grok-session")).unwrap();
    let home = std::env::var_os("GROK_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".grok")
        });
    let active: Value =
        serde_json::from_slice(&std::fs::read(home.join("active_sessions.json")).unwrap()).unwrap();
    let pid = active
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["session_id"] == id.trim())
        .and_then(|entry| entry["pid"].as_i64())
        .expect("the pinned conversation is not open");
    // SAFETY: a signal to the grok TUI this test started.
    assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGTERM) }, 0);
}

/// The session's own record, as text: the Pi/OMP transcript in the mailbox,
/// Grok's `updates.jsonl` for the conversation its bridge pinned, or
/// OpenCode's export of the conversation its plugin pinned.
fn live_transcript(harness: &str, mailbox: &std::path::Path, root: &str) -> String {
    if harness == "grok" {
        let id = std::fs::read_to_string(mailbox.join("session/grok-session")).unwrap();
        let home = std::env::var_os("GROK_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".grok")
            });
        let file = std::fs::read_dir(home.join("sessions"))
            .unwrap()
            .flatten()
            .map(|group| group.path().join(id.trim()).join("updates.jsonl"))
            .find(|file| file.is_file())
            .unwrap();
        return std::fs::read_to_string(file).unwrap();
    }
    if harness == "opencode" {
        let id = std::fs::read_to_string(mailbox.join("session/opencode-session")).unwrap();
        let out = std::process::Command::new("opencode")
            .args(["export", id.trim()])
            .current_dir(root)
            .output()
            .unwrap();
        return String::from_utf8_lossy(&out.stdout).into_owned();
    }
    let transcript = std::fs::read_dir(mailbox.join("session"))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .unwrap();
    std::fs::read_to_string(transcript).unwrap()
}

/// What each injected event leaves in [`live_transcript`], once.
fn injected_marker(harness: &str) -> &'static str {
    if harness == "opencode" || harness == "grok" {
        "\"ssfDeliveryId\""
    } else {
        "\"customType\":\"ssf-item-activity\""
    }
}

#[tokio::test]
async fn omp_delivery_uses_the_mailbox_not_terminal_input() {
    let base =
        std::env::temp_dir().join(format!("ssf-herdr-native-delivery-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let fake = base.join("herdr");
    crate::test_support::write_executable(
        &fake,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$(dirname "$0")/calls"
case "$1 $2" in
  "agent list")
    echo '{"agents":[{"agent":"omp","agent_status":"idle","pane_id":"w7:p1","workspace_id":"w7"}]}'
    ;;
  "agent prompt"|"pane send-text"|"pane send-keys")
    echo 'terminal input was used' >&2
    exit 1
    ;;
esac
"#,
    );
    let mailbox = base.join("mailbox");
    std::fs::create_dir(&mailbox).unwrap();
    std::fs::write(
        mailbox.join("ready.json"),
        format!("{{\"pid\":{}}}", std::process::id()),
    )
    .unwrap();
    let reader = mailbox.clone();
    let bridge = tokio::spawn(async move {
        loop {
            if let Some(pending) = std::fs::read_dir(&reader).unwrap().flatten().find_map(|e| {
                let path = e.path();
                (path.file_name().unwrap() != "ready.json"
                    && path.extension().is_some_and(|ext| ext == "json"))
                .then_some(path)
            }) {
                let value: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&pending).unwrap()).unwrap();
                std::fs::rename(&pending, format!("{}.ack", pending.display())).unwrap();
                return value["text"].as_str().unwrap().to_string();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    let h = Herdr::new(HerdrConfig {
        command: fake.to_string_lossy().into_owned(),
        ..HerdrConfig::default()
    });
    let relaunch = Relaunch {
        command: "unused",
        resume_command: None,
        harness: "omp",
        title: "unused",
        text: None,
        first_prompt: FirstPrompt::No,
        channel: Some((&mailbox, 3)),
    };
    h.deliver("w7", Some("w7:p1"), &relaunch, "[ssf] event")
        .await
        .unwrap();
    assert_eq!(bridge.await.unwrap(), "[ssf] event");
    let calls = std::fs::read_to_string(base.join("calls")).unwrap();
    assert!(calls.contains("agent list"), "{calls}");
    assert!(!calls.contains("agent prompt"), "{calls}");
    assert!(!calls.contains("pane send"), "{calls}");
    std::fs::remove_dir_all(base).unwrap();
}

/// Grok's bridge attests only after its ACP probe works: with no attested
/// bridge an event is pasted, as before the bridge; with one it goes to the
/// mailbox; and one published and not yet recorded is never pasted beside.
#[tokio::test]
async fn grok_delivery_falls_back_to_the_terminal_without_its_bridge() {
    let base = std::env::temp_dir().join(format!("ssf-herdr-grok-delivery-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let fake = base.join("herdr");
    crate::test_support::write_executable(
        &fake,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$(dirname "$0")/calls"
case "$1 $2" in
  "agent list")
    echo '{"agents":[{"agent":"grok","agent_status":"idle","pane_id":"w7:p1","workspace_id":"w7"}]}'
    ;;
  "agent prompt")
    echo '{"result":{"type":"agent_prompted"}}'
    ;;
esac
"#,
    );
    let mailbox = base.join("mailbox");
    std::fs::create_dir(&mailbox).unwrap();
    let h = Herdr::new(HerdrConfig {
        command: fake.to_string_lossy().into_owned(),
        ..HerdrConfig::default()
    });
    let relaunch = |sequence| Relaunch {
        command: "unused",
        resume_command: None,
        harness: "grok",
        title: "unused",
        text: None,
        first_prompt: FirstPrompt::No,
        channel: Some((&mailbox, sequence)),
    };
    h.deliver("w7", Some("w7:p1"), &relaunch(1), "[ssf] pasted")
        .await
        .unwrap();
    let calls = std::fs::read_to_string(base.join("calls")).unwrap();
    assert!(calls.contains("agent prompt"), "{calls}");
    assert_eq!(
        std::fs::read_dir(&mailbox).unwrap().count(),
        0,
        "a paste was also published"
    );

    std::fs::remove_file(base.join("calls")).unwrap();
    std::fs::write(
        mailbox.join("ready.json"),
        format!("{{\"pid\":{}}}", std::process::id()),
    )
    .unwrap();
    h.deliver("w7", Some("w7:p1"), &relaunch(2), "[ssf] bridged")
        .await
        .unwrap();
    let calls = std::fs::read_to_string(base.join("calls")).unwrap();
    assert!(!calls.contains("agent prompt"), "{calls}");
    assert!(crate::delivery_channel::has_record(
        &mailbox,
        2,
        "[ssf] bridged"
    ));

    // A bridge still starting (a relaunch): events wait for it.
    std::fs::remove_file(mailbox.join("ready.json")).unwrap();
    std::fs::remove_file(base.join("calls")).unwrap();
    let marker = |state: &str, pid: u32| {
        std::fs::write(
            mailbox.join("bridge.json"),
            format!("{{\"state\":\"{state}\",\"pid\":{pid}}}"),
        )
        .unwrap();
    };
    marker("starting", std::process::id());
    let held = h
        .deliver("w7", Some("w7:p1"), &relaunch(3), "[ssf] later")
        .await
        .unwrap_err();
    assert_eq!(
        crate::delivery_channel::hold(&held),
        Some(crate::delivery_channel::Hold::Unavailable),
        "{held:#}"
    );
    let calls = std::fs::read_to_string(base.join("calls")).unwrap_or_default();
    assert!(!calls.contains("agent prompt"), "{calls}");

    // The bridge stopped with event 2 unrecorded: it is pasted, then the new
    // one, and nothing is left for a bridge that is not coming.
    marker("stopped", std::process::id());
    h.deliver("w7", Some("w7:p1"), &relaunch(3), "[ssf] later")
        .await
        .unwrap();
    let calls = std::fs::read_to_string(base.join("calls")).unwrap();
    let bridged = calls.find("[ssf] bridged").expect(&calls);
    let later = calls.find("[ssf] later").expect(&calls);
    assert!(bridged < later, "{calls}");
    let left: Vec<_> = std::fs::read_dir(&mailbox)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('0') && !name.ends_with(".ack"))
        .collect();
    assert!(left.is_empty(), "{left:?}");

    // An event the conversation already records is acknowledged, not pasted
    // again, when the bridge declines before acknowledging it (a dead pid is
    // the same).
    std::fs::write(
        mailbox.join("ready.json"),
        format!("{{\"pid\":{}}}", std::process::id()),
    )
    .unwrap();
    h.deliver("w7", Some("w7:p1"), &relaunch(4), "[ssf] recorded")
        .await
        .unwrap();
    let name = std::fs::read_dir(&mailbox)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|name| name.starts_with("00000000000000000004-") && name.ends_with(".json"))
        .unwrap();
    std::fs::remove_file(mailbox.join("ready.json")).unwrap();
    let home = base.join("grok-home");
    let conversation = home.join("sessions/%2Fwork/0190-abc");
    std::fs::create_dir_all(&conversation).unwrap();
    std::fs::write(
        conversation.join("updates.jsonl"),
        format!(
            "{{\"update\":{{\"sessionUpdate\":\"user_message_chunk\",\"content\":{{\"type\":\"text\",\"text\":\"[ssf] recorded\",\"_meta\":{{\"ssfDeliveryId\":\"{name}\"}}}}}}}}\n"
        ),
    )
    .unwrap();
    std::fs::create_dir_all(mailbox.join("session")).unwrap();
    std::fs::write(mailbox.join("session/grok-session"), "0190-abc\n").unwrap();
    std::fs::write(
        mailbox.join("session/grok-home"),
        format!("{}\n", home.display()),
    )
    .unwrap();
    marker("declined", std::process::id());
    std::fs::remove_file(base.join("calls")).unwrap();
    h.deliver("w7", Some("w7:p1"), &relaunch(5), "[ssf] fifth")
        .await
        .unwrap();
    let calls = std::fs::read_to_string(base.join("calls")).unwrap();
    assert!(calls.contains("[ssf] fifth"), "{calls}");
    assert!(
        !calls.contains("[ssf] recorded"),
        "a recorded event was pasted again: {calls}"
    );
    assert!(mailbox.join(format!("{name}.ack")).is_file());
    assert!(!mailbox.join(&name).exists());
    std::fs::remove_dir_all(base).unwrap();
}

/// Against a running herdr server: makes a scratch repo, opens a
/// workspace, runs Claude Code in it, sends a prompt, removes it all.
/// `cargo test herdr_live -- --ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn herdr_live() {
    let base = std::env::temp_dir().join(format!("ssf-herdr-live-{}", std::process::id()));
    let root = base.join("widgets").to_string_lossy().to_string();
    std::fs::create_dir_all(&root).unwrap();
    for args in [
        vec!["init", "-q", "-b", "master"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(&args)
                .status()
                .unwrap()
                .success()
        );
    }
    let h = Herdr::new(HerdrConfig::default());
    h.status().await.unwrap();
    let wt = h
        .create_worktree(&root, "owner/widgets", "issue-3-x", 3, "ssf: test", None)
        .await
        .unwrap();
    eprintln!("workspace {} at {}", wt.id, wt.path);
    assert!(h.worktree_exists(&wt.id).await.unwrap());
    assert!(!h.has_live_agent(&wt.id).await.unwrap());
    let found = h
        .find_worktree_for_issue(&root, "owner/widgets", 3)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, wt.id);
    // A shell that leaves the checkout does not move the workspace (#50):
    // herdr's own binding, not a pane's cwd, says it is still ours.
    let (ws, _) = split_id(&wt.id);
    let root_pane = h.panes(ws).await.unwrap()[0].pane_id.clone();
    h.run(&["pane", "run", &root_pane, "cd /"]).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let cwd = h.panes(ws).await.unwrap()[0].cwd.clone();
    assert_eq!(cwd.as_deref(), Some("/"), "herdr did not see the cd");
    assert!(h.worktree_exists(&wt.id).await.unwrap());
    let away = h.ps().await.unwrap();
    let row = away.iter().find(|r| r.worktree_id == wt.id).unwrap();
    assert_eq!(row.linked_issue, Some(3));
    assert_eq!(row.repo_id, root);
    let back = format!("cd '{}'", wt.path);
    h.run(&["pane", "run", &root_pane, &back]).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let handle = h
        .launch(&wt.id, "claude --model haiku", "claude · #3", "claude")
        .await
        .unwrap();
    eprintln!("launched in {handle}");
    assert!(h.has_live_agent(&wt.id).await.unwrap());
    // The first prompt after a launch is the confirmed one (#121): if
    // the harness came up on a dialog and the paste went nowhere, this
    // is an error rather than a silent loss.
    h.send_first_prompt(&handle, "Reply with the single word pong and nothing else.")
        .await
        .unwrap();
    // The prompt must have been submitted, not just pasted.
    tokio::time::sleep(Duration::from_secs(8)).await;
    let v = h
        .run(&["agent", "wait", &handle, "--timeout", "60000"])
        .await
        .unwrap();
    eprintln!("wait: {v}");
    let screen = h.screen(&handle).await.unwrap();
    eprintln!("screen:\n{}", screen.join("\n"));
    let text = screen.join("\n").to_lowercase();
    assert!(text.contains("pong"), "the prompt was not answered");
    let rows = h.ps().await.unwrap();
    let row = rows.iter().find(|r| r.worktree_id == wt.id).unwrap();
    eprintln!("ps row: {row:?}");
    assert_eq!(row.linked_issue, Some(3));
    assert_eq!(row.repo_id, root);
    h.set_status(&wt.id, "completed").await.unwrap();
    // An id naming another checkout is not ours: nothing is touched.
    let (ws, _) = split_id(&wt.id);
    let foreign = make_id(ws, "/somewhere/else");
    assert!(!h.worktree_exists(&foreign).await.unwrap());
    h.remove_worktree(&foreign).await.unwrap();
    assert!(h.worktree_exists(&wt.id).await.unwrap());
    // Removal while the root pane's shell is elsewhere (#50): the agent
    // is quit (two ctrl+c), the shell sent away, and the workspace and
    // checkout must still go.
    for _ in 0..2 {
        h.run(&["pane", "send-keys", &handle, "ctrl+c"])
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    for _ in 0..20 {
        if !h.has_live_agent(&wt.id).await.unwrap() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(
        !h.has_live_agent(&wt.id).await.unwrap(),
        "claude did not quit"
    );
    h.run(&["pane", "run", &handle, "cd /"]).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(h.worktree_exists(&wt.id).await.unwrap());
    h.remove_worktree(&wt.id).await.unwrap();
    assert!(!h.worktree_exists(&wt.id).await.unwrap());
    assert!(!std::path::Path::new(&wt.path).exists());
    // The clone's own workspace, which herdr opened alongside ours, is
    // left alone in use; here the clone goes too.
    if let Ok(v) = h.run(&["worktree", "list", "--cwd", &root]).await
        && let Some(src) = v
            .pointer("/source/source_workspace_id")
            .and_then(Value::as_str)
    {
        let _ = h.run(&["workspace", "close", src]).await;
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// Against a running herdr server: opens a workspace on a scratch repo
/// the harness has never seen, launches the harness and gives it its
/// first prompt, which is the case #121 was about -- Codex comes up on
/// its directory-trust dialog and herdr calls that `idle`. The harness
/// is `SSF_LIVE_HARNESS` with `SSF_LIVE_COMMAND` (Codex by default).
/// `cargo test herdr_live_first_prompt -- --ignored --nocapture`.
///
/// The repository is nested in a base temp directory so that the
/// sibling `<root>.worktrees/` the worktree goes in is removed with it.
/// One thing the test does not clean up: a Codex run leaves a
/// `[projects."<base>/widgets"]` entry with `trust_level = "trusted"`
/// in `~/.codex/config.toml` (the repository root, not the worktree it
/// ran in), which has to be stripped by hand.
#[tokio::test]
#[ignore]
async fn herdr_live_first_prompt() {
    let harness = std::env::var("SSF_LIVE_HARNESS").unwrap_or_else(|_| "codex".into());
    let command = std::env::var("SSF_LIVE_COMMAND")
        .unwrap_or_else(|_| crate::models::default_command(&harness));
    let base = std::env::temp_dir().join(format!(
        "ssf-first-prompt-{}-{}",
        harness,
        std::process::id()
    ));
    let root = base.join("widgets").to_string_lossy().to_string();
    std::fs::create_dir_all(&root).unwrap();
    for args in [
        vec!["init", "-q", "-b", "master"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(&args)
                .status()
                .unwrap()
                .success()
        );
    }
    let h = Herdr::new(HerdrConfig::default());
    h.status().await.unwrap();
    let wt = h
        .create_worktree(
            &root,
            "owner/widgets",
            "issue-121-x",
            121,
            "ssf: test",
            None,
        )
        .await
        .unwrap();
    eprintln!("workspace {} at {}", wt.id, wt.path);
    let started = Instant::now();
    let handle = h
        .launch(&wt.id, &command, "first prompt · #121", &harness)
        .await
        .unwrap();
    eprintln!("launched {harness} in {handle} in {:?}", started.elapsed());
    eprintln!(
        "screen after settling:\n{}",
        h.screen(&handle).await.unwrap().join("\n")
    );
    let sent = Instant::now();
    h.send_first_prompt(&handle, "Reply with the single word pong and nothing else.")
        .await
        .unwrap();
    eprintln!("first prompt taken in {:?}", sent.elapsed());
    h.run(&["agent", "wait", &handle, "--timeout", "60000"])
        .await
        .unwrap();
    let screen = h.screen(&handle).await.unwrap().join("\n");
    eprintln!("screen:\n{screen}");
    assert!(
        screen.to_lowercase().contains("pong"),
        "the first prompt was not answered"
    );
    // Reproduce #279 directly: the body made it into a collapsed composer
    // card but its submitting Enter did not. Recovery must submit that body
    // in place, not paste another copy over it.
    let stranded = format!(
        "<attachment>\n\
# GitHub issue #14: Phase 1a — Compatibility spike\n\
https://github.com/mikekelly/inception/issues/14\n\
Opened by @MikeKellyBot for a delivery recovery test.\n\
{}\n\
</attachment>\n\
Reply with the single token RECOVERED-279 and nothing else.",
        (0..125)
            .map(|n| format!("Evidence line {n}: preserve the host-side record."))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let pasted = format!("{PASTE_START}{}{PASTE_END}", stranded.trim_end());
    h.run(&["pane", "send-text", &handle, &pasted])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    // OMP asks how a large paste should be represented. This is the one
    // Enter Herdr sends: it chooses the default wrapped attachment but does
    // not submit the resulting composer card.
    h.run(&["agent", "send-keys", &handle, "enter"])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let waiting = h.recent_screen(&handle).await.unwrap().join("\n");
    eprintln!("stranded composer:\n{waiting}");
    assert!(
        prompt_on_screen(&waiting, &stranded),
        "the stranded prompt was not identifiable"
    );
    h.recover_first_prompt(&handle, &stranded).await.unwrap();
    h.run(&["agent", "wait", &handle, "--timeout", "60000"])
        .await
        .unwrap();
    let recovered = h.screen(&handle).await.unwrap().join("\n");
    eprintln!("recovered screen:\n{recovered}");
    assert!(
        recovered.contains("RECOVERED-279"),
        "the stranded prompt was not answered"
    );
    h.remove_worktree(&wt.id).await.unwrap();
    if let Ok(v) = h.run(&["worktree", "list", "--cwd", &root]).await
        && let Some(src) = v
            .pointer("/source/source_workspace_id")
            .and_then(Value::as_str)
    {
        let _ = h.run(&["workspace", "close", src]).await;
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// Against a running herdr server and an installed OMP/Pi/OpenCode/Grok: proves #334's
/// gate.  A native event wakes an idle session without submitting a draft,
/// and an event accepted during a turn is handled afterwards.
/// `cargo test herdr_live_native_delivery -- --ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn herdr_live_native_delivery() {
    let harness = std::env::var("SSF_LIVE_HARNESS").unwrap_or_else(|_| "omp".into());
    assert!(crate::harness::channel(&harness).bridged());
    let base = std::env::temp_dir().join(format!(
        "ssf-native-delivery-{harness}-{}",
        std::process::id()
    ));
    let root = base.join("widgets").to_string_lossy().to_string();
    let mailbox = base.join("mailbox");
    std::fs::create_dir_all(&root).unwrap();
    for args in [
        vec!["init", "-q", "-b", "master"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(&args)
                .status()
                .unwrap()
                .success()
        );
    }
    let bridge = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("harness/ssf-delivery.ts");
    let launcher = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("harness/ssf-pi-launch");
    let quote = |path: &std::path::Path| format!("'{}'", path.display());
    let command = std::env::var("SSF_LIVE_COMMAND")
        .unwrap_or_else(|_| crate::models::default_command(&harness))
        .replace("\"$SSF_PI_BRIDGE\"", &quote(&bridge))
        .replace("\"$SSF_PI_LAUNCHER\"", &quote(&launcher));
    let opencode_bridge =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("harness/ssf-opencode.ts");
    let grok_bridge = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("harness/ssf-grok.mjs");
    let command = format!(
        "SSF_DELIVERY_MAILBOX={} SSF_OPENCODE_BRIDGE={} SSF_GROK_BRIDGE={} {command}",
        quote(&mailbox),
        quote(&opencode_bridge),
        quote(&grok_bridge)
    );
    let h = Herdr::new(HerdrConfig::default());
    h.status().await.unwrap();
    let wt = h
        .create_worktree(
            &root,
            "owner/widgets",
            "issue-334-native-delivery",
            334,
            "ssf: native delivery test",
            None,
        )
        .await
        .unwrap();
    let handle = h
        .launch(&wt.id, &command, "native delivery · #334", &harness)
        .await
        .unwrap();
    h.send_first_prompt(&handle, "Reply with only READY-334.")
        .await
        .unwrap();
    let settled = Instant::now() + Duration::from_secs(60);
    loop {
        let state = h
            .agents()
            .await
            .unwrap()
            .into_iter()
            .find(|agent| agent.pane_id == handle)
            .map(|agent| agent.status);
        if state.as_deref().is_some_and(|state| state != "working") {
            break;
        }
        assert!(
            Instant::now() < settled,
            "first turn did not settle: {state:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let draft = "HUMAN-DRAFT-334";
    h.run(&["pane", "send-text", &handle, draft]).await.unwrap();
    let draft_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let screen = h.screen(&handle).await.unwrap().join("\n");
        if screen.contains(draft) {
            break;
        }
        assert!(
            Instant::now() < draft_deadline,
            "synthetic draft never reached the composer:\n{screen}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    crate::delivery_channel::deliver(
        &mailbox,
        1,
        "[ssf] Native idle event. Reply with only IDLE-RECEIVED-334.",
    )
    .await
    .unwrap();
    h.run(&[
        "agent",
        "wait",
        &handle,
        "--until",
        "working",
        "--timeout",
        "15000",
    ])
    .await
    .unwrap();
    let settled = Instant::now() + Duration::from_secs(60);
    loop {
        let state = h
            .agents()
            .await
            .unwrap()
            .into_iter()
            .find(|agent| agent.pane_id == handle)
            .map(|agent| agent.status);
        if state.as_deref().is_some_and(|state| state != "working") {
            break;
        }
        assert!(
            Instant::now() < settled,
            "idle event did not settle: {state:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let idle_screen = h.screen(&handle).await.unwrap().join("\n");
    assert!(idle_screen.contains("IDLE-RECEIVED-334"), "{idle_screen}");
    assert!(
        idle_screen.contains(draft),
        "draft was disturbed:\n{idle_screen}"
    );
    h.run(&["pane", "send-keys", &handle, "ctrl+u"])
        .await
        .unwrap();

    // The window #390 closes, on a real harness: an event handed over while
    // a tool call is in flight is not acknowledged until the session's
    // transcript records it, so killing the harness here leaves it pending
    // and the relaunch injects it. A restarted harness must resume that
    // transcript, record the event once, and acknowledge the file -- not
    // submit it again through its terminal.
    let busy =
        "[ssf] Native kill-window event. After the current turn reply with only KILLWINDOW-390.";
    h.send_prompt(
        &handle,
        "Use the bash tool to run `sleep 30`, then reply with only NOWINDOW-334.",
    )
    .await
    .unwrap();
    h.run(&[
        "agent",
        "wait",
        &handle,
        "--until",
        "working",
        "--timeout",
        "15000",
    ])
    .await
    .unwrap();
    crate::delivery_channel::deliver(&mailbox, 2, busy)
        .await
        .unwrap();
    let kill_pending = wait_for_pending(&mailbox, 2).await;
    // OpenCode stores a prompt the moment it is submitted, busy or not, so its
    // record -- and the acknowledgement -- can come before the kill.
    assert!(
        harness == "opencode" || !kill_pending.with_extension("json.ack").is_file(),
        "an event the transcript does not hold was acknowledged (#390): {}",
        kill_pending.display()
    );
    stop_harness(&h, &handle, &harness, &mailbox).await;
    let stopped = Instant::now() + Duration::from_secs(10);
    while h
        .agents()
        .await
        .unwrap()
        .iter()
        .any(|agent| agent.pane_id == handle)
    {
        assert!(
            Instant::now() < stopped,
            "harness did not exit for relaunch"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let recovery = h
        .deliver(
            &wt.id,
            Some(&handle),
            &Relaunch {
                command: &command,
                resume_command: None,
                harness: &harness,
                title: "native delivery · #334",
                text: None,
                first_prompt: FirstPrompt::No,
                channel: Some((&mailbox, 2)),
            },
            busy,
        )
        .await
        .unwrap();
    assert!(recovery.relaunched && recovery.resumed);
    let handle = recovery.handle;
    let recorded = Instant::now() + Duration::from_secs(60);
    while !kill_pending.with_extension("json.ack").is_file() {
        assert!(
            Instant::now() < recorded,
            "the killed event was never recorded:\n{}",
            h.screen(&handle).await.unwrap().join("\n")
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let transcript = live_transcript(&harness, &mailbox, &root);
    assert_eq!(
        transcript.matches(injected_marker(&harness)).count(),
        2,
        "the killed event was not recorded exactly once, or the idle event was \
duplicated:\n{transcript}"
    );
    assert!(
        transcript.contains("KILLWINDOW-390"),
        "the event lost to the kill window was never injected:\n{transcript}"
    );

    // The other half of the window: the transcript recorded the event, but the
    // harness died before its file was renamed to `.ack`.  A relaunch must
    // acknowledge that existing record and not inject the event again.
    std::fs::rename(kill_pending.with_extension("json.ack"), &kill_pending).unwrap();
    stop_harness(&h, &handle, &harness, &mailbox).await;
    let stopped = Instant::now() + Duration::from_secs(10);
    while h
        .agents()
        .await
        .unwrap()
        .iter()
        .any(|agent| agent.pane_id == handle)
    {
        assert!(
            Instant::now() < stopped,
            "harness did not exit for the second relaunch"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let reack = h
        .deliver(
            &wt.id,
            Some(&handle),
            &Relaunch {
                command: &command,
                resume_command: None,
                harness: &harness,
                title: "native delivery · #334",
                text: None,
                first_prompt: FirstPrompt::No,
                channel: Some((&mailbox, 2)),
            },
            busy,
        )
        .await
        .unwrap();
    assert!(reack.relaunched && reack.resumed);
    let handle = reack.handle;
    let again = Instant::now() + Duration::from_secs(60);
    while !kill_pending.with_extension("json.ack").is_file() {
        assert!(
            Instant::now() < again,
            "a recorded event whose file was still pending was never \
acknowledged:\n{}",
            h.screen(&handle).await.unwrap().join("\n")
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let transcript = live_transcript(&harness, &mailbox, &root);
    assert_eq!(
        transcript.matches(injected_marker(&harness)).count(),
        2,
        "a relaunch re-injected an event the transcript already recorded:\n{transcript}"
    );

    h.send_prompt(
        &handle,
        "Use the bash tool to run `sleep 4`, then reply with only PRIMARY-DONE-334.",
    )
    .await
    .unwrap();
    h.run(&[
        "agent",
        "wait",
        &handle,
        "--until",
        "working",
        "--timeout",
        "15000",
    ])
    .await
    .unwrap();
    crate::delivery_channel::deliver(
        &mailbox,
        3,
        "[ssf] Native busy event. After the current turn reply with only FOLLOWUP-RECEIVED-334.",
    )
    .await
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let screen = h.screen(&handle).await.unwrap().join("\n");
        if screen.contains("FOLLOWUP-RECEIVED-334") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "busy event was not handled:\n{screen}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    // Grok shows a queued prompt's text on screen before it runs, so the
    // answer is read from the conversation: the busy event waited for the
    // turn in flight to finish, and was answered after it.
    if harness == "grok" {
        loop {
            let transcript = live_transcript(&harness, &mailbox, &root);
            let answer = transcript.find("\"text\":\"FOLLOWUP-RECEIVED-334\"");
            if let Some(answer) = answer {
                let primary = transcript
                    .find("\"text\":\"PRIMARY-DONE-334\"")
                    .expect("the turn in flight was cut short by the busy event");
                assert!(primary < answer, "the busy event ran before the turn ended");
                assert!(
                    !transcript.contains("\"stop_reason\":\"cancelled\""),
                    "a turn was cancelled:\n{transcript}"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "busy event was not answered:\n{transcript}"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    h.remove_worktree(&wt.id).await.unwrap();
    if let Ok(v) = h.run(&["worktree", "list", "--cwd", &root]).await
        && let Some(src) = v
            .pointer("/source/source_workspace_id")
            .and_then(Value::as_str)
    {
        let _ = h.run(&["workspace", "close", src]).await;
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// Codex 0.152.0 on its directory-trust dialog, which herdr 0.8.2
/// reports as `idle` (#121).
const CODEX_TRUST: &str = "\
  You are running Codex in /tmp/w.worktrees/issue-3-x

  Do you trust the contents of this directory? Working with untrusted
  contents comes with higher risk of prompt injection.

› 1. Yes, continue
  2. No, quit";

#[test]
fn the_screen_decides_what_to_do_after_a_wait() {
    // The state herdr reports does not say whether a dialog is up:
    // Codex's is `idle`, Claude Code's is `blocked`, and both have to
    // be answered before the first prompt goes in.
    assert_eq!(
        settle_step("idle", CODEX_TRUST),
        Settle::Answer(driver::TrustAnswer::Enter)
    );
    let claude = "Quick safety check: Is this a project you created or one you trust?\n\
❯ No, exit\n  Yes, I trust this folder\nEnter to confirm · Esc to cancel";
    assert_eq!(
        settle_step("blocked", claude),
        Settle::Answer(driver::TrustAnswer::DownEnter)
    );
    // Codex with the dialog answered and an empty composer: ready.
    let ready = "\
>_ You are using OpenAI Codex in /tmp/w.worktrees/issue-3-x

  To get started, describe a task or try one of these commands:

  /init - create an AGENTS.md file
▌ Ask Codex to do anything";
    assert_eq!(settle_step("idle", ready), Settle::Ready);
    // A question that is not about trust: ssf has no answer for it, and
    // a prompt pasted into it is lost, so the session is held (#541).
    let question = "Do you want to create an AGENTS.md file?\n❯ Yes\n  No";
    assert_eq!(settle_step("blocked", question), Settle::Held);
    assert_eq!(settle_step("working", ready), Settle::Ready);
    // An agent already working is past any first-run dialog, whatever
    // the screen has on it: what is there is its own output.
    assert_eq!(settle_step("working", CODEX_TRUST), Settle::Ready);
}

#[test]
fn a_blank_screen_is_not_ready_for_a_prompt() {
    // herdr 0.9 reports OpenCode 1.18 `idle` seconds before its TUI draws
    // a composer; a prompt sent then is lost (#508).
    assert_eq!(settle_step("idle", ""), Settle::Undrawn);
    assert_eq!(settle_step("idle", "\n   \n\n"), Settle::Undrawn);
    assert_eq!(settle_step("blocked", "\n\n"), Settle::Undrawn);
    // A working agent has taken input, whatever it has drawn.
    assert_eq!(settle_step("working", ""), Settle::Ready);
    let drawn = "  ┃  Ask anything… \"Fix a TODO in the codebase\"\n  ┃  Build auto";
    assert_eq!(settle_step("idle", drawn), Settle::Ready);
}

#[test]
fn a_stall_is_a_dialog_only_when_the_screen_shows_one() {
    let prompt = "You are working on mikekelly/simple-software-factory#121.\n\
Do you trust the contents of this directory? is what Codex asks.";
    // Codex's dialog: answer it and send the prompt again.
    assert_eq!(
        after_stall(CODEX_TRUST, prompt),
        AfterStall::Retry(driver::TrustAnswer::Enter)
    );
    // The prompt itself in the composer, quoting the dialog's wording:
    // text, not a dialog, so the send is taken as delivered.
    let pasted = "▌ You are working on mikekelly/simple-software-factory#121.\n\
▌ Do you trust the contents of this directory? is what Codex asks.";
    assert_eq!(after_stall(pasted, prompt), AfterStall::Submit);
    // A ready composer says nothing about where the submission got to, so
    // it is observed and never accepted merely because the wait stalled.
    let ready = "  Oh My Pi\n\n▌ Ask anything";
    assert_eq!(after_stall(ready, prompt), AfterStall::Observe);
}

#[test]
fn a_collapsed_omp_card_identifies_the_prompt() {
    let prompt = "<attachment>\n\
# GitHub issue #14: Phase 1a — Compatibility spike\n\
https://github.com/mikekelly/inception/issues/14\n\
\n\
Opened by @MikeKellyBot on 2026-09-13.\n\
</attachment>";
    let screen = "╭─── file #1 ───╮\n\
│# GitHub is…   │\n\
│https://git…   │\n\
│               │\n\
│Opened by @…   │\n\
╰ +125 lines ───╯";
    assert!(prompt_on_screen(screen, prompt));
    assert_eq!(after_stall(screen, prompt), AfterStall::Submit);
    let history = format!(
        "{screen}\n{}",
        (0..30)
            .map(|n| format!("assistant response line {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(!prompt_on_screen(&history, prompt));
}

/// #317: once the durable attempt marker exists, an idle session whose
/// screen no longer shows the prompt may already have consumed it. Recovery
/// must not turn the original assignment into a steering interjection.
#[tokio::test]
async fn ambiguous_first_prompt_recovery_does_not_resend() {
    let base = std::env::temp_dir().join(format!(
        "ssf-herdr-first-prompt-recovery-ambiguous-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let fake = base.join("herdr");
    crate::test_support::write_executable(
        &fake,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$(dirname "$0")/calls"
case "$1 $2" in
  "agent list")
    echo '{"agents":[{"agent":"codex","agent_status":"idle","pane_id":"w7:p1","workspace_id":"w7"}]}'
    ;;
  "pane read")
    printf '%s\n' 'Finished the requested work.' '▌ Ask Codex to do anything'
    ;;
  "agent prompt")
    echo 'the initial prompt was sent again' >&2
    exit 1
    ;;
esac
"#,
    );
    let h = Herdr::new(HerdrConfig {
        command: fake.to_string_lossy().into_owned(),
        ..HerdrConfig::default()
    });

    h.recover_first_prompt(
        "w7:p1",
        "# GitHub issue #317\nhttps://github.com/example/repo/issues/317",
    )
    .await
    .unwrap();

    let calls = std::fs::read_to_string(base.join("calls")).unwrap();
    assert!(calls.contains("agent list"), "{calls}");
    assert!(calls.contains("pane read"), "{calls}");
    assert!(!calls.contains("agent prompt"), "{calls}");
    std::fs::remove_dir_all(base).unwrap();
}

/// Recovery must not turn a failed observation into a successful seed: the
/// next pass needs to retry once the pane can be inspected safely.
#[tokio::test]
async fn unreadable_first_prompt_recovery_is_not_accepted_or_resent() {
    let base = std::env::temp_dir().join(format!(
        "ssf-herdr-first-prompt-recovery-unreadable-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let fake = base.join("herdr");
    crate::test_support::write_executable(
        &fake,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$(dirname "$0")/calls"
case "$1 $2" in
  "agent list")
    echo '{"agents":[{"agent":"codex","agent_status":"idle","pane_id":"w7:p1","workspace_id":"w7"}]}'
    ;;
  "pane read")
    echo '[unavailable] pane cannot be read' >&2
    exit 1
    ;;
  "agent prompt")
    echo 'the initial prompt was sent again' >&2
    exit 1
    ;;
esac
"#,
    );
    let h = Herdr::new(HerdrConfig {
        command: fake.to_string_lossy().into_owned(),
        ..HerdrConfig::default()
    });

    let error = h
        .recover_first_prompt(
            "w7:p1",
            "# GitHub issue #317\nhttps://github.com/example/repo/issues/317",
        )
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("pane cannot be read"),
        "{error:#}"
    );
    let calls = std::fs::read_to_string(base.join("calls")).unwrap();
    assert!(calls.contains("agent list"), "{calls}");
    assert!(calls.contains("pane read"), "{calls}");
    assert!(!calls.contains("agent prompt"), "{calls}");
    std::fs::remove_dir_all(base).unwrap();
}

/// #317's observed OMP sequence: the composer is identifiable and Enter is
/// delivered, but Herdr keeps reporting `idle`. The successful submission is
/// accepted so the next onboarding pass cannot submit the assignment again.
#[tokio::test]
async fn submitted_first_prompt_is_accepted_when_working_cannot_be_observed() {
    let base = std::env::temp_dir().join(format!(
        "ssf-herdr-first-prompt-recovery-unobserved-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let fake = base.join("herdr");
    crate::test_support::write_executable(
        &fake,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$(dirname "$0")/calls"
case "$1 $2" in
  "agent list")
    echo '{"agents":[{"agent":"omp","agent_status":"idle","pane_id":"w6:p1","workspace_id":"w6"}]}'
    ;;
  "pane read")
    printf '%s\n' '▌ # GitHub issue #317' '▌ https://github.com/example/repo/issues/317'
    ;;
  "agent send-keys")
    ;;
  "agent wait")
    echo '{"error":{"code":"timeout","message":"timed out waiting for agent status"}}' >&2
    exit 1
    ;;
  "agent prompt")
    echo 'the initial prompt was sent again' >&2
    exit 1
    ;;
esac
"#,
    );
    let h = Herdr::new(HerdrConfig {
        command: fake.to_string_lossy().into_owned(),
        ..HerdrConfig::default()
    });

    h.recover_first_prompt(
        "w6:p1",
        "# GitHub issue #317\nhttps://github.com/example/repo/issues/317",
    )
    .await
    .unwrap();

    let calls = std::fs::read_to_string(base.join("calls")).unwrap();
    assert_eq!(
        calls.matches("agent send-keys w6:p1 enter").count(),
        1,
        "{calls}"
    );
    assert_eq!(calls.matches("agent wait w6:p1").count(), 1, "{calls}");
    assert!(!calls.contains("agent prompt"), "{calls}");
    std::fs::remove_dir_all(base).unwrap();
}

#[tokio::test]
async fn submitted_first_prompt_propagates_operational_wait_failure() {
    let base = std::env::temp_dir().join(format!(
        "ssf-herdr-first-prompt-recovery-failed-wait-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let fake = base.join("herdr");
    crate::test_support::write_executable(
        &fake,
        r#"#!/bin/sh
case "$1 $2" in
  "agent send-keys")
    ;;
  "agent wait")
    echo '{"error":{"code":"not_found","message":"no such pane"}}' >&2
    exit 1
    ;;
esac
"#,
    );
    let h = Herdr::new(HerdrConfig {
        command: fake.to_string_lossy().into_owned(),
        ..HerdrConfig::default()
    });

    let error = h.submit_existing_prompt("w6:p1").await.unwrap_err();
    assert!(
        error.to_string().contains("[not_found] no such pane"),
        "{error:#}"
    );
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn agent_prompt_failures_are_told_apart() {
    // herdr sent nothing: the agent is at a question.
    assert_eq!(
        prompt_failure("herdr agent prompt w7:p1 <text> failed: [agent_blocked] agent is blocked"),
        PromptFailure::Blocked
    );
    // The text went in but nothing happened with it, which is what a
    // paste a dialog swallowed looks like.
    assert_eq!(
        prompt_failure(
            "herdr agent prompt w7:p1 <text> failed: [agent_prompt_stalled] agent did not \
start working within 5000ms"
        ),
        PromptFailure::Stalled
    );
    // A state change that never reached `working` in time: the text
    // went in too, so the screen decides, not the error.
    assert_eq!(
        prompt_failure(
            "herdr agent prompt w7:p1 <text> failed: [timeout] no matching state within \
15000ms"
        ),
        PromptFailure::Stalled
    );
    assert_eq!(
        prompt_failure("herdr agent prompt w7:p1 <text> failed: [not_found] no such pane"),
        PromptFailure::Other
    );
}

#[test]
fn parses_agent_and_pane_lists() {
    let agents = parse_agents(&json!({"agents": [
        {"agent": "claude", "agent_status": "working", "cwd": "/p/w.worktrees/issue-3-x",
         "pane_id": "w2:p1", "workspace_id": "w2", "terminal_title_stripped": "Running tests"},
        {"agent": "omp", "agent_status": "idle", "pane_id": "w3:p1", "workspace_id": "w3"}
    ]}));
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0].pane_id, "w2:p1");
    assert_eq!(agents[0].status, "working");
    assert_eq!(agents[0].title.as_deref(), Some("Running tests"));
    let panes = parse_panes(&json!({"panes": [
        {"pane_id": "w2:p1", "workspace_id": "w2", "cwd": "/p/w.worktrees/issue-3-x", "agent": "claude"},
        {"pane_id": "w2:p2", "workspace_id": "w2", "cwd": "/p/w.worktrees/issue-3-x"}
    ]}));
    assert_eq!(panes[1].agent, None);
    assert_eq!(panes[0].agent.as_deref(), Some("claude"));
    assert_eq!(panes[0].workspace_id, "w2");
}

#[test]
fn activity_session_reference_must_match_the_live_harness() {
    let rows = parse_agents(&json!({"agents": [
        {"pane_id": "w1:p1", "agent": "codex", "agent_session":
            {"agent": "codex", "kind": "id", "value": "session-1"}},
        {"pane_id": "w2:p1", "agent": "claude", "agent_session":
            {"agent": "codex", "kind": "id", "value": "old-session"}},
        {"pane_id": "w3:p1", "agent": "codex", "agent_session":
            {"agent": "codex", "kind": "path", "value": "/tmp/transcript"}}
    ]}));
    assert_eq!(rows[0].session_id.as_deref(), Some("session-1"));
    assert_eq!(rows[1].session_id, None);
    assert_eq!(rows[2].session_id, None);
}

#[test]
fn worktree_list_binds_workspaces_to_checkouts() {
    // `herdr worktree list` on 0.8.2, trimmed.
    let v = json!({"source": {"repo_root": "/p/widgets", "source_workspace_id": "wW"},
        "type": "worktree_list", "worktrees": [
        {"branch": "master", "is_linked_worktree": false, "is_prunable": false,
         "open_workspace_id": "wW", "path": "/p/widgets"},
        {"branch": "bot/issue-3-x", "is_linked_worktree": true, "is_prunable": true,
         "open_workspace_id": "wX", "path": "/p/widgets.worktrees/issue-3-x"},
        {"branch": "bot/issue-4-y", "is_linked_worktree": true, "is_prunable": false,
         "path": "/p/widgets.worktrees/issue-4-y"}
    ]});
    assert_eq!(
        bound_worktree(&v, "wX").as_deref(),
        Some("/p/widgets.worktrees/issue-3-x")
    );
    assert_eq!(bound_worktree(&v, "wW").as_deref(), Some("/p/widgets"));
    assert_eq!(bound_worktree(&v, "wZ"), None);
    assert_eq!(
        workspace_on(&v, "/p/widgets.worktrees/issue-3-x").as_deref(),
        Some("wX")
    );
    assert_eq!(workspace_on(&v, "/p/widgets.worktrees/issue-4-y"), None);
    assert_eq!(workspace_on(&v, "/elsewhere"), None);
    assert_eq!(bound_worktree(&json!({"worktrees": []}), "wX"), None);
}

/// #131: a resumed Claude Code with queued messages is `working` at
/// once, and `herdr agent wait` without `--until` never returns on
/// `working`, so the wait names every state but `unknown`.
#[test]
fn settle_waits_for_working_as_well_as_idle() {
    let args = settle_wait_args("w2:p1", "88988");
    assert_eq!(&args[..5], ["agent", "wait", "w2:p1", "--timeout", "88988"]);
    let until: Vec<&str> = args
        .windows(2)
        .filter(|w| w[0] == "--until")
        .map(|w| w[1])
        .collect();
    assert_eq!(until, ["idle", "working", "blocked", "done"]);
    assert!(!until.contains(&"unknown"));
    // And `working` on its own is settled, whatever the screen shows.
    assert_eq!(
        settle_step("working", "No conversation found with session ID"),
        Settle::Ready
    );
}

/// #131/#133: an agent in the pane is the resumed conversation
/// whatever the wait said or the screen shows; only an empty pane
/// fails the resume, and the screen names the reason there.
#[test]
fn a_resume_is_judged_by_the_agent_in_the_pane() {
    let not_found = vec!["Error: No conversation found with session ID abc".to_string()];
    let shell = vec!["$ ".to_string()];
    let timed_out = Err("[timeout] timed out waiting for agent status".to_string());
    // The three shapes seen live: at work at once, idle, alive past the wait.
    assert_eq!(
        resume_verdict(&Ok("working".into()), true, &[]),
        ResumeVerdict::Resumed("working".into())
    );
    assert_eq!(
        resume_verdict(&Ok("idle".into()), true, &[]),
        ResumeVerdict::Resumed("idle".into())
    );
    assert_eq!(
        resume_verdict(&timed_out, true, &[]),
        ResumeVerdict::Resumed("unsettled".into())
    );
    // A live agent quoting the failure text in its own output is kept.
    assert_eq!(
        resume_verdict(&Ok("idle".into()), true, &not_found),
        ResumeVerdict::Resumed("idle".into())
    );
    // No agent: the screen says why, or the wait does.
    assert_eq!(
        resume_verdict(&timed_out, false, &not_found),
        ResumeVerdict::Failed("the harness could not find its session".into())
    );
    assert_eq!(
        resume_verdict(&Ok("done".into()), false, &not_found),
        ResumeVerdict::Failed("the harness could not find its session".into())
    );
    assert_eq!(
        resume_verdict(&timed_out, false, &shell),
        ResumeVerdict::Failed("[timeout] timed out waiting for agent status".into())
    );
    assert_eq!(
        resume_verdict(&Ok("idle".into()), false, &shell),
        ResumeVerdict::Failed("the harness exited after the wait".into())
    );
}

#[test]
fn workspace_cwd_tells_root_and_item() {
    assert_eq!(
        root_and_item("/p/widgets.worktrees/issue-3-x"),
        (Some("/p/widgets".into()), Some(3))
    );
    // A reviewer worktree from before #115 is on the repository but
    // belongs to no item.
    assert_eq!(
        root_and_item("/p/widgets.worktrees/review-9"),
        (Some("/p/widgets".into()), None)
    );
    assert_eq!(root_and_item("/home/me/code/thing"), (None, None));
}

#[test]
fn joins_workspaces_panes_and_agents() {
    // w2's shell has cd'd away: herdr's binding places it, not the pane.
    // w7 is bound but has no pane; w8 is not on a worktree, so its pane
    // cwd is all there is.
    let ws = json!({"workspaces": [
        {"workspace_id": "w2", "label": "issue-3-x", "agent_status": "working",
         "worktree": {"checkout_path": "/p/widgets.worktrees/issue-3-x",
                      "is_linked_worktree": true, "repo_root": "/p/widgets"}},
        {"workspace_id": "w5", "label": "scratch", "agent_status": "idle"},
        {"workspace_id": "w7", "label": "issue-4-y", "agent_status": "idle",
         "worktree": {"checkout_path": "/p/widgets.worktrees/issue-4-y"}},
        {"workspace_id": "w8", "label": "loose", "agent_status": "idle"}
    ]});
    let panes = vec![
        Pane {
            pane_id: "w2:p1".into(),
            workspace_id: "w2".into(),
            cwd: Some("/".into()),
            agent: Some("claude".into()),
        },
        Pane {
            pane_id: "w8:p1".into(),
            workspace_id: "w8".into(),
            cwd: Some("/p/widgets.worktrees/issue-5-z".into()),
            agent: None,
        },
    ];
    let agents = vec![Agent {
        pane_id: "w2:p1".into(),
        workspace_id: "w2".into(),
        kind: "claude".into(),
        status: "working".into(),
        cwd: None,
        title: Some("cargo test".into()),
        session_id: None,
    }];
    let rows = join_ps(&ws, &panes, &agents);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].worktree_id, "w2@/p/widgets.worktrees/issue-3-x");
    assert_eq!(rows[0].repo_id, "/p/widgets");
    assert_eq!(rows[0].path, "/p/widgets.worktrees/issue-3-x");
    assert_eq!(rows[1].worktree_id, "w5");
    assert_eq!(rows[2].worktree_id, "w7@/p/widgets.worktrees/issue-4-y");
    assert_eq!(rows[2].linked_issue, Some(4));
    assert_eq!(rows[2].live_terminals, 0);
    assert_eq!(rows[3].worktree_id, "w8@/p/widgets.worktrees/issue-5-z");
    assert_eq!(rows[3].linked_issue, Some(5));
    assert_eq!(
        split_id(&rows[0].worktree_id),
        ("w2", Some("/p/widgets.worktrees/issue-3-x"))
    );
    assert_eq!(split_id("w5"), ("w5", None));
    assert_eq!(rows[0].linked_issue, Some(3));
    assert!(rows[0].is_working());
    assert_eq!(
        rows[0]
            .primary_agent()
            .unwrap()
            .last_assistant_message
            .as_deref(),
        Some("cargo test")
    );
    assert_eq!(rows[1].linked_issue, None);
    assert!(!rows[1].is_working());
    assert!(rows[1].primary_agent().is_none());
}

/// Exercise creation and recovery without connecting to a herdr server.
#[tokio::test]
async fn workspace_labels_use_github_repo_and_number_on_create_and_reopen() {
    let base = std::env::temp_dir().join(format!("ssf-herdr-labels-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let root = base.join("custom-checkout");
    std::fs::create_dir_all(&root).unwrap();
    for args in [
        vec!["init", "-q", "-b", "master"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let fake = base.join("herdr");
    crate::test_support::write_executable(
        &fake,
        r#"#!/bin/sh
case "$1 $2" in
  "worktree list") echo '{"worktrees": []}' ;;
  "worktree open")
    printf '%s\n' "$@" >> "$(dirname "$0")/args"
    echo '{"workspace":{"workspace_id":"w7"}}' ;;
esac
"#,
    );
    let h = Herdr::new(HerdrConfig {
        command: fake.to_string_lossy().into_owned(),
        ..HerdrConfig::default()
    });
    let root = root.to_str().unwrap();
    let created = h
        .create_worktree(
            root,
            "owner/widgets",
            "issue-282-long-title",
            282,
            "ssf: test",
            None,
        )
        .await
        .unwrap();
    let reopened = h
        .find_worktree_for_issue(root, "owner/widgets", 282)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(created.id, reopened.id);
    assert!(created.path.ends_with("/issue-282-long-title"));
    assert_eq!(
        created.branch.as_deref(),
        Some("refs/heads/bot/issue-282-long-title")
    );
    let args = std::fs::read_to_string(base.join("args")).unwrap();
    assert_eq!(
        args.matches("--label\nwidgets-282\n--no-focus").count(),
        2,
        "{args}"
    );
    std::fs::remove_dir_all(base).unwrap();
}

/// The failure mikekelly/pg-cbor-schema#33 hit: an item's story is its body
/// plus every comment, and that issue's was past 128 KiB, which is all the
/// kernel allows one argument. herdr takes prompt text as an argument, so
/// the spawn failed before herdr ran at all -- "spawning herdr (is herdr
/// installed?): Argument list too long (os error 7)" -- and the handover
/// left the item blocked with nothing running. The prompt has to reach the
/// pane in pieces that fit.
///
/// The prompt here is built by hand rather than assembled: the test needs
/// one past the kernel's 128 KiB, which a story from `initial_prompt` does
/// not necessarily reach now that #406 caps its activity -- one whose caps
/// are lifted, or whose description sits at GitHub's 65,536-character
/// maximum, still can. Live deliveries remain unbounded, and both take
/// this path.
#[tokio::test]
async fn a_first_prompt_too_long_for_one_argument_reaches_the_pane_whole() {
    let base = std::env::temp_dir().join(format!(
        "ssf-herdr-long-first-prompt-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let fake = base.join("herdr");
    crate::test_support::write_executable(
        &fake,
        r#"#!/bin/sh
d="$(dirname "$0")"
case "$1 $2" in
  "pane send-text")
    printf '%s' "$4" >> "$d/pasted"
    printf 'send-text %s\n' "${#4}" >> "$d/calls"
    ;;
  *)
    printf '%s\n' "$*" >> "$d/calls"
    ;;
esac
"#,
    );
    let h = Herdr::new(HerdrConfig {
        command: fake.to_string_lossy().into_owned(),
        ..HerdrConfig::default()
    });

    // Past the kernel's cap on one argument, with a multi-byte character
    // straddling the chunk boundary: two writes must not cut it in half.
    let prompt = format!(
        "{}→{}",
        "a".repeat(SEND_TEXT_CHUNK - 1),
        "z".repeat(SEND_TEXT_CHUNK)
    );
    assert!(
        prompt.len() > 128 * 1024,
        "the prompt has to be longer than one argument can carry"
    );

    h.send_first_prompt("w7:p1", &prompt).await.unwrap();

    let pasted = std::fs::read_to_string(base.join("pasted")).unwrap();
    assert_eq!(pasted, format!("{PASTE_START}{prompt}{PASTE_END}"));
    let calls = std::fs::read_to_string(base.join("calls")).unwrap();
    assert!(calls.contains("pane send-keys w7:p1 enter"), "{calls}");
    assert!(!calls.contains("agent prompt"), "{calls}");
    let writes: Vec<usize> = calls
        .lines()
        .filter_map(|line| line.strip_prefix("send-text "))
        .map(|len| len.parse().unwrap())
        .collect();
    assert!(writes.len() > 1, "the prompt went in one write: {calls}");
    assert!(
        writes
            .iter()
            .all(|len| *len <= SEND_TEXT_CHUNK + PASTE_START.len() + PASTE_END.len()),
        "a write is still argument-sized: {writes:?}"
    );
    std::fs::remove_dir_all(base).unwrap();
}

/// A delivery carries whatever activity arrived since the last pass, which
/// is as unbounded as the story is, and reaches the pane the same way. A
/// harness that answers a paste this size with a representation choice
/// spends the first Enter on that and leaves the prompt in the composer,
/// so the paste is submitted again -- and only ever pasted once.
#[tokio::test]
async fn a_delivery_too_long_for_one_argument_reaches_the_pane_whole() {
    let base = std::env::temp_dir().join(format!("ssf-herdr-long-delivery-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let fake = base.join("herdr");
    crate::test_support::write_executable(
        &fake,
        r#"#!/bin/sh
d="$(dirname "$0")"
case "$1 $2" in
  "pane send-text")
    printf '%s' "$4" >> "$d/pasted"
    ;;
  "pane read")
    # What OMP's composer shows for a wrapped paste this size.
    printf '%s\n' '│bbbbbbbbbb…│' '╰ +1423 lin… ╯'
    ;;
esac
printf '%s\n' "$*" >> "$d/calls"
"#,
    );
    let h = Herdr::new(HerdrConfig {
        command: fake.to_string_lossy().into_owned(),
        ..HerdrConfig::default()
    });

    let body = "b".repeat(SEND_TEXT_CHUNK * 3);
    h.send_prompt("w7:p1", &body).await.unwrap();

    let pasted = std::fs::read_to_string(base.join("pasted")).unwrap();
    assert_eq!(pasted, format!("{PASTE_START}{body}{PASTE_END}"));
    let calls = std::fs::read_to_string(base.join("calls")).unwrap();
    assert!(!calls.contains("agent prompt"), "{calls}");
    assert_eq!(
        calls.matches("pane send-keys w7:p1 enter").count(),
        2,
        "the prompt left in the composer was not submitted again: {calls}"
    );

    // A prompt that fits still goes through herdr's own prompt handling,
    // which is what reports the agent as blocked and waits on it.
    h.send_prompt("w7:p1", "short enough").await.unwrap();
    let calls = std::fs::read_to_string(base.join("calls")).unwrap();
    assert!(calls.contains("agent prompt w7:p1 short enough"), "{calls}");
    std::fs::remove_dir_all(base).unwrap();
}

/// Against a running herdr server: a prompt past the argument limit reaches
/// the pane byte for byte, framed as the one bracketed paste `agent prompt`
/// would have made, when it goes in as several writes. The pane runs
/// `stty raw` and `head` first, so what it reads is what herdr wrote, with
/// no shell or line discipline in between.
/// `cargo test herdr_live_long_paste -- --ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn herdr_live_long_paste_reaches_the_pane_whole() {
    let base = std::env::temp_dir().join(format!("ssf-herdr-long-paste-{}", std::process::id()));
    let root = base.join("widgets").to_string_lossy().to_string();
    std::fs::create_dir_all(&root).unwrap();
    for args in [
        vec!["init", "-q", "-b", "master"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(&args)
                .status()
                .unwrap()
                .success()
        );
    }
    let h = Herdr::new(HerdrConfig::default());
    h.status().await.unwrap();
    let wt = h
        .create_worktree(
            &root,
            "owner/widgets",
            "issue-33-paste",
            33,
            "ssf: test",
            None,
        )
        .await
        .unwrap();
    let (ws, _) = split_id(&wt.id);
    let pane = h.panes(ws).await.unwrap()[0].pane_id.clone();

    // Past the kernel's cap on one argument, with a multi-byte character
    // and a newline straddling chunk boundaries.
    let payload = format!(
        "{}→\n{}",
        "a".repeat(SEND_TEXT_CHUNK - 1),
        "z".repeat(SEND_TEXT_CHUNK)
    );
    let expected = format!("{PASTE_START}{payload}{PASTE_END}");
    let out = base.join("pasted.bin");
    let _ = std::fs::remove_file(&out);
    h.run(&[
        "pane",
        "run",
        &pane,
        &format!(
            "stty raw -echo; head -c {} > {}",
            expected.len(),
            out.display()
        ),
    ])
    .await
    .unwrap();

    let started = Instant::now();
    h.type_text(&pane, &payload).await.unwrap();
    while started.elapsed() < Duration::from_secs(20) {
        if std::fs::metadata(&out).is_ok_and(|m| m.len() as usize == expected.len()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let read = std::fs::read(&out).unwrap();
    assert_eq!(
        read.len(),
        expected.len(),
        "the pane read {} of {} bytes",
        read.len(),
        expected.len()
    );
    assert_eq!(read, expected.as_bytes(), "the paste did not arrive whole");

    let _ = h.run(&["pane", "run", &pane, "stty sane"]).await;
    h.remove_worktree(&wt.id).await.unwrap();
    if let Ok(v) = h.run(&["worktree", "list", "--cwd", &root]).await
        && let Some(src) = v
            .pointer("/source/source_workspace_id")
            .and_then(Value::as_str)
    {
        let _ = h.run(&["workspace", "close", src]).await;
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// Against a running herdr server: a harness is launched and given a first
/// prompt past the argument limit -- the case that failed for
/// mikekelly/pg-cbor-schema#33 -- and answers a token at the very end of
/// it, so the whole prompt arrived and was submitted. The harness is
/// `SSF_LIVE_HARNESS` with `SSF_LIVE_COMMAND` (Codex by default).
/// `cargo test herdr_live_long_first_prompt -- --ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn herdr_live_long_first_prompt() {
    let harness = std::env::var("SSF_LIVE_HARNESS").unwrap_or_else(|_| "codex".into());
    let command = std::env::var("SSF_LIVE_COMMAND")
        .unwrap_or_else(|_| crate::models::default_command(&harness));
    let base = std::env::temp_dir().join(format!(
        "ssf-long-first-prompt-{harness}-{}",
        std::process::id()
    ));
    let root = base.join("widgets").to_string_lossy().to_string();
    std::fs::create_dir_all(&root).unwrap();
    for args in [
        vec!["init", "-q", "-b", "master"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(&args)
                .status()
                .unwrap()
                .success()
        );
    }
    let h = Herdr::new(HerdrConfig::default());
    h.status().await.unwrap();
    let wt = h
        .create_worktree(
            &root,
            "owner/widgets",
            "issue-33-long-prompt",
            33,
            "ssf: test",
            None,
        )
        .await
        .unwrap();
    let mut story = String::new();
    for n in 0.. {
        if story.len() > 128 * 1024 {
            break;
        }
        story.push_str(&format!("Comment {n}: {}\n", "context line ".repeat(6)));
    }
    let prompt = format!(
        "# A story as long as a busy item's\n\n{story}\nReply with the single token \
LONG-PROMPT-OK and nothing else."
    );
    assert!(prompt.len() > 128 * 1024, "{} bytes", prompt.len());
    let handle = h
        .launch(&wt.id, &command, "long first prompt · #33", &harness)
        .await
        .unwrap();
    let sent = Instant::now();
    h.send_first_prompt(&handle, &prompt).await.unwrap();
    eprintln!("long first prompt taken in {:?}", sent.elapsed());
    h.run(&["agent", "wait", &handle, "--timeout", "120000"])
        .await
        .unwrap();
    let screen = h.screen(&handle).await.unwrap().join("\n");
    eprintln!("screen:\n{screen}");
    assert!(
        screen.contains("LONG-PROMPT-OK"),
        "the long first prompt was not answered"
    );
    h.remove_worktree(&wt.id).await.unwrap();
    if let Ok(v) = h.run(&["worktree", "list", "--cwd", &root]).await
        && let Some(src) = v
            .pointer("/source/source_workspace_id")
            .and_then(Value::as_str)
    {
        let _ = h.run(&["workspace", "close", src]).await;
    }
    let _ = std::fs::remove_dir_all(&base);
}

fn question_herdr(name: &str, blocked_listings: u32) -> (std::path::PathBuf, Herdr) {
    let base = std::env::temp_dir().join(format!("ssf-herdr-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let fake = base.join("herdr");
    // Blocked on a question for the first `blocked_listings` agent
    // listings, then idle at a composer: a person answered it.
    crate::test_support::write_executable(
        &fake,
        r#"#!/bin/sh
d="$(dirname "$0")"
printf '%s\n' "$*" >> "$d/calls"
n=$(cat "$d/n" 2>/dev/null || echo 0)
case "$1 $2" in
  "agent list")
    n=$((n+1)); echo $n > "$d/n"
    if [ $n -le LIMIT ]; then st=blocked; else st=idle; fi
    echo "{\"agents\":[{\"agent\":\"claude\",\"agent_status\":\"$st\",\"pane_id\":\"w7:p1\",\"workspace_id\":\"w7\"}]}"
    ;;
  "agent wait")
    echo '{"agent_status":"blocked"}'
    ;;
  "pane read")
    if [ $n -le LIMIT ]; then
      printf '%s\n' 'Choose the text style that looks best' '❯ 1. Dark mode' '  2. Light mode'
    else
      printf '%s\n' '> Try "fix lint errors"'
    fi
    ;;
  "agent prompt")
    echo '[agent_blocked] agent is blocked' >&2; exit 1
    ;;
esac
"#
        .replace("LIMIT", &blocked_listings.to_string()),
    );
    let h = Herdr::new(HerdrConfig {
        command: fake.to_string_lossy().into_owned(),
        ..HerdrConfig::default()
    });
    (base, h)
}

/// A harness at a question ssf does not know is reported at once as an
/// `AtQuestion` for its pane, with nothing typed into it: the caller holds
/// the session, and the launch does not wait on a person (#541).
#[tokio::test]
async fn an_unknown_question_is_reported_not_answered_or_waited_on() {
    let (base, h) = question_herdr("question-settle", 100);
    let started = std::time::Instant::now();
    let err = h.settle_harness("w7:p1", "claude").await.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        at_question(&err),
        Some(&AtQuestion {
            pane: "w7:p1".into(),
            first: true
        })
    );
    let calls = std::fs::read_to_string(base.join("calls")).unwrap();
    assert!(!calls.contains("send-keys"), "{calls}");
    assert!(!calls.contains("send-text"), "{calls}");
    std::fs::remove_dir_all(base).unwrap();
}

/// A prompt herdr refuses because the agent is at a question is never
/// pasted into it; the refusal is an `AtQuestion`, which the daemon holds.
#[tokio::test]
async fn a_blocked_prompt_is_held_not_pasted() {
    let (base, h) = question_herdr("question-prompt", 100);
    let err = h.send_prompt("w7:p1", "keep going").await.unwrap_err();
    assert_eq!(at_question(&err).map(|q| q.first), Some(false));
    let err = h.send_first_prompt("w7:p1", "the item").await.unwrap_err();
    assert_eq!(at_question(&err).map(|q| q.first), Some(true));
    let calls = std::fs::read_to_string(base.join("calls")).unwrap();
    assert!(!calls.contains("send-text"), "{calls}");
    assert!(!calls.contains("send-keys"), "{calls}");
    std::fs::remove_dir_all(base).unwrap();
}

/// The held session is looked at again on later passes: at the question
/// while herdr says blocked and the screen is unknown, not once answered.
#[tokio::test]
async fn a_question_is_seen_to_clear() {
    let (base, h) = question_herdr("question-clear", 1);
    assert!(h.at_question_now("w7:p1").await.unwrap());
    assert!(!h.at_question_now("w7:p1").await.unwrap());
    std::fs::remove_dir_all(base).unwrap();
}
