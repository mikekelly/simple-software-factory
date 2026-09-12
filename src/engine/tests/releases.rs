use super::*;

#[tokio::test]
async fn release_is_refused_for_unknown_active_and_non_forced_dependent_sessions() {
    let mut e = engine();
    let r = repo();
    e.cfg.repos.push(r.clone());
    // Not a session ssf knows.
    let resp = e
        .handle_request(Request::Release {
            session: "o/r#1".into(),
            force: false,
        })
        .await;
    assert!(!resp.ok);
    assert!(resp.error.unwrap().contains("not an agent session"));
    // An owner with an open item bound to it keeps its workspace until
    // someone explicitly overrides that protection.
    seeded(&mut e, 3, Some("b3"), false);
    e.entry(&r, 3).worktree_id = Some("repo::/w/3".into());
    seeded(&mut e, 4, Some("b3"), true);
    e.entry(&r, 4).shares_workspace_of = Some(3);
    let resp = e
        .handle_request(Request::Release {
            session: "o/r#4".into(),
            force: false,
        })
        .await;
    assert!(!resp.ok);
    assert!(resp.error.unwrap().contains("#4"));
    assert!(!e.entry(&r, 3).release_pending);
    // --force is still never allowed to remove a live owner's workspace.
    seeded(&mut e, 6, Some("b6"), true);
    e.entry(&r, 6).triggers = vec!["assigned".into()];
    e.entry(&r, 6).worktree_id = Some("repo::/w/6".into());
    let resp = e
        .handle_request(Request::Release {
            session: "o/r#6".into(),
            force: true,
        })
        .await;
    assert!(!resp.ok);
    assert!(resp.error.unwrap().contains("still open and assigned"));
    assert!(!e.entry(&r, 6).release_pending);
    // Nothing to release once it is gone.
    seeded(&mut e, 5, Some("b5"), false);
    e.entry(&r, 5).released_at = Some("t".into());
    let resp = e
        .handle_request(Request::Release {
            session: "o/r#5".into(),
            force: false,
        })
        .await;
    assert!(!resp.ok);
    assert!(resp.error.unwrap().contains("already released"));
}
#[tokio::test]
async fn forced_release_removes_a_workspace_with_open_bound_items() {
    use crate::release::testkit::scratch;

    let clean = scratch("force-bound-release").await;
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    seeded(&mut e, 3, Some("bot/issue-3"), false);
    {
        let st = e.entry(&r, 3);
        st.title = "Original work".into();
        st.html_url = "https://gh/3".into();
        st.worktree_id = Some("w3".into());
        st.worktree_path = Some(clean.work.clone());
    }
    d.seed("w3", "t3", READY_SCREEN);
    seeded(&mut e, 4, Some("bot/issue-3"), true);
    {
        let st = e.entry(&r, 4);
        st.shares_workspace_of = Some(3);
        st.worktree_id = Some("w3".into());
        st.worktree_path = Some(clean.work.clone());
        st.terminal_handle = Some("t3".into());
    }

    // The ordinary request preserves the open follow-up's workspace.
    let refused = e
        .handle_request(Request::Release {
            session: "o/r#4".into(),
            force: false,
        })
        .await;
    assert!(!refused.ok);
    assert!(refused.error.unwrap().contains("#4"));

    // A person who uses --force may release the owning session through
    // the bound item's identity. The response and the daemon pass both
    // record it as forced.
    let accepted = e
        .handle_request(Request::Release {
            session: "o/r#4".into(),
            force: true,
        })
        .await;
    assert!(accepted.ok, "{:?}", accepted.error);
    assert_eq!(accepted.data["session"], "o/r#3");
    assert_eq!(accepted.data["forced"], true);
    assert_eq!(accepted.data["pending"], true);
    assert_eq!(accepted.data["check"]["safe"], true);
    assert!(e.entry(&r, 3).release_pending);
    assert!(e.entry(&r, 3).release_forced);

    e.run_cleanups(&r).await;
    assert_eq!(d.log(), vec!["remove:w3"]);
    for n in [3, 4] {
        let st = e.entry(&r, n).clone();
        assert!(st.worktree_id.is_none(), "#{n}");
        assert!(st.worktree_path.is_none(), "#{n}");
        assert!(st.terminal_handle.is_none(), "#{n}");
        assert!(st.released_at.is_some(), "#{n}");
    }
    assert!(e.entry(&r, 4).active, "the follow-up stays open");
    assert_eq!(e.entry(&r, 4).shares_workspace_of, Some(3));

    // An unchanged poll of the still-open follow-up does not recreate
    // the workspace. It will be rehydrated only when activity needs
    // delivery to the session.
    e.entry(&r, 4).updated_at = Some("x".into());
    *stub.created.lock().unwrap() = vec![json!({
        "number": 4, "title": "Follow-up", "body": "work",
        "html_url": "https://gh/4", "state": "open",
        "user": {"login": "bot"}, "created_at": "x", "updated_at": "x"
    })];
    e.tick_repo(&r).await.unwrap();
    assert!(e.entry(&r, 3).worktree_id.is_none());
    assert!(e.entry(&r, 4).worktree_id.is_none());
    assert!(d.log().is_empty(), "the unchanged poll did not deliver");

    // A later delivery to the follow-up re-creates its owner's
    // workspace and mirrors the new binding back onto the follow-up.
    stub.set_issue(
        3,
        json!({
            "number": 3, "title": "Original work", "body": "work",
            "html_url": "https://gh/3", "state": "closed",
            "user": {"login": "alice"}, "created_at": "x", "updated_at": "x"
        }),
    );
    stub.set_timeline(3, vec![]);
    e.deliver_to(&r, 4, "later activity", None).await.unwrap();
    let owner = e.entry(&r, 3).clone();
    let dependent = e.entry(&r, 4).clone();
    assert!(owner.worktree_id.is_some());
    assert_eq!(dependent.worktree_id, owner.worktree_id);
    assert_eq!(dependent.worktree_path, owner.worktree_path);
    assert!(owner.released_at.is_none());
    assert!(dependent.released_at.is_none());
    assert_eq!(dependent.shares_workspace_of, Some(3));
}
#[test]
fn marking_released_forgets_the_workspace_on_the_owner_and_its_items() {
    let mut e = engine();
    let r = repo();
    for n in [1, 2] {
        seeded(&mut e, n, Some("b1"), false);
        let st = e.entry(&r, n);
        st.worktree_id = Some("repo::/w/1".into());
        st.worktree_path = Some("/w/1".into());
        st.terminal_handle = Some("h".into());
    }
    e.entry(&r, 2).shares_workspace_of = Some(1);
    e.entry(&r, 1).release_pending = true;
    seeded(&mut e, 3, Some("b3"), false);
    e.entry(&r, 3).worktree_id = Some("repo::/w/3".into());
    e.mark_released(&r, 1);
    for n in [1, 2] {
        let st = e.entry(&r, n).clone();
        assert!(st.worktree_id.is_none(), "#{n}");
        assert!(st.worktree_path.is_none());
        assert!(st.terminal_handle.is_none());
        assert!(st.released_at.is_some());
        assert!(!st.release_pending);
    }
    assert!(e.entry(&r, 3).worktree_id.is_some());
    assert!(e.entry(&r, 3).released_at.is_none());
    // Re-creating the workspace clears the mark.
    let wt = Worktree {
        id: "repo::/w/1b".into(),
        path: "/w/1b".into(),
        branch: None,
    };
    e.remember_worktree(&r, 1, &wt);
    assert!(e.entry(&r, 1).released_at.is_none());
}
#[tokio::test]
async fn daemon_side_refusals_are_capped_and_give_the_workspace_up() {
    let mut e = engine();
    let r = repo();
    e.cfg.repos.push(r.clone());
    seeded(&mut e, 1, Some("b1"), false);
    {
        let st = e.entry(&r, 1);
        st.worktree_id = Some("repo::/w/1".into());
        // No such directory: the re-check cannot pass.
        st.worktree_path = Some("/nonexistent/ssf-w1".into());
        st.github_state = Some("closed".into());
    }
    // The fake Orca cannot be asked, so the workspace counts as still
    // there and no agent is live to tell; the refusal is counted all
    // the same.
    for n in 1..=MAX_RELEASE_REFUSALS {
        e.entry(&r, 1).release_pending = true;
        let st = e.entry(&r, 1).clone();
        e.finish_release(&r, st).await;
        let st = e.entry(&r, 1).clone();
        assert!(!st.release_pending, "attempt {n}");
        assert_eq!(st.release_refusals, n);
        assert!(st.worktree_id.is_some(), "the workspace is kept");
        assert!(st.released_at.is_none());
    }
    // Given up: the agent's next request is refused outright...
    let resp = e
        .handle_request(Request::Release {
            session: "o/r#1".into(),
            force: false,
        })
        .await;
    assert!(!resp.ok);
    let err = resp.error.unwrap();
    assert!(err.contains("given up after 3 refusals"), "{err}");
    assert!(!e.entry(&r, 1).release_pending);
    // ...and it shows as such.
    let sessions = crate::status::sessions(&e.cfg, &e.state, None);
    assert_eq!(sessions[0].workspace_state.as_deref(), Some("given-up"));
    assert!(crate::status::render_peers(&sessions, None).contains("release given up"));
    // A person's forced release goes ahead and resets the count.
    let resp = e
        .handle_request(Request::Release {
            session: "o/r#1".into(),
            force: true,
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    let st = e.entry(&r, 1).clone();
    assert!(st.release_pending);
    assert!(st.release_forced, "forced: no re-check on the pass");
    assert!(st.released_at.is_none());
    assert_eq!(st.release_refusals, 0);
    // Re-creating the workspace also starts afresh.
    e.entry(&r, 1).release_refusals = MAX_RELEASE_REFUSALS;
    let wt = Worktree {
        id: "repo::/w/1b".into(),
        path: "/w/1b".into(),
        branch: None,
    };
    e.remember_worktree(&r, 1, &wt);
    assert_eq!(e.entry(&r, 1).release_refusals, 0);
}
#[test]
fn release_refused_prompt_names_the_work_and_the_last_warning() {
    let problems = vec!["1 uncommitted change".to_string()];
    let p = prompt::release_refused_prompt("o/r", 1, &problems, 1, 3);
    assert!(p.starts_with("[ssf] Release of this workspace refused (1 of 3)"));
    assert!(p.contains("- 1 uncommitted change"));
    assert!(p.contains("run `ssf release` again"));
    let last = prompt::release_refused_prompt("o/r", 1, &problems, 3, 3);
    assert!(last.contains("will not ask again"));
    assert!(!last.contains("run `ssf release` again"));
}
#[tokio::test]
async fn a_release_is_off_once_the_item_is_live_again() {
    let mut e = engine();
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos.push(r.clone());
    // Open and assigned: nothing to release, not even by force.
    seeded(&mut e, 1, Some("b1"), true);
    e.entry(&r, 1).triggers = vec!["assigned".into()];
    e.entry(&r, 1).worktree_id = Some("repo::/w/1".into());
    e.entry(&r, 1).worktree_path = Some("/w/1".into());
    let resp = e
        .handle_request(Request::Release {
            session: "o/r#1".into(),
            force: true,
        })
        .await;
    assert!(!resp.ok);
    assert!(resp.error.unwrap().contains("still open and assigned"));
    assert!(!e.entry(&r, 1).release_pending);
    // The reopen race: release approved, then the item came back before
    // the pass. The pass drops the release and leaves the workspace.
    {
        let st = e.entry(&r, 1);
        st.release_pending = true;
        st.release_forced = true;
        st.terminal_handle = Some("h".into());
    }
    d.seed("repo::/w/1", "h", READY_SCREEN);
    e.run_cleanups(&r).await;
    let st = e.entry(&r, 1).clone();
    assert!(!st.release_pending);
    assert!(!st.release_forced);
    assert_eq!(st.worktree_id.as_deref(), Some("repo::/w/1"));
    assert_eq!(st.worktree_path.as_deref(), Some("/w/1"));
    assert_eq!(st.terminal_handle.as_deref(), Some("h"));
    assert!(st.released_at.is_none());
    // A plain release is also dropped if a bound item becomes active
    // between request and the daemon's pass.
    seeded(&mut e, 2, Some("b2"), false);
    {
        let st = e.entry(&r, 2);
        st.worktree_id = Some("w2".into());
        st.worktree_path = Some("/nonexistent/ssf-w2".into());
        st.release_pending = true;
    }
    d.seed("w2", "t2", READY_SCREEN);
    seeded(&mut e, 3, Some("b2"), true);
    e.entry(&r, 3).shares_workspace_of = Some(2);
    e.run_cleanups(&r).await;
    assert!(!e.entry(&r, 2).release_pending);
    assert!(!e.entry(&r, 2).release_forced);
    assert_eq!(e.entry(&r, 2).worktree_id.as_deref(), Some("w2"));
    e.entry(&r, 3).active = false;
    e.entry(&r, 2).release_pending = true;
    e.run_cleanups(&r).await; // plain: re-checked, and the path is gone
    let st = e.entry(&r, 2).clone();
    assert!(!st.release_pending);
    assert_eq!(st.release_refusals, 1);
    assert!(st.worktree_id.is_some());
}
#[tokio::test]
async fn a_workspace_made_by_the_old_driver_is_re_created_on_the_new_one() {
    const ORCA_REPO: &str = "1b790ad2-4421-43dc-9f46-f7c09d0c321f";
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.driver = Some(DriverKind::Herdr);
    e.cfg.repos = vec![repo()];
    stub.set_timeline(5, vec![assigned_by(1, "alice")]);
    seeded(&mut e, 5, Some("bot/issue-5-fix-the-widget"), true);
    {
        let st = e.entry(&repo(), 5);
        st.title = "Fix the widget".into();
        st.html_url = "https://gh/5".into();
        st.worktree_name = Some("issue-5-fix-the-widget".into());
        // As a state file from before the driver was written down has it.
        st.driver = None;
        st.repo_id = Some(ORCA_REPO.into());
        st.worktree_id = Some(format!(
            "{ORCA_REPO}::/home/me/orca/projects/r.worktrees/issue-5-fix-the-widget"
        ));
        st.worktree_path = Some("/home/me/orca/projects/r.worktrees/issue-5-fix-the-widget".into());
        st.terminal_handle = Some("orca-terminal".into());
    }
    let delivered = e.deliver_to(&repo(), 5, "hello", None).await.unwrap();
    assert!(delivered.relaunched);
    let st = e.entry(&repo(), 5).clone();
    // The binding went through the current driver's project setup.
    assert_eq!(st.repo_id.as_deref(), Some("stub"));
    assert_eq!(st.driver.as_deref(), Some("herdr"));
    assert_eq!(
        st.worktree_id.as_deref(),
        Some("stub::/stub.worktrees/issue-5-fix-the-widget"),
        "re-created under its old name"
    );
    assert_eq!(st.terminal_handle.as_deref(), Some("t1"));
    assert!(st.seeded, "not re-onboarded");
    assert!(e.failures.is_empty(), "no failure counted");
    let log = d.log();
    assert_eq!(
        log[0],
        "relaunch:stub::/stub.worktrees/issue-5-fix-the-widget:false"
    );
    assert!(
        log[1].starts_with("deliver:stub::/stub.worktrees/issue-5-fix-the-widget:"),
        "{log:?}"
    );
    // The item is told the agent was attached again, and why.
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
             driver: herdr\n\
             branch: bot/issue-5-fix-the-widget\n\
             re-created: driver switch\n\
             conversation: fresh\n\
             ```"
    );
    // The record now says herdr: the next delivery finds the workspace
    // as it is and nothing is re-created, so nothing is posted.
    e.deliver_to(&repo(), 5, "again", None).await.unwrap();
    assert_eq!(
        d.log(),
        vec!["deliver:stub::/stub.worktrees/issue-5-fix-the-widget:again"]
    );
    assert!(stub.posts().is_empty());
    assert_eq!(
        e.entry(&repo(), 5).worktree_id.as_deref(),
        Some("stub::/stub.worktrees/issue-5-fix-the-widget")
    );

    // And the other way round: a record that says herdr, with the
    // checkout path as its repo id, once the repository runs in Orca.
    let d = crate::driver::StubDriver::new(DriverKind::Orca);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.driver = Some(DriverKind::Orca);
    {
        let st = e.entry(&repo(), 5);
        st.driver = Some("herdr".into());
        st.repo_id = Some("/home/me/ssf/projects/r".into());
        st.worktree_id = Some("w7@/home/me/ssf/projects/r.worktrees/issue-5-fix-the-widget".into());
        st.worktree_path = Some("/home/me/ssf/projects/r.worktrees/issue-5-fix-the-widget".into());
    }
    e.deliver_to(&repo(), 5, "hello", None).await.unwrap();
    let st = e.entry(&repo(), 5).clone();
    assert_eq!(st.repo_id.as_deref(), Some("stub"));
    assert_eq!(st.driver.as_deref(), Some("orca"));
    assert_eq!(
        st.worktree_id.as_deref(),
        Some("stub::/stub.worktrees/issue-5-fix-the-widget")
    );
    assert!(e.failures.is_empty());
    assert_eq!(
        d.log()[0],
        "relaunch:stub::/stub.worktrees/issue-5-fix-the-widget:false"
    );
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert!(
        posts[0].1.contains("driver: orca\n") && posts[0].1.contains("re-created: driver switch\n"),
        "{}",
        posts[0].1
    );
}
#[tokio::test]
async fn a_binding_that_fits_the_current_driver_is_kept() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.driver = Some(DriverKind::Herdr);
    e.cfg.repos = vec![repo()];
    seeded(&mut e, 5, None, true);
    {
        let st = e.entry(&repo(), 5);
        st.driver = None;
        st.repo_id = Some("stub".into());
        st.worktree_id = Some("w5".into());
        st.worktree_path = Some("/w/5".into());
    }
    d.seed("w5", "t5", READY_SCREEN);
    e.deliver_to(&repo(), 5, "hello", None).await.unwrap();
    let st = e.entry(&repo(), 5).clone();
    assert_eq!(st.worktree_id.as_deref(), Some("w5"));
    assert_eq!(st.repo_id.as_deref(), Some("stub"));
    assert_eq!(d.log(), vec!["deliver:w5:hello"]);
}
