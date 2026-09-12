use super::*;

#[test]
fn df_and_meminfo_parse() {
    let d = parse_df(
        "    Used    Avail    1B-blocks
17000000000 3000000000 21000000000
",
    )
    .unwrap();
    assert_eq!(d.used_bytes, 17_000_000_000);
    assert_eq!(d.pct(), 85);
    assert!(d.is_full());
    assert!(d.describe().starts_with("15.8 of 20 GiB used (85%)"));
    let d = parse_df("1 9 10").unwrap();
    assert_eq!(d.pct(), 10);
    assert!(!d.is_full());
    assert!(parse_df("garbage").is_none());
    assert!(parse_df("").is_none());
    let m = parse_meminfo(
            "MemTotal:       16384000 kB\nMemFree:  100 kB\nMemAvailable:    8192000 kB\nSwapTotal:  0 kB\nSwapFree:   0 kB\n",
        )
        .unwrap();
    assert_eq!(m.total_kib, 16_384_000);
    assert!(!m.is_short());
    assert_eq!(m.describe(), "8000 of 16000 MiB available");
    let m = parse_meminfo("MemTotal: 1000 kB\nMemAvailable: 99 kB\n").unwrap();
    assert!(m.is_short());
    let m = parse_meminfo(
        "MemTotal: 1000 kB\nMemAvailable: 900 kB\nSwapTotal: 2048 kB\nSwapFree: 1024 kB\n",
    )
    .unwrap();
    assert!(m.is_short());
    assert!(m.describe().ends_with(", 1 MiB swapped out"));
    assert!(parse_meminfo("MemFree: 1 kB\n").is_none());
    // This machine reads.
    let f = HostFacts::probe(Path::new("/nonexistent/deeper/still")).unwrap();
    assert!(f.cpus >= 1 && f.mem_mib > 0);
    assert_eq!(f.mount, "/");
    let here = disk_use(Path::new("/")).unwrap();
    assert!(here.size_bytes > 0);
}

