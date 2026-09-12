use super::client::*;
use super::prelude::*;
use super::*;

#[test]
fn client_target_is_the_only_local_remote_difference() {
    let args = vec!["status".into(), "--json".into()];
    assert_eq!(client_target(args.clone(), None).unwrap(), (None, args));

    let (server, args) = client_target(
        vec!["status".into(), "--server".into(), "bot@factory".into()],
        Some("configured".into()),
    )
    .unwrap();
    assert_eq!(server.as_deref(), Some("bot@factory"));
    assert_eq!(args, ["status"]);

    let (server, args) =
        client_target(vec!["--server=factory-alias".into(), "doctor".into()], None).unwrap();
    assert_eq!(server.as_deref(), Some("factory-alias"));
    assert_eq!(args, ["doctor"]);

    let trailing = vec![
        "launch".into(),
        "--".into(),
        "tool".into(),
        "--server".into(),
        "tool-value".into(),
    ];
    assert_eq!(
        client_target(trailing.clone(), None).unwrap(),
        (None, trailing)
    );
}

#[test]
fn client_target_requires_a_destination() {
    assert!(client_target(vec!["status".into(), "--server".into()], None).is_err());
}

#[test]
fn release_asset_binaries_find_their_companions() {
    assert_eq!(
        companion_client_path(Path::new("/tmp/ssf-server-0.4.0-linux-x86_64")),
        PathBuf::from("/tmp/ssf-0.4.0-linux-x86_64")
    );
    assert_eq!(
        factory_vm::companion_server_path(Path::new("/tmp/ssf-0.4.0-linux-x86_64")),
        PathBuf::from("/tmp/ssf-server-0.4.0-linux-x86_64")
    );
}

#[test]
fn factory_cli_routes_to_guest_but_vm_and_dashboard_settings_stay_on_host() {
    for args in [
        vec!["ssf", "repo", "list"],
        vec!["ssf", "repo", "add", "owner/repo", "--harness", "claude"],
        vec!["ssf", "repo", "set", "owner/repo", "--model", "model"],
        vec!["ssf", "repo", "remove", "owner/repo"],
        vec!["ssf", "auth", "login"],
        vec!["ssf", "auth", "logout"],
        vec!["ssf", "auth", "status"],
        vec!["ssf", "token"],
        vec!["ssf", "config"],
        vec!["ssf", "config", "path"],
        vec!["ssf", "config", "get", "daemon"],
        vec!["ssf", "config", "set", "daemon.poll_interval_secs", "30"],
        vec!["ssf", "agents"],
        vec!["ssf", "models", "claude"],
    ] {
        let cli = Cli::try_parse_from(&args).unwrap();
        let name =
            forwarded_name(&cli.command).unwrap_or_else(|| panic!("{args:?} stayed on host"));
        assert!(matches!(
            forwarding_gate(&Ok(false), "factory", "firecracker", name, None),
            Gate::Refuse(_)
        ));
    }
    for args in [
        vec!["ssf", "config", "get", "vm"],
        vec!["ssf", "config", "get", "vm.enabled"],
        vec!["ssf", "config", "get", "dashboard"],
        vec!["ssf", "config", "get", "dashboard.port"],
        vec!["ssf", "config", "set", "dashboard.enabled", "true"],
        vec!["ssf", "config", "set", "vm.enabled", "false"],
        vec!["ssf", "vm", "start"],
    ] {
        let cli = Cli::try_parse_from(&args).unwrap();
        assert_eq!(forwarded_name(&cli.command), None, "{args:?}");
    }
}

#[test]
fn the_backend_tooling_is_a_note_on_the_host_and_nothing_in_the_guest() {
    // Why it is only ever a note: `doctor` is forwarded, so a factory
    // in a running VM answers doctor from the guest...
    assert_eq!(forwarded_name(&Command::Doctor), Some("doctor"));
    assert!(factory_vm::forwards("doctor"));
    assert!(!reports_backend_tooling(true));
    // ...and a factory in a stopped VM never gets here at all: `main`
    // bails on any forwarded command but `status`, and that bail is
    // itself what names the tooling a host has not got. So the doctor
    // that prints this line is one running the factory on this
    // machine, where the backend is not in use and cannot fail.
    assert!(reports_backend_tooling(false));
}

