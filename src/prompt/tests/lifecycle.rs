use super::*;

#[test]
fn a_handed_over_session_is_told_where_it_came_from() {
    let story = "# The item\n\n## How to work on this\n";
    let p = handover_prompt("Claude Code", "issue", Some("  Branch pushed.  "), story);
    assert_eq!(
        p,
        "You took over this issue from a session on Claude Code that handed it over; its \
summary follows, then the issue as ssf tells it to a new session.\n\n\
## Summary from the outgoing session\n\n\
Branch pushed.\n\n\
# The item\n\n## How to work on this\n"
    );
    let p = handover_prompt("Pi", "pull request", None, story);
    assert!(
        p.starts_with(
            "You took over this pull request from a session on Pi that handed it over. It \
left no summary; read the pull request below.\n\n"
        ),
        "{p}"
    );
    assert!(p.ends_with(story), "{p}");
    assert_eq!(
        handover_refused_prompt("Pi", "the item is no longer active"),
        "[ssf] Handover to Pi refused: the item is no longer active. Carry on."
    );
}

/// The first message of a handed-over session says why it exists, in
/// place of the trigger list an ordinary session gets.
#[test]
fn the_first_message_of_a_handed_over_session_says_who_handed_it_over() {
    let issue: Issue = serde_json::from_value(serde_json::json!({
        "number": 18, "title": "T", "html_url": "https://x/18", "body": "b", "state": "open",
        "user": {"login": "h"}, "labels": [], "assignees": [],
        "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    let repo = RepoConfig {
        name: "o/r".into(),
        harness: "pi".into(),
        ..Default::default()
    };
    let daemon = DaemonConfig::default();
    let triggers = vec!["assigned".to_string()];
    let ctx = PromptContext {
        repo: &repo,
        daemon: &daemon,
        bot_login: "bot",
        driver: DriverKind::Herdr,
        pr: None,
        triggers: &triggers,
        owner: None,
        delegated_by: None,
        handed_over_from: Some("Claude Code"),
        projects: &[],
        project_prompt: None,
        harness_prompt: None,
        vm_guest: false,
        pushes_as: None,
    };
    let p = initial_prompt(&issue, &[], &ctx);
    assert!(
        p.contains(
            "because the agent session on Claude Code working on it handed #18 over to you."
        ),
        "{p}"
    );
    assert!(!p.contains("was assigned to @bot"), "{p}");
}

#[test]
fn followup_says_a_filed_issue_is_yours_when_assigned() {
    // #83: the session on #81 filed it; the project manager assigned
    // the bot. Read as bookkeeping, the session waited for a second
    // session that creator ownership never starts.
    let filed: Issue = serde_json::from_value(json!({
        "number": 83, "title": "VM: omp does not run",
        "body": "🤖#81 says: <!-- ssf: origin=o/r#81 -->\n\nThe guest's omp binary fails.",
        "html_url": "https://gh/83", "state": "open", "user": {"login": "bot"},
        "created_at": "t", "updated_at": "t"
    }))
    .unwrap();
    let repo = RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        ..Default::default()
    };
    let d = cfg();
    let triggers = vec!["assigned".to_string(), "created".to_string()];
    let ctx = PromptContext {
        repo: &repo,
        daemon: &d,
        bot_login: "bot",
        driver: DriverKind::Orca,
        pr: None,
        triggers: &triggers,
        owner: Some(81),
        delegated_by: None,
        handed_over_from: None,
        projects: &[],
        project_prompt: None,
        harness_prompt: None,
        vm_guest: false,
        pushes_as: None,
    };
    let ev = json!({"event":"assigned","id":9,"actor":{"login":"bot"},"assignee":{"login":"bot"},
            "created_at":"2026-01-05T15:04:00Z"});
    let assigned = render_event(&ev, false, &d, "bot").unwrap();
    assert!(assigned.assigns("bot"));
    assert!(!assigned.assigns("alice"));
    let status = Rendered {
        key: "project_v2_item_status_changed:10".into(),
        text: "- [t] @bot project v2 item status changed".into(),
        origin: None,
        assignee: None,
    };
    let f = followup_prompt(&filed, &[assigned.clone(), status.clone()], &ctx);
    assert_eq!(
        f,
        "[ssf] New activity on #83 \"VM: omp does not run\":\n\n\
- 2026-01-05 15:04Z @bot assigned @bot\n- [t] @bot project v2 item status changed\n\n\
#83 is now assigned to @bot. You filed it, so it is yours: work on it in this workspace; \
nobody else is spawned for it.",
        "{f}"
    );
    // Ordinary activity on the same item carries no such line, nor
    // does an assignment to someone else.
    let f = followup_prompt(&filed, std::slice::from_ref(&status), &ctx);
    assert!(!f.contains("yours"), "{f}");
    let other = json!({"event":"assigned","id":11,"actor":{"login":"bot"},"assignee":{"login":"alice"},
            "created_at":"t"});
    let other = render_event(&other, false, &d, "bot").unwrap();
    let f = followup_prompt(&filed, &[other], &ctx);
    assert!(!f.contains("yours"), "{f}");
    // Not the FYI shape: a subscriber is not told to work on it.
    let fyi = fyi_prompt(
        &filed,
        std::slice::from_ref(&assigned),
        &ctx,
        None,
        false,
        Fyi::Activity,
    );
    assert!(!fyi.contains("yours"), "{fyi}");
    // An issue bound some other way (no tag naming the owner) or the
    // session's own item gets nothing either.
    let mut foreign = filed.clone();
    foreign.body = Some("<!-- ssf: origin=o/r#2 -->\n\nx".into());
    let f = followup_prompt(&foreign, std::slice::from_ref(&assigned), &ctx);
    assert!(!f.contains("yours"), "{f}");
    let own = PromptContext {
        owner: None,
        ..ctx.clone()
    };
    let f = followup_prompt(&filed, std::slice::from_ref(&assigned), &own);
    assert_eq!(
        f,
        "[ssf] New activity on #83:\n\n- 2026-01-05 15:04Z @bot assigned @bot\n"
    );
    // A label on a filed issue is activity like any other: no label
    // asks anything of ssf since #115.
    let labelled = Rendered {
        key: "labeled:12".into(),
        text: "- [t] @alice added label \"review\"".into(),
        origin: None,
        assignee: None,
    };
    let f = followup_prompt(&filed, &[labelled], &ctx);
    assert_eq!(
        f,
        "[ssf] New activity on #83 \"VM: omp does not run\":\n\n- [t] @alice added label \"review\"\n"
    );
}

#[test]
fn fyi_and_tell_prompts() {
    let issue: Issue = serde_json::from_value(json!({
            "number": 5, "title": "Thing", "body": null, "html_url": "https://gh/5", "state": "closed",
            "state_reason": "completed", "user": {"login": "carol"}, "created_at": "t", "updated_at": "t"
        }))
        .unwrap();
    let repo = RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        ..Default::default()
    };
    let d = cfg();
    let triggers = vec!["assigned".to_string()];
    let ctx = PromptContext {
        repo: &repo,
        daemon: &d,
        bot_login: "bot",
        driver: DriverKind::Orca,
        pr: None,
        triggers: &triggers,
        owner: None,
        delegated_by: None,
        handed_over_from: None,
        projects: &[],
        project_prompt: None,
        harness_prompt: None,
        vm_guest: false,
        pushes_as: None,
    };
    let ev = Rendered {
        key: "k".into(),
        text: "- [t] @alice commented (u):\n  > hi".into(),
        origin: None,
        assignee: None,
    };
    let p = fyi_prompt(
        &issue,
        &[ev.clone()],
        &ctx,
        Some("o/r#5"),
        false,
        Fyi::Activity,
    );
    // The item is named once, in the header; the owner is not.
    assert!(p.starts_with(
        "[ssf] FYI: new activity on issue #5 \"Thing\" (https://gh/5):\n\n- [t] @alice"
    ));
    assert!(!p.contains("owned by"));
    // One line of boilerplate after the activity, no more.
    assert!(p.ends_with("  > hi\n\nFor information only; `ssf unsub 5` stops these messages."));
    assert!(!p.contains("again unless"));
    let p = fyi_prompt(&issue, &[], &ctx, None, false, Fyi::Closed);
    assert_eq!(
        p,
        "[ssf] FYI: issue #5 \"Thing\" (https://gh/5) has been closed (completed).\n\n\
For information only; you will not hear about it again unless it comes back."
    );
    let p = fyi_prompt(&issue, &[], &ctx, Some("o/r#5"), true, Fyi::Closed);
    assert!(p.contains("(https://gh/5) has been merged."));
    let p = fyi_prompt(&issue, &[ev], &ctx, Some("o/r#5"), false, Fyi::Tracked);
    assert!(p.contains(
            "[ssf] FYI: issue #5 \"Thing\" (https://gh/5) now has an agent session of its own (o/r#5), because it was assigned to @bot."
        ));
    let p = fyi_prompt(&issue, &[], &ctx, Some("o/r#5"), false, Fyi::Unassigned);
    assert!(p.starts_with(
            "[ssf] FYI: @bot is no longer involved with issue #5 \"Thing\" (https://gh/5), so its session has retired."
        ));

    let t = tell_prompt(Some("o/r#3"), Some("Fix it"), "are you done?", 100);
    assert!(t.starts_with(
            "[ssf] Message from the agent session on o/r#3 (\"Fix it\"), sent with `ssf tell`:\n\n  > are you done?"
        ));
    assert!(t.ends_with(
            "  > are you done?\n\nIf it needs an answer, comment on o/r#3; `ssf tell 3 \"...\"` only for an operational nudge."
        ));
    let t = tell_prompt(None, None, "hello", 100);
    assert!(t.starts_with("[ssf] Message from a human at the terminal, sent with `ssf tell`:"));
    assert!(t.ends_with("  > hello\n\nIt comes from outside GitHub, so answer here."));
}

