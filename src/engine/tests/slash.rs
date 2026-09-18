//! `/ssf <request>`: taking the command from an item's comments, running it
//! as a task, and what the item is told.

use super::*;
use crate::slash;

/// A timeline with the comments a test wants, in the shape GitHub sends.
fn timeline(comments: &[(u64, &str, &str)]) -> Vec<Value> {
    comments
        .iter()
        .map(|(id, who, body)| comment(*id, who, body))
        .collect()
}

fn pending(e: &Engine, r: &RepoConfig, number: u64) -> Vec<slash::Command> {
    e.state.repos[&r.name].issues[&number].slash_pending.clone()
}

#[test]
fn a_command_is_taken_once_queued_in_order_and_only_from_people_who_may_ask() {
    let _sandbox = crate::config::test_support::sandbox();
    let mut e = engine();
    let r = repo();
    e.cfg.repos = vec![r.clone()];

    let events = timeline(&[
        (1, "ann", "morning, all"),
        (2, "ann", "/ssf please assign this to claude fable low"),
        (3, "bot", "/ssf do as I say"),
        (4, "bob", "/ssf\n"),
        (
            5,
            "bob",
            "  /ssf look at the failing test  \nand the rest is prose",
        ),
        (6, "bob", "I quoted a command:\n/ssf not this one"),
    ]);
    e.take_commands(&r, 7, &events);

    // Anyone may drive in this engine, so what is left out is the bot's own
    // comment and the requests that are not requests.
    assert_eq!(
        pending(&e, &r, 7),
        vec![
            slash::Command {
                id: 2,
                author: "ann".into(),
                text: "please assign this to claude fable low".into(),
            },
            slash::Command {
                id: 5,
                author: "bob".into(),
                text: "look at the failing test".into(),
            },
        ],
        "queued oldest first, and only what a person addressed to ssf"
    );
    assert_eq!(
        e.state.repos[&r.name].issues[&7].slash_done.len(),
        3,
        "every comment that carried a command is remembered, so no pass asks twice"
    );

    // Walking the same timeline again (a relaunch, a story, a later pass)
    // changes nothing.
    e.take_commands(&r, 7, &events);
    assert_eq!(pending(&e, &r, 7).len(), 2);
}

#[test]
fn the_bot_and_strangers_cannot_ask_for_a_task() {
    let _sandbox = crate::config::test_support::sandbox();
    let mut e = engine();
    let mut r = repo();
    // An explicit list, so the collaborator default is out of the picture.
    r.allowed_users = Some(vec!["ann".into()]);
    e.cfg.repos = vec![r.clone()];

    e.take_commands(
        &r,
        7,
        &timeline(&[
            (1, "ann", "/ssf do it"),
            (2, "stranger", "/ssf do it too"),
            (3, "bot", "/ssf and again"),
        ]),
    );
    assert_eq!(
        pending(&e, &r, 7)
            .iter()
            .map(|c| c.author.as_str())
            .collect::<Vec<_>>(),
        vec!["ann"],
        "only a login that may drive the repository"
    );
}

#[test]
fn a_repository_can_turn_the_command_off() {
    let _sandbox = crate::config::test_support::sandbox();
    let mut e = engine();
    let mut r = repo();
    r.slash_commands = Some(false);
    e.cfg.repos = vec![r.clone()];
    e.take_commands(&r, 7, &timeline(&[(1, "ann", "/ssf do it")]));
    assert!(
        !e.state.repos.contains_key(&r.name),
        "switched off, a command is just a comment: nothing is queued or even remembered"
    );
    // The instance default, when the repository says nothing.
    let mut r2 = repo();
    r2.slash_commands = None;
    e.cfg.daemon.slash_commands = false;
    e.cfg.repos = vec![r2.clone()];
    e.take_commands(&r2, 7, &timeline(&[(1, "ann", "/ssf do it")]));
    assert!(!e.state.repos.contains_key(&r2.name));
}

