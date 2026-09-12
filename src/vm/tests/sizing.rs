use super::*;

fn facts(cpus: u32, mem_mib: u64, free_gib: u64) -> HostFacts {
    HostFacts {
        cpus,
        mem_mib,
        free_bytes: free_gib << 30,
        mount: "/home".into(),
    }
}

#[test]
fn sizing_rule_follows_the_machine_down_to_the_floors() {
    // A desktop: 8 CPUs, 32 GiB, 500 GiB free.
    assert_eq!(
        sizes_for(&facts(8, 32768, 500)),
        Sizes {
            vcpus: 7,
            mem_mib: 16384,
            data_gib: 250
        }
    );
    // Half the RAM lands on a 256 MiB boundary.
    assert_eq!(sizes_for(&facts(4, 31922, 160)).mem_mib, 15872);
    assert_eq!(sizes_for(&facts(4, 31922, 160)).data_gib, 80);
    // A small machine never goes under the old fixed sizes.
    assert_eq!(sizes_for(&facts(2, 4096, 30)), Sizes::MIN);
    assert_eq!(sizes_for(&facts(1, 1024, 0)), Sizes::MIN);
    assert_eq!(sizes_for(&facts(3, 8192, 41)).vcpus, 2);
    assert_eq!(sizes_for(&facts(3, 8192, 41)).data_gib, 20);
    assert_eq!(sizes_for(&facts(3, 8192, 42)).data_gib, 21);
}

#[test]
fn build_writes_unset_sizes_and_keeps_hand_set_ones() {
    let rule = Sizes {
        vcpus: 7,
        mem_mib: 16384,
        data_gib: 250,
    };
    // Nothing set: everything from the machine, and the file changes.
    let mut cfg = VmConfig::default();
    let c = choose_sizes(&mut cfg, [None, None, None], rule);
    assert_eq!(c.sizes, rule);
    assert!(c.changed);
    assert_eq!(c.sources, ["from this machine"; 3]);
    assert_eq!(cfg.data_gib, Some(250));
    // Set by hand: kept, nothing to write.
    let mut cfg = VmConfig {
        vcpus: Some(2),
        mem_mib: Some(4096),
        data_gib: Some(20),
        ..VmConfig::default()
    };
    let c = choose_sizes(&mut cfg, [None, None, None], rule);
    assert_eq!(c.sizes, Sizes::MIN);
    assert!(!c.changed);
    assert_eq!(c.sources, ["set in config.toml"; 3]);
    // A flag beats the file and the machine, and is written.
    let c = choose_sizes(&mut cfg, [None, Some(8192), None], rule);
    assert_eq!(c.sizes.mem_mib, 8192);
    assert_eq!(c.sources[1], "from the flag");
    assert!(c.changed);
    assert_eq!(cfg.mem_mib, Some(8192));
    // The same flag again changes nothing.
    assert!(!choose_sizes(&mut cfg, [None, Some(8192), None], rule).changed);
    // The effective sizes follow the file where set.
    let mut whole = Config {
        vm: cfg.clone(),
        ..Config::default()
    };
    assert_eq!(Vm::new(&whole).sizes().mem_mib, 8192);
    whole.vm.vcpus = None;
    assert!(Vm::new(&whole).sizes().vcpus >= Sizes::MIN.vcpus);
}

#[test]
fn grow_plans_refuse_to_shrink_and_skip_the_same_size() {
    assert_eq!(plan_grow(20, Some(40), 80).unwrap(), Some(40));
    assert_eq!(plan_grow(20, None, 80).unwrap(), Some(80));
    assert_eq!(plan_grow(20, Some(20), 80).unwrap(), None);
    // The rule is below today: nothing to do, not an error.
    assert_eq!(plan_grow(100, None, 80).unwrap(), None);
    let e = plan_grow(20, Some(10), 80).unwrap_err().to_string();
    assert!(e.contains("shrink"), "{e}");
    assert!(plan_grow(20, Some(0), 80).is_err());
}

