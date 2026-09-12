use super::*;
use crate::state::IssueState;

fn issue(n: u64, path: Option<&str>) -> IssueState {
    IssueState {
        number: n,
        title: format!("item {n}"),
        worktree_path: path.map(str::to_string),
        ..Default::default()
    }
}

fn state_with(issues: Vec<IssueState>) -> State {
    let mut st = State::default();
    let rs = st.repo_mut("o/r");
    for i in issues {
        rs.issues.insert(i.number, i);
    }
    st
}

/// A fake look at a path: the state is the last path segment, so a
/// test names the outcome it wants in the path itself.
async fn by_name(path: String) -> Inspection {
    let state = path.rsplit('/').next().unwrap_or("").replace('_', " ");
    let problems = if state == "dirty" {
        vec!["2 uncommitted changes (modified or untracked files)".to_string()]
    } else {
        Vec::new()
    };
    Inspection { state, problems }
}

#[tokio::test]
async fn report_picks_owned_workspaces_and_marks_open_and_closed() {
    let mut open = issue(1, Some("/w/dirty"));
    open.active = true;
    open.github_state = Some("open".into());
    let mut closed = issue(2, Some("/w/clean_and_pushed"));
    closed.github_state = Some("closed".into());
    let mut open_inactive = issue(3, Some("/w/unknown"));
    open_inactive.github_state = Some("open".into());
    let no_workspace = issue(4, None);
    let mut sub = issue(5, Some("/w/dirty"));
    sub.subscriber_only = true;
    let mut shared = issue(6, Some("/w/dirty"));
    shared.shares_workspace_of = Some(1);
    let mut gone = issue(7, Some("/w/already_gone"));
    gone.github_state = Some("merged".into());
    let st = state_with(vec![
        open,
        closed,
        open_inactive,
        no_workspace,
        sub,
        shared,
        gone,
    ]);
    let r = report_from(&st, true, by_name).await;
    assert!(r.daemon);
    let sessions: Vec<&str> = r.items.iter().map(|i| i.session.as_str()).collect();
    assert_eq!(sessions, ["o/r#1", "o/r#2", "o/r#3", "o/r#7"]);
    assert!(r.items[0].open);
    assert!(!r.items[1].open);
    assert!(
        r.items[2].open,
        "an open item with no session is still open"
    );
    assert!(!r.items[3].open);
    assert_eq!(r.items[0].state, "dirty");
    assert_eq!(r.items[0].problems.len(), 1);
    assert_eq!(r.items[1].state, "clean and pushed");
    assert_eq!(r.items[3].state, "already gone");
    let unpushed: Vec<&str> = r.unpushed().iter().map(|i| i.session.as_str()).collect();
    assert_eq!(
        unpushed,
        ["o/r#1", "o/r#3"],
        "dirty and unknown count, clean and gone do not"
    );
}

#[tokio::test]
async fn report_covers_repos_no_longer_watched_and_round_trips_as_json() {
    let mut st = state_with(vec![issue(1, Some("/w/clean_and_pushed"))]);
    st.repo_mut("gone/repo")
        .issues
        .insert(9, issue(9, Some("/w/dirty")));
    let r = report_from(&st, false, by_name).await;
    assert_eq!(r.items.len(), 2);
    assert_eq!(r.items[0].session, "gone/repo#9");
    let json = serde_json::to_string(&r).unwrap();
    let back: Report = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r);
    assert!(!back.daemon);
}

