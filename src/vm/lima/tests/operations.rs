use super::*;

#[test]
fn a_command_that_does_not_return_is_killed_at_its_limit() {
    // The reason every limactl call is bounded: an `ssf vm build`
    // once sat for a quarter of an hour with nothing to show and no
    // child process to look at. A command that sleeps stands in for
    // the limactl that never returned.
    let mut child = Command::new("sh")
        .args(["-c", "sleep 60"])
        .stdin(Stdio::null())
        .spawn()
        .expect("spawning sh");
    let started = Instant::now();
    let err = wait_within(
        &mut child,
        "limactl start ssf-one",
        Duration::from_millis(300),
    )
    .unwrap_err()
    .to_string();
    // It gave up at the limit rather than waiting for the sleep...
    assert!(started.elapsed() < Duration::from_secs(10), "{err}");
    // ...the message says which command and how long it was given...
    assert!(err.contains("`limactl start ssf-one`"), "{err}");
    assert!(err.contains("300 ms"), "{err}");
    // ...and the child is gone, not left behind still running.
    assert!(child.try_wait().unwrap().is_some(), "the child outlived it");

    // A command that does return does so with its own status, and the
    // limit is not waited out.
    let mut quick = Command::new("sh")
        .args(["-c", "exit 3"])
        .stdin(Stdio::null())
        .spawn()
        .expect("spawning sh");
    let started = Instant::now();
    let st = wait_within(&mut quick, "limactl list --json", Duration::from_secs(60)).unwrap();
    assert_eq!(st.code(), Some(3));
    assert!(started.elapsed() < Duration::from_secs(10));

    assert_eq!(human_duration(Duration::from_millis(300)), "300 ms");
    assert_eq!(human_duration(Duration::from_secs(1)), "1 second");
    assert_eq!(human_duration(PROBE_LIMIT), "1 minute");
    assert_eq!(human_duration(QUICK_LIMIT), "2 minutes");
    assert_eq!(
        human_duration(Duration::from_secs(90)),
        "1 minute 30 seconds"
    );
}

#[test]
fn every_limit_leaves_room_for_the_slow_commands() {
    // ssf's bound around `limactl start` is lima's own `--timeout`
    // plus a margin, so that lima times the boot out first and its
    // message is what a person reads; ssf's is the backstop for a
    // limactl that does not return at all.
    assert_eq!(start_timeout_arg(), "30m");
    assert_eq!(PROVISION_TIMEOUT, Duration::from_secs(30 * 60));
    // The guest provisions inside `limactl start`, so lima is given
    // the whole provisioning allowance and ssf's bound around the
    // call is that plus the margin -- neither can cut a slow but
    // healthy provisioning short.
    assert!(PROVISION_TIMEOUT + OWN_TIMEOUT_MARGIN > PROVISION_TIMEOUT);
    // A `limactl create` may download the base image; a probe over
    // `limactl shell` is a one-liner and must not hold up the loop.
    assert!(CREATE_LIMIT > QUICK_LIMIT);
    assert!(PROBE_LIMIT < QUICK_LIMIT);
    assert!(STOP_LIMIT > QUICK_LIMIT);
    // The gate's liveness question is the shortest of all: it is
    // asked in front of every forwarded command, so its bound is
    // what a person waits out when limactl has stopped answering,
    // and being cut short only costs the gate an answer it is
    // willing to do without. The supervisor's polling of the same
    // question keeps the listing's own bound, since it gives up
    // after MAX_UNANSWERED_PROBES rounds with no answer.
    assert!(LIVENESS_LIMIT < QUICK_LIMIT);
    // The survey asks two of these in a row with a person waiting on
    // the report, and has a filesystem fallback when neither
    // answers.
    assert!(SURVEY_LIMIT < QUICK_LIMIT);
}

