use super::*;

/// Session 9 follows item 5, which session 5 works on: both are live on
/// their own workspaces, and the item is on the assignee listing.
async fn follower_setup(
    stub: &GitHubStub,
    events: Events,
) -> (Engine, crate::driver::StubDriver, RepoConfig) {
    let (mut e, d) = blocked_setup(stub, READY_SCREEN);
    let r = repo();
    seeded(&mut e, 9, Some("bot/issue-9"), true);
    {
        let st = e.entry(&r, 9);
        st.worktree_id = Some("w9".into());
        st.terminal_handle = Some("t9".into());
        st.updated_at = Some("u1".into());
    }
    d.seed("w9", "t9", READY_SCREEN);
    let resp = e
        .handle_request(Request::Sub {
            from: "o/r#9".into(),
            target: "o/r#5".into(),
            events,
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    // The assignment that put the item on session 5's plate was delivered
    // when that session was started: it is not the news in these tests.
    e.entry(&r, 5)
        .seen
        .insert("assigned:1".into(), String::new());
    (e, d, r)
}

/// A timeline with the assignment above it, and whatever came after.
fn timeline(after: Vec<Value>) -> Vec<Value> {
    let mut t = vec![assigned_by(1, "alice")];
    t.extend(after);
    t
}

fn labeled(id: u64, who: &str, name: &str) -> Value {
    json!({"event":"labeled","id":id,"actor":{"login":who},
            "label":{"name":name},"created_at":"t"})
}

/// What a follower hears by default is the item's own state changing. A
/// comment on the item is not news for it: every FYI is a turn in the
/// follower's own session, and an item it does not work on must not be able
/// to spend them (#453).
#[tokio::test]
async fn a_follower_hears_the_items_state_changing_and_not_what_is_said_on_it() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d, r) = follower_setup(&stub, Events::State).await;

    // Alice comments on the item session 5 works on.
    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    stub.set_timeline(5, timeline(vec![comment(2, "alice", "how is it going?")]));
    e.tick_repo(&r).await.unwrap();

    let log = d.log();
    assert!(
        log.iter().any(|l| l.starts_with("deliver:w5:")),
        "the item's own session hears its activity: {log:?}"
    );
    assert!(
        !log.iter().any(|l| l.starts_with("deliver:w9:")),
        "a comment is not fanned out to a follower at the default level: {log:?}"
    );
    assert!(
        d.prompts().iter().any(|p| p.contains("how is it going?")),
        "the comment reached the item's own session"
    );

    // When the item itself moves, the follower hears it.
    stub.set_assigned(vec![assigned_item(5, "alice", "u3")]);
    stub.set_timeline(
        5,
        timeline(vec![
            comment(2, "alice", "how is it going?"),
            labeled(3, "alice", "urgent"),
        ]),
    );
    let _ = d.log();
    e.tick_repo(&r).await.unwrap();

    let log = d.log();
    assert!(
        log.iter().any(|l| l.starts_with("deliver:w9:")),
        "a label on the item is fanned out: {log:?}"
    );
    let fyi = d
        .prompts()
        .into_iter()
        .find(|p| p.contains("FYI"))
        .expect("an FYI for the follower");
    assert!(fyi.contains("added label \"urgent\""), "{fyi}");
    assert!(
        !fyi.contains("how is it going?"),
        "the follower is not shown the comments around it: {fyi}"
    );
}

/// `ssf sub --events all` is the way to ask for the rest, and following the
/// item again is how a follower asks for it: the same comment then arrives
/// as an FYI.
#[tokio::test]
async fn a_follower_that_asks_for_everything_hears_the_comments() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d, r) = follower_setup(&stub, Events::State).await;

    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    stub.set_timeline(5, timeline(vec![comment(2, "alice", "the first word")]));
    e.tick_repo(&r).await.unwrap();
    let _ = d.log();

    let resp = e
        .handle_request(Request::Sub {
            from: "o/r#9".into(),
            target: "o/r#5".into(),
            events: Events::All,
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.data["added"], false, "one subscription, not two");
    assert_eq!(resp.data["changed"], true, "its level moved");

    stub.set_assigned(vec![assigned_item(5, "alice", "u3")]);
    stub.set_timeline(
        5,
        timeline(vec![
            comment(2, "alice", "the first word"),
            comment(3, "alice", "the second word"),
        ]),
    );
    e.tick_repo(&r).await.unwrap();

    let prompts = d.prompts();
    let fyi = prompts
        .iter()
        .find(|p| p.contains("FYI"))
        .expect("an FYI for the follower");
    assert!(
        fyi.contains("the second word"),
        "the comment is fanned out at this level: {fyi}"
    );
    assert!(
        !fyi.contains("the first word"),
        "what was withheld is not replayed: {fyi}"
    );
}

