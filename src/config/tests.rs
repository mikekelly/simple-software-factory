#[test]
fn web_dashboard_defaults_and_security() {
    let default: super::Config = toml::from_str("").unwrap();
    assert!(!default.dashboard.enabled);
    assert_eq!(default.dashboard.bind.to_string(), "127.0.0.1");
    assert_eq!(default.dashboard.port, 8787);
    let configured: super::Config =
        toml::from_str("[dashboard]\nenabled = true\nbind = '::1'\nport = 9090").unwrap();
    configured.dashboard.validate().unwrap();
    assert_eq!(configured.dashboard.port, 9090);
    for address in ["0.0.0.0", "::", "100.64.0.1", "192.168.1.4"] {
        let mut config = configured.dashboard.clone();
        config.bind = address.parse().unwrap();
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("authenticated TLS")
        );
    }
    assert!(
        toml::from_str::<super::Config>("[dashboard]\nenabled = true\nbind = 'localhost'").is_err()
    );
}

use super::*;

#[test]
fn vm_infrastructure_updates_preserve_host_factory_and_vm_only_layouts() {
    let _sandbox = test_support::sandbox();
    for source in [
        "[vm]\nenabled = true\n",
        "driver = 'herdr'\n[[repo]]\nname = 'owner/host'\nharness = 'claude'\n[github]\nlogin = 'host-bot'\n[vm]\nenabled = true\n",
    ] {
        std::fs::write(config_path(), source).unwrap();
        let before: toml::Table = toml::from_str(source).unwrap();
        let mut cfg = Config::load().unwrap();
        cfg.vm.mem_mib = Some(8192);
        cfg.save_vm_settings().unwrap();
        let after: toml::Table =
            toml::from_str(&std::fs::read_to_string(config_path()).unwrap()).unwrap();
        assert_eq!(after["vm"]["mem_mib"].as_integer(), Some(8192));
        let factory = |mut table: toml::Table| {
            table.remove("vm");
            table
        };
        assert_eq!(factory(after), factory(before));
    }
    // Ordinary saves never perform an ownership migration, even when
    // a host-side ownership marker remains from earlier VM operation.
    let mut cfg = Config::load().unwrap();
    cfg.vm.dir = config_dir().join("vms").to_string_lossy().into_owned();
    let instance = crate::vm::Vm::new(&cfg);
    std::fs::create_dir_all(&instance.dir).unwrap();
    std::fs::write(instance.dir.join("guest-owned"), "1").unwrap();
    cfg.save().unwrap();
    assert_eq!(Config::load().unwrap().repos[0].name, "owner/host");
}

/// The three rules of a per-item override (`ssf handover`): nothing
/// overridden, the repository's own harness, another harness.
#[test]
fn per_item_overrides_follow_the_three_rules() {
    let repo = RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        command: Some("claude --dangerously-skip-permissions".into()),
        model: Some("fable-5.1".into()),
        effort: Some("high".into()),
        ..Default::default()
    };
    // Nothing overridden: the config as it stands.
    let same = repo.with_overrides(None);
    assert_eq!(same.harness, "claude");
    assert_eq!(same.command, repo.command);
    assert_eq!(same.model.as_deref(), Some("fable-5.1"));
    assert_eq!(same.effort.as_deref(), Some("high"));
    // The repository's own harness: the command stays, and each of
    // model and effort is the override's or, unset, the repository's.
    let o = crate::state::Overrides {
        harness: "claude".into(),
        model: Some("opus".into()),
        effort: None,
    };
    let eff = repo.with_overrides(Some(&o));
    assert_eq!(eff.harness, "claude");
    assert_eq!(eff.command, repo.command, "the command belongs to claude");
    assert_eq!(eff.model.as_deref(), Some("opus"));
    assert_eq!(eff.effort.as_deref(), Some("high"), "the repository's");
    assert!(
        eff.harness_command()
            .starts_with("claude --dangerously-skip-permissions"),
        "{}",
        eff.harness_command()
    );
    assert!(
        eff.harness_command().contains("opus"),
        "{}",
        eff.harness_command()
    );
    // Another harness: the command goes with the harness it belonged
    // to, and nothing falls back to the repository's settings.
    let o = crate::state::Overrides {
        harness: "pi".into(),
        model: None,
        effort: Some("medium".into()),
    };
    let eff = repo.with_overrides(Some(&o));
    assert_eq!(eff.harness, "pi");
    assert_eq!(eff.command, None);
    assert_eq!(eff.model, None, "not the repository's claude model");
    assert_eq!(eff.effort.as_deref(), Some("medium"));
    assert!(
        eff.harness_command()
            .starts_with(&crate::models::default_command("pi")),
        "{}",
        eff.harness_command()
    );
    // Everything else about the repository is untouched.
    assert_eq!(eff.name, "o/r");
}