#[test]
fn the_templates_boot_hook_waits_for_the_share_itself() {
    // The hook is the only part of the boot that is not in the share,
    // so it is the only part that can report the share missing. If it
    // went back to a bare `exec`, a guest that never got its mount
    // would write no log at all and the host would wait out
    // PROVISION_TIMEOUT with nothing to show.
    let hook = boot_hook();
    assert!(hook.starts_with("#!/bin/bash\n"), "{hook}");
    assert!(hook.contains(&format!("log={PROVISION_LOG}")), "{hook}");
    assert!(
        hook.contains(&format!("boot={GUEST_MOUNT}/guest/lima-boot.sh")),
        "{hook}"
    );
    assert!(hook.contains(&format!("i < {SHARE_WAIT_SECS}")), "{hook}");
    // And what it says when the wait runs out names the one thing the
    // version floor cannot settle: a `mountType` in lima's own
    // `_config` that mounts the share only once the guest is up.
    assert!(hook.contains("mountType"), "{hook}");
    assert!(hook.contains("_config/override.yaml"), "{hook}");
    // lima's home is not always `~/.lima`, and the message is read
    // by someone looking for a file.
    assert!(hook.contains("$LIMA_HOME"), "{hook}");
    // The log is emptied before that wait, not after it and not in
    // lima-boot.sh: the host reads "a log and none of the guest
    // scripts running" as this attempt having died, and while the
    // hook waited for the share the log on disk was the previous
    // attempt's, so a probe landing there printed the wrong tail.
    let empties = hook
        .lines()
        .position(|l| l.trim() == ": > \"$log\"")
        .unwrap_or_else(|| panic!("{hook}"));
    let waits = hook
        .lines()
        .position(|l| l.contains(&format!("i < {SHARE_WAIT_SECS}")))
        .expect("the wait");
    assert!(empties < waits, "{hook}");
    assert!(
        !std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("vm/guest/lima-boot.sh")
        )
        .expect("the guest script")
        .contains(": > \"$log\""),
        "lima-boot.sh must not truncate the log as well: it runs after the wait"
    );
    // The failure goes to the log the host watches, not only to
    // lima's output, and it is a failure (`exit 1`), not a fall
    // through into an `exec` that cannot work.
    let fail = hook
        .lines()
        .find(|l| l.contains("FAILED"))
        .unwrap_or_else(|| panic!("{hook}"));
    assert!(fail.contains("tee -a \"$log\""), "{fail}");
    assert!(hook.contains("    exit 1\n"), "{hook}");
    assert!(hook.trim_end().ends_with("exec bash \"$boot\""), "{hook}");
    // It has to be shell that runs: the guest gets it as written.
    let out = std::process::Command::new("bash")
        .args(["-n", "-c", &hook])
        .output()
        .expect("bash runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // And the template indents every line of it under `script: |`.
    let y = vm().lima_template(true).unwrap();
    for line in hook.lines().filter(|l| !l.is_empty()) {
        assert!(y.contains(&format!("\n      {line}\n")), "{line} in {y}");
    }
}

#[test]
fn the_ssh_wait_outlasts_the_seeds_own_waits() {
    // The marker the provisioning wait ends on is written before
    // `ssf-seed.service` is queued, so what the ssh wait after it is
    // really waiting for is the seed -- which waits for the share and
    // for the data disk before it writes authorized_keys. Read those
    // waits out of the script itself, so that lengthening one of them
    // and leaving this alone fails here rather than in the field with
    // "provisioned, but the guest does not answer as ssf".
    let script = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("vm/guest/seed-lima.sh"),
    )
    .expect("vm/guest/seed-lima.sh");
    let waits: u64 = script
        .lines()
        .filter_map(|l| l.trim().strip_prefix("wait_for "))
        .filter_map(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
        .sum();
    assert!(waits >= 240, "the seed's waits are {waits}s: {script}");
    assert!(
        SEED_TIMEOUT > Duration::from_secs(waits),
        "SEED_TIMEOUT is {}, the seed may wait {waits}s before it writes authorized_keys",
        human_duration(SEED_TIMEOUT)
    );
    // With room left for the home copy that follows them.
    assert!(SEED_TIMEOUT - Duration::from_secs(waits) >= Duration::from_secs(120));
}

