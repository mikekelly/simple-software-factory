use super::*;
use crate::driver::AgentInfo;
use crate::state::RepoState;

fn cfg() -> Config {
    let mut cfg = Config::default();
    cfg.repos.push(RepoConfig {
        name: "acme/widgets".into(),
        harness: "claude".into(),
        ..Default::default()
    });
    cfg
}

fn item(number: u64, worktree_id: Option<&str>) -> IssueState {
    IssueState {
        number,
        title: format!("Item {number}"),
        html_url: format!("https://github.com/acme/widgets/issues/{number}"),
        worktree_id: worktree_id.map(str::to_string),
        repo_id: Some("r1".into()),
        branch: Some("refs/heads/bot/issue-1".into()),
        active: true,
        kind: Some("issue".into()),
        github_state: None,
        triggers: vec!["assigned".into()],
        agent_session_id: Some("sess-1".into()),
        prompts_sent: 2,
        ..Default::default()
    }
}

fn state_with(items: Vec<IssueState>) -> State {
    let mut st = State::default();
    let mut rs = RepoState::default();
    for i in items {
        rs.issues.insert(i.number, i);
    }
    st.repos.insert("acme/widgets".into(), rs);
    st
}

fn workspace(id: &str, issue: Option<u64>, agent: Option<AgentInfo>) -> WorkspaceInfo {
    WorkspaceInfo {
        worktree_id: id.into(),
        repo_id: "r1".into(),
        path: format!("/w/{id}"),
        branch: Some("bot/issue-1".into()),
        column: Some("in-progress".into()),
        linked_issue: issue,
        last_activity_at: Some("2026-09-03T11:00:00Z".into()),
        agents: agent.into_iter().collect(),
        ..Default::default()
    }
}

/// An item whose retirement is held reports it, so an operator who
/// meets a refusal from `ssf release` has something that explains it.
#[test]
fn a_held_retirement_is_reported() {
    let mut held = item(1, Some("r1::/w/one"));
    held.retirement_held_at = Some("2026-09-08T14:30:55Z".into());
    let st = state_with(vec![held, item(2, Some("r1::/w/two"))]);
    let s = sessions(&cfg(), &st, None);
    assert_eq!(
        s[0].retirement_held_at.as_deref(),
        Some("2026-09-08T14:30:55Z")
    );
    assert!(s[1].retirement_held_at.is_none());
    // Both are omitted from the JSON entirely when there is no hold.
    let json = serde_json::to_string(&s[1]).unwrap();
    assert!(!json.contains("retirement_held"), "{json}");
}

#[test]
fn joins_by_worktree_id() {
    let st = state_with(vec![item(1, Some("r1::/w/one"))]);
    let ws = vec![workspace(
        "r1::/w/one",
        None,
        Some(AgentInfo {
            state: "working".into(),
            tool_name: Some("Bash".into()),
            tool_input: Some("cargo test\n--all".into()),
            last_assistant_message: Some("Running the tests now.".into()),
            ..Default::default()
        }),
    )];
    let s = sessions(&cfg(), &st, Some(&ws));
    assert_eq!(s.len(), 1);
    let s = &s[0];
    assert_eq!(s.id, "acme/widgets#1");
    assert_eq!(s.owner, "acme/widgets#1");
    assert_eq!(s.agent_state, "working");
    assert_eq!(s.tool.as_deref(), Some("Bash: cargo test"));
    assert_eq!(
        s.last_assistant_message.as_deref(),
        Some("Running the tests now.")
    );
    assert_eq!(s.branch.as_deref(), Some("bot/issue-1"));
    assert_eq!(s.column.as_deref(), Some("in-progress"));
    assert_eq!(s.last_activity_at.as_deref(), Some("2026-09-03T11:00:00Z"));
    assert_eq!(s.worktree_path.as_deref(), Some("/w/r1::/w/one"));
    assert_eq!(s.github_state, "open");
    assert!(s.workspace.is_some());
}

/// The branch the model carries is a plain name whatever the driver reported:
/// a workspace's own branch is a full ref, and every consumer of the model --
/// the terminal, the web cards, the overlay's Details -- shows the name.
#[test]
fn a_workspace_branch_loses_its_ref_prefix() {
    let st = state_with(vec![item(1, Some("r1::/w/one"))]);
    let mut ws = workspace("r1::/w/one", None, None);
    ws.branch = Some("refs/heads/bot/issue-1".into());
    let s = sessions(&cfg(), &st, Some(&[ws]));
    assert_eq!(s[0].branch.as_deref(), Some("bot/issue-1"));
}