#[test]
fn vm_sizes_stay_unset_until_written_and_old_files_pin_them() {
    // Unset: not in the file, so a later `ssf vm build` chooses them.
    let cfg = Config::default();
    let text = toml::to_string_pretty(&cfg).unwrap();
    assert!(!text.contains("vcpus"), "{text}");
    assert!(!text.contains("mem_mib"), "{text}");
    assert!(!text.contains("data_gib"), "{text}");
    assert!(text.contains("root_gib = 8"), "{text}");
    let back: Config = toml::from_str(&text).unwrap();
    assert_eq!(back.vm.vcpus, None);
    assert_eq!(back.vm.data_gib, None);
    // Set: written and read back.
    let mut cfg = Config::default();
    cfg.vm.vcpus = Some(3);
    cfg.vm.mem_mib = Some(15872);
    cfg.vm.data_gib = Some(80);
    let text = toml::to_string_pretty(&cfg).unwrap();
    assert!(text.contains("data_gib = 80"), "{text}");
    let back: Config = toml::from_str(&text).unwrap();
    assert_eq!(
        (back.vm.vcpus, back.vm.mem_mib, back.vm.data_gib),
        (Some(3), Some(15872), Some(80))
    );
    // A file from before, with the old constants written out, keeps them.
    let old: Config = toml::from_str("[vm]\nvcpus = 2\nmem_mib = 4096\ndata_gib = 20\n").unwrap();
    assert_eq!(old.vm.data_gib, Some(20));
}

#[test]
fn vm_backend_keys_round_trip_and_stay_unset_by_default() {
    let cfg = Config::default();
    let text = toml::to_string_pretty(&cfg).unwrap();
    for k in [
        "backend",
        "image",
        "vm_type",
        "guest_binary",
        "limactl",
        "herdr",
    ] {
        assert!(!text.contains(&format!("\n{k} =")), "{k} in {text}");
    }
    let back: Config = toml::from_str(&text).unwrap();
    assert_eq!(back.vm.backend, None);
    let mut cfg = Config::default();
    cfg.vm.backend = Some(BackendKind::Lima);
    cfg.vm.image = Some("https://example.com/arch.qcow2".into());
    cfg.vm.vm_type = Some("vz".into());
    cfg.vm.guest_binary = Some("~/dl/ssf-linux".into());
    cfg.vm.limactl = Some("/opt/homebrew/bin/limactl".into());
    cfg.vm.herdr = Some("~/dl/herdr-linux".into());
    let text = toml::to_string_pretty(&cfg).unwrap();
    assert!(text.contains("backend = \"lima\""), "{text}");
    assert!(text.contains("vm_type = \"vz\""), "{text}");
    let back: Config = toml::from_str(&text).unwrap();
    assert_eq!(back.vm.backend, Some(BackendKind::Lima));
    assert_eq!(
        back.vm.image.as_deref(),
        Some("https://example.com/arch.qcow2")
    );
    assert_eq!(back.vm.guest_binary.as_deref(), Some("~/dl/ssf-linux"));
    assert_eq!(
        back.vm.limactl.as_deref(),
        Some("/opt/homebrew/bin/limactl")
    );
    assert_eq!(back.vm.herdr.as_deref(), Some("~/dl/herdr-linux"));
    // The value is checked at load; the string forms match the ids.
    assert!(toml::from_str::<Config>("[vm]\nbackend = \"docker\"\n").is_err());
    assert_eq!("lima".parse::<BackendKind>().unwrap(), BackendKind::Lima);
    assert_eq!(
        "Firecracker".parse::<BackendKind>().unwrap(),
        BackendKind::Firecracker
    );
    assert!("qemu".parse::<BackendKind>().is_err());
    assert_eq!(BackendKind::Lima.to_string(), "lima");
    assert_eq!(BackendKind::default_for("macos"), BackendKind::Lima);
    assert_eq!(BackendKind::default_for("linux"), BackendKind::Firecracker);
}

