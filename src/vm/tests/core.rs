use super::*;

#[test]
fn tailscale_is_an_explicit_guest_action_with_a_stable_requested_name() {
    let provision = include_str!("../../../vm/guest/provision.sh");
    let enrol = include_str!("../../../vm/guest/tailscale.sh");
    assert!(!provision.contains("tailscale"));
    assert!(enrol.contains("hostname=${1:-ssf-vm}"));
    assert!(enrol.contains("tailscale up --hostname=\"$hostname\""));
    assert!(!enrol.contains("--ssh"));
    assert!(!enrol.contains("--advertise-routes"));
}

#[test]
fn a_bad_download_checksum_removes_the_cached_file() {
    if which("sha256sum").is_none() {
        return;
    }
    let sandbox = crate::config::test_support::sandbox();
    let archive = sandbox.root().join("ubuntu-root.tar.xz");
    std::fs::write(&archive, []).unwrap();
    verify_sha256(
        &archive,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    )
    .unwrap();
    let error = verify_sha256(&archive, &"0".repeat(64))
        .unwrap_err()
        .to_string();
    assert!(error.contains("cached download was removed"));
    assert!(!archive.exists());
}

#[test]
fn disabled_vm_management_preserves_host_factory_and_credentials() {
    let sandbox = crate::config::test_support::sandbox();
    let mut config = Config::default();
    config.github.token = Some("host-only-token".into());
    config.github.ssh_key_path = Some("/missing-host-key".into());
    config.vm.dir = sandbox.root().join("vm").to_string_lossy().into_owned();
    config.save().unwrap();
    let original = std::fs::read(crate::config::config_path()).unwrap();
    let mut vm = Vm::new(&config);
    let client = sandbox.root().join("ssf");
    std::fs::write(&client, "client").unwrap();
    std::fs::write(companion_server_path(&client), "server").unwrap();
    vm.binary = Some(client);
    std::fs::create_dir_all(&vm.dir).unwrap();
    std::fs::write(vm.key().with_extension("pub"), "access-public-key").unwrap();
    vm.ensure_factory_ownership(&config).unwrap();
    let mut stale = config.clone();
    stale.vm.enabled = true;
    // A long-running supervisor must respect the mode now on disk.
    vm.ensure_factory_ownership(&stale).unwrap();
    assert_eq!(
        std::fs::read(crate::config::config_path()).unwrap(),
        original
    );
    assert!(!vm.factory_owned());
    let seed = sandbox.root().join("seed");
    vm.seed_tree(&config, &seed).unwrap();
    assert!(!seed.join("config/config.toml").exists());
    assert!(!seed.join("config/token").exists());
    assert!(!seed.join("migration-source.toml").exists());
}

#[test]
fn changed_host_after_guest_commit_requires_explicit_resolution() {
    let mut host = Config::default();
    host.github.login = Some("original-bot".into());
    let receipt = toml::to_string(&guest_config(&host)).unwrap();
    validate_migration_receipt(&host, &receipt).unwrap();
    host.github.login = Some("another-bot".into());
    assert!(validate_migration_receipt(&host, &receipt).is_err());
}

#[test]
fn established_vm_seed_never_resolves_or_copies_bot_credentials() {
    let sandbox = crate::config::test_support::sandbox();
    let mut config = Config::default();
    config.vm.dir = sandbox.root().join("vm").to_string_lossy().into_owned();
    config.github.token = Some("must-not-be-seeded".into());
    config.github.ssh_key_path = Some("/missing-host-key".into());
    let mut vm = Vm::new(&config);
    let client = sandbox.root().join("ssf");
    std::fs::write(&client, "client").unwrap();
    std::fs::write(companion_server_path(&client), "server").unwrap();
    vm.binary = Some(client);
    std::fs::create_dir_all(&vm.dir).unwrap();
    std::fs::write(vm.dir.join("guest-owned"), "1").unwrap();
    std::fs::write(vm.key().with_extension("pub"), "host-access-public-key").unwrap();
    let seed = sandbox.root().join("seed");
    vm.seed_tree(&config, &seed).unwrap();
    assert!(seed.join("defaults.toml").exists());
    assert!(!seed.join("config/token").exists());
    assert!(!seed.join("config/config.toml").exists());
    assert!(!seed.join("migration-source.toml").exists());
}