#[test]
fn sizing_measures_the_filesystem_the_disks_are_on() {
    let base = Path::new("/home/me/.local/share/ssf/vm");
    let disks = PathBuf::from("/other/.lima/_disks");
    // Firecracker's disks are files under `[vm] dir`.
    assert_eq!(
        sizing_dir_for(BackendKind::Firecracker, base, Some(disks.clone())),
        (base.to_path_buf(), "[vm] dir")
    );
    // lima's are in lima's home, which can be another volume: the
    // "half the free space" rule and grow's warning have to be about
    // that one.
    assert_eq!(
        sizing_dir_for(BackendKind::Lima, base, Some(disks.clone())),
        (disks, "lima's disk directory")
    );
    // No home, no lima directory: the VM's own is the honest answer.
    assert_eq!(
        sizing_dir_for(BackendKind::Lima, base, None),
        (base.to_path_buf(), "[vm] dir")
    );
}

#[test]
fn the_supervisor_polls_a_lima_vm_less_often_than_a_firecracker_one() {
    // Firecracker's "is it up?" is a PID file and a /proc lookup;
    // lima's forks a ~60 MB Go binary and takes a lock in lima's
    // home. Doing that every five seconds for as long as the factory
    // runs is more than the question is worth on a laptop, and every
    // fork is another chance to fail and be misread as "the VM
    // exited".
    assert_eq!(
        supervise_interval(BackendKind::Firecracker),
        Duration::from_secs(5)
    );
    assert!(supervise_interval(BackendKind::Lima) >= Duration::from_secs(30));
    assert!(supervise_interval(BackendKind::Lima) > supervise_interval(BackendKind::Firecracker));
    // Still well inside the wait a person would sit through before
    // asking what happened.
    assert!(supervise_interval(BackendKind::Lima) <= Duration::from_secs(60));
}

#[test]
fn a_probe_that_can_never_be_made_ends_the_supervision() {
    // A probe that could not be made says nothing, so one of them
    // must not end the supervision -- but the loop that only ever
    // warned left `ssf-server` "supervising" a VM it had not heard about
    // for hours, with the service reading active the whole time.
    const { assert!(MAX_UNANSWERED_PROBES > 1) };
    for backend in [BackendKind::Lima, BackendKind::Firecracker] {
        let every = supervise_interval(backend);
        // Long enough to sit out a busy laptop, short enough that the
        // daemon does not pretend all day.
        let gave_up_after = every * MAX_UNANSWERED_PROBES;
        assert!(gave_up_after >= Duration::from_secs(30), "{backend:?}");
        assert!(gave_up_after <= Duration::from_secs(15 * 60), "{backend:?}");
        let err = cannot_tell_error(backend, MAX_UNANSWERED_PROBES, every).to_string();
        assert!(
            err.contains("cannot tell whether the VM is running"),
            "{err}"
        );
        // It names the tool that stopped answering, so the next thing
        // to look at is not a guess.
        let tool = match backend {
            BackendKind::Lima => "limactl",
            BackendKind::Firecracker => "pid file",
        };
        assert!(err.contains(tool), "{err}");
    }
}

#[test]
fn a_firecracker_vm_answers_the_running_question_either_way() {
    // Under Firecracker the probe is a file read, so it always has an
    // answer -- `None` (the probe could not be made) is lima's case,
    // and it is what keeps a transient `limactl` failure from ending
    // the supervisor and the ssh wait with "the VM exited".
    let mut cfg = Config::default();
    cfg.vm.dir = std::env::temp_dir().to_string_lossy().into_owned();
    cfg.vm.name = "no-such-vm".into();
    cfg.vm.backend = Some(BackendKind::Firecracker);
    let vm = Vm::new(&cfg);
    assert_eq!(vm.running_state(), Some(false));
    assert!(!vm.running());
}

