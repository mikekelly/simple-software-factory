use super::*;
use std::time::Duration;

fn o() -> Origin {
    Origin::new("acme/widgets", 12).unwrap()
}

fn launch(branch: Option<&str>) -> Launch {
    Launch {
        harness: "Claude Code".into(),
        model: Some("fable-5.1".into()),
        effort: Some("high".into()),
        command: None,
        driver: "herdr".into(),
        branch: branch.map(str::to_string),
    }
}

/// The shape every post shares: byline line, blank line, one fenced
/// block whose lines are all non-blank `key: value` lines under an
/// `ssf ...:` header.
fn check_shape(text: &str, event: &str) {
    let mut lines = text.lines();
    assert_eq!(
        lines.next().unwrap(),
        format!("🤖 ssf <!-- ssf: origin=acme/widgets#12 event={event} -->")
    );
    assert_eq!(lines.next().unwrap(), "");
    assert_eq!(lines.next().unwrap(), "```ssf");
    let header = lines.next().unwrap();
    assert!(
        header.starts_with("ssf ") && header.ends_with(':'),
        "{header}"
    );
    let body: Vec<&str> = lines.collect();
    assert_eq!(body.last().copied(), Some("```"));
    for l in &body[..body.len() - 1] {
        assert!(!l.trim().is_empty(), "blank line inside the block:\n{text}");
        let (k, v) = l.split_once(": ").expect(l);
        assert!(!k.is_empty() && !v.is_empty() && !v.contains('\n'), "{l}");
    }
    assert!(!text.ends_with('\n'));
    // The whole thing parses back as an event post, not a session's.
    let tag = crate::origin::parse(text).unwrap();
    assert_eq!(tag.origin, o());
    assert_eq!(tag.event(), Some(event));
    assert!(crate::origin::is_event_post(text));
}

#[test]
fn attached_names_the_launch() {
    let ev = Event::Attached(Attach::Started {
        launch: launch(Some("refs/heads/bot/issue-12-fix")),
        handed_off_from: None,
    });
    let text = comment(&o(), "issue", &ev);
    check_shape(&text, "attached");
    assert_eq!(
        text,
        "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=attached -->\n\n\
             ```ssf\n\
             ssf attaching agent to issue:\n\
             harness: Claude Code\n\
             model: fable-5.1\n\
             effort: high\n\
             driver: herdr\n\
             branch: bot/issue-12-fix\n\
             ```"
    );
    // Unset model and effort read as the harness's default; no branch,
    // no line; a delegated item names its parent.
    let ev = Event::Attached(Attach::Started {
        launch: Launch {
            model: None,
            effort: Some("  ".into()),
            branch: None,
            ..launch(None)
        },
        handed_off_from: Some("acme/widgets#4".into()),
    });
    let text = comment(&o(), "issue", &ev);
    check_shape(&text, "attached");
    assert_eq!(
        ev.block("issue"),
        "```ssf\n\
             ssf attaching agent to issue:\n\
             harness: Claude Code\n\
             model: the harness's default\n\
             effort: the harness's default\n\
             driver: herdr\n\
             handed off from: acme/widgets#4\n\
             ```"
    );
}

#[test]
fn a_configured_command_is_named_and_decides_the_defaults() {
    let ev = Event::Attached(Attach::Started {
        launch: Launch {
            model: None,
            effort: None,
            command: Some("claude --dangerously-skip-permissions --model opus".into()),
            ..launch(None)
        },
        handed_off_from: None,
    });
    assert_eq!(
        ev.block("issue"),
        "```ssf\n\
             ssf attaching agent to issue:\n\
             harness: Claude Code\n\
             model: the command's\n\
             effort: the command's\n\
             command: claude --dangerously-skip-permissions --model opus\n\
             driver: herdr\n\
             ```"
    );
    // Set alongside a command, model and effort are named as given.
    let ev = Event::Attached(Attach::Started {
        launch: Launch {
            command: Some("  claude  ".into()),
            ..launch(Some("bot/x"))
        },
        handed_off_from: None,
    });
    assert!(
        ev.block("issue")
            .contains("model: fable-5.1\neffort: high\ncommand: claude\ndriver: herdr\n")
    );
}

#[test]
fn bound_names_the_owning_session() {
    let ev = Event::Attached(Attach::Bound {
        session: "acme/widgets#4".into(),
        shares: 4,
    });
    let text = comment(&o(), "pull request", &ev);
    check_shape(&text, "attached");
    assert_eq!(
        ev.block("pull request"),
        "```ssf\n\
             ssf attaching agent to pull request:\n\
             session: acme/widgets#4\n\
             shares: workspace of #4\n\
             ```"
    );
}