#[test]
fn stopped_root_requires_matching_script_after_filesystem_recovery() {
    if which("mkfs.ext4").is_none() || which("debugfs").is_none() {
        return;
    }
    let sandbox = crate::config::test_support::sandbox();
    let mut config = Config::default();
    config.vm.dir = sandbox
        .root()
        .join("vm with spaces")
        .to_string_lossy()
        .into_owned();
    let vm = Vm::new(&config);
    std::fs::create_dir_all(&vm.dir).unwrap();
    let tree = sandbox.root().join("root-tree");
    std::fs::create_dir_all(tree.join("usr/local/lib/ssf")).unwrap();
    std::fs::create_dir_all(tree.join("usr/lib")).unwrap();
    std::fs::write(
        tree.join("usr/lib/os-release"),
        "NAME=Ubuntu\nID=ubuntu\nVERSION_ID=\"24.04\"\n",
    )
    .unwrap();
    std::fs::write(
        tree.join("usr/local/lib/ssf/seed-common.sh"),
        "old destructive script",
    )
    .unwrap();
    let disk = std::fs::File::create(vm.root_disk()).unwrap();
    disk.set_len(16 << 20).unwrap();
    drop(disk);
    run_ok(
        Command::new("mkfs.ext4")
            .args(["-q", "-d"])
            .arg(tree)
            .arg(vm.root_disk()),
        "scratch root",
    )
    .unwrap();
    assert!(
        vm.require_compatible_root()
            .unwrap_err()
            .to_string()
            .contains("refusing to boot")
    );
    // A reset from the same legacy image must still refuse. A rebuild
    // supplies the safe script through the image filesystem, not a patch.
    std::fs::write(
        sandbox
            .root()
            .join("root-tree/usr/local/lib/ssf/seed-common.sh"),
        include_bytes!("../../../vm/guest/seed-common.sh"),
    )
    .unwrap();
    run_ok(
        Command::new("mkfs.ext4")
            .args(["-F", "-q", "-d"])
            .arg(sandbox.root().join("root-tree"))
            .arg(vm.root_disk()),
        "rebuilt scratch root",
    )
    .unwrap();
    vm.require_compatible_root().unwrap();
    vm.require_compatible_root().unwrap();
    std::fs::write(vm.root_disk(), "not an ext4 root").unwrap();
    assert!(
        vm.require_compatible_root()
            .unwrap_err()
            .to_string()
            .contains("no guest boot")
    );
}

#[test]
fn stopped_arch_root_is_refused_before_it_can_see_factory_data() {
    if which("mkfs.ext4").is_none() || which("debugfs").is_none() {
        return;
    }
    let sandbox = crate::config::test_support::sandbox();
    let mut config = Config::default();
    config.vm.dir = sandbox.root().join("vm").to_string_lossy().into_owned();
    let vm = Vm::new(&config);
    std::fs::create_dir_all(&vm.dir).unwrap();
    let tree = sandbox.root().join("arch-root");
    std::fs::create_dir_all(tree.join("usr/local/lib/ssf")).unwrap();
    std::fs::create_dir_all(tree.join("usr/lib")).unwrap();
    std::fs::write(
        tree.join("usr/local/lib/ssf/seed-common.sh"),
        include_bytes!("../../../vm/guest/seed-common.sh"),
    )
    .unwrap();
    std::fs::write(
        tree.join("usr/lib/os-release"),
        "NAME=\"Arch Linux\"\nID=arch\n",
    )
    .unwrap();
    let disk = std::fs::File::create(vm.root_disk()).unwrap();
    disk.set_len(16 << 20).unwrap();
    drop(disk);
    run_ok(
        Command::new("mkfs.ext4")
            .args(["-q", "-d"])
            .arg(tree)
            .arg(vm.root_disk()),
        "scratch Arch root",
    )
    .unwrap();
    let error = vm.require_compatible_root().unwrap_err().to_string();
    assert!(error.contains("not the supported Ubuntu 24.04 LTS image"));
    assert!(error.contains("data disk is untouched"));
}