/// A tiny ext4 image grows, a VM's disk grows with the config's
/// arithmetic, and a running VM is refused. Needs e2fsprogs.
#[test]
fn grow_resizes_the_image_and_refuses_a_running_vm() {
    if which("mkfs.ext4").is_none() || which("resize2fs").is_none() {
        eprintln!("skipped: e2fsprogs not installed");
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "ssf-grow-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("one")).unwrap();
    let block_count = |img: &Path| -> u64 {
        let out = Command::new("dumpe2fs")
            .arg("-h")
            .arg(img)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let field = |k: &str| -> u64 {
            text.lines()
                .find_map(|l| l.strip_prefix(k))
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        };
        field("Block count:") * field("Block size:")
    };
    // MiB scale, straight through the image helper.
    let img = dir.join("small.ext4");
    let f = std::fs::File::create(&img).unwrap();
    f.set_len(16 << 20).unwrap();
    drop(f);
    run_ok(
        Command::new("mkfs.ext4").args(["-q", "-F"]).arg(&img),
        "mkfs",
    )
    .unwrap();
    assert_eq!(block_count(&img), 16 << 20);
    grow_image(&img, 48 << 20).unwrap();
    assert_eq!(block_count(&img), 48 << 20);
    assert_eq!(std::fs::metadata(&img).unwrap().len(), 48 << 20);
    // Through the VM: a 1 GiB sparse data disk to 2 GiB.
    let mut cfg = Config::default();
    cfg.vm.dir = dir.to_string_lossy().to_string();
    cfg.vm.name = "one".into();
    cfg.vm.data_gib = Some(1);
    let vm = Vm::new(&cfg);
    let e = vm.grow(Some(2)).unwrap_err().to_string();
    assert!(e.contains("does not exist yet"), "{e}");
    let f = std::fs::File::create(vm.data_disk()).unwrap();
    f.set_len(1 << 30).unwrap();
    drop(f);
    run_ok(
        Command::new("mkfs.ext4")
            .args(["-q", "-F", "-L", "ssf-data"])
            .arg(vm.data_disk()),
        "mkfs",
    )
    .unwrap();
    assert_eq!(vm.data_cap_gib(), 1);
    assert!(vm.grow(Some(0)).is_err(), "shrinking is refused");
    assert_eq!(vm.grow(Some(1)).unwrap(), None, "same size: nothing to do");
    assert_eq!(vm.grow(Some(2)).unwrap(), Some(2));
    assert_eq!(vm.data_cap_gib(), 2);
    assert_eq!(block_count(&vm.data_disk()), 2 << 30);
    // Sparse: the file takes a little space, not 2 GiB.
    let blocks =
        std::os::unix::fs::MetadataExt::blocks(&std::fs::metadata(vm.data_disk()).unwrap());
    assert!(blocks * 512 < 1 << 30, "{blocks} blocks");
    // Running (a process whose name says firecracker): refused.
    let fake = dir.join("firecracker");
    std::fs::copy("/bin/sleep", &fake).unwrap();
    let mut child = Command::new(&fake).arg("60").spawn().unwrap();
    std::fs::write(vm.fc_pid(), child.id().to_string()).unwrap();
    // spawn may return before the child has exec'd its new name.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !vm.running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        vm.running(),
        "{:?}",
        std::fs::read(format!("/proc/{}/cmdline", child.id()))
    );
    let e = vm.grow(Some(4)).unwrap_err().to_string();
    assert!(e.contains("is running"), "{e}");
    assert_eq!(vm.data_cap_gib(), 2, "untouched");
    child.kill().unwrap();
    child.wait().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn backend_is_the_platform_default_unless_configured() {
    let vm = vm();
    assert_eq!(vm.backend(), BackendKind::platform_default());
    if std::env::consts::OS == "linux" {
        assert_eq!(vm.backend(), BackendKind::Firecracker);
    }
    let mut cfg = Config::default();
    cfg.vm.backend = Some(BackendKind::Lima);
    assert_eq!(Vm::new(&cfg).backend(), BackendKind::Lima);
    cfg.vm.backend = Some(BackendKind::Firecracker);
    assert_eq!(Vm::new(&cfg).backend(), BackendKind::Firecracker);
    // A build writes the platform default once; a hand-set value wins.
    let mut c = VmConfig::default();
    assert_eq!(
        choose_backend(&mut c, BackendKind::Lima),
        (BackendKind::Lima, "for this machine", true)
    );
    assert_eq!(c.backend, Some(BackendKind::Lima));
    assert_eq!(
        choose_backend(&mut c, BackendKind::Firecracker),
        (BackendKind::Lima, "set in config.toml", false)
    );
}

#[test]
fn guest_binary_comes_from_the_config_the_host_or_the_release() {
    assert_eq!(
        guest_binary_source("macos", "aarch64", Some("~/dl/ssf")),
        GuestBinary::Configured("~/dl/ssf".into())
    );
    assert_eq!(
        guest_binary_source("linux", "x86_64", Some("/opt/ssf")),
        GuestBinary::Configured("/opt/ssf".into())
    );
    assert_eq!(
        guest_binary_source("linux", "x86_64", None),
        GuestBinary::Own
    );
    assert_eq!(
        guest_binary_source("linux", "aarch64", None),
        GuestBinary::Own
    );
    assert_eq!(
        guest_binary_source("macos", "aarch64", None),
        GuestBinary::Download {
            asset: format!("ssf-{}-linux-aarch64", env!("CARGO_PKG_VERSION"))
        }
    );
    assert_eq!(
        guest_binary_source("macos", "x86_64", None),
        GuestBinary::Download {
            asset: format!("ssf-{}-linux-x86_64", env!("CARGO_PKG_VERSION"))
        }
    );
    // A configured path that is missing is an error naming the key.
    let mut cfg = Config::default();
    cfg.vm.guest_binary = Some("/nonexistent/ssf-linux".into());
    let e = Vm::new(&cfg).guest_binary().unwrap_err().to_string();
    assert!(e.contains("guest_binary"), "{e}");
    for server in ["/opt/ssf-server", "/tmp/ssf-server-0.4.0-linux-x86_64"] {
        assert_eq!(
            companion_server_path(Path::new(server)),
            PathBuf::from(server)
        );
    }
    assert_eq!(
        companion_server_path(Path::new("/opt/ssf")),
        PathBuf::from("/opt/ssf-server")
    );
    assert_eq!(
        companion_server_path(Path::new("/tmp/ssf-0.4.0-linux-x86_64")),
        PathBuf::from("/tmp/ssf-server-0.4.0-linux-x86_64")
    );
}

#[test]
fn pid_files_name_the_program() {
    assert!(pid_runs(std::process::id(), "ssf") || pid_runs(std::process::id(), "vm"));
    assert!(!pid_runs(std::process::id(), "firecracker"));
    assert!(!pid_runs(u32::MAX - 1, "firecracker"));
}

/// Isolated real-boot regression; see docs/development.md for required assets.
#[tokio::test]
#[ignore = "requires KVM and explicit legacy/current Firecracker assets"]
async fn firecracker_ownership_boot_persistence() {
    let sandbox = crate::config::test_support::sandbox();
    let asset = |name: &str| -> String {
        let path = std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
        std::fs::canonicalize(path)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    };
    let safe_root = asset("SSF_VM_TEST_ROOTFS");
    let legacy_root = asset("SSF_VM_TEST_LEGACY_ROOTFS");
    let mut cfg = Config::default();
    // No host factory config or credentials enter this isolated VM.
    cfg.vm.dir = sandbox.root().join("vms").to_string_lossy().into_owned();
    cfg.vm.name = "ownership-boot-test".into();
    cfg.vm.backend = Some(BackendKind::Firecracker);
    cfg.vm.rootfs = Some(safe_root.clone());
    cfg.vm.kernel = Some(asset("SSF_VM_TEST_KERNEL"));
    cfg.vm.firecracker = Some(asset("SSF_VM_TEST_FIRECRACKER"));
    cfg.vm.gvproxy = Some(asset("SSF_VM_TEST_GVPROXY"));
    cfg.vm.data_gib = Some(2);
    cfg.vm.mem_mib = Some(2048);
    cfg.vm.vcpus = Some(2);
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    cfg.vm.ssh_port = port.local_addr().unwrap().port();
    drop(port);
    let mut vm = Vm::new(&cfg);
    vm.binary = Some(PathBuf::from(asset("SSF_VM_TEST_BINARY")));
    let result: Result<()> = async {
            vm.start(&cfg).await?;
            vm.ssh_output(&["ssf", "config", "set", "daemon.poll_interval_secs", "71"])?;
            // Unreferenced fixtures exercise persistent credential/key storage
            // without authenticating or starting work against any GitHub account.
            vm.ssh_output(&["sh", "-c", "printf credential-sentinel > ~/.config/ssf/test-credential; printf key-sentinel > ~/.config/ssf/test-key"])?;
            let snapshot = || -> Result<String> {
                vm.ssh_output(&["cat", "/home/ssf/.config/ssf/config.toml", "/home/ssf/.config/ssf/test-credential", "/home/ssf/.config/ssf/test-key"])
            };
            let expected = snapshot()?;
            anyhow::ensure!(expected.contains("71"), "guest configuration change missing");
            let check = |vm: &Vm| -> Result<()> {
                anyhow::ensure!(vm.ssh_output(&["cat", "/usr/local/lib/ssf/seed-common.sh"])? == include_str!("../../../vm/guest/seed-common.sh").trim(), "booted seed script differs");
                anyhow::ensure!(vm.ssh_output(&["cat", "/home/ssf/.config/ssf/config.toml", "/home/ssf/.config/ssf/test-credential", "/home/ssf/.config/ssf/test-key"])? == expected, "guest factory state changed across boot");
                vm.ssh_output(&["test", "-f", "/home/ssf/.config/ssf/guest-owned"])?;
                Ok(())
            };
            check(&vm)?;
            vm.stop().await?;
            vm.start(&cfg).await?;
            check(&vm)?;
            vm.reset().await?;
            vm.cfg.rootfs = Some(legacy_root);
            let error = vm.start(&cfg).await.err().context("legacy root unexpectedly booted")?.to_string();
            anyhow::ensure!(error.contains("refusing to boot"), "{error}");
            anyhow::ensure!(!vm.running(), "legacy VM is running");
            // Recovery changes only the disposable root. Assert the guest's
            // script and established state after boot, not offline readback.
            vm.reset().await?;
            vm.cfg.rootfs = Some(safe_root);
            vm.start(&cfg).await?;
            check(&vm)?;
            Ok(())
        }.await;
    let stopped = vm.stop().await;
    if stopped.is_err() {
        eprintln!(
            "VM stop failed; preserving scratch disks at {}",
            sandbox.root().display()
        );
        std::mem::forget(sandbox);
    }
    stopped.unwrap();
    result.unwrap();
}

/// Against the built image: starts the VM, reaches the guest daemon
/// over ssh, stops it. Needs `ssf vm build` done and port 2299 free.
/// `cargo test vm_live -- --ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn vm_live() {
    let mut cfg = Config::default();
    cfg.vm.name = "live-test".into();
    cfg.vm.ssh_port = 2299;
    cfg.vm.data_gib = Some(2);
    cfg.github.login = Some("test-bot".into());
    // Seeding the guest resolves the bot token out of the config
    // directory, which the test build otherwise refuses (#140). This
    // test is run by hand against the factory installed here, so it
    // says so: whatever this machine's factory signs in as is what
    // the guest gets, a token file or the gh keyring behind it.
    let _machine = crate::config::test_support::the_machine_itself();
    let mut vm = Vm::new(&cfg);
    // Under `cargo test` this process is the test harness, not ssf.
    let exe = std::env::current_exe().unwrap();
    vm.binary = Some(exe.parent().unwrap().join("../ssf").canonicalize().unwrap());
    assert!(vm.rootfs().exists(), "no image; run `ssf vm build` first");
    vm.destroy().await.unwrap();
    vm.start(&cfg).await.unwrap();
    assert!(vm.running());
    let st = vm.status().await;
    eprintln!("status: {st:?}");
    assert!(st.ssh);
    let who = vm.ssh_output(&["id", "-un"]).unwrap();
    assert_eq!(who, GUEST_USER);
    // Root through sudo, no password, and the guest knows it is one.
    let root = vm.ssh_output(&["sudo", "-n", "id", "-u"]).unwrap();
    assert_eq!(root, "0");
    let guide = vm.ssh_output(&["ssf", "guide"]).unwrap();
    assert!(guide.contains(crate::prompt::VM_GUEST_LINE), "{guide}");
    let cfg_text = vm
        .ssh_output(&["cat", &format!("{GUEST_HOME}/.config/ssf/config.toml")])
        .unwrap();
    assert!(cfg_text.contains("driver = \"herdr\""));
    let herdr = vm.ssh_output(&["herdr", "status", "server"]).unwrap();
    assert!(herdr.contains("running"), "{herdr}");
    let status = vm
        .ssh_output(&[&format!("{GUEST_ENV}=1"), "ssf", "status", "--json"])
        .unwrap();
    let v: Value = serde_json::from_str(&status).unwrap();
    eprintln!("guest status: {v}");
    assert!(
        v.get("sessions").is_some() || v.get("repos").is_some(),
        "{v}"
    );
    let net = vm
        .ssh_output(&[
            "curl",
            "-fsS",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "https://api.github.com/",
        ])
        .unwrap();
    assert_eq!(net, "200");
    vm.stop().await.unwrap();
    assert!(!vm.running());
    vm.destroy().await.unwrap();
}

