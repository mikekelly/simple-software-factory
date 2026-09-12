use super::*;

#[tokio::test]

async fn onboarding_posts_one_attached_event_and_no_more_after_that() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let mut r = repo();
    r.model = Some("fable-5.1".into());
    r.effort = Some("high".into());
    e.cfg.repos = vec![r.clone()];
    stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
    stub.set_timeline(5, vec![assigned_by(1, "alice")]);
    e.tick_repo(&r).await.unwrap();
    let st = e.entry(&r, 5).clone();
    assert!(st.seeded && st.active, "{st:?}");
    assert!(e.failures.is_empty(), "{:?}", e.failures);
    let log = d.log();
    assert!(
        log[0].starts_with("start:stub::/stub.worktrees/issue-5-t:"),
        "{log:?}"
    );
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].0, "/repos/o/r/issues/5/comments");
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#5 event=attached -->\n\n\
             ```ssf\n\
             ssf attaching agent to issue:\n\
             harness: Claude Code\n\
             model: fable-5.1\n\
             effort: high\n\
             driver: orca\n\
             branch: bot/issue-5-t\n\
             ```"
    );
    // The post is on the item's timeline now; the next pass neither
    // delivers it to the agent nor posts again.
    let event_post = comment(2, "bot", &posts[0].1);
    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    stub.set_timeline(5, vec![assigned_by(1, "alice"), event_post.clone()]);
    e.tick_repo(&r).await.unwrap();
    assert!(d.log().is_empty(), "nothing to deliver");
    assert!(stub.posts().is_empty());
    let st = e.entry(&r, 5).clone();
    assert_eq!(st.updated_at.as_deref(), Some("u2"));
    assert!(st.seen.contains_key("commented:2"), "{:?}", st.seen.keys());
    assert!(
        st.untagged.is_empty(),
        "not a person's post: {:?}",
        st.untagged
    );
    assert!(
        st.origins.is_empty(),
        "not a session's post: {:?}",
        st.origins
    );
    assert_eq!(
        crate::status::sessions(&e.cfg, &e.state, None)[0].untagged_posts,
        0
    );
    // A daemon restart: the state file survives, memory does not, and
    // an unchanged item is not attached again.
    let dir = std::env::temp_dir().join(format!("ssf-engine-events-{}", std::process::id()));
    let path = dir.join("state.json");
    e.state.save_to(&path).unwrap();
    let mut e = engine_at(&stub.base);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.repos = vec![r.clone()];
    e.state = State::load_from(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    e.tick_repo(&r).await.unwrap();
    assert!(d.log().is_empty(), "{:?}", d.log());
    assert!(stub.posts().is_empty());
}
#[tokio::test]
async fn binding_to_an_owning_session_posts_attached_on_the_bound_item() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    // Session 1 is live on w1; item 7 was opened by it.
    seeded(&mut e, 1, Some("bot/issue-1"), true);
    {
        let st = e.entry(&r, 1);
        st.worktree_id = Some("w1".into());
        st.worktree_path = Some("/w/1".into());
        st.terminal_handle = Some("t1".into());
        st.repo_id = Some("stub".into());
        st.driver = Some("orca".into());
    }
    d.seed("w1", "t1", READY_SCREEN);
    let opened = json!({
        "number": 7, "title": "child", "body": "🤖#1 says: <!-- ssf: origin=o/r#1 -->\n\nfollow-up",
        "html_url": "https://gh/7", "state": "open", "user": {"login": "bot"},
        "assignees": [{"login": "bot"}], "created_at": "x", "updated_at": "u1"
    });
    stub.set_assigned(vec![opened]);
    stub.set_timeline(7, vec![assigned_by(1, "bot")]);
    e.tick_repo(&r).await.unwrap();
    let st = e.entry(&r, 7).clone();
    assert_eq!(st.shares_workspace_of, Some(1), "{st:?}");
    let log = d.log();
    assert!(
        log[0].starts_with("deliver:w1:[ssf] Now tracking issue #7"),
        "{log:?}"
    );
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].0, "/repos/o/r/issues/7/comments");
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#7 event=attached -->\n\n\
             ```ssf\n\
             ssf attaching agent to issue:\n\
             session: o/r#1\n\
             shares: workspace of #1\n\
             ```"
    );
}
#[tokio::test]
async fn event_posts_are_not_delivered_or_fanned_out() {
    let mut e = engine();
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos.push(r.clone());
    seeded(&mut e, 1, Some("bot/issue-1"), true);
    seeded(&mut e, 3, Some("bot/issue-3"), true);
    for n in [1, 3] {
        let st = e.entry(&r, n);
        st.worktree_id = Some(format!("w{n}"));
        st.terminal_handle = Some(format!("t{n}"));
        d.seed(&format!("w{n}"), &format!("t{n}"), READY_SCREEN);
    }
    // Session 1 follows item 3, whose only news is the daemon saying
    // it resumed session 3's harness.
    e.entry(&r, 3).subscribers = vec!["o/r#1".into()];
    let o = Origin::new("o/r", 3).unwrap();
    let post = events::comment(
        &o,
        "issue",
        &Event::Resumed {
            harness: "Claude Code".into(),
            conversation: Conversation::Fresh,
            after: "restart",
        },
    );
    let timeline = vec![comment(9, "bot", &post)];
    let diff = e.diff(&r, &BTreeMap::new(), &timeline);
    assert!(diff.rendered.is_empty(), "{:?}", diff.rendered);
    assert!(diff.seen.contains_key("commented:9"));
    assert!(
        e.for_recipient(&diff.rendered, "o/r#1").is_empty()
            && e.for_recipient(&diff.rendered, "o/r#3").is_empty()
    );
    e.fan_out(
        &r,
        &issue(3, "alice", None),
        &diff.rendered,
        Fyi::Activity,
        false,
        &[],
    )
    .await;
    assert!(d.log().is_empty(), "{:?}", d.log());
}
#[tokio::test]
async fn event_comments_can_be_switched_off() {
    let _sandbox = crate::config::test_support::sandbox();
    // The instance says no: onboarding posts nothing, but happens.
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    e.cfg.daemon.event_comments = false;
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
    stub.set_timeline(5, vec![assigned_by(1, "alice")]);
    e.tick_repo(&r).await.unwrap();
    assert!(e.entry(&r, 5).seeded);
    assert!(d.log()[0].starts_with("start:"));
    assert!(stub.posts().is_empty());

    // The repository says yes over an instance that says no: a
    // blocked session is reported.
    let (mut e, d) = blocked_setup(&stub, LOGIN_SCREEN);
    e.cfg.daemon.event_comments = false;
    e.cfg.repos[0].event_comments = Some(true);
    probe_returning(&mut e, LoginState::SignedOut, None);
    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    stub.set_timeline(5, vec![assigned_by(1, "alice"), comment(2, "alice", "go")]);
    e.tick_repo(&e.cfg.repos[0].clone()).await.unwrap();
    assert!(e.entry(&repo(), 5).blocked.as_ref().unwrap().reported);
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert!(posts[0].1.contains("event=blocked -->"));
    assert!(d.log().is_empty(), "held");

    // The repository says no over an instance that says yes: still
    // blocked and held, nothing posted, and the record still says
    // reported so the pass does not try again.
    let (mut e, d) = blocked_setup(&stub, LOGIN_SCREEN);
    e.cfg.repos[0].event_comments = Some(false);
    probe_returning(&mut e, LoginState::SignedOut, None);
    e.tick_repo(&e.cfg.repos[0].clone()).await.unwrap();
    let st = e.entry(&repo(), 5).clone();
    assert!(st.blocked.as_ref().unwrap().reported);
    assert!(stub.posts().is_empty());
    assert!(d.log().is_empty(), "held");
    assert_eq!(st.updated_at.as_deref(), Some("u1"));
    let err = e.deliver_to(&repo(), 5, "hello", None).await.unwrap_err();
    assert!(is_blocked(&err), "{err:#}");
}
#[tokio::test]
async fn releasing_or_purging_a_workspace_posts_released() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    let closed = |e: &mut Engine, n: u64| {
        seeded(e, n, Some(&format!("bot/issue-{n}")), false);
        let st = e.entry(&repo(), n);
        st.worktree_id = Some(format!("w{n}"));
        // No such directory: the checks cannot pass, so only a
        // forced removal goes ahead.
        st.worktree_path = Some(format!("/nonexistent/ssf-w{n}"));
        st.github_state = Some("closed".into());
        st.kind = Some("pull_request".into());
        st.retired_at = Some(now_iso());
        d.with(|s| {
            s.worktrees.insert(format!("w{n}"));
        });
    };
    closed(&mut e, 1);
    closed(&mut e, 2);
    // Item 3 shares session 1's workspace: nothing is posted on it.
    seeded(&mut e, 3, None, false);
    e.entry(&r, 3).shares_workspace_of = Some(1);
    e.entry(&r, 3).worktree_id = Some("w1".into());
    e.entry(&r, 3).github_state = Some("closed".into());
    {
        let st = e.entry(&r, 1);
        st.release_pending = true;
        st.release_forced = true;
    }
    let st = e.entry(&r, 1).clone();
    e.finish_release(&r, st).await;
    assert!(e.entry(&r, 1).worktree_id.is_none());
    assert!(e.entry(&r, 3).worktree_id.is_none());
    assert_eq!(d.log(), vec!["remove:w1"]);
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].0, "/repos/o/r/issues/1/comments");
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#1 event=released -->\n\n\
             ```ssf\n\
             ssf releasing workspace of pull request:\n\
             by: ssf release\n\
             forced: yes\n\
             branch: bot/issue-1\n\
             ```"
    );
    // Already gone: marked released, nothing said.
    {
        let st = e.entry(&r, 1);
        st.worktree_id = Some("w1".into());
        st.release_pending = true;
    }
    let st = e.entry(&r, 1).clone();
    e.finish_release(&r, st).await;
    assert!(e.entry(&r, 1).released_at.is_some());
    assert!(stub.posts().is_empty());
    // Purge, forced since the checks cannot run.
    let out = e.purge(false, None, true).await.unwrap();
    assert_eq!(out["workspaces"][0]["removed"], true, "{out}");
    assert_eq!(d.log(), vec!["remove:w2"]);
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].0, "/repos/o/r/issues/2/comments");
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#2 event=released -->\n\n\
             ```ssf\n\
             ssf releasing workspace of pull request:\n\
             by: ssf purge\n\
             forced: yes\n\
             branch: bot/issue-2\n\
             ```"
    );
    // A dry run removes nothing and says nothing.
    closed(&mut e, 4);
    e.purge(true, None, true).await.unwrap();
    assert!(d.log().is_empty());
    assert!(stub.posts().is_empty());
}
#[tokio::test]
async fn giving_up_on_a_binding_posts_gave_up_once() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    seeded(&mut e, 5, Some("bot/issue-5"), true);
    e.entry(&r, 5).terminal_handle = Some("t5".into());
    let err = anyhow::anyhow!("no such terminal\n  (it was closed)").context("delivering to orca");
    for n in 1..MAX_DELIVERY_FAILURES {
        e.note_failure(&r, 5, &err).await;
        assert_eq!(e.failures[&("o/r".to_string(), 5)], n);
        assert!(e.entry(&r, 5).seeded);
    }
    assert!(stub.posts().is_empty());
    e.note_failure(&r, 5, &err).await;
    let st = e.entry(&r, 5).clone();
    assert!(!st.seeded && st.terminal_handle.is_none(), "{st:?}");
    assert_eq!(e.failures[&("o/r".to_string(), 5)], 0);
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#5 event=gave-up -->\n\n\
             ```ssf\n\
             ssf giving up on agent binding for issue:\n\
             failures: 5\n\
             last error: delivering to orca: no such terminal (it was closed)\n\
             next: re-onboarding the item\n\
             ```"
    );
    // Five more without an onboarding in between: the count resets
    // again, but there is no binding to drop and nothing to say.
    for _ in 0..MAX_DELIVERY_FAILURES {
        e.note_failure(&r, 5, &err).await;
    }
    assert_eq!(e.failures[&("o/r".to_string(), 5)], 0);
    assert!(!e.entry(&r, 5).seeded);
    assert!(stub.posts().is_empty(), "no binding, no post");
    // An item that never got a session (its onboarding fails every
    // look) is counted and reset the same way, and never told.
    e.entry(&r, 9).title = "never onboarded".into();
    for n in 1..=MAX_DELIVERY_FAILURES {
        e.note_failure(&r, 9, &err).await;
        assert_eq!(
            e.failures[&("o/r".to_string(), 9)],
            n % MAX_DELIVERY_FAILURES
        );
    }
    assert!(stub.posts().is_empty());
    // A sign-in phrase in an error is cut out; the rest is kept.
    assert_eq!(
        safe_error("orca: the screen said: Login expired · Please run /login"),
        "orca: the screen said: […] · Please […]"
    );
    assert_eq!(
        safe_error("gh: You are not logged into any GitHub hosts. Run gh auth login."),
        "gh: You are […]to any GitHub hosts. Run gh auth login."
    );
    assert_eq!(safe_error("plain failure"), "plain failure");
    assert!(!crate::driver::quotes_login_prompt(&safe_error(
        "NOT LOGGED IN\nInvalid API key\nSign in with ChatGPT"
    )));
    // Every line of what ssf is about to write down counts, however
    // long the text is: a phrase on the second line of a screen dump
    // is out of what a screen check reads, but the same dump collapsed
    // onto the one line that is posted puts it right there.
    let mut dump =
        String::from("delivery failed; the screen showed:\nLogin expired · Please run /login\n");
    for i in 0..25 {
        dump.push_str(&format!("│ line {i} of the transcript\n"));
    }
    assert!(
        crate::driver::quotes_login_prompt(&dump),
        "found wherever it stands"
    );
    assert_eq!(
        crate::driver::login_dialog("claude", &dump),
        None,
        "a screen is judged by its bottom"
    );
    let posted = safe_error(&events::one_line(&dump));
    assert!(
        posted.starts_with("delivery failed; the screen showed: […] · Please […] │ line 0"),
        "{posted}"
    );
    assert!(!crate::driver::quotes_login_prompt(&posted));
    assert!(!posted.contains("run /login"));
}
#[tokio::test]
async fn a_given_up_owner_relaunched_for_a_dependent_posts_resumed() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    seeded(&mut e, 5, Some("bot/issue-5"), true);
    {
        let st = e.entry(&r, 5);
        st.seeded = false;
        st.worktree_id = Some("w5".into());
        st.worktree_path = Some("/w/5".into());
        st.repo_id = Some("stub".into());
        st.driver = Some("orca".into());
    }
    seeded(&mut e, 8, None, true);
    {
        let st = e.entry(&r, 8);
        st.kind = Some("pull_request".into());
        st.shares_workspace_of = Some(5);
    }
    d.with(|s| {
        s.worktrees.insert("w5".into());
        s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect();
    });
    e.deliver_to(&r, 8, "[ssf] a comment on the PR", None)
        .await
        .unwrap();
    assert_eq!(d.log()[0], "relaunch:w5:false");
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].0, "/repos/o/r/issues/5/comments");
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#5 event=resumed -->\n\n\
             ```ssf\n\
             ssf resuming agent on issue:\n\
             harness: Claude Code\n\
             conversation: fresh\n\
             after: lost terminal\n\
             ```"
    );
    assert!(e.onboarding.is_none());
}
#[tokio::test]
async fn onboarding_onto_a_kept_workspace_posts_attached_again() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    // As `note_failure` leaves an item after giving up: workspace
    // remembered, binding dropped; the agent in it is gone too.
    seeded(&mut e, 5, Some("bot/issue-5-t"), true);
    {
        let st = e.entry(&r, 5);
        st.seeded = false;
        st.worktree_id = Some("w5".into());
        st.worktree_path = Some("/w/5".into());
        st.worktree_name = Some("issue-5-t".into());
        st.repo_id = Some("stub".into());
        st.driver = Some("orca".into());
    }
    d.with(|s| {
        s.worktrees.insert("w5".into());
        s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect();
    });
    stub.set_assigned(vec![assigned_item(5, "alice", "u1")]);
    stub.set_timeline(5, vec![assigned_by(1, "alice")]);
    e.tick_repo(&r).await.unwrap();
    let st = e.entry(&r, 5).clone();
    assert!(st.seeded && st.active, "{st:?}");
    assert_eq!(st.worktree_id.as_deref(), Some("w5"), "kept");
    let log = d.log();
    assert_eq!(log[0], "relaunch:w5:false", "{log:?}");
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#5 event=attached -->\n\n\
             ```ssf\n\
             ssf attaching agent to issue again:\n\
             harness: Claude Code\n\
             model: the harness's default\n\
             effort: the harness's default\n\
             driver: orca\n\
             branch: bot/issue-5-t\n\
             workspace: kept\n\
             conversation: fresh\n\
             ```"
    );
    // With the agent still there: attached again, conversation kept.
    e.entry(&r, 5).seeded = false;
    stub.set_assigned(vec![assigned_item(5, "alice", "u2")]);
    e.tick_repo(&r).await.unwrap();
    assert!(d.log()[0].starts_with("deliver:w5:"));
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert!(
        posts[0]
            .1
            .ends_with("workspace: kept\nconversation: kept\n```"),
        "{}",
        posts[0].1
    );
}
#[tokio::test]
async fn a_gone_workspace_is_re_created_and_the_item_told() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.repos = vec![repo()];
    seeded(&mut e, 5, Some("bot/issue-5-fix-the-widget"), true);
    {
        let st = e.entry(&repo(), 5);
        st.title = "Fix the widget".into();
        st.html_url = "https://gh/5".into();
        st.worktree_name = Some("issue-5-fix-the-widget".into());
        st.repo_id = Some("stub".into());
        st.driver = Some("orca".into());
        st.worktree_id = Some("stub::/stub.worktrees/issue-5-fix-the-widget".into());
        st.worktree_path = Some("/stub.worktrees/issue-5-fix-the-widget".into());
        st.agent_session_id = Some("sess-5".into());
    }
    // The stub driver has no such workspace: it is re-created.
    let delivered = e.deliver_to(&repo(), 5, "hello", None).await.unwrap();
    assert!(delivered.relaunched);
    assert_eq!(
        d.log()[0],
        "relaunch:stub::/stub.worktrees/issue-5-fix-the-widget:true"
    );
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#5 event=attached -->\n\n\
             ```ssf\n\
             ssf attaching agent to issue again:\n\
             harness: Claude Code\n\
             model: the harness's default\n\
             effort: the harness's default\n\
             driver: orca\n\
             branch: bot/issue-5-fix-the-widget\n\
             re-created: workspace gone\n\
             conversation: resumed\n\
             ```"
    );
}
#[tokio::test]
async fn a_bound_pull_request_is_attached_as_one() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
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
    // As `onboard` has it before binding: kind and PR details known.
    {
        let st = e.entry(&r, 8);
        st.kind = Some("pull_request".into());
        st.pr = Some(pr("bot/issue-1"));
    }
    let mut item = issue(8, "bot", Some("fixes #1"));
    item.pull_request = Some(json!({}));
    let diff = e.diff(&r, &BTreeMap::new(), &[]);
    e.bind_to(&r, &item, 1, diff, vec!["created".into()], None)
        .await
        .unwrap();
    assert_eq!(e.entry(&r, 8).shares_workspace_of, Some(1));
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#8 event=attached -->\n\n\
             ```ssf\n\
             ssf attaching agent to pull request:\n\
             session: o/r#1\n\
             shares: workspace of #1\n\
             ```"
    );
}
#[tokio::test]
async fn a_delegated_item_is_attached_with_its_parent_named() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    seeded(&mut e, 1, Some("bot/issue-1"), true);
    let opened = json!({
        "number": 7, "title": "child", "body": "🤖#1 says: <!-- ssf: origin=o/r#1 mode=delegate -->\n\nover to you",
        "html_url": "https://gh/7", "state": "open", "user": {"login": "bot"},
        "assignees": [{"login": "bot"}], "created_at": "x", "updated_at": "u1"
    });
    stub.set_assigned(vec![opened]);
    stub.set_timeline(7, vec![assigned_by(1, "bot")]);
    e.tick_repo(&r).await.unwrap();
    let st = e.entry(&r, 7).clone();
    assert_eq!(st.delegated_by.as_deref(), Some("o/r#1"), "{st:?}");
    assert!(st.shares_workspace_of.is_none(), "a session of its own");
    assert!(d.log()[0].starts_with("start:stub::/stub.worktrees/issue-7-child:"));
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].0, "/repos/o/r/issues/7/comments");
    assert!(
        posts[0]
            .1
            .ends_with("driver: orca\nbranch: bot/issue-7-child\nhanded off from: o/r#1\n```"),
        "{}",
        posts[0].1
    );
}
#[tokio::test]
async fn a_resumed_agent_at_work_is_settled_and_not_doubled() {
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
    d.with(|s| {
        s.live.clear();
        s.resume = crate::driver::StubResume::Settles;
        s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect();
    });
    let delivered = e.deliver_to(&repo(), 5, "[ssf] hello", None).await.unwrap();
    assert!(delivered.relaunched && delivered.resumed);
    assert_eq!(d.log(), vec!["relaunch:w5:true", "deliver:w5:[ssf] hello"]);
    let launches = d.launches();
    assert_eq!(launches.len(), 1, "one launch, the resume: {launches:?}");
    assert!(launches[0].contains("--resume sess-5"), "{launches:?}");
    let st = e.entry(&repo(), 5).clone();
    assert_eq!(st.agent_session_id.as_deref(), Some("sess-5"), "kept");
    assert_eq!(st.terminal_handle.as_deref(), Some("t1"));
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].1, resumed_block("resumed"));
}
#[tokio::test]
async fn a_resume_whose_harness_exits_is_followed_by_one_fresh_harness() {
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
    d.with(|s| {
        s.live.clear();
        s.resume = crate::driver::StubResume::Exits;
        s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect();
    });
    let delivered = e.deliver_to(&repo(), 5, "[ssf] hello", None).await.unwrap();
    assert!(delivered.relaunched && !delivered.resumed);
    let log = d.log();
    assert_eq!(log[0], "resume-exited:w5");
    assert_eq!(log[1], "relaunch:w5:false");
    let launches = d.launches();
    assert_eq!(
        launches.len(),
        2,
        "the resume, then the fresh start: {launches:?}"
    );
    assert!(launches[0].contains("--resume sess-5"), "{launches:?}");
    assert!(!launches[1].contains("--resume"), "{launches:?}");
    let st = e.entry(&repo(), 5).clone();
    assert_eq!(st.agent_session_id, None, "a fresh conversation");
    assert_eq!(st.terminal_handle.as_deref(), Some("t1"));
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].1, resumed_block("fresh"));
}
#[tokio::test]
async fn a_resume_alive_past_the_wait_is_kept_not_replaced() {
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
    d.with(|s| {
        s.live.clear();
        s.resume = crate::driver::StubResume::Unsettled;
        s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect();
    });
    let delivered = e.deliver_to(&repo(), 5, "[ssf] hello", None).await.unwrap();
    assert!(delivered.relaunched && delivered.resumed);
    assert_eq!(
        d.log(),
        vec![
            "resume-unsettled:w5",
            "relaunch:w5:true",
            "deliver:w5:[ssf] hello"
        ]
    );
    assert_eq!(d.launches().len(), 1, "no second launch");
    let st = e.entry(&repo(), 5).clone();
    assert_eq!(st.terminal_handle.as_deref(), Some("t1"));
    assert_eq!(st.agent_session_id.as_deref(), Some("sess-5"), "kept");
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].1, resumed_block("resumed"));
}
#[tokio::test]
async fn a_relaunch_posts_resumed_with_why() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = blocked_setup(&stub, READY_SCREEN);
    // The terminal is gone; a delivery starts the harness again.
    d.with(|s| {
        s.live.clear();
        s.relaunch_screen = READY_SCREEN.iter().map(|l| l.to_string()).collect();
    });
    e.deliver_to(&repo(), 5, "[ssf] hello", None).await.unwrap();
    assert_eq!(d.log()[0], "relaunch:w5:true");
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#5 event=resumed -->\n\n\
             ```ssf\n\
             ssf resuming agent on issue:\n\
             harness: Claude Code\n\
             conversation: resumed\n\
             after: lost terminal\n\
             ```"
    );
    // A live agent: a delivery, no relaunch, no post.
    e.deliver_to(&repo(), 5, "[ssf] again", None).await.unwrap();
    assert_eq!(d.log(), vec!["deliver:w5:[ssf] again"]);
    assert!(stub.posts().is_empty());
    // Gone again over a restart: the startup pass brings it back,
    // fresh this time (no session id captured).
    d.with(|s| s.live.clear());
    e.entry(&repo(), 5).agent_session_id = None;
    stub.set_collaborators(Some(vec![]));
    e.resume_interrupted(&[DriverKind::Orca]).await;
    assert!(!e.startup_pass);
    let log = d.log();
    assert_eq!(log[0], "relaunch:w5:false", "{log:?}");
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert!(
            posts[0].1.ends_with(
                "ssf resuming agent on issue:\nharness: Claude Code\nconversation: fresh\nafter: restart\n```"
            ),
            "{}",
            posts[0].1
        );
}