/// An item that never had a session of its own is polled for its followers
/// alone, so the level is what stands between a busy item and a follower's
/// context there too: the poll keeps its place either way, and the state
/// change that follows is delivered without the comments beside it.
#[tokio::test]
async fn a_subscriber_only_item_delivers_at_the_followers_level() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos.push(r.clone());
    seeded(&mut e, 9, Some("bot/issue-9"), true);
    {
        let st = e.entry(&r, 9);
        st.worktree_id = Some("w9".into());
        st.terminal_handle = Some("t9".into());
    }
    d.seed("w9", "t9", READY_SCREEN);
    // Item 3 is nobody's: following it makes it tracked for session 9.
    stub.set_issue(
        3,
        json!({"number": 3, "title": "Three", "body": "", "html_url": "https://gh/3",
               "state": "open", "user": {"login": "alice"}, "created_at": "x",
               "updated_at": "u1"}),
    );
    stub.set_timeline(3, vec![]);
    let resp = e
        .handle_request(Request::Sub {
            from: "o/r#9".into(),
            target: "o/r#3".into(),
            events: Events::State,
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    assert!(e.entry(&r, 3).subscriber_only);

    // A comment lands on it: polled, seen, and not delivered.
    stub.set_issue(
        3,
        json!({"number": 3, "title": "Three", "body": "", "html_url": "https://gh/3",
               "state": "open", "user": {"login": "alice"}, "created_at": "x",
               "updated_at": "u2"}),
    );
    stub.set_timeline(3, vec![comment(1, "alice", "anyone there?")]);
    e.tick_repo(&r).await.unwrap();

    assert!(d.log().is_empty(), "{:?}", d.log());
    assert!(
        e.entry(&r, 3).seen.contains_key("commented:1"),
        "the poll keeps its place: the comment is not news later either"
    );

    // The item moving is delivered, this time without the comment.
    stub.set_issue(
        3,
        json!({"number": 3, "title": "Three", "body": "", "html_url": "https://gh/3",
               "state": "open", "user": {"login": "alice"}, "created_at": "x",
               "updated_at": "u3"}),
    );
    stub.set_timeline(
        3,
        vec![
            comment(1, "alice", "anyone there?"),
            labeled(2, "alice", "urgent"),
        ],
    );
    e.tick_repo(&r).await.unwrap();

    let log = d.log();
    assert_eq!(log.len(), 1, "{log:?}");
    let fyi = d.prompts().pop().expect("an FYI");
    assert!(fyi.contains("added label \"urgent\""), "{fyi}");
    assert!(!fyi.contains("anyone there?"), "{fyi}");
}

/// The parent's follow of a child it delegated is written once. The child is
/// re-onboarded whenever its binding is given up (a run of delivery failures
/// does that), and the parent's level must survive it: a parent that asked
/// for the comments with `ssf sub --events all` is not silently put back on
/// state changes (#453).
#[tokio::test]
async fn a_re_onboarded_child_leaves_the_parents_level_alone() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    seeded(&mut e, 1, Some("bot/issue-1"), true);
    {
        let st = e.entry(&r, 1);
        st.worktree_id = Some("w1".into());
        st.terminal_handle = Some("t1".into());
    }
    d.seed("w1", "t1", READY_SCREEN);
    let opened = json!({
        "number": 7, "title": "child",
        "body": "🤖#1 says: <!-- ssf: origin=o/r#1 mode=delegate -->\n\nover to you",
        "html_url": "https://gh/7", "state": "open", "user": {"login": "bot"},
        "assignees": [{"login": "bot"}], "created_at": "x", "updated_at": "u1"
    });
    stub.set_assigned(vec![opened.clone()]);
    stub.set_timeline(7, vec![assigned_by(1, "bot")]);
    e.tick_repo(&r).await.unwrap();
    assert_eq!(e.entry(&r, 7).subscribers, vec!["o/r#1"]);

    // The parent asks for everything on its child.
    let resp = e
        .handle_request(Request::Sub {
            from: "o/r#1".into(),
            target: "o/r#7".into(),
            events: Events::All,
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(e.entry(&r, 7).events_for("o/r#1"), Events::All);

    // The child's binding is given up: `note_failure` clears the seed after
    // a run of failures, and the next pass onboards it again.
    e.entry(&r, 7).seeded = false;
    e.entry(&r, 7).active = false;
    stub.set_assigned(vec![opened]);
    e.tick_repo(&r).await.unwrap();

    assert!(
        e.entry(&r, 7).seeded,
        "the pass onboarded it again: {:?}",
        e.entry(&r, 7)
    );
    assert_eq!(e.entry(&r, 7).subscribers, vec!["o/r#1"], "one follow");
    assert_eq!(
        e.entry(&r, 7).events_for("o/r#1"),
        Events::All,
        "the parent's level survived the re-onboard"
    );
}
