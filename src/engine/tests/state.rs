use super::*;

#[test]
fn drivers_follow_the_config() {
    let mut e = engine();
    let orca = repo();
    let mut herdr = repo();
    herdr.name = "o/h".into();
    herdr.driver = Some(DriverKind::Herdr);
    e.cfg.repos = vec![orca.clone(), herdr.clone()];
    // What a reloaded config that added a herdr repo does.
    e.sync_drivers();
    assert_eq!(e.driver(&orca).kind(), DriverKind::Orca);
    assert_eq!(e.driver(&herdr).kind(), DriverKind::Herdr);
    // And the other way: the default switched, Orca no longer used.
    e.cfg.driver = Some(DriverKind::Herdr);
    e.cfg.repos = vec![herdr.clone()];
    e.sync_drivers();
    assert_eq!(e.drivers.kinds(), vec![DriverKind::Herdr]);
}
#[test]
fn owner_follows_bindings_and_survives_cycles() {
    let mut issues: BTreeMap<u64, IssueState> = BTreeMap::new();
    for (n, o) in [
        (1, None),
        (2, Some(1)),
        (3, Some(2)),
        (4, Some(5)),
        (5, Some(4)),
    ] {
        issues.insert(
            n,
            IssueState {
                number: n,
                shares_workspace_of: o,
                ..Default::default()
            },
        );
    }
    assert_eq!(owner_in(&issues, 3), 1);
    assert_eq!(owner_in(&issues, 1), 1);
    assert_eq!(owner_in(&issues, 9), 9, "unknown items own themselves");
    let cyclic = owner_in(&issues, 4);
    assert!(cyclic == 4 || cyclic == 5);
}
#[test]
fn origin_tag_binds_to_the_opening_session() {
    let mut e = engine();
    let r = repo();
    seeded(&mut e, 1, Some("bot/issue-1"), true);
    let tagged = issue(7, "bot", Some("<!-- ssf: origin=o/r#1 -->\n\nchild"));
    let scan = origin::scan(&tagged, &[], "bot");
    assert_eq!(e.find_owner(&r, &tagged, None, &scan), Some(1));
    // Through a chain: the PR was bound to the issue, a comment-opened
    // issue from the PR's session lands on the issue's session too.
    e.entry(&r, 7).seeded = true;
    e.entry(&r, 7).shares_workspace_of = Some(1);
    let grandchild = issue(8, "bot", Some("<!-- ssf: origin=o/r#7 -->"));
    let scan = origin::scan(&grandchild, &[], "bot");
    assert_eq!(e.find_owner(&r, &grandchild, None, &scan), Some(1));
    // A hand-off is not bound.
    let delegated = issue(9, "bot", Some("<!-- ssf: origin=o/r#1 mode=delegate -->"));
    let scan = origin::scan(&delegated, &[], "bot");
    assert_eq!(e.find_owner(&r, &delegated, None, &scan), None);
    assert!(scan.origin_tag.unwrap().is_delegate());
    // A human's body with a pasted tag is not an origin.
    let human = issue(10, "alice", Some("<!-- ssf: origin=o/r#1 -->"));
    let scan = origin::scan(&human, &[], "bot");
    assert_eq!(e.find_owner(&r, &human, None, &scan), None);
    // Another repository's session, or one ssf never tracked: no binding.
    let elsewhere = issue(11, "bot", Some("<!-- ssf: origin=x/y#1 -->"));
    let scan = origin::scan(&elsewhere, &[], "bot");
    assert_eq!(e.find_owner(&r, &elsewhere, None, &scan), None);
    let unknown = issue(12, "bot", Some("<!-- ssf: origin=o/r#99 -->"));
    let scan = origin::scan(&unknown, &[], "bot");
    assert_eq!(e.find_owner(&r, &unknown, None, &scan), None);
}
#[test]
fn pull_request_branch_binds_to_the_workspace_on_it() {
    let mut e = engine();
    let r = repo();
    seeded(&mut e, 1, Some("bot/fix"), false);
    seeded(&mut e, 2, Some("bot/fix"), true);
    seeded(&mut e, 3, Some("bot/other"), true);
    let untagged = issue(7, "bot", None);
    let scan = origin::scan(&untagged, &[], "bot");
    assert_eq!(
        e.find_owner(&r, &untagged, Some(&pr("bot/fix")), &scan),
        Some(2),
        "an active session beats a retired one"
    );
    assert_eq!(
        e.find_owner(&r, &untagged, Some(&pr("nobody")), &scan),
        None
    );
    assert_eq!(e.find_owner(&r, &untagged, None, &scan), None);
    // A retired session on the branch is still the owner (it gets
    // rehydrated) rather than duplicated.
    e.entry(&r, 2).active = false;
    e.entry(&r, 2).branch = None;
    assert_eq!(
        e.find_owner(&r, &untagged, Some(&pr("bot/fix")), &scan),
        Some(1)
    );
    // A fork's branch name means nothing here.
    let mut fork = pr("bot/fix");
    fork.head_repo = "someone/r".into();
    assert_eq!(e.find_owner(&r, &untagged, Some(&fork), &scan), None);
    // An unknown origin tag falls back to the branch.
    let tagged = issue(8, "bot", Some("<!-- ssf: origin=o/r#99 -->"));
    let scan = origin::scan(&tagged, &[], "bot");
    assert_eq!(
        e.find_owner(&r, &tagged, Some(&pr("bot/fix")), &scan),
        Some(1)
    );
}
#[test]
fn dependents_and_mirroring() {
    let mut e = engine();
    let r = repo();
    seeded(&mut e, 1, Some("bot/fix"), true);
    {
        let o = e.entry(&r, 1);
        o.worktree_id = Some("repo::/w/1".into());
        o.worktree_path = Some("/w/1".into());
        o.agent_session_id = Some("sess".into());
        o.terminal_handle = Some("h1".into());
    }
    seeded(&mut e, 2, None, true);
    e.entry(&r, 2).shares_workspace_of = Some(1);
    seeded(&mut e, 3, None, false);
    e.entry(&r, 3).shares_workspace_of = Some(1);
    assert_eq!(e.active_dependents(&r, 1), vec![2]);
    assert!(e.active_dependents(&r, 2).is_empty());
    assert_eq!(e.owner_of(&r, 2), 1);
    e.mirror_owner(&r, 2, 1);
    let c = e.entry(&r, 2).clone();
    assert_eq!(c.worktree_id.as_deref(), Some("repo::/w/1"));
    assert_eq!(c.branch.as_deref(), Some("refs/heads/bot/fix"));
    assert_eq!(c.agent_session_id.as_deref(), Some("sess"));
    assert_eq!(c.terminal_handle.as_deref(), Some("h1"));
}
#[test]
fn tagged_bot_comments_are_kept_and_sorted_per_recipient() {
    let mut e = engine();
    let r = repo();
    e.cfg.repos.push(r.clone());
    seeded(&mut e, 1, Some("bot/issue-1"), true);
    seeded(&mut e, 3, Some("bot/issue-3"), true);
    // PR 7 is owned by session 1.
    seeded(&mut e, 7, None, true);
    e.entry(&r, 7).shares_workspace_of = Some(1);
    let timeline = vec![
        comment(1, "alice", "human"),
        comment(2, "bot", "untagged bot comment"),
        comment(3, "bot", "<!-- ssf: origin=o/r#1 -->\n\nfrom one"),
        comment(4, "bot", "<!-- ssf: origin=o/r#3 -->\n\nfrom three"),
        comment(
            5,
            "bot",
            "<!-- ssf: origin=o/r#7 -->\n\nfrom the PR's session",
        ),
        comment(6, "bot", "<!-- ssf: origin=x/y#2 -->\n\nfrom elsewhere"),
        comment(
            8,
            "bot",
            "🤖 ssf <!-- ssf: origin=o/r#3 event=attached -->\n\n```ssf\nssf attaching agent to issue:\nharness: Claude Code\n```",
        ),
    ];
    let d = e.diff(&r, &BTreeMap::new(), &timeline);
    let keys: Vec<&str> = d.rendered.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "commented:1",
            "commented:2",
            "commented:3",
            "commented:4",
            "commented:5",
            "commented:6"
        ],
        "the untagged bot comment is a person's; tagged ones stay; the daemon's event post is nobody's"
    );
    assert_eq!(d.seen.len(), 7, "everything is recorded as seen");
    assert!(d.seen.contains_key("commented:8"));
    assert!(
        d.rendered[1]
            .text
            .contains("@bot commented (not from a session) (u2):\n  > untagged bot comment"),
        "{}",
        d.rendered[1].text
    );
    assert!(d.rendered[1].origin.is_none());
    assert!(d.rendered[2].text.contains("(from the agent on o/r#1)"));

    // Session 1 (which also acts on PR 7) does not get its own posts
    // back; the person's post reaches everyone.
    let mine = e.for_recipient(&d.rendered, "o/r#1");
    let keys: Vec<&str> = mine.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(
        keys,
        vec!["commented:1", "commented:2", "commented:4", "commented:6"]
    );
    // Session 3 sees session 1's (and the PR's) comments, not its own.
    let theirs = e.for_recipient(&d.rendered, "o/r#3");
    let keys: Vec<&str> = theirs.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "commented:1",
            "commented:2",
            "commented:3",
            "commented:5",
            "commented:6"
        ]
    );
    // Case-insensitive on the repository, like everything else.
    assert_eq!(e.for_recipient(&d.rendered, "O/R#3").len(), 5);
    assert_eq!(e.acting_session("o/r#7"), "o/r#1");
    assert_eq!(e.acting_session("x/y#2"), "x/y#2");
    assert_eq!(e.acting_session("garbage"), "garbage");
    e.cfg.daemon.include_own_events = true;
    assert_eq!(e.for_recipient(&d.rendered, "o/r#1").len(), 6);

    // The bot's commits and cross-references are still its own echo,
    // and a plain `gh` comment by the bot login is not.
    let timeline = vec![
        json!({"event":"cross-referenced","id":20,"actor":{"login":"bot"},"created_at":"t",
                "source":{"issue":{"title":"x","html_url":"u"}}}),
        json!({"event":"referenced","id":21,"actor":{"login":"bot"},"commit_id":"abc","created_at":"t"}),
        comment(22, "bot", "typed by hand as the bot"),
    ];
    e.cfg.daemon.include_own_events = false;
    let d = e.diff(&r, &BTreeMap::new(), &timeline);
    let keys: Vec<&str> = d.rendered.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, vec!["commented:22"]);
}
#[test]
fn startup_pass_looks_at_owning_active_sessions_only() {
    let mut e = engine();
    let r = repo();
    let bind = |e: &mut Engine, n: u64| {
        seeded(e, n, Some(&format!("b{n}")), true);
        e.entry(&r, n).worktree_id = Some(format!("repo::/w/{n}"));
    };
    bind(&mut e, 1); // owns its workspace: resumed
    bind(&mut e, 2); // owned by #1: its owner is the one to look at
    e.entry(&r, 2).shares_workspace_of = Some(1);
    bind(&mut e, 3); // closed, workspace released and about to go
    e.entry(&r, 3).active = false;
    e.entry(&r, 3).release_pending = true;
    bind(&mut e, 4); // retired but kept
    e.entry(&r, 4).active = false;
    seeded(&mut e, 5, None, true); // never got a workspace
    bind(&mut e, 6); // active, release approved after a reopen race
    e.entry(&r, 6).release_pending = true;
    e.entry(&r, 7).active = true; // not seeded yet
    e.entry(&r, 7).worktree_id = Some("repo::/w/7".into());
    // #11 closed while #12, bound to its workspace, is still open: the
    // workspace was kept for #12, and its harness is the one to start.
    bind(&mut e, 11);
    e.entry(&r, 11).active = false;
    bind(&mut e, 12);
    e.entry(&r, 12).shares_workspace_of = Some(11);
    // #13 closed with its workspace about to be released, even though
    // #14 still points at it: nothing to bring back. (The stale
    // close-time flag from an older daemon means the same.)
    bind(&mut e, 13);
    e.entry(&r, 13).active = false;
    e.entry(&r, 13).cleanup_pending = true;
    bind(&mut e, 14);
    e.entry(&r, 14).shares_workspace_of = Some(13);
    assert_eq!(e.resume_candidates(&r), vec![1, 11]);
    assert!(
        e.resume_candidates(&RepoConfig {
            name: "o/other".into(),
            ..Default::default()
        })
        .is_empty()
    );
}
#[test]
fn last_bot_comment_is_the_final_word() {
    let timeline = vec![
        json!({"event":"commented","id":1,"user":{"login":"bot"},"body":"first <!-- ssf: origin=o/r#5 -->","html_url":"u1"}),
        json!({"event":"commented","id":2,"user":{"login":"bot"},"body":"<!-- ssf: origin=o/r#5 -->\n\ndone","html_url":"u2"}),
        json!({"event":"commented","id":3,"user":{"login":"alice"},"body":"thanks","html_url":"u3"}),
        json!({"event":"closed","id":4,"actor":{"login":"alice"}}),
        // The daemon's own post after the agent's last word is not it.
        json!({"event":"commented","id":5,"user":{"login":"bot"},"body":"🤖 ssf <!-- ssf: origin=o/r#5 event=released -->\n\n```ssf\nssf releasing workspace of issue:\nby: ssf release\n```","html_url":"u5"}),
    ];
    let c = last_bot_comment(&timeline, "Bot").unwrap();
    assert_eq!(c.body, "done");
    assert_eq!(c.url, "u2");
    assert_eq!(c.session.as_deref(), Some("o/r#5"));
    assert_eq!(c.author, "bot");
    assert!(last_bot_comment(&timeline[2..], "bot").is_none());
}
#[test]
fn an_assignment_seen_after_a_created_only_pass_is_not_lost() {
    let mut e = engine();
    let r = repo();
    let created = vec!["created".to_string()];
    let both = vec!["assigned".to_string(), "created".to_string()];
    let i18 = issue(18, "bot", None);

    // Pass 1: first seen on the creator listing only; nothing binds it,
    // so onboarding leaves it ignored at this updated_at and listing.
    assert!(e.needs_look(&r, 18, Some(&i18), &created));
    e.state
        .repo_mut(&r.name)
        .ignored
        .insert(18, Ignored::new(&i18, &created));

    // Pass 2: the creator listing is a 304, or reports it unchanged.
    assert!(!e.needs_look(&r, 18, None, &created));
    assert!(!e.needs_look(&r, 18, Some(&i18), &created));
    // Being subscribed to meanwhile (tracked for subscribers only, no
    // session) changes nothing about that.
    e.entry(&r, 18).subscriber_only = true;
    assert!(!e.needs_look(&r, 18, Some(&i18), &created));

    // Pass 3: the assignee listing now carries it, with the very same
    // updated_at. That is a change for us: the item is looked at, and
    // with no session it is onboarded (with `assigned` in its
    // triggers, so it is not ignored again).
    assert!(e.needs_look(&r, 18, Some(&i18), &both));
    assert!(
        e.needs_look(&r, 18, None, &both),
        "a new listing membership counts even with every listing a 304"
    );
    assert!(!both.iter().all(|t| t == "created"));
    // The same listings in another order are the same listings.
    let reversed = vec!["created".to_string(), "assigned".to_string()];
    assert!(Ignored::new(&i18, &both).stands(Some(&i18), &reversed));
    // The other human triggers count the same way.
    for t in ["mentioned", "review_requested"] {
        assert!(e.needs_look(&r, 18, Some(&i18), &[t.into(), "created".into()]));
    }

    // A change on GitHub with the same listing re-evaluates as before.
    let mut later = i18.clone();
    later.updated_at = "y".into();
    assert!(e.needs_look(&r, 18, Some(&later), &created));

    // Once it has a session, only listings that changed matter, ignored
    // or not.
    seeded(&mut e, 18, None, true);
    assert!(e.needs_look(&r, 18, Some(&i18), &created));
    assert!(!e.needs_look(&r, 18, None, &both));
    // A retired session's item falls back to the ignore record, which
    // onboarding clears (`onboard` removes it before binding).
    e.entry(&r, 18).active = false;
    assert!(!e.needs_look(&r, 18, Some(&i18), &created));
    e.state.repo_mut(&r.name).ignored.remove(&18);
    assert!(e.needs_look(&r, 18, Some(&i18), &created));
}
#[tokio::test]
async fn engine_constructor_refuses_a_second_owner_before_auth_or_state_access() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut cfg = Config::default();
    cfg.github.api_url = stub.base.clone();
    cfg.github.token = Some("test-token".into());

    let first = Engine::new(cfg.clone()).await.unwrap();
    assert_eq!(stub.hits(), vec!["/user"]);
    let before = std::fs::read(crate::state::state_path()).unwrap();

    let err = match Engine::new(cfg.clone()).await {
        Ok(_) => panic!("a second engine acquired the same state directory"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("another ssf daemon is listening"));
    assert!(stub.hits().is_empty(), "the rejected engine called GitHub");
    assert_eq!(
        std::fs::read(crate::state::state_path()).unwrap(),
        before,
        "the rejected engine rewrote state"
    );

    drop(first);
    let second = Engine::new(cfg).await.unwrap();
    assert_eq!(stub.hits(), vec!["/user"]);
    drop(second);
}
#[tokio::test]
async fn engine_constructor_refuses_a_live_socket_before_auth_or_state_access() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut cfg = Config::default();
    cfg.github.api_url = stub.base.clone();
    cfg.github.token = Some("test-token".into());
    let path = crate::ipc::socket_path();
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let err = match Engine::new(cfg).await {
        Ok(_) => panic!("an engine started beside a legacy daemon socket"),
        Err(err) => err,
    };
    assert_eq!(
        err.to_string(),
        format!(
            "another ssf daemon is listening on {}; stop it first",
            path.display()
        )
    );
    assert!(stub.hits().is_empty(), "the refused engine called GitHub");
    assert!(
        !crate::state::state_path().exists(),
        "the refused engine created state"
    );
    drop(listener);
}

