use super::*;

/// An item nobody has assigned or looked at: `ssf assign` is the way its
/// first session comes up on a stack of its own.
fn unassigned_item(number: u64, author: &str, updated_at: &str) -> Value {
    let mut item = assigned_item(number, author, updated_at);
    item["assignees"] = json!([]);
    item
}

/// The engine side of `ssf assign` on an issue nobody has touched: the
/// overrides are written in the same request as the GitHub assignment, so
/// the pass that follows onboards the item on that stack.
#[tokio::test]
async fn an_untouched_item_is_assigned_and_onboards_on_the_chosen_stack() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    stub.set_issue(7, unassigned_item(7, "alice", "u1"));
    stub.set_timeline(7, vec![assigned_by(1, "alice")]);

    let resp = e
        .handle_request(Request::Assign {
            item: "o/r#7".into(),
            harness: "pi".into(),
            model: Some("openrouter/anthropic/claude-sonnet-4".into()),
            effort: Some("high".into()),
            by: Some("o/r#1".into()),
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(
        stub.assignments(),
        vec![(
            "/repos/o/r/issues/7/assignees".to_string(),
            "bot".to_string()
        )]
    );
    assert_eq!(
        e.entry(&r, 7).overrides,
        Some(Overrides {
            harness: "pi".into(),
            model: Some("openrouter/anthropic/claude-sonnet-4".into()),
            effort: Some("high".into()),
        })
    );
    // `--json` carries what `ssf handover`'s does, plus whether the
    // assignment happened and whether anything was pinned.
    let v = &resp.data;
    assert_eq!(v["session"], "o/r#7");
    assert_eq!(v["title"], "t");
    assert_eq!(v["assigned"], true);
    assert_eq!(v["overrides_written"], true);
    assert_eq!(v["from"]["harness"], "claude");
    assert_eq!(v["to"]["harness"], "pi");
    assert_eq!(v["to"]["model"], "openrouter/anthropic/claude-sonnet-4");

    // The ordinary pass onboards it, on the stack the overrides name: the
    // launch log and the `attached` post both say so.
    stub.set_assigned(vec![assigned_item(7, "alice", "u2")]);
    e.tick_repo(&r).await.unwrap();
    assert!(e.failures.is_empty(), "{:?}", e.failures);
    let launched = d.launches();
    assert_eq!(launched.len(), 1, "{launched:?}");
    assert!(
        launched[0].starts_with("pi:"),
        "started on the item's own harness: {launched:?}"
    );
    assert!(
        launched[0].contains("--model openrouter/anthropic/claude-sonnet-4"),
        "with the model it was given: {launched:?}"
    );
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert!(
        posts[0].1.contains("harness: Pi")
            && posts[0]
                .1
                .contains("model: openrouter/anthropic/claude-sonnet-4")
            && posts[0].1.contains("effort: high"),
        "{posts:?}"
    );
}

/// The row the workaround used to need: the bot is already assigned, the
/// daemon has not onboarded the item yet, and the stack is written now so
/// that onboarding comes up on it.
#[tokio::test]
async fn an_item_already_assigned_to_the_bot_still_gets_its_stack_written() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    stub.set_issue(7, assigned_item(7, "alice", "u1"));

    let resp = e
        .handle_request(Request::Assign {
            item: "o/r#7".into(),
            harness: "pi".into(),
            model: None,
            effort: None,
            by: None,
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.data["assigned"], false, "the bot is on it already");
    assert_eq!(resp.data["overrides_written"], true);
    assert!(stub.assignments().is_empty(), "nothing to assign");
    assert_eq!(
        e.entry(&r, 7).overrides,
        Some(Overrides {
            harness: "pi".into(),
            model: None,
            effort: None,
        })
    );
}

/// A stack equal to the one the item would run anyway is not pinned: the
/// assignment happens, the item keeps following `ssf repo set`.
#[tokio::test]
async fn a_stack_the_repository_already_runs_is_assigned_without_overrides() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let mut r = repo();
    r.model = Some("opus".into());
    r.effort = Some("high".into());
    e.cfg.repos = vec![r.clone()];
    stub.set_issue(7, unassigned_item(7, "alice", "u1"));

    let resp = e
        .handle_request(Request::Assign {
            item: "o/r#7".into(),
            harness: "claude".into(),
            model: Some("opus".into()),
            effort: Some("high".into()),
            by: None,
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.data["assigned"], true);
    assert_eq!(resp.data["overrides_written"], false);
    assert_eq!(stub.assignments().len(), 1);
    assert!(e.peek(&r, 7).is_none(), "no record, nothing written");
}

/// Every seat the item can already be in is a refusal naming the command
/// that does act on it, and nothing is assigned or written.
#[tokio::test]
async fn a_seat_on_the_item_is_refused_with_the_command_that_moves_it() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, _d) = handover_setup(&stub);
    let r = repo();

    // A session with a workspace.
    let refused = |resp: crate::ipc::Response| resp.error.unwrap_or_default();
    let seat = e
        .handle_request(Request::Assign {
            item: "o/r#5".into(),
            harness: "pi".into(),
            model: None,
            effort: None,
            by: None,
        })
        .await;
    assert!(!seat.ok);
    let why = refused(seat);
    assert!(why.contains("already has a session"), "{why}");
    assert!(why.contains("ssf handover o/r#5"), "{why}");

    // A handover on its way to being carried out.
    e.handover("o/r#5", "codex", None, None, None, None)
        .await
        .unwrap();
    let pending = e
        .handle_request(Request::Assign {
            item: "o/r#5".into(),
            harness: "pi".into(),
            model: None,
            effort: None,
            by: None,
        })
        .await;
    assert!(!pending.ok);
    let why = refused(pending);
    assert!(
        why.contains("a handover to codex is already pending"),
        "{why}"
    );
    assert!(why.contains("--cancel"), "{why}");
    e.entry(&r, 5).handover = None;

    // A release on its way.
    e.entry(&r, 5).release_pending = true;
    let pending = e
        .handle_request(Request::Assign {
            item: "o/r#5".into(),
            harness: "pi".into(),
            model: None,
            effort: None,
            by: None,
        })
        .await;
    assert!(!pending.ok);
    let why = refused(pending);
    assert!(why.contains("a release of this item's workspace"), "{why}");
    e.entry(&r, 5).release_pending = false;

    // An item bound to another item's session: its overrides would be
    // inert, and the session to hand over is the owner.
    seeded(&mut e, 6, Some("bot/issue-6"), false);
    e.entry(&r, 6).shares_workspace_of = Some(5);
    stub.set_issue(6, unassigned_item(6, "alice", "u1"));
    let bound = e
        .handle_request(Request::Assign {
            item: "o/r#6".into(),
            harness: "pi".into(),
            model: None,
            effort: None,
            by: None,
        })
        .await;
    assert!(!bound.ok);
    let why = refused(bound);
    assert!(why.contains("is worked by o/r#5"), "{why}");
    assert!(why.contains("ssf handover o/r#5"), "{why}");
    assert!(e.entry(&r, 6).overrides.is_none());
    assert!(stub.assignments().is_empty(), "nothing was assigned");
}

