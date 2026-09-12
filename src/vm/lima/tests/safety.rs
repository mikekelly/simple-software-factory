use super::*;

#[test]
fn a_copy_of_the_template_that_cannot_be_read_counts_as_stale() {
    // A file that cannot be read says nothing about what lima will do
    // with the disk, and "nothing" is not "clean". A directory where
    // the file should be is that case without a file mode, which a
    // test run as root would ignore.
    let t = Fake::new("Stopped");
    let yaml = t.instance_yaml();
    std::fs::remove_file(&yaml).unwrap();
    std::fs::create_dir(&yaml).unwrap();
    assert_eq!(
        t.vm.repair_stale_format(Some(&t.instance), Why::FoundStale),
        Some(yaml)
    );
}

#[test]
fn a_template_ssf_could_not_rewrite_stops_the_boot_as_well() {
    // The third way this used to fail open: the rewrite of ssf's own
    // template failed, the warning scrolled past, and the function
    // still returned "repaired". That file is what `ssf vm reset`
    // creates the next instance from.
    let t = Fake::new("Stopped");
    let path = t.vm.template_path();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(
        t.vm.repair_stale_format(None, Why::FoundStale),
        Some(path.clone())
    );
    let err = t.vm.stale_format_error(&path).to_string();
    assert!(err.contains("refusing to boot ssf-one"), "{err}");
    assert!(err.contains("could not rewrite"), "{err}");
    // The cure is not `limactl edit`: nothing here is lima's.
    assert!(!err.contains("limactl edit"), "{err}");
}

#[test]
fn only_force_over_a_disk_no_guest_was_ever_seen_on_deletes_it() {
    // The one path in ssf that destroys the factory's data disk, and
    // until now the only one with no test. A disk exists (the fake
    // answers `disk list`), so a plain build keeps it; `--force`
    // alone keeps it too, because a disk with no marker is one a
    // build saw a guest come up on. Only `--force` over the marker
    // deletes it -- and then the build makes a fresh one, marks that
    // one unproven in turn, and is the one build that may hand lima
    // `format: true`.
    for (force, marked, deleted) in [
        (false, false, false),
        (false, true, false),
        (true, false, false),
        (true, true, true),
    ] {
        let t = Fake::new("Stopped");
        if marked {
            t.vm.mark_disk_unproven();
        }
        let make_disk =
            t.vm.take_data_disk(force)
                .unwrap_or_else(|e| panic!("force={force} marked={marked}: {e:#}"));
        assert_eq!(make_disk, deleted, "force={force} marked={marked}");
        assert_eq!(
            t.ran("disk delete"),
            deleted,
            "force={force} marked={marked}: limactl ran {:?}",
            t.commands()
        );
        if deleted {
            assert!(
                t.commands().iter().any(|c| c == "disk delete ssf-one"),
                "{:?}",
                t.commands()
            );
            // The marker goes with the disk it pointed at; the fresh
            // disk gets its own.
            assert!(!t.vm.unproven_disk().exists());
            t.vm.create_data_disk().unwrap();
            assert!(
                t.commands()
                    .iter()
                    .any(|c| c.starts_with("disk create ssf-one --size")),
                "{:?}",
                t.commands()
            );
            assert!(t.vm.unproven_disk().exists());
        } else {
            // Nothing was destroyed, and the marker (where there was
            // one) still points at the same disk.
            assert_eq!(t.vm.unproven_disk().exists(), marked);
        }
    }
}

