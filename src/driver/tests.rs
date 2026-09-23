use super::*;

#[test]
fn first_prompt_delivery_distinguishes_send_recovery_and_seeded_sessions() {
    assert_eq!(FirstPrompt::for_state(false, false), FirstPrompt::Send);
    assert_eq!(FirstPrompt::for_state(false, true), FirstPrompt::Recover);
    assert_eq!(FirstPrompt::for_state(true, false), FirstPrompt::No);
    assert_eq!(FirstPrompt::for_state(true, true), FirstPrompt::No);
}

#[test]
fn log_lines_never_carry_the_token() {
    let wrapper = "SSF_CONFIG_DIR='/c' SSF_STATE_DIR='/s' SSF_GITHUB_TOKEN='gho_abc123' '/bin/ssf' launch --repo 'o/r' -- 'claude'";
    assert_eq!(
        redacted(wrapper),
        "SSF_CONFIG_DIR='/c' SSF_STATE_DIR='/s' SSF_GITHUB_TOKEN=<redacted> '/bin/ssf' launch --repo 'o/r' -- 'claude'"
    );
    // Unquoted, at the end, twice, and absent.
    assert_eq!(
        redacted("SSF_GITHUB_TOKEN=gho_x ssf"),
        "SSF_GITHUB_TOKEN=<redacted> ssf"
    );
    assert_eq!(
        redacted("A=1 SSF_GITHUB_TOKEN='gho_x'"),
        "A=1 SSF_GITHUB_TOKEN=<redacted>"
    );
    assert_eq!(
        redacted("SSF_GITHUB_TOKEN='a' SSF_GITHUB_TOKEN=b"),
        "SSF_GITHUB_TOKEN=<redacted> SSF_GITHUB_TOKEN=<redacted>"
    );
    assert_eq!(redacted("claude --model haiku"), "claude --model haiku");
    assert!(!redacted(wrapper).contains("gho_"));
}

#[tokio::test]
async fn worktree_listing_refuses_a_repo_id_that_is_no_directory() {
    let err = local_worktrees("1b790ad2-4421-43dc-9f46-f7c09d0c321f")
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("is not a directory"), "{msg}");
    assert!(msg.contains("1b790ad2"), "{msg}");
    assert!(!msg.contains("cannot change to"), "{msg}");
}

#[test]
fn repo_root_tolerates_compound_ids() {
    assert_eq!(repo_root("/p/widgets::w7"), "/p/widgets");
    assert_eq!(repo_root("/p/widgets"), "/p/widgets");
}