#[test]
fn guest_git_carries_keys_and_tokens_in_and_rewrites_the_paths() {
    let dir = std::env::temp_dir().join(format!(
        "ssf-guest-git-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let key = dir.join("id_ed25519");
    std::fs::write(&key, "private").unwrap();
    std::fs::write(crate::keys::public_path(&key), "public").unwrap();
    let other_key = dir.join("other").join("id_ed25519");
    std::fs::create_dir_all(other_key.parent().unwrap()).unwrap();
    std::fs::write(&other_key, "other private").unwrap();
    let token_file = dir.join("pat");
    std::fs::write(&token_file, "ghp_file\n").unwrap();
    let mut host = Config {
        git: GitConfig {
            name: Some("Ann".into()),
            email: Some("ann@example.com".into()),
            signing_key: Some(SigningKey::Path(key.to_string_lossy().to_string())),
            credential: Some("token:ann".into()),
        },
        ..Config::default()
    };
    host.repos.push(RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        git: GitConfig {
            signing_key: Some(SigningKey::Path(other_key.to_string_lossy().to_string())),
            credential: Some(format!("file:{}", token_file.display())),
            ..GitConfig::default()
        },
        ..RepoConfig::default()
    });
    host.repos.push(RepoConfig {
        name: "o/s".into(),
        harness: "claude".into(),
        git: GitConfig {
            signing_key: Some(SigningKey::Path(
                dir.join("missing").to_string_lossy().to_string(),
            )),
            credential: Some("!gh auth git-credential".into()),
            ..GitConfig::default()
        },
        ..RepoConfig::default()
    });
    let mut guest = guest_config(&host);
    let keys = dir.join("seed/keys");
    let tokens = dir.join("seed/tokens");
    assert!(
        guest_git(&host, &mut guest, &keys, &tokens, &|_| Ok(
            "ghp_keyring".into()
        ))
        .is_err()
    );
    host.repos[1].git.signing_key = Some(SigningKey::Off(false));
    guest = guest_config(&host);
    guest_git(&host, &mut guest, &keys, &tokens, &|login| {
        assert_eq!(login, "ann");
        Ok("ghp_keyring".to_string())
    })
    .unwrap();
    // Name and email go through untouched; the key and its .pub are copied.
    assert_eq!(guest.git.name.as_deref(), Some("Ann"));
    assert_eq!(
        guest.git.signing_key,
        Some(SigningKey::Path(format!("{GUEST_KEYS_DIR}/id_ed25519")))
    );
    assert_eq!(
        std::fs::read_to_string(keys.join("id_ed25519")).unwrap(),
        "private"
    );
    assert_eq!(
        std::fs::read_to_string(keys.join("id_ed25519.pub")).unwrap(),
        "public"
    );
    // The keyring token becomes a file the guest reads.
    assert_eq!(
        guest.git.credential.as_deref(),
        Some(format!("file:{GUEST_TOKENS_DIR}/ann").as_str())
    );
    assert_eq!(
        std::fs::read_to_string(tokens.join("ann")).unwrap(),
        "ghp_keyring\n"
    );
    // The repo's key shares a name with the instance one: numbered.
    assert_eq!(
        guest.repos[0].git.signing_key,
        Some(SigningKey::Path(format!("{GUEST_KEYS_DIR}/id_ed25519.2")))
    );
    assert_eq!(
        std::fs::read_to_string(keys.join("id_ed25519.2")).unwrap(),
        "other private"
    );
    assert_eq!(
        guest.repos[0].git.credential.as_deref(),
        Some(format!("file:{GUEST_TOKENS_DIR}/pat").as_str())
    );
    assert_eq!(
        std::fs::read_to_string(tokens.join("pat")).unwrap(),
        "ghp_file\n"
    );
    // Explicitly disabled signing and a helper string pass through.
    assert_eq!(guest.repos[1].git.signing_key, Some(SigningKey::Off(false)));
    assert_eq!(
        guest.repos[1].git.credential.as_deref(),
        Some("!gh auth git-credential")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