#[tokio::test(flavor = "current_thread")]
async fn a_request_on_an_item_runs_as_a_task_and_the_item_hears_both_ends() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let mut r = repo();
    r.harness = "claude".into();
    e.cfg.repos = vec![r.clone()];
    let item = assigned_item(7, "ann", "u1");
    stub.set_assigned(vec![item.clone()]);
    stub.set_issue(7, item.clone());
    stub.set_timeline(7, timeline(&[(2, "ann", "/ssf tell me about this item")]));
    d.with(|s| {
        s.worktrees.insert("w7".into());
    });
    {
        let st = e.entry(&r, 7);
        st.seeded = true;
        st.active = true;
        st.worktree_id = Some("w7".into());
        st.worktree_path = Some("/w/w7".into());
        st.kind = Some("issue".into());
        st.updated_at = Some("later".into());
    }
    stub.post_bodies();
    // A pass reloads the config from disk, so what this test configured has
    // to be there for it to load.
    e.cfg.save().unwrap();

    e.tick().await;

    let rs = &e.state.repos[&r.name];
    assert!(
        rs.issues[&7].slash_pending.is_empty(),
        "a request that started is no longer waiting"
    );
    let running = rs.issues[&7]
        .slash_running
        .as_ref()
        .expect("the item remembers the task on it");
    assert_eq!(running.harness, "claude");
    assert_eq!(running.command.text, "tell me about this item");
    assert_eq!(e.tasks.count(), 1);

    let posts = stub.post_bodies();
    let started = posts
        .iter()
        .find(|(_, body)| body.contains("event=task-started"))
        .map(|(_, body)| body.clone())
        .unwrap_or_else(|| panic!("no task-started post: {posts:?}"));
    assert!(
        started.contains("ssf running a task on issue:"),
        "{started}"
    );
    assert!(started.contains("harness: Claude Code"), "{started}");
    assert!(started.contains("asked by: ann"), "{started}");
    assert!(
        started.contains("request: tell me about this item"),
        "{started}"
    );
    // Let the task finish (its wrapper cannot run in a test), then the end
    // is posted with the status and where the run's own output is.
    wait_for_task(&mut e).await;
    let posts = stub.post_bodies();
    let ended = posts
        .iter()
        .find(|(_, body)| body.contains("event=task-ended"))
        .map(|(_, body)| body.clone())
        .unwrap_or_else(|| panic!("no task-ended post: {posts:?}"));
    assert!(ended.contains("ssf task on issue finished:"), "{ended}");
    assert!(ended.contains("exit: "), "{ended}");
    assert!(ended.contains("log: "), "{ended}");
    assert!(
        e.state.repos[&r.name].issues[&7].slash_running.is_none(),
        "the item is no longer running a task"
    );
    assert_eq!(e.tasks.count(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn a_request_a_harness_cannot_run_headless_is_refused_on_the_item() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let mut r = repo();
    // A harness ssf knows how to run in a terminal, and not otherwise.
    r.harness = "aider".into();
    e.cfg.repos = vec![r.clone()];
    let item = assigned_item(7, "ann", "u1");
    stub.set_assigned(vec![item.clone()]);
    stub.set_issue(7, item.clone());
    stub.set_timeline(7, timeline(&[(2, "ann", "/ssf do the thing")]));
    {
        let st = e.entry(&r, 7);
        st.seeded = true;
        st.active = true;
        st.kind = Some("issue".into());
        st.updated_at = Some("later".into());
    }
    stub.post_bodies();
    // A pass reloads the config from disk, so what this test configured has
    // to be there for it to load.
    e.cfg.save().unwrap();

    e.tick().await;

    let rs = &e.state.repos[&r.name];
    assert!(
        rs.issues[&7].slash_pending.is_empty(),
        "a request nothing can carry out is not left waiting for ever"
    );
    assert!(rs.issues[&7].slash_running.is_none());
    assert_eq!(e.tasks.count(), 0);
    let posts = stub.post_bodies();
    let refused = posts
        .iter()
        .find(|(_, body)| body.contains("event=task-refused"))
        .map(|(_, body)| body.clone())
        .unwrap_or_else(|| panic!("no task-refused post: {posts:?}"));
    assert!(
        refused.contains("ssf not running a task on issue:"),
        "{refused}"
    );
    assert!(refused.contains("asked by: ann"), "{refused}");
    assert!(
        refused.contains("does not know how to run"),
        "the refusal says what could not be done: {refused}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_task_the_daemon_was_running_when_it_stopped_is_reported_on_the_way_back() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    {
        let st = e.entry(&r, 7);
        st.kind = Some("issue".into());
        st.slash_running = Some(slash::Running {
            command: slash::Command {
                id: 2,
                author: "ann".into(),
                text: "do the thing".into(),
            },
            harness: "claude".into(),
        });
    }
    stub.post_bodies();

    e.recover_tasks().await;

    assert!(e.state.repos[&r.name].issues[&7].slash_running.is_none());
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    let (path, body) = &posts[0];
    assert!(path.ends_with("/repos/o/r/issues/7/comments"), "{path}");
    assert!(body.contains("event=task-ended"), "{body}");
    assert!(
        body.contains("exit: the daemon stopped while it ran"),
        "{body}"
    );
    // Nothing is said twice: the record is cleared by the report.
    e.recover_tasks().await;
    assert!(stub.post_bodies().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_record_that_remembers_a_taken_command_is_not_given_up() {
    let _sandbox = crate::config::test_support::sandbox();
    let mut e = engine();
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    // The subscriber that later leaves is a session ssf knows.
    seeded(&mut e, 1, Some("bot/issue-1"), true);

    // An open item, nobody's: the record is the only place that remembers
    // which comments were taken, and its item is walked again (the daemon's
    // own posts move `updated_at`), so it must stay. Both ways a record goes
    // are checked: an unsubscribe that empties the subscriber list, and the
    // sweep at load time.
    let open = |e: &mut Engine, number: u64| {
        let st = e.entry(&r, number);
        st.subscriber_only = true;
        st.subscribers = vec!["o/r#1".into()];
        st.github_state = Some("open".into());
        st.slash_done.insert(2);
    };
    open(&mut e, 20);
    let resp = e
        .handle_request(Request::Unsub {
            from: "o/r#1".into(),
            target: "o/r#20".into(),
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(
        resp.data["untracked"], false,
        "the record remembers a taken comment"
    );
    assert!(e.state.repos["o/r"].issues.contains_key(&20));

    // The same record through the load-time sweep, which is the other way a
    // record with nothing else on it goes: written out and read back.
    e.state.save().unwrap();
    let reloaded = crate::state::State::load().unwrap();
    assert!(
        reloaded.repos["o/r"].issues.contains_key(&20),
        "reading the state back must not drop it either"
    );
    assert!(reloaded.repos["o/r"].issues[&20].slash_done.contains(&2));

    // A closed item is on no listing, so its timeline is never walked again
    // and the record may go as it always did.
    let mut e2 = engine();
    e2.cfg.repos = vec![r.clone()];
    seeded(&mut e2, 1, Some("bot/issue-1"), true);
    let st = e2.entry(&r, 21);
    st.subscriber_only = true;
    st.subscribers = vec!["o/r#1".into()];
    st.github_state = Some("closed".into());
    st.slash_done.insert(2);
    let resp = e2
        .handle_request(Request::Unsub {
            from: "o/r#1".into(),
            target: "o/r#21".into(),
        })
        .await;
    assert!(resp.ok);
    assert_eq!(resp.data["untracked"], true);
    assert!(!e2.state.repos["o/r"].issues.contains_key(&21));

    // And a request still owed a run is never forgotten, open or closed.
    let mut e3 = engine();
    e3.cfg.repos = vec![r.clone()];
    seeded(&mut e3, 1, Some("bot/issue-1"), true);
    let st = e3.entry(&r, 22);
    st.subscriber_only = true;
    st.subscribers = vec!["o/r#1".into()];
    st.github_state = Some("closed".into());
    st.slash_pending = vec![slash::Command {
        id: 3,
        author: "ann".into(),
        text: "do it".into(),
    }];
    let resp = e3
        .handle_request(Request::Unsub {
            from: "o/r#1".into(),
            target: "o/r#22".into(),
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.data["untracked"], false);
    assert!(e3.state.repos["o/r"].issues.contains_key(&22));
}

#[tokio::test(flavor = "current_thread")]
async fn a_request_is_not_taken_again_on_a_later_pass() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    // The bot's own item, nothing acting on it: it is ignored as created-only
    // and gets no session, which is where a request is easiest to run twice:
    // the daemon's `task-started`/`task-ended` posts move the item's
    // `updated_at`, so every pass looks at it again.
    let listed = |updated: &str| {
        json!({
            "number": 18, "title": "t", "body": "work", "html_url": "https://gh/18",
            "state": "open", "user": {"login": "bot"}, "created_at": "x", "updated_at": updated
        })
    };
    *stub.created.lock().unwrap() = vec![listed("x")];
    stub.set_issue(18, listed("x"));
    stub.set_timeline(18, timeline(&[(2, "ann", "/ssf look at this")]));
    e.cfg.save().unwrap();

    e.tick_repo(&r).await.unwrap();
    e.run_tasks(&r).await;
    assert_eq!(e.tasks.count(), 1, "the request ran");
    wait_for_task(&mut e).await;

    // GitHub moves the item's `updated_at` when the daemon posts about it.
    *stub.created.lock().unwrap() = vec![listed("y")];
    stub.set_issue(18, listed("y"));
    stub.bump_created_etag();
    e.tick_repo(&r).await.unwrap();
    assert!(
        pending(&e, &r, 18).is_empty(),
        "a command already taken is not taken again: {:?}",
        pending(&e, &r, 18)
    );
    e.run_tasks(&r).await;
    assert_eq!(e.tasks.count(), 0, "and nothing is started again");
}

/// Collect until the item's task has ended. The task a test starts is a real
/// child process (`sh -c` around a wrapper of this build's own), so this
/// waits on it rather than assuming a time.
async fn wait_for_task(e: &mut Engine) {
    for _ in 0..400 {
        e.collect_tasks().await;
        let running = e
            .state
            .repos
            .values()
            .flat_map(|rs| rs.issues.values())
            .any(|s| s.slash_running.is_some());
        if !running {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the task never ended");
}
