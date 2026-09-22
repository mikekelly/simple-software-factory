use super::*;

/// #355: a session started again on an item with history is shown the
/// item's whole story, its own earlier posts included, so it can read what
/// it already said and promised; the live follow-up that prompted the
/// restart still leaves them out.
#[tokio::test]
async fn a_restarted_session_is_shown_its_own_earlier_posts() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    let r = repo();
    stub.set_issue(5, assigned_item(5, "alice", "u2"));
    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    stub.set_timeline(
        5,
        vec![
            assigned_by(1, "alice"),
            comment(
                2,
                "bot",
                "🤖#5 says: <!-- ssf: origin=o/r#5 -->\n\nPushed the parser fix; the flag is untested.",
            ),
            comment(3, "alice", "one more thing: keep the flag"),
        ],
    );
    // The session had already read its own post and the assignment; only
    // the person's comment is new.
    {
        let st = e.entry(&r, 5);
        st.seen.insert("assigned:1".into(), String::new());
        st.seen.insert("commented:2".into(), "t".into());
    }
    // The pane is gone and there is no conversation to resume: the harness
    // starts from scratch, so it is given the story ahead of the new
    // activity.
    e.entry(&r, 5).agent_session_id = None;
    d.with(|s| {
        s.live.remove("w5");
    });
    e.tick_repo(&r).await.unwrap();
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    let story = &prompts[0];
    assert!(
        story.contains("Pushed the parser fix; the flag is untested."),
        "its own earlier post is replayed: {story}"
    );
    assert!(
        story.contains("one more thing: keep the flag"),
        "the new activity is there too: {story}"
    );
    // The same story, built on its own for a harness that has to be
    // started again outside a follow-up.
    let story = e.first_message(&r, 5, None).await.unwrap().text;
    assert!(
        story.contains("Pushed the parser fix; the flag is untested."),
        "its own post is in the story a restart is given: {story}"
    );
}

/// The post the session itself made in this very pass is the one the delta
/// never carries (its own posts are not echoed back), so the catch-up
/// story must keep it: a session whose pane died between its comment and
/// the next poll is exactly the one that needs to read what it promised.
#[tokio::test]
async fn a_restarted_session_is_shown_the_post_it_made_this_pass() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    let r = repo();
    stub.set_issue(5, assigned_item(5, "alice", "u2"));
    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    stub.set_timeline(
        5,
        vec![
            assigned_by(1, "alice"),
            comment(
                2,
                "bot",
                "🤖#5 says: <!-- ssf: origin=o/r#5 -->\n\nPushed the parser fix; the flag is untested.",
            ),
            comment(3, "alice", "one more thing: keep the flag"),
        ],
    );
    e.entry(&r, 5).agent_session_id = None;
    d.with(|s| {
        s.live.remove("w5");
    });
    e.tick_repo(&r).await.unwrap();
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("Pushed the parser fix; the flag is untested."),
        "an own post new in this pass is not dropped from the story: {}",
        prompts[0]
    );
    // And not twice: the story is where it is read, the delta below it
    // carries the person's comment alone.
    let story = &prompts[0];
    assert_eq!(
        story
            .matches("Pushed the parser fix; the flag is untested.")
            .count(),
        1,
        "{story}"
    );
}

/// An item assigned to the bot again after retiring: the session is started
/// on it afresh, so its relaunch story is the same catch-up, own posts
/// included.
#[tokio::test]
async fn an_item_assigned_again_is_told_its_own_posts() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    let r = repo();
    stub.set_timeline(
        5,
        vec![
            assigned_by(1, "alice"),
            comment(
                2,
                "bot",
                "🤖#5 says: <!-- ssf: origin=o/r#5 -->\n\nPushed the parser fix; the flag is untested.",
            ),
            comment(3, "alice", "assigned again, please carry on"),
        ],
    );
    let issue: Issue = serde_json::from_value(assigned_item(5, "alice", "u2")).unwrap();
    let mut st = e.entry(&r, 5).clone();
    st.triggers = vec!["assigned".into()];
    // No pane and no conversation to resume: the harness is started from
    // scratch, so it is given the story.
    e.entry(&r, 5).agent_session_id = None;
    d.with(|s| {
        s.live.remove("w5");
    });
    e.reactivate(&r, "o", "r", &issue, st).await.unwrap();
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("Pushed the parser fix; the flag is untested."),
        "the session reads what it said before: {}",
        prompts[0]
    );
    assert!(
        prompts[0].contains("## Activity so far"),
        "and the item's own story with it: {}",
        prompts[0]
    );
}