/// A bound item's owner may have no session either (it retired), and then
/// `ssf handover` on it is refused for having nothing to hand over: the
/// refusal has to name the command that does work.
#[tokio::test]
async fn a_bound_item_whose_owner_has_no_session_is_told_to_assign_it() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    // The owner is retired: no workspace, nothing running.
    seeded(&mut e, 5, Some("bot/issue-5"), false);
    seeded(&mut e, 6, Some("bot/issue-6"), false);
    e.entry(&r, 6).shares_workspace_of = Some(5);
    stub.set_issue(5, unassigned_item(5, "alice", "u1"));
    stub.set_issue(6, unassigned_item(6, "alice", "u1"));

    let resp = e
        .handle_request(Request::Assign {
            item: "o/r#6".into(),
            harness: "pi".into(),
            model: None,
            effort: None,
            by: None,
        })
        .await;
    assert!(!resp.ok);
    let why = resp.error.unwrap_or_default();
    assert!(why.contains("is worked by o/r#5"), "{why}");
    assert!(
        why.contains("ssf assign o/r#5 --harness pi"),
        "the owner has no session, so handover would refuse: {why}"
    );
    // Which is what actually moves the bound item onto the stack: the
    // owner takes it, and the bound item follows the owner's overrides.
    let ok = e
        .handle_request(Request::Assign {
            item: "o/r#5".into(),
            harness: "pi".into(),
            model: None,
            effort: None,
            by: None,
        })
        .await;
    assert!(ok.ok, "{:?}", ok.error);
    assert_eq!(e.effective(&r, 6).harness, "pi", "the bound item follows");
    assert!(e.entry(&r, 6).overrides.is_none(), "written on the owner");
}