#[test]
fn a_finishing_build_states_a_fact_and_every_other_caller_reports_a_repair() {
    // The build that made the data disk is the one build allowed to
    // let lima format it, so that build turning the flag off at the
    // end is not a repair: every first build used to warn that a
    // build had not finished. The two readings differ only in what
    // they say, which is why the saying is a function.
    let finishing = format_off_note(Why::FinishingBuild, "ssf-one", "lima's copy");
    assert!(finishing.contains("carries the factory now"), "{finishing}");
    assert!(
        !finishing.contains("a build that did not finish"),
        "{finishing}"
    );
    let stale = format_off_note(Why::FoundStale, "ssf-one", "lima's copy");
    assert!(stale.contains("a build that did not finish"), "{stale}");
    assert!(stale.contains("putting `format: false` back"), "{stale}");

    // And it is only the wording: a finishing build repairs a stopped
    // instance in place ...
    let t = Fake::new("Stopped");
    assert_eq!(
        t.vm.repair_stale_format(Some(&t.instance), Why::FinishingBuild),
        None
    );
    assert!(
        t.commands()
            .iter()
            .any(|c| c == &format!("edit ssf-one --set {FORMAT_OFF}")),
        "{:?}",
        t.commands()
    );
    assert!(!says_format_true(
        &std::fs::read_to_string(t.instance_yaml()).unwrap()
    ));
    // ... and hands back what it could not repair just the same, so
    // `lima_first_boot` fails rather than leaving the flag behind.
    let t = Fake::with("Stopped", Edit::Ignored, DiskList::Answers);
    assert_eq!(
        t.vm.repair_stale_format(Some(&t.instance), Why::FinishingBuild),
        Some(t.instance_yaml())
    );
}

#[test]
fn a_reset_refuses_a_template_that_would_let_lima_format_the_disk() {
    // `ssf vm reset` creates the next instance from ssf's own
    // template, so a `format: true` still in it is a boot over a disk
    // that holds the factory -- the one thing this path exists to
    // stop. The repair's answer used to be dropped with a comment
    // saying nothing could come back; a rewrite that fails hands back
    // the template, and the reset now refuses on it.
    let t = Fake::new("Stopped");
    let path = t.vm.template_path();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let err = t
        .vm
        .lima_reset()
        .expect_err("a reset must not create an instance from a template that says format: true")
        .to_string();
    assert!(
        err.contains("still lets lima format the data disk"),
        "{err}"
    );
    assert!(err.contains("could not rewrite"), "{err}");
    // And it stopped before it touched the instance.
    assert!(!t.ran("delete") && !t.ran("create"), "{:?}", t.commands());
}

#[test]
fn the_console_names_the_instance_when_there_is_no_log_to_show() {
    // `lima_console_log` picked `serial.log` whenever `serialv.log`
    // was missing -- including when neither was there -- so
    // `ssf vm console` on an instance that has never booted ran
    // `tail` on a path that did not exist and showed `tail: cannot
    // open`, which reads as a broken command.
    let t = Fake::new("Stopped");
    let dir = PathBuf::from(&t.instance.dir);
    let err =
        t.vm.lima_console_log()
            .expect_err("no log has been written yet")
            .to_string();
    assert!(err.contains("ssf-one"), "{err}");
    assert!(err.contains("no console log yet"), "{err}");
    // Whichever lima wrote is the one that comes back.
    std::fs::write(dir.join("serialv.log"), "virtio\n").unwrap();
    assert_eq!(t.vm.lima_console_log().unwrap(), dir.join("serialv.log"));
    std::fs::write(dir.join("serial.log"), "serial\n").unwrap();
    assert_eq!(t.vm.lima_console_log().unwrap(), dir.join("serial.log"));
}

#[test]
fn a_cleanup_after_a_failed_build_does_not_talk_about_refusing_a_boot() {
    // `after_failed_build` and the look `ssf vm start` takes once the
    // guest is up both warn with this: no boot is being refused
    // there, and saying so sent people looking for a boot that had
    // not happened.
    let t = Fake::new("Stopped");
    let note = t.vm.stale_format_note(&t.instance_yaml());
    assert!(!note.contains("refusing to boot"), "{note}");
    assert!(
        note.contains("the next boot of ssf-one is refused"),
        "{note}"
    );
    assert!(note.contains(FORMAT_OFF), "{note}");
}

