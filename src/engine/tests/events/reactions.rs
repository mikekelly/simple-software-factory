use super::*;

// #566: reactions leave `updated_at` alone, so they are found through the
// timeline's ETag and delivered as one line each: who, which emoji, where.

fn reacted_comment(counts: Value) -> Value {
    let mut c = comment(2, "alice", "the long comment body");
    c["reactions"] = counts;
    c
}

fn item_with(counts: Value) -> Value {
    let mut i = assigned_item(5, "alice", "u2");
    i["reactions"] = counts;
    i
}

/// A session on #5 whose follow-up has seen the item once at `u2`, with
/// alice's 👍 already on comment 2: recorded, not delivered.
async fn baselined(stub: &GitHubStub) -> (Engine, crate::driver::StubDriver) {
    let (mut e, d) = blocked_setup(stub, READY_SCREEN);
    let r = repo();
    stub.set_issue(5, item_with(json!({"total_count": 0})));
    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    stub.set_timeline(
        5,
        vec![
            assigned_by(1, "alice"),
            reacted_comment(json!({"total_count": 1, "+1": 1})),
        ],
    );
    stub.set_reactions("issues/comments/2", &[("alice", "+1")]);
    {
        let st = e.entry(&r, 5);
        st.seen.insert("assigned:1".into(), String::new());
        st.seen.insert("commented:2".into(), "t".into());
    }
    e.tick_repo(&r).await.unwrap();
    assert!(
        d.prompts().is_empty(),
        "reactions already there are not news"
    );
    let st = e.entry(&r, 5).clone();
    assert_eq!(
        st.seen.get("reactions:commented:2").map(String::as_str),
        Some("alice:+1")
    );
    assert!(!st.timeline_etags.is_empty());
    let _ = (d.log(), stub.hits());
    (e, d)
}

#[tokio::test]
async fn a_reaction_is_delivered_and_a_quiet_poll_costs_a_304() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = baselined(&stub).await;
    let r = repo();

    // Nothing moved: the timeline answers 304 and nothing is fetched or said.
    e.tick_repo(&r).await.unwrap();
    assert!(d.prompts().is_empty());
    let hits = stub.hits();
    assert!(!hits.iter().any(|h| h == "/repos/o/r/issues/5"), "{hits:?}");
    assert!(!hits.iter().any(|h| h.contains("reactions")), "{hits:?}");

    // bob adds ❤️: the item's `updated_at` stays, the timeline moves.
    stub.set_timeline(
        5,
        vec![
            assigned_by(1, "alice"),
            reacted_comment(json!({"total_count": 2, "+1": 1, "heart": 1})),
        ],
    );
    stub.set_reactions("issues/comments/2", &[("alice", "+1"), ("bob", "heart")]);
    e.tick_repo(&r).await.unwrap();
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("@bob reacted ❤️ to u2"),
        "{}",
        prompts[0]
    );
    assert!(
        !prompts[0].contains("the long comment body"),
        "{}",
        prompts[0]
    );
    assert!(!prompts[0].contains("@alice"), "{}", prompts[0]);

    // And it is not said twice.
    e.tick_repo(&r).await.unwrap();
    assert!(d.prompts().is_empty());
}

#[tokio::test]
async fn removals_and_swaps_are_delivered() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = baselined(&stub).await;
    let r = repo();

    // alice swaps 👍 for 👎.
    stub.set_timeline(
        5,
        vec![
            assigned_by(1, "alice"),
            reacted_comment(json!({"total_count": 1, "-1": 1})),
        ],
    );
    stub.set_reactions("issues/comments/2", &[("alice", "-1")]);
    e.tick_repo(&r).await.unwrap();
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("@alice reacted 👎 to u2"),
        "{}",
        prompts[0]
    );
    assert!(
        prompts[0].contains("@alice removed 👍 from u2"),
        "{}",
        prompts[0]
    );

    // And takes it back.
    stub.set_timeline(
        5,
        vec![
            assigned_by(1, "alice"),
            reacted_comment(json!({"total_count": 0})),
        ],
    );
    stub.set_reactions("issues/comments/2", &[]);
    e.tick_repo(&r).await.unwrap();
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("@alice removed 👎 from u2"),
        "{}",
        prompts[0]
    );
}

#[tokio::test]
async fn refused_logins_and_the_bot_s_own_reactions_are_recorded_not_delivered() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = baselined(&stub).await;
    let r = repo();
    e.cfg.daemon.allowed_users = Some(vec!["alice".into()]);

    stub.set_timeline(
        5,
        vec![
            assigned_by(1, "alice"),
            reacted_comment(json!({"total_count": 3, "+1": 1, "eyes": 1, "rocket": 1})),
        ],
    );
    stub.set_reactions(
        "issues/comments/2",
        &[("alice", "+1"), ("mallory", "eyes"), ("bot", "rocket")],
    );
    e.tick_repo(&r).await.unwrap();
    assert!(d.prompts().is_empty());
    assert_eq!(
        e.entry(&r, 5)
            .seen
            .get("reactions:commented:2")
            .map(String::as_str),
        Some("alice:+1 bot:rocket mallory:eyes")
    );
}

#[tokio::test]
async fn reactions_can_be_ignored_like_any_event() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = baselined(&stub).await;
    let r = repo();
    e.cfg.daemon.ignored_events.push("reacted".into());
    stub.set_timeline(
        5,
        vec![
            assigned_by(1, "alice"),
            reacted_comment(json!({"total_count": 2, "+1": 1, "heart": 1})),
        ],
    );
    stub.set_reactions("issues/comments/2", &[("alice", "+1"), ("bob", "heart")]);
    e.tick_repo(&r).await.unwrap();
    assert!(d.prompts().is_empty());
}

#[tokio::test]
async fn a_reaction_on_the_body_is_delivered_with_the_next_timeline_look() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = baselined(&stub).await;
    let r = repo();
    stub.set_issue(5, item_with(json!({"total_count": 1, "hooray": 1})));
    stub.set_reactions("issues/5", &[("carol", "hooray")]);
    // Something moves the timeline (here: bob's ❤️ on the comment).
    stub.set_timeline(
        5,
        vec![
            assigned_by(1, "alice"),
            reacted_comment(json!({"total_count": 2, "+1": 1, "heart": 1})),
        ],
    );
    stub.set_reactions("issues/comments/2", &[("alice", "+1"), ("bob", "heart")]);
    e.tick_repo(&r).await.unwrap();
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("@carol reacted 🎉 to https://gh/5"),
        "{}",
        prompts[0]
    );
}