#[test]
fn the_machine_name_comes_from_whichever_source_this_os_has() {
    // Linux: /etc/hostname, exactly as before.
    assert_eq!(pick_hostname(Some("box\n".into()), || None, || None), "box");
    // macOS has no /etc/hostname; the name a person gave the Mac
    // comes first, `hostname` after it. Neither must be allowed to
    // leave the key labelled "ssf on localhost".
    assert_eq!(
        pick_hostname(
            None,
            || Some("Mike's MacBook Pro\n".into()),
            || Some("mikes-mbp.local\n".into())
        ),
        "Mike's MacBook Pro"
    );
    assert_eq!(
        pick_hostname(None, || None, || Some("mikes-mbp.local\n".into())),
        "mikes-mbp.local"
    );
    // Empty answers count as no answer.
    assert_eq!(
        pick_hostname(
            Some("  \n".into()),
            || Some("".into()),
            || Some(" mac \n".into())
        ),
        "mac"
    );
    assert_eq!(pick_hostname(None, || None, || None), "localhost");
}

#[test]
fn only_a_definite_no_keeps_a_command_out_of_the_guest() {
    // A guest that is up takes the command, with nothing said.
    assert_eq!(
        forwarding_gate(&Ok(true), "default", "lima", "tell", None),
        Gate::Send(None)
    );
    // A guest that is down does not, and the refusal names what the
    // host has not got when that is why it cannot be started.
    assert_eq!(
        forwarding_gate(&Ok(false), "default", "lima", "tell", None),
        Gate::Refuse(
            "the factory runs in VM default, which is not running; `ssf vm start` first".into()
        )
    );
    let Gate::Refuse(why) = forwarding_gate(
        &Ok(false),
        "default",
        "lima",
        "doctor",
        Some("limactl not installed; install lima"),
    ) else {
        panic!("a stopped VM refuses a command that needs it")
    };
    assert!(
        why.contains("lima cannot start it: limactl not installed"),
        "{why}"
    );

    // "The probe could not be made" is neither answer. Under lima it
    // forks `limactl`, and one fork that fails -- or is cut off by
    // LIVENESS_LIMIT -- must not refuse every forwarded command over
    // a factory that is running, nor report a stopped VM to the bar
    // widget. The command goes to the guest, and says why first: the
    // reason is not in the log at every log level.
    let probe = Err("asking lima whether ssf-default is running: fork/exec: \
resource temporarily unavailable"
        .to_string());
    let Gate::Send(Some(note)) = forwarding_gate(&probe, "default", "lima", "tell", None) else {
        panic!("an unanswered probe forwards the command")
    };
    assert!(
        note.starts_with("could not tell whether VM default is running: asking lima"),
        "{note}"
    );
    assert!(note.contains("sending `ssf tell` to it anyway"), "{note}");
    assert!(!note.contains("is not running,"), "{note}");
    // A host with no `limactl` at all cannot answer the probe, so
    // the refusal that names the missing tooling is never reached:
    // the note carries it instead, or the person sees nothing but
    // ssh refusing a connection.
    let Gate::Send(Some(note)) = forwarding_gate(
        &probe,
        "default",
        "lima",
        "status",
        Some("limactl not installed; install lima"),
    ) else {
        panic!("an unanswered probe forwards the command")
    };
    assert!(
        note.contains("if it is down, lima cannot start it: limactl not installed"),
        "{note}"
    );
    // And the note carries the advice the refusal used to give: the
    // ssh failure that may follow it says nothing about ssf.
    assert!(note.contains("`ssf vm start` starts it"), "{note}");
}

#[test]
fn the_widget_gets_an_answer_for_a_guest_that_did_not_give_one() {
    // The document names the host service, which is read from the
    // state directory: a test's must be its own (#140).
    let _sandbox = crate::config::test_support::sandbox();
    // The host cannot fill in what only the guest knows, so the
    // sessions and repositories are empty rather than invented; what
    // it can fill in is the VM, and each of the three answers the
    // probe can give reaches the document as itself. An ssh failure
    // with nothing put in its place is the case this exists to stop:
    // the widget parses that as a factory with nothing in it.
    assert_eq!(probe_word(&Ok(true)), "running");
    assert_eq!(probe_word(&Ok(false)), "stopped");
    assert_eq!(probe_word(&Err("no answer".into())), "unknown");
    for state in ["running", "stopped", "unknown"] {
        let v = vm_status_for_guest(state);
        assert_eq!(v["vm"], state);
        assert_eq!(v["service_active"], false);
        assert_eq!(v["factory_reachable"], false);
        assert_eq!(v["factory_location"], "guest");
        assert_eq!(v["host_vm"]["state"], state);
        assert_eq!(v["sessions"], serde_json::json!([]));
        assert_eq!(v["repos"], serde_json::json!([]));
    }
}