/// A pull request the daemon would bind to another session's branch is a
/// refusal too, even though its own record says nothing of the sort yet.
#[tokio::test]
async fn a_pull_request_on_another_sessions_branch_is_refused() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    seeded(&mut e, 5, Some("bot/issue-5"), true);

    let mut pull = unassigned_item(9, "bot", "u1");
    pull["pull_request"] = json!({"url": "https://api.github.test/pulls/9"});
    pull["head"] = json!({"ref": "bot/issue-5", "repo": {"full_name": "o/r"}});
    stub.set_issue(9, pull);
    stub.set_timeline(9, vec![]);
    stub.set_pull(
        9,
        json!({"number": 9, "head": {"ref": "bot/issue-5", "repo": {"full_name": "o/r"}}}),
    );

    let resp = e
        .handle_request(Request::Assign {
            item: "o/r#9".into(),
            harness: "pi".into(),
            model: None,
            effort: None,
            by: None,
        })
        .await;
    assert!(!resp.ok);
    let why = resp.error.unwrap_or_default();
    assert!(why.contains("is worked by o/r#5"), "{why}");
    assert!(stub.assignments().is_empty(), "nothing was assigned");

    // A `mode=delegate` item, by contrast, was handed off to be worked and
    // wants a session of its own.
    let mut delegated = unassigned_item(9, "bot", "u1");
    delegated["pull_request"] = json!({"url": "https://api.github.test/pulls/9"});
    delegated["body"] = json!("🤖#5 says: <!-- ssf: origin=o/r#5 mode=delegate -->\n\nwork it");
    stub.set_issue(9, delegated);
    let ok = e
        .handle_request(Request::Assign {
            item: "o/r#9".into(),
            harness: "pi".into(),
            model: None,
            effort: None,
            by: None,
        })
        .await;
    assert!(ok.ok, "{:?}", ok.error);
    assert_eq!(stub.assignments().len(), 1);
    assert_eq!(
        e.entry(&r, 9)
            .overrides
            .as_ref()
            .map(|o| o.harness.as_str()),
        Some("pi")
    );
}

/// Every way the stack itself can be wrong: refused with nothing assigned
/// and nothing written.
#[tokio::test]
async fn a_stack_that_cannot_be_run_is_refused_before_anything_is_written() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    stub.set_issue(7, unassigned_item(7, "alice", "u1"));
    let assign = |harness: &str, model: Option<&str>, effort: Option<&str>| Request::Assign {
        item: "o/r#7".into(),
        harness: harness.into(),
        model: model.map(str::to_string),
        effort: effort.map(str::to_string),
        by: None,
    };

    // An item on a repository ssf does not watch.
    let unlisted = e
        .handle_request(Request::Assign {
            item: "x/y#7".into(),
            harness: "pi".into(),
            model: None,
            effort: None,
            by: None,
        })
        .await;
    assert!(!unlisted.ok);
    assert!(
        unlisted
            .error
            .unwrap_or_default()
            .contains("not a watched repository"),
        "refused by name"
    );

    // A harness ssf does not know, one that is not installed, and one that
    // is installed but not signed in.
    let unknown = e.handle_request(assign("nope", None, None)).await;
    assert!(!unknown.ok);
    assert!(
        unknown
            .error
            .unwrap_or_default()
            .contains("not a harness ssf")
    );
    e.installed = std::sync::Arc::new(|_| false);
    let missing = e.handle_request(assign("pi", None, None)).await;
    assert!(!missing.ok);
    assert!(
        missing
            .error
            .unwrap_or_default()
            .contains("is not installed where the daemon runs")
    );
    e.installed = std::sync::Arc::new(|_| true);
    probe_returning(&mut e, LoginState::SignedOut, None);
    let signed_out = e.handle_request(assign("pi", None, None)).await;
    assert!(!signed_out.ok);
    assert!(
        signed_out
            .error
            .unwrap_or_default()
            .contains("is not signed in here")
    );
    probe_returning(&mut e, LoginState::SignedIn, None);

    // A model or effort the harness does not take.
    let bad_model = e.handle_request(assign("crush", Some("opus"), None)).await;
    assert!(!bad_model.ok);
    assert!(
        bad_model
            .error
            .unwrap_or_default()
            .contains("does not take a model setting")
    );
    let bad_effort = e.handle_request(assign("pi", None, Some("enormous"))).await;
    assert!(!bad_effort.ok);
    assert!(
        bad_effort
            .error
            .unwrap_or_default()
            .contains("is not a level pi accepts")
    );
    assert!(stub.assignments().is_empty(), "nothing was assigned");
    assert!(e.peek(&r, 7).is_none(), "nothing was written");
}