#[test]
fn a_running_instance_is_never_edited_in_place() {
    // `limactl edit` refuses a running instance (lima 2.2.0: "cannot
    // edit a running instance"), so a repair that ran it anyway got
    // an error, warned, and left the instance booting with
    // `format: true` over a disk that by then held the factory. The
    // rule is: ssf's own template is always rewritten, the instance's
    // copy only while it is stopped, and a running one is reported.
    assert_eq!(
        plan_repair(true, true, true, true),
        Repair {
            template: true,
            instance: false,
            blocked: true
        }
    );
    assert_eq!(
        plan_repair(true, true, true, false),
        Repair {
            template: true,
            instance: true,
            blocked: false
        }
    );
    // Only lima's copy stale (a build rewrote ssf's own and then died).
    assert_eq!(
        plan_repair(true, false, true, false),
        Repair {
            template: false,
            instance: true,
            blocked: false
        }
    );
    // Only ssf's own stale: nothing to edit, and nothing blocked.
    assert_eq!(
        plan_repair(true, true, false, true),
        Repair {
            template: true,
            instance: false,
            blocked: false
        }
    );
    // Nothing stale, and -- whatever the templates say -- no disk
    // means nothing to protect: the build that makes the disk is the
    // one build that may hand lima `format: true`.
    assert_eq!(plan_repair(true, false, false, false), Repair::default());
    assert_eq!(plan_repair(false, true, true, false), Repair::default());
    assert_eq!(plan_repair(false, true, true, true), Repair::default());
}

#[test]
fn the_repair_edits_a_stopped_instance_and_reports_a_running_one() {
    // The same rules through `limactl` itself: a fake one records
    // what it was asked to do.
    for (status, edited) in [("Stopped", true), ("Running", false)] {
        let t = Fake::new(status);
        let stale = t.vm.repair_stale_format(Some(&t.instance), Why::FoundStale);
        // ssf's own template is a file: it is put right either way.
        assert!(
            !says_format_true(&std::fs::read_to_string(t.vm.template_path()).unwrap()),
            "{status}"
        );
        assert_eq!(
            t.ran("edit"),
            edited,
            "{status}: limactl ran {:?}",
            t.commands()
        );
        if edited {
            assert!(
                t.commands()
                    .iter()
                    .any(|c| c == &format!("edit ssf-one --set {FORMAT_OFF}")),
                "{:?}",
                t.commands()
            );
            // Repaired: nothing for the caller to refuse over.
            assert_eq!(stale, None);
        } else {
            // Not repaired, and said so: lima's copy of the template
            // comes back, and every caller that is about to boot the
            // instance stops on it.
            assert_eq!(stale, Some(t.instance_yaml()));
            let err = t.vm.stale_format_error(&stale.unwrap()).to_string();
            assert!(err.contains("refusing to boot ssf-one"), "{err}");
            assert!(err.contains("ssf vm stop"), "{err}");
            assert!(err.contains(FORMAT_OFF), "{err}");
        }
    }
}

#[tokio::test]
async fn legacy_lima_root_is_refused_before_boot() {
    let _sandbox = crate::config::test_support::sandbox();
    let t = Fake::new("Stopped");
    let inst = t.vm.lima_instance().unwrap().unwrap();
    std::fs::remove_file(Path::new(&inst.dir).join("ssf-safe-root-v2")).unwrap();
    let error =
        t.vm.lima_start(&Config::default())
            .await
            .unwrap_err()
            .to_string();
    assert!(error.contains("ssf vm reset"), "{error}");
    assert!(!t.ran("start"), "{:?}", t.commands());
    std::fs::write(Path::new(&inst.dir).join("ssf-fresh-root-v2"), "1").unwrap();
    require_safe_root(Path::new(&inst.dir)).unwrap();
}

