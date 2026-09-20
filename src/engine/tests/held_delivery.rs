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
