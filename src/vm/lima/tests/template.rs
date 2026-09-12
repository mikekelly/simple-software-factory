use super::*;

#[test]
fn a_name_whose_disk_label_would_not_fit_is_refused() {
    assert_eq!(MAX_NAME_LEN, 7);
    assert!(check_name("default").is_ok());
    assert!(check_name("e2e").is_ok());
    let err = check_name("factory1").unwrap_err().to_string();
    assert!(err.contains("at most 7"), "{err}");
}

#[test]
fn names_and_files_follow_the_vm_name() {
    let vm = vm();
    assert_eq!(vm.lima_name(), "ssf-one");
    assert_eq!(vm.lima_disk_name(), "ssf-one");
    assert_eq!(vm.share_dir(), PathBuf::from("/v/one/share"));
    assert_eq!(vm.template_path(), PathBuf::from("/v/one/lima.yaml"));
    assert_eq!(
        lima_env("one"),
        "SSF_VM_DATA_DISK=ssf-one\nSSF_VM_NAME=one\n"
    );
    assert_eq!(lima_arch("x86_64").unwrap(), "x86_64");
    assert_eq!(lima_arch("aarch64").unwrap(), "aarch64");
    assert!(lima_arch("riscv64").is_err());
    assert_eq!(gib_ceil(1 << 30), 1);
    assert_eq!(gib_ceil((1 << 30) + 1), 2);
    assert_eq!(gib_ceil(0), 0);
}

#[test]
fn the_lima_version_is_read_out_of_what_limactl_prints() {
    // What a release prints.
    assert_eq!(
        parse_lima_version("limactl version 2.2.0\n"),
        Some(LimaVersion(2, 2, 0))
    );
    // A `v` prefix, and a build from git: newer than the release it
    // names, so the suffix is dropped rather than read as semver's
    // pre-release, which would make it older.
    assert_eq!(
        parse_lima_version("limactl version v2.2.0-15-g1234567"),
        Some(LimaVersion(2, 2, 0))
    );
    assert_eq!(
        parse_lima_version("limactl version 2.3.0-beta.0"),
        Some(LimaVersion(2, 3, 0))
    );
    // Two components are a version; a lima with none is not read as
    // one, so that preflight lets it through instead of refusing it.
    assert_eq!(
        parse_lima_version("limactl version 2.1"),
        Some(LimaVersion(2, 1, 0))
    );
    assert_eq!(parse_lima_version("limactl version <unknown>"), None);
    assert_eq!(parse_lima_version("limactl version HEAD"), None);
    assert_eq!(parse_lima_version(""), None);
}

