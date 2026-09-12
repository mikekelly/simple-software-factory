use super::*;

#[test]
fn keys_prefer_ids_and_fall_back_sensibly() {
    assert_eq!(
        event_key(&json!({"event":"commented","id":42})).unwrap(),
        "commented:42"
    );
    assert_eq!(
        event_key(&json!({"event":"committed","sha":"abc"})).unwrap(),
        "committed:abc"
    );
    assert_eq!(
            event_key(&json!({"event":"cross-referenced","created_at":"t","source":{"issue":{"html_url":"u"}}})).unwrap(),
            "cross-referenced:u:t"
        );
    assert!(event_key(&json!({"id": 1})).is_none());
}

#[test]
fn renders_comment_with_quote_and_ignores_noise() {
    let ev = json!({"event":"commented","id":1,"user":{"login":"alice"},"created_at":"2026-01-01T00:00:00Z",
            "updated_at":"2026-01-01T00:00:00Z","body":"hello\nworld","html_url":"https://x/1"});
    let r = render_event(&ev, false, &cfg(), "bot").unwrap();
    assert!(r.text.contains("@alice commented"));
    assert!(r.text.contains("  > hello\n  > world"));
    let edited = render_event(&ev, true, &cfg(), "bot").unwrap();
    assert!(edited.text.contains("edited their comment"));
    let noise = json!({"event":"subscribed","id":2,"actor":{"login":"bob"}});
    assert!(render_event(&noise, false, &cfg(), "bot").is_none());
}

#[test]
fn timestamps_are_short_and_drop_todays_date() {
    assert_eq!(
        fmt_when("2026-09-04T17:40:02Z", "2026-09-05"),
        "2026-09-04 17:40Z"
    );
    assert_eq!(fmt_when("2026-09-05T09:03:59Z", "2026-09-05"), "09:03Z");
    assert_eq!(fmt_when("t", "2026-09-05"), "t");
    assert_eq!(fmt_when("", "2026-09-05"), "");
    assert_eq!(fmt_when("2026-09-05T09", "2026-09-05"), "2026-09-05T09");
    assert_eq!(today_utc().len(), 10);
    let ev = json!({"event":"assigned","id":7,"actor":{"login":"carol"},"assignee":{"login":"bot"},
            "created_at":"2026-01-02T03:04:05Z"});
    let r = render_event(&ev, false, &cfg(), "bot").unwrap();
    assert_eq!(r.text, "- 2026-01-02 03:04Z @carol assigned @bot");
    let inline = json!({"event":"line-commented","comments":[{"id":8,"user":{"login":"alice"},"path":"a.rs",
            "line":3,"body":"typo","html_url":"u8","created_at":"2026-01-02T03:04:05Z"}]});
    let r = render_event(&inline, false, &cfg(), "bot").unwrap();
    assert!(
        r.text
            .starts_with("- 2026-01-02 03:04Z @alice commented on `a.rs` line 3 (u8):"),
        "{}",
        r.text
    );
}

#[test]
fn truncates_long_bodies() {
    let mut c = cfg();
    c.max_body_chars = 5;
    let ev = json!({"event":"commented","id":1,"user":{"login":"a"},"body":"0123456789"});
    let r = render_event(&ev, false, &c, "bot").unwrap();
    assert!(r.text.contains("01234"));
    assert!(r.text.contains("truncated"));
    assert!(!r.text.contains("56789"));
}

#[test]
fn worktree_names_are_slugged_and_bounded() {
    assert_eq!(
        worktree_name(7, "Fix the Login Bug!"),
        "issue-7-fix-the-login-bug"
    );
    assert_eq!(worktree_name(8, "   "), "issue-8");
    let long = worktree_name(9, &"a".repeat(200));
    assert!(long.len() <= "issue-9-".len() + 40);
}