#[test]
fn falls_back_to_driver_link_when_binding_is_stale() {
    let st = state_with(vec![item(7, Some("r1::/w/gone"))]);
    let ws = vec![workspace("r1::/w/seven", Some(7), None)];
    let s = sessions(&cfg(), &st, Some(&ws));
    assert_eq!(s[0].agent_state, "no-agent");
    assert_eq!(s[0].worktree_path.as_deref(), Some("/w/r1::/w/seven"));
}

#[test]
fn reports_missing_workspace_and_unavailable_driver() {
    let st = state_with(vec![item(1, Some("r1::/w/one")), item(2, None)]);
    let s = sessions(&cfg(), &st, Some(&[]));
    assert_eq!(s[0].agent_state, "no-workspace");
    assert_eq!(s[1].agent_state, "unbound");
    // Without the driver, the binding's own branch still shows without the ref prefix.
    let s = sessions(&cfg(), &st, None);
    assert_eq!(s[0].agent_state, "unknown");
    assert_eq!(s[0].branch.as_deref(), Some("bot/issue-1"));
    assert!(s[0].workspace.is_none());
    // Recorded state wins; without it an active item is open, a retired one unknown.
    let mut retired = item(3, None);
    retired.active = false;
    let mut merged = item(4, None);
    merged.github_state = Some("merged".into());
    let st = state_with(vec![retired, merged]);
    let s = sessions(&cfg(), &st, None);
    assert_eq!(s[0].github_state, "unknown");
    assert_eq!(s[1].github_state, "merged");
}

#[test]
fn pr_joining_an_issue_workspace_is_owned_by_that_session() {
    let mut pr = item(9, Some("r1::/w/one"));
    pr.kind = Some("pull_request".into());
    pr.shares_workspace_of = Some(1);
    let st = state_with(vec![item(1, Some("r1::/w/one")), pr]);
    let s = sessions(&cfg(), &st, Some(&[]));
    let pr = s.iter().find(|s| s.number == 9).unwrap();
    assert_eq!(pr.owner, "acme/widgets#1");
    assert_eq!(pr.shares_workspace_of.as_deref(), Some("acme/widgets#1"));
    assert!(pr.is_pull_request());
}

#[test]
fn json_keeps_the_issue_fields() {
    let _sandbox = crate::config::test_support::sandbox();
    let mut cfg = cfg();
    cfg.driver = Some(DriverKind::Herdr);
    let snap = Snapshot {
        cfg,
        state: state_with(vec![item(1, Some("r1::/w/one"))]),
        workspaces: Vec::new(),
        down: vec![DriverKind::Herdr],
        errors: vec!["not running".into()],
    };
    let v = snap.to_json();
    assert_eq!(v["driver"]["available"], false);
    assert_eq!(v["driver"]["error"], "not running");
    let issue = &v["repos"][0]["issues"][0];
    for key in [
        "number",
        "title",
        "url",
        "active",
        "worktree_id",
        "prompts_sent",
    ] {
        assert!(!issue[key].is_null(), "{key} missing");
    }
    assert_eq!(v["sessions"][0]["agent_state"], "unknown");
    assert_eq!(v["sessions"][0]["subscribers"], json!([]));
    assert_eq!(v["sessions"][0]["untagged_posts"], 0);
}

#[test]
fn bot_identity_uses_config_before_start_and_cache_with_a_token() {
    let _sandbox = crate::config::test_support::sandbox();
    let mut cfg = cfg();
    cfg.github.login = Some("configured-bot".into());
    cfg.github.token = Some("configured-token".into());
    let mut state = State::default();
    let snap = Snapshot {
        cfg: cfg.clone(),
        state: state.clone(),
        workspaces: Vec::new(),
        down: Vec::new(),
        errors: Vec::new(),
    };
    assert_eq!(snap.bot_login(), Some("configured-bot"));
    assert_eq!(snap.to_json()["bot_login"], "configured-bot");
    assert!(render_status(&snap).contains("bot:     configured-bot"));

    // A token-only setup has no configured login; retain the daemon's
    // actual identity while its credential remains available.
    cfg.github.login = None;
    state.bot_login = Some("daemon-bot".into());
    let snap = Snapshot {
        cfg,
        state,
        workspaces: Vec::new(),
        down: Vec::new(),
        errors: Vec::new(),
    };
    assert_eq!(snap.bot_login(), Some("daemon-bot"));
    assert_eq!(snap.to_json()["bot_login"], "daemon-bot");
}