/// What `lima_preflight` says with a `limactl` that prints
/// `line` for `--version` and answers nothing else.
fn preflight_against(line: &str) -> Result<()> {
    let dir = std::env::temp_dir().join(format!(
        "ssf-lima-ver-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let limactl = dir.join("limactl");
    std::fs::write(&limactl, format!("#!/bin/sh\nprintf '%s\\n' '{line}'\n")).unwrap();
    make_executable(&limactl).unwrap();
    let mut cfg = Config::default();
    cfg.vm.dir = dir.join("vm").to_string_lossy().into_owned();
    cfg.vm.name = "one".into();
    cfg.vm.backend = Some(BackendKind::Lima);
    cfg.vm.limactl = Some(limactl.to_string_lossy().into_owned());
    let out = Vm::new(&cfg).lima_preflight();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn a_lima_under_the_floor_is_refused_with_its_version_named() {
    // The version limactl was asked for anyway is read: an older
    // lima used to get all the way to a first boot and fail there,
    // over a base image it could not resolve or a share that was
    // mounted too late, saying neither.
    let e = format!(
        "{:#}",
        preflight_against("limactl version 1.2.1").unwrap_err()
    );
    assert!(e.contains("lima 1.2.1"), "{e}");
    assert!(e.contains("2.0.1 or newer"), "{e}");
    assert!(e.contains("Upgrade lima"), "{e}");
    // The version gate is passed on a new enough lima, and on one
    // whose version cannot be read at all -- a lima built without it
    // stamped in prints `<unknown>`, and refusing that would be
    // refusing a lima that was never asked about. Whatever preflight
    // goes on to say about qemu on this machine is not this gate's.
    for line in ["limactl version 2.2.0", "limactl version <unknown>"] {
        let after = preflight_against(line).err().map(|e| format!("{e:#}"));
        assert!(
            !after.as_deref().unwrap_or_default().contains("or newer"),
            "{after:?}"
        );
    }
}

#[test]
fn the_floor_is_the_oldest_lima_that_resolves_the_templates_base() {
    // 2.0.1, not 2.0.0: the opaque locator the template writes is
    // how lima 2.0 spells one, and 2.0.0's release tarball ships
    // `templates/_images/` empty. Held here so the constant and the
    // reason cannot drift apart.
    // The versions the floor was settled against by running them:
    // 1.2.1 and 2.0.0 fail `limactl template validate` on the
    // template ssf renders, 2.0.1 and 2.2.0 pass it.
    assert_eq!(MIN_LIMA.to_string(), "2.0.1");
    assert!(LimaVersion(1, 2, 1) < MIN_LIMA);
    assert!(LimaVersion(2, 0, 0) < MIN_LIMA);
    assert!(LimaVersion(2, 0, 1) >= MIN_LIMA);
    assert!(LimaVersion(2, 2, 0) >= MIN_LIMA);
    // Both bases are named in the opaque form the floor is chosen
    // for -- `template://...`, which every 1.x takes, would mean a
    // different floor.
    for arch in ["x86_64", "aarch64"] {
        let base = base_template(arch);
        assert!(
            base.starts_with("template:_images/"),
            "{base}: the base locator moved; MIN_LIMA is chosen for it"
        );
    }
}

#[test]
fn template_has_the_base_per_arch_the_sizes_the_mount_and_the_disk() {
    let vm = vm();
    let t = Template {
        disk: "ssf-one",
        share: Path::new("/v/one/share"),
        ssh_port: 2222,
        sizes: vm.sizes(),
        root_gib: 8,
        arch: "x86_64",
        image: None,
        vm_type: None,
        format_disk: true,
    };
    let y = render_template(&t);
    assert!(y.starts_with("# written by ssf;"), "{y}");
    assert!(y.contains("base:\n  - template:_images/archlinux\n"), "{y}");
    assert!(!y.contains("images:"), "{y}");
    assert!(!y.contains("vmType"), "{y}");
    // No `mountType` field: the type is lima's to pick (see
    // `render_template`). The word itself does occur, in the boot
    // hook's failure message, so this looks for the key rather than
    // the string.
    assert!(
        !y.lines().any(|l| l.trim_start().starts_with("mountType")),
        "{y}"
    );
    assert!(y.contains("arch: x86_64\n"), "{y}");
    assert!(y.contains("cpus: 3\n"), "{y}");
    assert!(y.contains("memory: \"8192MiB\"\n"), "{y}");
    // root_gib under the floor is lifted to it.
    assert!(y.contains("disk: \"20GiB\"\n"), "{y}");
    assert!(
            y.contains("mounts:\n  - location: \"/v/one/share\"\n    mountPoint: /mnt/ssf\n    writable: false\n"),
            "{y}"
        );
    assert!(y.contains("localPort: 2222\n"), "{y}");
    assert!(y.contains("loadDotSSHPubKeys: false"), "{y}");
    assert!(
        y.contains("additionalDisks:\n  - name: ssf-one\n    format: true\n    fsType: ext4\n"),
        "{y}"
    );
    assert!(
        y.contains("containerd:\n  system: false\n  user: false\n"),
        "{y}"
    );
    assert!(y.contains("  - mode: system\n"), "{y}");
    assert!(y.contains("      exec bash \"$boot\"\n"), "{y}");
    // aarch64 boots Ubuntu; a set image replaces the base; vmType
    // and a larger root pass through.
    let y = render_template(&Template {
        arch: "aarch64",
        image: Some("https://example.com/arch.qcow2"),
        vm_type: Some("vz"),
        root_gib: 30,
        ..t.clone()
    });
    assert!(y.contains("vmType: vz\n"), "{y}");
    assert!(!y.contains("base:"), "{y}");
    assert!(
        y.contains(
            "images:\n  - location: \"https://example.com/arch.qcow2\"\n    arch: aarch64\n"
        ),
        "{y}"
    );
    assert!(y.contains("disk: \"30GiB\"\n"), "{y}");
    assert!(
        render_template(&Template {
            arch: "aarch64",
            ..t.clone()
        })
        .contains("template:_images/ubuntu-lts"),
    );
    // Through the VM: its own share dir and port.
    let y = vm.lima_template(true).unwrap();
    assert!(y.contains("location: \"/v/one/share\""), "{y}");
    assert!(y.contains("name: ssf-one"), "{y}");
}

#[test]
fn only_the_build_that_makes_the_data_disk_lets_lima_format_it() {
    // lima's guest boot script formats an additional disk when it
    // cannot find the `lima-<disk>` label and `format` is true, then
    // mounts the partition by device either way. So the build that
    // creates the disk asks for a filesystem, and every template
    // after that (and the instance's own copy, through
    // `limactl edit --set`) says false: a disk that has lost its
    // label must then fail to mount rather than be reformatted.
    let vm = vm();
    let made = vm.lima_template(true).unwrap();
    assert!(
        made.contains("additionalDisks:\n  - name: ssf-one\n    format: true\n    fsType: ext4\n"),
        "{made}"
    );
    let kept = vm.lima_template(false).unwrap();
    assert!(
        kept.contains("additionalDisks:\n  - name: ssf-one\n    format: false\n    fsType: ext4\n"),
        "{kept}"
    );
    // Everything else about the two is the same.
    assert_eq!(made.replace("format: true", "format: false"), kept);
    // What flips the instance's own copy (yq syntax; `limactl help
    // yq-restrictions`).
    assert_eq!(FORMAT_OFF, ".additionalDisks[0].format = false");
}
