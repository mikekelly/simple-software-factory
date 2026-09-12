use super::*;

#[tokio::test]
async fn a_new_harness_at_its_sign_in_prompt_blocks_the_new_session() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    d.with(|s| {
        s.relaunch_screen = vec!["  Use /login to log into a provider".into(), "❯ ".into()];
    });
    e.handover("o/r#5", "pi", None, None, None, Some("o/r#5"))
        .await
        .unwrap();
    e.run_handovers(&repo()).await;
    let st = e.entry(&repo(), 5).clone();
    assert!(st.overrides.is_some(), "the handover stands");
    let b = st.blocked.clone().expect("blocked");
    assert_eq!(b.harness, "pi");
    assert!(b.reported);
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 2, "{posts:?}");
    assert!(posts[0].1.contains("ssf handing over issue:"), "{posts:?}");
    assert_eq!(
        posts[1].1,
        format!(
            "🤖 ssf <!-- ssf: origin=o/r#5 event=blocked -->\n\n\
                 ```ssf\n\
                 ssf holding deliveries to agent on issue:\n\
                 harness: Pi\n\
                 reason: not signed in\n\
                 fix: {}\n\
                 ```",
            login::how_to_sign_in("pi").replace('`', "")
        )
    );
}
#[tokio::test]
async fn a_new_harness_that_will_not_start_blocks_the_item() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    d.with(|s| {
        s.start_error = Some("pi exited at once: ambiguous model gpt-5.5".into());
    });
    e.handover(
        "o/r#5",
        "pi",
        Some("openai/gpt-6"),
        None,
        None,
        Some("o/r#5"),
    )
    .await
    .unwrap();
    e.run_handovers(&repo()).await;
    let st = e.entry(&repo(), 5).clone();
    assert!(st.handover.is_none(), "carried out, not left pending");
    assert!(st.overrides.is_some(), "the handover stands");
    assert!(st.terminal_handle.is_none(), "nothing is running");
    let b = st.blocked.clone().expect("blocked");
    // The login check cannot tell (the default in these tests), so
    // the block stands as what was seen: the harness would not start.
    assert_eq!(b.reason, Blocked::START);
    assert_eq!(b.harness, "pi");
    assert!(b.reported);
    assert!(b.detail.contains("ambiguous model"), "{}", b.detail);
    // The item says both, in order, and nothing says a session attached.
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 2, "{posts:?}");
    assert!(posts[0].1.contains("ssf handing over issue:"), "{posts:?}");
    assert_eq!(
        posts[1].1,
        "🤖 ssf <!-- ssf: origin=o/r#5 event=blocked -->\n\n\
             ```ssf\n\
             ssf holding deliveries to agent on issue:\n\
             harness: Pi\n\
             reason: could not be started: pi exited at once: ambiguous model gpt-5.5\n\
             fix: start Pi by hand in the workspace, or fix the model or effort and hand over again\n\
             ```"
    );
    // A person sees it in the status commands.
    let view = crate::status::BlockedView::from_blocked(&b);
    assert!(
        view.describe().starts_with("Pi could not be started since"),
        "{}",
        view.describe()
    );
    // And the recovery is the usual one: after the wait the harness is
    // started again in the same workspace, on the item's overrides.
    let _ = (d.log(), d.launches());
    if let Some(cur) = e.entry(&repo(), 5).blocked.as_mut() {
        cur.since = "2020-01-01T00:00:00Z".into();
    }
    let st = e.entry(&repo(), 5).clone();
    let b = st.blocked.clone().unwrap();
    e.recover(&repo(), 5, &st, b).await;
    assert!(e.entry(&repo(), 5).blocked.is_none(), "the block is lifted");
    let launched = d.launches();
    assert_eq!(launched.len(), 1, "{launched:?}");
    assert!(launched[0].starts_with("pi:"), "{launched:?}");
    let log = d.log();
    assert!(log.iter().any(|l| l.starts_with("relaunch:w5:")), "{log:?}");
}
#[tokio::test]