#[test]
fn a_disk_no_guest_ever_used_is_named_in_the_builds_own_error() {
    // A build that died before lima's boot script formatted the disk
    // leaves it blank, and no later build, start or reset formats a
    // disk it did not create. The way out belongs in the error the
    // build fails with -- the failure it actually shows is about ssh,
    // eight minutes later, and points nowhere near the disk.
    let t = Fake::new("Stopped");
    let untouched = format!(
        "{:#}",
        t.vm.blank_disk_hint(anyhow::anyhow!("the guest does not answer as ssf"))
    );
    assert_eq!(untouched, "the guest does not answer as ssf");
    t.vm.mark_disk_unproven();
    assert!(t.vm.unproven_disk().exists());
    let hinted = format!(
        "{:#}",
        t.vm.blank_disk_hint(anyhow::anyhow!("the guest does not answer as ssf"))
    );
    assert!(
        hinted.contains("the guest does not answer as ssf"),
        "{hinted}"
    );
    assert!(hinted.contains("ssf vm build --force"), "{hinted}");
    assert!(hinted.contains("ssf-one"), "{hinted}");
}

#[test]
fn a_liveness_probe_that_could_not_be_made_is_not_a_stopped_vm() {
    // The listing is a fork of a ~60 MB Go binary against a lima
    // home someone else may hold the lock on. When it fails, the one
    // thing that must not happen is the answer "the VM is not
    // running": the gate in front of every forwarded command refuses
    // on that, and the supervisor ends the daemon on it.
    let t = Fake::listing(Listing::Fails);
    let err =
        t.vm.running_now()
            .expect_err("a failed listing is not an answer");
    let text = format!("{err:#}");
    assert!(
        text.contains("asking lima whether ssf-one is running"),
        "{text}"
    );
    assert!(text.contains("failed to lock the lima home"), "{text}");
    assert_eq!(t.vm.running_state(), None);
    // And the lossy form is exactly what neither may use: it says
    // the instance is stopped, over an instance the fake reports as
    // Running.
    assert!(!t.vm.running());
    assert!(t.vm.running_now().is_err());
    // A listing that answers is unchanged by any of this.
    let up = Fake::listing(Listing::Answers);
    assert!(up.vm.running_now().unwrap());
    assert_eq!(up.vm.running_state(), Some(true));
}

#[tokio::test]
async fn status_json_keeps_an_unanswered_lima_probe_unknown() {
    let t = Fake::listing(Listing::Fails);
    let status = t.vm.status().await;

    assert_eq!(status.running, None);
    assert!(status.probe_error.is_some());
    let json = serde_json::to_value(status).unwrap();
    assert_eq!(json["running"], serde_json::Value::Null);
    assert!(
        json["probe_error"]
            .as_str()
            .is_some_and(|error| error.contains("failed to lock the lima home"))
    );
}