#[tokio::test]
async fn a_start_repairs_the_flag_before_the_boot_and_refuses_what_it_cannot_repair() {
    // The start writes the share tree, which seeds the guest from the
    // bot token and so resolves the config directory (#140).
    let _sandbox = crate::config::test_support::sandbox();
    // `ssf vm start` is the boot that a `format: true` left by a
    // failed build would reach, and by then the data disk holds the
    // factory. A stopped instance is repaired in place and the start
    // goes on (as far as this fake takes it); a running one cannot be
    // edited, and the start stops rather than leaving lima free to
    // reformat the disk.
    let t = Fake::new("Running");
    let err =
        t.vm.lima_start(&Config::default())
            .await
            .expect_err("a start over a stale format flag must not go ahead")
            .to_string();
    assert!(
        err.contains("still lets lima format the data disk"),
        "{err}"
    );
    assert!(err.contains("refusing to boot ssf-one"), "{err}");
    // It stopped before it did anything to the instance.
    assert!(!t.ran("start") && !t.ran("edit"), "{:?}", t.commands());

    // Stopped: the flag is turned off first (`limactl edit` takes a
    // stopped instance) and the start goes on to boot it. This fake's
    // instance never comes up, which is the other fix in the same
    // path: a probe that cannot run against an instance lima says is
    // stopped ends the wait then and there, instead of leaving it
    // parked for PROVISION_TIMEOUT with nothing on screen.
    let t = Fake::new("Stopped");
    let started = Instant::now();
    let err =
        t.vm.lima_start(&Config::default())
            .await
            .expect_err("this instance never provisions itself")
            .to_string();
    assert!(t.ran("edit"), "{:?}", t.commands());
    assert!(
        t.commands()
            .iter()
            .any(|c| c == &format!("edit ssf-one --set {FORMAT_OFF}")),
        "{:?}",
        t.commands()
    );
    assert!(err.contains("is stopped, not running"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "the wait sat for {:?}",
        started.elapsed()
    );
}

#[test]
fn an_edit_limactl_called_a_success_is_read_back_before_it_is_believed() {
    // `limactl edit --set` exiting 0 is limactl's word that its own
    // `--set` ran, not that the flag is off: a lima whose restricted
    // yq matched nothing (a schema that moved the key, a differently
    // shaped `additionalDisks`) would exit 0 over an unchanged file,
    // and the whole "no boot over a flag that is still there"
    // guarantee would rest on that exit status alone.
    let t = Fake::with("Stopped", Edit::Ignored, DiskList::Answers);
    let stale = t.vm.repair_stale_format(Some(&t.instance), Why::FoundStale);
    assert!(t.ran("edit"), "{:?}", t.commands());
    assert!(
        says_format_true(&std::fs::read_to_string(t.instance_yaml()).unwrap()),
        "the fake was supposed to leave the file alone"
    );
    assert_eq!(stale, Some(t.instance_yaml()));
    let err = t.vm.stale_format_error(&stale.unwrap()).to_string();
    assert!(err.contains("refusing to boot ssf-one"), "{err}");
}

#[tokio::test]
async fn a_start_refuses_when_the_edit_did_not_take() {
    // The same thing from the caller's end: nothing is booted over a
    // repair that only limactl's exit status said had happened.
    let t = Fake::with("Stopped", Edit::Ignored, DiskList::Answers);
    let err =
        t.vm.lima_start(&Config::default())
            .await
            .expect_err("an edit that changed nothing is not a repair")
            .to_string();
    assert!(err.contains("refusing to boot ssf-one"), "{err}");
    assert!(!t.ran("start"), "{:?}", t.commands());
}

#[test]
fn a_disk_probe_that_failed_is_not_an_answer() {
    // `limactl disk list` erroring used to be read as "there is no
    // data disk", which is the one answer that makes a stale
    // `format: true` harmless -- so a lima that could not be asked
    // let the boot through. The probe that did not run now counts as
    // "the disk is there": the cost is a refused boot and a message.
    let t = Fake::with("Running", Edit::Applies, DiskList::Fails);
    assert_eq!(
        t.vm.repair_stale_format(Some(&t.instance), Why::FoundStale),
        Some(t.instance_yaml())
    );
}

#[test]
fn lima_answers_whether_there_is_a_vm_here_the_directory_does_not() {
    // `ssf uninstall` used to read "is there a VM" off `[vm]
    // dir`/<name>. Under Firecracker that directory *is* the VM;
    // under lima it holds the template, the ssh key and the share,
    // and the instance and the data disk are in lima's home. A
    // directory removed by hand, or a `[vm] dir` that was changed,
    // then read as "no VM" and left both behind.
    let t = Fake::new("Stopped");
    std::fs::remove_dir_all(&t.vm.dir).unwrap();
    assert!(!t.vm.dir.exists());
    assert_eq!(
        t.vm.survey(),
        Survey {
            present: Some(true),
            running: Some(false),
            startable: true,
            data: Some(true),
        }
    );
    let t = Fake::new("Running");
    assert_eq!(t.vm.survey().running, Some(true));
}

#[test]
fn a_data_disk_that_outlived_its_instance_is_a_vm_with_nothing_to_start() {
    // `limactl delete` of an instance leaves an external disk where
    // it is, so the disk holding the clones and worktrees can be all
    // that is left. It is worth not losing silently -- and nothing
    // can be started to mount it, so `ssf vm start` is no remedy.
    let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Answers, Listing::Empty);
    std::fs::remove_dir_all(&t.vm.dir).unwrap();
    assert_eq!(
        t.vm.survey(),
        Survey {
            present: Some(true),
            running: Some(false),
            startable: false,
            data: Some(true),
        }
    );
}