async fn a_pending_handover_can_be_cancelled() {
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    let nothing = e
        .handle_request(crate::ipc::Request::CancelHandover {
            session: "o/r#5".into(),
        })
        .await;
    assert!(!nothing.ok);
    assert!(
        nothing.error.unwrap().contains("no handover is pending"),
        "refused with the reason"
    );
    e.handover("o/r#5", "pi", None, None, Some("half done"), Some("o/r#5"))
        .await
        .unwrap();
    let r = e
        .handle_request(crate::ipc::Request::CancelHandover {
            session: "o/r#5".into(),
        })
        .await;
    assert!(r.ok, "{:?}", r.error);
    assert_eq!(r.data["session"], "o/r#5");
    assert_eq!(r.data["harness_name"], "Pi");
    assert_eq!(r.data["told"], true);
    // The agent that was told to stop hears that it carries on, and
    // nothing is posted on the item: the handover was never announced.
    let log = d.log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(
        log[0],
        "deliver:w5:[ssf] The handover to Pi was cancelled: this session keeps t"
    );
    assert!(stub.posts().is_empty());
    // Nothing pending: the pass leaves the session alone and the
    // ordinary commands work again.
    assert!(e.entry(&repo(), 5).handover.is_none());
    e.run_handovers(&repo()).await;
    assert!(d.log().is_empty(), "the session stays");
    assert!(e.entry(&repo(), 5).overrides.is_none());
    assert!(e.resume_candidates(&repo()).contains(&5));
    e.tell(None, "o/r#5", "hello").await.unwrap();
}
#[tokio::test]
async fn a_handover_closes_an_outstanding_hold_on_the_item() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    e.entry(&repo(), 5).blocked = Some(Blocked {
        reason: Blocked::LOGIN.into(),
        harness: "claude".into(),
        detail: "Login expired · Please run /login".into(),
        since: (chrono::Utc::now() - chrono::Duration::minutes(20))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        reported: true,
        credential: None,
        retried_at: None,
        retries: 0,
        told_at: None,
        tell_failures: 0,
    });
    e.handover("o/r#5", "pi", None, None, Some("half done"), None)
        .await
        .unwrap();
    e.run_handovers(&repo()).await;
    let st = e.entry(&repo(), 5).clone();
    assert!(st.blocked.is_none(), "{:?}", st.blocked);
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 3, "{posts:?}");
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#5 event=unblocked -->\n\n\
             ```ssf\n\
             ssf resuming deliveries to agent on issue:\n\
             harness: Claude Code\n\
             held for: 20 min\n\
             conversation: handed over\n\
             ```"
    );
    assert!(posts[1].1.contains("ssf handing over issue:"), "{posts:?}");
    assert!(
        posts[2].1.contains("ssf attaching agent to issue:"),
        "{posts:?}"
    );
    let log = d.log();
    assert_eq!(log[0], "stop:t5", "{log:?}");
}
#[tokio::test]
async fn a_harness_that_will_not_start_and_is_signed_out_is_a_login_block() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    d.with(|s| s.start_error = Some("pi exited at once".into()));
    // Accepted while the check cannot tell; signed out by the time
    // the pass runs (a login that lapsed in between).
    e.handover("o/r#5", "pi", None, None, None, None)
        .await
        .unwrap();
    probe_returning(&mut e, LoginState::SignedOut, Some("cred-old"));
    e.run_handovers(&repo()).await;
    let b = e.entry(&repo(), 5).blocked.clone().expect("blocked");
    assert_eq!(b.reason, Blocked::LOGIN);
    assert_eq!(b.harness, "pi");
    assert_eq!(b.credential.as_deref(), Some("cred-old"));
    assert!(
        b.detail.contains("exited at once"),
        "what was seen is kept: {}",
        b.detail
    );
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 2, "{posts:?}");
    assert!(posts[1].1.contains("reason: not signed in"), "{posts:?}");
    assert!(
        posts[1].1.contains(&format!(
            "fix: {}",
            login::how_to_sign_in("pi").replace('`', "")
        )),
        "{posts:?}"
    );
    // And it recovers as a sign-in block does: nothing while the
    // check still says signed out, whatever the backoff says.
    let _ = d.log();
    e.entry(&repo(), 5).blocked.as_mut().unwrap().since = "2020-01-01T00:00:00Z".into();
    let st = e.entry(&repo(), 5).clone();
    e.recover(&repo(), 5, &st, b).await;
    assert!(d.log().is_empty(), "still signed out");
    assert!(e.entry(&repo(), 5).blocked.is_some());
}
#[tokio::test]
async fn the_summary_outlives_a_harness_that_would_not_start() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    d.with(|s| s.start_error = Some("pi exited at once".into()));
    e.handover(
        "o/r#5",
        "pi",
        None,
        None,
        Some("The parser is half migrated; the flag is unverified."),
        Some("o/r#5"),
    )
    .await
    .unwrap();
    e.run_handovers(&repo()).await;
    let st = e.entry(&repo(), 5).clone();
    assert_eq!(
        st.blocked.as_ref().map(|b| b.reason.as_str()),
        Some("start")
    );
    let note = st.handover_note.clone().expect("the summary is kept");
    assert_eq!(note.from, "Claude Code");
    assert!(note.summary.unwrap().contains("half migrated"));
    // A restart that comes up at a sign-in prompt read the message as
    // a screen, not as a session: the block becomes the sign-in one
    // and the summary is still owed.
    let _ = (d.log(), d.prompts(), stub.post_bodies());
    d.with(|s| s.relaunch_screen = PI_LOGIN_SCREEN.iter().map(|l| l.to_string()).collect());
    e.entry(&repo(), 5).blocked.as_mut().unwrap().since = "2020-01-01T00:00:00Z".into();
    let st = e.entry(&repo(), 5).clone();
    let b = st.blocked.clone().unwrap();
    e.recover(&repo(), 5, &st, b).await;
    let st = e.entry(&repo(), 5).clone();
    assert_eq!(
        st.blocked.as_ref().map(|b| b.reason.as_str()),
        Some("login"),
        "{:?}",
        st.blocked
    );
    assert!(
        st.handover_note.is_some(),
        "the message went into the sign-in screen, so the summary waits"
    );
    // The restart after the backoff tells the new harness what the
    // outgoing agent left, then the item's story.
    let _ = (d.log(), d.prompts(), stub.post_bodies());
    d.with(|s| s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect());
    {
        let b = e.entry(&repo(), 5).blocked.as_mut().unwrap();
        b.since = "2020-01-01T00:00:00Z".into();
        b.retried_at = None;
    }
    let st = e.entry(&repo(), 5).clone();
    let b = st.blocked.clone().unwrap();
    e.recover(&repo(), 5, &st, b).await;
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].starts_with("You took over this issue from a session on Claude Code"),
        "{}",
        prompts[0]
    );
    assert!(
        prompts[0].contains("The parser is half migrated; the flag is unverified."),
        "the summary is delivered: {}",
        prompts[0]
    );
    // Read once: the next start is not given it again.
    assert!(e.entry(&repo(), 5).handover_note.is_none());
}
#[tokio::test]
async fn a_started_harness_behind_a_start_block_is_told_before_the_block_lifts() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    d.with(|s| s.start_error = Some("pi did not become idle in time".into()));
    e.handover("o/r#5", "pi", None, None, Some("half done"), None)
        .await
        .unwrap();
    e.run_handovers(&repo()).await;
    assert!(e.entry(&repo(), 5).blocked.is_some());
    // The pane is there after all, and idle.
    d.seed("w5", "t9", READY_SCREEN);
    let _ = (d.log(), d.prompts(), stub.post_bodies());
    let st = e.entry(&repo(), 5).clone();
    let b = st.blocked.clone().unwrap();
    e.recover(&repo(), 5, &st, b).await;
    // Delivered into the pane that is there, not restarted.
    let log = d.log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(
        log[0].starts_with("deliver:w5:You took over this issue"),
        "{log:?}"
    );
    let prompts = d.prompts();
    assert!(prompts[0].contains("half done"), "{}", prompts[0]);
    let st = e.entry(&repo(), 5).clone();
    assert!(st.blocked.is_none(), "the block is lifted");
    assert!(st.handover_note.is_none(), "read once");
    assert_eq!(st.terminal_handle.as_deref(), Some("t9"));
    // What the story showed counts as seen, so the pass that follows
    // does not deliver it again.
    assert_eq!(st.updated_at.as_deref(), Some("u1"));
    assert!(st.seen.contains_key("assigned:1"), "{:?}", st.seen);
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert!(
        posts[0]
            .1
            .contains("ssf resuming deliveries to agent on issue:"),
        "{posts:?}"
    );
}
#[tokio::test]
async fn a_handover_blocked_at_the_sign_in_prompt_is_told_when_a_person_signs_in() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    d.with(|s| s.relaunch_screen = PI_LOGIN_SCREEN.iter().map(|l| l.to_string()).collect());
    e.handover("o/r#5", "pi", None, None, Some("half migrated"), None)
        .await
        .unwrap();
    e.run_handovers(&repo()).await;
    let st = e.entry(&repo(), 5).clone();
    assert_eq!(
        st.blocked.as_ref().map(|b| b.reason.as_str()),
        Some("login")
    );
    assert!(st.handover_note.is_some(), "nothing has read the summary");
    // A person runs the sign-in in the terminal: the pane that is
    // there is past its prompt, but was never told what it is for.
    let handle = st.terminal_handle.clone().expect("the pane came up");
    d.with(|s| {
        s.screens.insert(
            handle.clone(),
            READY_SCREEN.iter().map(|l| l.to_string()).collect(),
        )
    });
    let _ = (d.log(), d.prompts(), stub.post_bodies());
    let b = st.blocked.clone().unwrap();
    e.recover(&repo(), 5, &st, b).await;
    // Told where it stands, not restarted.
    let log = d.log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(
        log[0].starts_with("deliver:w5:You took over this issue"),
        "{log:?}"
    );
    let prompts = d.prompts();
    assert!(prompts[0].contains("half migrated"), "{}", prompts[0]);
    let st = e.entry(&repo(), 5).clone();
    assert!(st.blocked.is_none(), "the block is lifted");
    assert!(st.handover_note.is_none(), "read once");
    assert_eq!(st.terminal_handle.as_deref(), Some(handle.as_str()));
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert!(
        posts[0]
            .1
            .contains("ssf resuming deliveries to agent on issue:"),
        "{posts:?}"
    );
}
#[tokio::test]
async fn a_started_harness_is_told_once_per_backoff() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    d.with(|s| s.start_error = Some("pi did not become idle in time".into()));
    e.handover("o/r#5", "pi", None, None, Some("half done"), None)
        .await
        .unwrap();
    e.run_handovers(&repo()).await;
    // The pane is there after all, and the item cannot be read: the
    // message it is owed cannot be assembled.
    d.seed("w5", "t9", READY_SCREEN);
    stub.issues.lock().unwrap().remove(&5);
    let _ = (d.log(), d.prompts(), stub.post_bodies(), stub.hits());
    for _ in 0..2 {
        let st = e.entry(&repo(), 5).clone();
        let b = st.blocked.clone().expect("still blocked");
        e.recover(&repo(), 5, &st, b).await;
    }
    let reads = stub
        .hits()
        .into_iter()
        .filter(|h| h.starts_with("/repos/o/r/issues/5"))
        .count();
    assert_eq!(reads, 1, "one attempt, not one per pass");
    assert!(d.prompts().is_empty(), "nothing landed");
    let st = e.entry(&repo(), 5).clone();
    let b = st.blocked.expect("still blocked");
    assert_eq!(b.tell_failures, 1, "the next attempt waits");
    assert!(b.told_at.is_some());
    // The restart backoff is untouched: telling is not a restart.
    assert_eq!(b.retries, 0);
    assert!(b.retried_at.is_none());
    assert!(st.handover_note.is_some(), "the summary is still owed");
}
#[tokio::test]
async fn a_failed_restart_does_not_hold_up_telling_a_running_harness() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    d.with(|s| s.start_error = Some("pi did not become idle in time".into()));
    e.handover("o/r#5", "pi", None, None, Some("half done"), None)
        .await
        .unwrap();
    e.run_handovers(&repo()).await;
    // A restart was tried a moment ago and got nowhere.
    {
        let b = e.entry(&repo(), 5).blocked.as_mut().unwrap();
        b.retries = 1;
        b.retried_at = Some(now_iso());
    }
    // The pane is there after all, and past any prompt.
    d.seed("w5", "t9", READY_SCREEN);
    let _ = (d.log(), d.prompts(), stub.post_bodies());
    let st = e.entry(&repo(), 5).clone();
    let b = st.blocked.clone().unwrap();
    e.recover(&repo(), 5, &st, b).await;
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "told at once: {prompts:?}");
    assert!(prompts[0].contains("half done"), "{}", prompts[0]);
    let st = e.entry(&repo(), 5).clone();
    assert!(st.blocked.is_none(), "the block is lifted");
    assert!(st.handover_note.is_none(), "read once");
}
#[tokio::test]
async fn a_message_that_lands_in_a_sign_in_screen_keeps_the_hold_it_had() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    d.with(|s| s.start_error = Some("pi did not become idle in time".into()));
    e.handover("o/r#5", "pi", None, None, Some("half done"), None)
        .await
        .unwrap();
    e.run_handovers(&repo()).await;
    let before = e.entry(&repo(), 5).blocked.clone().expect("blocked");
    assert!(before.reported, "the item was told of the hold");
    // Nothing is live in the workspace by the time the message goes
    // out, and the harness started in its place shows Pi's sign-in
    // prompt.
    d.with(|s| {
        s.live.remove("w5");
        s.relaunch_screen = PI_LOGIN_SCREEN.iter().map(|l| l.to_string()).collect();
    });
    e.tell_a_started_harness(&repo(), 5, &before).await;
    let st = e.entry(&repo(), 5).clone();
    let b = st.blocked.clone().expect("still blocked");
    assert_eq!(b.reason, Blocked::LOGIN, "the fresher answer stands");
    assert!(b.detail.contains("/login"), "{}", b.detail);
    assert_eq!(b.since, before.since, "the same hold, from when it began");
    assert!(b.reported, "and the item is not told of it twice");
    assert_eq!(b.tell_failures, 1, "the message did not land");
    assert!(st.handover_note.is_some(), "the summary is still owed");
    // The pass that follows finds it reported: no second `blocked`.
    e.recover(&repo(), 5, &st, b).await;
    let posts = stub.post_bodies();
    let blocked = posts
        .iter()
        .filter(|(_, body)| body.contains("event=blocked"))
        .count();
    assert_eq!(blocked, 1, "one hold, one post: {posts:?}");
}
#[tokio::test]
async fn a_second_handover_without_a_summary_keeps_the_one_still_owed() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    d.with(|s| s.start_error = Some("pi exited at once".into()));
    e.handover("o/r#5", "pi", None, None, Some("half migrated"), None)
        .await
        .unwrap();
    e.run_handovers(&repo()).await;
    assert!(
        e.entry(&repo(), 5).handover_note.is_some(),
        "nobody read it"
    );
    let _ = (d.log(), d.prompts(), stub.post_bodies());
    // Handed on again with nothing to add: Codex still has to be
    // told what the session that did the work left.
    e.handover("o/r#5", "codex", None, None, None, None)
        .await
        .unwrap();
    e.run_handovers(&repo()).await;
    let prompts = d.prompts();
    assert!(
        prompts[0].starts_with("You took over this issue from a session on Claude Code"),
        "{}",
        prompts[0]
    );
    assert!(prompts[0].contains("half migrated"), "{}", prompts[0]);
    assert!(
        e.entry(&repo(), 5).handover_note.is_none(),
        "read at last, so nothing is owed"
    );
    // The post says what the new session was given, not what the
    // command carried.
    let posts = stub.post_bodies();
    let handed = posts
        .iter()
        .find(|(_, b)| b.contains("event=handed-over"))
        .expect("the handover is posted");
    assert!(handed.1.contains("\nsummary: yes\n"), "{}", handed.1);
}
#[tokio::test]
async fn a_second_handover_names_the_session_that_did_the_work() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    d.with(|s| s.start_error = Some("pi exited at once".into()));
    e.handover("o/r#5", "pi", None, None, Some("half migrated"), None)
        .await
        .unwrap();
    e.run_handovers(&repo()).await;
    assert_eq!(
        e.entry(&repo(), 5)
            .handover_note
            .as_ref()
            .map(|n| n.from.as_str()),
        Some("Claude Code")
    );
    let _ = (d.log(), d.prompts(), stub.post_bodies());
    e.handover(
        "o/r#5",
        "codex",
        None,
        None,
        Some("still half migrated"),
        None,
    )
    .await
    .unwrap();
    e.run_handovers(&repo()).await;
    let st = e.entry(&repo(), 5).clone();
    assert!(st.handover_note.is_none(), "Codex took it on");
    let prompts = d.prompts();
    assert!(
        prompts[0].starts_with("You took over this issue from a session on Claude Code"),
        "{}",
        prompts[0]
    );
    assert!(prompts[0].contains("still half migrated"), "{}", prompts[0]);
    // The post says what the item was configured on all the same.
    let posts = stub.post_bodies();
    let handed = posts
        .iter()
        .find(|(_, b)| b.contains("event=handed-over"))
        .expect("the handover is posted");
    assert!(handed.1.contains("\nfrom: Pi\n"), "{posts:?}");
    assert!(handed.1.contains("\nto: Codex\n"), "{posts:?}");
}
#[test]
fn a_handover_retires_the_workspace_s_last_conversation_too() {
    let id = |s: &str| Some(s.to_string());
    // Both known and different: both go.
    assert_eq!(
        retired_conversations(id("sess-5"), id("sess-6")),
        vec!["sess-5".to_string(), "sess-6".to_string()]
    );
    // The usual case: the record's id is the newest transcript.
    assert_eq!(
        retired_conversations(id("sess-5"), id("sess-5")),
        vec!["sess-5".to_string()]
    );
    // Never captured: the transcript alone is what there is to skip.
    assert_eq!(
        retired_conversations(None, id("sess-6")),
        vec!["sess-6".to_string()]
    );
    // A harness that keeps no transcripts (or an empty workspace):
    // nothing but the record's id.
    assert_eq!(
        retired_conversations(id("sess-5"), None),
        vec!["sess-5".to_string()]
    );
    assert!(retired_conversations(None, None).is_empty());
    // On the record, both are remembered and neither twice.
    let mut st = IssueState::default();
    retire(&mut st, &retired_conversations(id("sess-5"), id("sess-6")));
    retire(&mut st, &retired_conversations(id("sess-6"), None));
    assert_eq!(st.retired_session_ids, vec!["sess-5", "sess-6"]);
}