#[test]
fn item_refs_accept_numbers_and_sessions() {
    let me = origin::Origin::new("o/r", 3).unwrap();
    assert_eq!(item_ref("7", Some(&me)).unwrap(), "o/r#7");
    assert_eq!(item_ref("#7", Some(&me)).unwrap(), "o/r#7");
    assert_eq!(item_ref("x/y#7", None).unwrap(), "x/y#7");
    assert!(item_ref("7", None).is_err());
    // The reviewer sessions of before #115 had a suffix of their own.
    assert!(item_ref("7:reviewer", Some(&me)).is_err());
    assert!(item_ref("x/y#7:reviewer", None).is_err());
    assert!(item_ref("nonsense", Some(&me)).is_err());
}
#[test]
fn the_handover_message_names_the_new_stack_and_ends_the_session() {
    assert_eq!(
        handover_recorded_text(
            "o/r#5",
            "Fix the widget",
            "Pi",
            Some("openai/gpt-6"),
            Some("high"),
            None,
            Some(1234),
            10,
        ),
        "Handover of o/r#5 (\"Fix the widget\") recorded: to Pi (model openai/gpt-6, effort \
high), with a summary of 1,234 chars.\nThe daemon ends this session on its next pass (within \
10s) and starts the new one in the same workspace. Stop working now: do not start anything \
else, and do not run this command again."
    );
    let plain = handover_recorded_text("o/r#5", "T", "Codex", None, None, None, None, 30);
    assert!(
        plain.starts_with(
            "Handover of o/r#5 (\"T\") recorded: to Codex (the harness's default model, the \
harness's default effort), without a summary."
        ),
        "{plain}"
    );
    assert!(plain.contains("within 30s"), "{plain}");
    // With a repository command configured it is the command that
    // decides an unset model or effort, as the `handed-over` post
    // says of the same handover.
    let by_command = handover_recorded_text(
        "o/r#5",
        "T",
        "Claude Code",
        None,
        None,
        Some("claude --dangerously-skip-permissions"),
        None,
        10,
    );
    assert!(
        by_command.starts_with(
            "Handover of o/r#5 (\"T\") recorded: to Claude Code (the command's model, the \
command's effort), without a summary."
        ),
        "{by_command}"
    );
    assert_eq!(thousands(0), "0");
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(1_000_000), "1,000,000");
}