#[test]
fn a_lima_home_nobody_could_look_in_is_not_a_lima_home_with_nothing_in_it() {
    // Where `limactl` will not answer, lima's own filesystem is
    // what is left to read, and a denied `stat` on a root-owned
    // lima home answered "nothing here". Both fallbacks, because
    // they are different code: the disk listing failing alone drops
    // into an `Err` arm inside `lima_survey`, while the instance
    // listing failing takes the whole survey to `lima_unanswered`.
    use std::os::unix::fs::PermissionsExt;
    for listing in [Listing::Answers, Listing::Fails] {
        let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Fails, listing);
        std::fs::remove_dir_all(&t.vm.dir).unwrap();
        let home = t.vm.lima_home.clone().unwrap();
        std::fs::create_dir_all(home.join("_disks").join(t.vm.lima_disk_name())).unwrap();
        // Readable first, so what follows is about the denial and
        // not about the fixture.
        assert_eq!(t.vm.survey().data, Some(true), "{listing:?}");

        let mut perm = std::fs::metadata(&home).unwrap().permissions();
        perm.set_mode(0o000);
        std::fs::set_permissions(&home, perm).unwrap();
        let seen = std::fs::metadata(home.join("_disks").join(t.vm.lima_disk_name()))
            .map_err(|e| e.kind());
        // While the mode is still 0o000: after the restore below
        // this is true for everyone, and says nothing.
        let readable_anyway = std::fs::read_dir(&home).is_ok();
        let s = t.vm.survey();
        // Restored before the assertions, so a failure still leaves
        // the fixture's `Drop` able to remove it.
        let mut perm = std::fs::metadata(&home).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&home, perm).unwrap();

        match seen {
            Err(std::io::ErrorKind::NotFound) => {
                panic!("the fixture is wrong: the disk directory is there")
            }
            Err(_) => assert_eq!(
                s.data, None,
                "a denied stat is a question that was never answered ({listing:?})"
            ),
            // euid 0 ignores the mode, so this case cannot be
            // built here and this run proves nothing about it. The
            // assertion is the narrow one that is true: the home
            // really was readable.
            Ok(_) => assert!(
                readable_anyway,
                "a stat succeeded through a directory nothing should have been able to read"
            ),
        }
        // The instance listing failing takes `present` with it:
        // nothing was established about the VM either, and
        // `Some(false)` there would send the destroy step down the
        // "no VM" arm over a machine nobody could look at.
        if listing == Listing::Fails && seen.is_err() {
            assert_eq!(s.present, None, "and the same for the VM itself");
        }
    }
}

