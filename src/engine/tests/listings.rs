use super::*;

#[tokio::test(flavor = "current_thread")]
async fn a_new_repo_enrollment_lists_existing_allocations_without_starting_them() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let mut r = repo();
    r.enrolled_at = Some("generation-1".into());
    e.cfg.repos = vec![r.clone()];
    let listed = assigned_item(3, "alice", "u1");
    stub.set_assigned(vec![listed.clone()]);

    // A candidate left by a prior enrollment is not part of this one.
    e.state.repo_mut(&r.name).adoption_candidates.insert(
        99,
        AdoptionCandidate {
            number: 99,
            title: "stale".into(),
            html_url: "https://gh/99".into(),
            updated_at: "old".into(),
            kind: "issue".into(),
            triggers: vec!["assigned".into()],
        },
    );

    // Retained state from another enrollment is detached, but its checkout
    // remains available for an explicit adoption to reuse.
    {
        let st = e.entry(&r, 3);
        st.seeded = true;
        st.active = true;
        st.worktree_id = Some("w3".into());
        st.worktree_path = Some("/worktrees/w3".into());
        st.terminal_handle = Some("old-terminal".into());
        st.agent_session_id = Some("old-conversation".into());
    }
    // This old session disappeared from the listings while the repository
    // was removed. Discovery must not resume it merely to retire it.
    {
        let st = e.entry(&r, 9);
        st.seeded = true;
        st.active = true;
        st.worktree_id = Some("w9".into());
        st.worktree_path = Some("/worktrees/w9".into());
        st.terminal_handle = Some("old-nine".into());
        st.agent_session_id = Some("old-nine-conversation".into());
    }
    let mut closed = assigned_item(9, "alice", "u2");
    closed["state"] = json!("closed");
    stub.set_issue(9, closed);
    d.with(|s| {
        s.worktrees.insert("w3".into());
        s.worktrees.insert("w9".into());
    });
    e.startup_pending = vec![DriverKind::Herdr];
    e.cfg.save().unwrap();
    e.tick().await;

    let rs = &e.state.repos[&r.name];
    assert_eq!(rs.enrollment_seen.as_deref(), Some("generation-1"));
    assert_eq!(
        rs.adoption_candidates.keys().copied().collect::<Vec<_>>(),
        vec![3],
        "the new enrollment must replace the prior candidate snapshot"
    );
    assert_eq!(rs.adoption_candidates[&3].triggers, vec!["assigned"]);
    assert!(!rs.issues[&3].seeded && !rs.issues[&3].active);
    assert_eq!(rs.issues[&3].worktree_id.as_deref(), Some("w3"));
    assert_eq!(
        rs.issues[&3].terminal_handle.as_deref(),
        Some("old-terminal")
    );
    assert_eq!(
        rs.issues[&3].agent_session_id.as_deref(),
        Some("old-conversation")
    );
    assert!(d.launches().is_empty(), "discovery must not start an agent");
    assert!(d.log().is_empty(), "discovery touched a retained session");
    assert!(!rs.issues[&9].seeded && !rs.issues[&9].active);

    // A later listing change still cannot start the candidate implicitly.
    stub.set_assigned(vec![assigned_item(3, "alice", "u2")]);
    e.tick_repo(&r).await.unwrap();
    assert!(d.launches().is_empty());
    assert!(e.state.repos[&r.name].adoption_candidates.contains_key(&3));
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_adoption_starts_fresh_with_the_complete_github_story() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let mut r = repo();
    r.enrolled_at = Some("generation-1".into());
    e.cfg.repos = vec![r.clone()];
    let listed = assigned_item(3, "alice", "u1");
    stub.set_assigned(vec![listed.clone()]);
    stub.set_issue(3, listed);
    stub.set_timeline(
        3,
        vec![
            assigned_by(1, "alice"),
            comment(2, "alice", "the historical detail adoption must replay"),
        ],
    );
    e.tick_repo(&r).await.unwrap();
    assert!(d.prompts().is_empty());
    {
        let st = e.entry(&r, 3);
        st.worktree_id = Some("w3".into());
        st.worktree_path = Some("/worktrees/w3".into());
        st.terminal_handle = Some("stale-terminal".into());
        st.agent_session_id = Some("old-conversation".into());
    }
    d.seed("w3", "old-terminal", READY_SCREEN);

    let response = e
        .handle_request(Request::Adopt {
            items: vec!["o/r#3".into()],
        })
        .await;
    assert!(response.ok, "{:?}", response.error);
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(prompts[0].contains("the historical detail adoption must replay"));
    let log = d.log();
    assert_eq!(log[0], "stop:old-terminal", "{log:?}");
    assert!(
        log.iter().any(|entry| entry == "relaunch:w3:false"),
        "adoption starts fresh rather than resuming: {log:?}"
    );
    assert!(e.state.repos[&r.name].adoption_candidates.is_empty());
    let st = &e.state.repos[&r.name].issues[&3];
    assert!(st.seeded && st.active, "{st:?}");
    assert!(e.refetch.contains(&r.name));
}