#[test]
fn origin_tags_are_summarised_per_session() {
    let mut it = item(1, None);
    it.origin = Some("acme/widgets#9".into());
    it.origins.insert("c1".into(), "acme/widgets#9".into());
    it.origins.insert("c2".into(), "acme/widgets#9".into());
    it.origins.insert("c3".into(), "acme/widgets#4".into());
    it.untagged.insert("c4".into(), "https://x".into());
    let st = state_with(vec![it]);
    let s = sessions(&cfg(), &st, None);
    assert_eq!(s[0].origin.as_deref(), Some("acme/widgets#9"));
    assert_eq!(s[0].posts_by_session["acme/widgets#9"], 2);
    assert_eq!(s[0].posts_by_session["acme/widgets#4"], 1);
    assert_eq!(s[0].untagged_posts, 1);
    assert!(render_peers(&s, None).contains("opened by acme/widgets#9"));
}

#[test]
fn peers_table_lists_each_session_with_its_facts() {
    let st = state_with(vec![item(1, Some("r1::/w/one"))]);
    let ws = vec![workspace(
        "r1::/w/one",
        None,
        Some(AgentInfo {
            state: "done".into(),
            last_assistant_message: Some("Opened PR #2.\nDone.".into()),
            ..Default::default()
        }),
    )];
    let s = sessions(&cfg(), &st, Some(&ws));
    let text = render_peers(&s, Some("acme/widgets#1"));
    assert!(text.starts_with("acme/widgets\n"));
    // herdr's `done` is an agent that ended its turn: shown as idle (#510).
    assert!(text.contains("#1     issue open    idle"), "{text}");
    assert!(text.contains("Item 1 (you)"), "{text}");
    assert!(
        text.contains("branch bot/issue-1  ·  column in-progress  ·  via assigned  ·  prompts 2"),
        "{text}"
    );
    assert!(text.contains("said: Opened PR #2. Done."), "{text}");
}

#[test]
fn a_blocked_session_is_flagged_everywhere() {
    let _sandbox = crate::config::test_support::sandbox();
    let mut it = item(1, Some("r1::/w/one"));
    it.blocked = Some(Blocked {
        reason: "login".into(),
        harness: "claude".into(),
        detail: "Login expired · Please run /login".into(),
        since: "2026-09-06T14:30:00Z".into(),
        reported: true,
        credential: None,
        retried_at: None,
        retries: 0,
        told_at: None,
        tell_failures: 0,
        first_message_owed: false,
    });
    let st = state_with(vec![it, item(2, None)]);
    let s = sessions(&cfg(), &st, Some(&[]));
    let b = s[0].blocked.as_ref().unwrap();
    assert_eq!(b.reason, "login");
    assert!(b.fix.contains("claude, sign in"), "{}", b.fix);
    assert_eq!(b.harness_name, "Claude Code");
    assert!(
        b.describe()
            .starts_with("Claude Code at its sign-in prompt since"),
        "{}",
        b.describe()
    );
    assert!(s[1].blocked.is_none());
    let table = render_peers(&s, None);
    assert!(
        table.contains("BLOCKED: Claude Code at its sign-in prompt"),
        "{table}"
    );
    let snap = Snapshot {
        cfg: cfg(),
        state: st,
        workspaces: Vec::new(),
        down: Vec::new(),
        errors: Vec::new(),
    };
    let v = snap.to_json();
    assert_eq!(v["blocked_sessions"], json!(["acme/widgets#1"]));
    assert_eq!(v["sessions"][0]["blocked"]["harness"], "claude");
    assert!(v["sessions"][1]["blocked"].is_null());
    let text = render_status(&snap);
    assert!(
        text.contains("BLOCKED: acme/widgets#1: Claude Code at its sign-in prompt"),
        "{text}"
    );
    // The other reason: the harness never came up (a handover to a
    // harness that exits as it is launched).
    let mut it = item(1, Some("r1::/w/one"));
    it.blocked = Some(Blocked {
        reason: Blocked::START.into(),
        harness: "pi".into(),
        detail: "pi exited at once: ambiguous model".into(),
        since: "2026-09-06T14:30:00Z".into(),
        reported: true,
        ..Default::default()
    });
    let s = sessions(&cfg(), &state_with(vec![it]), Some(&[]));
    let b = s[0].blocked.as_ref().unwrap();
    assert!(b.fix.contains("start Pi by hand"), "{}", b.fix);
    assert!(
        b.describe().starts_with("Pi could not be started since"),
        "{}",
        b.describe()
    );
    assert!(
        render_peers(&s, None).contains("BLOCKED: Pi could not be started since"),
        "{}",
        render_peers(&s, None)
    );
}