#[test]
fn stopped_root_replays_legacy_journal_before_trusting_script() {
    if ["mkfs.ext4", "debugfs", "e2fsck"]
        .iter()
        .any(|tool| which(tool).is_none())
    {
        return;
    }
    let sandbox = crate::config::test_support::sandbox();
    let mut config = Config::default();
    config.vm.dir = sandbox.root().join("vm").to_string_lossy().into_owned();
    let vm = Vm::new(&config);
    std::fs::create_dir_all(&vm.dir).unwrap();
    let tree = sandbox.root().join("root-tree");
    let script_path = "/usr/local/lib/ssf/seed-common.sh";
    std::fs::create_dir_all(tree.join("usr/local/lib/ssf")).unwrap();
    std::fs::create_dir_all(tree.join("usr/lib")).unwrap();
    std::fs::write(
        tree.join("usr/lib/os-release"),
        "NAME=Ubuntu\nID=ubuntu\nVERSION_ID=\"24.04\"\n",
    )
    .unwrap();
    std::fs::write(
        tree.join(script_path.trim_start_matches('/')),
        include_bytes!("../../../vm/guest/seed-common.sh"),
    )
    .unwrap();
    let disk = std::fs::File::create(vm.root_disk()).unwrap();
    disk.set_len(64 << 20).unwrap();
    drop(disk);
    run_ok(
        Command::new("mkfs.ext4")
            .args(["-q", "-b", "4096", "-d"])
            .arg(&tree)
            .arg(vm.root_disk()),
        "scratch journaled root",
    )
    .unwrap();
    let block = Command::new("debugfs")
        .args(["-R", &format!("bmap {script_path} 0")])
        .arg(vm.root_disk())
        .output()
        .unwrap();
    assert!(block.status.success());
    let block: u64 = String::from_utf8(block.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Model the state left by an offline patch: the file looks current,
    // but a committed journal transaction still contains its legacy data.
    // Journal replay at boot would restore that obsolete first block.
    let legacy = b"#!/bin/sh\nrm -rf /home/ssf/.config/ssf\n";
    let mut legacy_block = vec![0_u8; 4096];
    legacy_block[..legacy.len()].copy_from_slice(legacy);
    let block_file = sandbox.root().join("legacy-block");
    std::fs::write(&block_file, legacy_block).unwrap();
    let commands = sandbox.root().join("journal-commands");
    std::fs::write(
        &commands,
        format!(
            "journal_open\njournal_write -b {block} \"{}\"\njournal_close\n",
            block_file.display()
        ),
    )
    .unwrap();
    run_ok(
        Command::new("debugfs")
            .args(["-w", "-f"])
            .arg(commands)
            .arg(vm.root_disk()),
        "queue legacy journal transaction",
    )
    .unwrap();
    let read_script = || {
        let output = Command::new("debugfs")
            .args(["-R", &format!("cat {script_path}")])
            .arg(vm.root_disk())
            .output()
            .unwrap();
        assert!(output.status.success());
        output.stdout
    };
    assert_eq!(
        read_script(),
        include_bytes!("../../../vm/guest/seed-common.sh")
    );
    assert!(
        vm.require_compatible_root()
            .unwrap_err()
            .to_string()
            .contains("refusing to boot")
    );
    assert!(read_script().starts_with(legacy));
    // Recovery is persistent: a repeated startup attempt also refuses.
    assert!(vm.require_compatible_root().is_err());
}

#[test]
fn shared_files_cannot_replace_factory_state() {
    for path in [
        ".config/ssf/token",
        "/home/ssf/.config/ssf/config.toml",
        "/var/lib/ssf/home/.config/ssf/keys/bot",
        ".gitconfig",
        "../ssf/.config/ssf/token",
    ] {
        assert!(validate_shared_destination(path).is_err(), "{path}");
    }
    assert!(validate_shared_destination(".ssh/personal-key").is_ok());
}

#[test]
fn guest_initialization_is_persistent_and_idempotent() {
    let sandbox = crate::config::test_support::sandbox();
    let seed = sandbox.root().join("seed");
    let guest = sandbox.root().join("guest");
    std::fs::create_dir_all(&seed).unwrap();
    std::fs::write(
        seed.join("defaults.toml"),
        toml::to_string(&guest_config(&Config::default())).unwrap(),
    )
    .unwrap();
    initialize_guest_factory(&seed, &guest).unwrap();
    std::fs::write(guest.join("token"), "guest-token").unwrap();
    std::fs::write(guest.join("config.toml"), "guest edits").unwrap();
    initialize_guest_factory(&seed, &guest).unwrap();
    assert_eq!(
        std::fs::read_to_string(guest.join("token")).unwrap(),
        "guest-token"
    );
    assert_eq!(
        std::fs::read_to_string(guest.join("config.toml")).unwrap(),
        "guest edits"
    );
}

#[test]
fn legacy_migration_checks_all_conflicts_before_writing() {
    let sandbox = crate::config::test_support::sandbox();
    let seed = sandbox.root().join("seed");
    let candidate = seed.join("config");
    let guest = sandbox.root().join("guest");
    std::fs::create_dir_all(&candidate).unwrap();
    std::fs::create_dir_all(&guest).unwrap();
    let config = toml::to_string(&guest_config(&Config::default())).unwrap();
    std::fs::write(candidate.join("config.toml"), &config).unwrap();
    std::fs::write(candidate.join("token"), "host-token").unwrap();
    std::fs::write(guest.join("token"), "guest-token").unwrap();
    assert!(
        initialize_guest_factory(&seed, &guest)
            .unwrap_err()
            .to_string()
            .contains("conflict")
    );
    assert!(!guest.join("config.toml").exists());
    assert!(!guest.join("guest-owned").exists());
    assert_eq!(
        std::fs::read_to_string(guest.join("token")).unwrap(),
        "guest-token"
    );
    // A retry after explicit resolution imports missing files, retaining
    // identical files from an interrupted earlier import.
    std::fs::write(guest.join("token"), "host-token").unwrap();
    initialize_guest_factory(&seed, &guest).unwrap();
    assert_eq!(
        std::fs::read_to_string(guest.join("config.toml")).unwrap(),
        config
    );
    assert!(guest.join("guest-owned").exists());
}

#[test]
fn differing_legacy_repository_settings_stop_migration() {
    let sandbox = crate::config::test_support::sandbox();
    let seed = sandbox.root().join("seed");
    let guest = sandbox.root().join("guest");
    std::fs::create_dir_all(seed.join("config")).unwrap();
    std::fs::create_dir_all(&guest).unwrap();
    let current = guest_config(&Config::default());
    let mut incoming = current.clone();
    incoming.github.login = Some("different-bot".into());
    std::fs::write(
        seed.join("config/config.toml"),
        toml::to_string(&incoming).unwrap(),
    )
    .unwrap();
    let original = toml::to_string(&current).unwrap();
    std::fs::write(guest.join("config.toml"), &original).unwrap();
    assert!(initialize_guest_factory(&seed, &guest).is_err());
    assert_eq!(
        std::fs::read_to_string(guest.join("config.toml")).unwrap(),
        original
    );
    assert!(!guest.join("guest-owned").exists());
    // Explicit guest precedence: remove the host candidate and retry.
    std::fs::remove_file(seed.join("config/config.toml")).unwrap();
    initialize_guest_factory(&seed, &guest).unwrap();
    assert_eq!(
        std::fs::read_to_string(guest.join("config.toml")).unwrap(),
        original
    );
}

#[test]
fn files_and_assets_follow_the_config() {
    let vm = vm();
    assert_eq!(vm.dir, PathBuf::from("/v/one"));
    assert_eq!(vm.firecracker(), PathBuf::from("/v/firecracker"));
    assert_eq!(vm.rootfs(), PathBuf::from("/v/rootfs.ext4"));
    let mut cfg = Config::default();
    cfg.vm.kernel = Some("~/k/vmlinux".into());
    let vm = Vm::new(&cfg);
    assert!(vm.kernel().ends_with("k/vmlinux"));
    assert!(!vm.kernel().starts_with("~"));
}

#[test]
fn a_directory_nobody_could_look_in_is_not_a_directory_with_nothing_in_it() {
    // A denied `stat` answered a confident "no data disk" about a
    // directory nobody looked into, and that is the one value that
    // lets `ssf uninstall` past its refusal.
    use std::os::unix::fs::PermissionsExt;
    let base = std::env::temp_dir().join(format!(
        "ssf-unread-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut cfg = Config::default();
    cfg.vm.name = "one".into();
    cfg.vm.backend = Some(BackendKind::Firecracker);
    cfg.vm.dir = base.to_string_lossy().into_owned();
    let vm = Vm::new(&cfg);
    std::fs::create_dir_all(&vm.dir).unwrap();
    let disk = vm.dir.join("data.ext4");
    std::fs::write(&disk, b"clones").unwrap();
    // The two answers that are answers, so what follows is about
    // the third and not about the fixture.
    assert_eq!(there(&disk), Some(true));
    assert_eq!(there(&vm.dir.join("nothing-here")), Some(false));
    assert_eq!(vm.survey().data, Some(true));

    let mut perm = std::fs::metadata(&vm.dir).unwrap().permissions();
    perm.set_mode(0o000);
    std::fs::set_permissions(&vm.dir, perm).unwrap();
    let seen = std::fs::metadata(&disk).map_err(|e| e.kind());
    // Taken while the mode is still 0o000, because that is the only
    // moment it says anything: after the restore below it is true
    // for everyone, and a guard that is true for everyone is not a
    // guard.
    let readable_anyway = std::fs::read_dir(&vm.dir).is_ok();
    // Both readings taken while the denial is in force: the cleanup
    // below removes the directory, and a `there` called after it
    // would answer `Some(false)` about a path that really is gone.
    let denied = there(&disk);
    let survey = vm.survey();
    // Restored before the assertions, so a failure still leaves the
    // temporary directory removable.
    let mut perm = std::fs::metadata(&vm.dir).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&vm.dir, perm).unwrap();
    std::fs::remove_dir_all(&base).unwrap();

    match seen {
        Err(std::io::ErrorKind::NotFound) => {
            panic!("the fixture is wrong: the disk was written above")
        }
        Err(_) => {
            assert_eq!(denied, None, "a denied stat is not an answer");
            assert_eq!(
                survey.data, None,
                "and the refusal has to see it as one that was never answered"
            );
        }
        // euid 0 ignores the mode, so this case cannot be built here and
        // this run proves nothing about it. The assertion is the
        // narrow one that is true: the directory really was
        // readable, so the stat above was allowed to succeed.
        Ok(_) => assert!(
            readable_anyway,
            "a stat succeeded through a directory nothing should have been able to read"
        ),
    }
}

#[test]
fn a_stranded_disk_is_a_lima_question_and_an_unread_directory_answers_it_yes() {
    use std::os::unix::fs::PermissionsExt;
    let base = std::env::temp_dir().join(format!(
        "ssf-stranded-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let vm_for = |backend| {
        let mut cfg = Config::default();
        cfg.vm.name = "factory".into();
        cfg.vm.dir = base.to_string_lossy().into_owned();
        cfg.vm.backend = Some(backend);
        Vm::new(&cfg)
    };
    let lima = vm_for(BackendKind::Lima);
    std::fs::create_dir_all(&lima.dir).unwrap();
    assert_eq!(lima.stranded_data_disk(), None, "no such file");

    let disk = lima.dir.join("data.ext4");
    std::fs::write(&disk, b"clones").unwrap();
    assert_eq!(lima.stranded_data_disk().as_deref(), Some(disk.as_path()));
    // Under Firecracker the same file is simply this VM's own disk,
    // and `Survey::data` is what speaks for it.
    assert_eq!(vm_for(BackendKind::Firecracker).stranded_data_disk(), None);

    // A directory nobody could read answers "maybe", and maybe is a
    // reason not to destroy it unasked.
    let mut perm = std::fs::metadata(&lima.dir).unwrap().permissions();
    perm.set_mode(0o000);
    std::fs::set_permissions(&lima.dir, perm).unwrap();
    let seen = std::fs::metadata(&disk).map_err(|e| e.kind());
    // Both readings while the mode is still 0o000: afterwards they
    // are true for everyone and say nothing.
    let unknown = lima.stranded_data_disk();
    let readable_anyway = std::fs::read_dir(&lima.dir).is_ok();
    let mut perm = std::fs::metadata(&lima.dir).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&lima.dir, perm).unwrap();
    std::fs::remove_dir_all(&base).unwrap();

    match seen {
        Err(std::io::ErrorKind::NotFound) => {
            panic!("the fixture is wrong: the disk was written above")
        }
        Err(_) => assert_eq!(unknown.as_deref(), Some(disk.as_path())),
        // euid 0 ignores the mode, so this case cannot be built here
        // and this run proves nothing about it. The assertion is the
        // narrow one that is true: the directory really was readable.
        Ok(_) => assert!(
            readable_anyway,
            "a stat succeeded through a directory nothing should have been able to read"
        ),
    }
}

#[test]
fn under_firecracker_the_directory_is_the_vm() {
    // The disks are the VM, so the directory (or a guest running off
    // them) is the whole answer -- there is no bookkeeping of
    // lima's kind to ask on top of it, and no question that could
    // fail to be asked.
    let mut cfg = Config::default();
    cfg.vm.name = "one".into();
    cfg.vm.backend = Some(BackendKind::Firecracker);
    let dir = std::env::temp_dir().join(format!(
        "ssf-fc-present-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    cfg.vm.dir = dir.to_string_lossy().into_owned();
    let vm = Vm::new(&cfg);
    assert!(!vm.dir.exists());
    let empty = vm.survey();
    std::fs::create_dir_all(&vm.dir).unwrap();
    let built = vm.survey();
    // Removed before the assertions, so a failure leaves nothing in
    // the temporary directory behind it.
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(
        empty,
        Survey {
            present: Some(false),
            running: Some(false),
            startable: false,
            data: Some(false),
        }
    );
    // The directory is the VM, but only the data disk in it holds
    // anyone's work, and a build that got no further than the
    // directory has none.
    assert_eq!(
        built,
        Survey {
            present: Some(true),
            running: Some(false),
            startable: true,
            data: Some(false),
        }
    );
}

#[test]
fn firecracker_config_lists_drives_in_order() {
    let vm = vm();
    let boot = vm.boot_files(&vm.dir);
    let v = vm.fc_config_json(
        &[
            (Path::new("/v/one/root.ext4"), false),
            (Path::new("/v/one/data.ext4"), false),
            (Path::new("/v/one/seed.ext4"), true),
        ],
        &boot,
        None,
    );
    let drives = v["drives"].as_array().unwrap();
    assert_eq!(drives.len(), 3);
    assert_eq!(drives[0]["is_root_device"], true);
    assert_eq!(drives[1]["is_root_device"], false);
    assert_eq!(drives[2]["is_read_only"], true);
    assert_eq!(drives[2]["path_on_host"], "/v/one/seed.ext4");
    assert_eq!(v["vsock"]["uds_path"], "/v/one/v.sock");
    assert_eq!(v["vsock"]["guest_cid"], 3);
    assert_eq!(v["machine-config"]["vcpu_count"], 2);
    assert_eq!(v["machine-config"]["mem_size_mib"], 4096);
    let args = v["boot-source"]["boot_args"].as_str().unwrap();
    assert!(args.contains("root=/dev/vda rw"));
    assert!(!args.contains("init="));
    let p = vm.fc_config_json(
        &[(Path::new("/b/base.ext4"), false)],
        &boot,
        Some("/x/init"),
    );
    assert!(
        p["boot-source"]["boot_args"]
            .as_str()
            .unwrap()
            .ends_with(" init=/x/init")
    );
}

#[test]
fn guest_config_is_herdr_only_on_the_data_disk() {
    let mut host = Config::default();
    host.dashboard.enabled = true;
    host.dashboard.port = 9090;
    host.driver = Some(DriverKind::Orca);
    host.vm.enabled = true;
    host.vm.files = vec!["~/.claude/.credentials.json".into()];
    host.herdr.projects_dir = "~/ssf/projects".into();
    host.repos.push(RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        driver: Some(DriverKind::Orca),
        path: Some("/home/me/r".into()),
        ..RepoConfig::default()
    });
    host.repos.push(RepoConfig {
        name: "o/s".into(),
        harness: "codex".into(),
        ..RepoConfig::default()
    });
    assert_eq!(
        orca_repos(&host),
        vec!["o/r".to_string(), "o/s".to_string()]
    );
    let g = guest_config(&host);
    assert_eq!(g.driver, Some(DriverKind::Herdr));
    assert!(!g.dashboard.enabled);
    assert_eq!(g.dashboard.port, 8787);
    assert!(!g.vm.enabled);
    assert!(g.vm.files.is_empty());
    assert_eq!(g.herdr.projects_dir, GUEST_PROJECTS_DIR);
    assert_eq!(g.herdr.command, GUEST_HERDR);
    assert!(
        g.repos
            .iter()
            .all(|r| r.driver.is_none() && r.path.is_none())
    );
    assert_eq!(g.repos[0].harness, "claude");
    assert_eq!(g.daemon.startup_driver_wait_secs, 0);
    // It round-trips through TOML with nothing unknown.
    let text = toml::to_string_pretty(&g).unwrap();
    let back: Config = toml::from_str(&text).unwrap();
    assert_eq!(back.driver_for(&back.repos[0]), DriverKind::Herdr);
}

#[test]
fn file_specs_land_under_the_guest_home() {
    let home = Path::new("/home/me");
    assert_eq!(
        parse_file_spec("/home/me/.claude/.credentials.json", home),
        (
            PathBuf::from("/home/me/.claude/.credentials.json"),
            ".claude/.credentials.json".to_string()
        )
    );
    assert_eq!(
        parse_file_spec("/etc/thing:/etc/thing", home),
        (PathBuf::from("/etc/thing"), "/etc/thing".to_string())
    );
    assert_eq!(
        parse_file_spec("/etc/thing", home),
        (PathBuf::from("/etc/thing"), "/etc/thing".to_string())
    );
    assert_eq!(
        parse_file_spec("/tmp/key:.config/x/key", home),
        (PathBuf::from("/tmp/key"), ".config/x/key".to_string())
    );
    assert_eq!(
        parse_file_spec("/tmp/key:~/.config/x/key", home),
        (PathBuf::from("/tmp/key"), ".config/x/key".to_string())
    );
    let (src, dest) = parse_file_spec("~/.codex/auth.json", home);
    assert!(!src.starts_with("~"));
    assert!(dest.ends_with(".codex/auth.json"));
}

#[test]
fn captured_status_reuses_only_its_factory_connection() {
    let sandbox = crate::config::test_support::sandbox();
    let mut vm = vm();
    let args = vec!["status".into(), "--json".into()];
    let command_args = |vm: &Vm| {
        vm.capture_ssf_command(&args, sandbox.root())
            .unwrap()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    let first = command_args(&vm);
    assert_eq!(first, command_args(&vm));
    assert!(first.contains(&"ControlMaster=auto".to_owned()));
    assert!(first.contains(&"ControlPersist=60".to_owned()));
    assert!(first.contains(&"ssf@127.0.0.1".to_owned()));
    assert_eq!(first.last().unwrap(), "SSF_VM_GUEST=1 ssf status --json");
    let path = |args: Vec<String>| {
        args.into_iter()
            .find(|arg| arg.starts_with("ControlPath="))
            .unwrap()
    };
    let original = path(first);
    vm.cfg.ssh_port += 1;
    assert_ne!(original, path(command_args(&vm)));
    vm.cfg.ssh_port -= 1;
    vm.dir = PathBuf::from("/v/two");
    assert_ne!(original, path(command_args(&vm)));
    assert!(!vm.ssh_args(true).iter().any(|arg| arg.contains("Control")));
}

#[test]
fn status_control_directory_rejects_shared_and_symlink_paths() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    let sandbox = crate::config::test_support::sandbox();
    let directory = status_control_directory(sandbox.root()).unwrap();
    assert_eq!(std::fs::metadata(&directory).unwrap().mode() & 0o777, 0o700);
    assert_eq!(status_control_directory(sandbox.root()).unwrap(), directory);
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(status_control_directory(sandbox.root()).is_err());
    std::fs::remove_dir(&directory).unwrap();
    symlink(sandbox.root(), &directory).unwrap();
    assert!(status_control_directory(sandbox.root()).is_err());
    std::fs::remove_file(&directory).unwrap();
    std::fs::write(&directory, "occupied").unwrap();
    assert!(status_control_directory(sandbox.root()).is_err());
}

#[test]
fn ssh_args_pin_the_key_port_and_hosts_file() {
    let vm = vm();
    let args = vm.ssh_args(true);
    let joined = args.join(" ");
    assert!(joined.contains("-i /v/one/id_ed25519"));
    assert!(joined.contains("-p 2222"));
    assert!(joined.contains("UserKnownHostsFile=/v/one/known_hosts"));
    assert!(joined.contains("BatchMode=yes"));
    assert!(!vm.ssh_args(false).join(" ").contains("BatchMode"));
    // Every attempt is bounded: a connection that never comes up
    // gives up after ConnectTimeout, and one that goes silent after
    // the keepalives, so the waits that call ssh in a loop cannot be
    // held open by a single attempt. An interactive session keeps the
    // connect timeout but not the keepalives.
    assert!(joined.contains("ConnectTimeout=5"));
    assert!(joined.contains("ServerAliveInterval=15"));
    assert!(joined.contains("ServerAliveCountMax=4"));
    assert!(!vm.ssh_args(false).join(" ").contains("ServerAlive"));
    let cfg = vm.ssh_config();
    assert!(cfg.starts_with("Host ssf-one\n"));
    assert!(cfg.contains("Port 2222"));
    assert!(cfg.contains("User ssf"));
    assert_eq!(
        shell_join(&[
            "ssf".into(),
            "tell".into(),
            "o/r#1".into(),
            "hi there".into()
        ]),
        "ssf tell 'o/r#1' 'hi there'"
    );
    assert_eq!(shell_join(&["it's".into()]), "'it'\\''s'");
}

#[test]
fn every_known_agent_has_a_login_flow() {
    for a in crate::agents::list() {
        let l = login(&a.id).unwrap_or_else(|| panic!("no login for {}", a.id));
        assert!(!l.argv.is_empty());
        assert!(
            !l.credential.starts_with('/'),
            "{} is home-relative",
            l.credential
        );
        assert!(!l.hint.is_empty());
    }
    assert!(login("cursor").is_none());
}

#[test]
fn login_checks_are_shell_tests_in_the_home() {
    assert_eq!(
        login("claude").unwrap().check(),
        "test -s .claude/.credentials.json"
    );
    assert_eq!(
        login("copilot").unwrap().check(),
        "grep -qs token .copilot/config.json"
    );
}

#[test]
fn login_states_parse_the_script_output() {
    let s = parse_login_states("claude 1 1\ncodex 1 0\nomp 0 0\nbroken line\n");
    assert_eq!(s.len(), 3);
    assert!(s[0].installed && s[0].logged_in);
    assert!(s[1].installed && !s[1].logged_in);
    assert!(!s[2].installed && !s[2].logged_in);
}

#[test]
fn url_scanner_finds_the_first_complete_url_once() {
    let mut sc = UrlScanner::default();
    assert_eq!(
        sc.feed(b"If the browser didn't open, visit: https://claude.com/oauth?code=tr"),
        None
    );
    assert_eq!(
        sc.feed(b"ue&state=x\r\nPaste code here >"),
        Some("https://claude.com/oauth?code=true&state=x".into())
    );
    assert_eq!(sc.feed(b"https://second.example/\n"), None);
    // Colour escapes around and inside the URL are dropped.
    let mut sc = UrlScanner::default();
    assert_eq!(
        sc.feed(b"\x1b[1mvisit \x1b[4mhttps://auth.openai.com/codex/device\x1b[0m\n"),
        Some("https://auth.openai.com/codex/device".into())
    );
    let mut sc = UrlScanner::default();
    assert_eq!(sc.feed(b"no url here\n"), None);
    // Two-byte escapes (cursor save, keypad mode) and an OSC title do
    // not swallow the URL; the buffer stays bounded while nothing is pending.
    let mut sc = UrlScanner::default();
    assert_eq!(
        sc.feed(b"\x1b7\x1b=\x1b]0;title\x07\x1b[?25lvisit https://x.example/a\n"),
        Some("https://x.example/a".into())
    );
    let mut sc = UrlScanner::default();
    for _ in 0..1000 {
        assert_eq!(sc.feed(b"some plain output line\n"), None);
    }
    assert!(sc.text.len() < 64, "{}", sc.text.len());
    assert_eq!(
        sc.feed(b"then https://y.example/ done"),
        Some("https://y.example/".into())
    );
}