#[tokio::test(flavor = "current_thread")]
async fn adopting_a_pull_request_always_starts_its_own_fresh_session() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let mut r = repo();
    r.enrolled_at = Some("generation-1".into());
    e.cfg.repos = vec![r.clone()];
    seeded(&mut e, 1, Some("bot/issue-1"), true);

    let mut pull = assigned_item(3, "alice", "u1");
    pull["user"]["login"] = json!("bot");
    pull["body"] = json!("🤖#1 says: <!-- ssf: origin=o/r#1 -->\n\nImplementation");
    pull["pull_request"] = json!({"url": "https://api.github.test/pulls/3"});
    stub.set_assigned(vec![pull.clone()]);
    stub.set_issue(3, pull);
    stub.set_timeline(3, vec![assigned_by(1, "alice")]);
    stub.set_pull(
        3,
        json!({
            "head": {"ref": "feature", "repo": {"full_name": "o/r"}},
            "base": {"ref": "main"},
            "requested_reviewers": []
        }),
    );
    e.tick_repo(&r).await.unwrap();

    let response = e
        .handle_request(Request::Adopt {
            items: vec!["o/r#3".into()],
        })
        .await;

    assert!(response.ok, "{:?}", response.error);
    let adopted = &e.state.repos[&r.name].issues[&3];
    assert!(adopted.seeded && adopted.active, "{adopted:?}");
    assert!(adopted.shares_workspace_of.is_none(), "{adopted:?}");
    assert_ne!(adopted.worktree_id.as_deref(), Some("w1"));
}

#[tokio::test]
async fn adoption_rechecks_the_allocation_before_stopping_an_existing_agent() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let mut r = repo();
    r.enrolled_at = Some("generation-1".into());
    e.cfg.repos = vec![r.clone()];
    stub.set_assigned(vec![assigned_item(3, "alice", "u1")]);
    e.tick_repo(&r).await.unwrap();

    let mut no_longer_assigned = assigned_item(3, "alice", "u2");
    no_longer_assigned["assignees"] = json!([]);
    stub.set_issue(3, no_longer_assigned);
    stub.set_timeline(3, vec![assigned_by(1, "alice")]);
    {
        let st = e.entry(&r, 3);
        st.worktree_id = Some("w3".into());
        st.terminal_handle = Some("old-terminal".into());
    }
    d.seed("w3", "old-terminal", READY_SCREEN);

    let response = e
        .handle_request(Request::Adopt {
            items: vec!["o/r#3".into()],
        })
        .await;

    assert!(!response.ok, "a stale candidate was adopted");
    assert!(
        response
            .error
            .as_deref()
            .is_some_and(|e| e.contains("no longer allocated")),
        "{:?}",
        response.error
    );
    assert!(d.log().is_empty(), "the existing agent was disturbed");
    assert!(e.state.repos[&r.name].adoption_candidates.is_empty());
}