/// Which command put the item on its stack is recorded rather than
/// inferred, so an assignment that replaces a handover's overrides is not
/// still read as a handover (and the reverse).
#[tokio::test]
async fn the_stack_records_which_command_wrote_it() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    let r = repo();

    // Item 5 is handed over to codex, and then retires with its workspace
    // kept: overrides and the handover's stamp are both still on it.
    e.handover("o/r#5", "codex", None, None, Some("half done"), None)
        .await
        .unwrap();
    e.run_handovers(&r).await;
    let st = e.entry(&r, 5).clone();
    assert_eq!(
        st.overrides.as_ref().map(|o| o.harness.as_str()),
        Some("codex")
    );
    assert!(st.handed_over_at.is_some());
    assert!(st.assigned_at.is_none());
    e.entry(&r, 5).active = false;
    e.entry(&r, 5).terminal_handle = None;

    // `ssf assign` replaces them: the item is on pi because it was
    // assigned, which `assigned_at` says. `handed_over_at` is left where
    // it is: it is what bounds the transcript capture window, and the
    // workspace still holds the transcript the handover left.
    let ok = e
        .handle_request(Request::Assign {
            item: "o/r#5".into(),
            harness: "pi".into(),
            model: None,
            effort: None,
            by: None,
        })
        .await;
    assert!(ok.ok, "{:?}", ok.error);
    let st = e.entry(&r, 5).clone();
    assert_eq!(
        st.overrides.as_ref().map(|o| o.harness.as_str()),
        Some("pi")
    );
    assert!(st.assigned_at.is_some(), "assigned, which is the point");
    assert!(
        st.handed_over_at.is_some(),
        "the capture window is untouched"
    );

    // The item comes back (a human reassigns it, or the pass sees new
    // activity), and a handover to codex takes the record over again:
    // the stamp follows the last writer.
    e.entry(&r, 5).active = true;
    let st = e.entry(&r, 5).clone();
    d.seed(
        &format!("w{}", st.number),
        &format!("t{}", st.number),
        READY_SCREEN,
    );
    e.entry(&r, 5).terminal_handle = Some("t5".into());
    e.handover("o/r#5", "codex", None, None, Some("and back"), None)
        .await
        .unwrap();
    e.run_handovers(&r).await;
    let st = e.entry(&r, 5).clone();
    assert_eq!(
        st.overrides.as_ref().map(|o| o.harness.as_str()),
        Some("codex")
    );
    assert!(st.handed_over_at.is_some());
    assert!(st.assigned_at.is_none(), "the handover is the last writer");
}

/// The item a closed issue is assigned on GitHub starts nothing: no pass
/// onboards a closed item (`state=open` listings), so the answer says the
/// stack waits rather than promising a session within the poll interval.
#[tokio::test]
async fn an_assignment_on_a_closed_item_says_no_session_starts_yet() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    let mut closed = unassigned_item(7, "alice", "u1");
    closed["state"] = json!("closed");
    stub.set_issue(7, closed);

    let resp = e
        .handle_request(Request::Assign {
            item: "o/r#7".into(),
            harness: "pi".into(),
            model: None,
            effort: None,
            by: None,
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.data["open"], false);
    assert_eq!(resp.data["assigned"], true);
    assert_eq!(stub.assignments().len(), 1, "still assigned on GitHub");
    assert_eq!(
        e.entry(&r, 7)
            .overrides
            .as_ref()
            .map(|o| o.harness.as_str()),
        Some("pi"),
        "and the stack is ready on the item"
    );
    // The record the request creates carries what the item is, so `ssf
    // status` does not show an untitled row before the next pass: not
    // that a closed item ever gets one.
    let st = e.entry(&r, 7).clone();
    assert_eq!(st.title, "t");
    assert_eq!(st.kind.as_deref(), Some("issue"));
    assert_eq!(st.github_state.as_deref(), Some("closed"));
}