#[test]
fn re_created_says_why_and_how_the_conversation_went() {
    let ev = Event::Attached(Attach::ReCreated {
        launch: launch(Some("bot/issue-12-fix")),
        reason: "driver switch",
        conversation: Conversation::of(false),
    });
    let text = comment(&o(), "issue", &ev);
    check_shape(&text, "attached");
    assert_eq!(
        ev.block("issue"),
        "```ssf\n\
             ssf attaching agent to issue again:\n\
             harness: Claude Code\n\
             model: fable-5.1\n\
             effort: high\n\
             driver: herdr\n\
             branch: bot/issue-12-fix\n\
             re-created: driver switch\n\
             conversation: fresh\n\
             ```"
    );
}

#[test]
fn kept_says_the_workspace_was_there_already() {
    let ev = Event::Attached(Attach::Kept {
        launch: launch(Some("bot/issue-12-fix")),
        handed_off_from: Some("acme/widgets#4".into()),
        conversation: Conversation::Kept,
    });
    let text = comment(&o(), "issue", &ev);
    check_shape(&text, "attached");
    assert_eq!(
        ev.block("issue"),
        "```ssf\n\
             ssf attaching agent to issue again:\n\
             harness: Claude Code\n\
             model: fable-5.1\n\
             effort: high\n\
             driver: herdr\n\
             branch: bot/issue-12-fix\n\
             handed off from: acme/widgets#4\n\
             workspace: kept\n\
             conversation: kept\n\
             ```"
    );
}

#[test]
fn handed_over_names_both_ends() {
    let ev = Event::HandedOver {
        from: launch(Some("bot/issue-12-fix")),
        to: Launch {
            harness: "Pi".into(),
            model: None,
            effort: None,
            ..launch(None)
        },
        summary: true,
        by: Some("acme/widgets#12".into()),
        refused: None,
    };
    let text = comment(&o(), "issue", &ev);
    check_shape(&text, "handed-over");
    assert_eq!(
        text,
        "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=handed-over -->\n\n\
             ```ssf\n\
             ssf handing over issue:\n\
             from: Claude Code\n\
             from model: fable-5.1\n\
             from effort: high\n\
             to: Pi\n\
             to model: the harness's default\n\
             to effort: the harness's default\n\
             summary: yes\n\
             by: acme/widgets#12\n\
             ```"
    );
    // No summary, and nobody's session behind it.
    let ev = Event::HandedOver {
        from: launch(Some("bot/issue-12-fix")),
        to: Launch {
            harness: "Pi".into(),
            model: None,
            effort: None,
            ..launch(None)
        },
        summary: false,
        by: None,
        refused: None,
    };
    assert!(
        ev.block("pull request")
            .starts_with("```ssf\nssf handing over pull request:\n"),
        "{}",
        ev.block("pull request")
    );
    assert!(
        ev.block("issue")
            .ends_with("summary: no\nby: a person at the terminal\n```"),
        "{}",
        ev.block("issue")
    );
}

/// A configured command decides an unset model or effort, so the
/// block names it on the side that has one, as the `attached` block
/// does: without it `the command's` refers to nothing.
#[test]
fn a_handover_names_a_configured_command_on_each_side() {
    let ev = Event::HandedOver {
        from: Launch {
            model: None,
            effort: None,
            command: Some("claude --model opus".into()),
            ..launch(Some("bot/issue-12-fix"))
        },
        to: Launch {
            harness: "Codex".into(),
            model: None,
            effort: Some("medium".into()),
            command: Some("codex --search".into()),
            ..launch(None)
        },
        summary: true,
        by: None,
        refused: None,
    };
    check_shape(&comment(&o(), "issue", &ev), "handed-over");
    assert_eq!(
        ev.block("issue"),
        "```ssf\n\
             ssf handing over issue:\n\
             from: Claude Code\n\
             from model: the command's\n\
             from effort: the command's\n\
             from command: claude --model opus\n\
             to: Codex\n\
             to model: the command's\n\
             to effort: medium\n\
             to command: codex --search\n\
             summary: yes\n\
             by: a person at the terminal\n\
             ```"
    );
    // A refusal says nothing about the side that stays.
    let Event::HandedOver { from, to, .. } = ev else {
        unreachable!()
    };
    let refused = Event::HandedOver {
        from,
        to,
        summary: true,
        by: None,
        refused: Some("the item is no longer active".into()),
    };
    assert_eq!(
        refused.block("issue"),
        "```ssf\n\
             ssf not handing over issue:\n\
             to: Codex\n\
             to model: the command's\n\
             to effort: medium\n\
             to command: codex --search\n\
             by: a person at the terminal\n\
             refused: the item is no longer active\n\
             ```"
    );
}