#[tokio::test]
async fn ignored_items_are_not_fetched_on_a_full_listing_or_after_a_restart() {
    let stub = GitHubStub::start().await;
    let r = repo();
    let created = vec!["created".to_string()];
    let listed = |n: u64| {
        json!({
            "number": n, "title": "t", "body": null, "html_url": format!("https://gh/{n}"),
            "state": "open", "user": {"login": "bot"}, "created_at": "x", "updated_at": "x"
        })
    };
    *stub.created.lock().unwrap() = vec![listed(18), listed(19)];

    // Both were looked at on an earlier pass and ignored as created-only.
    let mut e = engine_at(&stub.base);
    for n in [18, 19] {
        e.state
            .repo_mut(&r.name)
            .ignored
            .insert(n, Ignored::new(&issue(n, "bot", None), &created));
    }

    // Pass 1: no ETags yet, so a full creator listing carrying both,
    // unchanged. Nothing is fetched, and nothing is onboarded (which
    // would fail at the driver here and be counted as a failure).
    e.tick_repo(&r).await.unwrap();
    assert_listings_only(&stub.hits());
    assert!(e.failures.is_empty(), "{:?}", e.failures);
    assert_eq!(e.state.repos[&r.name].created_numbers, vec![18, 19]);

    // A daemon restart: the state file survives, memory does not.
    let dir = std::env::temp_dir().join(format!("ssf-engine-ignored-{}", std::process::id()));
    let path = dir.join("state.json");
    e.state.save_to(&path).unwrap();
    let mut e = engine_at(&stub.base);
    e.state = State::load_from(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        e.state.repos[&r.name]
            .ignored
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![18, 19],
        "the ignore records are persisted"
    );

    // Pass 2: GitHub's ETag rolled over, so a full listing again, with
    // the same content. Nothing is fetched.
    stub.bump_created_etag();
    e.tick_repo(&r).await.unwrap();
    let hits = stub.hits();
    assert!(hits.iter().any(|h| h.contains("creator=")), "{hits:?}");
    assert_listings_only(&hits);
    assert!(e.failures.is_empty(), "{:?}", e.failures);

    // Pass 3: the creator listing is a 304 while another listing
    // changed, so the items come from the cached numbers with no
    // fresh copy. Still nothing is fetched.
    e.tick_repo(&r).await.unwrap();
    assert_listings_only(&stub.hits());
    assert!(e.failures.is_empty(), "{:?}", e.failures);

    // Control: an item nothing remembers is fetched by number on such a
    // pass (the stub fails the fetch, which the pass survives), and its
    // neighbour is not.
    e.state.repo_mut(&r.name).ignored.remove(&19);
    e.tick_repo(&r).await.unwrap();
    let hits = stub.hits();
    assert!(
        hits.contains(&"/repos/o/r/issues/19".to_string()),
        "{hits:?}"
    );
    assert!(
        !hits.iter().any(|h| h.starts_with("/repos/o/r/issues/18")),
        "{hits:?}"
    );

    // An item that leaves every listing is looked at before its record
    // is dropped (issue #138): closed, so #19's record goes, while #18
    // is still listed and keeps its own.
    e.state
        .repo_mut(&r.name)
        .ignored
        .insert(19, Ignored::new(&issue(19, "bot", None), &created));
    stub.set_issue(
        19,
        json!({
            "number": 19, "title": "t", "body": null, "html_url": "https://gh/19",
            "state": "closed", "user": {"login": "bot"}, "created_at": "x", "updated_at": "x"
        }),
    );
    *stub.created.lock().unwrap() = vec![listed(18)];
    stub.bump_created_etag();
    let _ = stub.hits();
    e.tick_repo(&r).await.unwrap();
    let hits = stub.hits();
    assert!(
        hits.contains(&"/repos/o/r/issues/19".to_string()),
        "#19 was looked at before being forgotten: {hits:?}"
    );
    assert!(
        !hits.iter().any(|h| h.starts_with("/repos/o/r/issues/18")),
        "#18 is still listed, so nothing is asked about it: {hits:?}"
    );
    assert_eq!(
        e.state.repos[&r.name]
            .ignored
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![18]
    );
}
#[tokio::test]
async fn one_unblock_owes_exactly_one_full_fetch() {
    let stub = GitHubStub::start().await;
    let r = repo();
    let created = vec!["created".to_string()];
    *stub.created.lock().unwrap() = vec![json!({
        "number": 18, "title": "t", "body": null, "html_url": "https://gh/18",
        "state": "open", "user": {"login": "bot"}, "created_at": "x", "updated_at": "x"
    })];
    let mut e = engine_at(&stub.base);
    // The one listed item is already ignored as created-only, so these
    // passes fetch nothing by number and onboard nothing.
    e.state
        .repo_mut(&r.name)
        .ignored
        .insert(18, Ignored::new(&issue(18, "bot", None), &created));

    // Pass 1: no ETags yet, so a full creator listing. That listing is
    // the one the stub answers 304 to, so it stands for all four here.
    e.tick_repo(&r).await.unwrap();
    assert_eq!(stub.created_fulls(), 1);

    // Pass 2: the ETag it stored is sent back and answered 304.
    e.tick_repo(&r).await.unwrap();
    assert_eq!(stub.created_fulls(), 1, "pass 2 was conditional");

    // A session comes back. The pass it comes back on has read its
    // listings already (and stores the ETags it read at its end), so
    // the full fetch it is owed falls to the next pass.
    let b = Blocked {
        reason: "login".into(),
        harness: "claude".into(),
        detail: "Login expired".into(),
        since: now_iso(),
        reported: false,
        credential: None,
        retried_at: None,
        retries: 0,
        told_at: None,
        tell_failures: 0,
        first_message_owed: false,
    };
    e.unblock(&r, 18, &b, Conversation::Kept).await;
    assert!(e.refetch.contains(&r.name));
    assert!(e.state.repos[&r.name].created_etag.is_none());

    // Pass 3 spends the flag: one full listing, so what was held is seen.
    e.tick_repo(&r).await.unwrap();
    assert_eq!(stub.created_fulls(), 2, "pass 3 fetched in full");
    assert!(e.refetch.is_empty(), "the flag is spent, not re-armed");

    // Pass 4, and every pass after it, is back to conditional requests.
    for _ in 0..3 {
        e.tick_repo(&r).await.unwrap();
    }
    assert_eq!(stub.created_fulls(), 2, "one unblock, one full fetch");
    assert!(e.refetch.is_empty());
    assert!(e.failures.is_empty(), "{:?}", e.failures);
    assert!(stub.post_bodies().is_empty());
}
#[tokio::test(flavor = "current_thread")]
async fn tick_preserves_one_owed_full_fetch_across_the_pass_boundary() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let r = repo();
    let created = vec!["created".to_string()];
    *stub.created.lock().unwrap() = vec![json!({
        "number": 18, "title": "t", "body": null, "html_url": "https://gh/18",
        "state": "open", "user": {"login": "bot"}, "created_at": "x", "updated_at": "x"
    })];
    let mut e = engine_at(&stub.base);
    e.drivers = Drivers::from_list(vec![Driver::Stub(crate::driver::StubDriver::new(
        DriverKind::Herdr,
    ))]);
    e.cfg.github.api_url = stub.base.clone();
    e.cfg.repos = vec![r.clone()];
    e.cfg.daemon.conflict_check_interval_secs = 0;
    e.cfg.save().unwrap();
    e.state
        .repo_mut(&r.name)
        .ignored
        .insert(18, Ignored::new(&issue(18, "bot", None), &created));

    // Establish and then exercise the cached creator-listing ETag.
    e.tick().await;
    assert!(e.state.last_error.is_none(), "{:?}", e.state.last_error);
    assert_eq!(stub.created_fulls(), 1, "the first pass was full");
    e.tick().await;
    assert!(e.state.last_error.is_none(), "{:?}", e.state.last_error);
    assert_eq!(stub.created_fulls(), 1, "the second pass was conditional");

    // A session comes back after its pass read the listings. That pass
    // writes its cached ETag back, leaving only `refetch` to make the
    // next outer tick fetch the listing in full.
    let b = Blocked {
        reason: "login".into(),
        harness: "claude".into(),
        detail: "Login expired".into(),
        since: now_iso(),
        reported: false,
        credential: None,
        retried_at: None,
        retries: 0,
        told_at: None,
        tell_failures: 0,
        first_message_owed: false,
    };
    let read_this_pass = e.state.repos[&r.name].created_etag.clone().unwrap();
    e.unblock(&r, 18, &b, Conversation::Kept).await;
    e.state.repo_mut(&r.name).created_etag = Some(read_this_pass);

    e.tick().await;
    assert!(e.state.last_error.is_none(), "{:?}", e.state.last_error);
    assert_eq!(stub.created_fulls(), 2, "the owed pass was full");
    assert!(e.refetch.is_empty(), "the owed fetch was spent once");

    let _ = stub.hits();
    e.tick().await;
    assert!(e.state.last_error.is_none(), "{:?}", e.state.last_error);
    let hits = stub.hits();
    assert_eq!(
        hits.iter()
            .filter(|h| h.starts_with("/repos/o/r/issues?creator="))
            .count(),
        1,
        "the final pass requested the creator listing: {hits:?}"
    );
    assert_eq!(
        stub.created_fulls(),
        2,
        "the pass after the owed fetch was conditional"
    );
    assert!(e.refetch.is_empty(), "the owed fetch was not re-armed");
    assert!(e.failures.is_empty(), "{:?}", e.failures);
    assert!(stub.post_bodies().is_empty());
}
#[tokio::test]
async fn an_unblock_after_the_listings_were_read_makes_the_next_pass_full() {
    let stub = GitHubStub::start().await;
    let r = repo();
    let created = vec!["created".to_string()];
    *stub.created.lock().unwrap() = vec![json!({
        "number": 18, "title": "t", "body": null, "html_url": "https://gh/18",
        "state": "open", "user": {"login": "bot"}, "created_at": "x", "updated_at": "x"
    })];
    let mut e = engine_at(&stub.base);
    e.state
        .repo_mut(&r.name)
        .ignored
        .insert(18, Ignored::new(&issue(18, "bot", None), &created));

    // Pass 1 has no ETags, pass 2 sends the ones it stored and is
    // answered 304.
    e.tick_repo(&r).await.unwrap();
    assert_eq!(stub.created_fulls(), 1);
    e.tick_repo(&r).await.unwrap();
    assert_eq!(stub.created_fulls(), 1, "pass 2 was conditional");

    // The session comes back part-way through a pass that has already
    // read its listings: the ETags it clears are written back when that
    // pass stores what it read, so nothing but the flag survives it.
    let b = Blocked {
        reason: "login".into(),
        harness: "claude".into(),
        detail: "Login expired".into(),
        since: now_iso(),
        reported: false,
        credential: None,
        retried_at: None,
        retries: 0,
        told_at: None,
        tell_failures: 0,
        first_message_owed: false,
    };
    let read_this_pass = e.state.repos[&r.name].created_etag.clone();
    assert!(read_this_pass.is_some());
    e.unblock(&r, 18, &b, Conversation::Kept).await;
    e.state.repo_mut(&r.name).created_etag = read_this_pass;

    // The next pass is a full one on the strength of the flag alone.
    e.tick_repo(&r).await.unwrap();
    assert_eq!(stub.created_fulls(), 2, "the next pass was full");
    assert!(e.refetch.is_empty());

    // And only that one: the passes after it are conditional again.
    for _ in 0..3 {
        e.tick_repo(&r).await.unwrap();
    }
    assert_eq!(stub.created_fulls(), 2, "one unblock, one full fetch");
    assert!(e.failures.is_empty(), "{:?}", e.failures);
    assert!(stub.post_bodies().is_empty());
}
#[tokio::test]
async fn a_rejected_bot_opened_item_is_not_onboarded_again() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    *stub.created.lock().unwrap() = vec![json!({
        "number": 18, "title": "t", "body": "no tag here",
        "html_url": "https://gh/18", "state": "open",
        "user": {"login": "bot"}, "created_at": "x", "updated_at": "x"
    })];

    // Pass 1: onboarded, found to be nobody's, ignored.
    e.tick_repo(&r).await.unwrap();
    assert!(e.failures.is_empty(), "{:?}", e.failures);
    let hits = stub.hits();
    assert!(
        hits.iter()
            .any(|h| h.starts_with("/repos/o/r/issues/18/timeline")),
        "{hits:?}"
    );
    assert!(
        e.state.repos[&r.name].ignored.contains_key(&18),
        "the rejection records an ignore: {:?}",
        e.state.repos[&r.name].ignored
    );

    // Pass 2, listing unchanged (304): nothing is looked at.
    e.tick_repo(&r).await.unwrap();
    assert_listings_only(&stub.hits());

    // Pass 3, the same listing served in full (GitHub rolled its ETag):
    // still nothing.
    stub.bump_created_etag();
    e.tick_repo(&r).await.unwrap();
    assert_listings_only(&stub.hits());
    assert!(
        e.state.repos[&r.name].ignored.contains_key(&18),
        "the record survives the pass"
    );
}
#[tokio::test]
async fn a_short_listing_does_not_throw_away_ignore_records() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    *stub.created.lock().unwrap() = vec![untagged_listed(18, "open")];
    stub.set_issue(18, untagged_listed(18, "open"));

    // Onboarded once, found to be nobody's, ignored.
    e.tick_repo(&r).await.unwrap();
    assert!(e.failures.is_empty(), "{:?}", e.failures);
    assert_eq!(ignored_numbers(&e, &r), vec![18]);

    // The listing comes back empty, and then it recovers.
    *stub.created.lock().unwrap() = vec![];
    stub.bump_created_etag();
    e.tick_repo(&r).await.unwrap();
    assert_eq!(ignored_numbers(&e, &r), vec![18], "the record stands");
    *stub.created.lock().unwrap() = vec![untagged_listed(18, "open")];
    stub.bump_created_etag();
    let _ = stub.hits();
    e.tick_repo(&r).await.unwrap();

    // Nothing was fetched by number, so nothing was onboarded again.
    assert_listings_only(&stub.hits());
    assert!(e.failures.is_empty(), "{:?}", e.failures);
    assert_eq!(ignored_numbers(&e, &r), vec![18]);
}
#[tokio::test]
async fn an_ignored_item_is_asked_about_once_and_forgotten_when_it_is_gone() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    *stub.created.lock().unwrap() = vec![untagged_listed(18, "open"), untagged_listed(19, "open")];
    stub.set_issue(18, untagged_listed(18, "open"));
    stub.set_issue(19, untagged_listed(19, "open"));
    e.tick_repo(&r).await.unwrap();
    assert_eq!(ignored_numbers(&e, &r), vec![18, 19]);

    // Both missing: each is asked about once...
    *stub.created.lock().unwrap() = vec![];
    stub.bump_created_etag();
    let _ = stub.hits();
    e.tick_repo(&r).await.unwrap();
    let hits = stub.hits();
    for n in [18, 19] {
        assert!(
            hits.contains(&format!("/repos/o/r/issues/{n}")),
            "asked about #{n}: {hits:?}"
        );
    }

    // ...and not again while the absence lasts.
    stub.bump_created_etag();
    e.tick_repo(&r).await.unwrap();
    let hits = stub.hits();
    assert!(
        !hits.iter().any(|h| h.starts_with("/repos/o/r/issues/1")),
        "{hits:?}"
    );
    assert_eq!(ignored_numbers(&e, &r), vec![18, 19]);

    // #19 comes back on the listing and goes missing again, closed
    // this time. A listing that flaps buys no look of its own: the
    // record is asked about again when the next look is due, and its
    // record goes then. #18 keeps its own.
    *stub.created.lock().unwrap() = vec![untagged_listed(18, "open"), untagged_listed(19, "open")];
    stub.bump_created_etag();
    e.tick_repo(&r).await.unwrap();
    *stub.created.lock().unwrap() = vec![untagged_listed(18, "open")];
    stub.set_issue(19, untagged_listed(19, "closed"));
    stub.bump_created_etag();
    let _ = stub.hits();
    e.tick_repo(&r).await.unwrap();
    assert_listings_only(&stub.hits());
    assert_eq!(
        ignored_numbers(&e, &r),
        vec![18, 19],
        "asked about too soon"
    );

    rewind_absence(&mut e, &r, 19, ABSENT_RECHECK);
    stub.bump_created_etag();
    e.tick_repo(&r).await.unwrap();
    assert_eq!(ignored_numbers(&e, &r), vec![18]);
    assert_eq!(
        recorded_numbers(&e, &r),
        vec![18],
        "the closed item's own record goes with its ignore record (#409)"
    );
    assert!(e.failures.is_empty(), "{:?}", e.failures);

    // #18 goes missing while open: looked at once, record kept, and
    // then left alone. An item that never comes back (its mention
    // edited away, say) would otherwise be looked at for ever, so the
    // record is given up once the absence is too long to be a lagging
    // listing — without asking GitHub again.
    *stub.created.lock().unwrap() = vec![];
    stub.bump_created_etag();
    e.tick_repo(&r).await.unwrap();
    assert_eq!(ignored_numbers(&e, &r), vec![18]);
    let _ = stub.hits();
    stub.bump_created_etag();
    e.tick_repo(&r).await.unwrap();
    assert_listings_only(&stub.hits());
    assert_eq!(ignored_numbers(&e, &r), vec![18], "given up too soon");

    rewind_absence(&mut e, &r, 18, ABSENT_GIVE_UP);
    stub.bump_created_etag();
    e.tick_repo(&r).await.unwrap();
    assert!(ignored_numbers(&e, &r).is_empty(), "the record goes");
    assert!(
        recorded_numbers(&e, &r).is_empty(),
        "and so does the item's own record: {:?}",
        recorded_numbers(&e, &r)
    );
    assert_listings_only(&stub.hits());
}

