use super::*;

// The repository the held-delivery test runs on: the OMP/Pi mailbox is the
// channel this hold belongs to.
fn omp_repo() -> RepoConfig {
    RepoConfig {
        harness: "omp".into(),
        ..repo()
    }
}

/// An event the harness has taken but the session has not recorded yet leaves
/// a newer one waiting. Nothing was published for it, so it must not be
/// reported as delivered: the item's watermark and prompt count stay where
/// they were, no failure is counted, and the item is armed for another look,
/// because what clears the hold happens inside the harness and no listing
/// change announces it (#390).
#[tokio::test]
async fn a_held_delivery_is_not_a_delivery_and_earns_another_look() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
    let r = omp_repo();
    e.cfg.repos = vec![r.clone()];
    d.with(|s| s.deliver_held = true);
    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    stub.set_timeline(
        5,
        vec![assigned_by(1, "alice"), comment(2, "alice", "please hurry")],
    );

    e.tick_repo(&r).await.unwrap();

    // Nothing reached the harness, and the item keeps what it is owed.
    assert!(d.log().is_empty(), "nothing may reach a held session");
    let st = e.entry(&r, 5).clone();
    assert_eq!(
        st.updated_at.as_deref(),
        Some("u1"),
        "a held delivery is not a delivery"
    );
    assert!(!st.seen.contains_key("commented:2"), "{:?}", st.seen.keys());
    assert_eq!(st.prompts_sent, 0, "no prompt was sent");
    assert!(
        e.failures.is_empty(),
        "a busy session is not a failing item: {:?}",
        e.failures
    );
    assert!(
        e.refetch.contains(&r.name),
        "the held item is armed so the next pass looks again"
    );

    // Once the hold clears, the same activity goes out on the next pass --
    // without any new event on the item, which is the whole point of the
    // arming above.
    d.with(|s| s.deliver_held = false);
    e.tick_repo(&r).await.unwrap();
    let log = d.log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(log[0].starts_with("deliver:w5:"), "{log:?}");
    let st = e.entry(&r, 5).clone();
    assert!(st.seen.contains_key("commented:2"), "{:?}", st.seen.keys());
    assert_eq!(st.prompts_sent, 1);
    assert!(e.failures.is_empty(), "{:?}", e.failures);
}

/// A session whose mailbox has no live bridge behind it -- the marker was
/// lost and the poller that repairs it is not running, or the session came up
/// without the bridge at all -- is not a failing item.  Nothing was published
/// for it, the events stay on the item, and what fixes it is a session
/// restart: giving the binding up would drop the item's session and deliver
/// nothing, which is what the incident behind #395 looked like from the item.
#[tokio::test]
async fn a_session_without_a_live_bridge_keeps_its_binding_and_its_events() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
    let r = omp_repo();
    e.cfg.repos = vec![r.clone()];
    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    stub.set_timeline(
        5,
        vec![assigned_by(1, "alice"), comment(2, "alice", "please hurry")],
    );
    d.with(|s| s.deliver_unavailable = true);

    // Well past the point a counted failure would have given the binding up.
    for _ in 0..MAX_DELIVERY_FAILURES + 2 {
        e.tick_repo(&r).await.unwrap();
    }

    let st = e.entry(&r, 5).clone();
    assert!(st.seeded, "the binding was dropped: {st:?}");
    assert!(
        !st.seen.contains_key("commented:2"),
        "an event nobody took counts as seen: {:?}",
        st.seen.keys()
    );
    assert_eq!(st.prompts_sent, 0);
    assert!(
        e.failures.is_empty(),
        "a missing bridge is not a failing item: {:?}",
        e.failures
    );
    let posts = stub.post_bodies();
    assert!(
        posts.is_empty(),
        "none of this is an item's news: {posts:?}"
    );
    assert!(
        e.refetch.contains(&r.name),
        "the item is armed so the next pass looks again"
    );
    assert!(
        e.channel_lost.contains(&(r.name.clone(), 5)),
        "the incident is not said at all"
    );

    // The bridge comes back (the session was restarted, or the poller
    // repaired the marker) and the same activity goes out on the next pass.
    d.with(|s| s.deliver_unavailable = false);
    e.tick_repo(&r).await.unwrap();
    let st = e.entry(&r, 5).clone();
    assert!(st.seen.contains_key("commented:2"), "{:?}", st.seen.keys());
    assert_eq!(st.prompts_sent, 1);
    assert!(e.failures.is_empty(), "{:?}", e.failures);
    assert!(
        !e.channel_lost.contains(&(r.name.clone(), 5)),
        "the incident stays said after the channel is back"
    );
}
