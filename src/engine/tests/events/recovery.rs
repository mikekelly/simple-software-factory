use super::*;

#[test]
fn ssf_texts_never_look_like_a_login_prompt() {
    use crate::driver::login_dialog;
    let harnesses = [
        "claude", "codex", "gemini", "copilot", "opencode", "pi", "omp", "grok", "crush", "zzz",
    ];
    for h in harnesses {
        let name = login::display_name(h);
        let fix = login::how_to_sign_in(h);
        let b = Blocked {
            reason: "login".into(),
            harness: h.into(),
            detail: "Login expired · Please run /login".into(),
            since: now_iso(),
            reported: true,
            credential: None,
            retried_at: None,
            retries: 0,
            told_at: None,
            tell_failures: 0,
        };
        let o = Origin::new("o/r", 5).unwrap();
        let texts = [
            prompt::login_back_prompt(&prompt::LoginBack {
                harness: &name,
                since: &b.since,
                number: 5,
                title: "Fix it",
                url: "https://gh/5",
            }),
            events::comment(
                &o,
                "issue",
                &Event::Blocked {
                    harness: name.clone(),
                    reason: "not signed in".into(),
                    fix: fix.clone(),
                },
            ),
            // The other block: a harness that would not start at all,
            // whose reason line carries the driver's own words.
            events::comment(
                &o,
                "issue",
                &Event::Blocked {
                    harness: name.clone(),
                    reason: format!(
                        "could not be started: {}",
                        safe_error(&events::one_line(
                            "herdr said: the pane exited at once\nLogin expired · Please run /login"
                        ))
                    ),
                    fix: crate::status::fix_for(&Blocked {
                        reason: Blocked::START.into(),
                        harness: h.into(),
                        ..b.clone()
                    }),
                },
            ),
            prompt::start_again_prompt(&prompt::LoginBack {
                harness: &name,
                since: &b.since,
                number: 5,
                title: "Fix it",
                url: "https://gh/5",
            }),
            crate::status::BlockedView::from_blocked(&Blocked {
                reason: Blocked::START.into(),
                detail: "the pane exited at once".into(),
                ..b.clone()
            })
            .describe(),
            SessionBlocked {
                session: "o/r#5".into(),
                blocked: Blocked {
                    reason: Blocked::START.into(),
                    ..b.clone()
                },
            }
            .to_string(),
            events::comment(
                &o,
                "issue",
                &Event::Unblocked {
                    harness: name.clone(),
                    held: Duration::from_secs(12 * 60),
                    conversation: Conversation::Resumed,
                },
            ),
            events::comment(
                &o,
                "pull request",
                &Event::Unblocked {
                    harness: name.clone(),
                    held: Duration::from_secs(30),
                    conversation: Conversation::Kept,
                },
            ),
            // A delivery error as the daemon would write it down: a
            // harness's own sign-in words in it are withheld, and
            // backticks (which would end the fence) stripped.
            events::comment(
                &o,
                "issue",
                &Event::GaveUp {
                    failures: 5,
                    last_error: safe_error(&events::one_line(
                        "orca said:\n```\nLogin expired · Please run /login\n```\nnot logged in",
                    )),
                },
            ),
            events::comment(
                &o,
                "issue",
                &Event::GaveUp {
                    failures: 5,
                    last_error: safe_error("`orca worktree deliver` failed: no such terminal"),
                },
            ),
            events::comment(
                &o,
                "issue",
                &Event::GaveUp {
                    failures: 5,
                    last_error: safe_error(
                        "gh: You are not logged into any GitHub hosts. Run gh auth login.",
                    ),
                },
            ),
            prompt::handover_prompt(
                &name,
                "issue",
                Some("Branch pushed; the parser is left."),
                "the item's story",
            ),
            prompt::handover_prompt(&name, "pull request", None, "the item's story"),
            prompt::handover_refused_prompt(&name, "the item is no longer active"),
            prompt::handover_cancelled_prompt(&name),
            crate::handover_cancelled_text("o/r#5", "Fix it", &name, true),
            crate::handover_cancelled_text("o/r#5", "Fix it", &name, false),
            prompt::handover_refused_prompt(
                &name,
                &events::one_line("could not stop the running agent: no such terminal"),
            ),
            events::comment(
                &o,
                "issue",
                &Event::HandedOver {
                    from: handover_launch(&name),
                    to: handover_launch("Pi"),
                    summary: true,
                    by: Some("o/r#5".into()),
                    refused: None,
                },
            ),
            events::comment(
                &o,
                "issue",
                &Event::HandedOver {
                    from: handover_launch("Pi"),
                    to: handover_launch(&name),
                    summary: false,
                    by: None,
                    refused: Some(safe_error(&events::one_line(
                        "could not stop the running agent: Login expired · Please run /login",
                    ))),
                },
            ),
            events::comment(
                &o,
                "issue",
                &Event::Attached(Attach::HandedOver {
                    launch: handover_launch(&name),
                    from: "Pi".into(),
                }),
            ),
            crate::handover_recorded_text(
                "o/r#5",
                "Fix it",
                &name,
                Some("fable-5.1"),
                Some("high"),
                None,
                Some(1_234),
                10,
            ),
            crate::handover_recorded_text("o/r#5", "Fix it", &name, None, None, None, None, 10),
            crate::handover_recorded_text(
                "o/r#5",
                "Fix it",
                &name,
                None,
                None,
                Some("claude --dangerously-skip-permissions"),
                None,
                10,
            ),
            crate::summary_quotes_a_sign_in_screen_text(&crate::driver::redact_login_phrases(
                "the pane said Login expired · Please run /login, so I stopped",
            )),
            crate::status::BlockedView::from_blocked(&b).describe(),
            SessionBlocked {
                session: "o/r#5".into(),
                blocked: b.clone(),
            }
            .to_string(),
            fix.clone(),
        ];
        for text in texts {
            // As the harness would show it: at the bottom of the screen,
            // after the agent's prompt marker, and without the `[ssf]`
            // marker, which would make the echo skip pass over the very
            // line under test.
            let screen = format!("⏺ {}\n\n❯ ", text.replace("[ssf]", ""));
            for judge in harnesses {
                assert_eq!(
                    login_dialog(judge, &screen),
                    None,
                    "{judge} takes this {h} text for a login prompt: {text}"
                );
            }
        }
    }
}
#[tokio::test]
async fn harness_notes_follow_the_session_through_handover_and_restart() {
    let sandbox = crate::config::test_support::sandbox();
    let worktree = sandbox.root().join("worktree");
    std::fs::create_dir_all(&worktree).unwrap();
    for (file, text) in [
        ("SSF.md", "Shared project guidance."),
        ("SSF.claude.md", "Claude-only guidance."),
        ("SSF.codex.md", "Codex-only guidance."),
    ] {
        std::fs::write(worktree.join(file), text).unwrap();
    }
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    let r = repo();
    e.entry(&r, 5).worktree_path = Some(worktree.to_string_lossy().into_owned());
    let issue: Issue = serde_json::from_value(assigned_item(5, "alice", "u1")).unwrap();
    let initial = e.initial_text(&r, &issue, &[]);
    assert!(initial.contains("Shared project guidance."));
    assert!(initial.contains("Claude-only guidance."));
    assert!(!initial.contains("Codex-only guidance."));

    e.handover("o/r#5", "codex", None, None, None, None)
        .await
        .unwrap();
    e.run_handovers(&r).await;
    let prompts = d.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(prompts[0].contains("Shared project guidance."));
    assert!(prompts[0].contains("Codex-only guidance."));
    assert!(!prompts[0].contains("Claude-only guidance."));

    // A fresh session after the handover uses the persisted override.
    let restarted = e.first_message(&r, 5).await.unwrap().text;
    assert!(restarted.contains("Shared project guidance."));
    assert!(restarted.contains("Codex-only guidance."));
    assert!(!restarted.contains("Claude-only guidance."));
}
#[tokio::test]
async fn a_handover_replaces_the_session_in_the_same_workspace() {
    let _sandbox = crate::config::test_support::sandbox();
    let stub = GitHubStub::start().await;
    let (mut e, d) = handover_setup(&stub);
    let v = e
        .handover(
            "o/r#5",
            "pi",
            Some("openai/gpt-6"),
            Some("high"),
            Some("Branch pushed; the parser is left."),
            Some("o/r#5"),
        )
        .await
        .unwrap();
    assert_eq!(v["session"], "o/r#5");
    assert_eq!(v["title"], "Fix the widget");
    assert_eq!(v["from"]["harness"], "claude");
    assert_eq!(v["from"]["model"], Value::Null);
    assert_eq!(v["to"]["harness"], "pi");
    assert_eq!(v["to"]["model"], "openai/gpt-6");
    assert_eq!(v["to"]["effort"], "high");
    assert_eq!(v["summary_chars"], 34);
    // Recorded and nothing else: the agent that asked is still there.
    assert!(e.entry(&repo(), 5).handover.is_some());
    assert!(d.log().is_empty(), "{:?}", d.log());
    assert!(stub.posts().is_empty());
    // While it is pending, nothing else touches the session.
    let err = e
        .handover("o/r#5", "codex", None, None, None, None)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("a handover to pi is already pending"),
        "{err:#}"
    );
    let err = e.tell(None, "o/r#5", "hello").await.unwrap_err();
    assert!(
        err.to_string().contains("a handover to pi is pending"),
        "{err:#}"
    );
    assert!(!e.resume_candidates(&repo()).contains(&5));
    // Delivery failures counted against the session that is going.
    e.failures.insert(("o/r".into(), 5), 2);

    e.run_handovers(&repo()).await;
    let log = d.log();
    assert_eq!(log[0], "stop:t5", "{log:?}");
    assert!(
        log[1].starts_with("start:w5:You took over this issue from a session on Claude"),
        "{log:?}"
    );
    assert_eq!(log.len(), 2, "{log:?}");
    let launched = d.launches();
    assert_eq!(launched.len(), 1, "{launched:?}");
    assert!(
        launched[0].starts_with("pi:") && launched[0].contains("openai/gpt-6"),
        "{launched:?}"
    );
    let st = e.entry(&repo(), 5).clone();
    assert!(st.handover.is_none(), "carried out");
    assert_eq!(
        st.overrides,
        Some(Overrides {
            harness: "pi".into(),
            model: Some("openai/gpt-6".into()),
            effort: Some("high".into()),
        })
    );
    // The old session is retired on the record; the workspace is not,
    // and nothing counted against it follows the new one.
    assert!(st.agent_session_id.is_none());
    assert!(e.failures.is_empty());
    // Its conversation is remembered as retired, so the transcript it
    // wrote moments ago is not captured as the new session's.
    assert_eq!(st.retired_session_ids, vec!["sess-5".to_string()]);
    assert!(st.blocked.is_none());
    assert_eq!(st.worktree_id.as_deref(), Some("w5"));
    assert_eq!(st.branch.as_deref(), Some("refs/heads/bot/issue-5"));
    assert!(st.seeded && st.active);
    assert!(st.terminal_handle.is_some());
    let posts = stub.post_bodies();
    assert_eq!(posts.len(), 2, "{posts:?}");
    assert_eq!(
        posts[0].1,
        "🤖 ssf <!-- ssf: origin=o/r#5 event=handed-over -->\n\n\
             ```ssf\n\
             ssf handing over issue:\n\
             from: Claude Code\n\
             from model: the harness's default\n\
             from effort: the harness's default\n\
             to: Pi\n\
             to model: openai/gpt-6\n\
             to effort: high\n\
             summary: yes\n\
             by: o/r#5\n\
             ```"
    );
    assert_eq!(
        posts[1].1,
        "🤖 ssf <!-- ssf: origin=o/r#5 event=attached -->\n\n\
             ```ssf\n\
             ssf attaching agent to issue:\n\
             harness: Pi\n\
             model: openai/gpt-6\n\
             effort: high\n\
             driver: orca\n\
             branch: bot/issue-5\n\
             handed over from: Claude Code\n\
             ```"
    );
}