#[test]
fn a_liveness_probe_that_never_returns_is_cut_off_rather_than_waited_out() {
    // The gate asks this in front of every forwarded command, with a
    // person waiting on it, so a limactl that has stopped returning
    // is given a bound and its silence becomes "cannot tell" -- the
    // answer the gate is willing to carry on without. The bound
    // itself is LIVENESS_LIMIT; this holds the mechanism to a
    // fraction of it so the suite does not sit out fifteen seconds.
    let t = Fake::listing(Listing::Hangs);
    let started = Instant::now();
    let err =
        t.vm.lima_running_probe(Duration::from_millis(300))
            .expect_err("a listing that never returns is not an answer");
    let text = format!("{err:#}");
    assert!(text.contains("did not finish within"), "{text}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_stopped_or_missing_instance_ends_the_provisioning_wait() {
    // A probe that cannot run is usually transient (ssh in the guest
    // is not up yet). A stopped or deleted instance is not: nothing
    // is going to provision itself, and reading it as "nothing seen"
    // is what parked a wait for the whole PROVISION_TIMEOUT in
    // silence.
    assert_eq!(terminal_state(Some("Running")), None);
    let gone = terminal_state(None).unwrap();
    assert!(gone.contains("not there any more"), "{gone}");
    assert_eq!(
        terminal_state(Some("Stopped")).unwrap(),
        "is stopped, not running"
    );
    assert_eq!(
        terminal_state(Some("Broken")).unwrap(),
        "is broken, not running"
    );
}

#[test]
fn a_stale_format_true_is_recognised_in_either_template() {
    // What `repair_stale_format` reads: ssf's own template, and
    // lima's copy of it in the instance directory (a build that died
    // after `limactl create` leaves `format: true` in both).
    assert!(says_format_true(&vm().lima_template(true).unwrap()));
    assert!(!says_format_true(&vm().lima_template(false).unwrap()));
    // lima's copy indents and lists it as it pleases.
    assert!(says_format_true(
        "additionalDisks:\n- name: ssf-one\n  format: true\n  fsType: ext4\n"
    ));
    assert!(says_format_true("  - format: true\n"));
    assert!(!says_format_true("# format: true is what a build writes\n"));
    assert!(!says_format_true(""));
}

#[test]
fn the_probe_says_what_it_found_in_its_output_not_its_status() {
    // Every test in the probe fails in the ordinary case, so the script
    // must end by forcing a zero status; otherwise the call counts as
    // failed and its output is discarded.
    let probe = provision_probe();
    assert!(probe.trim_end().ends_with("exit 0"), "{probe}");
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(
            // Keep host processes from changing the probe's output: the
            // test's shell-local pgrep always reports no match.
            format!(
                "pgrep() {{ return 1; }}; {}",
                probe
                    .replace(PROVISION_MARKER, "/nonexistent/marker")
                    .replace(PROVISION_LOG, "/nonexistent/log")
            ),
        )
        .output()
        .expect("sh runs");
    assert!(
        out.status.success(),
        "a probe that finds nothing still exits 0"
    );
    assert_eq!(
        parse_probe(&String::from_utf8_lossy(&out.stdout)),
        Probe::default()
    );
}

#[test]
fn the_provision_probe_cannot_match_itself() {
    let probe = provision_probe();
    assert!(
        probe.contains(&format!("pgrep -f '{PROVISION_PGREP}'")),
        "{probe}"
    );
    // `pgrep -f` matches whole command lines, and `limactl shell
    // <name> sh -c "<probe>"` puts this string in one. The bracket
    // classes exist so that what pgrep looks for does not occur in
    // what it is looking through: with them gone, the probe would
    // always report "running" and a dead provisioning would never be
    // caught. Undo the brackets to get what pgrep matches, and hold
    // that neither is in the probe.
    for plain in [
        PROVISION_PGREP
            .replace("[l]", "l")
            .replace("[.]", ".")
            .split('|')
            .next()
            .unwrap()
            .to_string(),
        "provision.sh".to_string(),
    ] {
        assert!(!probe.contains(&plain), "{probe} contains {plain}");
    }
    assert_eq!(PROVISION_PGREP, "[l]ima-boot[.]sh|[p]rovision[.]sh");
}

#[test]
fn the_wait_ends_on_the_marker_and_on_three_idle_looks() {
    let seen = |out: &str| parse_probe(out);
    assert_eq!(
        seen("done\nlog\n"),
        Probe {
            done: true,
            log: true,
            running: false
        }
    );
    assert_eq!(seen(""), Probe::default());
    assert_eq!(
        seen("  log \n running \n"),
        Probe {
            done: false,
            log: true,
            running: true
        }
    );
    // The marker ends it whatever else the round saw.
    assert_eq!(
        provision_step(seen("done\nlog\nrunning\n"), 2),
        Step::Provisioned
    );
    // A log and nothing running: three rounds in a row, then failed.
    assert_eq!(provision_step(seen("log\n"), 0), Step::Wait(1));
    assert_eq!(provision_step(seen("log\n"), 1), Step::Wait(2));
    assert_eq!(provision_step(seen("log\n"), 2), Step::Failed);
    // A script running (or no log yet) puts the count back.
    assert_eq!(provision_step(seen("log\nrunning\n"), 2), Step::Wait(0));
    assert_eq!(provision_step(seen(""), 2), Step::Wait(0));
}

#[test]
fn lima_home_and_the_disk_directory_follow_lima() {
    let home = Path::new("/home/me");
    assert_eq!(
        lima_home_from(None, Some(home)),
        Some(PathBuf::from("/home/me/.lima"))
    );
    assert_eq!(
        disks_dir_from(None, Some(home)),
        Some(PathBuf::from("/home/me/.lima/_disks"))
    );
    // LIMA_HOME wins, and a tilde in it is expanded.
    assert_eq!(
        disks_dir_from(Some("/elsewhere/lima"), Some(home)),
        Some(PathBuf::from("/elsewhere/lima/_disks"))
    );
    assert_eq!(
        disks_dir_from(Some("  "), Some(home)),
        Some(PathBuf::from("/home/me/.lima/_disks"))
    );
    assert_eq!(disks_dir_from(None, None), None);
}

#[test]
fn limactl_json_lines_parse() {
    let text = r#"{"name":"ssf-default","status":"Running","dir":"/Users/me/.lima/ssf-default","vmType":"vz","arch":"aarch64","cpus":7,"memory":17179869184,"disk":21474836480,"sshLocalPort":2222,"sshAddress":"127.0.0.1","hostAgentPID":4242,"driverPID":4243}
{"name":"other","status":"Stopped","dir":"/Users/me/.lima/other","sshLocalPort":0}
not json at all
"#;
    let v = parse_instances(text);
    assert_eq!(v.len(), 2);
    assert_eq!(v[0].name, "ssf-default");
    assert!(v[0].is_running());
    assert_eq!(v[0].dir, "/Users/me/.lima/ssf-default");
    assert_eq!(v[0].ssh_local_port, 2222);
    assert!(!v[1].is_running());
    assert!(v.iter().all(|i| i.name != "missing"));
    assert!(parse_instances("").is_empty());
    let d = parse_disks(
        r#"{"name":"ssf-default","size":21474836480,"format":"qcow2","dir":"/Users/me/.lima/_disks/ssf-default","instance":"","instanceDir":"","mountPoint":"/mnt/lima-ssf-default"}
"#,
    );
    assert_eq!(d.len(), 1);
    assert_eq!(gib_ceil(d[0].size), 20);
    assert_eq!(d[0].mount_point, "/mnt/lima-ssf-default");
    assert!(parse_disks("").is_empty());
}

#[test]
fn grow_under_lima_plans_with_the_shared_rule() {
    // The same planner as Firecracker's, over lima's byte sizes.
    let current = gib_ceil(21474836480);
    assert_eq!(current, 20);
    assert_eq!(plan_grow(current, Some(40), 80).unwrap(), Some(40));
    assert_eq!(plan_grow(current, None, 80).unwrap(), Some(80));
    assert_eq!(plan_grow(current, Some(20), 80).unwrap(), None);
    assert!(plan_grow(current, Some(10), 80).is_err());
}

#[test]
fn copy_dir_copies_the_tree_with_modes() {
    let dir = std::env::temp_dir().join(format!(
        "ssf-copy-dir-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let from = dir.join("from");
    std::fs::create_dir_all(from.join("units")).unwrap();
    std::fs::write(from.join("lima-boot.sh"), "#!/bin/bash\n").unwrap();
    make_executable(&from.join("lima-boot.sh")).unwrap();
    std::fs::write(from.join("units/ssf.service"), "[Unit]\n").unwrap();
    let to = dir.join("to/guest");
    copy_dir(&from, &to).unwrap();
    assert_eq!(
        std::fs::read_to_string(to.join("units/ssf.service")).unwrap(),
        "[Unit]\n"
    );
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(to.join("lima-boot.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111, "{mode:o}");
    let _ = std::fs::remove_dir_all(&dir);
}