// The records #409 is about are the shape an item ignored at creation
// leaves: no session, no subscriber, no workspace. Anything else still
// answers to something after the item stops being looked at, and keeps
// its record when the ignore record goes.
#[tokio::test]
async fn forgetting_an_ignored_item_keeps_a_record_with_a_session() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    let created = vec!["created".to_string()];
    for n in [18, 19] {
        e.state
            .repo_mut(&r.name)
            .ignored
            .insert(n, Ignored::new(&issue(n, "bot", None), &created));
    }
    // #18 was ignored at creation and picked up a session afterwards (an
    // assignment between passes, say), so its record is one to retire
    // through; #19 is only an ignore record.
    seeded(&mut e, 18, Some("bot/issue-18"), false);
    e.entry(&r, 18).worktree_id = Some("w18".into());
    for n in [18, 19] {
        e.entry(&r, n).github_state = Some("open".into());
        stub.set_issue(n, untagged_listed(n, "closed"));
    }
    *stub.created.lock().unwrap() = vec![];

    e.tick_repo(&r).await.unwrap();
    assert!(ignored_numbers(&e, &r).is_empty(), "both records go");
    assert_eq!(
        recorded_numbers(&e, &r),
        vec![18],
        "the session's own record stays for the retirement and the cleanup"
    );
    assert!(e.failures.is_empty(), "{:?}", e.failures);
}
// A record that answers to nothing has no session to retire through it, no
// subscriber to poll for and no workspace to release, so once its item is
// on no listing the pass forgets it: the shape an onboarding that never
// got a session onto the item leaves behind -- an item ignored at creation
// whose ignore record has already gone, or a bind whose delivery never
// landed (#409).
#[tokio::test]
async fn a_record_answering_to_nothing_is_forgotten_once_its_item_is_unlisted() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    let created = vec!["created".to_string()];

    // #18 never got a session and has no ignore record (a bind whose
    // delivery never landed, say). #20 is the same shape but was a
    // session's, whose binding a run of failures gave up.
    for n in [18, 20] {
        let st = e.entry(&r, n);
        st.kind = Some("pull_request".into());
        st.github_state = Some("open".into());
    }
    {
        let st = e.entry(&r, 20);
        st.bound_at = Some("2026-09-20T09:21:40Z".into());
        st.retired_at = Some("2026-09-20T09:46:28Z".into());
        st.github_state = Some("merged".into());
    }
    // #19 is still being ignored: its clock decides, not this pass.
    e.state
        .repo_mut(&r.name)
        .ignored
        .insert(19, Ignored::new(&issue(19, "bot", None), &created));
    e.entry(&r, 19).github_state = Some("open".into());
    stub.set_issue(19, untagged_listed(19, "open"));

    e.tick_repo(&r).await.unwrap();
    assert_eq!(
        recorded_numbers(&e, &r),
        vec![19, 20],
        "#18 is gone; the ignored item's clock and the session's record stay"
    );
    assert!(
        !stub.hits().iter().any(|h| h.contains("/issues/18")),
        "no request was spent on it: {:?}",
        stub.hits()
    );

    // The clock does forget its item's record once GitHub says it closed
    // -- the same moment the ignore record goes.
    rewind_absence(&mut e, &r, 19, ABSENT_RECHECK);
    stub.set_issue(19, untagged_listed(19, "closed"));
    stub.bump_created_etag();
    e.tick_repo(&r).await.unwrap();
    assert!(ignored_numbers(&e, &r).is_empty());
    assert_eq!(
        recorded_numbers(&e, &r),
        vec![20],
        "the session's record stays"
    );
    assert!(e.failures.is_empty(), "{:?}", e.failures);
}
#[tokio::test]
async fn an_ignored_item_gone_from_github_loses_its_record_but_a_failure_does_not() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    let created = vec!["created".to_string()];
    for n in [18, 19] {
        e.state
            .repo_mut(&r.name)
            .ignored
            .insert(n, Ignored::new(&issue(n, "bot", None), &created));
    }
    // #18 is gone from GitHub; #19's fetch fails (the stub answers 500
    // for an item it was not given).
    stub.set_missing(18);
    *stub.created.lock().unwrap() = vec![];
    e.tick_repo(&r).await.unwrap();
    assert_eq!(ignored_numbers(&e, &r), vec![19]);

    // The failure is not retried on every pass while it lasts.
    let _ = stub.hits();
    stub.bump_created_etag();
    e.tick_repo(&r).await.unwrap();
    assert_listings_only(&stub.hits());
    assert_eq!(ignored_numbers(&e, &r), vec![19]);
}
#[tokio::test]
async fn absent_records_beyond_a_pass_s_looks_are_not_starved() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    let created = vec!["created".to_string()];
    let n = ABSENT_LOOKS_PER_PASS as u64;
    // Twice what a pass looks at, all open and on no listing, plus one
    // that has been absent long enough to be given up.
    for i in 1..=2 * n {
        e.state
            .repo_mut(&r.name)
            .ignored
            .insert(i, Ignored::new(&issue(i, "bot", None), &created));
        stub.set_issue(i, untagged_listed(i, "open"));
    }
    e.state
        .repo_mut(&r.name)
        .ignored
        .insert(9000, Ignored::new(&issue(9000, "bot", None), &created));

    e.tick_repo(&r).await.unwrap();
    let mut asked: BTreeSet<u64> = looked_at(&stub.hits());
    assert_eq!(asked.len(), ABSENT_LOOKS_PER_PASS, "one pass's worth");

    // Every record is due again on the next pass, which is the
    // ordinary case: the prune only runs on a pass where a listing
    // changed, and those are rarer than the look interval.
    for i in 1..=2 * n {
        rewind_absence(&mut e, &r, i, ABSENT_RECHECK);
    }
    // #9000 sorts last by number and was never looked at, so under a
    // budget that ran in number order it would wait behind everything.
    // It is past the giving up, which the budget does not ration.
    rewind_absence(&mut e, &r, 9000, ABSENT_GIVE_UP);
    stub.bump_created_etag();
    e.tick_repo(&r).await.unwrap();
    assert!(
        !e.state.repos[&r.name].ignored.contains_key(&9000),
        "given up whatever the budget was spent on"
    );
    asked.extend(looked_at(&stub.hits()));

    // The ones the earlier passes could not reach are asked about on
    // the passes that follow, rather than the same few going round.
    for _ in 0..3 {
        for i in 1..=2 * n {
            rewind_absence(&mut e, &r, i, ABSENT_RECHECK);
        }
        stub.bump_created_etag();
        e.tick_repo(&r).await.unwrap();
        asked.extend(looked_at(&stub.hits()));
    }
    let missed: Vec<u64> = (1..=2 * n).filter(|i| !asked.contains(i)).collect();
    assert!(missed.is_empty(), "never looked at: {missed:?}");
    assert_eq!(
        e.state.repos[&r.name].ignored.len(),
        2 * n as usize,
        "all still open, so all still ignored"
    );
}
#[tokio::test]
async fn an_unreadable_clock_in_an_ignore_record_is_replaced() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    let created = vec!["created".to_string()];
    let at = e
        .state
        .repo_mut(&r.name)
        .ignored
        .entry(18)
        .or_insert_with(|| Ignored::new(&issue(18, "bot", None), &created));
    at.absent_since = Some("not a date".into());
    at.asked_at = Some("not a date".into());
    stub.set_issue(18, untagged_listed(18, "open"));

    e.tick_repo(&r).await.unwrap();
    let at = &e.state.repos[&r.name].ignored[&18];
    assert!(
        at.absent_since.as_deref().and_then(since).is_some(),
        "the absence is dated afresh: {at:?}"
    );
    assert!(
        at.asked_at.as_deref().and_then(since).is_some(),
        "and the look that was due happened: {at:?}"
    );
}
#[tokio::test]
async fn an_ignored_item_that_leaves_one_of_its_listings_is_left_alone() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    let item = json!({
        "number": 5, "title": "t", "body": "@bot look at this",
        "html_url": "https://gh/5", "state": "open", "user": {"login": "stranger"},
        "assignees": [{"login": "bot"}], "created_at": "x", "updated_at": "u1"
    });
    e.state.repo_mut(&r.name).ignored.insert(
        5,
        Ignored::new(
            &serde_json::from_value(item.clone()).unwrap(),
            &["assigned".to_string(), "mentioned".to_string()],
        ),
    );
    // Only the assignee listing carries it this pass: nothing is
    // fetched, and the record is left as it was.
    stub.set_assigned(vec![item]);
    e.tick_repo(&r).await.unwrap();
    assert_listings_only(&stub.hits());
    assert_eq!(
        e.state.repos[&r.name].ignored[&5].triggers,
        vec!["assigned".to_string(), "mentioned".to_string()],
    );
    assert!(e.failures.is_empty(), "{:?}", e.failures);
}
#[tokio::test]
async fn a_refused_item_keeps_its_record_when_its_listing_comes_back_short() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    let mentioned = vec!["mentioned".to_string()];
    let item = json!({
        "number": 42, "title": "t", "body": "@bot look at this",
        "html_url": "https://gh/42", "state": "open",
        "user": {"login": "stranger"}, "created_at": "x", "updated_at": "x"
    });
    e.state.repo_mut(&r.name).ignored.insert(
        42,
        Ignored::new(&serde_json::from_value(item.clone()).unwrap(), &mentioned),
    );
    stub.set_issue(42, item);
    // On no listing this pass: still open, so the record stands.
    e.tick_repo(&r).await.unwrap();
    assert_eq!(ignored_numbers(&e, &r), vec![42]);
}
#[test]
fn purge_candidates_are_closed_owning_sessions_without_open_dependents() {
    let mut e = engine();
    let r = repo();
    let bind = |e: &mut Engine, n: u64, state: &str, active: bool, retired: &str| {
        seeded(e, n, Some(&format!("b{n}")), active);
        let st = e.entry(&r, n);
        st.worktree_id = Some(format!("repo::/w/{n}"));
        st.worktree_path = Some(format!("/w/{n}"));
        st.github_state = Some(state.into());
        st.retired_at = Some(retired.into());
    };
    bind(&mut e, 1, "closed", false, "2026-01-01T00:00:00Z"); // yes
    bind(&mut e, 2, "merged", false, "2026-01-01T00:00:00Z"); // yes
    bind(&mut e, 3, "open", true, "2026-01-01T00:00:00Z"); // still active
    bind(&mut e, 4, "open", false, "2026-01-01T00:00:00Z"); // unassigned but open
    bind(&mut e, 5, "closed", false, "2026-01-01T00:00:00Z"); // owns open #6
    bind(&mut e, 6, "open", true, "2026-01-01T00:00:00Z");
    e.entry(&r, 6).shares_workspace_of = Some(5);
    bind(&mut e, 7, "closed", false, "2026-01-01T00:00:00Z"); // bound to #5's workspace
    e.entry(&r, 7).shares_workspace_of = Some(5);
    bind(&mut e, 8, "closed", false, "2026-01-01T00:00:00Z"); // already released
    e.entry(&r, 8).worktree_id = None;
    bind(&mut e, 9, "closed", false, "2099-01-01T00:00:00Z"); // retired "just now"
    let nums = |v: Vec<IssueState>| v.into_iter().map(|s| s.number).collect::<Vec<_>>();
    assert_eq!(nums(e.purge_candidates(&r, None)), vec![1, 2, 9]);
    assert_eq!(nums(e.purge_candidates(&r, Some(30))), vec![1, 2]);
    // Once #6 closes, #5's workspace is a candidate too.
    e.entry(&r, 6).active = false;
    assert_eq!(nums(e.purge_candidates(&r, None)), vec![1, 2, 5, 9]);
    assert!(
        e.purge_candidates(
            &RepoConfig {
                name: "x/y".into(),
                ..Default::default()
            },
            None
        )
        .is_empty()
    );
}
#[tokio::test]
async fn purge_judges_a_checkout_whose_workspace_is_gone_by_the_checkout() {
    use crate::release::testkit::{scratch, sh};
    let s = scratch("purge-stray").await;
    let (path, _) = crate::driver::add_local_worktree(&s.work, "issue-1-x", None)
        .await
        .unwrap();
    std::fs::write(Path::new(&path).join("only-here.txt"), "only here\n").unwrap();
    sh(&path, &["add", "."]).await;
    sh(&path, &["commit", "-q", "-m", "only here"]).await;
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    e.cfg.driver = Some(DriverKind::Herdr);
    e.cfg.repos = vec![repo()];
    let r = repo();
    for (n, p) in [(1, path.clone()), (2, "/nonexistent/w/2".to_string())] {
        seeded(&mut e, n, Some(&format!("bot/issue-{n}-x")), false);
        let st = e.entry(&r, n);
        // Neither workspace is one the driver knows.
        st.worktree_id = Some(format!("w{n}@{p}"));
        st.worktree_path = Some(p);
        st.github_state = Some("closed".into());
        st.retired_at = Some("2026-01-01T00:00:00Z".into());
    }
    let rows = |v: Value| {
        v["workspaces"]
            .as_array()
            .cloned()
            .unwrap()
            .into_iter()
            .map(|r| {
                (
                    r["session"].as_str().unwrap().to_string(),
                    r["state"].as_str().unwrap().to_string(),
                    r["workspace"].as_str().map(str::to_string),
                    r["removed"].as_bool().unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    // Dry run: the checkout on disk is judged (its branch was never
    // pushed), the one that is not is already gone.
    let v = e.purge(true, None, false).await.unwrap();
    assert_eq!(
        rows(v),
        vec![
            (
                "o/r#1".to_string(),
                "unpushed commits".to_string(),
                Some("gone".to_string()),
                false
            ),
            ("o/r#2".to_string(), "already gone".to_string(), None, false),
        ]
    );
    assert!(Path::new(&path).is_dir());
    // For real: the unpushed one is kept, the other forgotten.
    let v = e.purge(false, None, false).await.unwrap();
    assert!(!rows(v)[0].3);
    assert!(Path::new(&path).is_dir());
    assert!(e.peek(&r, 1).unwrap().worktree_id.is_some());
    assert!(e.peek(&r, 2).unwrap().worktree_id.is_none());
    // Pushed: clean and pushed, so it goes, with git since no driver
    // has it.
    sh(&path, &["push", "-q", "-u", "origin", "bot/issue-1-x"]).await;
    let v = e.purge(false, None, false).await.unwrap();
    assert_eq!(
        rows(v),
        vec![(
            "o/r#1".to_string(),
            "clean and pushed".to_string(),
            Some("gone".to_string()),
            true
        )]
    );
    assert!(!Path::new(&path).exists());
    assert!(e.peek(&r, 1).unwrap().worktree_id.is_none());
    assert!(
        d.log().iter().all(|l| !l.starts_with("remove:")),
        "{:?}",
        d.log()
    );
}
