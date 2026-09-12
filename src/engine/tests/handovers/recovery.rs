use super::*;

#[test]
fn the_capture_window_never_reaches_back_past_a_handover() {
    let at = |s: &str| SystemTime::from(chrono::DateTime::parse_from_rfc3339(s).unwrap());
    let launched = "2026-09-07T12:00:30Z";
    // No handover: the slack stands.
    assert_eq!(
        capture_since(launched, None),
        at("2026-09-07T12:00:25Z"),
        "five seconds of slack"
    );
    // The handover is inside the slack: the window starts there.
    assert_eq!(
        capture_since(launched, Some("2026-09-07T12:00:28Z")),
        at("2026-09-07T12:00:28Z")
    );
    // A later relaunch is long past it: the slack stands again.
    assert_eq!(
        capture_since("2026-09-07T13:00:30Z", Some("2026-09-07T12:00:28Z")),
        at("2026-09-07T13:00:25Z")
    );
    // Nothing readable: everything is too new to adopt.
    assert_eq!(capture_since("not a time", None), SystemTime::UNIX_EPOCH);
}
#[test]
fn events_by_unlisted_users_are_not_delivered() {
    let mut e = engine();
    e.cfg.daemon.allowed_users = Some(vec!["alice".into()]);
    let r = repo();
    e.cfg.repos.push(r.clone());
    let timeline = vec![
        comment(1, "alice", "hi"),
        comment(2, "Mallory", "evil"),
        comment(3, "bot", "<!-- ssf: origin=o/r#1 -->\n\nfrom one"),
        comment(4, "bot", "typed as the bot"),
        json!({"event":"labeled","id":5,"actor":{"login":"mallory"},"label":{"name":"review"},"created_at":"t"}),
        json!({"event":"labeled","id":6,"actor":{"login":"ALICE"},"label":{"name":"bug"},"created_at":"t"}),
        json!({"event":"committed","sha":"abc123def","author":{"name":"Mallory","date":"t"},"message":"m"}),
        json!({"event":"reviewed","id":8,"user":{"login":"mallory"},"state":"approved","body":"lgtm","created_at":"t"}),
        json!({"event":"line-commented","id":9,"comments":[
                {"id":91,"user":{"login":"mallory"},"body":"x","path":"a","line":1,"created_at":"t"},
                {"id":92,"user":{"login":"alice"},"body":"y","path":"a","line":2,"created_at":"t"}]}),
        json!({"event":"commented","id":10,"user":{"login":"github-project-automation[bot]"},"body":"moved","created_at":"t"}),
        json!({"event":"line-commented","id":11,"comments":[
                {"id":93,"user":{"login":"mallory"},"body":"z","path":"a","line":1,"created_at":"t"}]}),
    ];
    let keys = |d: &Diff| d.rendered.iter().map(|r| r.key.clone()).collect::<Vec<_>>();
    let d = e.diff(&r, &BTreeMap::new(), &timeline);
    assert_eq!(
        keys(&d),
        vec![
            "commented:1",
            "commented:3",
            "commented:4",
            "labeled:6",
            "committed:abc123def",
            "line-commented:9"
        ]
    );
    // Only alice's line comment is rendered out of the batch.
    let batch = &d.rendered[5];
    assert!(
        batch.text.contains("@alice") && !batch.text.contains("mallory"),
        "{}",
        batch.text
    );
    // Everything is still counted as seen, so nothing dropped comes
    // back as news later, and each drop is remembered so it is logged
    // once (info) however often the timeline is walked again.
    assert_eq!(d.seen.len(), 11);
    assert!(e.dropped_logged.lock().unwrap().contains("o/r:commented:2"));
    assert_eq!(e.dropped_logged.lock().unwrap().len(), 6);
    // A repository list replaces the instance list.
    e.cfg.repos[0].allowed_users = Some(vec!["MALLORY".into()]);
    let r = e.cfg.repos[0].clone();
    let d = e.diff(&r, &BTreeMap::new(), &timeline);
    assert_eq!(
        keys(&d),
        vec![
            "commented:2",
            "commented:3",
            "commented:4",
            "labeled:5",
            "committed:abc123def",
            "reviewed:8",
            "line-commented:9",
            "line-commented:11"
        ]
    );
    // The wildcard delivers everything, bot accounts included.
    e.cfg.repos[0].allowed_users = Some(vec!["*".into()]);
    let r = e.cfg.repos[0].clone();
    assert_eq!(e.diff(&r, &BTreeMap::new(), &timeline).rendered.len(), 11);
    // No list at all and no collaborators fetched yet: nobody but the bot.
    e.cfg.repos[0].allowed_users = None;
    e.cfg.daemon.allowed_users = None;
    let r = e.cfg.repos[0].clone();
    assert_eq!(
        keys(&e.diff(&r, &BTreeMap::new(), &timeline)),
        vec!["commented:3", "commented:4", "committed:abc123def"]
    );
    assert_eq!(e.allow_list(&r).source, Source::Collaborators);
}
#[tokio::test]
async fn items_asked_for_by_unlisted_users_are_ignored_until_an_allowed_user_asks() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    e.cfg.daemon.allowed_users = Some(vec!["Alice".into()]);
    let r = repo();
    e.cfg.repos.push(r.clone());
    stub.set_assigned(vec![assigned_item(5, "mallory", "u1")]);
    stub.set_timeline(5, vec![assigned_by(1, "mallory")]);
    e.tick_repo(&r).await.unwrap();
    let rs = e.state.repos.get("o/r").unwrap();
    assert_eq!(
        rs.ignored.get(&5).map(|i| i.updated_at.as_str()),
        Some("u1")
    );
    assert!(rs.issues.get(&5).is_none_or(|s| !s.seeded), "no session");
    assert!(e.failures.is_empty(), "a refusal is not a failure");
    assert!(stub.hits().iter().any(|h| h.contains("/issues/5/timeline")));
    // Unchanged: not read again.
    e.tick_repo(&r).await.unwrap();
    assert!(!stub.hits().iter().any(|h| h.contains("/issues/5/")));
    // Alice assigns the bot herself: the item changed, and now it is
    // taken on. The driver is not there in this test, so onboarding
    // fails after the gate, which is the point: the ignore record is
    // gone and a delivery failure is counted instead.
    stub.set_assigned(vec![assigned_item(5, "mallory", "u2")]);
    stub.set_timeline(5, vec![assigned_by(1, "mallory"), assigned_by(2, "alice")]);
    e.tick_repo(&r).await.unwrap();
    let rs = e.state.repos.get("o/r").unwrap();
    assert!(!rs.ignored.contains_key(&5));
    assert_eq!(e.failures.get(&("o/r".to_string(), 5)), Some(&1));
}
#[tokio::test]
async fn a_retired_item_assigned_again_by_an_unlisted_user_stays_retired() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    e.cfg.daemon.allowed_users = Some(vec!["alice".into()]);
    let r = repo();
    e.cfg.repos.push(r.clone());
    seeded(&mut e, 5, Some("bot/issue-5"), false);
    e.entry(&r, 5).triggers = vec!["assigned".into()];
    stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
    stub.set_timeline(5, vec![assigned_by(1, "alice"), assigned_by(2, "mallory")]);
    e.tick_repo(&r).await.unwrap();
    assert!(!e.peek(&r, 5).unwrap().active);
    assert!(e.state.repos["o/r"].ignored.contains_key(&5));
    assert!(e.failures.is_empty());
}
#[tokio::test]
async fn the_startup_pass_knows_the_collaborators_before_it_resumes_anything() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    e.cfg.daemon.allowed_users = None;
    e.cfg.daemon.accepted_anyone_risk = false;
    let r = repo();
    e.cfg.repos.push(r.clone());
    seeded(&mut e, 1, Some("bot/issue-1"), true);
    e.entry(&r, 1).worktree_id = Some("wt1".into());
    // No answer from GitHub: the repository is skipped, nothing is
    // resumed on a guess.
    e.resume_interrupted(&[DriverKind::Orca]).await;
    assert!(stub.hits().iter().any(|h| h.contains("/collaborators")));
    assert!(!e.allow_list(&r).allows("alice"));
    // With an answer, the list is in place before any session is
    // looked at (the driver is not there in this test, so the session
    // itself is left alone after that).
    stub.set_collaborators(Some(vec![
        json!({"login": "alice", "permissions": {"push": true}}),
    ]));
    e.resume_interrupted(&[DriverKind::Orca]).await;
    assert!(e.allow_list(&r).allows("alice"));
    assert_eq!(e.allow_list(&r).source, Source::Collaborators);
}
#[tokio::test]
async fn collaborators_with_push_access_are_the_default_list() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    e.cfg.daemon.allowed_users = None;
    e.cfg.daemon.accepted_anyone_risk = false;
    let r = repo();
    e.cfg.repos.push(r.clone());
    // No list and no answer from GitHub: the pass fails, closed.
    let err = e.tick_repo(&r).await.unwrap_err();
    assert!(format!("{err:#}").contains("collaborators"), "{err:#}");
    assert!(!e.allow_list(&r).allows("alice"));
    stub.set_collaborators(Some(vec![
        json!({"login": "Alice", "permissions": {"pull": true, "push": true}}),
        json!({"login": "reader", "permissions": {"pull": true, "push": false}}),
        json!({"login": "some-app[bot]", "permissions": {"push": true}}),
    ]));
    e.tick_repo(&r).await.unwrap();
    let l = e.allow_list(&r);
    assert!(l.allows("alice") && l.allows("ALICE") && l.allows("bot"));
    assert!(!l.allows("reader") && !l.allows("some-app[bot]"));
    assert_eq!(l.source, Source::Collaborators);
    assert_eq!(l.describe(), "@alice (collaborators with push access)");
    // Once per pass, conditionally: the second answer is a 304.
    stub.hits();
    e.tick_repo(&r).await.unwrap();
    let hits = stub.hits();
    assert_eq!(
        hits.iter().filter(|h| h.contains("/collaborators")).count(),
        1
    );
    assert!(e.allow_list(&r).allows("alice"));
    // A refresh that fails keeps the last list.
    stub.set_collaborators(None);
    e.tick_repo(&r).await.unwrap();
    assert!(e.allow_list(&r).allows("alice"));
    // A configured list is used instead, and nothing is fetched.
    e.cfg.repos[0].allowed_users = Some(vec![]);
    let r = e.cfg.repos[0].clone();
    stub.hits();
    e.tick_repo(&r).await.unwrap();
    assert!(!stub.hits().iter().any(|h| h.contains("/collaborators")));
    assert!(!e.allow_list(&r).allows("alice"));
    assert_eq!(e.allow_list(&r).source, Source::Repo);
}
#[tokio::test]
async fn a_mention_still_in_the_item_holds_the_session_through_an_empty_listing() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let r = repo();
    let item = |body: &str| {
        json!({
            "number": 5, "title": "t", "body": body, "html_url": "https://gh/5",
            "state": "open", "user": {"login": "alice"},
            "created_at": "x", "updated_at": "u1"
        })
    };
    let comment = |body: &str| {
        json!({
            "event": "commented", "body": body, "html_url": "https://gh/5#c1",
            "updated_at": "u2", "actor": {"login": "alice"}, "user": {"login": "alice"}
        })
    };
    // The stub's mentioned listing is always empty, which is the
    // listing that retired this session on the live factory.
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.repos = vec![r.clone()];
    seeded(&mut e, 5, Some("bot/issue-5"), true);
    {
        let st = e.entry(&r, 5);
        st.triggers = vec!["mentioned".into()];
        st.worktree_id = Some("w5".into());
        st.worktree_path = Some("/w/5".into());
        st.terminal_handle = Some("t5".into());
    }
    d.seed("w5", "t5", READY_SCREEN);

    // The mention is in the item's body: the session stays.
    stub.set_issue(5, item("please look @bot"));
    stub.set_timeline(5, vec![]);
    e.tick_repo(&r).await.unwrap();
    assert!(
        e.entry(&r, 5).active,
        "retired although the body still mentions the bot"
    );

    assert!(
        e.entry(&r, 5).retirement_held_at.is_some(),
        "the hold was not recorded"
    );
    assert!(
        e.entry(&r, 5).retirement_announced,
        "the incident was not announced"
    );

    // While that hold is fresh the timeline is not walked again: the
    // listing is wrong and stays wrong, and the walk is the expensive
    // part. The item itself is still read every pass, so a close is
    // still noticed at once.
    stub.hits();
    e.tick_repo(&r).await.unwrap();
    let paths: Vec<String> = stub.hits();
    assert!(
        !paths.iter().any(|p| p.contains("/timeline")),
        "the timeline was walked inside the hold: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p == "/repos/o/r/issues/5"),
        "the item itself was not read inside the hold: {paths:?}"
    );

    // Once the interval has passed the item is read again. This time
    // the mention is in a review comment, one level down in the
    // timeline the way GitHub reports a batch of them.
    e.entry(&r, 5).retirement_held_at = Some(EXPIRED.into());
    stub.set_issue(5, item("nothing to see"));
    stub.set_timeline(
        5,
        vec![json!({
            "event": "line-commented",
            "comments": [{"body": "@bot what do you think?", "user": {"login": "alice"}}]
        })],
    );
    e.tick_repo(&r).await.unwrap();
    assert!(
        e.entry(&r, 5).active,
        "retired although a review comment still mentions the bot"
    );

    // A near miss is not a mention, so this one does retire.
    e.entry(&r, 5).retirement_held_at = Some(EXPIRED.into());
    stub.set_timeline(5, vec![comment("ask @bot-2, not this one")]);
    e.tick_repo(&r).await.unwrap();
    assert!(
        !e.entry(&r, 5).active,
        "kept although nothing mentions the bot any more"
    );
    assert!(
        e.entry(&r, 5).retirement_held_at.is_none(),
        "the hold outlived the retirement"
    );
    assert!(
        !e.entry(&r, 5).retirement_announced,
        "the next incident would announce itself as an old one"
    );
}
#[tokio::test]
async fn a_hold_on_the_item_itself_leaves_no_pacing_behind() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let r = repo();
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.repos = vec![r.clone()];
    seeded(&mut e, 5, Some("bot/issue-5"), true);
    {
        let st = e.entry(&r, 5);
        st.triggers = vec!["assigned".into(), "mentioned".into()];
        st.worktree_id = Some("w5".into());
        st.worktree_path = Some("/w/5".into());
        st.terminal_handle = Some("t5".into());
        // A hold left by an earlier pass.
        st.retirement_held_at = Some(now_iso());
    }
    d.seed("w5", "t5", READY_SCREEN);
    // Still assigned, so the item itself answers and no walk is paced.
    stub.set_issue(
        5,
        json!({
            "number": 5, "title": "t", "body": "no mention here",
            "html_url": "https://gh/5", "state": "open", "user": {"login": "alice"},
            "assignees": [{"login": "bot"}],
            "created_at": "x", "updated_at": "u1"
        }),
    );
    stub.set_timeline(5, vec![]);
    e.tick_repo(&r).await.unwrap();
    assert!(e.entry(&r, 5).active, "an assigned item was retired");
    assert!(
        e.entry(&r, 5).retirement_held_at.is_none(),
        "an assignment left a stamp pacing a walk it never made"
    );

    // So the moment the assignment goes, the mention is re-checked at
    // once rather than waiting out a stamp it never earned.
    stub.set_issue(
        5,
        json!({
            "number": 5, "title": "t", "body": "no mention here",
            "html_url": "https://gh/5", "state": "open", "user": {"login": "alice"},
            "created_at": "x", "updated_at": "u1"
        }),
    );
    stub.hits();
    e.tick_repo(&r).await.unwrap();
    let paths = stub.hits();
    assert!(
        paths.iter().any(|p| p.contains("/issues/5/timeline")),
        "the mention was not re-checked once the assignment went: {paths:?}"
    );
    assert!(
        !e.entry(&r, 5).active,
        "nothing named the bot, so it retires"
    );
}
#[tokio::test]
async fn a_hold_paces_the_re_read_without_delaying_a_close_or_outliving_the_clock() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let r = repo();
    let item = |state: &str| {
        json!({
            "number": 5, "title": "t", "body": "no mention here",
            "html_url": "https://gh/5", "state": state, "user": {"login": "alice"},
            "created_at": "x", "updated_at": "u1"
        })
    };
    let held_session = |e: &mut Engine, at: &str| {
        seeded(e, 5, Some("bot/issue-5"), true);
        let st = e.entry(&repo(), 5);
        st.triggers = vec!["mentioned".into()];
        st.worktree_id = Some("w5".into());
        st.worktree_path = Some("/w/5".into());
        st.terminal_handle = Some("t5".into());
        st.retirement_held_at = Some(at.to_string());
    };

    // A closed item retires on the next pass, hold or no hold: the
    // hold only ever paces the mention re-check, which a closed item
    // never reaches.
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.repos = vec![r.clone()];
    held_session(&mut e, &now_iso());
    d.seed("w5", "t5", READY_SCREEN);
    stub.set_issue(5, item("closed"));
    stub.set_timeline(5, vec![]);
    e.tick_repo(&r).await.unwrap();
    assert!(
        !e.entry(&r, 5).active,
        "a closed item waited for the hold to expire"
    );

    // An expired hold reads the item again and retires it.
    let mut e = engine_at(&stub.base);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.repos = vec![r.clone()];
    held_session(&mut e, "2026-01-01T00:00:00Z");
    stub.set_issue(5, item("open"));
    e.tick_repo(&r).await.unwrap();
    assert!(!e.entry(&r, 5).active, "an expired hold went on holding");

    // So does one stamped ahead of the clock, which would otherwise
    // read as fresh until the clock caught up with it.
    let mut e = engine_at(&stub.base);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.repos = vec![r.clone()];
    held_session(&mut e, "2099-01-01T00:00:00Z");
    e.tick_repo(&r).await.unwrap();
    assert!(
        !e.entry(&r, 5).active,
        "a hold stamped in the future held forever"
    );
}
#[tokio::test]
async fn an_item_back_on_a_listing_clears_the_hold_it_left_behind() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let r = repo();
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.repos = vec![r.clone()];
    seeded(&mut e, 5, Some("bot/issue-5"), true);
    {
        let st = e.entry(&r, 5);
        st.triggers = vec!["assigned".into()];
        st.worktree_id = Some("w5".into());
        st.worktree_path = Some("/w/5".into());
        st.terminal_handle = Some("t5".into());
        // The same `updated_at` the listing reports, so the pass has
        // no reason to look at the item: the hold must still clear.
        st.updated_at = Some("u2".into());
        st.retirement_held_at = Some(now_iso());
    }
    d.seed("w5", "t5", READY_SCREEN);
    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    stub.set_timeline(5, vec![]);
    e.tick_repo(&r).await.unwrap();
    assert!(
        e.entry(&r, 5).retirement_held_at.is_none(),
        "the hold survived the item coming back onto a listing"
    );
}
#[tokio::test]
async fn a_review_request_still_on_the_pull_request_holds_the_session() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let r = repo();
    stub.set_issue(
        7,
        json!({
            "number": 7, "title": "t", "body": "no mention here",
            "html_url": "https://gh/7", "state": "open", "user": {"login": "alice"},
            "pull_request": {"url": "https://gh/pulls/7"},
            "created_at": "x", "updated_at": "u1"
        }),
    );
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.repos = vec![r.clone()];
    seeded(&mut e, 7, Some("bot/issue-7"), true);
    {
        let st = e.entry(&r, 7);
        st.triggers = vec!["review_requested".into()];
        st.worktree_id = Some("w7".into());
        st.worktree_path = Some("/w/7".into());
        st.terminal_handle = Some("t7".into());
    }
    d.seed("w7", "t7", READY_SCREEN);

    // The pull request was never registered, so fetching it fails.
    // Nothing is known either way, so the session is kept.
    e.tick_repo(&r).await.unwrap();
    assert!(
        e.entry(&r, 7).active,
        "retired on a re-check that could not be made"
    );

    // It still asks the bot for a review: kept, and for a good reason.
    e.entry(&r, 7).retirement_held_at = Some(EXPIRED.into());
    stub.set_pull(
        7,
        json!({
            "head": {"ref": "b", "repo": {"full_name": "o/r"}},
            "base": {"ref": "main"},
            "requested_reviewers": [{"login": "Bot"}]
        }),
    );
    e.tick_repo(&r).await.unwrap();
    assert!(e.entry(&r, 7).active, "retired although the review stands");

    // The request has been withdrawn: now it retires.
    e.entry(&r, 7).retirement_held_at = Some(EXPIRED.into());
    stub.set_pull(
        7,
        json!({
            "head": {"ref": "b", "repo": {"full_name": "o/r"}},
            "base": {"ref": "main"},
            "requested_reviewers": []
        }),
    );
    e.tick_repo(&r).await.unwrap();
    assert!(
        !e.entry(&r, 7).active,
        "kept although the review request is gone"
    );
}
#[test]
fn the_release_refusal_follows_the_trigger_that_holds_the_item() {
    let t = |s: &str| vec![s.to_string()];
    assert_eq!(why_active(&t("assigned")).1, "Close or unassign the item");
    assert!(why_active(&t("mentioned")).0.contains("mentions the bot"));
    assert_eq!(why_active(&t("mentioned")).1, "Close the item");
    assert!(why_active(&t("review_requested")).0.contains("review"));
    assert!(why_active(&t("created")).0.contains("opened by the bot"));
    // An assignment is the clearest thing to act on, so it wins.
    assert_eq!(
        why_active(&["mentioned".to_string(), "assigned".to_string()]).1,
        "Close or unassign the item"
    );
    assert_eq!(why_active(&[]).1, "Close the item");
}
#[tokio::test]
async fn conflict_simulation_reports_paths_without_touching_the_worktree() {
    use crate::release::testkit::{scratch, sh};

    let s = scratch("conflict-merge-tree").await;
    let (path, branch) = crate::driver::add_local_worktree(&s.work, "issue-1-conflict", None)
        .await
        .unwrap();
    std::fs::write(std::path::Path::new(&path).join("a.txt"), "feature\n").unwrap();
    sh(&path, &["add", "a.txt"]).await;
    sh(&path, &["commit", "-q", "-m", "feature"]).await;
    std::fs::write(std::path::Path::new(&s.work).join("a.txt"), "base\n").unwrap();
    sh(&s.work, &["add", "a.txt"]).await;
    sh(&s.work, &["commit", "-q", "-m", "base"]).await;
    sh(&s.work, &["push", "-q", "origin", "main"]).await;
    let base = conflict_git(&s.work, &["rev-parse", "refs/remotes/origin/main"])
        .await
        .unwrap();
    let head = conflict_git(&path, &["rev-parse", "HEAD"]).await.unwrap();
    let before = std::fs::read(std::path::Path::new(&path).join("a.txt")).unwrap();
    let (conflict, files) = engine()
        .simulate_conflict(&s.work, &base, &head)
        .await
        .unwrap();
    assert!(conflict);
    assert_eq!(files, vec!["a.txt"]);
    assert_eq!(
        std::fs::read(std::path::Path::new(&path).join("a.txt")).unwrap(),
        before,
        "merge-tree must not change the agent worktree"
    );
    assert_eq!(branch, "refs/heads/bot/issue-1-conflict");

    // A modify/delete conflict has no "Merge conflict in" prose, so the
    // NUL-delimited name section is the source of truth for it too.
    let s = scratch("conflict-modify-delete").await;
    let (path, _) = crate::driver::add_local_worktree(&s.work, "issue-4-conflict", None)
        .await
        .unwrap();
    std::fs::write(std::path::Path::new(&path).join("a.txt"), "feature\n").unwrap();
    sh(&path, &["add", "a.txt"]).await;
    sh(&path, &["commit", "-q", "-m", "feature"]).await;
    sh(&s.work, &["rm", "-q", "a.txt"]).await;
    sh(&s.work, &["commit", "-q", "-m", "delete"]).await;
    sh(&s.work, &["push", "-q", "origin", "main"]).await;
    let base = conflict_git(&s.work, &["rev-parse", "refs/remotes/origin/main"])
        .await
        .unwrap();
    let head = conflict_git(&path, &["rev-parse", "HEAD"]).await.unwrap();
    let (conflict, files) = engine()
        .simulate_conflict(&s.work, &base, &head)
        .await
        .unwrap();
    assert!(conflict);
    assert_eq!(files, vec!["a.txt"]);
}