#[test]
fn tags_are_stripped_from_bodies_and_shown_as_sessions() {
    let ev = json!({"event":"commented","id":1,"user":{"login":"bot"},"created_at":"t",
            "body":"🤖#9 <!-- ssf: origin=o/r#9 -->\n\ndone","html_url":"https://x/1"});
    let r = render_event(&ev, false, &cfg(), "bot").unwrap();
    assert!(
        r.text
            .contains("@bot commented (from the agent on o/r#9) (https://x/1):\n  > done"),
        "{}",
        r.text
    );
    assert!(!r.text.contains("<!--"));
    assert!(!r.text.contains("🤖"), "the byline goes with the tag");
    assert_eq!(r.origin.as_deref(), Some("o/r#9"));
    // A comment by the bot login with no tag was typed by a person.
    let ev = json!({"event":"commented","id":11,"user":{"login":"bot"},"created_at":"t",
            "body":"typed as the bot","html_url":"https://x/11"});
    let r = render_event(&ev, false, &cfg(), "bot").unwrap();
    assert!(
        r.text
            .contains("@bot commented (not from a session) (https://x/11):\n  > typed as the bot"),
        "{}",
        r.text
    );
    assert!(r.origin.is_none());
    // A tag at the end of the body (posts made before the byline) still
    // attributes the post to its session.
    let ev = json!({"event":"commented","id":12,"user":{"login":"bot"},"created_at":"t",
            "body":"old style\n\n<!-- ssf: origin=o/r#9 -->","html_url":"https://x/12"});
    let r = render_event(&ev, false, &cfg(), "bot").unwrap();
    assert!(
        r.text
            .contains("(from the agent on o/r#9) (https://x/12):\n  > old style"),
        "{}",
        r.text
    );
    assert_eq!(r.origin.as_deref(), Some("o/r#9"));
    let review = json!({"event":"reviewed","id":2,"user":{"login":"bot"},"state":"approved",
            "body":"<!-- ssf: origin=o/r#9 -->"});
    let r = render_event(&review, false, &cfg(), "bot").unwrap();
    assert!(
        r.text
            .ends_with("reviewed (approved) (from the agent on o/r#9)")
    );
    assert_eq!(r.origin.as_deref(), Some("o/r#9"));
    // A post by one of the reviewer sessions of before #115 (tagged
    // `role=reviewer`) reads as the item's session's.
    let review = json!({"event":"reviewed","id":3,"user":{"login":"bot"},"state":"changes_requested",
            "body":"<!-- ssf: origin=o/r#9 role=reviewer -->\n\nnits"});
    let r = render_event(&review, false, &cfg(), "bot").unwrap();
    assert!(
        r.text
            .contains("reviewed (changes_requested) (from the agent on o/r#9):\n  > nits")
    );
    assert_eq!(r.origin.as_deref(), Some("o/r#9"));
    let inline = json!({"event":"line-commented","comments":[{"id":8,"user":{"login":"bot"},"path":"a.rs",
            "line":3,"body":"<!-- ssf: origin=o/r#9 role=reviewer -->\n\ntypo","html_url":"u8"}]});
    let r = render_event(&inline, false, &cfg(), "bot").unwrap();
    assert!(
        r.text
            .contains("commented on `a.rs` line 3 (from the agent on o/r#9)")
    );
    assert_eq!(r.origin.as_deref(), Some("o/r#9"));
    // A human quoting a bot comment is not "from a session".
    let human = json!({"event":"commented","id":5,"user":{"login":"alice"},"created_at":"t",
            "body":"> <!-- ssf: origin=o/r#9 -->\n\nthanks","html_url":"https://x/5"});
    let r = render_event(&human, false, &cfg(), "bot").unwrap();
    assert!(r.text.contains("@alice commented (https://x/5)"));
    assert!(r.origin.is_none());
    assert!(
        r.text.contains("<!-- ssf: origin=o/r#9 -->"),
        "quoted text is shown verbatim"
    );

    let issue: Issue = serde_json::from_value(json!({
            "number": 4, "title": "PR", "body": "<!-- ssf: origin=o/r#3 -->\n\nFixes it", "html_url": "https://gh/4",
            "state": "open", "user": {"login": "bot"}, "created_at": "t", "updated_at": "t"
        })).unwrap();
    let repo = RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        ..Default::default()
    };
    let d = cfg();
    let ctx = PromptContext {
        repo: &repo,
        daemon: &d,
        bot_login: "bot",
        driver: DriverKind::Orca,
        pr: None,
        triggers: &[],
        owner: None,
        delegated_by: None,
        handed_over_from: None,
        projects: &[],
        project_prompt: None,
        harness_prompt: None,
        vm_guest: false,
        pushes_as: None,
    };
    let p = initial_prompt(&issue, &[], &ctx);
    assert!(
        p.contains("\n\nOpened by @bot (from the agent on o/r#3) on t.\n"),
        "{p}"
    );
    assert!(p.contains("## Description\n\nFixes it\n\n## Activity"));
}