#[tokio::test]
async fn github_rename_repairs_config_state_and_historical_session_names() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    stub.set_identity("o/new-name");
    let mut e = engine_at(&stub.base);
    let mut r = repo();
    r.github_id = Some(1);
    e.cfg.repos.push(r.clone());

    let item = e.entry(&r, 7);
    item.seeded = true;
    item.active = true;
    item.origin = Some("o/r#3".into());
    item.delegated_by = Some("o/r#2".into());
    item.subscribers = vec!["o/r#4".into(), "other/repo#9".into()];
    e.failures.insert(("o/r".into(), 7), 2);

    e.identity_checked_at = None;
    e.reconcile_repo_identities(false).await;

    let repaired = &e.cfg.repos[0];
    assert_eq!(repaired.name, "o/new-name");
    assert_eq!(repaired.github_id, Some(1));
    assert!(repaired.matches_name("o/r"));
    assert!(!e.state.repos.contains_key("o/r"));
    let item = &e.state.repos["o/new-name"].issues[&7];
    assert_eq!(item.origin.as_deref(), Some("o/new-name#3"));
    assert_eq!(item.delegated_by.as_deref(), Some("o/new-name#2"));
    assert_eq!(item.subscribers, ["o/new-name#4", "other/repo#9"]);
    assert_eq!(e.failures.get(&("o/new-name".into(), 7)), Some(&2));
    assert_eq!(e.acting_session("o/r#7"), "o/new-name#7");

    let saved = Config::load().unwrap();
    assert_eq!(saved.repos[0].name, "o/new-name");
    assert!(saved.repos[0].matches_name("o/r"));
}

#[tokio::test]
async fn repository_identity_is_not_checked_on_every_issue_poll() {
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let mut r = repo();
    r.github_id = Some(1);
    e.cfg.repos.push(r);

    e.reconcile_repo_identities(false).await;

    assert!(stub.hits().is_empty());
}

#[tokio::test]
async fn first_identity_check_enrols_the_stable_github_id() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    e.cfg.repos.push(repo());

    e.identity_checked_at = None;
    e.reconcile_repo_identities(false).await;

    assert_eq!(e.cfg.repos[0].github_id, Some(1));
    assert_eq!(e.cfg.repos[0].name, "o/r");
    assert_eq!(stub.hits(), vec!["/repos/o/r"]);
}