#[test]
fn nothing_in_lima_and_no_directory_is_no_vm() {
    let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Empty, Listing::Empty);
    std::fs::remove_dir_all(&t.vm.dir).unwrap();
    assert_eq!(t.vm.survey().present, Some(false));
    // The directory on its own is still ssf's to remove -- the
    // template, the ssh key and the share are in it -- but none of
    // that is anyone's work, so it is no reason to refuse.
    std::fs::create_dir_all(&t.vm.dir).unwrap();
    let s = t.vm.survey();
    assert_eq!((s.present, s.data), (Some(true), Some(false)));
}

#[test]
fn a_lima_that_will_not_answer_falls_back_to_limas_own_filesystem() {
    // `limactl` moved by an upgrade, off the PATH the service runs
    // under, a stale `[vm] limactl`, a locked lima home: evidence
    // about the tool, not about the machine. What settles it is
    // whether `<lima home>/<name>` or `<lima home>/_disks/<disk>` is
    // on disk -- so a missing binary can never be the reason a disk
    // full of workspaces is treated as absent, and a machine with
    // nothing of lima's on it still gets a clean `no VM`.
    let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Empty, Listing::Fails);
    std::fs::remove_dir_all(&t.vm.dir).unwrap();
    let home = t.vm.lima_home.clone().unwrap();
    // The fake's instance directory is there.
    let s = t.vm.survey();
    assert_eq!(
        (s.present, s.running, s.startable, s.data),
        (Some(true), None, false, Some(false))
    );
    // ... and so is a data disk, which is the half that matters.
    std::fs::create_dir_all(home.join("_disks").join("ssf-one")).unwrap();
    assert_eq!(t.vm.survey().data, Some(true));
    // Nothing of lima's anywhere: a real "no VM", not a refusal.
    std::fs::remove_dir_all(&home).unwrap();
    assert_eq!(
        t.vm.survey(),
        Survey {
            present: Some(false),
            running: Some(false),
            startable: false,
            data: Some(false),
        }
    );
}

#[test]
fn a_disk_probe_that_failed_leaves_the_instances_answer_standing() {
    // Only the disk question went unanswered: lima still said there
    // is no instance, so nothing is running and nothing can start.
    // The disk falls back to lima's own filesystem.
    let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Fails, Listing::Empty);
    std::fs::remove_dir_all(&t.vm.dir).unwrap();
    assert_eq!(
        t.vm.survey(),
        Survey {
            present: Some(false),
            running: Some(false),
            startable: false,
            data: Some(false),
        }
    );
    // The direction that matters: the disk is on disk, so it is
    // there to be destroyed and there to stop the destroying.
    let home = t.vm.lima_home.clone().unwrap();
    std::fs::create_dir_all(home.join("_disks").join("ssf-one")).unwrap();
    assert_eq!(
        t.vm.survey(),
        Survey {
            present: Some(true),
            running: Some(false),
            startable: false,
            data: Some(true),
        }
    );
}

#[test]
fn a_disk_that_could_not_be_deleted_fails_the_step_with_the_instance_gone() {
    // The destroy is two deletions and the second can fail on its
    // own. It must stay a failure: `ssf uninstall` only writes
    // `[vm] enabled = false` when the destroy succeeded, and a
    // swallowed error here would turn the next run into a host-mode
    // one over a data disk that is still there.
    let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Fails, Listing::Answers);
    let home = t.vm.lima_home.clone().unwrap();
    std::fs::create_dir_all(home.join("_disks").join("ssf-one")).unwrap();
    let err = t.vm.lima_destroy().unwrap_err().to_string();
    assert!(err.contains("may hold it"), "{err}");
    assert!(t.ran("delete -f ssf-one"), "{:?}", t.commands());
    // With nothing of the disk in lima's home there is nothing to
    // delete, and a limactl that would not say so is no reason to
    // fail the step.
    std::fs::remove_dir_all(home.join("_disks")).unwrap();
    assert!(t.vm.lima_destroy().is_ok());
}