#[test]
fn primary_agent_prefers_working_then_newest() {
    let workspace = WorkspaceInfo {
        agents: vec![
            AgentInfo {
                state: "done".into(),
                updated_at: Some("2026-01-01T00:00:02Z".into()),
                ..Default::default()
            },
            AgentInfo {
                state: "open".into(),
                updated_at: Some("2026-01-01T00:00:01Z".into()),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    assert_eq!(workspace.primary_agent().unwrap().state, "done");

    let workspace = WorkspaceInfo {
        agents: vec![
            AgentInfo {
                state: "done".into(),
                updated_at: Some("2026-01-01T00:00:02Z".into()),
                ..Default::default()
            },
            AgentInfo {
                state: "working".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    assert_eq!(workspace.primary_agent().unwrap().state, "working");
}

#[test]
fn worktrees_go_next_to_the_checkout() {
    assert_eq!(
        worktrees_dir("/p/widgets"),
        PathBuf::from("/p/widgets.worktrees")
    );
    assert_eq!(branch_for("issue-3-x"), "bot/issue-3-x");
    assert_eq!(branch_for("scratch-k3f9"), "scratch/k3f9");
    assert_eq!(number_of_name("scratch-k3f9"), None);
}

#[test]
fn names_tell_their_item() {
    assert_eq!(number_of_name("issue-12"), Some(12));
    assert_eq!(number_of_name("issue-12-fix-it"), Some(12));
    assert_eq!(number_of_name("pr-7-x"), Some(7));
    assert_eq!(
        number_of_name("review-7-x"),
        None,
        "an old reviewer worktree is not the PR's"
    );
    assert_eq!(number_of_name("issue-12x"), None);
    assert_eq!(number_of_name("scratch"), None);
    assert_eq!(number_of_name("issue-"), None);
}

#[test]
fn trust_dialogs_of_each_harness_are_recognised() {
    // Claude Code: "No, exit" comes first.
    let claude = "Quick safety check: Is this a project you created or one you trust?\n\
❯ No, exit\n  Yes, I trust this folder\nEnter to confirm · Esc to cancel";
    assert_eq!(trust_dialog(claude), Some(TrustAnswer::DownEnter));
    // Claude Code's bypass-permissions acceptance, once per machine.
    let bypass = "WARNING: Claude Code running in Bypass Permissions mode\n\
In Bypass Permissions mode, Claude Code will not ask for your approval before running \
potentially dangerous commands.\n❯ No, exit\n  Yes, I accept\nEnter to confirm · Esc to cancel";
    assert_eq!(trust_dialog(bypass), Some(TrustAnswer::DownEnter));
    assert_eq!(
        trust_dialog("⏵⏵ bypass permissions on (shift+tab to cycle)"),
        None
    );
    // Codex: "Yes, continue" comes first.
    let codex = "Do you trust the contents of this directory? Working with untrusted \
contents comes with higher risk of prompt injection.\n› 1. Yes, continue\n  2. No, quit";
    assert_eq!(trust_dialog(codex), Some(TrustAnswer::Enter));
    // Gemini and Pi, when started without --skip-trust / --approve.
    let gemini = "Do you trust the files in this folder?\n● 1. Trust folder (wt)\n  2. Trust parent folder\n  3. Don't trust";
    assert_eq!(trust_dialog(gemini), Some(TrustAnswer::Enter));
    let pi = "Trust project folder?\n/tmp/wt\n→ Trust\n  Trust parent folder";
    assert_eq!(trust_dialog(pi), Some(TrustAnswer::Enter));
    // A ready prompt, or unrelated text, is not a dialog.
    assert_eq!(trust_dialog("❯ \n⏵⏵ bypass permissions on"), None);
    assert_eq!(
        trust_dialog("Folder /tmp/wt has been added to trusted folders."),
        None
    );
    // The wording scrolled up the screen is text, not a dialog: only
    // the bottom of the screen is a dialog's place (#121).
    let scrolled = format!(
        "{codex}\n{}",
        "the agent's answer\n".repeat(TRUST_TAIL_LINES)
    );
    assert_eq!(trust_dialog(&scrolled), None);
}

#[test]
fn login_prompts_of_each_harness_are_recognised() {
    // Claude Code answering a prompt after its token was revoked
    // (seen live on 2026-09-06, issue #81), and its login screen.
    let expired = "❯ [ssf] New activity on #81:\n\n  Login expired · Please run /login\n\n\
❯ \n  ⏵⏵ bypass permissions on (shift+tab to cycle)";
    assert_eq!(
        login_dialog("claude", expired).as_deref(),
        Some("Login expired · Please run /login")
    );
    let screen = "Welcome to Claude Code v2.1.258\n\
 Claude Code can be used with your Claude subscription or billed based on API usage through your Console account.\n\
 Select login method:\n ❯ 1. Claude account with subscription · Pro, Max, Team, or Enterprise\n\
   2. Anthropic Console account · API usage billing\n   3. 3rd-party platform · Amazon Bedrock, Microsoft Foundry, or Vertex AI";
    assert!(login_dialog("claude", screen).is_some());
    assert!(
        login_dialog(
            "claude",
            "API Error: 401 Invalid API key · Please run /login"
        )
        .is_some()
    );
    assert!(
        login_dialog(
            "claude",
            "Your session has expired. Please run /login to sign in again."
        )
        .is_some()
    );
    // The same words far up the screen, quoted by a working agent
    // reading this test, do not count: only the bottom of the screen.
    let mut quoted = vec![
        "⏺ Read(src/driver.rs)".to_string(),
        "  Login expired · Please run /login".to_string(),
    ];
    quoted.extend((0..20).map(|i| format!("  line {i} of the file")));
    quoted.push("❯ ".into());
    assert_eq!(login_dialog("claude", &quoted.join("\n")), None);
    // A ready prompt, the trust dialog, or an agent at work.
    assert_eq!(login_dialog("claude", "❯ \n⏵⏵ bypass permissions on"), None);
    assert_eq!(
        login_dialog("claude", "❯ No, exit\n  Yes, I trust this folder"),
        None
    );
    assert_eq!(
        login_dialog(
            "claude",
            "⏺ Running cargo test…\n  Logging in progress in src/login.rs"
        ),
        None
    );
    // Codex's login screen (0.152.0, empty home).
    let codex = "  Welcome to Codex, OpenAI's command-line coding agent\n\
  Sign in with ChatGPT to use Codex as part of your paid plan\n  or connect an API key for usage-based billing\n\
> 1. Sign in with ChatGPT\n     Usage included with Plus, Pro, Business, and Enterprise plans\n\
  2. Sign in with Device Code\n  3. Provide your own API key\n  Press enter to continue";
    assert!(login_dialog("codex", codex).is_some());
    assert_eq!(
        login_dialog("codex", "› Working on the tests\n  Auth: OAuth"),
        None
    );
    // Gemini (0.57.0), Grok (device login), Pi (0.84.4), Oh My Pi,
    // OpenCode (1.18.25) and Crush (0.92.0) with an empty home.
    let gemini = "│ ? Get started\n│   How would you like to authenticate for this project?\n\
│   ● 1. Sign in with Google\n│     2. Use Gemini API Key\n│     3. Vertex AI\n│   No authentication method selected.";
    assert_eq!(
        login_dialog("gemini", gemini).as_deref(),
        Some("How would you like to authenticate for this project?")
    );
    let grok = "Approve in your browser to finish signing in.\n854F-EX33\nWaiting for approval...\nctrl+q  quit";
    assert!(login_dialog("grok", grok).is_some());
    let pi = " Warning: No models available. Use /login to log into a provider via OAuth or API key. See:\n\
   /home/x/pi/docs/providers.md\n0.0%/0 (auto)     unknown";
    assert!(login_dialog("pi", pi).is_some());
    let omp = "Setup step 1 of 5\nSet up your providers\n╭─ Select provider to login ───╮\n│ ❯ ChatGPT Plus/Pro (Codex Subscription) │";
    assert!(login_dialog("omp", omp).is_some());
    let opencode = "┃  Ask anything... \"Fix a TODO in the codebase\"\n┃  Build auto · Big Pickle OpenCode Zen\n\
● Tip Run /connect to add an AI provider and start coding";
    assert!(login_dialog("opencode", opencode).is_some());
    let crush = " To start, let's choose a provider and model.\n > Find your fave\n Charm Hyper";
    assert!(login_dialog("crush", crush).is_some());
    // A harness ssf knows nothing about still gets the common phrases.
    assert!(login_dialog("other", "Error: not logged in").is_some());
    assert_eq!(login_dialog("other", "all good"), None);
    // Echoed `[ssf]` text does not count: a person quoting the phrase
    // in a comment, delivered as activity and still on the screen.
    let quoted = "❯ [ssf] New activity on #5 \"Fix it\" (https://gh/5):\n\n\
- 15:20Z @mike commented (https://gh/c1):\n  > the terminal says Login expired · Please run /login, is that you?\n\
- 15:21Z @mike assigned @bot\n\n⏺ Yes, and I am fine now.\n\n❯ ";
    assert_eq!(login_dialog("claude", quoted), None);
    // But the harness's own answer right after the echo still does.
    assert!(login_dialog("claude", expired).is_some());
    let after_echo = "❯ [ssf] New activity on #5:\n- 15:20Z @mike commented:\n  > hi\n\nLogin expired · Please run /login\n❯ ";
    assert!(login_dialog("claude", after_echo).is_some());
    // The first prompt ends with the item, so its comments are what sits
    // at the bottom of the pane: a phrase quoted in one is ssf's
    // rendering of someone else's words (#355), not a sign-in prompt.
    let first_prompt = "❯ [ssf] GitHub issue #5: Fix it\nhttps://gh/5\n\n\
Opened by @mike on 2026-09-17 08:00Z. Labels: daemon.\n\n\
## Description\n\n  > Retry the sign-in when the token lapses.\n\n\
## Activity so far\n\n- 09:20Z @mike commented (https://gh/c1):\n  > the daemon does not retry: Login expired · Please run /login\n\n❯ ";
    assert_eq!(login_dialog("claude", first_prompt), None);
    // A dialog drawn flush or in a box is still read, wherever it is.
    let after_first_prompt = format!("{first_prompt}Login expired · Please run /login");
    assert!(login_dialog("claude", &after_first_prompt).is_some());
    let boxed = "│ ? Get started\n│   How would you like to authenticate for this project?";
    assert!(login_dialog("gemini", boxed).is_some());
}

/// Text ssf is about to write down or paste somewhere is read whole:
/// neither the screen check's tail window nor its `[ssf]` echo rule
/// applies, because nothing vouches for who wrote it.
#[test]
fn text_of_ssf_s_own_is_read_line_by_line() {
    let mut summary = String::from(
        "Handing over the parser work.\n\nThe pane kept saying \"Please run /login\", which is \
why I gave up on it.\n",
    );
    for i in 0..40 {
        summary.push_str(&format!("- step {i}: done\n"));
    }
    assert_eq!(
        login_prompt_line(&summary).as_deref(),
        Some("The pane kept saying \"Please run /login\", which is why I gave up on it."),
        "a phrase in line three of a long text still counts"
    );
    assert_eq!(
        login_dialog("claude", &summary),
        None,
        "the same text as a screen is judged by its bottom alone"
    );
    // An `[ssf]` marker in it vouches for nothing: anyone can write one.
    let echoed = "[ssf] the note said:\n- not logged in, it said\n";
    assert!(quotes_login_prompt(echoed));
    assert_eq!(login_dialog("claude", echoed), None);
    assert_eq!(login_prompt_line("all good, branch pushed"), None);
}

#[test]
fn parses_git_worktree_list() {
    let text = "worktree /p/widgets\nHEAD abc\nbranch refs/heads/master\n\n\
worktree /p/widgets.worktrees/issue-3\nHEAD def\nbranch refs/heads/bot/issue-3\n\n\
worktree /p/widgets.worktrees/tmp\nHEAD 123\ndetached\n";
    let list = parse_worktree_list(text);
    assert_eq!(list.len(), 3);
    assert_eq!(list[1].path, "/p/widgets.worktrees/issue-3");
    assert_eq!(list[1].branch.as_deref(), Some("refs/heads/bot/issue-3"));
    assert_eq!(list[2].branch, None);
}

#[test]
fn omp_authenticated_setup_is_not_a_login_prompt() {
    let screen = "Setup step 1 of 5\nSet up your providers\nSelect provider to login\nAnthropic ○ not logged in\nOpenRouter ● logged in (api key)\nPress Esc when you're done.";
    assert_eq!(login_dialog("omp", screen), None);
    assert_eq!(
        blocking_dialog("omp", screen).unwrap().0,
        crate::state::Blocked::SETUP
    );
    assert!(
        login_dialog(
            "omp",
            &screen.replace("● logged in (api key)", "○ not logged in")
        )
        .is_some()
    );
    assert!(login_dialog("pi", screen).is_some());
    assert!(
        login_dialog(
            "omp",
            "Select provider to login\nOpenRouter ● logged in (api key)"
        )
        .is_some()
    );
    assert_eq!(
        blocking_dialog("omp", "Setup step 2 of 5\nChoose a theme\nesc skip")
            .unwrap()
            .0,
        crate::state::Blocked::SETUP
    );
    let echoed = format!("[ssf] activity\n> {}", screen.replace('\n', "\n> "));
    assert_eq!(blocking_dialog("omp", &echoed), None);
    let old = format!("{screen}{}", "\nordinary output".repeat(16));
    assert_eq!(blocking_dialog("omp", &old), None);
}

#[test]
fn omp_full_height_setup_keeps_header_context() {
    let screen = [
        "Setup step 1 of 5",
        "Set up your providers",
        "Sign in and pick a web search provider. Press Esc when you're done.",
        "Providers",
        "Select provider to login",
        "Anthropic",
        "OpenAI",
        "GitHub Copilot",
        "Google",
        "Google Gemini CLI",
        "Google Antigravity",
        "OpenAI Codex",
        "OpenRouter ● logged in (api key)",
        "Vercel AI Gateway",
        "Z.AI",
        "Type to search",
        "↑/↓ select · enter confirm · esc skip · ctrl+c exit setup",
    ]
    .join("\n");
    assert_eq!(login_dialog("omp", &screen), None);
    assert_eq!(
        blocking_dialog("omp", &screen).unwrap().0,
        crate::state::Blocked::SETUP
    );
    assert!(login_dialog("omp", &screen.replace("● logged in (api key)", "")).is_some());
    let stale = format!("{screen}{}", "\nordinary output".repeat(16));
    assert_eq!(blocking_dialog("omp", &stale), None);
    let echoed = format!("[ssf] activity\n> {}", screen.replace('\n', "\n> "));
    assert_eq!(blocking_dialog("omp", &echoed), None);
}