#[test]
fn drivers_come_from_the_top_level_and_per_repo() {
    let cfg: Config = toml::from_str(
        r#"
driver = "herdr"
[herdr]
projects_dir = "~/work"
[[repo]]
name = "a/b"
harness = "claude"
[[repo]]
name = "c/d"
harness = "claude"
driver = "orca"
"#,
    )
    .unwrap();
    assert_eq!(cfg.driver, Some(DriverKind::Herdr));
    assert_eq!(cfg.driver_for(&cfg.repos[0]), DriverKind::Herdr);
    assert_eq!(cfg.driver_for(&cfg.repos[1]), DriverKind::Orca);
    assert_eq!(
        cfg.drivers_in_use(),
        vec![DriverKind::Orca, DriverKind::Herdr]
    );
    assert!(cfg.projects_dir(DriverKind::Herdr).ends_with("work"));
    assert!(
        cfg.projects_dir(DriverKind::Orca)
            .ends_with("orca/projects")
    );
    let empty = Config::default();
    assert_eq!(empty.driver, None);
    assert_eq!(empty.default_driver(), DriverKind::Herdr);
    assert_eq!(empty.drivers_in_use(), vec![DriverKind::Herdr]);
    assert_eq!("Herdr".parse::<DriverKind>().unwrap(), DriverKind::Herdr);
    assert!("tmux".parse::<DriverKind>().is_err());
    // The per-repo choice round-trips through the file.
    let text = toml::to_string(&cfg).unwrap();
    assert!(text.contains("driver = \"orca\""));
    let again: Config = toml::from_str(&text).unwrap();
    assert_eq!(again.repos[1].driver, Some(DriverKind::Orca));
    assert_eq!(again.repos[0].driver, None);
}

#[test]
fn startup_wait_reads_its_old_orca_name_and_writes_the_new_one() {
    let old: Config = toml::from_str("[daemon]\nstartup_orca_wait_secs = 7\n").unwrap();
    assert_eq!(old.daemon.startup_driver_wait_secs, 7);
    let new: Config = toml::from_str("[daemon]\nstartup_driver_wait_secs = 9\n").unwrap();
    assert_eq!(new.daemon.startup_driver_wait_secs, 9);
    assert_eq!(Config::default().daemon.startup_driver_wait_secs, 120);
    let text = toml::to_string(&old).unwrap();
    assert!(text.contains("startup_driver_wait_secs = 7"));
    assert!(!text.contains("startup_orca_wait_secs"));
}

#[test]
fn herdr_is_the_default_driver_and_repos_fall_back_to_it() {
    let cfg: Config = toml::from_str(
        r#"
[orca]
projects_dir = "~/orca/projects"
[[repo]]
name = "a/b"
harness = "claude"
[[repo]]
name = "c/d"
harness = "claude"
driver = "orca"
"#,
    )
    .unwrap();
    assert_eq!(DriverKind::default(), DriverKind::Herdr);
    assert_eq!(cfg.driver, None);
    assert_eq!(cfg.default_driver(), DriverKind::Herdr);
    assert_eq!(cfg.driver_for(&cfg.repos[0]), DriverKind::Herdr);
    assert_eq!(cfg.driver_for(&cfg.repos[1]), DriverKind::Orca);
    assert_eq!(
        cfg.drivers_in_use(),
        vec![DriverKind::Orca, DriverKind::Herdr]
    );
    // The unset key stays unset through a save, so a later
    // `ssf repo add` does not silently pin the new default.
    let text = toml::to_string(&cfg).unwrap();
    assert!(!text.starts_with("driver"), "{text}");
    assert!(!text.contains("\ndriver = \"herdr\""), "{text}");
    let again: Config = toml::from_str(&text).unwrap();
    assert_eq!(again.driver, None);
}

#[test]
fn driver_note_only_when_a_repo_relies_on_the_unset_default() {
    let mut cfg: Config = toml::from_str(
        r#"
[[repo]]
name = "a/b"
harness = "claude"
[[repo]]
name = "c/d"
harness = "claude"
driver = "orca"
"#,
    )
    .unwrap();
    let note = cfg.driver_note().expect("a/b relies on the default");
    assert!(note.contains("a/b runs in herdr"), "{note}");
    assert!(!note.contains("c/d"), "{note}");
    assert!(note.contains("ssf config set driver orca"), "{note}");
    // Set explicitly (either way): nothing to say.
    cfg.driver = Some(DriverKind::Orca);
    assert_eq!(cfg.driver_note(), None);
    cfg.driver = Some(DriverKind::Herdr);
    assert_eq!(cfg.driver_note(), None);
    // Unset, but every repository picks its own: nothing to say.
    cfg.driver = None;
    cfg.repos[0].driver = Some(DriverKind::Herdr);
    assert_eq!(cfg.driver_note(), None);
    // No repositories at all: nothing runs anywhere yet.
    assert_eq!(Config::default().driver_note(), None);
}