#[tokio::test]
async fn the_vm_directory_goes_even_when_limas_half_of_the_destroy_failed() {
    // `[vm] dir` is ssf's own -- the generated template, the ssh key
    // and the share -- and lima not answering is no reason to leave
    // it behind for the next run to trip over. The failure is still
    // the step's failure, and it has to be, because `ssf uninstall`
    // only writes `[vm] enabled = false` once the destroy succeeded.
    let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Fails, Listing::Fails);
    assert!(t.vm.dir.exists());
    let err = t.vm.destroy().await.unwrap_err().to_string();
    assert!(err.contains("still holds something of ssf-one"), "{err}");
    assert!(!t.vm.dir.exists(), "the directory is ssf's own and goes");
    // Nothing of lima's and no directory: a destroy with nothing to
    // do is not a failure. What it prints is not pinned here --
    // stdout is awkward to capture from a test -- so this asserts
    // only what it can.
    std::fs::remove_dir_all(t.vm.lima_home.clone().unwrap()).unwrap();
    assert!(t.vm.destroy().await.is_ok());
}

#[test]
fn a_limactl_that_cannot_run_does_not_fail_a_destroy_over_nothing() {
    // `ssf uninstall` on a machine with no lima left: the destroy
    // step must not end in a failed step over a binary that is not
    // there any more. With something of lima's still in its home it
    // must, because there is then a VM ssf cannot delete.
    let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Fails, Listing::Fails);
    let home = t.vm.lima_home.clone().unwrap();
    let err = t.vm.lima_destroy().unwrap_err().to_string();
    assert!(err.contains("still holds something of ssf-one"), "{err}");
    std::fs::remove_dir_all(&home).unwrap();
    assert!(!t.vm.lima_destroy().unwrap());
}

#[test]
fn a_destroy_does_not_declare_a_disk_absent_it_could_not_look_for() {
    // The step that declares the destroy finished falls back to
    // lima's own filesystem when `limactl` fails, and read it with
    // `exists()`. A denied `stat` then reported a clean destroy
    // over storage nobody could look for.
    //
    // Both of its fallbacks, because they are different code and
    // the second says the more dangerous thing: with the *disk*
    // listing failing, `lima_destroy` refuses the step over a disk
    // it cannot account for; with the *instance* listing failing it
    // consults `lima_leftovers`, whose `Some(false)` is a `return
    // Ok(false)` -- a destroy reported as finished.
    use std::os::unix::fs::PermissionsExt;
    for listing in [Listing::Answers, Listing::Fails] {
        let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Fails, listing);
        let home = t.vm.lima_home.clone().unwrap();
        std::fs::create_dir_all(home.join("_disks").join(t.vm.lima_disk_name())).unwrap();

        let mut perm = std::fs::metadata(&home).unwrap().permissions();
        perm.set_mode(0o000);
        std::fs::set_permissions(&home, perm).unwrap();
        let seen = std::fs::metadata(home.join("_disks").join(t.vm.lima_disk_name()))
            .map_err(|e| e.kind());
        // While the mode is still 0o000: afterwards it is true for
        // everyone and says nothing.
        let readable_anyway = std::fs::read_dir(&home).is_ok();
        let got = t.vm.lima_destroy().map_err(|e| format!("{e:#}"));
        let mut perm = std::fs::metadata(&home).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&home, perm).unwrap();

        match seen {
            Err(std::io::ErrorKind::NotFound) => {
                panic!("the fixture is wrong: the disk directory is there")
            }
            Err(_) => {
                let err =
                    got.expect_err("storage nobody could look for is not storage that is gone");
                let want = match listing {
                    Listing::Answers => "may hold it",
                    _ => "could not be read or is not there",
                };
                assert!(err.contains(want), "{listing:?}: {err}");
            }
            // euid 0 ignores the mode, so this case cannot be built
            // here and this run proves nothing about it. The
            // assertion is the narrow one that is true: the home
            // really was readable.
            Ok(_) => assert!(
                readable_anyway,
                "a stat succeeded through a directory nothing should have been able to read"
            ),
        }
    }
}
