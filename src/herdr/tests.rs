use super::*;
use serde_json::json;

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
        .create_worktree(&root, "issue-3-x", "ssf: test", None)
        .await
        .unwrap();
    eprintln!("workspace {} at {}", wt.id, wt.path);
    assert!(h.worktree_exists(&wt.id).await.unwrap());
    assert!(!h.has_live_agent(&wt.id).await.unwrap());
    let found = h.find_worktree_for_issue(&root, 3).await.unwrap().unwrap();
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
        .create_worktree(&root, "issue-121-x", "ssf: test", None)
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
    // A question that is not about trust: ssf has no answer for it, so
    // the prompt goes in anyway and the harness queues it.
    let question = "Do you want to create an AGENTS.md file?\n❯ Yes\n  No";
    assert_eq!(settle_step("blocked", question), Settle::AskAnyway);
    assert_eq!(settle_step("working", ready), Settle::Ready);
    // An agent already working is past any first-run dialog, whatever
    // the screen has on it: what is there is its own output.
    assert_eq!(settle_step("working", CODEX_TRUST), Settle::Ready);
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
    assert_eq!(after_stall(pasted, prompt), AfterStall::Accept);
    // A harness herdr cannot narrate, sitting at a ready composer: the
    // stall says nothing, so the prompt is taken as delivered.
    let ready = "  Oh My Pi\n\n▌ Ask anything";
    assert_eq!(after_stall(ready, prompt), AfterStall::Accept);
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