#[tokio::test]
async fn a_missing_path_is_already_gone_and_a_plain_dir_is_unknown() {
    let dir = std::env::temp_dir().join(format!("ssf-uninstall-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let missing = dir.join("nope").to_string_lossy().to_string();
    assert_eq!(inspect_path(missing).await.state, "already gone");
    let found = inspect_path(dir.to_string_lossy().to_string()).await;
    assert_eq!(found.state, "unknown");
    assert_eq!(found.problems.len(), 1, "{:?}", found.problems);
    let _ = std::fs::remove_dir_all(&dir);
}

fn facts() -> Facts {
    Facts {
        service_active: true,
        service_enabled: true,
        desktop_present: true,
        bot: Some("bot".into()),
        has_token: true,
        key_ids: true,
        vm_mode: true,
        vm_name: "factory".into(),
        vm_present: Some(true),
        vm_running: Some(true),
        vm_startable: true,
        vm_data: Some(true),
        vm_disk: Some("ssf-factory".into()),
        vm_removed: "its disks in /vm/factory".into(),
        vm_base: PathBuf::from("/nonexistent/vm"),
        config_dir: PathBuf::from("/c"),
        state_dir: PathBuf::from("/s"),
        projects: vec![PathBuf::from("/p")],
        vm_stranded_disk: None,
    }
}

#[test]
fn a_stranded_disk_stops_the_destroy_even_when_the_guest_answered() {
    // A Firecracker `data.ext4` in `<[vm] dir>/<name>` is on no
    // disk lima mounted, so a healthy lima guest reports a clean
    // machine and knows nothing about the clones on it. `run` sets
    // `opts.vm_unchecked` only where the report failed, so on that
    // path the flag stays false -- and nothing else stood between
    // those clones and `Vm::destroy`, with no prompt at all under
    // `--yes`. Hence the check is its own clause in `hard_stop`,
    // which every path reaches, rather than another input to that
    // flag.
    let mut f = facts();
    f.vm_stranded_disk = Some(PathBuf::from("/g/vm/factory/data.ext4"));
    // Exactly what `run` reaches `hard_stop` with when the guest
    // answered: no `vm_unchecked`, and a report with nothing in it.
    let healthy = Opts::default();
    let why = hard_stop(&f, &Report::default(), &healthy, false)
        .expect("a stranded disk stops the command");
    assert!(why.contains("/g/vm/factory/data.ext4"), "{why}");
    assert!(
        why.contains("put [vm] backend back to firecracker"),
        "{why}"
    );
    // Deliberately not asserted here: `hard_stop(.., true).is_none()`
    // holds for every input, so it would say nothing about this
    // clause. `--force`'s early return is above all of them.
    //
    // Without the disk the same facts go through, so the two
    // assertions above are about the disk and not about the
    // fixture.
    f.vm_stranded_disk = None;
    assert!(hard_stop(&f, &Report::default(), &healthy, false).is_none());
}

#[test]
fn the_stranded_disk_is_read_on_the_host_and_survives_the_guests_answer() {
    let _sandbox = crate::config::test_support::sandbox();
    let base = std::env::temp_dir().join(format!(
        "ssf-stranded-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut cfg = crate::config::Config::default();
    cfg.vm.enabled = true;
    cfg.vm.name = "factory".into();
    cfg.vm.dir = base.to_string_lossy().into_owned();
    cfg.vm.backend = Some(vm::BackendKind::Lima);
    // A `limactl` that is not there, so this asks lima nothing and
    // cannot reach the developer's own `~/.lima`.
    cfg.vm.limactl = Some(base.join("no-limactl").to_string_lossy().into_owned());
    let vmm = vm::Vm::new(&cfg);
    std::fs::create_dir_all(&vmm.dir).unwrap();
    let disk = vmm.dir.join("data.ext4");
    std::fs::write(&disk, b"clones").unwrap();

    let gathered = Facts::gather(&cfg, &vmm);
    let mut answered = gathered.clone();
    answered.ssh_answered(&vmm);
    let without = {
        std::fs::remove_file(&disk).unwrap();
        let f = Facts::gather(&cfg, &vmm);
        std::fs::write(&disk, b"clones").unwrap();
        f
    };
    std::fs::remove_dir_all(&base).unwrap();

    assert_eq!(gathered.vm_stranded_disk.as_deref(), Some(disk.as_path()));
    // `ssh_answered` is the ordinary VM-mode path -- guest up,
    // answering -- and it rebuilds several of these fields by hand.
    // Dropping the disk there would take the refusal off exactly the
    // machines that have one.
    assert_eq!(answered.vm_stranded_disk.as_deref(), Some(disk.as_path()));
    // And it is the file that decides, not the configuration.
    assert_eq!(without.vm_stranded_disk, None);
}

#[test]
fn render_lists_what_goes_and_what_stays() {
    let r = Report {
        daemon: true,
        items: vec![Item {
            session: "o/r#9".into(),
            title: "a title".into(),
            open: false,
            path: "/p/w".into(),
            state: "dirty".into(),
            problems: vec!["1 uncommitted change".into()],
        }],
    };
    let text = render(&facts(), &r, &Opts::default());
    assert!(
        text.contains(&format!(
            "stop:    {} (running, enabled) and VM factory",
            crate::platform::service_name()
        )),
        "{text}"
    );
    assert!(
        text.contains("installed Omarchy widget and menu entries (remove them through Omarchy)"),
        "{text}"
    );
    assert!(text.contains("clean and pushed (ssf purge)"), "{text}");
    assert!(
        text.contains("VM factory and its disks in /vm/factory; the clones"),
        "{text}"
    );
    assert!(
        text.contains("revoke:  @bot's SSH and signing keys on GitHub; its token here"),
        "{text}"
    );
    assert!(
        text.contains("/p (clones and worktrees; may hold unpushed work)"),
        "{text}"
    );
    assert!(text.contains("/c and /s (remove with --data)"), "{text}");
    assert!(!text.contains("(--data)"), "{text}");
    assert!(
        text.contains("closed  o/r#9 \"a title\"  [dirty]  /p/w"),
        "{text}"
    );
    assert!(text.contains("- 1 uncommitted change"), "{text}");
    assert!(!text.contains("(none)"), "{text}");
}

#[test]
fn the_vm_base_is_not_called_safe_to_remove_when_nothing_looked_in_it() {
    // `[vm] dir` held "safe to remove" unconditionally, on the
    // strength of nothing: the report never looks inside it. What
    // is in there is usually the image and the downloads, and
    // sometimes a whole VM directory a changed `[vm] name` left
    // behind, with clones and worktrees on its disk. Saying "safe"
    // about a directory nobody inspected is the report telling a
    // person to delete their own work.
    //
    // The fixture's `vm_base` does not exist, so no test reached
    // this line at all -- `render` prints it only when the
    // directory is there.
    let base = std::env::temp_dir().join(format!(
        "ssf-vmbase-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let mut f = facts();
    f.vm_base = base.clone();
    let text = render(&f, &Report::default(), &Opts::default());
    std::fs::remove_dir_all(&base).unwrap();
    let line = text
        .lines()
        .find(|l| l.contains(&base.display().to_string()))
        .unwrap_or_else(|| panic!("the base has to be named at all: {text}"));
    assert!(
        line.contains("retained -- inspect before removing"),
        "the claim has to be about what was checked: {line}"
    );
    assert!(
        !line.contains("safe to remove"),
        "nothing looked in it: {line}"
    );
}

#[test]
fn under_lima_the_report_names_the_instance_and_disk_not_the_vm_directory() {
    // The lima disks are not under `[vm] dir`: they are lima's, in
    // lima's own home, and `ssf vm destroy` names them.
    let mut f = facts();
    f.vm_removed =
            "the lima instance ssf-factory and its data disk ssf-factory in lima's home, and /vm/factory"
                .into();
    let text = render(&f, &Report::default(), &Opts::default());
    assert!(
            text.contains(
                "VM factory and the lima instance ssf-factory and its data disk ssf-factory in lima's home, and /vm/factory; the clones"
            ),
            "{text}"
        );
}

#[test]
fn render_with_the_daemon_down_says_not_purged() {
    let mut f = facts();
    let text = render(&f, &Report::default(), &Opts::default());
    assert!(
        text.contains(
            "workspaces of closed items: not purged (the daemon is not running); left in place"
        ),
        "{text}"
    );
    assert!(text.contains("(none)"), "{text}");
    f.vm_running = Some(false);
    let text = render(&f, &Report::default(), &Opts::default());
    assert!(
        text.contains("not purged (VM factory is not running)"),
        "{text}"
    );
    // A probe that could not be made is a third answer, and saying
    // "is not running" over it points at `ssf vm start` for a guest
    // that may be working.
    f.vm_running = None;
    let text = render(&f, &Report::default(), &Opts::default());
    assert!(
        text.contains("not purged (VM factory could not be asked whether it is running)"),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "stop:    {} (running, enabled) and VM factory (if it is up",
            crate::platform::service_name()
        )),
        "{text}"
    );
}

#[test]
fn render_with_data_moves_the_dirs_from_keep_to_remove() {
    let text = render(
        &facts(),
        &Report::default(),
        &Opts {
            data: true,
            ..Opts::default()
        },
    );
    assert!(text.contains("/c and /s (--data)"), "{text}");
    assert!(!text.contains("remove with --data"), "{text}");
}

#[test]
fn render_says_what_is_already_gone() {
    let f = Facts {
        service_active: false,
        service_enabled: false,
        desktop_present: false,
        bot: None,
        has_token: false,
        key_ids: false,
        vm_present: Some(false),
        vm_running: Some(false),
        vm_startable: false,
        vm_data: Some(false),
        ..facts()
    };
    let text = render(&f, &Report::default(), &Opts::default());
    assert!(!text.contains("stop:"), "{text}");
    assert!(!text.contains("bar widget"), "{text}");
    assert!(
        text.contains("remove:  workspaces of closed items: not purged"),
        "{text}"
    );
    assert!(text.contains("no VM"), "{text}");
    assert!(
        text.contains("revoke:  bot not signed in; nothing to revoke"),
        "{text}"
    );
}

#[test]
fn render_with_the_vm_unchecked_says_so_instead_of_none() {
    // The VM's disks are there but it is not running.
    let stopped = Facts {
        vm_running: Some(false),
        ..facts()
    };
    let text = render(
        &stopped,
        &Report::default(),
        &Opts {
            vm_unchecked: true,
            report_error: Some("could not get the report".into()),
            ..Opts::default()
        },
    );
    assert!(text.contains("could not get the report"), "{text}");
    assert!(
        text.contains("VM factory is not running: its workspaces")
            && text.contains("`ssf vm start` first"),
        "{text}"
    );
    assert!(!text.contains("(none)"), "{text}");
}

#[test]
fn hard_stop_names_unpushed_work_and_what_force_does_to_it() {
    let dirty = Report {
        daemon: true,
        items: vec![Item {
            session: "o/r#1".into(),
            title: "t".into(),
            open: true,
            path: "/p/w".into(),
            state: "dirty".into(),
            problems: vec![],
        }],
    };
    let host = Facts::default();
    let why = hard_stop(&host, &dirty, &Opts::default(), false).unwrap();
    assert!(why.starts_with("1 workspace holds"), "{why}");
    assert!(why.ends_with("leave it where it is"), "{why}");
    let guest = Facts {
        vm_mode: true,
        vm_name: "factory".into(),
        ..Facts::default()
    };
    let why = hard_stop(&guest, &dirty, &Opts::default(), false).unwrap();
    assert!(why.ends_with("destroy it with the VM's disks"), "{why}");
    assert!(hard_stop(&guest, &dirty, &Opts::default(), true).is_none());
    // Clean and gone workspaces do not stop anything.
    let mut clean = dirty.clone();
    clean.items[0].state = "clean and pushed".into();
    clean.items.push(Item {
        state: "already gone".into(),
        ..clean.items[0].clone()
    });
    assert!(hard_stop(&host, &clean, &Opts::default(), false).is_none());
}

#[test]
fn hard_stop_on_an_unchecked_vm_says_how_to_check_it() {
    let stopped = Facts {
        vm_mode: true,
        vm_name: "factory".into(),
        vm_present: Some(true),
        vm_running: Some(false),
        vm_startable: true,
        ..Facts::default()
    };
    let unchecked = Opts {
        vm_unchecked: true,
        ..Opts::default()
    };
    let why = hard_stop(&stopped, &Report::default(), &unchecked, false).unwrap();
    assert!(
        why.contains("is not running") && why.contains("`ssf vm start`"),
        "{why}"
    );
    let running = Facts {
        vm_running: Some(true),
        ..stopped.clone()
    };
    let why = hard_stop(&running, &Report::default(), &unchecked, false).unwrap();
    assert!(
        why.contains("gave no report") && why.contains("`ssf vm restart`"),
        "{why}"
    );
    assert!(hard_stop(&running, &Report::default(), &unchecked, true).is_none());
    assert!(hard_stop(&running, &Report::default(), &Opts::default(), false).is_none());
}

#[test]
fn render_with_a_silent_running_vm_points_at_restart() {
    let facts = Facts {
        vm_mode: true,
        vm_name: "factory".into(),
        vm_present: Some(true),
        vm_running: Some(true),
        vm_startable: true,
        ..Facts::default()
    };
    let opts = Opts {
        vm_unchecked: true,
        report_error: Some("VM factory is running but does not answer on ssh".into()),
        ..Opts::default()
    };
    let text = render(&facts, &Report::default(), &opts);
    assert!(text.contains("does not answer on ssh"), "{text}");
    assert!(
        text.contains("VM factory gave no report: its workspaces")
            && text.contains("`ssf vm restart` first"),
        "{text}"
    );
    assert!(!text.contains("factory is not running"), "{text}");
}

#[test]
fn an_unaskable_backend_is_neither_a_running_vm_nor_a_missing_one() {
    // The three answers the report and the refusal have to keep
    // apart. Reading "the probe could not be made" as "not running"
    // put `ssf vm start`, and a `--force` that destroys the clones
    // on the data disk, in front of a person whose VM was up.
    let base = Facts {
        vm_mode: true,
        vm_name: "factory".into(),
        vm_present: None,
        vm_running: None,
        vm_startable: false,
        ..Facts::default()
    };
    let unchecked = Opts {
        vm_unchecked: true,
        ..Opts::default()
    };
    let why = hard_stop(&base, &Report::default(), &unchecked, false).unwrap();
    assert!(
        why.contains("could not be asked whether it is running") && why.contains("limactl list"),
        "{why}"
    );
    assert!(!why.contains("ssf vm start"), "{why}");
    let text = render(&base, &Report::default(), &unchecked);
    // The report may not promise to destroy what it could not find.
    assert!(
        text.contains("could not be asked whether it is there"),
        "{text}"
    );
    assert!(!text.contains("go with it"), "{text}");
    assert!(!text.contains("no VM"), "{text}");
}

#[test]
fn a_disk_that_outlived_its_instance_is_not_offered_a_start() {
    // `ssf vm start` refuses an instance that is not there, so
    // offering it as the way to check the workspaces on a disk that
    // outlived one leaves `--force` as the only real route.
    let orphan = Facts {
        vm_mode: true,
        vm_name: "factory".into(),
        vm_present: Some(true),
        vm_running: Some(false),
        vm_startable: false,
        // The state that reaches this message: a disk, and no
        // instance. Leftovers in `[vm] dir` alone hold no work, so
        // they never get here.
        vm_data: Some(true),
        ..Facts::default()
    };
    let unchecked = Opts {
        vm_unchecked: true,
        ..Opts::default()
    };
    let why = hard_stop(&orphan, &Report::default(), &unchecked, false).unwrap();
    assert!(why.contains("data disk outlived its instance"), "{why}");
    assert!(!why.contains("ssf vm start"), "{why}");
    assert!(why.contains("--force"), "{why}");
}

#[test]
fn leftovers_in_the_vm_directory_are_removed_without_a_refusal() {
    // Under lima `[vm] dir`/<name> holds the generated template, the
    // ssh key and the share -- ssf's own, no one's work. A build
    // that died before `limactl create`, or a person who deleted the
    // instance and the disk by hand, leaves exactly that. There is
    // something to remove and nothing to check, so the command must
    // not refuse, and must not say a data disk outlived anything.
    let leftovers = Facts {
        vm_mode: true,
        vm_name: "factory".into(),
        vm_present: Some(true),
        vm_running: Some(false),
        vm_startable: false,
        vm_data: Some(false),
        vm_removed: "/v/factory".into(),
        ..Facts::default()
    };
    // The rule itself, not `Opts::default()`, which would make any
    // facts pass: a disk that is not there is the one answer that
    // lets the command go ahead.
    assert!(!unchecked_workspaces(leftovers.vm_data));
    assert!(unchecked_workspaces(Some(true)));
    assert!(unchecked_workspaces(None), "an unasked disk stops it too");
    let text = render(&leftovers, &Report::default(), &Opts::default());
    assert!(text.contains("VM factory and /v/factory"), "{text}");
    assert!(!text.contains("go with it"), "{text}");
    assert!(!text.contains("outlived"), "{text}");
}

#[test]
fn the_report_names_only_the_parts_of_a_lima_vm_that_are_there() {
    let survey = |startable, running, data| vm::Survey {
        present: Some(true),
        running,
        startable,
        data,
    };
    assert_eq!(
        lima_removed(
            "ssf-f",
            "ssf-f",
            Some(Path::new("/v/f")),
            &survey(true, Some(false), Some(true))
        ),
        "the lima instance ssf-f, its data disk ssf-f in lima's home, and /v/f"
    );
    // An instance whose directory was removed by hand: the missing
    // directory is not named.
    assert_eq!(
        lima_removed(
            "ssf-f",
            "ssf-f",
            None,
            &survey(true, Some(true), Some(true))
        ),
        "the lima instance ssf-f and its data disk ssf-f in lima's home"
    );
    // A disk that outlived its instance.
    assert_eq!(
        lima_removed(
            "ssf-f",
            "ssf-f",
            None,
            &survey(false, Some(false), Some(true))
        ),
        "its data disk ssf-f in lima's home"
    );
    // Leftovers in `[vm] dir` and nothing of lima's: ssf's own
    // template, ssh key and share, and no promise about anyone else.
    assert_eq!(
        lima_removed(
            "ssf-f",
            "ssf-f",
            Some(Path::new("/v/f")),
            &survey(false, Some(false), Some(false))
        ),
        "/v/f"
    );
    // Nothing could be asked: everything is hedged, nothing asserted.
    assert_eq!(
        lima_removed("ssf-f", "ssf-f", None, &survey(false, None, None)),
        "the lima instance ssf-f if it is there and its data disk ssf-f in lima's home if it is there"
    );
}

#[test]
fn the_refusal_names_the_thing_that_actually_could_not_be_checked() {
    // One source of wording for the report and the refusal, and no
    // arm may assert a fact its inputs do not carry.
    let f = |running, startable, data| Facts {
        vm_mode: true,
        vm_name: "factory".into(),
        vm_present: Some(true),
        vm_running: running,
        vm_startable: startable,
        vm_data: data,
        ..Facts::default()
    };
    // ssh answered but the guest's report did not: `run` settles
    // `vm_running` to `Some(true)` on the strength of that ssh, so
    // the refusal blames the guest and not `limactl`.
    let (what, remedy) = vm_uncheckable(&f(Some(true), false, None));
    assert!(what.contains("gave no report"), "{what}");
    assert!(remedy.contains("ssf vm restart"), "{remedy}");
    let (what, remedy) = vm_uncheckable(&f(Some(false), true, Some(true)));
    assert!(what.contains("is not running"), "{what}");
    assert!(remedy.contains("ssf vm start"), "{remedy}");
    // The one refusal whose whole remedy is a command the person
    // types, so it has to carry the disk's own name -- `ssf-factory`,
    // not `[vm] name`.
    let named = Facts {
        vm_disk: Some("ssf-factory".into()),
        ..f(Some(false), false, Some(true))
    };
    let (what, remedy) = vm_uncheckable(&named);
    assert!(what.contains("data disk outlived its instance"), "{what}");
    assert!(
        remedy.contains("limactl disk delete ssf-factory"),
        "{remedy}"
    );
    // The disk question is the one that failed: nothing may be said
    // about a disk outliving anything.
    let (what, remedy) = vm_uncheckable(&f(Some(false), false, None));
    assert!(what.contains("could not be asked about"), "{what}");
    assert!(!what.contains("outlived"), "{what}");
    assert!(remedy.contains("limactl disk list"), "{remedy}");
    let (what, remedy) = vm_uncheckable(&f(None, false, None));
    assert!(what.contains("whether it is running"), "{what}");
    assert!(remedy.contains("limactl list"), "{remedy}");
    // Only a guest that is up, or one that can be started, may be
    // pointed at a start or a restart.
    for (running, startable) in [(Some(false), false), (None, false), (None, true)] {
        let (_, remedy) = vm_uncheckable(&f(running, startable, None));
        assert!(!remedy.contains("ssf vm start"), "{remedy}");
        assert!(!remedy.contains("ssf vm restart"), "{remedy}");
    }
}

#[test]
fn host_mode_does_not_destroy_a_data_disk_it_never_asked_about() {
    // `[vm] enabled = false` does not remove a VM: the instance and
    // the data disk survive the flag, and the destroy step is not
    // gated on it. The host's own daemon answers for this machine's
    // workspaces and knows nothing of the sessions that ran in the
    // guest, so `items: (none)` there is "not looked at" -- and
    // printing it under a line promising to delete a disk of clones,
    // with no refusal in between, is how `--yes` came to destroy
    // them.
    let host = Facts {
        vm_mode: false,
        vm_name: "factory".into(),
        vm_present: Some(true),
        vm_running: Some(false),
        vm_startable: true,
        vm_data: Some(true),
        vm_disk: Some("ssf-factory".into()),
        ..Facts::default()
    };
    let unchecked = Opts {
        vm_unchecked: unchecked_workspaces(host.vm_data),
        ..Opts::default()
    };
    assert!(
        unchecked.vm_unchecked,
        "a data disk stops it in host mode too"
    );
    let why = hard_stop(&host, &Report::default(), &unchecked, false).unwrap();
    assert!(why.contains("`[vm] enabled = false`"), "{why}");
    assert!(why.contains("vm.enabled true"), "{why}");
    // Starting the VM would not clear this one: nothing asks the
    // guest while the configuration does not point at it.
    assert!(!why.contains("`ssf vm start`"), "{why}");
    let text = render(&host, &Report::default(), &unchecked);
    assert!(!text.contains("(none)"), "{text}");
    // A host that never had a VM is untouched by any of it.
    let bare = Facts {
        vm_data: Some(false),
        ..host.clone()
    };
    assert!(!unchecked_workspaces(bare.vm_data));
    assert!(hard_stop(&bare, &Report::default(), &Opts::default(), false).is_none());
}

#[test]
fn ssh_answering_proves_the_guest_is_there_and_not_that_it_has_a_disk() {
    // Under lima the sshd that answered is lima's own, and it is not
    // gated on the data disk being mounted -- so an instance whose
    // disk was deleted by hand answers ssh perfectly well. Reading a
    // disk out of that would promise to destroy clones that are not
    // there, and refuse over them.
    let mut cfg = Config::default();
    cfg.vm.name = "factory".into();
    cfg.vm.dir = "/nonexistent/ssf-vm".into();
    cfg.vm.backend = Some(vm::BackendKind::Lima);
    let vm = vm::Vm::new(&cfg);
    for (data, promises_clones) in [(Some(true), true), (Some(false), false), (None, true)] {
        let mut f = Facts {
            vm_mode: true,
            vm_name: "factory".into(),
            vm_present: None,
            vm_running: None,
            vm_startable: false,
            vm_data: data,
            ..Facts::default()
        };
        f.ssh_answered(&vm);
        assert_eq!(f.vm_present, Some(true));
        assert_eq!(f.vm_running, Some(true));
        assert!(f.vm_startable);
        assert_eq!(f.vm_data, data, "ssh says nothing about the disk");
        let text = render(&f, &Report::default(), &Opts::default());
        assert_eq!(
            text.contains("clones and worktrees on its data disk"),
            promises_clones,
            "{text}"
        );
        // A disk lima could not be asked about is hedged, never
        // promised as fact.
        assert_eq!(
            text.contains("if it is there, go with it"),
            data.is_none(),
            "{text}"
        );
        // Whatever the backend could not say, nothing is hedged
        // about the instance any more.
        assert!(!f.vm_removed.contains("instance ssf-factory if it is there"));
    }
}

#[test]
fn package_removal_is_pacman_on_omarchy() {
    let cmd = package_removal_command();
    if crate::platform::is_omarchy() && crate::platform::which("pacman").is_some() {
        assert_eq!(cmd, "sudo pacman -R ssf");
    } else {
        assert!(cmd.contains("ssf"), "{cmd}");
    }
}

#[test]
fn data_removal_tolerates_missing_dirs() {
    let dir = std::env::temp_dir().join(format!(
        "ssf-uninstall-data-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    assert!(!remove_dir(&dir).unwrap());
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/f"), "x").unwrap();
    assert!(remove_dir(&dir).unwrap());
    assert!(!dir.exists());
    assert!(!remove_dir(&dir).unwrap());
}
