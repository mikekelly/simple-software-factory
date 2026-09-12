use super::*;

/// Every message kind, rendered with the fixtures the catalogue on #16
/// used (issue #18, PR #22, issue #21, the board, `SSF.md`), with sizes.
/// Run with `cargo test prompt_catalogue -- --ignored --nocapture` to
/// measure a wording change; nothing is asserted.
#[test]
#[ignore = "prints the prompt catalogue with sizes"]
fn prompt_catalogue() {
    use crate::github::StatusOption;
    let d = cfg();
    let bot = "OverlayBot";
    let repo = RepoConfig {
        name: "mikekelly/simple-software-factory".into(),
        harness: "claude".into(),
        ..Default::default()
    };
    let base = "https://github.com/mikekelly/simple-software-factory";
    let issue18: Issue = serde_json::from_value(json!({
            "number": 18,
            "title": "Resume sessions after a machine restart: startup reconciliation pass",
            "body": "After a reboot Orca's terminals are gone and ssf only notices when the next GitHub event arrives.\n\nProposal: a startup reconciliation pass that resumes every active session whose worktree exists but has no live agent terminal.",
            "html_url": format!("{base}/issues/18"), "state": "open", "state_reason": "completed",
            "user": {"login": bot}, "labels": [{"name": "daemon"}],
            "created_at": "2026-09-04T13:56:29Z", "updated_at": "2026-09-04T17:29:10Z"
        })).unwrap();
    let pr22_title = "Comment by default, tell as the exception: prompt and README wording";
    let pr22: Issue = serde_json::from_value(json!({
            "number": 22, "title": pr22_title,
            "body": "<!-- ssf: origin=mikekelly/simple-software-factory#21 -->\n\nCloses #21\n\nReworded the peers/sub/tell paragraph.",
            "html_url": format!("{base}/pull/22"), "state": "open", "pull_request": {},
            "user": {"login": bot},
            "created_at": "2026-09-04T17:17:00Z", "updated_at": "2026-09-04T17:17:00Z"
        })).unwrap();
    let pr = PrInfo {
        head_ref: "mikekelly/issue-21-comment-by-default-tell-as-the-exception".into(),
        head_repo: "mikekelly/simple-software-factory".into(),
        base_ref: "master".into(),
        ..Default::default()
    };
    let issue21: Issue = serde_json::from_value(json!({
        "number": 21, "title": pr22_title, "body": "",
        "html_url": format!("{base}/issues/21"), "state": "closed", "state_reason": "completed",
        "user": {"login": bot},
        "created_at": "2026-09-04T17:10:00Z", "updated_at": "2026-09-04T18:10:00Z"
    }))
    .unwrap();
    let boards = vec![ProjectCard {
        project_id: "PVT_kwHN2ebOAYlXgQ".into(),
        title: "Simple Software Factory".into(),
        url: "https://github.com/users/mikekelly/projects/5".into(),
        item_id: "PVTI_lAHN2ebOAYlXgc4OXRyA".into(),
        status: Some("Todo".into()),
        status_field_id: Some("PVTSSF_lAHN2ebOAYlXgc4YTeZE".into()),
        status_options: [
            ("Longrunners", "6ea12c8d"),
            ("Todo", "f75ad846"),
            ("In Progress", "47fc9ee4"),
            ("Done", "98236657"),
        ]
        .iter()
        .map(|(n, i)| StatusOption {
            id: i.to_string(),
            name: n.to_string(),
        })
        .collect(),
    }];
    let notes = ProjectPrompt {
            source: "SSF.md".into(),
            text: "# Notes for ssf agents\n\n\
- Work on the issue's branch and open a PR that references the issue.\n\
- Keep `cargo test` green and run `cargo fmt` and `cargo clippy` before pushing.\n\
- Update `README.md` and `config.example.toml` for any user-visible behaviour.\n\
- Rebuild the package with `cd packaging && makepkg -fd` before calling something done; commit the `pkgver` bump makepkg makes to `packaging/PKGBUILD`.\n\
- The installed service runs the last package the maintainer installed, so verify daemon behaviour with unit tests and scratch `SSF_CONFIG_DIR`/`SSF_STATE_DIR` runs rather than expecting to see your change live."
                .into(),
        };
    let back = LoginBack {
        harness: "Claude Code",
        since: "2026-09-04T17:29:10Z",
        number: 18,
        title: &issue18.title,
        url: &issue18.html_url,
    };
    let render = |v: Value| render_event(&v, false, &d, bot).unwrap();
    let ev = |kind: &str, actor: &str, at: &str, extra: Value| {
        let mut v = json!({"event": kind, "id": 1, "actor": {"login": actor}, "created_at": at});
        if let (Some(m), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            for (k, val) in e {
                m.insert(k.clone(), val.clone());
            }
        }
        render(v)
    };
    let added = ev(
        "added_to_project_v2",
        bot,
        "2026-09-04T13:56:29Z",
        json!({}),
    );
    let assigned = ev(
        "assigned",
        "mikekelly",
        "2026-09-04T17:29:10Z",
        json!({"assignee": {"login": bot}}),
    );
    let comment = ev(
        "commented",
        "mikekelly",
        "2026-09-04T17:40:02Z",
        json!({"html_url": format!("{base}/issues/18#issuecomment-1"),
                "body": "Please stagger the relaunches by at least ten seconds; nine Claude sessions starting at once will thrash the machine."}),
    );
    let requested = ev(
        "review_requested",
        "mikekelly",
        "2026-09-04T17:45:00Z",
        json!({"requested_reviewer": {"login": bot}}),
    );
    let closed = ev(
        "closed",
        "mikekelly",
        "2026-09-04T18:00:00Z",
        json!({"state_reason": "completed"}),
    );
    let unassigned = ev(
        "unassigned",
        "mikekelly",
        "2026-09-04T18:00:00Z",
        json!({"assignee": {"login": bot}}),
    );
    let reassigned = ev(
        "assigned",
        "mikekelly",
        "2026-09-04T18:05:00Z",
        json!({"assignee": {"login": bot}}),
    );
    let bot_closed = ev(
        "closed",
        bot,
        "2026-09-04T18:10:00Z",
        json!({"state_reason": "completed"}),
    );

    let assigned_t = vec!["assigned".to_string()];
    let created_t = vec!["created".to_string()];
    let delegated_t = vec!["assigned".to_string(), "created".to_string()];
    let own = PromptContext {
        repo: &repo,
        daemon: &d,
        bot_login: bot,
        driver: DriverKind::Orca,
        pr: None,
        triggers: &assigned_t,
        owner: None,
        delegated_by: None,
        handed_over_from: None,
        projects: &boards,
        project_prompt: Some(notes.clone()),
        harness_prompt: None,
        vm_guest: false,
        pushes_as: None,
    };
    let owned_pr = PromptContext {
        pr: Some(&pr),
        triggers: &created_t,
        owner: Some(21),
        projects: &[],
        project_prompt: None,
        harness_prompt: None,
        vm_guest: false,
        pushes_as: None,
        ..own.clone()
    };
    let child = PromptContext {
        triggers: &delegated_t,
        delegated_by: Some("mikekelly/simple-software-factory#16"),
        handed_over_from: None,
        ..own.clone()
    };
    let handed_over = PromptContext {
        handed_over_from: Some("Claude Code"),
        ..own.clone()
    };
    let sub = PromptContext {
        projects: &[],
        project_prompt: None,
        harness_prompt: None,
        vm_guest: false,
        pushes_as: None,
        ..own.clone()
    };
    let final_comment = FinalComment {
        author: bot.into(),
        session: Some("mikekelly/simple-software-factory#21".into()),
        url: format!("{base}/issues/21#issuecomment-5544055473"),
        body: "Done: PR #22. Prompt reworded, README updated, pkgver bumped.".into(),
    };
    let owner21 = Some("mikekelly/simple-software-factory#21");
    let catalogue: Vec<(&str, String)> = vec![
        (
            "initial_prompt (issue, assigned, on a board, repo has SSF.md)",
            initial_prompt(&issue18, &[added.clone(), assigned.clone()], &own),
        ),
        (
            "tracked_prompt (session's own PR picked up, no human trigger)",
            tracked_prompt(&pr22, &[], &owned_pr),
        ),
        (
            "followup_prompt (new activity on the session's item)",
            followup_prompt(&issue18, std::slice::from_ref(&comment), &own),
        ),
        (
            "followup_prompt on an owned PR (review requested from the bot)",
            followup_prompt(&pr22, std::slice::from_ref(&requested), &owned_pr),
        ),
        (
            "initial_prompt for a delegated (handed-off) issue",
            initial_prompt(&issue18, &[], &child),
        ),
        (
            "closed_prompt (the session's issue was closed)",
            closed_prompt(&issue18, std::slice::from_ref(&closed), &own),
        ),
        (
            "unassigned_prompt",
            unassigned_prompt(&issue18, std::slice::from_ref(&unassigned), &own),
        ),
        (
            "reassigned_prompt",
            reassigned_prompt(&issue18, std::slice::from_ref(&reassigned), &own),
        ),
        (
            "delegated_closed_prompt (parent hears its hand-off finished)",
            delegated_closed_prompt(&issue21, false, Some(&final_comment), &sub),
        ),
        (
            "fyi_prompt (subscriber sees activity on someone else's item)",
            fyi_prompt(
                &issue21,
                std::slice::from_ref(&comment),
                &sub,
                owner21,
                false,
                Fyi::Activity,
            ),
        ),
        (
            "fyi_prompt (subscribed item closed)",
            fyi_prompt(
                &issue21,
                std::slice::from_ref(&bot_closed),
                &sub,
                owner21,
                false,
                Fyi::Closed,
            ),
        ),
        (
            "tell_prompt (message from another session)",
            tell_prompt(
                Some("mikekelly/simple-software-factory#16"),
                Some("Project management"),
                "master moved after you branched (#14 and #13 merged); please rebase onto origin/master before pushing again.",
                d.max_body_chars,
            ),
        ),
        (
            "handover_prompt (a session handed the item over, with a summary)",
            handover_prompt(
                "Claude Code",
                "issue",
                Some(
                    "Branch `bot/issue-18-resume-sessions` is pushed and PR #22 is open \
against it. The startup pass and its tests are done; what is left is the \
`startup_orca_wait_secs` option and the README section. `cargo test` is green; the packaging \
bump is not done.",
                ),
                &initial_prompt(&issue18, &[added.clone(), assigned.clone()], &handed_over),
            ),
        ),
        (
            "handover_prompt (handed over with no summary)",
            handover_prompt(
                "Claude Code",
                "issue",
                None,
                &initial_prompt(&issue18, &[added.clone(), assigned.clone()], &handed_over),
            ),
        ),
        (
            "handover_refused_prompt (the daemon could not carry it out)",
            handover_refused_prompt("Pi", "the item is no longer active"),
        ),
        (
            "handover_cancelled_prompt (`ssf handover --cancel`)",
            handover_cancelled_prompt("Pi"),
        ),
        (
            "login_back_prompt (the harness was signed in again)",
            login_back_prompt(&back),
        ),
        (
            "start_again_prompt (the harness would not start, and now has)",
            start_again_prompt(&back),
        ),
        (
            "tell_prompt (from a human shell, no session)",
            tell_prompt(None, None, "stop, I'm changing the spec", d.max_body_chars),
        ),
    ];
    println!("\n| # | prompt | chars | ~tokens |\n|---|---|---|---|");
    let mut total = 0;
    for (i, (name, text)) in catalogue.iter().enumerate() {
        let n = text.chars().count();
        total += n;
        println!("| {} | {name} | {n} | ~{} |", i + 1, n / 4);
    }
    println!("| | total | {total} | ~{} |", total / 4);
    for (i, (name, text)) in catalogue.iter().enumerate() {
        println!(
            "\n<details>\n<summary><b>{}. {name}</b> ({} chars)</summary>\n\n```text\n{text}\n```\n\n</details>",
            i + 1,
            text.chars().count()
        );
    }
}