/// Onboarding's first prompt is the same catch-up: an item whose history
/// carries posts from this session id is shown them from its first message
/// on.
#[tokio::test]
async fn onboarding_a_session_shows_it_its_own_posts() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    stub.set_issue(7, assigned_item(7, "alice", "u2"));
    stub.set_assigned(vec![assigned_item(7, "alice", "u2")]);
    stub.set_timeline(
        7,
        vec![
            assigned_by(1, "alice"),
            comment(
                2,
                "bot",
                "🤖#7 says: <!-- ssf: origin=o/r#7 -->\n\nHalf done; the flag is untested.",
            ),
        ],
    );
    e.tick_repo(&r).await.unwrap();
    assert!(e.failures.is_empty(), "{:?}", e.failures);
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("Half done; the flag is untested."),
        "the item's history is shown with its own posts: {}",
        prompts[0]
    );
}

#[tokio::test]
async fn a_handover_without_a_summary_says_so() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    e.handover("o/r#5", "codex", None, None, None, None)
        .await
        .unwrap();
    e.run_handovers(&repo()).await;
    let log = d.log();
    assert!(
        log[1].starts_with("start:w5:You took over this issue from a session on Claude"),
        "{log:?}"
    );
    assert_eq!(
        e.entry(&repo(), 5).overrides,
        Some(Overrides {
            harness: "codex".into(),
            model: None,
            effort: None,
        })
    );
    let posts = stub.post_bodies();
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#5 event=handed-over -->\n\n\
             ```ssf\n\
             ssf handing over issue:\n\
             from: Claude Code\n\
             from model: the harness's default\n\
             from effort: the harness's default\n\
             to: Codex\n\
             to model: the harness's default\n\
             to effort: the harness's default\n\
             summary: no\n\
             by: a person at the terminal\n\
             ```"
    );
}
#[tokio::test]
async fn the_story_the_new_session_is_told_counts_as_delivered() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    e.handover("o/r#5", "pi", None, None, Some("half done"), None)
        .await
        .unwrap();
    // A comment lands after the handover was recorded: the pass runs
    // the handovers before it looks at the item.
    stub.set_issue(5, assigned_item(5, "alice", "u2"));
    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    stub.set_timeline(
        5,
        vec![
            assigned_by(1, "alice"),
            comment(2, "alice", "one more thing: keep the flag"),
        ],
    );
    e.run_handovers(&repo()).await;
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("one more thing: keep the flag"),
        "the story carries the new comment: {}",
        prompts[0]
    );
    let st = e.entry(&repo(), 5).clone();
    assert_eq!(st.updated_at.as_deref(), Some("u2"));
    assert!(st.seen.contains_key("commented:2"), "{:?}", st.seen);
    // The rest of the pass has nothing left to tell the new session.
    let _ = (d.log(), stub.post_bodies());
    e.tick_repo(&repo()).await.unwrap();
    let log = d.log();
    assert!(log.is_empty(), "delivered twice: {log:?}");
}
#[tokio::test]
async fn the_overrides_outlive_the_handover_pass() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    e.handover("o/r#5", "codex", None, None, None, None)
        .await
        .unwrap();
    e.run_handovers(&repo()).await;
    let _ = (d.log(), d.launches(), stub.post_bodies());
    // The terminal is gone: the startup pass brings the session back,
    // on the harness the handover put on the item.
    d.with(|s| {
        s.live.remove("w5");
    });
    e.entry(&repo(), 5).agent_session_id = Some("sess-5".into());
    e.resume_interrupted(&[DriverKind::Herdr]).await;
    let launched = d.launches();
    assert_eq!(launched.len(), 1, "{launched:?}");
    assert!(
        launched[0].starts_with("codex:") && launched[0].contains("resume sess-5"),
        "{launched:?}"
    );
    // And so does a workspace that has to be re-created.
    let _ = (d.log(), stub.post_bodies());
    d.with(|s| {
        s.worktrees.remove("w5");
        s.live.remove("w5");
    });
    e.entry(&repo(), 5).repo_id = Some("stub".into());
    e.deliver_to(&repo(), 5, "[ssf] hello", None).await.unwrap();
    let launched = d.launches();
    assert!(
        launched.iter().all(|l| l.starts_with("codex:")),
        "{launched:?}"
    );
}
#[tokio::test]
async fn a_model_only_handover_keeps_the_repository_command() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    let r = RepoConfig {
        command: Some("claude --dangerously-skip-permissions".into()),
        effort: Some("high".into()),
        ..repo()
    };
    e.cfg.repos = vec![r.clone()];
    let v = e
        .handover(
            "o/r#5",
            "claude",
            Some("opus"),
            None,
            Some("what is left"),
            None,
        )
        .await
        .unwrap();
    assert_eq!(v["from"]["harness"], "claude");
    assert_eq!(v["from"]["model"], Value::Null);
    assert_eq!(v["from"]["effort"], "high");
    assert_eq!(v["to"]["model"], "opus");
    assert_eq!(v["to"]["effort"], "high", "the repository's effort stays");
    e.run_handovers(&r).await;
    let launched = d.launches();
    assert_eq!(launched.len(), 1, "{launched:?}");
    assert!(
        launched[0].starts_with("claude:")
            && launched[0].contains("--dangerously-skip-permissions"),
        "{launched:?}"
    );
    assert!(launched[0].contains("opus"), "{launched:?}");
    // The context-compaction threshold travels with the launch: onto the
    // command line for the harness that takes it there, and to the wrapper,
    // which cannot resolve it for an item whose overrides the config it reads
    // does not describe.
    assert!(launched[0].contains("--autocompact 300000"), "{launched:?}");
    assert!(
        launched[0].contains("--auto-compaction-tokens 300000"),
        "{launched:?}"
    );
    // A handover on the same harness is where a transcript is most
    // easily mixed up, so the item says one happened whether or not
    // an id was ever captured for the session that left.
    assert!(e.entry(&r, 5).handed_over_at.is_some());
    assert_eq!(
        e.entry(&r, 5).overrides,
        Some(Overrides {
            harness: "claude".into(),
            model: Some("opus".into()),
            effort: None,
        })
    );
    let posts = stub.post_bodies();
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#5 event=handed-over -->\n\n\
             ```ssf\n\
             ssf handing over issue:\n\
             from: Claude Code\n\
             from model: the command's\n\
             from effort: high\n\
             from command: claude --dangerously-skip-permissions\n\
             to: Claude Code\n\
             to model: opus\n\
             to effort: high\n\
             to command: claude --dangerously-skip-permissions\n\
             summary: yes\n\
             by: a person at the terminal\n\
             ```"
    );
}
#[tokio::test]
async fn a_handover_on_a_bound_item_is_the_owning_session_s() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    seeded(&mut e, 6, Some("bot/issue-5"), true);
    {
        let st = e.entry(&repo(), 6);
        st.shares_workspace_of = Some(5);
        st.worktree_id = Some("w5".into());
        st.worktree_path = Some("/w/5".into());
        st.title = "Follow-up".into();
        st.html_url = "https://gh/6".into();
        // The bound item mirrors the owner's conversation.
        st.agent_session_id = Some("sess-5".into());
    }
    let v = e
        .handover(
            "o/r#6",
            "pi",
            None,
            None,
            Some("what is left"),
            Some("o/r#6"),
        )
        .await
        .unwrap();
    assert_eq!(v["session"], "o/r#5", "the owning session's");
    assert_eq!(v["title"], "Fix the widget");
    assert!(e.entry(&repo(), 5).handover.is_some());
    assert!(e.entry(&repo(), 6).handover.is_none());
    e.run_handovers(&repo()).await;
    let log = d.log();
    assert_eq!(log[0], "stop:t5", "{log:?}");
    assert!(log[1].starts_with("start:w5:"), "{log:?}");
    assert!(e.entry(&repo(), 5).overrides.is_some());
    assert!(
        e.entry(&repo(), 6).overrides.is_none(),
        "the override lives on the owner"
    );
    // The retired conversation is gone from the bound item too, and
    // is not offered back to the new session through the mirror.
    let bound_state = e.entry(&repo(), 6).clone();
    assert!(bound_state.agent_session_id.is_none());
    assert_eq!(bound_state.retired_session_ids, vec!["sess-5".to_string()]);
    // Both items run the new harness, and say so.
    assert_eq!(e.effective(&repo(), 6).harness, "pi");
    let sessions = crate::status::sessions(&e.cfg, &e.state, Some(&[]));
    let bound = sessions.iter().find(|s| s.number == 6).unwrap();
    assert_eq!(bound.harness, "pi");
    assert_eq!(
        sessions.iter().find(|s| s.number == 5).unwrap().harness,
        "pi"
    );
}
#[tokio::test]
async fn handovers_are_refused_with_the_reason() {
    let stub = GitHubStub::start().await;
    let (mut e, _d) = handover_setup(&stub);
    let msg = |r: Result<Value>| r.unwrap_err().to_string();
    // Not a session ssf knows.
    assert!(
        msg(e.handover("o/r#9", "pi", None, None, None, None).await)
            .contains("is not an agent session ssf knows")
    );
    // A harness nothing knows, and a model or effort the harness
    // cannot take.
    assert!(
        msg(e.handover("o/r#5", "zzz", None, None, None, None).await)
            .contains("zzz is not a harness ssf knows")
    );
    assert!(
        msg(e
            .handover("o/r#5", "pi", None, Some("turbo"), None, None)
            .await)
        .contains("is not a level pi accepts")
    );
    // Not installed here, then not signed in here.
    e.installed = std::sync::Arc::new(|_| false);
    assert!(
        msg(e.handover("o/r#5", "pi", None, None, None, None).await)
            .contains("Pi is not installed where the daemon runs")
    );
    e.installed = std::sync::Arc::new(|_| true);
    probe_returning(&mut e, LoginState::SignedOut, None);
    let err = msg(e.handover("o/r#5", "pi", None, None, None, None).await);
    assert!(err.contains("Pi is not signed in here"), "{err}");
    assert!(err.contains(&login::how_to_sign_in("pi")), "{err}");
    // The signed-out answer is not remembered: an operator who signs
    // the harness in and runs the command again is not told the same
    // thing until the next pass, because the command drops the memo
    // for that harness before it asks.
    e.probe = std::sync::Arc::new(|_| Probe {
        state: LoginState::SignedIn,
        detail: "test".into(),
        fingerprint: None,
    });
    e.handover("o/r#5", "pi", None, None, None, None)
        .await
        .expect("the fresh login is seen straight away");
    e.entry(&repo(), 5).handover = None;
    probe_returning(&mut e, LoginState::Unknown, None);
    // The target is what the item already runs.
    assert!(
        msg(e.handover("o/r#5", "claude", None, None, None, None).await)
            .contains("already on claude with that model and effort")
    );
    // An empty summary is not a summary: the CLI refuses it, and so
    // does the daemon, for a request that did not come through it.
    assert!(
        msg(e
            .handover("o/r#5", "pi", None, None, Some("  \n"), None)
            .await)
        .contains("the summary is empty")
    );
    // A summary that would read as the new harness's sign-in screen.
    let err = msg(e
        .handover(
            "o/r#5",
            "pi",
            None,
            None,
            Some("I got stuck: the pane kept saying Please run /login"),
            None,
        )
        .await);
    assert!(
        err.contains("would read as a harness's own sign-in screen"),
        "{err}"
    );
    assert!(!crate::driver::quotes_login_prompt(&err), "{err}");
    // A summary longer than the cap.
    let long = "x".repeat(crate::ipc::MAX_SUMMARY_CHARS + 1);
    assert!(
        msg(e
            .handover("o/r#5", "pi", None, None, Some(&long), None)
            .await)
        .contains("the most a handover carries is 8000")
    );
    // A release is pending on it.
    e.entry(&repo(), 5).release_pending = true;
    assert!(
        msg(e.handover("o/r#5", "pi", None, None, None, None).await)
            .contains("a release is pending on this item")
    );
    e.entry(&repo(), 5).release_pending = false;
    // And a release is refused while a handover is pending.
    e.handover("o/r#5", "pi", None, None, None, None)
        .await
        .unwrap();
    e.entry(&repo(), 5).active = false;
    assert!(
        msg(e.release("o/r#5", false).await).contains("a handover to pi is pending"),
        "a release must not race the handover"
    );
    // The item has no running session at all.
    e.entry(&repo(), 5).handover = None;
    assert!(
        msg(e.handover("o/r#5", "pi", None, None, None, None).await)
            .contains("the item has no running session")
    );
}
#[tokio::test]
async fn a_handover_the_pass_cannot_carry_out_is_refused_on_the_item() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    e.handover(
        "o/r#5",
        "pi",
        None,
        None,
        Some("what is left"),
        Some("o/r#5"),
    )
    .await
    .unwrap();
    // The item is dropped between the request and the pass.
    e.entry(&repo(), 5).active = false;
    e.run_handovers(&repo()).await;
    let st = e.entry(&repo(), 5).clone();
    assert!(st.handover.is_none(), "the pending handover is off");
    assert!(st.overrides.is_none(), "nothing was changed");
    assert_eq!(st.agent_session_id.as_deref(), Some("sess-5"));
    let log = d.log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(
        log[0],
        "deliver:w5:[ssf] Handover to Pi refused: the item is no longer active. "
    );
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#5 event=handed-over -->\n\n\
             ```ssf\n\
             ssf not handing over issue:\n\
             to: Pi\n\
             to model: the harness's default\n\
             to effort: the harness's default\n\
             by: o/r#5\n\
             refused: the item is no longer active\n\
             ```"
    );
}
#[tokio::test]
async fn a_pending_handover_survives_a_restart() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    e.handover("o/r#5", "pi", None, None, Some("half done"), None)
        .await
        .unwrap();
    // As a restart leaves it: the state as written, read back.
    let written = serde_json::to_string(&e.state).unwrap();
    let mut e = engine_at(&stub.base);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.repos = vec![repo()];
    e.state = serde_json::from_str(&written).unwrap();
    let h = e.entry(&repo(), 5).handover.clone().unwrap();
    assert_eq!(h.harness, "pi");
    assert_eq!(h.summary.as_deref(), Some("half done"));
    e.run_handovers(&repo()).await;
    assert!(e.entry(&repo(), 5).handover.is_none());
    assert_eq!(
        e.entry(&repo(), 5)
            .overrides
            .as_ref()
            .map(|o| o.harness.clone()),
        Some("pi".into())
    );
    let log = d.log();
    assert_eq!(log[0], "stop:t5", "{log:?}");
    assert!(log[1].starts_with("start:w5:You took over"), "{log:?}");
}
