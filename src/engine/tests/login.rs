use super::*;

#[tokio::test]
async fn a_session_at_a_login_prompt_is_blocked_told_and_held() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, LOGIN_SCREEN);
    probe_returning(&mut e, LoginState::SignedOut, Some("cred-old"));
    // New activity on the item this pass.
    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    stub.set_timeline(
        5,
        vec![assigned_by(1, "alice"), comment(2, "alice", "please hurry")],
    );
    e.tick_repo(&repo()).await.unwrap();
    let st = e.entry(&repo(), 5).clone();
    let b = st.blocked.clone().expect("blocked");
    assert_eq!(b.reason, "login");
    assert_eq!(b.harness, "claude");
    assert_eq!(b.detail, "Login expired · Please run /login");
    assert!(b.reported);
    assert_eq!(b.credential.as_deref(), Some("cred-old"));
    // The item was told once, by the daemon (not as the session).
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].0, "/repos/o/r/issues/5/comments");
    assert_eq!(
        posts[0].1,
        format!(
            "🤖 ssf <!-- ssf: origin=o/r#5 event=blocked -->\n\n\
                 ```ssf\n\
                 ssf holding deliveries to agent on issue:\n\
                 harness: Claude Code\n\
                 reason: not signed in\n\
                 fix: {}\n\
                 ```",
            login::how_to_sign_in("claude").replace('`', "")
        )
    );
    assert!(posts[0].1.contains("fix: claude auth login on the host"));
    // Nothing was pasted, and the activity is still owed: `updated_at`
    // did not move, the comment is not marked seen, no failure counted.
    assert!(d.log().is_empty(), "no delivery into a blocked session");
    assert_eq!(st.updated_at.as_deref(), Some("u1"));
    assert!(!st.seen.contains_key("comment:2"), "{:?}", st.seen.keys());
    assert!(e.failures.is_empty());
    // A second pass: still blocked, still one comment, still nothing pasted.
    e.tick_repo(&repo()).await.unwrap();
    assert!(stub.posts().is_empty());
    assert!(d.log().is_empty());
    assert!(e.entry(&repo(), 5).blocked.is_some());
    // Direct deliveries (a tell, a subscriber's FYI) are refused with
    // the reason, not silently lost.
    let err = e.deliver_to(&repo(), 5, "hello", None).await.unwrap_err();
    assert!(is_blocked(&err), "{err:#}");
    assert!(
        err.to_string()
            .contains("Claude Code has been at its sign-in prompt"),
        "{err:#}"
    );
    assert!(err.to_string().contains("claude auth login"), "{err:#}");
    let err = e.tell(None, "o/r#5", "hello").await.unwrap_err();
    assert!(is_blocked(&err), "{err:#}");
    assert!(d.log().is_empty());
}
#[tokio::test]
async fn a_blocked_session_is_started_again_once_the_login_is_back() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, LOGIN_SCREEN);
    let since = (chrono::Utc::now() - chrono::Duration::minutes(12))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    e.entry(&repo(), 5).blocked = Some(Blocked {
        reason: "login".into(),
        harness: "claude".into(),
        detail: "Login expired · Please run /login".into(),
        since: since.clone(),
        reported: true,
        credential: Some("cred-old".into()),
        retried_at: None,
        retries: 0,
        told_at: None,
        tell_failures: 0,
    });
    e.state.repo_mut("o/r").issues_etag = Some("etag".into());
    stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
    stub.set_timeline(5, vec![assigned_by(1, "alice")]);
    // Signed out: nothing happens.
    probe_returning(&mut e, LoginState::SignedOut, Some("cred-old"));
    e.tick_repo(&repo()).await.unwrap();
    assert!(e.entry(&repo(), 5).blocked.is_some());
    assert!(d.log().is_empty());
    assert!(stub.posts().is_empty());
    // Signed in, same credential, block younger than the retry
    // interval as far as the record says? It is 12 minutes old, so
    // the retry is due; make it fresh first to see it held back.
    e.entry(&repo(), 5).blocked.as_mut().unwrap().retried_at = Some(now_iso());
    probe_returning(&mut e, LoginState::SignedIn, Some("cred-old"));
    e.tick_repo(&repo()).await.unwrap();
    assert!(e.entry(&repo(), 5).blocked.is_some());
    assert!(d.log().is_empty(), "not due yet");
    // A new credential file: the harness is quit and started again
    // with its conversation resumed, given the login-back message,
    // the listings are fetched afresh and the item is told.
    probe_returning(&mut e, LoginState::SignedIn, Some("cred-new"));
    d.with(|s| s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect());
    let fulls_before = stub.created_fulls();
    e.tick_repo(&repo()).await.unwrap();
    let st = e.entry(&repo(), 5).clone();
    assert!(st.blocked.is_none(), "{:?}", st.blocked);
    let log = d.log();
    assert_eq!(log[0], "stop:t5");
    assert_eq!(log[1], "relaunch:w5:true");
    assert!(
        log[2].starts_with("deliver:w5:[ssf] Your Claude Code sign-in lapsed at"),
        "{log:?}"
    );
    assert_eq!(st.terminal_handle.as_deref(), Some("t1"));
    assert_eq!(st.prompts_sent, 1);
    // One `unblocked` post carries the conversation line; the restart
    // that lifted the block is not a `resumed` event of its own.
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#5 event=unblocked -->\n\n\
             ```ssf\n\
             ssf resuming deliveries to agent on issue:\n\
             harness: Claude Code\n\
             held for: 12 min\n\
             conversation: resumed\n\
             ```"
    );
    // ETags were dropped by the recovery, then set again by the pass.
    assert!(e.state.repos["o/r"].issues_etag.is_some());
    // The recovery ran before the pass read its listings, so the full
    // fetch it is owed is this pass, and the `refetch` flag it armed is
    // spent unused at the start of it: the passes after this one are
    // conditional again (issue #141).
    assert_eq!(stub.created_fulls(), fulls_before + 1, "one full fetch");
    assert!(e.refetch.is_empty());
    for _ in 0..2 {
        e.tick_repo(&repo()).await.unwrap();
    }
    assert_eq!(
        stub.created_fulls(),
        fulls_before + 1,
        "and no more after it"
    );
    // A failing item would clear all four ETags for its own reasons and
    // make the count above misleading; a quiet pass also posts nothing.
    assert!(e.failures.is_empty(), "{:?}", e.failures);
    assert!(stub.post_bodies().is_empty());
}
#[tokio::test]
async fn a_harness_started_again_onto_the_login_prompt_stays_blocked_quietly() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, LOGIN_SCREEN);
    let since = (chrono::Utc::now() - chrono::Duration::minutes(30))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    e.entry(&repo(), 5).blocked = Some(Blocked {
        reason: "login".into(),
        harness: "claude".into(),
        detail: "Login expired · Please run /login".into(),
        since: since.clone(),
        reported: true,
        credential: Some("cred-old".into()),
        retried_at: None,
        retries: 0,
        told_at: None,
        tell_failures: 0,
    });
    stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
    stub.set_timeline(5, vec![assigned_by(1, "alice")]);
    // The check claims signed in but the harness comes back to the
    // same prompt: the record keeps its start, no second comment, and
    // the next attempt waits for the retry interval.
    probe_returning(&mut e, LoginState::Unknown, None);
    d.with(|s| s.relaunch_screen = LOGIN_SCREEN.iter().map(|l| l.to_string()).collect());
    e.tick_repo(&repo()).await.unwrap();
    let st = e.entry(&repo(), 5).clone();
    let b = st.blocked.expect("still blocked");
    assert_eq!(b.since, since);
    assert!(b.reported);
    assert!(b.retried_at.is_some());
    assert_eq!(b.retries, 1, "the next attempt waits twice as long");
    assert_eq!(retry_wait(0).as_secs(), 600);
    assert_eq!(retry_wait(1).as_secs(), 1200);
    assert_eq!(retry_wait(3).as_secs(), 3600);
    assert_eq!(retry_wait(30).as_secs(), 3600);
    let log = d.log();
    assert_eq!(log[0], "stop:t5");
    assert_eq!(log[1], "relaunch:w5:true");
    assert!(stub.posts().is_empty(), "the item was told already");
    // And not again on the very next pass.
    e.tick_repo(&repo()).await.unwrap();
    assert!(d.log().is_empty());
}
#[tokio::test]
async fn a_person_signing_in_at_the_terminal_lifts_the_block_without_a_restart() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
    e.entry(&repo(), 5).blocked = Some(Blocked {
        reason: "login".into(),
        harness: "claude".into(),
        detail: "Login expired · Please run /login".into(),
        since: now_iso(),
        reported: true,
        credential: None,
        retried_at: None,
        retries: 0,
        told_at: None,
        tell_failures: 0,
    });
    e.state.repo_mut("o/r").issues_etag = Some("etag".into());
    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    stub.set_timeline(
        5,
        vec![assigned_by(1, "alice"), comment(2, "alice", "go on")],
    );
    probe_returning(&mut e, LoginState::Unknown, None);
    e.tick_repo(&repo()).await.unwrap();
    let st = e.entry(&repo(), 5).clone();
    assert!(st.blocked.is_none());
    // No restart, and the held activity went in on the same pass.
    let log = d.log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(
        log[0].starts_with("deliver:w5:[ssf] New activity"),
        "{log:?}"
    );
    assert_eq!(st.updated_at.as_deref(), Some("u2"));
    // The item had been told of the block, so it hears the hold is
    // over, however quick, and that nothing was started again.
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#5 event=unblocked -->\n\n\
             ```ssf\n\
             ssf resuming deliveries to agent on issue:\n\
             harness: Claude Code\n\
             held for: less than a minute\n\
             conversation: kept\n\
             ```"
    );
}
#[tokio::test]
async fn a_relaunch_that_lands_on_a_login_prompt_blocks_the_session() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
    // The agent is gone (a reboot); the machine is not signed in.
    d.with(|s| {
        s.live.clear();
        s.relaunch_screen = LOGIN_SCREEN.iter().map(|l| l.to_string()).collect();
    });
    probe_returning(&mut e, LoginState::SignedOut, None);
    let err = e
        .deliver_to(&repo(), 5, "[ssf] hello", None)
        .await
        .unwrap_err();
    assert!(is_blocked(&err), "{err:#}");
    let st = e.entry(&repo(), 5).clone();
    let b = st.blocked.clone().expect("blocked");
    assert!(!b.reported, "the comment is left to the pass");
    assert_eq!(st.terminal_handle.as_deref(), Some("t1"));
    let log = d.log();
    assert_eq!(log[0], "relaunch:w5:true");
    // The next pass reports it, once.
    stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
    stub.set_timeline(5, vec![assigned_by(1, "alice")]);
    e.tick_repo(&repo()).await.unwrap();
    assert!(e.entry(&repo(), 5).blocked.as_ref().unwrap().reported);
    assert_eq!(stub.posts(), vec!["/repos/o/r/issues/5/comments"]);
    e.tick_repo(&repo()).await.unwrap();
    assert!(stub.posts().is_empty());
    // A working agent is never read for a login prompt.
    let (mut e, d) = blocked_setup(&stub, LOGIN_SCREEN);
    d.with(|s| {
        s.working.insert("w5".into());
    });
    stub.set_timeline(5, vec![assigned_by(1, "alice")]);
    e.tick_repo(&repo()).await.unwrap();
    assert!(e.entry(&repo(), 5).blocked.is_none());
}
#[tokio::test]
async fn a_blocked_harness_that_is_gone_is_judged_by_its_restart() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, LOGIN_SCREEN);
    let since = (chrono::Utc::now() - chrono::Duration::minutes(30))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let record = Blocked {
        reason: "login".into(),
        harness: "claude".into(),
        detail: "Login expired · Please run /login".into(),
        since: since.clone(),
        reported: true,
        credential: Some("cred-old".into()),
        retried_at: None,
        retries: 0,
        told_at: None,
        tell_failures: 0,
    };
    e.entry(&repo(), 5).blocked = Some(record.clone());
    // The terminal vanished (a reboot, a closed terminal).
    d.with(|s| {
        s.live.clear();
        s.relaunch_screen = LOGIN_SCREEN.iter().map(|l| l.to_string()).collect();
    });
    stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
    stub.set_timeline(5, vec![assigned_by(1, "alice")]);
    // Still signed out: nothing is started, nothing is said.
    probe_returning(&mut e, LoginState::SignedOut, Some("cred-old"));
    e.tick_repo(&repo()).await.unwrap();
    assert_eq!(e.entry(&repo(), 5).blocked, Some(record.clone()));
    assert!(d.log().is_empty());
    assert!(stub.posts().is_empty());
    // The check says signed in but the restart lands on the prompt:
    // the record stays (no comment either way), the attempt is noted.
    probe_returning(&mut e, LoginState::SignedIn, Some("cred-new"));
    e.tick_repo(&repo()).await.unwrap();
    let b = e.entry(&repo(), 5).blocked.clone().expect("still blocked");
    assert_eq!(b.since, since);
    assert!(b.reported);
    assert_eq!(b.retries, 1);
    assert_eq!(b.credential.as_deref(), Some("cred-new"));
    let log = d.log();
    assert_eq!(log[0], "relaunch:w5:true", "{log:?}");
    assert!(stub.posts().is_empty());
    // A restart that comes up working lifts the block, with the one
    // "signed in again" comment, and the held activity follows.
    e.entry(&repo(), 5).blocked.as_mut().unwrap().retried_at = Some(since.clone());
    d.with(|s| {
        s.live.clear();
        s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect();
    });
    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    stub.set_timeline(
        5,
        vec![assigned_by(1, "alice"), comment(2, "alice", "go on")],
    );
    e.tick_repo(&repo()).await.unwrap();
    let st = e.entry(&repo(), 5).clone();
    assert!(st.blocked.is_none(), "{:?}", st.blocked);
    let log = d.log();
    assert_eq!(log[0], "relaunch:w5:true", "{log:?}");
    assert!(
        log[1].starts_with("deliver:w5:[ssf] Your Claude Code sign-in lapsed"),
        "{log:?}"
    );
    assert!(
        log.iter()
            .any(|l| l.starts_with("deliver:w5:[ssf] New activity")),
        "{log:?}"
    );
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert!(
        posts[0].1.contains("event=unblocked -->")
            && posts[0]
                .1
                .contains("held for: 30 min\nconversation: resumed\n"),
        "{}",
        posts[0].1
    );
    assert_eq!(st.updated_at.as_deref(), Some("u2"));
    // Gone again while still blocked, activity arrives through the
    // normal path and the restart is fine: lifted the same way, once.
    e.entry(&repo(), 5).blocked = Some(record.clone());
    d.with(|s| s.live.clear());
    stub.set_assigned(vec![assigned_item(5, "alice", "u3")]);
    stub.set_timeline(
        5,
        vec![assigned_by(1, "alice"), comment(3, "alice", "and again")],
    );
    probe_returning(&mut e, LoginState::SignedOut, Some("cred-new"));
    e.tick_repo(&repo()).await.unwrap();
    assert!(e.entry(&repo(), 5).blocked.is_none());
    let log = d.log();
    assert_eq!(log[0], "relaunch:w5:true", "{log:?}");
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert!(posts[0].1.contains("event=unblocked -->"), "{}", posts[0].1);
}