#[test]
fn owned_items_get_tracking_and_closing_notes() {
    let pr_issue: Issue = serde_json::from_value(json!({
            "number": 4, "title": "Fix it", "body": "<!-- ssf: origin=o/r#3 -->\n\nFixes it", "html_url": "https://gh/4",
            "state": "open", "state_reason": "completed", "user": {"login": "bot"}, "created_at": "t", "updated_at": "t",
            "pull_request": {}
        })).unwrap();
    let repo = RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        ..Default::default()
    };
    let d = cfg();
    let pr = PrInfo {
        head_ref: "bot/fix".into(),
        head_repo: "o/r".into(),
        base_ref: "main".into(),
        ..Default::default()
    };
    let triggers = vec!["created".to_string(), "review_requested".to_string()];
    let ctx = PromptContext {
        repo: &repo,
        daemon: &d,
        bot_login: "bot",
        driver: DriverKind::Orca,
        pr: Some(&pr),
        triggers: &triggers,
        owner: Some(3),
        delegated_by: None,
        handed_over_from: None,
        projects: &[],
        project_prompt: None,
        harness_prompt: None,
        vm_guest: false,
        pushes_as: None,
    };
    let ev = Rendered {
        key: "k".into(),
        text: "- [t] @alice requested a review from @bot".into(),
        origin: None,
        assignee: None,
    };
    let p = tracked_prompt(&pr_issue, &[ev], &ctx);
    // A review asked on an owned pull request is the session's own to
    // deal with, like any other trigger: no reviewer session exists.
    assert!(p.starts_with(
            "[ssf] Now tracking pull request #4 \"Fix it\" (https://gh/4) for this session, because this session opened it. It reached ssf because it requested a review from @bot; that is for you to act on.\n\nActivity so far:\n\n- [t] @alice requested a review from @bot\n"
        ), "{p}");
    assert!(!p.contains("reviewer session"));
    assert!(!p.contains("SSF_ISSUE"));
    assert!(p.ends_with(
        "\nAnswer on it with `gh pr comment 4 --repo o/r`; pushes to `bot/fix` update it."
    ));
    assert!(!p.contains("## How to work on this"));
    // Other human triggers on the owned PR still are.
    let assigned = vec!["created".to_string(), "assigned".to_string()];
    let actx = PromptContext {
        triggers: &assigned,
        ..ctx.clone()
    };
    let p = tracked_prompt(&pr_issue, &[], &actx);
    assert!(p.contains("because it was assigned to @bot; that is for you to act on"));
    assert!(!p.contains("reviewer session"));

    // A branch match rather than a tag.
    let mut untagged = pr_issue.clone();
    untagged.body = None;
    let p = tracked_prompt(&untagged, &[], &ctx);
    assert!(p.contains("because its branch `bot/fix` is this workspace's branch"));
    assert!(p.contains("(no activity yet)"));

    // A bound item is named with its title (the session may have
    // several); the session's own item, by number only.
    let c = closed_prompt(&pr_issue, &[], &ctx);
    assert_eq!(
        c,
        "[ssf] #4 \"Fix it\" has been closed (completed).\n\nNo further updates for it; your own item, #3, is unaffected."
    );
    let u = unassigned_prompt(&pr_issue, &[], &ctx);
    assert_eq!(
        u,
        "[ssf] The review request for @bot on #4 \"Fix it\" has been fulfilled or withdrawn.\n\nNo further updates for it unless it is brought back in; your own item, #3, is unaffected."
    );
    let own = PromptContext {
        owner: None,
        ..ctx.clone()
    };
    let c = closed_prompt(&pr_issue, &[], &own);
    assert_eq!(
        c,
        "[ssf] #4 has been closed (completed).\n\nStop working on it: commit anything worth keeping, push, and leave a short final comment on it; then, only if everything is on origin, `ssf release` gives this workspace back (it refuses if anything would be lost; a kept workspace is fine). No further updates for it."
    );
    let u = unassigned_prompt(&pr_issue, &[], &own);
    assert!(u.starts_with("[ssf] The review request for @bot on #4 has been fulfilled or withdrawn.\n\nStop working on it:"));
    let assigned_ctx = PromptContext {
        triggers: &assigned,
        ..own.clone()
    };
    let u = unassigned_prompt(&pr_issue, &[], &assigned_ctx);
    assert!(u.starts_with("[ssf] @bot is no longer assigned to or requested on #4.\n\n"));
    // A mention-triggered item was never assigned, so it is not told
    // it was unassigned; and the login is written without its `@` so
    // an agent quoting the line cannot mention the bot back onto it.
    let mentioned = vec!["mentioned".to_string()];
    let mentioned_ctx = PromptContext {
        triggers: &mentioned,
        ..own.clone()
    };
    let u = unassigned_prompt(&pr_issue, &[], &mentioned_ctx);
    assert!(
            u.starts_with(
                "[ssf] The mention of bot that started this session on #4 is gone.\n\nStop working on it:"
            ),
            "{u}"
        );
    assert!(!u.contains("@bot"));
    let r = reassigned_prompt(&pr_issue, &[], &assigned_ctx);
    assert_eq!(
        r,
        "[ssf] #4 has been assigned to @bot again. Activity since then:\n\n(no new activity)\n\nResume work on it."
    );
    let f = followup_prompt(&pr_issue, &[], &own);
    assert!(f.starts_with("[ssf] New activity on #4:"), "{f}");
    let f = followup_prompt(&pr_issue, &[], &ctx);
    assert!(f.starts_with("[ssf] New activity on #4 \"Fix it\":"), "{f}");

    // The message a delegating parent gets.
    let last = FinalComment {
        author: "bot".into(),
        session: Some("o/r#4".into()),
        url: "https://gh/4#c1".into(),
        body: "Done, see PR #5.".into(),
    };
    let m = delegated_closed_prompt(&pr_issue, true, Some(&last), &ctx);
    assert!(m.starts_with(
            "[ssf] #4 \"Fix it\" (https://gh/4), the pull request this session handed off, has been merged."
        ));
    assert!(m.contains(
        "Final comment by @bot (from the agent on o/r#4) (https://gh/4#c1):\n  > Done, see PR #5."
    ));
    let m = delegated_closed_prompt(&pr_issue, false, None, &ctx);
    assert!(m.contains("has been closed (completed)."));
    assert!(m.contains("It has no comments."));
}

#[test]
fn interrupted_prompt_names_the_session_and_where_it_is() {
    let p = interrupted_prompt(&Interrupted {
        number: 18,
        title: "Resume sessions",
        url: "https://gh/18",
        branch: Some("refs/heads/bot/issue-18"),
        path: Some("/w/issue-18"),
    });
    assert!(p.starts_with("[ssf] The factory restarted"));
    assert!(p.contains("session for #18 \"Resume sessions\" (https://gh/18)"));
    assert!(p.contains("on branch `bot/issue-18` in `/w/issue-18`"));
    assert!(p.contains("`git status`, `git log`"));
    assert!(p.contains("say so on the item"));

    let r = interrupted_prompt(&Interrupted {
        number: 25,
        title: "Fix",
        url: "https://gh/25",
        branch: None,
        path: None,
    });
    assert!(r.contains("session for #25 \"Fix\" (https://gh/25)."));
    assert!(!r.contains("on branch"));
}
