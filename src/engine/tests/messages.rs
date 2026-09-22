use super::*;

/// The web API's message write reaches the item's agent over the daemon's own
/// delivery path: the same one a comment relay takes, so a prompt into a live
/// session is recorded exactly as its own activity is.
#[tokio::test]
async fn a_message_reaches_the_items_agent_the_way_its_activity_does() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
    let resp = e
        .handle_request(Request::Message {
            item: "o/r#5".into(),
            text: "the test is red again".into(),
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.data["session"], "o/r#5");
    assert_eq!(resp.data["title"], "Fix the widget");
    assert_eq!(resp.data["delivered"], true);
    assert!(
        d.log().iter().any(|line| line.starts_with("deliver:w5:")),
        "{:?}",
        d.log()
    );
    assert!(
        d.prompts()
            .iter()
            .any(|text| text.contains("the test is red again")),
        "{:?}",
        d.prompts()
    );
    let st = e.entry(&repo(), 5).clone();
    assert_eq!(st.prompts_sent, 1);
    assert!(st.last_prompt_at.is_some());
    assert_eq!(st.terminal_handle.as_deref(), Some("t5"));
}

/// An item ssf holds with no agent has nothing to tell: the write is refused
/// as the item's state (`409` on the web API), not delivered to nobody.
#[tokio::test]
async fn a_message_to_an_item_with_no_agent_is_a_refusal_about_the_item() {
    let mut e = engine();
    e.cfg.repos.push(repo());
    let resp = e
        .handle_request(Request::Message {
            item: "o/r#9".into(),
            text: "hello".into(),
        })
        .await;
    assert!(!resp.ok);
    assert_eq!(resp.kind, Some(crate::ipc::RefusalKind::Conflict));
    let error = resp.error.unwrap();
    assert!(error.contains("o/r#9 has no agent"), "{error}");
    // The item is named and the way to give it one is in the refusal, since
    // a person reading it in the overlay is the one who decides.
    assert!(error.contains("ssf assign o/r#9"), "{error}");
}

/// A session at its sign-in prompt takes nothing, and that is the item's state
/// rather than a failure of the write: the refusal carries the daemon's own
/// words about the block, and the message is not lost to a prompt nobody read.
#[tokio::test]
async fn a_message_to_a_blocked_session_is_a_refusal_about_the_block() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, LOGIN_SCREEN);
    e.entry(&repo(), 5).blocked = Some(Blocked {
        reason: "login".into(),
        harness: "claude".into(),
        detail: "Login expired · Please run /login".into(),
        since: now_iso(),
        reported: true,
        credential: Some("cred-old".into()),
        retried_at: None,
        retries: 0,
        told_at: None,
        tell_failures: 0,
    });
    let resp = e
        .handle_request(Request::Message {
            item: "o/r#5".into(),
            text: "hello".into(),
        })
        .await;
    assert!(!resp.ok);
    assert_eq!(resp.kind, Some(crate::ipc::RefusalKind::Conflict));
    let error = resp.error.unwrap();
    assert!(
        error.contains("Claude Code has been at its sign-in prompt"),
        "{error}"
    );
    assert!(error.contains("claude auth login"), "{error}");
    assert!(d.log().is_empty(), "nothing reaches a blocked session");
    assert_eq!(e.entry(&repo(), 5).prompts_sent, 0);
}

/// An item worked by another item's session is told through that session: the
/// message goes where the item's own activity goes, and the session named is
/// the one that acts on it.
#[tokio::test]
async fn a_message_to_a_bound_item_reaches_the_session_that_works_it() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
    seeded(&mut e, 6, Some("bot/issue-5"), true);
    {
        let st = e.entry(&repo(), 6);
        st.title = "Follow-up".into();
        st.worktree_id = Some("w5".into());
        st.worktree_path = Some("/w/5".into());
        st.shares_workspace_of = Some(5);
    }
    let resp = e
        .handle_request(Request::Message {
            item: "o/r#6".into(),
            text: "one more thing".into(),
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.data["session"], "o/r#5");
    assert!(
        d.log().iter().any(|line| line.starts_with("deliver:w5:")),
        "{:?}",
        d.log()
    );
    // The owner's bookkeeping moves, the bound item's does not.
    assert_eq!(e.entry(&repo(), 5).prompts_sent, 1);
    assert_eq!(e.entry(&repo(), 6).prompts_sent, 0);
}
