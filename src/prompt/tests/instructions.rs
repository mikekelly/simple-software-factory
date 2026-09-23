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
    let ctx = PromptContext {
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
        global_prompt: None,
        global_harness_prompt: None,
        project_prompt: None,
        harness_prompt: None,
        vm_guest: false,
        pushes_as: None,
    };
    assert!(instructions(&issue, &ctx).contains("through the herdr multiplexer"));
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
        enrolled_at: None,
        github_id: None,
        aliases: Vec::new(),
        driver: None,
        model: None,
        effort: None,
        auto_compaction_tokens: None,
        command: None,
        clone_url: None,
        path: None,
        base_branch: None,
        conflict_check_interval_secs: None,
        first_prompt_max_events: None,
        first_prompt_max_chars: None,
        instructions: Some("Run the tests.".into()),
        prompt_file: None,
        allowed_users: None,
        accepted_anyone_risk: false,
        event_comments: None,
        item_pane_input: None,
        slash_commands: None,
        git: Default::default(),
    };
    let d = cfg();
    let ctx = PromptContext {
        repo: &repo,
        daemon: &d,
        bot_login: "bot",
        driver: DriverKind::Herdr,
        pr: None,
        triggers: &[],
        owner: None,
        delegated_by: None,
        handed_over_from: None,
        projects: &[],
        global_prompt: None,
        global_harness_prompt: None,
        project_prompt: None,
        harness_prompt: None,
        vm_guest: false,
        pushes_as: None,
    };
    let p = initial_prompt(&issue, &[], &ctx);
    let (how, item) = split_item(&p);
    assert!(
        p.starts_with("[ssf] Simple Software Factory (ssf) spawned you"),
        "{p}"
    );
    assert!(
        !p.starts_with('#'),
        "a leading # is a prompt action in OMP: {p}"
    );
    assert!(item.starts_with(
            "[ssf] GitHub issue #3: Add thing\nhttps://gh/3\n\nOpened by @carol on 2026-01-01 00:00Z. Labels: feature.\n"
        ), "{p}");
    // The item is named once: the header has the URL, the reason has `#3`.
    assert_eq!(p.matches("https://gh/3").count(), 1);
    assert!(!p.contains("o/r#3"));
    assert!(p.contains("(no activity yet)"));
    assert!(
        how.contains(
            "[ssf] Simple Software Factory (ssf) spawned you as a coding agent for the GitHub \
account @bot, through the herdr multiplexer, into a worktree of this repository, because #3 was \
assigned to @bot.\n\n\
## How to work on this\n\n\
You are a remote colleague working this issue to delivery: clarify on it until the outcome is \
unambiguous, deliver (a pull request, a review, an answer), and let the people on it decide and \
review on GitHub. New activity on it arrives here as messages prefixed `[ssf]`; act on them. \
This terminal is unmanned: what a person, or another session, should see goes on the issue as \
a GitHub comment. Say there what you are about to do, and when you need a decision or have \
delivered.\n\n\
- Posts are read on GitHub: write GitHub Flavored Markdown, link the exact lines you mean \
(pinned to a commit), and use tables, Mermaid diagrams, task lists, `<details>` for long output, \
and screenshots or wireframes where they make a decision easier.\n\
- `gh` and `git push` already act as @bot; your posts are marked as this session's. Act only as \
@bot; never use another account, token or key you find on this machine.\n\
- `--assignee bot` on a `gh` create gives the new item a session of its own; `ssf sub` follows \
another item; `ssf handover` passes this one to another harness; `ssf release` retires this \
workspace; `ssf doctor` checks the machine. `ssf guide` is the reference behind all of this.\n"
        ),
        "{p}"
    );
    assert!(item.contains("## Description\n\n  > Please add"), "{p}");
    assert!(!how.contains("## Description"), "{p}");
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
    // The prompt carries every affordance a session needs for coherent
    // work, since reading the guide is not guaranteed, and points at
    // `ssf guide` alone for the rest; `ssf skill` is an operator's index.
    assert!(p.contains("`ssf guide` is the reference behind all of this"));
    assert!(!p.contains("ssf skill"));
    for moved in [
        "ssf peers",
        "ssf subs",
        "reviewer session",
        "mode=delegate",
        "herdr agent",
    ] {
        assert!(
            !p.contains(moved),
            "{moved} belongs in the guide, not the prompt"
        );
    }
    // Advice about how to work is the repository's to give (SSF.md or AGENTS.md).
    for advice in [
        "commit as you go",
        "Post a short comment",
        "rather than guessing",
        "careful colleague",
    ] {
        assert!(!p.contains(advice), "{advice} is advice, not a rule");
    }
    assert!(!p.contains("handed off to you"));
    // The repository's own instructions close the guidance, before the
    // item begins.
    assert!(how.trim_end().ends_with("Run the tests."), "{p}");

    let ctx = PromptContext {
        global_prompt: Some(ProjectPrompt {
            source: "~/.ssf/SSF.md".into(),
            text: "This machine is private.".into(),
        }),
        global_harness_prompt: Some(ProjectPrompt {
            source: "~/.ssf/SSF.claude.md".into(),
            text: "Use the machine Claude account.".into(),
        }),
        project_prompt: Some(ProjectPrompt {
            source: "SSF.md".into(),
            text: "Cards go to Review when a PR is open.".into(),
        }),
        vm_guest: false,
        pushes_as: None,
        ..ctx
    };
    let p = initial_prompt(&issue, &[], &ctx);
    assert!(p.contains(
        "## Global SSF agent guidance (`~/.ssf/SSF.md`)\n\nThis machine is private.\n\n\
## Global harness guidance (`~/.ssf/SSF.claude.md`)\n\nUse the machine Claude account.\n\n\
Run the tests.\n\n## SSF agent guidance (`SSF.md`)\n\nCards go to Review"
    ));
    assert!(!p.contains("They say"));
    let ctx = PromptContext {
        harness_prompt: Some(ProjectPrompt {
            source: "SSF.codex.md".into(),
            text: "Use native subagents.".into(),
        }),
        ..ctx
    };
    let p = initial_prompt(&issue, &[], &ctx);
    assert!(p.contains("Cards go to Review when a PR is open.\n\n## Harness guidance (`SSF.codex.md`)\n\nUse native subagents."));
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
        "- The session on o/r#1 handed this off and follows it as a subscriber; your final \
comment is all it gets, so sum up the outcome."
    ));
    assert_eq!(p.matches("handed").count(), 2, "{p}");
}

