use super::*;

#[tokio::test(flavor = "current_thread")]
async fn parking_open_created_issue_preserves_wip_and_aliases_until_explicit_resume() {
    use crate::release::testkit::{scratch, sh};
    let _sandbox = crate::config::test_support::sandbox();
    let git = scratch("parking-wip").await;
    sh(&git.work, &["checkout", "-b", "bot/issue-5"]).await;
    std::fs::write(Path::new(&git.work).join("unpublished"), "local commit").unwrap();
    sh(&git.work, &["add", "unpublished"]).await;
    sh(&git.work, &["commit", "-m", "unpublished work"]).await;
    std::fs::write(Path::new(&git.work).join("stash-me"), "stash").unwrap();
    sh(
        &git.work,
        &["stash", "push", "-u", "-m", "parking evidence"],
    )
    .await;
    std::fs::write(Path::new(&git.work).join("dirty"), "still in progress").unwrap();
    let head = crate::release::git(&git.work, &["rev-parse", "HEAD"])
        .await
        .unwrap();
    let stash = crate::release::git(&git.work, &["rev-parse", "refs/stash"])
        .await
        .unwrap();
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
    let r = repo();
    let mut issue = assigned_item(5, "bot", "u1");
    issue["assignees"] = json!([]);
    stub.set_issue(5, issue.clone());
    *stub.created.lock().unwrap() = vec![issue.clone()];
    {
        let st = e.entry(&r, 5);
        st.worktree_path = Some(git.work.clone());
        st.repo_id = Some(git.work.clone());
        st.github_state = Some("open".into());
        st.triggers = vec!["created".into()];
    }
    seeded(&mut e, 6, Some("bot/issue-5"), false);
    seeded(&mut e, 7, Some("bot/issue-5"), true);
    for (n, state) in [(6, "merged"), (7, "open")] {
        let st = e.entry(&r, n);
        st.shares_workspace_of = Some(5);
        st.kind = Some("pull_request".into());
        st.github_state = Some(state.into());
    }
    stub.set_issue(7, assigned_item(7, "bot", "u1"));
    e.entry(&r, 7).triggers = vec!["created".into()];
    d.seed("unrelated", "t9", READY_SCREEN);
    d.with(|s| s.stop_removes_workspace = true);
    // An open bot-created item still cannot be released, before or after parking.
    assert!(e.release("o/r#5", false).await.is_err());
    let response = e
        .park(
            "o/r#7",
            "device acceptance pending after source slice #6 merged",
            "@alice",
        )
        .await
        .unwrap();
    assert_eq!(response["session"], "o/r#5");
    assert_eq!(e.owner_of(&r, 7), 5);
    assert_eq!(e.entry(&r, 6).github_state.as_deref(), Some("merged"));
    assert_eq!(e.entry(&r, 7).github_state.as_deref(), Some("open"));
    assert!(e.release("o/r#5", false).await.is_err());
    assert!(d.log().contains(&"stop:t5".into()));
    assert_eq!(
        d.with(|s| s.live.get("unrelated").cloned()),
        Some("t9".into())
    );
    assert!(
        e.deliver_to(&r, 7, "ordinary PR comment", None)
            .await
            .unwrap_err()
            .is::<SessionParked>()
    );
    // Save/load as a daemon restart, then stale creation listings and a comment.
    e.state.save().unwrap();
    e.state = State::load().unwrap();
    e.resume_interrupted(&[DriverKind::Herdr]).await;
    issue["updated_at"] = json!("u2");
    stub.set_issue(5, issue.clone());
    *stub.created.lock().unwrap() = vec![issue.clone()];
    stub.bump_created_etag();
    stub.set_timeline(5, vec![comment(51, "alice", "routine comment")]);
    for _ in 0..2 {
        e.tick_repo(&r).await.unwrap();
    }
    assert!(e.entry(&r, 5).parked.is_some());
    assert!(e.resume_candidates(&r).is_empty());
    assert!(d.launches().is_empty());
    let workspaces = e.driver(&r).ps().await.unwrap();
    let state = crate::status::sessions(&e.cfg, &e.state, Some(&workspaces));
    let dashboard = crate::status::dashboard_presentation(&json!({"sessions":state})).unwrap();
    assert!(dashboard["cards"].as_array().unwrap().is_empty());
    assert_eq!(
        dashboard["parked"].as_array().unwrap().len(),
        1,
        "{dashboard:#}"
    );
    assert_eq!(
        dashboard["parked"][0]["parked"]["next_action_owner"],
        "@alice"
    );
    assert_eq!(
        dashboard["parked"][0]["additional"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let resumed = e.resume_parked("o/r#7").await.unwrap();
    assert_eq!(resumed["worktree_path"], git.work);
    assert_eq!(resumed["agent_session_id"], "sess-5");
    assert_eq!(resumed["branch"], "refs/heads/bot/issue-5");
    assert!(e.entry(&r, 5).parked.is_none());
    let launches = d.launches();
    assert!(launches[0].contains("--resume") && launches[0].contains("sess-5"));
    assert!(d.log().iter().any(|entry| entry.starts_with("reopen:")));
    assert_eq!(
        crate::release::git(&git.work, &["rev-parse", "HEAD"])
            .await
            .unwrap(),
        head
    );
    assert_eq!(
        crate::release::git(&git.work, &["rev-parse", "refs/stash"])
            .await
            .unwrap(),
        stash
    );
    assert_eq!(
        std::fs::read_to_string(Path::new(&git.work).join("dirty")).unwrap(),
        "still in progress"
    );
    e.tick_repo(&r).await.unwrap();
    assert!(d.prompts().iter().any(|p| p.contains("routine comment")));
    std::fs::remove_dir_all(&git.dir).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn parking_refuses_work_dialogs_handover_missing_conversation_and_pending_delivery() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
    let r = repo();
    d.with(|s| {
        s.working.insert("w5".into());
    });
    assert!(e.park("o/r#5", "acceptance", "@alice").await.is_err());
    d.with(|s| {
        s.working.clear();
        s.questions.insert("t5".into());
    });
    assert!(e.park("o/r#5", "acceptance", "@alice").await.is_err());
    d.with(|s| {
        s.questions.clear();
        s.ps_error = Some("driver unavailable".into());
    });
    assert!(e.park("o/r#5", "acceptance", "@alice").await.is_err());
    e.entry(&r, 5).handover = Some(PendingHandover {
        harness: "codex".into(),
        ..Default::default()
    });
    assert!(e.park("o/r#5", "acceptance", "@alice").await.is_err());
    e.entry(&r, 5).handover = None;
    e.entry(&r, 5).agent_session_id = None;
    assert!(e.park("o/r#5", "acceptance", "@alice").await.is_err());
    e.entry(&r, 5).agent_session_id = Some("sess-5".into());
    let mailbox = crate::delivery_channel::mailbox("o/r", 5);
    std::fs::create_dir_all(&mailbox).unwrap();
    std::fs::write(mailbox.join("ready.json"), "{}").unwrap();
    std::fs::write(mailbox.join("bridge.json"), "{}").unwrap();
    std::fs::write(mailbox.join("codex-binding.json"), "{}").unwrap();
    std::fs::write(mailbox.join("codex-0001.json"), r#"{"confirmed":true}"#).unwrap();
    std::fs::write(mailbox.join("claude-0001.json"), r#"{"confirmed":false}"#).unwrap();
    assert!(e.park("o/r#5", "acceptance", "@alice").await.is_err());
    std::fs::write(mailbox.join("claude-0001.json"), r#"{"confirmed":true}"#).unwrap();
    std::fs::write(mailbox.join("0001.json"), "pending").unwrap();
    assert!(e.park("o/r#5", "acceptance", "@alice").await.is_err());
    assert!(d.log().iter().all(|entry| !entry.starts_with("stop:")));
    assert!(e.entry(&r, 5).parked.is_none());
    std::fs::remove_file(mailbox.join("0001.json")).unwrap();
    e.park("o/r#5", "acceptance", "@alice").await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn failed_resume_keeps_the_hold_and_never_recreates_a_missing_checkout() {
    use crate::release::testkit::scratch;
    let _sandbox = crate::config::test_support::sandbox();
    let git = scratch("parking-resume-failure").await;
    crate::release::testkit::sh(&git.work, &["checkout", "-b", "bot/issue-5"]).await;
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
    let r = repo();
    e.entry(&r, 5).worktree_path = Some(git.work.clone());
    e.entry(&r, 5).launched_stack = Some(Overrides {
        harness: "claude".into(),
        model: Some("original-model".into()),
        effort: Some("high".into()),
    });
    e.park("o/r#5", "acceptance", "@alice").await.unwrap();
    assert!(
        e.handover("o/r#5", "codex", None, None, None, None)
            .await
            .is_err()
    );
    d.with(|s| {
        s.start_error = Some("cannot launch".into());
        s.relaunch_screen = READY_SCREEN.iter().map(|line| line.to_string()).collect();
    });
    assert!(e.resume_parked("o/r#5").await.is_err());
    assert!(e.entry(&r, 5).parked.is_some());
    assert!(d.launches().is_empty());
    e.cfg.repos[0].harness = "codex".into();
    e.resume_parked("o/r#5").await.unwrap();
    let launches = d.launches();
    assert!(launches[0].starts_with("claude:"));
    assert!(launches[0].contains("sess-5"));
    assert_eq!(
        e.entry(&r, 5)
            .launched_stack
            .as_ref()
            .unwrap()
            .model
            .as_deref(),
        Some("original-model")
    );
    // A missing retained checkout must never cause a branch/base fallback.
    e.park("o/r#5", "another boundary", "@alice").await.unwrap();
    e.entry(&r, 5).worktree_path = Some(git.dir.join("missing").to_string_lossy().into());
    assert!(e.resume_parked("o/r#5").await.is_err());
    assert!(e.entry(&r, 5).parked.is_some());
    assert!(d.launches().is_empty());
    std::fs::remove_dir_all(&git.dir).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn parked_pr_completion_is_recorded_without_resuming_its_open_owner() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
    let r = repo();
    e.park("o/r#5", "device acceptance", "@alice")
        .await
        .unwrap();
    seeded(&mut e, 7, Some("bot/issue-5"), true);
    e.entry(&r, 7).shares_workspace_of = Some(5);
    e.entry(&r, 7).kind = Some("pull_request".into());
    let mut item = assigned_item(7, "bot", "u2");
    item["state"] = json!("closed");
    item["pull_request"] = json!({"url":"https://gh/7"});
    stub.set_issue(7, item);
    stub.set_pull(7, json!({"merged":true}));
    stub.set_timeline(7, vec![comment(70, "alice", "source slice accepted")]);
    e.retire_issue(&r, "o", "r", 7).await.unwrap();
    assert_eq!(e.entry(&r, 7).github_state.as_deref(), Some("merged"));
    assert!(!e.entry(&r, 7).active);
    assert!(e.entry(&r, 5).active && e.entry(&r, 5).parked.is_some());
    assert!(
        e.entry(&r, 7).seen.is_empty(),
        "pending completion evidence must not be marked delivered"
    );
    assert!(d.launches().is_empty());
}
