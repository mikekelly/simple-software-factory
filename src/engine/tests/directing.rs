//! A comment is never a command.
//!
//! Until #398 a comment whose first line was `/ssf <request>` was taken as a
//! request to the factory itself, and the daemon walked an item's timeline on
//! every attach, resume, relaunch and handover, so such a comment was re-read
//! again and again: the one that drove the issue sat on an item in
//! `mikekelly/ex_jev` and each pass acted on it, overriding what the operator
//! had since asked for at the terminal. Directing an item is `ssf handover`
//! and `ssf assign` now, and a comment is ordinary activity however it is
//! written, so what is asserted here is that nothing is taken, run, posted or
//! remembered for one.

use super::*;

/// The comment from the incident, verbatim: a person's, on the allow-list,
/// addressed to the old command.
const THE_INCIDENT: &str = "/ssf handover to claude / fable / low";

/// Nothing about an item's timeline that carries a `/ssf` comment is acted
/// on: onboarding the item attaches a session and says so, and the comment
/// reaches that session as the ordinary activity it is.
#[tokio::test]
async fn a_slash_command_on_an_item_is_ordinary_activity() {
    let sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];

    stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
    stub.set_timeline(
        5,
        vec![
            assigned_by(1, "alice"),
            comment(2, "mikekelly", THE_INCIDENT),
        ],
    );
    e.tick_repo(&r).await.unwrap();

    // One post, the attach: no `task-started`, no refusal, nothing about a
    // request, and the run the old code would have started leaves no log.
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert!(
        posts[0].1.contains("ssf attaching agent to issue:"),
        "{posts:?}"
    );
    assert!(
        !sandbox.state_dir().join("tasks").exists(),
        "nothing was run from the comment"
    );

    // It is delivered to the session like anything else said on the item,
    // and the daemon keeps nothing about it: the record the state file is
    // written from has no place for a request at all.
    let st = e.entry(&r, 5).clone();
    assert!(st.seen.contains_key("commented:2"), "{:?}", st.seen.keys());
    let saved = std::fs::read_to_string(sandbox.state_dir().join("state.json")).unwrap();
    assert!(!saved.contains("slash"), "{saved}");

    // And again on a later pass whose timeline still carries it: a
    // daemon that restarts, or a session brought back, re-reads the item
    // and finds nothing to take from it a second time either.
    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    stub.set_timeline(
        5,
        vec![
            assigned_by(1, "alice"),
            comment(2, "mikekelly", THE_INCIDENT),
        ],
    );
    e.tick_repo(&r).await.unwrap();
    assert!(stub.posts().is_empty(), "{:?}", stub.post_bodies());
    assert!(
        !sandbox.state_dir().join("tasks").exists(),
        "nothing was run from the comment"
    );
}