/// An item bound to the session being re-pinned mirrors its conversation
/// id, so the retired one goes from that record too: `sf status` would
/// otherwise show a bound row holding a conversation of the harness the
/// item left.
#[tokio::test]
async fn reassigning_a_harness_retires_the_conversation_bound_items_mirror() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    // #5 retired with its workspace kept and a conversation on it; #6 is
    // bound to it and mirrors that conversation.
    seeded(&mut e, 5, Some("bot/issue-5"), false);
    e.entry(&r, 5).worktree_id = Some("w5".into());
    e.entry(&r, 5).worktree_path = Some("/w/5".into());
    e.entry(&r, 5).agent_session_id = Some("sess-5".into());
    seeded(&mut e, 6, Some("bot/issue-6"), false);
    e.entry(&r, 6).shares_workspace_of = Some(5);
    e.entry(&r, 6).agent_session_id = Some("sess-5".into());
    stub.set_issue(5, unassigned_item(5, "alice", "u1"));

    let resp = e
        .handle_request(Request::Assign {
            item: "o/r#5".into(),
            harness: "pi".into(),
            model: None,
            effort: None,
            by: None,
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    let bound = e.entry(&r, 6).clone();
    assert!(bound.agent_session_id.is_none(), "the mirror is cleared");
    assert!(
        bound.retired_session_ids.contains(&"sess-5".to_string()),
        "and the id is never captured again: {:?}",
        bound.retired_session_ids
    );
}

/// A retired item whose workspace is kept comes back on the assigned
/// stack, and the conversation the old harness left is not resumed by the
/// new one.
#[tokio::test]
async fn a_reassigned_item_drops_the_conversation_of_the_harness_it_left() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let d = crate::driver::StubDriver::new(DriverKind::Herdr);
    e.drivers = Drivers::from_list(vec![Driver::Stub(d.clone())]);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    seeded(&mut e, 7, Some("bot/issue-7"), false);
    {
        let st = e.entry(&r, 7);
        st.title = "Fix the widget".into();
        st.html_url = "https://gh/7".into();
        st.worktree_id = Some("w7".into());
        st.worktree_path = Some("/w/7".into());
        st.terminal_handle = Some("t7".into());
        st.agent_session_id = Some("sess-7".into());
    }
    stub.set_issue(7, unassigned_item(7, "alice", "u1"));

    let resp = e
        .handle_request(Request::Assign {
            item: "o/r#7".into(),
            harness: "pi".into(),
            model: None,
            effort: None,
            by: None,
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    let st = e.entry(&r, 7).clone();
    assert!(st.agent_session_id.is_none(), "the old id is gone");
    assert!(
        st.retired_session_ids.contains(&"sess-7".to_string()),
        "{:?}",
        st.retired_session_ids
    );
    assert_eq!(
        st.overrides.as_ref().map(|o| o.harness.as_str()),
        Some("pi")
    );
    // A handover is still the only writer that stamps `handed_over_at`,
    // and an assignment stamps its own field, which is what tells the two
    // apart in `ssf status`/`ssf peers`.
    assert!(st.handed_over_at.is_none(), "assigned, not handed over");
    assert!(st.assigned_at.is_some(), "the assignment recorded itself");
}

/// The same harness with another model keeps the conversation: there is
/// nothing about it the harness cannot resume.
#[tokio::test]
async fn reassigning_the_same_harness_keeps_its_conversation() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let mut e = engine_at(&stub.base);
    let r = repo();
    e.cfg.repos = vec![r.clone()];
    seeded(&mut e, 7, Some("bot/issue-7"), false);
    e.entry(&r, 7).agent_session_id = Some("sess-7".into());
    stub.set_issue(7, unassigned_item(7, "alice", "u1"));

    let resp = e
        .handle_request(Request::Assign {
            item: "o/r#7".into(),
            harness: "claude".into(),
            model: Some("opus".into()),
            effort: None,
            by: None,
        })
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    let st = e.entry(&r, 7).clone();
    assert_eq!(st.agent_session_id.as_deref(), Some("sess-7"));
    assert!(st.retired_session_ids.is_empty());
    assert_eq!(st.overrides.and_then(|o| o.model), Some("opus".into()));
}
