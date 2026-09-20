//! A comment that reads like the old `/ssf` slash-command is an ordinary
//! comment (#397).
//!
//! `/ssf <request>` used to be taken from an item's timeline wherever the
//! daemon already read it, and run as a one-off headless task. That made a
//! comment durable state: the request was remembered on the item's record and
//! taken on every later walk of the timeline, so a `/ssf handover ...` a
//! person left before an operator switched the factory's stack pulled the item
//! back onto the old one at the next session start, and kept doing it until
//! the comment was deleted. The parser is gone; this pins that such a comment
//! is now only activity for the item's session, on an item with everything a
//! task would need to run.

use super::*;

/// Handing the item over is the whole reason a person wrote one of these, so
/// a replay shows up as a run of the repository's harness and a task post on
/// the item. Neither may happen: the comment reaches the session like any
/// other, and the stack the operator configured stands.
#[tokio::test]
async fn a_comment_that_reads_like_the_old_command_is_only_activity() {
    use crate::release::testkit::scratch;

    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    // A real checkout, so nothing but the removed parser stands between this
    // comment and a task being started for it.
    let s = scratch("slash-comment").await;
    let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
    let mut r = repo();
    r.path = Some(s.work.clone());
    e.cfg.repos = vec![r.clone()];
    e.entry(&r, 5).repo_id = Some(s.work.clone());
    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    stub.set_timeline(
        5,
        vec![
            assigned_by(1, "alice"),
            comment(2, "alice", "/ssf handover to claude / fable / low"),
        ],
    );

    e.tick_repo(&r).await.unwrap();

    assert!(
        e.entry(&r, 5).handover.is_none(),
        "the comment is not an instruction to the factory"
    );
    assert_eq!(
        e.entry(&r, 5).overrides,
        None,
        "the item keeps the stack it was running"
    );
    let log = d.log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(log[0].starts_with("deliver:w5:"), "{log:?}");
    let prompts = d.with(|s| s.prompts.clone());
    assert!(
        prompts
            .iter()
            .any(|p| p.contains("/ssf handover to claude / fable / low")),
        "the comment reaches the session as activity: {prompts:?}"
    );
    let posts = stub.post_bodies();
    assert!(
        !posts.iter().any(|(_, body)| body.contains("event=task-")),
        "no task was started or refused for it: {posts:?}"
    );
    // And nothing about the comment is remembered: the request used to be
    // kept on the item's record and taken again on every later walk of the
    // timeline, which is what let one old comment outlive the operator's
    // changes. What the item's record holds is what a later pass reads.
    let record = serde_json::to_value(e.entry(&r, 5)).unwrap();
    let remembered: Vec<&str> = record
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| k.starts_with("slash"))
        .map(String::as_str)
        .collect();
    assert!(remembered.is_empty(), "remembered: {remembered:?}");
}