#[test]
fn the_doctor_line_names_the_backend_tooling_and_what_to_install() {
    // lima needs limactl, and qemu too on Linux (its only driver
    // there); a Mac runs the Virtualization framework instead.
    let mac = backend_tools(BackendKind::Lima, "macos", "aarch64", None, None);
    assert_eq!(mac.len(), 1);
    assert_eq!(mac[0].name, "limactl");
    // The floor is in what it says to install. That string is only
    // rendered for a tool that is missing, so it is what someone
    // with no lima at all is told to get -- the version of a lima
    // that *is* installed is `ssf vm build`'s to check, not this
    // line's.
    assert!(
        mac[0].install.contains(&lima::MIN_LIMA.to_string()),
        "{:?}",
        mac[0]
    );
    // ... unless the config asks for qemu, which is the driver that
    // has to be installed. Keyed on the OS alone, a Mac with
    // `[vm] vm_type = "qemu"` passed every check ssf makes and then
    // failed inside `limactl create`.
    let mac_qemu = backend_tools(BackendKind::Lima, "macos", "aarch64", None, Some("qemu"));
    assert_eq!(mac_qemu.len(), 2);
    assert_eq!(mac_qemu[1].name, "qemu-system-aarch64");
    assert!(
        mac_qemu[1].install.contains("brew install qemu"),
        "{:?}",
        mac_qemu[1]
    );
    assert!(mac_qemu[1].install.contains("vm_type"), "{:?}", mac_qemu[1]);
    // `vz` is the Virtualization framework: no qemu.
    assert_eq!(
        backend_tools(BackendKind::Lima, "macos", "aarch64", None, Some("vz")).len(),
        1
    );
    assert!(lima_uses_qemu("macos", Some("qemu")));
    assert!(!lima_uses_qemu("macos", None));
    assert!(!lima_uses_qemu("macos", Some("vz")));
    // qemu is lima's only Linux driver, whatever the config says.
    assert!(lima_uses_qemu("linux", None));
    assert!(lima_uses_qemu("linux", Some("qemu")));
    let linux = backend_tools(BackendKind::Lima, "linux", "x86_64", None, None);
    assert_eq!(linux.len(), 2);
    assert_eq!(linux[1].name, "qemu-system-x86_64");
    assert!(
        linux[1].install.contains("qemu-system-x86"),
        "{:?}",
        linux[1]
    );
    // `[vm] limactl` is what doctor looks for when it is set.
    let set = backend_tools(
        BackendKind::Lima,
        "macos",
        "aarch64",
        Some("/opt/l/limactl"),
        None,
    );
    assert_eq!(set[0].name, "/opt/l/limactl");
    // And a `~` in it is expanded, as `Vm::limactl` expands it when
    // it runs the thing: doctor and `ssf vm status` used to look
    // "~/bin/limactl" up on PATH -- which `which` only searches for a
    // bare name -- and report a limactl that works as not installed.
    if let Some(home) = dirs::home_dir() {
        let tilde = backend_tools(
            BackendKind::Lima,
            "macos",
            "aarch64",
            Some("~/bin/limactl"),
            None,
        );
        assert_eq!(
            tilde[0].name,
            home.join("bin/limactl").to_string_lossy().to_string()
        );
    }
    // A path is a path, whatever it starts with; a bare name is
    // looked up on PATH.
    assert_eq!(which("/nonexistent/limactl"), None);
    assert_eq!(which("./nonexistent-limactl"), None);
    assert!(which("sh").is_some());
    assert_eq!(which("definitely-not-a-program-on-this-path"), None);
    // Firecracker asks one question: may this user use KVM?
    let fc = backend_tools(BackendKind::Firecracker, "linux", "x86_64", None, None);
    assert_eq!(fc.len(), 1);
    assert!(fc[0].device);
    assert_eq!(fc[0].name, "/dev/kvm");
    assert!(fc[0].install.contains("usermod -aG kvm"), "{:?}", fc[0]);

    let found = vec![
        Some("/usr/bin/limactl".to_string()),
        Some("/usr/bin/qemu-system-x86_64".to_string()),
    ];
    let (ok, msg) = backend_tooling_line(&linux, &found);
    assert!(ok);
    assert_eq!(
        msg,
        "limactl at /usr/bin/limactl, qemu-system-x86_64 at /usr/bin/qemu-system-x86_64"
    );
    // Only what is missing is reported, with what to install.
    let (ok, msg) = backend_tooling_line(&linux, &[Some("/usr/bin/limactl".to_string()), None]);
    assert!(!ok);
    assert!(msg.starts_with("qemu-system-x86_64 not installed; install qemu"));
    assert!(!msg.contains("limactl at"), "{msg}");
    let (ok, msg) = backend_tooling_line(&fc, &[None]);
    assert!(!ok);
    assert!(msg.starts_with("/dev/kvm not usable by you; "));
    let (ok, msg) = backend_tooling_line(&fc, &[Some("/dev/kvm".into())]);
    assert!(ok);
    assert_eq!(msg, "/dev/kvm usable");
}
