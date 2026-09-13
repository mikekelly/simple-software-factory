use super::*;

#[test]
fn first_prompt_names_the_driver() {
    let issue: Issue = serde_json::from_value(serde_json::json!({
        "number": 3, "title": "T", "html_url": "https://x/3", "body": "", "state": "open",
        "user": {"login": "h"}, "labels": [], "assignees": [],
        "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    let repo = RepoConfig {
        name: "o/r".into(),
        github_id: None,
        aliases: Vec::new(),
        harness: "claude".into(),
        ..Default::default()
    };
    let d = DaemonConfig::default();
    let triggers = vec!["assigned".to_string()];
    let mut ctx = PromptContext {
        repo: &repo,
        daemon: &d,
        bot_login: "bot",
        driver: DriverKind::Herdr,
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
    assert!(instructions(&issue, &ctx).contains("through the herdr multiplexer"));
    ctx.driver = DriverKind::Orca;
    assert!(instructions(&issue, &ctx).contains("through the Orca multiplexer"));
}

#[test]
fn initial_prompt_mentions_bot_and_issue() {
    let issue: Issue = serde_json::from_value(json!({
            "number": 3, "title": "Add thing", "body": "Please add", "html_url": "https://gh/3", "state": "open",
            "user": {"login": "carol"}, "assignees": [{"login":"bot"}], "labels": [{"name":"feature"}],
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
        })).unwrap();
    let repo = RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        github_id: None,
        aliases: Vec::new(),
        driver: None,
        model: None,
        effort: None,
        command: None,
        clone_url: None,
        path: None,
        base_branch: None,
        conflict_check_interval_secs: None,
        instructions: Some("Run the tests.".into()),
        prompt_file: None,
        allowed_users: None,
        accepted_anyone_risk: false,
        event_comments: None,
        git: Default::default(),
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
    assert!(p.starts_with(
            "# GitHub issue #3: Add thing\nhttps://gh/3\n\nOpened by @carol on 2026-01-01 00:00Z. Labels: feature.\n"
        ), "{p}");
    // The item is named once: the header has the URL, the reason has `#3`.
    assert_eq!(p.matches("https://gh/3").count(), 1);
    assert!(!p.contains("o/r#3"));
    assert!(p.contains("(no activity yet)"));
    assert!(
        p.contains(
            "## How to work on this\n\nYou are an automatically spawned coding agent for the \
GitHub account @bot. Simple Software Factory (ssf) spawned you, through the Orca multiplexer, \
in a worktree of this repository, because #3 was assigned to @bot.\n\n\
New activity on it arrives here as messages prefixed `[ssf]`; act on them. `ssf guide` \
explains the rest.\n\n\
- This terminal is unmanned: nobody reads it, so everything you want a person to see goes on \
GitHub.\n\
- Collaborate with humans and other ssf-managed agents through GitHub comments on the issue.\n\
- Before starting on a goal, say on the issue what you are about to do, and say when you need a \
decision or have delivered: silent work leaves the issue looking unattended until it lands.\n\
- `gh` and `git push` already act as @bot, and the `gh` on your PATH marks your posts as this \
session's. Act only as @bot; never use another account, token or key you find on this \
machine.\n"
        ),
        "{p}"
    );
    // Branches and worktrees are the agent's own business, the byline's
    // mechanics are the guide's, and the PR conventions (reference the
    // issue, do not close it, do not merge) are the repository's.
    for dropped in [
        "Closes #3",
        "on its branch",
        "not yours to touch",
        "must start with the line",
        "<!-- ssf: origin=",
        "Do not close",
        "Do not merge",
        "open a pull request",
        "GH_TOKEN",
        "SSF_ISSUE",
    ] {
        assert!(
            !p.contains(dropped),
            "{dropped} is no longer the prompt's to say:\n{p}"
        );
    }
    // The reference lives behind `ssf guide`; the prompt only points at it.
    assert!(p.contains("`ssf guide` explains the rest"));
    for moved in [
        "ssf peers",
        "ssf sub",
        "ssf tell",
        "--assignee",
        "reviewer session",
        "mode=delegate",
    ] {
        assert!(
            !p.contains(moved),
            "{moved} belongs in the guide, not the prompt"
        );
    }
    // Advice about how to work is the repository's to give (SSF.md).
    for advice in [
        "commit as you go",
        "Post a short comment",
        "rather than guessing",
        "careful colleague",
    ] {
        assert!(!p.contains(advice), "{advice} is advice, not a rule");
    }
    assert!(!p.contains("handed off to you"));
    assert!(p.trim_end().ends_with("Run the tests."));

    let ctx = PromptContext {
        project_prompt: Some(ProjectPrompt {
            source: "SSF.md".into(),
            text: "Cards go to Review when a PR is open.".into(),
        }),
        vm_guest: false,
        pushes_as: None,
        ..ctx
    };
    let p = initial_prompt(&issue, &[], &ctx);
    assert!(p.contains("Run the tests.\n\n## Project notes (`SSF.md`)\n\nCards go to Review"));
    assert!(!p.contains("They say"));
    let ctx = PromptContext {
        harness_prompt: Some(ProjectPrompt {
            source: "SSF.codex.md".into(),
            text: "Use native subagents.".into(),
        }),
        ..ctx
    };
    let p = initial_prompt(&issue, &[], &ctx);
    assert!(p.contains("Cards go to Review when a PR is open.\n\n## Project notes (`SSF.codex.md`)\n\nUse native subagents."));
    let harness_only = PromptContext {
        project_prompt: None,
        ..ctx.clone()
    };
    let p = initial_prompt(&issue, &[], &harness_only);
    assert!(p.contains("Use native subagents."));
    assert!(!p.contains("Cards go to Review"));
    let triggers = vec!["assigned".to_string(), "created".to_string()];
    let child = PromptContext {
        triggers: &triggers,
        delegated_by: Some("o/r#1"),
        handed_over_from: None,
        ..ctx
    };
    let p = initial_prompt(&issue, &[], &child);
    assert!(p.contains(
            "because #3 was assigned to @bot and was opened by the agent session working on o/r#1 and handed off to you."
        ));
    // The bullet does not say "handed off" again; the reason did.
    assert!(p.contains(
        "- The session on o/r#1, which handed this off, follows the issue as a subscriber"
    ));
    assert_eq!(p.matches("handed").count(), 2, "{p}");
    assert!(p.contains("To ask it something, comment on this issue."));
}

#[test]
fn initial_prompt_is_the_bare_minimum() {
    use crate::github::StatusOption;
    let issue: Issue = serde_json::from_value(json!({
        "number": 24, "title": "Prompts: bare functional minimum", "body": "Cut the prompts down.",
        "html_url": "https://github.com/o/r/issues/24", "state": "open",
        "user": {"login": "carol"}, "created_at": "2026-09-04T20:03:47Z", "updated_at": "t"
    }))
    .unwrap();
    let repo = RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        ..Default::default()
    };
    let d = cfg();
    let boards = vec![ProjectCard {
        project_id: "PVT_kwHN2ebOAYlXgQ".into(),
        title: "Simple Software Factory".into(),
        url: "https://github.com/users/o/projects/5".into(),
        item_id: "PVTI_lAHN2ebOAYlXgc4OYASe".into(),
        status: Some("In Progress".into()),
        status_field_id: Some("PVTSSF_lAHN2ebOAYlXgc4YTeZE".into()),
        status_options: ["Longrunners", "Todo", "In Progress", "Done"]
            .iter()
            .map(|n| StatusOption {
                id: "6ea12c8d".into(),
                name: n.to_string(),
            })
            .collect(),
    }];
    let triggers = vec!["assigned".to_string()];
    let ctx = PromptContext {
        repo: &repo,
        daemon: &d,
        bot_login: "OverlayBot",
        driver: DriverKind::Orca,
        pr: None,
        triggers: &triggers,
        owner: None,
        delegated_by: None,
        handed_over_from: None,
        projects: &boards,
        project_prompt: None,
        harness_prompt: None,
        vm_guest: false,
        pushes_as: None,
    };
    let ev = Rendered {
        key: "k".into(),
        text: "- [2026-09-04T20:45:16Z] @OverlayBot assigned @OverlayBot".into(),
        origin: None,
        assignee: None,
    };
    let p = initial_prompt(&issue, &[ev], &ctx);
    assert!(
        p.chars().count() < 2500,
        "initial prompt is {} chars:\n{p}",
        p.chars().count()
    );
    // The board rule sits with the boards, not among the instructions.
    let boards = &p[p.find("## Project boards").unwrap()..p.find("## Description").unwrap()];
    assert!(boards.contains("Keep the card's Status accurate; which column fits is your call."));
    let how = &p[p.find("## How to work on this").unwrap()..];
    assert!(!how.contains("card"));
}

#[test]
fn a_person_credential_names_who_pushes() {
    let issue: Issue = serde_json::from_value(serde_json::json!({
        "number": 16, "title": "T", "html_url": "https://x/16", "body": "", "state": "open",
        "user": {"login": "h"}, "labels": [], "assignees": [],
        "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    let repo = RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        ..Default::default()
    };
    let daemon = DaemonConfig::default();
    let triggers = vec!["assigned".to_string()];
    let mut ctx = PromptContext {
        repo: &repo,
        daemon: &daemon,
        bot_login: "bot",
        driver: DriverKind::Herdr,
        pr: None,
        triggers: &triggers,
        owner: None,
        delegated_by: None,
        handed_over_from: None,
        projects: &[],
        project_prompt: None,
        harness_prompt: None,
        vm_guest: false,
        pushes_as: Some("@ann".into()),
    };
    let p = instructions(&issue, &ctx);
    assert!(
        p.contains("- `gh` already acts as @bot and `git push` as @ann, and the `gh` on your PATH"),
        "{p}"
    );
    assert!(
        p.contains("Act only through those; never use another account"),
        "{p}"
    );
    assert!(!p.contains("Act only as @bot"), "{p}");
    ctx.pushes_as = None;
    let p = instructions(&issue, &ctx);
    assert!(
        p.contains("- `gh` and `git push` already act as @bot, and"),
        "{p}"
    );
    assert!(p.contains("Act only as @bot;"), "{p}");
    // The other credential kinds get a description rather than a login.
    assert_eq!(
        crate::config::Credential::File("/t".into())
            .prompt_pusher()
            .unwrap(),
        "the account whose token is in `/t`"
    );
    assert!(
        crate::config::Credential::Helper("store".into())
            .prompt_pusher()
            .unwrap()
            .contains("`store`")
    );
    assert!(crate::config::Credential::Bot.prompt_pusher().is_none());
}

#[test]
fn vm_guest_gets_one_line_about_root() {
    let issue: Issue = serde_json::from_value(serde_json::json!({
        "number": 3, "title": "T", "html_url": "https://x/3", "body": "", "state": "open",
        "user": {"login": "h"}, "labels": [], "assignees": [],
        "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    let repo = RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        ..Default::default()
    };
    let daemon = DaemonConfig::default();
    let triggers = vec!["assigned".to_string()];
    let mut ctx = PromptContext {
        repo: &repo,
        daemon: &daemon,
        bot_login: "bot",
        driver: DriverKind::Herdr,
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
    assert!(!instructions(&issue, &ctx).contains("sudo"));
    assert!(!guide("bot", false).contains("sudo"));
    ctx.vm_guest = true;
    let text = instructions(&issue, &ctx);
    assert!(text.contains(&format!("- {VM_GUEST_LINE}\n")), "{text}");
    assert_eq!(text.matches("sudo").count(), 1);
    let g = guide("bot", true);
    assert!(g.contains(VM_GUEST_LINE));
    assert_eq!(g.matches("sudo").count(), 1);
}

#[test]
fn guide_holds_the_moved_reference() {
    let g = guide("bot", false);
    assert!(g.starts_with("# ssf guide\n\n"));
    assert!(g.contains("`ssf peers` lists the agent sessions"));
    assert!(!g.contains("Leave their branches and workspaces alone"));
    assert!(
        g.contains("Use `Refs #N` to link a pull request to ongoing management or tracking work.")
    );
    assert!(g.contains("Use `Closes #N` only when merging completes the entire issue"));
    assert!(g.contains("needs it added by hand, as the first line of the body"));
    assert!(g.contains("To speak to the agent on another item, comment on that item with `gh`"));
    assert!(g.contains("`ssf tell <n> \"message\"`"));
    assert!(g.contains("\"master moved, rebase\", \"terminal is being replaced\""));
    assert!(g.contains("a session whose item is already closed"));
    assert!(g.contains("`ssf sub <n>`"));
    assert!(g.contains("`ssf unsub <n>` stops them; `ssf subs` lists"));
    assert!(g.contains("from the agent on owner/repo#M"));
    assert!(g.contains("`--assignee bot` in the same `gh ... create` command"));
    assert!(g.contains("You are subscribed to it automatically"));
    // One session per item: no reviewer, no label, no role; a second
    // opinion is the session's own to arrange, with the herdr recipe.
    assert!(g.contains("## Second opinions"));
    assert!(g.contains("ssf runs one session per item and starts no reviewer for your work"));
    assert!(g.contains("A subagent of your own harness is the default."));
    assert!(
        g.contains("`herdr workspace create --cwd \"$PWD\" --label second-opinion --no-focus`")
    );
    assert!(g.contains("`herdr agent start second-opinion --kind <kind> --pane <pane>`"));
    assert!(g.contains("`herdr agent send-keys <pane> down`"));
    assert!(g.contains("`herdr agent prompt <pane> \"<brief>\" --wait`"));
    assert!(g.contains("`herdr pane read <pane> --lines 200 --format text`"));
    assert!(g.contains("`herdr workspace close <id>`"));
    assert!(g.contains("`ssf peers` may show it as your session until it is closed"));
    for gone in [
        "reviewer session",
        "SSF_ROLE",
        ":reviewer",
        "`review` label",
        "--add-reviewer",
        "role=reviewer",
        "(reviewer)",
    ] {
        assert!(!g.contains(gone), "{gone} is gone with #115:\n{g}");
    }
    assert!(g.contains("<!-- ssf: origin=owner/repo#N -->"));
}

#[test]
fn initial_prompt_lists_project_boards_without_prescribing_columns() {
    use crate::github::StatusOption;
    let issue: Issue = serde_json::from_value(json!({
            "number": 3, "title": "Add thing", "body": "Please add", "html_url": "https://gh/3", "state": "open",
            "user": {"login": "carol"}, "created_at": "t", "updated_at": "t"
        })).unwrap();
    let repo = RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        ..Default::default()
    };
    let d = cfg();
    let boards = vec![
        ProjectCard {
            project_id: "PVT_1".into(),
            title: "Roadmap".into(),
            url: "https://gh/p/1".into(),
            item_id: "PVTI_1".into(),
            status: Some("Todo".into()),
            status_field_id: Some("PVTSSF_1".into()),
            status_options: vec![
                StatusOption {
                    id: "a1".into(),
                    name: "Todo".into(),
                },
                StatusOption {
                    id: "b2".into(),
                    name: "In Progress".into(),
                },
            ],
        },
        ProjectCard {
            project_id: "PVT_2".into(),
            title: "Bare".into(),
            url: "https://gh/p/2".into(),
            item_id: "PVTI_2".into(),
            ..Default::default()
        },
    ];
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
        projects: &boards,
        project_prompt: None,
        harness_prompt: None,
        vm_guest: false,
        pushes_as: None,
    };
    let p = initial_prompt(&issue, &[], &ctx);
    assert!(p.contains("## Project boards\n\n- Roadmap (https://gh/p/1): Status is \"Todo\". Options: \"Todo\", \"In Progress\".\n"));
    assert!(p.contains(
            "`gh project item-edit --project-id PVT_1 --id PVTI_1 --field-id PVTSSF_1 --single-select-option-id <option id>`, where \"Todo\" = a1, \"In Progress\" = b2."
        ));
    assert!(p.contains(
        "- Bare (https://gh/p/2): this board has no Status field.\n\nKeep the card's Status \
accurate; which column fits is your call.\n\n## Description"
    ));
    assert!(!p.contains("never moves cards"));
    assert!(p.find("## Project boards").unwrap() < p.find("## Description").unwrap());
    // No column is prescribed for any situation: the option names appear
    // only in the board listing, never in the instructions.
    let how = &p[p.find("## How to work on this").unwrap()..];
    assert!(!how.contains("card"));
    assert!(!how.contains("Todo") && !how.contains("In Progress"));

    let none = PromptContext {
        projects: &[],
        ..ctx
    };
    let p = initial_prompt(&issue, &[], &none);
    assert!(!p.contains("Project boards"));
    assert!(!p.contains("gh project item-edit"));
}

#[test]
fn project_prompt_is_read_from_the_worktree() {
    let dir = std::env::temp_dir().join(format!(
        "ssf-prompt-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join(".ssf")).unwrap();
    let repo = RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        ..Default::default()
    };
    assert_eq!(ProjectPrompt::load(&repo, &dir), None, "no file, no notes");
    std::fs::write(dir.join("SSF.md"), "  \n").unwrap();
    assert_eq!(
        ProjectPrompt::load(&repo, &dir),
        None,
        "blank file, no notes"
    );
    std::fs::write(dir.join("SSF.md"), "\n# Notes\n\nBe brief.\n\n").unwrap();
    assert_eq!(
        ProjectPrompt::load(&repo, &dir),
        Some(ProjectPrompt {
            source: "SSF.md".into(),
            text: "# Notes\n\nBe brief.".into()
        })
    );

    std::fs::write(dir.join(".ssf/prompt.md"), "From the dotdir.").unwrap();
    let repo = RepoConfig {
        prompt_file: Some(".ssf/prompt.md".into()),
        ..repo
    };
    let pp = ProjectPrompt::load(&repo, &dir).unwrap();
    assert_eq!(pp.source, ".ssf/prompt.md");
    assert_eq!(pp.text, "From the dotdir.");

    let outside = dir.join("elsewhere.md");
    std::fs::write(&outside, "Absolute.").unwrap();
    let repo = RepoConfig {
        prompt_file: Some(outside.to_string_lossy().to_string()),
        ..repo
    };
    assert_eq!(
        ProjectPrompt::load(&repo, Path::new("/nonexistent"))
            .unwrap()
            .text,
        "Absolute."
    );
    assert_eq!(ProjectPrompt::load_harness(&repo, &dir, "codex"), None);
    for empty in ["  \n", "<!-- private instructions -->"] {
        std::fs::write(dir.join("SSF.codex.md"), empty).unwrap();
        assert_eq!(ProjectPrompt::load_harness(&repo, &dir, "codex"), None);
    }
    std::fs::write(dir.join("SSF.codex.md"), "<!-- private -->\nCodex notes.").unwrap();
    std::fs::write(dir.join("SSF.claude.md"), "Claude notes.").unwrap();
    // The shared file remains absolute; harness files still come from
    // the checkout root, and only the selected harness is included.
    assert_eq!(
        ProjectPrompt::load_harness(&repo, &dir, "codex"),
        Some(ProjectPrompt {
            source: "SSF.codex.md".into(),
            text: "Codex notes.".into(),
        })
    );
    assert_eq!(ProjectPrompt::load_harness(&repo, &dir, "pi"), None);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn html_comments_in_the_notes_do_not_reach_the_agent() {
    let notes = "# Notes\n\n<!--\nfor the person editing this\n-->\n\n- Autonomy: a person \
approves everything.\n  <!-- the other end reads: no approval is needed -->\n- Commit as you go.\n";
    assert_eq!(
        without_html_comments(notes),
        (
            "# Notes\n\n- Autonomy: a person approves everything.\n- Commit as you go.".into(),
            None
        )
    );
    assert_eq!(
        without_html_comments("  \n<!-- only a comment -->\n"),
        (String::new(), None)
    );
    // An unclosed comment swallows the rest, and says which line opened it.
    assert_eq!(
        without_html_comments("a\nb <!-- unterminated\nc"),
        ("a\nb".into(), Some(2))
    );
    assert_eq!(
        without_html_comments("plain\n\ntext\n"),
        ("plain\n\ntext".into(), None)
    );
    let source = include_str!("../../../SSF.example.md");
    let (heading, rest) = source.split_once("<!--").unwrap();
    let (_, rules) = rest.split_once("-->").unwrap();
    assert!(!heading.contains("- ") && !rules.contains("<!--"));
    let expected = format!("{}\n\n{}", heading.trim(), rules.trim());
    let (example, unclosed) = without_html_comments(source);
    assert!(unclosed.is_none());
    assert_eq!(example, expected, "every shipped instruction must survive");
    let source = include_str!("../../../SSF.md");
    let (notes, unclosed) = without_html_comments(source);
    assert!(unclosed.is_none());
    assert_eq!(notes, source.trim());
}