#[test]
fn a_refused_handover_says_only_what_it_would_have_been() {
    let ev = Event::HandedOver {
        from: launch(Some("bot/issue-12-fix")),
        to: Launch {
            harness: "Codex".into(),
            model: Some("gpt-6".into()),
            effort: None,
            ..launch(None)
        },
        summary: true,
        by: Some("acme/widgets#12".into()),
        refused: Some("could not stop the running agent:\nno such terminal".into()),
    };
    let text = comment(&o(), "issue", &ev);
    check_shape(&text, "handed-over");
    assert_eq!(
        ev.block("issue"),
        "```ssf\n\
             ssf not handing over issue:\n\
             to: Codex\n\
             to model: gpt-6\n\
             to effort: the harness's default\n\
             by: acme/widgets#12\n\
             refused: could not stop the running agent: no such terminal\n\
             ```"
    );
}

#[test]
fn attached_after_a_handover_names_the_harness_it_came_from() {
    let ev = Event::Attached(Attach::HandedOver {
        launch: Launch {
            harness: "Pi".into(),
            model: Some("openai/gpt-6".into()),
            effort: None,
            ..launch(Some("bot/issue-12-fix"))
        },
        from: "Claude Code".into(),
    });
    let text = comment(&o(), "issue", &ev);
    check_shape(&text, "attached");
    assert_eq!(
        ev.block("issue"),
        "```ssf\n\
             ssf attaching agent to issue:\n\
             harness: Pi\n\
             model: openai/gpt-6\n\
             effort: the harness's default\n\
             driver: herdr\n\
             branch: bot/issue-12-fix\n\
             handed over from: Claude Code\n\
             ```"
    );
}

#[test]
fn resumed_says_after_what() {
    let ev = Event::Resumed {
        harness: "Codex".into(),
        conversation: Conversation::of(true),
        after: "restart",
    };
    let text = comment(&o(), "issue", &ev);
    check_shape(&text, "resumed");
    assert_eq!(
        text,
        "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=resumed -->\n\n\
             ```ssf\n\
             ssf resuming agent on issue:\n\
             harness: Codex\n\
             conversation: resumed\n\
             after: restart\n\
             ```"
    );
}

#[test]
fn blocked_and_unblocked() {
    let ev = Event::Blocked {
        harness: "Claude Code".into(),
        reason: "not signed in".into(),
        fix: "`claude auth login` on the host".into(),
    };
    let text = comment(&o(), "issue", &ev);
    check_shape(&text, "blocked");
    // Markdown backticks would render literally inside the fence.
    assert_eq!(
        text,
        "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=blocked -->\n\n\
             ```ssf\n\
             ssf holding deliveries to agent on issue:\n\
             harness: Claude Code\n\
             reason: not signed in\n\
             fix: claude auth login on the host\n\
             ```"
    );
    // The other reason: the harness never came up (see
    // `Engine::finish_handover`).
    let ev = Event::Blocked {
        harness: "Pi".into(),
        reason: "could not be started: pi exited at once".into(),
        fix: "start Pi by hand in the workspace, or fix the model or effort and hand over again"
            .into(),
    };
    let text = comment(&o(), "issue", &ev);
    check_shape(&text, "blocked");
    assert_eq!(
        text,
        "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=blocked -->\n\n\
             ```ssf\n\
             ssf holding deliveries to agent on issue:\n\
             harness: Pi\n\
             reason: could not be started: pi exited at once\n\
             fix: start Pi by hand in the workspace, or fix the model or effort and hand over again\n\
             ```"
    );
    let ev = Event::Unblocked {
        harness: "Claude Code".into(),
        held: Duration::from_secs(12 * 60 + 30),
        conversation: Conversation::Resumed,
    };
    let text = comment(&o(), "issue", &ev);
    check_shape(&text, "unblocked");
    assert_eq!(
        text,
        "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=unblocked -->\n\n\
             ```ssf\n\
             ssf resuming deliveries to agent on issue:\n\
             harness: Claude Code\n\
             held for: 12 min\n\
             conversation: resumed\n\
             ```"
    );
    let quick = Event::Unblocked {
        harness: "Claude Code".into(),
        held: Duration::from_secs(59),
        conversation: Conversation::Kept,
    };
    assert!(
        quick
            .block("pull request")
            .contains("held for: less than a minute\nconversation: kept\n")
    );
    // A hold closed by a handover: the conversation is neither kept
    // nor resumed, it belongs to the session that has gone.
    let over = Event::Unblocked {
        harness: "Claude Code".into(),
        held: Duration::from_secs(3 * 60 * 60),
        conversation: Conversation::HandedOver,
    };
    assert!(
        over.block("issue")
            .contains("held for: 180 min\nconversation: handed over\n"),
        "{}",
        over.block("issue")
    );
}