fn parse(toml_src: &str) -> Result<Config> {
    let dir = std::env::temp_dir().join(format!("ssf-config-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!(
        "{}.toml",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, toml_src).unwrap();
    let r = Config::load_from(&path);
    let _ = std::fs::remove_file(&path);
    r
}

#[test]
fn the_old_reviewer_keys_still_load_and_are_not_written_back() {
    let cfg = parse(
        r#"
[daemon]
review_label = "review"
cleanup_grace_secs = 900

[[repo]]
name = "acme/widgets"
harness = "claude"
"#,
    )
    .unwrap();
    assert_eq!(cfg.daemon.review_label.as_deref(), Some("review"));
    assert_eq!(cfg.daemon.cleanup_grace_secs, Some(900));
    assert_eq!(
        cfg.daemon.retired_keys(),
        vec!["daemon.review_label", "daemon.cleanup_grace_secs"]
    );
    assert!(DaemonConfig::default().retired_keys().is_empty());
    let out = toml::to_string(&cfg).unwrap();
    assert!(!out.contains("review_label"), "{out}");
    assert!(!out.contains("cleanup_grace_secs"), "{out}");
}

#[test]
fn model_and_effort_are_applied_to_the_command() {
    let cfg = parse(
        r#"
[[repo]]
name = "acme/widgets"
harness = "claude"
command = "claude --dangerously-skip-permissions"
model = "opus"
effort = "high"
"#,
    )
    .unwrap();
    assert_eq!(
        cfg.repos[0].harness_command(),
        "claude --dangerously-skip-permissions --model opus --effort high"
    );
    let cfg = parse(
        r#"
[[repo]]
name = "acme/widgets"
harness = "codex"
model = "gpt-5.5"
"#,
    )
    .unwrap();
    assert_eq!(
        cfg.repos[0].harness_command(),
        "codex --dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust -m gpt-5.5"
    );
}

#[test]
fn default_command_is_permission_free_and_command_overrides_it() {
    let cfg = parse(
        r#"
[[repo]]
name = "acme/widgets"
harness = "claude"

[[repo]]
name = "acme/gadgets"
harness = "claude"
command = "claude --permission-mode acceptEdits"

[[repo]]
name = "acme/other"
harness = "aider"
"#,
    )
    .unwrap();
    assert_eq!(
        cfg.repos[0].harness_command(),
        "claude --dangerously-skip-permissions --disallowedTools AskUserQuestion"
    );
    assert_eq!(
        cfg.repos[1].harness_command(),
        "claude --permission-mode acceptEdits"
    );
    assert_eq!(cfg.repos[2].harness_command(), "aider");
    // Resuming builds on the same base.
    assert_eq!(
        crate::sessions::resume_command("claude", &cfg.repos[0].harness_command(), "abc").unwrap(),
        "claude --dangerously-skip-permissions --disallowedTools AskUserQuestion --resume abc"
    );
}

#[test]
fn bad_effort_is_rejected_at_load() {
    let err = parse(
        r#"
[[repo]]
name = "acme/widgets"
harness = "claude"
effort = "ultra"
"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("acme/widgets"), "{err:#}");
    assert!(format!("{err:#}").contains("ultra"), "{err:#}");
}

#[test]
fn model_for_a_harness_without_model_support_is_rejected() {
    assert!(
        parse(
            r#"
[[repo]]
name = "acme/widgets"
harness = "crush"
model = "x"
"#,
        )
        .is_err()
    );
}

#[test]
fn settings_round_trip_through_toml() {
    let cfg = parse(
        r#"
[[repo]]
name = "acme/widgets"
harness = "claude"
model = "sonnet"
effort = "low"
"#,
    )
    .unwrap();
    let out = toml::to_string_pretty(&cfg).unwrap();
    assert!(out.contains("model = \"sonnet\""), "{out}");
    assert!(out.contains("effort = \"low\""), "{out}");
}
#[test]
fn event_comments_default_on_and_the_repository_has_the_last_word() {
    let cfg = parse(
        r#"
[[repo]]
name = "acme/widgets"
harness = "claude"

[[repo]]
name = "acme/quiet"
harness = "claude"
event_comments = false
"#,
    )
    .unwrap();
    assert!(cfg.daemon.event_comments, "on unless switched off");
    assert!(cfg.event_comments(&cfg.repos[0]));
    assert!(!cfg.event_comments(&cfg.repos[1]));
    // Unset per repository is not written; set is.
    let text = toml::to_string_pretty(&cfg).unwrap();
    assert_eq!(text.matches("event_comments").count(), 2, "{text}");
    assert!(
        text.contains("[daemon]\nevent_comments = true")
            || text.contains("event_comments = true\n"),
        "{text}"
    );
    // The instance switched off, a repository back on.
    let cfg = parse(
        r#"
[daemon]
event_comments = false

[[repo]]
name = "acme/widgets"
harness = "claude"

[[repo]]
name = "acme/loud"
harness = "claude"
event_comments = true
"#,
    )
    .unwrap();
    assert!(!cfg.event_comments(&cfg.repos[0]));
    assert!(cfg.event_comments(&cfg.repos[1]));
}

#[test]
fn a_wildcard_allow_list_needs_its_marker() {
    let err = parse(
        r#"
[daemon]
allowed_users = ["*"]
"#,
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("--accept-anyone-risk"),
        "{err:#}"
    );
    assert!(
        format!("{err:#}").contains("daemon.allowed_users"),
        "{err:#}"
    );
    let cfg = parse(
        r#"
[daemon]
allowed_users = ["*"]
accepted_anyone_risk = true
"#,
    )
    .unwrap();
    assert!(cfg.anyone_allowed_anywhere());
    let err = parse(
        r#"
[[repo]]
name = "acme/widgets"
harness = "claude"
allowed_users = ["alice", "*"]
"#,
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("acme/widgets"), "{err:#}");
    assert!(
        format!("{err:#}").contains("ssf repo set acme/widgets"),
        "{err:#}"
    );
    let cfg = parse(
        r#"
[[repo]]
name = "acme/widgets"
harness = "claude"
allowed_users = ["alice", "*"]
accepted_anyone_risk = true

[[repo]]
name = "acme/gadgets"
harness = "claude"
allowed_users = ["alice"]
"#,
    )
    .unwrap();
    assert!(cfg.anyone_allowed(&cfg.repos[0]));
    assert!(!cfg.anyone_allowed(&cfg.repos[1]));
    assert!(cfg.anyone_allowed_anywhere());
    // The marker round-trips only when set.
    let out = toml::to_string_pretty(&cfg).unwrap();
    assert_eq!(
        out.matches("accepted_anyone_risk = true").count(),
        1,
        "{out}"
    );
    assert!(!out.contains("accepted_anyone_risk = false"), "{out}");
}

#[test]
fn the_repo_list_replaces_the_instance_list() {
    let cfg = parse(
        r#"
[github]
login = "bot"

[daemon]
allowed_users = ["Alice"]

[[repo]]
name = "acme/a"
harness = "claude"

[[repo]]
name = "acme/b"
harness = "claude"
allowed_users = ["bob"]

[[repo]]
name = "acme/c"
harness = "claude"
allowed_users = []
"#,
    )
    .unwrap();
    use crate::allow::Source;
    let (a, b, c) = (&cfg.repos[0], &cfg.repos[1], &cfg.repos[2]);
    assert_eq!(
        cfg.allowed_users(a).map(|(l, s)| (l.to_vec(), s)),
        Some((vec!["Alice".to_string()], Source::Instance))
    );
    assert_eq!(
        cfg.allowed_users(b).map(|(l, s)| (l.to_vec(), s)),
        Some((vec!["bob".to_string()], Source::Repo))
    );
    assert_eq!(
        cfg.allowed_users(c).map(|(l, s)| (l.to_vec(), s)),
        Some((vec![], Source::Repo))
    );
    assert_eq!(cfg.access_summary(a), "@alice (instance list)");
    assert_eq!(cfg.access_summary(b), "@bob (repo list)");
    assert_eq!(
        cfg.access_summary(c),
        "nobody but the bot (repo list: empty)"
    );
    assert!(!cfg.anyone_allowed_anywhere());
    // Neither set: the collaborators, fetched by the daemon.
    let cfg = parse(
        r#"
[[repo]]
name = "acme/a"
harness = "claude"
"#,
    )
    .unwrap();
    assert!(cfg.allowed_users(&cfg.repos[0]).is_none());
    assert_eq!(
        cfg.access_summary(&cfg.repos[0]),
        "collaborators with push access (default)"
    );
    assert!(!cfg.anyone_allowed_anywhere());
}

fn bot_config() -> Config {
    let mut cfg = Config::default();
    cfg.github.login = Some("acme-bot".into());
    cfg.github.email = Some("1+acme-bot@users.noreply.github.com".into());
    cfg.github.ssh_key_path = Some("/keys/acme-bot_ed25519".into());
    cfg.github.signing_key_id = Some(7);
    cfg.repos.push(RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        ..RepoConfig::default()
    });
    cfg
}

#[test]
fn git_identity_is_the_bot_unless_configured() {
    let cfg = bot_config();
    let id = cfg.git_identity(Some(&cfg.repos[0]));
    assert!(id.is_bot());
    assert_eq!(id.name.as_deref(), Some("acme-bot"));
    assert_eq!(
        id.email.as_deref(),
        Some("1+acme-bot@users.noreply.github.com")
    );
    assert_eq!(
        id.signing_key.as_deref(),
        Some(Path::new("/keys/acme-bot_ed25519"))
    );
    assert_eq!(id.credential, Credential::Bot);
    // Nothing recorded at all: no identity, still the bot credential.
    let empty = Config::default();
    let id = empty.git_identity(None);
    assert!(id.name.is_none() && id.email.is_none() && id.signing_key.is_none());
    assert!(id.who().contains("no identity"));
    // A bot without a signing key enrolled signs nothing.
    let mut cfg = bot_config();
    cfg.github.signing_key_id = None;
    assert!(cfg.git_identity(None).signing_key.is_none());
}

#[test]
fn git_tables_merge_key_by_key_with_the_repo_winning() {
    let mut cfg = bot_config();
    cfg.git = GitConfig {
        name: Some("Ann Person".into()),
        email: Some("ann@example.com".into()),
        signing_key: None,
        credential: Some("token:ann".into()),
    };
    // Instance identity: a person, unsigned by default, pushing as herself.
    let id = cfg.git_identity(Some(&cfg.repos[0]));
    assert_eq!(id.source, IdentitySource::Instance);
    assert_eq!(id.who(), "Ann Person <ann@example.com>");
    assert!(
        id.signing_key.is_none(),
        "a person is unsigned unless a key is given"
    );
    assert_eq!(id.credential, Credential::Token("ann".into()));
    // The repo overrides one key and adds a signing key.
    cfg.repos[0].git = GitConfig {
        email: Some("ann@work.example".into()),
        signing_key: Some(SigningKey::Path("~/.ssh/id_ed25519".into())),
        credential: Some("bot".into()),
        ..GitConfig::default()
    };
    let id = cfg.git_identity(Some(&cfg.repos[0]));
    assert_eq!(id.source, IdentitySource::Repo);
    assert_eq!(id.who(), "Ann Person <ann@work.example>");
    assert_eq!(
        id.signing_key,
        Some(expand_tilde("~/.ssh/id_ed25519")),
        "signing key comes from the repo table"
    );
    assert_eq!(id.credential, Credential::Bot);
    // `[git]` alone still applies with no repository given.
    assert_eq!(
        cfg.git_identity(None).credential,
        Credential::Token("ann".into())
    );
    // Turning the bot's signing off without changing who commits.
    let mut cfg = bot_config();
    cfg.git.signing_key = Some(SigningKey::Off(false));
    let id = cfg.git_identity(Some(&cfg.repos[0]));
    assert!(id.is_bot());
    assert!(id.signing_key.is_none());
    assert!(
        id.describe("acme-bot").contains("unsigned"),
        "{}",
        id.describe("acme-bot")
    );
}

#[test]
fn credential_values_parse() {
    assert_eq!(Credential::parse("bot").unwrap(), Credential::Bot);
    assert_eq!(Credential::parse(" Bot ").unwrap(), Credential::Bot);
    assert_eq!(
        Credential::parse("token:@ann").unwrap(),
        Credential::Token("ann".into())
    );
    assert_eq!(
        Credential::parse("file:/run/secrets/gh").unwrap(),
        Credential::File(PathBuf::from("/run/secrets/gh"))
    );
    assert_eq!(
        Credential::parse("!gh auth git-credential").unwrap(),
        Credential::Helper("!gh auth git-credential".into())
    );
    assert!(Credential::parse("").is_err());
    assert!(Credential::parse("token:").is_err());
    assert!(Credential::parse("file: ").is_err());
    assert!(
        Credential::parse("tokn:ann").is_err(),
        "a misspelt kind is not a helper"
    );
    assert_eq!(
        Credential::parse("cache --timeout=3600").unwrap(),
        Credential::Helper("cache --timeout=3600".into())
    );
    assert_eq!(
        Credential::parse("/usr/lib/git-core/git-credential-libsecret").unwrap(),
        Credential::Helper("/usr/lib/git-core/git-credential-libsecret".into())
    );
    for c in [
        Credential::Bot,
        Credential::Token("ann".into()),
        Credential::File(PathBuf::from("/t")),
        Credential::Helper("store".into()),
    ] {
        assert_eq!(Credential::parse(&c.to_config()).unwrap(), c);
    }
}

#[test]
fn a_vm_name_that_is_not_a_directory_under_vm_dir_is_refused_at_load() {
    // `Vm::new` is `base.join(&cfg.vm.name)`, and `ssf vm destroy`
    // and `ssf uninstall` remove the result whole. `join` gives
    // three ways for that result to be somewhere nobody meant, and
    // each is a `remove_dir_all` of the wrong tree.
    let load = |name: &str| -> Result<Config> {
        let cfg: Config = toml::from_str(&format!("[vm]\nname = {name:?}\n"))?;
        cfg.validate()?;
        Ok(cfg)
    };
    // Asserted through the join, not just through the message, so
    // this says what the name would have done rather than only that
    // it was refused.
    let base = std::path::Path::new("/g/vm");
    for (name, why) in [
        ("", "is empty"),
        (".", "names no directory"),
        ("/srv/other", "is an absolute path"),
        ("..", "contains `..`"),
        ("a/../..", "contains `..`"),
        ("../elsewhere", "contains `..`"),
    ] {
        let err = load(name).unwrap_err();
        assert!(format!("{err:#}").contains(why), "{name:?}: {err:#}");
    }
    // One predicate, asked both ways, and it is the property the
    // guard exists to protect: does the join make a *new* directory
    // strictly under `[vm] dir`? Every refused name fails it, and
    // every accepted one passes -- so this discriminates rather
    // than restating the list above.
    let strictly_under = |name: &str| {
        let j = base.join(name);
        j.starts_with(base)
            && j != base
            && !j
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
    };
    for name in ["", ".", "..", "a/../..", "../elsewhere", "/srv/other"] {
        assert!(
            !strictly_under(name),
            "{name:?} joins to {}, which `ssf vm destroy` would remove",
            base.join(name).display()
        );
    }

    // Ordinary and nested names are untouched: `a/b` is a directory
    // under `[vm] dir` like any other, and refusing it would break
    // configurations that are doing nothing wrong.
    for name in [
        "factory",
        "a/b",
        "one/two/three",
        "./nested",
        "with space",
        "   ",
    ] {
        let cfg = load(name).unwrap_or_else(|e| panic!("{name:?} is a fine name: {e:#}"));
        assert_eq!(cfg.vm.name, name);
        assert!(strictly_under(name), "{name:?}");
    }
}

#[test]
fn git_tables_are_validated_at_load() {
    let load = |text: &str| -> Result<Config> {
        let cfg: Config = toml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    };
    let ok = load(
            "[git]\nname = \"Ann\"\nemail = \"ann@example.com\"\nsigning_key = false\n\n\
             [[repo]]\nname = \"o/r\"\nharness = \"claude\"\n[repo.git]\ncredential = \"token:ann\"\n",
        )
        .unwrap();
    assert_eq!(ok.git.signing_key, Some(SigningKey::Off(false)));
    assert_eq!(ok.repos[0].git.credential.as_deref(), Some("token:ann"));
    // Name without email, at one level or across two.
    assert!(load("[git]\nname = \"Ann\"\n").is_err());
    let err = load("[[repo]]\nname = \"o/r\"\nharness = \"claude\"\n[repo.git]\nemail = \"a@b\"\n")
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("git.email is set without git.name"),
        "{err:#}"
    );
    // Email at the instance and name on the repo is a whole identity.
    assert!(
            load(
                "[git]\nemail = \"a@b\"\n[[repo]]\nname = \"o/r\"\nharness = \"claude\"\n[repo.git]\nname = \"Ann\"\n"
            )
            .is_err(),
            "the instance table alone is still half an identity"
        );
    assert!(load("[git]\nsigning_key = true\n").is_err());
    assert!(load("[git]\ncredential = \"token:\"\n").is_err());
    assert!(load("[git]\nunknown = 1\n").is_err());
    // An empty table round-trips to nothing.
    let text = toml::to_string_pretty(&Config::default()).unwrap();
    assert!(!text.contains("[git]"), "{text}");
}

/// A bare `herdr` that is not on PATH resolves to `~/.local/bin/herdr`
/// when that file exists; otherwise, and for any path, it is returned
/// as given.
#[test]
fn herdr_command_falls_back_to_local_bin() {
    let home = std::env::temp_dir().join(format!("ssf-herdr-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let empty_path = std::ffi::OsString::from(home.join("nothing-here"));
    // Not installed anywhere: the name as given.
    assert_eq!(
        herdr_command_path_in("herdr", Some(&empty_path), Some(&home)),
        PathBuf::from("herdr")
    );
    // A file that cannot run is not the CLI: still the name as given.
    let write = |path: &Path, mode: u32| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    write(&bin.join("herdr"), 0o644);
    assert_eq!(
        herdr_command_path_in("herdr", Some(&empty_path), Some(&home)),
        PathBuf::from("herdr")
    );
    // The installer's default location.
    write(&bin.join("herdr"), 0o755);
    assert_eq!(
        herdr_command_path_in("herdr", Some(&empty_path), Some(&home)),
        bin.join("herdr")
    );
    // On PATH: the bare name stays so PATH resolves it.
    let on_path = std::ffi::OsString::from(&bin);
    assert_eq!(
        herdr_command_path_in("herdr", Some(&on_path), Some(&home)),
        PathBuf::from("herdr")
    );
    // A non-executable placeholder on PATH does not stop the fallback.
    let stub_dir = home.join("stubs");
    std::fs::create_dir_all(&stub_dir).unwrap();
    write(&stub_dir.join("herdr"), 0o644);
    let stub_path = std::ffi::OsString::from(&stub_dir);
    assert_eq!(
        herdr_command_path_in("herdr", Some(&stub_path), Some(&home)),
        bin.join("herdr")
    );
    // A path is never rewritten.
    assert_eq!(
        herdr_command_path_in("/opt/herdr/bin/herdr", Some(&empty_path), Some(&home)),
        PathBuf::from("/opt/herdr/bin/herdr")
    );
    assert_eq!(
        herdr_command_path_in("herdr", None, None),
        PathBuf::from("herdr")
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// The resolution rules behind `config_dir`/`state_dir`, which the test
/// build never runs for real: the environment wins outright, otherwise
/// the platform's directory gets `ssf` on the end, and a platform that
/// answers nothing falls back to the literal path.
#[test]
fn a_directory_follows_the_environment_then_the_platform_then_the_fallback() {
    assert_eq!(
        dir_from(
            Some("/scratch/ssf"),
            Some(PathBuf::from("/home/u/.config")),
            "~/.config"
        ),
        PathBuf::from("/scratch/ssf")
    );
    assert_eq!(
        dir_from(
            None,
            Some(PathBuf::from("/home/u/.local/state")),
            "~/.local/state"
        ),
        PathBuf::from("/home/u/.local/state/ssf")
    );
    assert_eq!(
        dir_from(None, None, "~/.local/state"),
        PathBuf::from("~/.local/state/ssf")
    );
}

#[test]
fn blank_tokens_are_not_configured_credentials() {
    let _sandbox = test_support::sandbox();
    for token in ["", " \t\n "] {
        let mut cfg = Config::default();
        cfg.github.token = Some(token.into());
        assert_eq!(cfg.token_source(), "none");
        assert!(cfg.github_token().is_err());
    }
    std::fs::write(token_path(), " \t\n ").unwrap();
    let cfg = Config::default();
    assert_eq!(cfg.token_source(), "none");
    assert!(cfg.github_token().is_err());
}

/// While a sandbox is held, every path the daemon writes to is inside
/// it, whatever `SSF_CONFIG_DIR`/`SSF_STATE_DIR` say (#140).
#[test]
fn a_sandbox_takes_over_both_directories_and_goes_away_with_the_guard() {
    let root = {
        let sb = test_support::sandbox();
        assert_eq!(config_dir(), sb.config_dir());
        assert_eq!(state_dir(), sb.state_dir());
        assert_eq!(config_path(), sb.config_dir().join("config.toml"));
        assert_eq!(token_path(), sb.config_dir().join("token"));
        assert_eq!(
            crate::state::state_path(),
            sb.state_dir().join("state.json")
        );
        assert!(sb.root().is_dir());
        sb.root().to_path_buf()
    };
    assert!(!root.exists(), "the sandbox outlived its guard");
}

/// A nested sandbox is in force until it is dropped, and the outer one
/// comes back after it.
#[test]
fn a_nested_sandbox_puts_the_outer_one_back() {
    let outer = test_support::sandbox();
    {
        let inner = test_support::sandbox();
        assert_eq!(state_dir(), inner.state_dir());
    }
    assert_eq!(state_dir(), outer.state_dir());
}

/// macOS keeps ssf's directories in the XDG places under `~`, never
/// `~/Library/Application Support`, so a guest and its host name the
/// same paths. Asserted for both platforms from here, since the suite
/// only ever runs on one of them.
#[test]
fn macos_keeps_the_xdg_places_under_the_home_directory() {
    let home = dirs::home_dir().expect("a home directory");
    assert_eq!(platform_config_base(true), Some(home.join(".config")));
    assert_eq!(platform_state_base(true), Some(home.join(".local/state")));
    assert_eq!(
        dir_from(None, platform_config_base(true), "~/.config"),
        home.join(".config/ssf")
    );
    assert_eq!(
        dir_from(None, platform_state_base(true), "~/.local/state"),
        home.join(".local/state/ssf")
    );
    // The environment still wins on either platform.
    for macos in [true, false] {
        assert_eq!(
            dir_from(Some("/scratch"), platform_config_base(macos), "~/.config"),
            PathBuf::from("/scratch")
        );
    }
}

/// The escape the `#[ignore]`d live tests take: the machine's own
/// directories, answered without creating or deleting anything. This
/// one only compares paths — nothing `cargo test` runs on its own may
/// write through that guard.
#[test]
fn the_live_test_guard_answers_the_machine_s_own_directories() {
    let _machine = test_support::the_machine_itself();
    assert_eq!(config_dir(), real_config_dir());
    assert_eq!(state_dir(), real_state_dir());
    assert_eq!(
        crate::state::state_path(),
        real_state_dir().join("state.json")
    );
}

/// The point of the guard: a test that would have written to the live
/// daemon's state file fails where it would have written, and the
/// message says what to do about it.
#[test]
#[should_panic(expected = "reached the real state directory")]
fn without_a_sandbox_the_state_directory_is_refused() {
    let _ = state_dir();
}

#[test]
#[should_panic(expected = "reached the real config directory")]
fn without_a_sandbox_the_config_directory_is_refused() {
    let _ = config_dir();
}