#[test]
fn a_handed_over_session_shows_its_own_harness_and_a_pending_handover() {
    let _sandbox = crate::config::test_support::sandbox();
    // #1 has been handed over to Pi; #2 shares its workspace, so it
    // runs the same harness; #3 has a handover waiting for the pass.
    let mut one = item(1, Some("r1::/w/one"));
    one.overrides = Some(Overrides {
        harness: "pi".into(),
        model: Some("openai/gpt-6".into()),
        effort: Some("high".into()),
    });
    // A handover is the writer that stamps the item; an assignment does
    // not (see `an_assigned_session_shows_the_stack_it_was_given`).
    one.handed_over_at = Some("2026-09-07T09:00:00Z".into());
    // The harness it went to never read what the outgoing session
    // left: the note waits on the item for the one that does.
    one.handover_note = Some(HandoverNote {
        from: "Claude Code".into(),
        summary: Some("half migrated".into()),
    });
    let mut two = item(2, Some("r1::/w/one"));
    two.shares_workspace_of = Some(1);
    // Bound to the bound item: the chain leads to #1 all the same.
    let mut four = item(4, Some("r1::/w/one"));
    four.shares_workspace_of = Some(2);
    let mut three = item(3, Some("r1::/w/three"));
    three.handover = Some(PendingHandover {
        harness: "codex".into(),
        model: None,
        effort: None,
        summary: Some("half done".into()),
        by: Some("acme/widgets#3".into()),
        requested_at: "2026-09-07T10:00:00Z".into(),
    });
    let st = state_with(vec![one, two, three, four]);
    let s = sessions(&cfg(), &st, Some(&[]));
    assert_eq!(s[0].harness, "pi");
    assert_eq!(s[0].model.as_deref(), Some("openai/gpt-6"));
    assert_eq!(s[0].effort.as_deref(), Some("high"));
    assert_eq!(s[1].harness, "pi", "the bound item shares the workspace");
    assert_eq!(s[2].harness, "claude", "not handed over yet");
    assert!(s[2].overrides.is_none());
    assert_eq!(s[3].harness, "pi", "two hops to the session that acts");
    assert_eq!(s[3].owner, "acme/widgets#1");
    let h = s[2].handover.as_ref().unwrap();
    assert_eq!(h.harness_name, "Codex");
    assert_eq!(h.summary_chars, Some(9));
    assert_eq!(
        h.describe(),
        "codex, asked by acme/widgets#3",
        "{}",
        h.describe()
    );
    let table = render_peers(&s, None);
    assert!(table.contains("handed over to pi"), "{table}");
    assert!(table.contains("model openai/gpt-6"), "{table}");
    assert!(
        table.contains("handover pending: codex, asked by acme/widgets#3"),
        "{table}"
    );
    assert!(
        table.contains("handover note waiting: from Claude Code, summary 13 chars"),
        "{table}"
    );
    let snap = Snapshot {
        cfg: cfg(),
        state: st,
        workspaces: Vec::new(),
        down: Vec::new(),
        errors: Vec::new(),
    };
    let v = snap.to_json();
    assert_eq!(v["sessions"][0]["overrides"]["harness"], "pi");
    assert!(v["sessions"][1]["overrides"]["harness"] == "pi");
    assert_eq!(v["sessions"][2]["handover"]["harness"], "codex");
    assert!(v["sessions"][2]["overrides"].is_null());
    let text = render_status(&snap);
    assert!(
        text.contains("handed over: harness=pi model=openai/gpt-6 effort=high"),
        "{text}"
    );
    assert!(text.contains("handover pending: codex"), "{text}");
    assert!(
        text.contains("handover note waiting: from Claude Code, summary 13 chars"),
        "{text}"
    );
    assert_eq!(v["sessions"][0]["handover_note"]["from"], "Claude Code");
    assert_eq!(v["sessions"][0]["handover_note"]["summary_chars"], 13);
    assert!(v["sessions"][2]["handover_note"].is_null());
}