#[test]
fn gave_up_keeps_the_error_on_one_line() {
    let ev = Event::GaveUp {
        failures: 5,
        last_error: "orca worktree deliver failed:\n  exit status 1\n\tno such terminal".into(),
    };
    let text = comment(&o(), "issue", &ev);
    check_shape(&text, "gave-up");
    assert_eq!(
        text,
        "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=gave-up -->\n\n\
             ```ssf\n\
             ssf giving up on agent binding for issue:\n\
             failures: 5\n\
             last error: orca worktree deliver failed: exit status 1 no such terminal\n\
             next: re-onboarding the item\n\
             ```"
    );
    // Backticks go: three in a row would end the block early.
    let hostile = Event::GaveUp {
        failures: 5,
        last_error: "said:\n```\nboom\n```\nand `more`".into(),
    };
    let block = hostile.block("issue");
    assert!(
        block.contains("\nlast error: said: boom and more\n"),
        "{block}"
    );
    assert_eq!(block.matches("```").count(), 2, "{block}");
    let nothing = Event::GaveUp {
        failures: 5,
        last_error: " `` \n\t".into(),
    };
    assert!(
        nothing.block("issue").contains("\nlast error: (empty)\n"),
        "{}",
        nothing.block("issue")
    );
    assert_eq!(one_line("  a\n`b`  c "), "a b c");
    assert_eq!(one_line(" ` "), "");
    let long = Event::GaveUp {
        failures: 5,
        last_error: "x".repeat(500),
    };
    let block = long.block("issue");
    let line = block
        .lines()
        .find(|l| l.starts_with("last error: "))
        .unwrap();
    assert_eq!(
        line.chars().count(),
        "last error: ".len() + MAX_VALUE_CHARS + 1
    );
    assert!(line.ends_with('\u{2026}'));
}

#[test]
fn released_says_by_whom() {
    let ev = Event::Released {
        by: "ssf release",
        forced: false,
        branch: Some("refs/heads/bot/issue-12-fix".into()),
    };
    let text = comment(&o(), "issue", &ev);
    check_shape(&text, "released");
    assert_eq!(
        text,
        "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=released -->\n\n\
             ```ssf\n\
             ssf releasing workspace of issue:\n\
             by: ssf release\n\
             branch: bot/issue-12-fix\n\
             ```"
    );
    let forced = Event::Released {
        by: "ssf purge",
        forced: true,
        branch: None,
    };
    check_shape(&comment(&o(), "pull request", &forced), "released");
    assert_eq!(
        forced.block("pull request"),
        "```ssf\n\
             ssf releasing workspace of pull request:\n\
             by: ssf purge\n\
             forced: yes\n\
             ```"
    );
}

#[test]
fn names_are_the_tag_values() {
    let names: Vec<&str> = [
        Event::Attached(Attach::Bound {
            session: "a/b#1".into(),
            shares: 1,
        }),
        Event::Resumed {
            harness: "x".into(),
            conversation: Conversation::Fresh,
            after: "lost terminal",
        },
        Event::Blocked {
            harness: "x".into(),
            reason: "not signed in".into(),
            fix: "y".into(),
        },
        Event::Unblocked {
            harness: "x".into(),
            held: Duration::ZERO,
            conversation: Conversation::Kept,
        },
        Event::GaveUp {
            failures: 1,
            last_error: "e".into(),
        },
        Event::HandedOver {
            from: launch(None),
            to: launch(None),
            summary: false,
            by: None,
            refused: None,
        },
        Event::Released {
            by: "ssf release",
            forced: false,
            branch: None,
        },
    ]
    .iter()
    .map(Event::name)
    .collect();
    assert_eq!(
        names,
        vec![
            "attached",
            "resumed",
            "blocked",
            "unblocked",
            "gave-up",
            "handed-over",
            "released"
        ]
    );
}