/// #355: the operating contract comes before the material it applies to:
/// ssf's own prompt, then the guidance (operator, repository, `SSF.md`,
/// harness), then the item's header, boards, description and activity.
#[test]
fn the_prompt_states_the_rules_before_the_item() {
    let issue: Issue = serde_json::from_value(json!({
        "number": 7, "title": "Reorder", "body": "Body text", "html_url": "https://gh/7",
        "state": "open", "user": {"login": "carol"}, "labels": [],
        "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    let repo = RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        ..Default::default()
    };
    let d = cfg();
    let boards = vec![ProjectCard {
        title: "Roadmap".into(),
        url: "https://gh/p/1".into(),
        status: Some("Todo".into()),
        ..Default::default()
    }];
    let triggers = vec!["assigned".to_string()];
    let ctx = PromptContext {
        repo: &repo,
        daemon: &d,
        bot_login: "bot",
        driver: DriverKind::Herdr,
        pr: None,
        triggers: &triggers,
        owner: None,
        delegated_by: None,
        handed_over_from: None,
        projects: &boards,
        global_prompt: None,
        global_harness_prompt: None,
        project_prompt: Some(ProjectPrompt {
            source: "SSF.md".into(),
            text: "Own the issue through delivery.".into(),
        }),
        harness_prompt: Some(ProjectPrompt {
            source: "SSF.claude.md".into(),
            text: "Use native subagents.".into(),
        }),
        vm_guest: false,
        pushes_as: None,
    };
    let ev = Rendered {
        key: "k".into(),
        text: "- [t] @alice commented (https://gh/7#c1):\n  > go".into(),
        origin: None,
        assignee: None,
        state_change: false,
    };
    let p = initial_prompt(&issue, &[ev], &ctx);
    let at = |needle: &str| {
        p.find(needle)
            .unwrap_or_else(|| panic!("{needle} missing from:\n{p}"))
    };
    let order = [
        "[ssf] Simple Software Factory",
        "## How to work on this",
        "## SSF agent guidance (`SSF.md`)",
        "## Harness guidance (`SSF.claude.md`)",
        "[ssf] GitHub issue #7",
        "## Project boards",
        "## Description",
        "## Activity so far",
    ];
    let mut last = 0;
    for (i, needle) in order.iter().enumerate() {
        let at = at(needle);
        assert!(
            at >= last,
            "{needle} ({i}) comes before the part it follows in:\n{p}"
        );
        last = at;
    }
    // Only the item's own part is after the guidance; the guidance names
    // no item content.
    let (how, item) = split_item(&p);
    for item_part in ["## Description", "## Activity so far", "## Project boards"] {
        assert!(!how.contains(item_part), "{p}");
        assert!(item.contains(item_part), "{p}");
    }
    assert!(
        item.contains("- [t] @alice commented"),
        "the activity is the item's: {p}"
    );
}

/// The description is someone else's text, so it is rendered as a
/// quoted block the way a comment body is: the sign-in detector skips a
/// line carrying the `> ` marker wherever it appears in an echoed prompt
/// (`driver::dialog_candidates`). The first prompt ends with the item,
/// so a description that quotes a harness's sign-in phrase sits at the
/// bottom of the pane (#372).
#[test]
fn a_sign_in_phrase_in_the_description_is_not_a_login_prompt() {
    let issue: Issue = serde_json::from_value(json!({
        "number": 5, "title": "Fix it",
        "body": "The pane kept saying:\n\nLogin expired · Please run /login\n\nso I gave up.",
        "html_url": "https://gh/5", "state": "open", "user": {"login": "mike"},
        "created_at": "2026-09-17T08:00:00Z", "updated_at": "t"
    }))
    .unwrap();
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
        driver: DriverKind::Herdr,
        pr: None,
        triggers: &[],
        owner: None,
        delegated_by: None,
        handed_over_from: None,
        projects: &[],
        global_prompt: None,
        global_harness_prompt: None,
        project_prompt: None,
        harness_prompt: None,
        vm_guest: false,
        pushes_as: None,
    };
    let p = initial_prompt(&issue, &[], &ctx);
    // Every line of the description, the blank ones included, carries
    // the marker; nothing is dropped.
    assert!(
        p.contains(
            "\n\n## Description\n\n  > The pane kept saying:\n  > \n  > Login expired · Please \
run /login\n  > \n  > so I gave up.\n\n## Activity so far\n\n(no activity yet)\n"
        ),
        "{p}"
    );
    // As the pane shows it -- the echoed prompt, the composer's last
    // line -- no harness reads the item's words as its own prompt.
    let pane = format!("❯ {p}❯ ");
    for h in [
        "claude", "codex", "gemini", "copilot", "grok", "pi", "omp", "opencode", "crush", "other",
    ] {
        assert_eq!(crate::driver::login_dialog(h, &pane), None, "{h}:\n{pane}");
    }
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
        driver: DriverKind::Herdr,
        pr: None,
        triggers: &triggers,
        owner: None,
        delegated_by: None,
        handed_over_from: None,
        projects: &boards,
        global_prompt: None,
        global_harness_prompt: None,
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
        state_change: true,
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
    let (how, _) = split_item(&p);
    assert!(
        how.contains("## How to work on this"),
        "the instructions are the prompt's opening: {p}"
    );
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
        global_prompt: None,
        global_harness_prompt: None,
        project_prompt: None,
        harness_prompt: None,
        vm_guest: false,
        pushes_as: Some("@ann".into()),
    };
    let p = instructions(&issue, &ctx);
    assert!(
        p.contains("- `gh` already acts as @bot and `git push` as @ann; your posts are marked"),
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
        p.contains("- `gh` and `git push` already act as @bot; your posts are marked"),
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
        global_prompt: None,
        global_harness_prompt: None,
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
    // The prompt names the GitHub affordances in one line; the guide
    // says what each is for. `ssf skill` is for operators, not sessions.
    assert!(g.contains("`ssf skill sessions` the lifecycle reference behind this guide"));
    assert!(!g.contains("`ssf skill` prints"));
    assert!(g.contains("## Posts\n\n"));
    for affordance in [
        "pinned to a commit",
        "task lists (`- [ ]`)",
        "`<details>` around logs",
        "`@mention` for the person who owns a decision",
        "`gh` cannot attach an image",
    ] {
        assert!(g.contains(affordance), "{affordance}");
    }
    // The herdr recipe is setup-specific and sits in a labelled section
    // at the end, after the principle.
    assert!(g.contains("## A second opinion through herdr\n\n"));
    assert!(g.ends_with("until it is closed.\n"));
    assert!(
        g.find("## Second opinions").unwrap()
            < g.find("## A second opinion through herdr").unwrap()
    );
    assert!(
        g.contains("Use `Refs #N` to link a pull request to ongoing management or tracking work.")
    );
    assert!(g.contains("Use `Closes #N` only when merging completes the entire issue"));
    assert!(g.contains("needs it added by hand, as the first line of the body"));
    assert!(g.contains("To speak to the agent on another item, comment on that item with `gh`"));
    assert!(g.contains("the item is the only channel between sessions"));
    assert!(g.contains("worked at its terminal through herdr"));
    assert!(!g.contains("`ssf tell"));
    // A comment is never a command: the `/ssf` affordance is gone (#398),
    // and the guide says where directing an item lives instead.
    assert!(g.contains("A comment is a comment: ssf reads no command out of one"));
    assert!(!g.contains("task-started"));
    assert!(g.contains("`ssf sub <n>`"));
    assert!(g.contains("`ssf sub <n> --events all`, which adds comments, reviews and commits"));
    assert!(g.contains("`ssf unsub <n>` stops them; `ssf subs` lists"));
    assert!(g.contains("from the agent on owner/repo#M"));
    assert!(g.contains("`--assignee bot` in the same `gh ... create` command"));
    // #456: the hand-off paragraph warns against assigning the pull
    // request the session will merge itself; a second pair of eyes is a
    // reviewer subagent.
    assert!(g.contains("Leave `--assignee` off the pull request you will merge yourself"));
    assert!(g.contains("Your second pair of eyes is a reviewer subagent"));
    assert!(g.contains("You are subscribed to it automatically"));
    assert!(g.contains("`ssf assign <n|owner/repo#n> --harness <id> [--model <id>] [--effort"));
    assert!(g.contains("an orchestrator or project-manager item, an architectural review"));
    assert!(g.contains("The item starts in the same poll-interval window as any other"));
    assert!(
        g.contains("an item that already has a session is refused: `ssf handover` is the tool")
    );
    // One session per item: no reviewer, no label, no role; a second
    // opinion is the session's own to arrange, with the herdr recipe.
    assert!(g.contains("## Second opinions"));
    assert!(g.contains("ssf runs one session per item and starts no reviewer for your work"));
    assert!(g.contains("A subagent of your own harness is the default;"));
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
        driver: DriverKind::Herdr,
        pr: None,
        triggers: &[],
        owner: None,
        delegated_by: None,
        handed_over_from: None,
        projects: &boards,
        global_prompt: None,
        global_harness_prompt: None,
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
    // The item's own part, in the order a message reads it: header and
    // boards, then the description and the activity.
    let (how, item) = split_item(&p);
    assert!(item.find("## Project boards").unwrap() < item.find("## Description").unwrap());
    assert!(item.find("## Description").unwrap() < item.find("## Activity so far").unwrap());
    // No column is prescribed for any situation: the option names appear
    // only in the board listing, never in the instructions.
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
fn global_prompts_are_read_from_the_factory_home() {
    let sandbox = crate::config::test_support::sandbox();
    let directory = sandbox.home().join(".ssf");
    std::fs::create_dir_all(&directory).unwrap();
    let repo = RepoConfig {
        name: "o/r".into(),
        harness: "codex".into(),
        ..Default::default()
    };

    assert_eq!(ProjectPrompt::load_global(&repo), None);
    std::fs::write(directory.join("SSF.md"), "Machine guidance.").unwrap();
    std::fs::write(
        directory.join("SSF.codex.md"),
        "<!-- note -->\nCodex guidance.",
    )
    .unwrap();
    std::fs::write(directory.join("SSF.claude.md"), "Claude guidance.").unwrap();

    assert_eq!(
        ProjectPrompt::load_global(&repo),
        Some(ProjectPrompt {
            source: "~/.ssf/SSF.md".into(),
            text: "Machine guidance.".into(),
        })
    );
    assert_eq!(
        ProjectPrompt::load_global_harness(&repo, "codex"),
        Some(ProjectPrompt {
            source: "~/.ssf/SSF.codex.md".into(),
            text: "Codex guidance.".into(),
        })
    );
    assert_eq!(ProjectPrompt::load_global_harness(&repo, "pi"), None);
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