/// `ssf assign` writes the same overrides a handover does, and a person
/// reading the status commands is told which of the two put the stack
/// there: an assigned item is on `harness pi` with the model and effort
/// facts, not "handed over to pi" (which is also where the item's
/// `via assigned` trigger sits on the same line).
#[test]
fn an_assigned_session_shows_the_stack_it_was_given() {
    let _sandbox = crate::config::test_support::sandbox();
    let mut one = item(1, Some("r1::/w/one"));
    one.overrides = Some(Overrides {
        harness: "pi".into(),
        model: Some("openai/gpt-6".into()),
        effort: Some("high".into()),
    });
    one.assigned_at = Some("2026-09-07T09:00:00Z".into());
    // Overrides an assignment wrote, on an item that was handed over
    // before them: the assignment's stamp is the one that counts.
    let mut three = item(3, Some("r1::/w/three"));
    three.overrides = Some(Overrides {
        harness: "codex".into(),
        model: None,
        effort: None,
    });
    three.handed_over_at = Some("2026-09-07T08:00:00Z".into());
    three.assigned_at = Some("2026-09-07T09:00:00Z".into());
    let mut two = item(2, Some("r1::/w/two"));
    two.overrides = Some(Overrides {
        harness: "pi".into(),
        model: None,
        effort: None,
    });
    two.handed_over_at = Some("2026-09-07T09:00:00Z".into());
    let st = state_with(vec![one, two, three]);
    let s = sessions(&cfg(), &st, None);
    assert!(s[0].assigned_stack, "assigned: its own stamp");
    assert!(!s[1].assigned_stack, "the handover stamped it");
    assert!(s[2].assigned_stack, "the assignment wrote these last");
    let table = render_peers(&s, None);
    assert!(table.contains("harness pi"), "{table}");
    assert!(table.contains("model openai/gpt-6"), "{table}");
    assert!(table.contains("handed over to pi"), "{table}");
    assert!(table.contains("harness codex"), "{table}");
    let snap = Snapshot {
        cfg: cfg(),
        state: st,
        workspaces: Vec::new(),
        down: Vec::new(),
        errors: Vec::new(),
    };
    let text = render_status(&snap);
    assert!(
        text.contains("assigned: harness=pi model=openai/gpt-6 effort=high"),
        "{text}"
    );
    assert!(text.contains("handed over: harness=pi\n"), "{text}");
    assert!(text.contains("assigned: harness=codex\n"), "{text}");
}

/// A session left on another harness by a config edit (`ssf repo set`) says
/// what its pane is running and what the next launch would start. Only the
/// harness is the driver's to report, so the stack the *next* launch uses is
/// the only one with a model and effort (#349).
#[test]
fn a_session_left_on_an_older_harness_reports_both_stacks() {
    let _sandbox = crate::config::test_support::sandbox();
    let mut cfg = cfg();
    cfg.repos[0].model = Some("deepseek/deepseek-flash".into());
    cfg.repos[0].effort = Some("high".into());
    let st = state_with(vec![item(1, Some("r1::/w/one"))]);
    let ws = vec![workspace(
        "r1::/w/one",
        None,
        Some(AgentInfo {
            state: "working".into(),
            agent_type: Some("codex".into()),
            ..Default::default()
        }),
    )];
    let joined = sessions(&cfg, &st, Some(&ws));
    let s = &joined[0];
    assert_eq!(s.harness, "codex", "the pane, not the config");
    assert_eq!(s.model, None, "what the running session was launched with");
    assert_eq!(s.effort, None);
    let next = s.next_launch.as_ref().expect("the change is pending");
    assert_eq!(next.harness, "claude");
    assert_eq!(next.model.as_deref(), Some("deepseek/deepseek-flash"));
    assert_eq!(next.effort.as_deref(), Some("high"));
    assert_eq!(
        s.next_launch_change().as_deref(),
        Some("harness codex → claude next launch (model deepseek/deepseek-flash, effort high)")
    );
    let peers = render_peers(&joined, None);
    assert!(
        peers.contains(
            "harness codex → claude next launch (model deepseek/deepseek-flash, effort high)"
        ),
        "{peers}"
    );
    let snap = Snapshot {
        cfg,
        state: st,
        workspaces: ws,
        down: Vec::new(),
        errors: Vec::new(),
    };
    let v = snap.to_json();
    assert_eq!(v["sessions"][0]["harness"], "codex");
    assert_eq!(v["sessions"][0]["next_launch"]["harness"], "claude");
    assert_eq!(v["sessions"][0]["model"], serde_json::Value::Null);
    let text = render_status(&snap);
    assert!(
        text.contains(
            "harness codex → claude next launch (model deepseek/deepseek-flash, effort high)"
        ),
        "{text}"
    );
    let card = dashboard_presentation(&v).unwrap();
    assert_eq!(card["cards"][0]["harness"], "codex");
    assert_eq!(card["cards"][0]["next_launch"]["harness"], "claude");
}