#[test]
fn a_handover_summary_comes_from_the_flag_or_the_file() {
    let dir = std::env::temp_dir().join(format!("ssf-summary-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("summary.md");
    std::fs::write(&path, "what is left").unwrap();
    assert_eq!(
        handover_summary(None, Some(&path), false)
            .unwrap()
            .as_deref(),
        Some("what is left")
    );
    assert_eq!(
        handover_summary(Some("inline".into()), None, false)
            .unwrap()
            .as_deref(),
        Some("inline")
    );
    assert!(handover_summary(None, None, true).unwrap().is_none());
    // No summary form at all: the clap group cannot require one, since
    // `--cancel` takes none either, so the check is here.
    let e = handover_summary(None, None, false).unwrap_err().to_string();
    assert!(e.contains("say what the new session is told"), "{e}");
    // Empty, and over the cap, are the writer's to fix.
    std::fs::write(&path, "   \n").unwrap();
    let e = handover_summary(None, Some(&path), false)
        .unwrap_err()
        .to_string();
    assert!(e.contains("write a summary or pass --no-summary"), "{e}");
    assert!(
        handover_summary(Some(String::new()), None, false)
            .unwrap_err()
            .to_string()
            .contains("the summary is empty")
    );
    let long = "x".repeat(ipc::MAX_SUMMARY_CHARS + 1);
    let e = handover_summary(Some(long), None, false)
        .unwrap_err()
        .to_string();
    assert!(e.contains("8,001 characters") && e.contains("8,000"), "{e}");
    assert!(
        handover_summary(None, Some(&dir.join("nope.md")), false)
            .unwrap_err()
            .to_string()
            .contains("reading the summary from")
    );
    // A summary that quotes a sign-in screen would block the session
    // it starts: refused here, where the author can reword it, and
    // the refusal itself does not repeat the phrase.
    let e = handover_summary(
        Some("Blocked all afternoon: the pane kept saying Please run /login".into()),
        None,
        false,
    )
    .unwrap_err()
    .to_string();
    assert!(
        e.contains("would read as a harness's own sign-in screen"),
        "{e}"
    );
    assert!(!driver::quotes_login_prompt(&e), "{e}");
    assert!(
        e.contains("[\u{2026}]"),
        "the phrase is redacted, not dropped: {e}"
    );
    // Wherever the phrase stands in it: a long summary that quotes one
    // in its third line, and one that has an `[ssf]` marker of its own.
    let mut long =
        String::from("Handing over.\n\nThe pane kept saying \"Please run /login\" at me.\n");
    for i in 0..40 {
        long.push_str(&format!("- step {i}: done\n"));
    }
    assert!(
        handover_summary(Some(long), None, false)
            .unwrap_err()
            .to_string()
            .contains("would read as a harness's own sign-in screen")
    );
    assert!(
        handover_summary(
            Some("[ssf] the note said:\n- not logged in, it said".into()),
            None,
            false
        )
        .unwrap_err()
        .to_string()
        .contains("would read as a harness's own sign-in screen")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allowed_users_flags_parse_logins_and_the_wildcard() {
    assert_eq!(
        parse_allowed_users("Alice, @bob,carol"),
        vec!["Alice", "bob", "carol"]
    );
    assert_eq!(parse_allowed_users("*"), vec!["*"]);
    assert!(parse_allowed_users("").is_empty());
    let mut entry = RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        ..Default::default()
    };
    set_repo_allowed_users(&mut entry, "alice,bob", false).unwrap();
    assert_eq!(
        entry.allowed_users.as_deref(),
        Some(&["alice".to_string(), "bob".to_string()][..])
    );
    assert!(!entry.accepted_anyone_risk);
    set_repo_allowed_users(&mut entry, "*", true).unwrap();
    assert!(entry.accepted_anyone_risk);
    // Back to a list: the marker goes, so a later hand edit is refused.
    set_repo_allowed_users(&mut entry, "alice", false).unwrap();
    assert!(!entry.accepted_anyone_risk);
}

#[test]
fn the_wildcard_is_refused_without_consent() {
    assert!(anyone_risk_decision(true, false, "x", || unreachable!()).is_ok());
    let err = anyone_risk_decision(false, false, "x", || unreachable!()).unwrap_err();
    assert!(err.to_string().contains("--accept-anyone-risk"), "{err}");
    assert!(err.to_string().contains("ANYONE"), "{err}");
    assert!(anyone_risk_decision(false, true, "x", || Ok(false)).is_err());
    assert!(anyone_risk_decision(false, true, "x", || Ok(true)).is_ok());
}

#[test]
fn config_set_replaces_the_old_startup_wait_key_with_the_new_one() {
    let dir = std::env::temp_dir().join(format!(
        "ssf-config-set-rename-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[daemon]\nstartup_orca_wait_secs = 60\n").unwrap();
    // The new name over a file holding the old one.
    config_set_at(&path, "daemon.startup_driver_wait_secs", "30", false).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("startup_orca_wait_secs"), "{text}");
    assert_eq!(
        Config::load_from(&path)
            .unwrap()
            .daemon
            .startup_driver_wait_secs,
        30
    );
    // The old name is still accepted and lands under the new one.
    config_set_at(&path, "daemon.startup_orca_wait_secs", "45", false).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("startup_driver_wait_secs = 45"), "{text}");
    assert!(!text.contains("startup_orca_wait_secs"), "{text}");
    assert_eq!(
        Config::load_from(&path)
            .unwrap()
            .daemon
            .startup_driver_wait_secs,
        45
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn config_set_switches_event_comments_through_the_generic_path() {
    let dir = std::env::temp_dir().join(format!(
        "ssf-config-set-events-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "").unwrap();
    assert!(Config::load_from(&path).unwrap().daemon.event_comments);
    config_set_at(&path, "daemon.event_comments", "false", false).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("event_comments = false"), "{text}");
    assert!(!Config::load_from(&path).unwrap().daemon.event_comments);
    config_set_at(&path, "daemon.event_comments", "true", false).unwrap();
    assert!(Config::load_from(&path).unwrap().daemon.event_comments);
    // Per-repository values go through `ssf repo set`, not here.
    assert!(config_set_at(&path, "repo.event_comments", "false", false).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn repo_add_and_set_switch_event_comments_and_clear_puts_it_back() {
    let dir = std::env::temp_dir().join(format!(
        "ssf-repo-events-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    let add = |event_comments: Option<bool>| RepoCommand::Add {
        name: "o/r".into(),
        harness: "claude".into(),
        driver: None,
        path: None,
        clone_url: None,
        base_branch: None,
        command: None,
        model: None,
        effort: None,
        instructions: None,
        prompt_file: None,
        allowed_users: None,
        accept_anyone_risk: false,
        event_comments,
    };
    let set = |event_comments: Option<bool>, clear: Vec<String>| RepoCommand::Set {
        name: "o/r".into(),
        harness: None,
        driver: None,
        path: None,
        clone_url: None,
        base_branch: None,
        command: None,
        model: None,
        effort: None,
        instructions: None,
        prompt_file: None,
        allowed_users: None,
        accept_anyone_risk: false,
        event_comments,
        git_name: None,
        git_email: None,
        git_signing_key: None,
        git_credential: None,
        clear,
    };
    let loaded = || Config::load_from(&path).unwrap();
    // Unset by default: the instance decides, and nothing is written.
    repo_at(&path, add(None)).unwrap();
    assert_eq!(loaded().repos[0].event_comments, None);
    assert!(loaded().event_comments(&loaded().repos[0]));
    let text = std::fs::read_to_string(&path).unwrap();
    let repo_table = text.split("[[repo]]").nth(1).unwrap();
    assert!(!repo_table.contains("event_comments"), "{text}");
    // Set off, then on, then cleared.
    repo_at(&path, set(Some(false), vec![])).unwrap();
    let cfg = loaded();
    assert_eq!(cfg.repos[0].event_comments, Some(false));
    assert!(!cfg.event_comments(&cfg.repos[0]));
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("event_comments = false")
    );
    repo_at(&path, set(Some(true), vec![])).unwrap();
    assert_eq!(loaded().repos[0].event_comments, Some(true));
    repo_at(&path, set(None, vec![])).unwrap();
    assert_eq!(loaded().repos[0].event_comments, Some(true), "left alone");
    repo_at(&path, set(None, vec!["event_comments".into()])).unwrap();
    assert_eq!(loaded().repos[0].event_comments, None);
    // `repo add` over an existing entry takes the flag too.
    repo_at(&path, add(Some(false))).unwrap();
    assert_eq!(loaded().repos[0].event_comments, Some(false));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn config_set_writes_the_wildcard_only_with_its_marker() {
    let dir = std::env::temp_dir().join(format!(
        "ssf-config-set-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    config_set_at(&path, "daemon.allowed_users", r#"["*"]"#, true).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("accepted_anyone_risk = true"), "{text}");
    let cfg = Config::load_from(&path).unwrap();
    assert!(cfg.anyone_allowed_anywhere());
    // A list again: the marker goes with the wildcard.
    config_set_at(&path, "daemon.allowed_users", r#"["Alice", "bob"]"#, false).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("accepted_anyone_risk"), "{text}");
    let cfg = Config::load_from(&path).unwrap();
    assert_eq!(
        cfg.daemon.allowed_users.as_deref(),
        Some(&["Alice".to_string(), "bob".to_string()][..])
    );
    // A bare login list works too.
    config_set_at(&path, "daemon.allowed_users", "carol", false).unwrap();
    let cfg = Config::load_from(&path).unwrap();
    assert_eq!(
        cfg.daemon.allowed_users.as_deref(),
        Some(&["carol".to_string()][..])
    );
    // A hand edit that adds the wildcard without the marker is refused
    // at load, and by any later `config set` of another key.
    std::fs::write(&path, "[daemon]\nallowed_users = [\"*\"]\n").unwrap();
    assert!(Config::load_from(&path).is_err());
    let err = config_set_at(&path, "daemon.poll_interval_secs", "5", false).unwrap_err();
    assert!(
        format!("{err:#}").contains("--accept-anyone-risk"),
        "{err:#}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ssf-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git_value<'a>(plan: &'a LaunchEnv, key: &str) -> Vec<&'a str> {
    plan.git
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .collect()
}

fn env_value<'a>(plan: &'a LaunchEnv, key: &str) -> Option<&'a str> {
    plan.env
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

#[test]
fn launch_env_is_the_bot_by_default() {
    let dir = scratch_dir("launch-bot");
    let key = dir.join("bot_ed25519");
    std::fs::write(&key, "k").unwrap();
    std::fs::write(keys::public_path(&key), "p").unwrap();
    let mut cfg = Config::default();
    cfg.github.login = Some("acme-bot".into());
    cfg.github.ssh_key_path = Some(key.to_string_lossy().to_string());
    cfg.github.signing_key_id = Some(1);
    cfg.repos.push(RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        ..RepoConfig::default()
    });
    let plan = launch_env(&cfg, cfg.repos.first(), "/opt/ssf", true);
    assert_eq!(env_value(&plan, "GIT_AUTHOR_NAME"), Some("acme-bot"));
    assert_eq!(env_value(&plan, "GIT_COMMITTER_NAME"), Some("acme-bot"));
    assert_eq!(
        env_value(&plan, "GIT_AUTHOR_EMAIL"),
        Some("acme-bot@users.noreply.github.com")
    );
    assert_eq!(
        env_value(&plan, "GIT_COMMITTER_EMAIL"),
        env_value(&plan, "GIT_AUTHOR_EMAIL")
    );
    assert_eq!(
        git_value(&plan, "credential.helper"),
        vec!["", "!/opt/ssf git-credential"]
    );
    assert_eq!(git_value(&plan, "gpg.format"), vec!["ssh"]);
    assert_eq!(
        git_value(&plan, "user.signingkey"),
        vec![keys::public_path(&key).to_string_lossy().as_ref()]
    );
    assert_eq!(git_value(&plan, "commit.gpgsign"), vec!["true"]);
    assert_eq!(git_value(&plan, "tag.gpgsign"), vec!["true"]);
    assert!(
        env_value(&plan, "GIT_SSH_COMMAND")
            .unwrap()
            .contains("IdentitiesOnly=yes")
    );
    assert!(plan.notes.is_empty(), "{:?}", plan.notes);
    // Without a token there is no bot helper to configure.
    let plan = launch_env(&cfg, cfg.repos.first(), "/opt/ssf", false);
    assert!(git_value(&plan, "credential.helper").is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn launch_env_commits_as_the_configured_person() {
    let dir = scratch_dir("launch-person");
    let bot_key = dir.join("bot_ed25519");
    std::fs::write(&bot_key, "k").unwrap();
    let person_key = dir.join("id_ed25519");
    std::fs::write(&person_key, "k").unwrap();
    let mut cfg = Config::default();
    cfg.github.login = Some("acme-bot".into());
    cfg.github.ssh_key_path = Some(bot_key.to_string_lossy().to_string());
    cfg.github.signing_key_id = Some(1);
    cfg.git.name = Some("Ann Person".into());
    cfg.git.email = Some("ann@example.com".into());
    cfg.repos.push(RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        ..RepoConfig::default()
    });
    // A person with nothing else: unsigned, bot pushes the commits.
    let plan = launch_env(&cfg, cfg.repos.first(), "/opt/ssf", true);
    assert_eq!(env_value(&plan, "GIT_AUTHOR_NAME"), Some("Ann Person"));
    assert_eq!(env_value(&plan, "GIT_COMMITTER_NAME"), Some("Ann Person"));
    assert_eq!(
        env_value(&plan, "GIT_AUTHOR_EMAIL"),
        Some("ann@example.com")
    );
    assert_eq!(git_value(&plan, "user.name"), vec!["Ann Person"]);
    assert_eq!(git_value(&plan, "user.email"), vec!["ann@example.com"]);
    assert!(git_value(&plan, "gpg.format").is_empty());
    assert_eq!(git_value(&plan, "commit.gpgsign"), vec!["false"]);
    assert_eq!(git_value(&plan, "tag.gpgsign"), vec!["false"]);
    assert_eq!(
        git_value(&plan, "credential.helper"),
        vec!["", "!/opt/ssf git-credential"]
    );
    // SSH remotes still go through the bot's key.
    assert!(
        env_value(&plan, "GIT_SSH_COMMAND")
            .unwrap()
            .contains("bot_ed25519")
    );
    // Their own key (no .pub next to it: the private path is used) and
    // their own token, set on the repository.
    cfg.repos[0].git.signing_key = Some(config::SigningKey::Path(
        person_key.to_string_lossy().to_string(),
    ));
    cfg.repos[0].git.credential = Some("token:ann".into());
    let plan = launch_env(&cfg, cfg.repos.first(), "/opt/ssf", true);
    assert_eq!(git_value(&plan, "gpg.format"), vec!["ssh"]);
    assert_eq!(
        git_value(&plan, "user.signingkey"),
        vec![person_key.to_string_lossy().as_ref()]
    );
    assert_eq!(git_value(&plan, "commit.gpgsign"), vec!["true"]);
    assert_eq!(
        git_value(&plan, "credential.helper"),
        vec!["", "!/opt/ssf git-credential"],
        "the token is looked up by the helper, per SSF_REPO"
    );
    // A helper string replaces ours; a missing key means unsigned, with a note.
    cfg.repos[0].git.credential = Some("!gh auth git-credential".into());
    cfg.repos[0].git.signing_key = Some(config::SigningKey::Path(
        dir.join("gone").to_string_lossy().to_string(),
    ));
    let plan = launch_env(&cfg, cfg.repos.first(), "/opt/ssf", true);
    assert_eq!(
        git_value(&plan, "credential.helper"),
        vec!["", "!gh auth git-credential"]
    );
    assert_eq!(git_value(&plan, "commit.gpgsign"), vec!["false"]);
    assert!(
        plan.notes.iter().any(|n| n.contains("gone")),
        "{:?}",
        plan.notes
    );
    // Another repository without an override is the instance identity.
    let other = RepoConfig {
        name: "o/s".into(),
        harness: "claude".into(),
        ..RepoConfig::default()
    };
    let plan = launch_env(&cfg, Some(&other), "/opt/ssf", true);
    assert_eq!(env_value(&plan, "GIT_AUTHOR_NAME"), Some("Ann Person"));
    assert_eq!(git_value(&plan, "commit.gpgsign"), vec!["false"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn config_set_writes_the_git_table_whole() {
    let dir = scratch_dir("config-set-git");
    let path = dir.join("config.toml");
    // One half of an identity is refused, with the way to set both.
    let err = config_set_at(&path, "git.name", "Ann", false).unwrap_err();
    assert!(
        format!("{err:#}").contains("ssf config set git '{"),
        "{err:#}"
    );
    assert!(!path.exists());
    config_set_at(
        &path,
        "git",
        r#"{ name = "Ann Person", email = "ann@example.com" }"#,
        false,
    )
    .unwrap();
    let cfg = Config::load_from(&path).unwrap();
    assert_eq!(cfg.git.name.as_deref(), Some("Ann Person"));
    assert_eq!(cfg.git.email.as_deref(), Some("ann@example.com"));
    // With both there, one key at a time is fine.
    config_set_at(&path, "git.email", "ann@work.example", false).unwrap();
    config_set_at(&path, "git.signing_key", "false", false).unwrap();
    config_set_at(&path, "git.credential", "token:ann", false).unwrap();
    let cfg = Config::load_from(&path).unwrap();
    assert_eq!(cfg.git.email.as_deref(), Some("ann@work.example"));
    assert_eq!(cfg.git.signing_key, Some(config::SigningKey::Off(false)));
    assert_eq!(
        cfg.git_identity(None).credential,
        config::Credential::Token("ann".into())
    );
    assert!(config_set_at(&path, "git.credential", "token:", false).is_err());
    let err = config_set_at(&path, "git.credential", "tokn:ann", false).unwrap_err();
    assert!(format!("{err:#}").contains("token:"), "{err:#}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_helper_answers_as_the_bot_outside_a_session() {
    let mut cfg = Config::default();
    cfg.github.login = Some("acme-bot".into());
    cfg.git.credential = Some("token:ann".into());
    cfg.repos.push(RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        git: config::GitConfig {
            credential: Some("file:/t".into()),
            ..Default::default()
        },
        ..RepoConfig::default()
    });
    assert_eq!(
        push_credential(&cfg, Some("o/r")),
        config::Credential::File(PathBuf::from("/t"))
    );
    assert_eq!(
        push_credential(&cfg, Some("O/R")),
        config::Credential::File(PathBuf::from("/t"))
    );
    assert_eq!(
        push_credential(&cfg, Some("o/other")),
        config::Credential::Token("ann".into())
    );
    // The daemon's clones and a guest shell: never the person.
    assert_eq!(push_credential(&cfg, None), config::Credential::Bot);
}

#[test]
fn signing_key_flag_reads_false_as_off() {
    assert_eq!(parse_signing_key("false"), config::SigningKey::Off(false));
    assert_eq!(parse_signing_key(" OFF "), config::SigningKey::Off(false));
    assert_eq!(
        parse_signing_key("~/.ssh/id_ed25519"),
        config::SigningKey::Path("~/.ssh/id_ed25519".into())
    );
}

#[test]
fn guide_and_launch_identity_prefer_session_then_daemon_then_config() {
    assert_eq!(
        configured_bot_login(
            Some("session-bot"),
            Some("daemon-bot"),
            Some("configured-bot")
        ),
        Some("session-bot".into())
    );
    assert_eq!(
        configured_bot_login(Some(""), Some("daemon-bot"), Some("configured-bot")),
        Some("daemon-bot".into())
    );
    assert_eq!(
        configured_bot_login(None, None, Some("configured-bot")),
        Some("configured-bot".into())
    );
    assert_eq!(configured_bot_login(None, None, Some("")), None);
    assert_eq!(configured_bot_login(None, None, None), None);
}

async fn auth_test_api() -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut request = [0; 1024];
            let _ = socket.read(&mut request).await;
            let body = r#"{"login":"new-bot","id":42,"type":"User"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    base
}

#[tokio::test]
async fn auth_does_not_rewrite_the_live_daemon_snapshot() {
    // Auth used to load and then save this whole file. The fixture holds
    // an active session and a daemon snapshot, including the old cache,
    // so a byte-for-byte check catches either login or logout doing that.
    let _sandbox = config::test_support::sandbox();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut cfg = Config::default();
    cfg.github.api_url = auth_test_api().await;
    cfg.github.token = Some("legacy-inline-token".into());
    cfg.save().unwrap();
    let live = r#"{
  "bot_login": "daemon-bot",
  "last_poll_at": "2026-09-09T01:00:00Z",
  "last_error": "driver was restarting",
  "repos": {
    "acme/widgets": {
      "issues": {
        "183": {
          "number": 183,
          "title": "Keep this session",
          "worktree_path": "/scratch/widgets.worktrees/issue-183",
          "terminal_handle": "live-terminal",
          "agent_session_id": "live-conversation",
          "active": true
        }
      }
    }
  }
}
"#;
    let state_path = state::state_path();
    std::fs::write(&state_path, live).unwrap();
    // This is the daemon's in-memory snapshot while auth runs.
    let daemon_state = state::State::load().unwrap();

    auth(AuthCommand::Login {
        user: None,
        web: false,
        token: Some("test-token".into()),
        no_keys: true,
        email: None,
        yes: true,
    })
    .await
    .unwrap();
    assert_eq!(
        Config::load().unwrap().github.login.as_deref(),
        Some("new-bot")
    );
    let authenticated = Config::load().unwrap();
    assert!(authenticated.github.token.is_none());
    assert_eq!(authenticated.github_token().unwrap(), "test-token");
    assert_eq!(std::fs::read_to_string(&state_path).unwrap(), live);

    // The daemon may save its snapshot after auth. It keeps the live
    // binding, and it cannot overwrite the separate auth configuration.
    daemon_state.save().unwrap();
    assert_eq!(
        Config::load().unwrap().github.login.as_deref(),
        Some("new-bot")
    );

    let after_daemon_save = std::fs::read_to_string(&state_path).unwrap();
    auth_logout(true).await.unwrap();
    assert!(Config::load().unwrap().github.login.is_none());
    assert_eq!(
        std::fs::read_to_string(&state_path).unwrap(),
        after_daemon_save
    );

    // A blank inline token is not a credential either; it must not make
    // the retained daemon cache look like a live sign-in after logout.
    let mut logged_out = Config::load().unwrap();
    logged_out.github.token = Some(" \t\n ".into());
    logged_out.save().unwrap();

    let status = status::Snapshot {
        cfg: Config::load().unwrap(),
        state: state::State::load().unwrap(),
        workspaces: Vec::new(),
        down: Vec::new(),
        errors: Vec::new(),
    };
    assert!(status.to_json()["bot_login"].is_null());
    assert!(status::render_status(&status).contains("bot:     (not signed in)"));

    let state = state::State::load().unwrap();
    let session = &state.repos["acme/widgets"].issues[&183];
    assert_eq!(session.terminal_handle.as_deref(), Some("live-terminal"));
    assert_eq!(
        session.agent_session_id.as_deref(),
        Some("live-conversation")
    );
    assert!(session.active);
}