/// The ordinary case is untouched: an item that has never been launched
/// reports the stack its record would launch, and nothing is pending.
#[test]
fn a_session_that_never_launched_reports_the_configured_stack() {
    let _sandbox = crate::config::test_support::sandbox();
    let mut cfg = cfg();
    cfg.repos[0].model = Some("opus".into());
    cfg.repos[0].effort = Some("high".into());
    let s = sessions(
        &cfg,
        &state_with(vec![item(1, Some("r1::/w/one"))]),
        Some(&[]),
    );
    assert_eq!(s[0].harness, "claude");
    assert_eq!(s[0].model.as_deref(), Some("opus"));
    assert_eq!(s[0].effort.as_deref(), Some("high"));
    assert!(s[0].next_launch.is_none());
    assert!(s[0].next_launch_change().is_none());
    let json = serde_json::to_string(&s[0]).unwrap();
    assert!(!json.contains("next_launch"), "{json}");
    // And the same through the whole payload: nothing is invented for a
    // session the driver does not know about.
    let snap = Snapshot {
        cfg,
        state: state_with(vec![item(1, Some("r1::/w/one"))]),
        workspaces: Vec::new(),
        down: Vec::new(),
        errors: Vec::new(),
    };
    assert_eq!(snap.to_json()["sessions"][0]["harness"], "claude");
    assert!(snap.to_json()["sessions"][0]["next_launch"].is_null());
}

#[test]
fn ago_buckets() {
    let t = |secs: i64| {
        (chrono::Utc::now() - chrono::Duration::seconds(secs))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    };
    assert_eq!(ago(Some(&t(5))), "now");
    assert_eq!(ago(Some(&t(150))), "3m");
    assert_eq!(ago(Some(&t(7200))), "2h");
    assert_eq!(ago(Some(&t(3 * 86_400))), "3d");
    assert_eq!(ago(None), "");
    assert_eq!(ago(Some("garbage")), "");
}

/// A report says whether the *factory* is running from the daemon, and names
/// the service unit's own state as the detail it is: a daemon started
/// outside the unit is not an inactive factory (#463).
#[test]
fn the_service_line_follows_the_daemon_and_names_the_unit() {
    let _sandbox = crate::config::test_support::sandbox();
    let unit = crate::platform::service_instance();
    // Where the daemon and the unit agree, the line reads as it always did.
    assert_eq!(crate::platform::service_state(true, true, true), "running");
    assert_eq!(
        crate::platform::service_state(true, true, false),
        "running (disabled)"
    );
    assert_eq!(
        crate::platform::service_state(false, false, true),
        "stopped"
    );
    assert_eq!(
        crate::platform::service_state(false, false, false),
        "stopped (disabled)"
    );
    // A daemon the unit is not running is a running factory, and the unit
    // is named rather than equated with it.
    assert_eq!(
        crate::platform::service_state(true, false, true),
        format!("running (started outside {unit}, which is stopped)")
    );
    assert_eq!(
        crate::platform::service_state(true, false, false),
        format!("running (started outside {unit}, which is stopped and disabled)")
    );
    // And the unit being up does not stand in for a daemon that is not.
    assert_eq!(
        crate::platform::service_state(false, true, true),
        format!("daemon not answering ({unit} running)")
    );
}

#[test]
fn doctor_summary_keeps_only_failures_and_warnings() {
    assert_eq!(doctor_summary(None), Value::Null);
    let report = json!({
        "checked_at": "2026-09-25T10:00:00+00:00",
        "problems": 1,
        "checks": [
            {"level": "ok", "message": "fine"},
            {"level": "fail", "message": "broken"},
            {"level": "note", "message": "fyi"},
            {"level": "warn", "message": "odd"},
        ],
    });
    assert_eq!(
        doctor_summary(Some(&report)),
        json!({
            "checked_at": "2026-09-25T10:00:00+00:00",
            "problems": 1,
            "warnings": [
                {"level": "fail", "message": "broken"},
                {"level": "warn", "message": "odd"},
            ],
        })
    );
}
