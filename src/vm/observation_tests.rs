//! Permission regressions for the filesystem observations shared by
//! uninstall, vm status, and doctor.
//!
//! These run their assertions in a child process.  A root test runner drops
//! the child to the conventional `nobody` uid after the test binary has been
//! loaded, so mode bits still have their ordinary effect.  Every child first
//! proves that the operation meant to fail really returns `PermissionDenied`;
//! a privileged runner can therefore never turn these into passing no-ops.

#![cfg(unix)]

use super::{BackendKind, Vm};
use crate::config::Config;
use crate::uninstall::{Facts, kept, left_in_place, unread_note};
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const CHILD_TEST: &str = "vm::observation_tests::permission_fixture_child";
const CASE_ENV: &str = "SSF_OBSERVATION_PERMISSION_CASE";
const ROOT_ENV: &str = "SSF_OBSERVATION_PERMISSION_ROOT";
const DELETED_CWD_ROOT: &str = "SSF_OBSERVATION_DELETED_CWD_ROOT";

#[derive(Clone, Copy)]
enum Case {
    InaccessibleParent,
    VmEntries,
    InaccessibleData,
    OwnSymlink,
    NestedOwnData,
    UnreadFirecrackerStray,
    LimaHomeParent,
    LimaHome,
    LimaHomeEntries,
    LimaDisks,
    LimaDiskEntries,
    DanglingSymlink,
    InaccessibleSymlinkTarget,
    LimaLeftoversCompletion,
    LimaDiskCompletion,
    FirecrackerCompletion,
    LimaDeniedLocal,
    LimaOwnInstanceSymlink,
    LimaOwnDiskSymlink,
}

impl Case {
    fn id(self) -> &'static str {
        match self {
            Self::InaccessibleParent => "inaccessible-parent",
            Self::VmEntries => "vm-entries",
            Self::InaccessibleData => "inaccessible-data",
            Self::OwnSymlink => "own-symlink",
            Self::NestedOwnData => "nested-own-data",
            Self::UnreadFirecrackerStray => "unread-firecracker-stray",
            Self::LimaHomeParent => "lima-home-parent",
            Self::LimaHome => "lima-home",
            Self::LimaHomeEntries => "lima-home-entries",
            Self::LimaDisks => "lima-disks",
            Self::LimaDiskEntries => "lima-disk-entries",
            Self::DanglingSymlink => "dangling-symlink",
            Self::InaccessibleSymlinkTarget => "inaccessible-symlink-target",
            Self::LimaLeftoversCompletion => "lima-leftovers-completion",
            Self::LimaDiskCompletion => "lima-disk-completion",
            Self::FirecrackerCompletion => "firecracker-completion",
            Self::LimaDeniedLocal => "lima-denied-local",
            Self::LimaOwnInstanceSymlink => "lima-own-instance-symlink",
            Self::LimaOwnDiskSymlink => "lima-own-disk-symlink",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "inaccessible-parent" => Self::InaccessibleParent,
            "vm-entries" => Self::VmEntries,
            "inaccessible-data" => Self::InaccessibleData,
            "own-symlink" => Self::OwnSymlink,
            "nested-own-data" => Self::NestedOwnData,
            "unread-firecracker-stray" => Self::UnreadFirecrackerStray,
            "lima-home-parent" => Self::LimaHomeParent,
            "lima-home" => Self::LimaHome,
            "lima-home-entries" => Self::LimaHomeEntries,
            "lima-disks" => Self::LimaDisks,
            "lima-disk-entries" => Self::LimaDiskEntries,
            "dangling-symlink" => Self::DanglingSymlink,
            "inaccessible-symlink-target" => Self::InaccessibleSymlinkTarget,
            "lima-leftovers-completion" => Self::LimaLeftoversCompletion,
            "lima-disk-completion" => Self::LimaDiskCompletion,
            "firecracker-completion" => Self::FirecrackerCompletion,
            "lima-denied-local" => Self::LimaDeniedLocal,
            "lima-own-instance-symlink" => Self::LimaOwnInstanceSymlink,
            "lima-own-disk-symlink" => Self::LimaOwnDiskSymlink,
            other => panic!("unknown permission fixture {other}"),
        }
    }
}

struct Fixture {
    root: PathBuf,
    protected: Vec<PathBuf>,
}

impl Fixture {
    fn new(case: Case) -> Self {
        let root = std::env::temp_dir().join(format!(
            "ssf-observation-{}-{}-{}",
            case.id(),
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        set_mode(&root, 0o755);
        let mut fixture = Self {
            root,
            protected: Vec::new(),
        };
        fixture.build(case);
        fixture
    }

    fn build(&mut self, case: Case) {
        match case {
            Case::InaccessibleParent => {
                let wall = self.root.join("wall");
                fs::create_dir_all(wall.join("vm/old")).unwrap();
                fs::write(wall.join("vm/old/data.ext4"), b"work").unwrap();
                self.protect(&wall, 0o000);
            }
            Case::VmEntries => {
                let base = self.root.join("vm");
                fs::create_dir(&base).unwrap();
                for name in [OsString::from("new"), OsString::from("old"), odd_name()] {
                    fs::create_dir(base.join(name)).unwrap();
                }
                self.protect(&base, 0o444);
            }
            Case::InaccessibleData => {
                for name in ["new", "old"] {
                    let dir = self.root.join("vm").join(name);
                    fs::create_dir_all(&dir).unwrap();
                    fs::write(dir.join("data.ext4"), b"work").unwrap();
                    self.protect(&dir, 0o000);
                }
            }
            Case::OwnSymlink => {
                let base = self.root.join("vm");
                let wall = self.root.join("wall");
                fs::create_dir_all(&base).unwrap();
                fs::create_dir_all(wall.join("target")).unwrap();
                symlink(wall.join("target"), base.join("new")).unwrap();
                self.protect(&wall, 0o000);
            }
            Case::NestedOwnData => {
                let nested = self.root.join("vm/new/nested");
                fs::create_dir_all(&nested).unwrap();
                fs::write(nested.join("data.ext4"), b"work").unwrap();
                self.protect(&nested, 0o000);
            }
            Case::UnreadFirecrackerStray => {
                let old = self.root.join("vm/old");
                fs::create_dir_all(&old).unwrap();
                fs::write(old.join("data.ext4"), b"work").unwrap();
                self.protect(&old, 0o000);
            }
            Case::LimaHomeParent => {
                let wall = self.root.join("lima-wall");
                fs::create_dir_all(wall.join("lima/_disks/ssf-old")).unwrap();
                fs::create_dir(wall.join("lima/ssf-old")).unwrap();
                self.protect(&wall, 0o000);
            }
            Case::LimaHome => {
                let home = self.root.join("lima");
                fs::create_dir_all(home.join("_disks/ssf-old")).unwrap();
                fs::create_dir(home.join("ssf-old")).unwrap();
                fs::create_dir_all(home.join("vm/new")).unwrap();
                fs::write(home.join("vm/new/data.ext4"), b"work").unwrap();
                self.protect(&home, 0o000);
            }
            Case::LimaHomeEntries => {
                let home = self.root.join("lima");
                fs::create_dir_all(home.join("_disks")).unwrap();
                for name in [
                    OsString::from("ssf-new"),
                    OsString::from("ssf-old"),
                    odd_name(),
                ] {
                    fs::create_dir(home.join(name)).unwrap();
                }
                self.protect(&home, 0o444);
            }
            Case::LimaDisks => {
                let disks = self.root.join("lima/_disks");
                fs::create_dir_all(disks.join("ssf-old")).unwrap();
                self.protect(&disks, 0o000);
            }
            Case::LimaDiskEntries => {
                let disks = self.root.join("lima/_disks");
                fs::create_dir_all(&disks).unwrap();
                for name in [
                    OsString::from("ssf-new"),
                    OsString::from("ssf-old"),
                    odd_name(),
                ] {
                    fs::create_dir(disks.join(name)).unwrap();
                }
                self.protect(&disks, 0o444);
            }
            Case::DanglingSymlink => {
                symlink(self.root.join("missing-target"), self.root.join("vm")).unwrap();
            }
            Case::InaccessibleSymlinkTarget => {
                let wall = self.root.join("wall");
                fs::create_dir_all(wall.join("target/old")).unwrap();
                fs::write(wall.join("target/old/data.ext4"), b"work").unwrap();
                symlink(wall.join("target"), self.root.join("vm")).unwrap();
                self.protect(&wall, 0o000);
            }
            Case::LimaLeftoversCompletion => {
                let home = self.root.join("lima");
                fs::create_dir_all(home.join("_disks/ssf-new")).unwrap();
                fs::create_dir(home.join("ssf-new")).unwrap();
                self.protect(&home, 0o000);
            }
            Case::LimaDiskCompletion => {
                let disks = self.root.join("lima/_disks");
                fs::create_dir_all(disks.join("ssf-new")).unwrap();
                write_limactl(&self.root.join("limactl"), true);
                self.protect(&disks, 0o000);
            }
            Case::FirecrackerCompletion => {
                let wall = self.root.join("wall");
                fs::create_dir_all(wall.join("vm/new")).unwrap();
                self.protect(&wall, 0o000);
            }
            Case::LimaDeniedLocal => {
                let base = self.root.join("vm");
                let wall = self.root.join("wall");
                fs::create_dir_all(&base).unwrap();
                fs::create_dir_all(wall.join("target")).unwrap();
                fs::create_dir_all(self.root.join("lima/_disks")).unwrap();
                symlink(wall.join("target"), base.join("new")).unwrap();
                write_limactl(&self.root.join("limactl"), false);
                self.protect(&wall, 0o000);
            }
            Case::LimaOwnInstanceSymlink | Case::LimaOwnDiskSymlink => {
                let home = self.root.join("lima");
                let wall = self.root.join("wall");
                fs::create_dir_all(home.join("_disks")).unwrap();
                fs::create_dir_all(wall.join("target")).unwrap();
                let link = match case {
                    Case::LimaOwnInstanceSymlink => home.join("ssf-new"),
                    Case::LimaOwnDiskSymlink => home.join("_disks/ssf-new"),
                    _ => unreachable!(),
                };
                symlink(wall.join("target"), link).unwrap();
                write_limactl(&self.root.join("limactl"), false);
                self.protect(&wall, 0o000);
            }
        }
    }

    fn protect(&mut self, path: &Path, mode: u32) {
        set_mode(path, mode);
        self.protected.push(path.to_path_buf());
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for path in &self.protected {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn odd_name() -> OsString {
    OsString::from_vec(b"ssf-odd-\xff".to_vec())
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn write_limactl(path: &Path, fail_disk_list: bool) {
    let body = if fail_disk_list {
        "#!/bin/sh\ncase \" $* \" in *\" disk list \"*) exit 1;; *) exit 0;; esac\n"
    } else {
        "#!/bin/sh\nexit 0\n"
    };
    fs::write(path, body).unwrap();
    set_mode(path, 0o755);
}

fn run(case: Case) {
    let fixture = Fixture::new(case);
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--ignored", "--nocapture"])
        .env(CASE_ENV, case.id())
        .env(ROOT_ENV, &fixture.root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "permission child for {} failed\nstdout:\n{}\nstderr:\n{}",
        case.id(),
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(&format!("permission fixture completed: {}", case.id())),
        "permission child for {} did not run the requested fixture\nstdout:\n{}",
        case.id(),
        stdout
    );
}

#[test]
fn inaccessible_parent_is_unknown_in_every_report() {
    run(Case::InaccessibleParent);
}

#[test]
fn enumerable_vm_directory_names_every_unstattable_entry() {
    run(Case::VmEntries);
}

#[test]
fn stattable_vm_entries_name_both_inaccessible_data_disks() {
    run(Case::InaccessibleData);
}

#[test]
fn stattable_own_symlink_names_the_inaccessible_followed_path() {
    run(Case::OwnSymlink);
}

#[test]
fn nested_own_vm_names_its_inaccessible_data_disk() {
    run(Case::NestedOwnData);
}

#[test]
fn missing_own_vm_stays_absent_beside_an_unread_firecracker_stray() {
    run(Case::UnreadFirecrackerStray);
}

#[test]
fn lima_home_beneath_an_inaccessible_parent_is_named() {
    run(Case::LimaHomeParent);
}

#[test]
fn inaccessible_lima_home_is_named() {
    run(Case::LimaHome);
}

#[test]
fn enumerable_lima_home_names_every_unstattable_entry() {
    run(Case::LimaHomeEntries);
}

#[test]
fn inaccessible_lima_disks_directory_is_named() {
    run(Case::LimaDisks);
}

#[test]
fn enumerable_lima_disks_names_every_unstattable_entry() {
    run(Case::LimaDiskEntries);
}

#[test]
fn dangling_symlink_is_the_missing_baseline() {
    run(Case::DanglingSymlink);
}

#[test]
fn symlink_with_an_inaccessible_target_is_unknown() {
    run(Case::InaccessibleSymlinkTarget);
}

#[test]
fn denied_lima_leftovers_fail_destroy() {
    run(Case::LimaLeftoversCompletion);
}

#[test]
fn denied_lima_disk_fallback_fails_destroy() {
    run(Case::LimaDiskCompletion);
}

#[test]
fn denied_firecracker_directory_fails_destroy() {
    run(Case::FirecrackerCompletion);
}

#[test]
fn denied_local_lima_directory_keeps_presence_unknown() {
    run(Case::LimaDeniedLocal);
}

#[test]
fn own_lima_instance_symlink_names_its_inaccessible_target() {
    run(Case::LimaOwnInstanceSymlink);
}

#[test]
fn own_lima_disk_symlink_names_its_inaccessible_target() {
    run(Case::LimaOwnDiskSymlink);
}

#[test]
fn a_deleted_working_directory_suppresses_relative_remedies_in_every_report() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!(
            "ssf-deleted-cwd-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(root.join("cwd")).unwrap();
    for dir in ["vm/old", "vm/new", "vm/new/nested"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    for dir in [
        "lima/ssf-old",
        "lima/ssf-new",
        "lima/_disks/ssf-old",
        "lima/_disks/ssf-new",
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::write(root.join("vm/old/data.ext4"), b"old clones").unwrap();
    fs::write(root.join("vm/new/data.ext4"), b"older clones").unwrap();
    fs::write(root.join("vm/new/nested/data.ext4"), b"live clones").unwrap();
    let limactl = root.join("limactl");
    fs::write(
        &limactl,
        format!(
            r#"#!/bin/sh
if [ "$*" = "--tty=false list --json" ]; then echo '{{"name":"ssf-listed","status":"Stopped","dir":"{}/lima/ssf-listed"}}'; fi
if [ "$*" = "--tty=false disk list --json" ]; then echo '{{"name":"ssf-listed-disk","size":7516192768,"dir":"{}/lima/_disks/ssf-listed-disk"}}'; fi
case "$*" in
  '--tty=false list --json')
    printf '%s\n' '{{"name":"ssf-old","status":"Stopped","dir":"{}/lima/ssf-old"}}'
    printf '%s\n' '{{"name":"ssf-new","status":"Stopped","dir":"{}/lima/ssf-new"}}' ;;
  '--tty=false disk list --json')
    printf '%s\n' '{{"name":"ssf-old","size":7516192768,"dir":"{}/lima/_disks/ssf-old"}}'
    printf '%s\n' '{{"name":"ssf-new","size":7516192768,"dir":"{}/lima/_disks/ssf-new"}}' ;;
esac
"#,
            root.display(),
            root.display(),
            root.display(),
            root.display(),
            root.display(),
            root.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&limactl, fs::Permissions::from_mode(0o755)).unwrap();

    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--ignored", "--nocapture"])
        .env(DELETED_CWD_ROOT, &root)
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&root);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "deleted-cwd child failed\nstdout:\n{}\nstderr:\n{}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("deleted-cwd fixture completed"),
        "deleted-cwd child did not run\nstdout:\n{stdout}"
    );
}

/// Run inside the shared isolated fixture child because removing the
/// process's working directory makes `current_dir()` fail permanently;
/// no parallel test should inherit that process state.
fn exercise_deleted_cwd(root: PathBuf) {
    let _sandbox = crate::config::test_support::sandbox();
    std::env::set_current_dir(root.join("cwd")).unwrap();

    let configurations: Vec<_> = [BackendKind::Firecracker, BackendKind::Lima]
        .into_iter()
        .map(|backend| {
            let mut cfg = Config::default();
            // Keep a configured instance and disk in the listing. The
            // remedy is suppressed below, but `mine` must still feed
            // present/data when the relative home or tool cannot be
            // absolutised.
            cfg.vm.name = "new".into();
            cfg.vm.dir = "../vm".into();
            cfg.vm.backend = Some(backend);
            cfg.vm.limactl = Some(root.join("missing-limactl").to_string_lossy().into_owned());
            let mut vm = Vm::new(&cfg);
            vm.lima_home = Some(root.join("empty-lima"));
            (backend, cfg, vm)
        })
        .collect();

    let lima_contexts: Vec<_> = ["relative-home", "relative-tool"]
        .into_iter()
        .map(|case| {
            let mut cfg = Config::default();
            cfg.vm.name = "new".into();
            cfg.vm.dir = root.join("vm").to_string_lossy().into_owned();
            cfg.vm.backend = Some(BackendKind::Lima);
            cfg.vm.limactl = Some(if case == "relative-tool" {
                "../limactl".into()
            } else {
                root.join("limactl").to_string_lossy().into_owned()
            });
            let mut vm = Vm::new(&cfg);
            vm.lima_home = Some(if case == "relative-home" {
                PathBuf::from("../lima")
            } else {
                root.join("lima")
            });
            (case, cfg, vm)
        })
        .collect();

    // Positive control: while the relative base can be resolved, the
    // unrelated VM is discovered and the live VM's ancestor is not.
    for (backend, _, vm) in &configurations {
        let (strays, unread) = vm.strays_on_filesystem();
        assert!(unread.is_empty(), "{backend} positive control: {unread:?}");
        assert_eq!(
            strays
                .iter()
                .map(|stray| stray.name.as_str())
                .collect::<Vec<_>>(),
            ["old"],
            "{backend} positive control: {strays:?}"
        );
        assert!(
            strays[0].remove.starts_with("rm -rf /") && strays[0].remove.ends_with("/vm/old"),
            "stable positive-control remedy: {}",
            strays[0].remove
        );
    }
    // Positive control for the lima half: both relative settings can be
    // stabilised while the cwd has a name, so its filesystem inventory
    // offers remedies carrying the absolute home and tool paths.
    for (case, _, vm) in &lima_contexts {
        let (strays, unread) = vm.strays_on_disk_read();
        assert!(unread.is_empty(), "{case} positive control: {unread:?}");
        assert_eq!(strays.len(), 2, "{case} positive control: {strays:?}");
        assert!(
            strays
                .iter()
                .all(|stray| stray.remove.contains(&root.display().to_string())),
            "{case} stable positive-control remedies: {strays:?}"
        );
    }

    fs::remove_dir(root.join("cwd")).unwrap();
    assert!(
        std::env::current_dir().is_err(),
        "cwd still had an absolute name"
    );
    assert!(
        fs::metadata("../vm").unwrap().is_dir(),
        "relative base is readable"
    );
    assert!(
        fs::metadata("../vm/new/nested/data.ext4")
            .unwrap()
            .is_file(),
        "configured data is still directly observable"
    );

    let expected = vec![PathBuf::from("../vm")];
    for (backend, cfg, vm) in configurations {
        let survey = vm.survey();
        assert_eq!(survey.present, Some(true), "{backend} configured VM");
        assert_eq!(
            survey.data,
            Some(backend == BackendKind::Firecracker),
            "{backend} configured data"
        );
        assert_eq!(survey.unread, expected, "{backend} survey unread");
        assert!(
            survey.strays.is_empty(),
            "{backend} survey remedies: {:?}",
            survey.strays
        );

        let (doctor_strays, doctor_unread) = vm.strays_on_filesystem();
        assert_eq!(doctor_unread, expected, "{backend} doctor unread");
        assert!(
            doctor_strays.is_empty(),
            "{backend} doctor remedies: {doctor_strays:?}"
        );
        let doctor = crate::stray_notes(&doctor_strays, &doctor_unread);
        assert!(
            doctor.contains("../vm could not be read or named completely"),
            "{doctor}"
        );
        assert!(!doctor.contains("rm -rf"), "{doctor}");

        let status = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(vm.status());
        assert_eq!(status.unread, expected, "{backend} status unread");
        assert!(
            status.strays.is_empty(),
            "{backend} status remedies: {:?}",
            status.strays
        );
        let status_text = crate::render_vm_status(&status);
        assert!(
            status_text.contains("../vm could not be read or named completely"),
            "{status_text}"
        );
        assert!(!status_text.contains("rm -rf"), "{status_text}");

        let facts = Facts::gather(&cfg, &vm);
        assert_eq!(facts.vm_unread, expected, "{backend} uninstall unread");
        assert!(
            facts.vm_strays.is_empty(),
            "{backend} uninstall remedies: {:?}",
            facts.vm_strays
        );
        let keep = kept(&facts, false);
        let base = keep
            .iter()
            .find(|line| line.starts_with("../vm ("))
            .expect("relative VM base retained");
        assert!(base.contains("could not finish inspecting"), "{base}");
        assert!(!base.contains("safe to remove"), "{base}");
        let epilogue = left_in_place(&facts, false);
        assert!(
            epilogue.contains("../vm (VM image and downloads; ssf could not finish inspecting it"),
            "{epilogue}"
        );
        assert!(!epilogue.contains("rm -rf"), "{epilogue}");

        // Failure to name the working directory is not itself proof
        // that every relative target exists. Metadata can still prove a
        // different base absent through the open cwd inode.
        let mut missing_cfg = cfg.clone();
        missing_cfg.vm.dir = "../missing".into();
        missing_cfg.vm.name = "new".into();
        let mut missing_vm = Vm::new(&missing_cfg);
        missing_vm.lima_home = Some(root.join("empty-lima"));
        let (missing_strays, missing_unread) = missing_vm.strays_on_filesystem();
        assert!(missing_strays.is_empty());
        assert!(
            missing_unread.is_empty(),
            "confirmed absence is not unread: {missing_unread:?}"
        );
    }

    // Lima may still execute a relative home or configured tool through
    // the deleted cwd's open inode, and its JSON listings can therefore
    // succeed. The reported deletion command cannot be made stable,
    // though: pasted elsewhere it can select another lima home or
    // executable. Successful listings and filesystem discovery share
    // that remedy boundary and retain the affected object paths instead.
    for (case, cfg, vm) in lima_contexts {
        let expected = if case == "relative-home" {
            vec![
                PathBuf::from("../lima/_disks/ssf-listed-disk"),
                PathBuf::from("../lima/_disks/ssf-old"),
                PathBuf::from("../lima/ssf-listed"),
                PathBuf::from("../lima/ssf-old"),
            ]
        } else {
            vec![
                root.join("lima/_disks/ssf-listed-disk"),
                root.join("lima/_disks/ssf-old"),
                root.join("lima/ssf-listed"),
                root.join("lima/ssf-old"),
            ]
        };

        let survey = vm.survey();
        assert_eq!(survey.present, Some(true), "{case} configured VM");
        assert_eq!(survey.data, Some(true), "{case} configured data");
        assert_eq!(survey.unread, expected, "{case} survey unread");
        assert!(
            survey.strays.iter().all(|stray| !matches!(
                stray.kind,
                super::StrayKind::LimaInstance | super::StrayKind::LimaDisk
            )),
            "{case} survey lima remedies: {:?}",
            survey.strays
        );

        let (doctor_strays, doctor_unread) = vm.strays_on_filesystem();
        let filesystem_expected = if case == "relative-home" {
            vec![
                PathBuf::from("../lima/_disks/ssf-old"),
                PathBuf::from("../lima/ssf-old"),
            ]
        } else {
            vec![root.join("lima/_disks/ssf-old"), root.join("lima/ssf-old")]
        };
        assert_eq!(doctor_unread, filesystem_expected, "{case} doctor unread");
        assert!(
            doctor_strays
                .iter()
                .all(|stray| !stray.remove.contains("limactl")),
            "{case} doctor lima remedies: {doctor_strays:?}"
        );
        let doctor = crate::stray_notes(&doctor_strays, &doctor_unread);
        assert!(!doctor.contains("limactl"), "{case} doctor: {doctor}");

        let status = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(vm.status());
        assert!(
            status.probe_error.is_none(),
            "{case}: {:?}",
            status.probe_error
        );
        assert_eq!(status.unread, expected, "{case} status unread");
        assert!(
            status
                .strays
                .iter()
                .all(|stray| !stray.remove.contains("limactl")),
            "{case} status lima remedies: {:?}",
            status.strays
        );
        let status_text = crate::render_vm_status(&status);
        assert!(
            !status_text.contains("limactl"),
            "{case} status: {status_text}"
        );

        let facts = Facts::gather(&cfg, &vm);
        assert_eq!(facts.vm_unread, expected, "{case} uninstall unread");
        assert!(
            facts
                .vm_strays
                .iter()
                .all(|stray| !stray.remove.contains("limactl")),
            "{case} uninstall lima remedies: {:?}",
            facts.vm_strays
        );
        let keep = kept(&facts, false).join("\n");
        let epilogue = left_in_place(&facts, false);
        assert!(!keep.contains("limactl"), "{case} uninstall keep: {keep}");
        assert!(
            !epilogue.contains("limactl"),
            "{case} uninstall epilogue: {epilogue}"
        );
    }

    println!("deleted-cwd fixture completed");
}

/// Invoked by the named parent tests above.  Ignoring it in an ordinary test
/// run prevents a second, environment-free invocation from masquerading as
/// permission coverage.
#[test]
#[ignore = "permission fixture child; invoked by the parent tests"]
fn permission_fixture_child() {
    if let Some(root) = std::env::var_os(DELETED_CWD_ROOT) {
        exercise_deleted_cwd(PathBuf::from(root));
        return;
    }
    let case = Case::parse(&std::env::var(CASE_ENV).expect("permission case supplied by parent"));
    let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("fixture root supplied by parent"));
    drop_root_privileges();
    let _sandbox = crate::config::test_support::sandbox();
    exercise(case, &root);
    println!("permission fixture completed: {}", case.id());
}

fn drop_root_privileges() {
    // SAFETY: these calls only inspect and permanently narrow this disposable
    // child process's credentials; no other test runs in this exact-test child.
    if unsafe { libc::geteuid() } == 0 {
        // 65534 is the kernel's conventional overflow/nobody identity.  The
        // fixtures are world-readable up to the exact component under test.
        assert_eq!(unsafe { libc::setgid(65534) }, 0, "dropping fixture gid");
        assert_eq!(unsafe { libc::setuid(65534) }, 0, "dropping fixture uid");
        assert_ne!(unsafe { libc::geteuid() }, 0, "fixture remained root");
    }
}

fn exercise(case: Case, root: &Path) {
    match case {
        Case::LimaLeftoversCompletion => return exercise_lima_leftovers_completion(root),
        Case::LimaDiskCompletion => return exercise_lima_disk_completion(root),
        Case::FirecrackerCompletion => return exercise_firecracker_completion(root),
        Case::LimaDeniedLocal => return exercise_lima_denied_local(root),
        Case::LimaOwnInstanceSymlink | Case::LimaOwnDiskSymlink => {
            return exercise_lima_own_symlink(case, root);
        }
        _ => {}
    }
    let base = match case {
        Case::InaccessibleParent => root.join("wall/vm"),
        Case::LimaHome => root.join("lima/vm"),
        _ => root.join("vm"),
    };
    let lima_home = match case {
        Case::LimaHomeParent => root.join("lima-wall/lima"),
        _ => root.join("lima"),
    };
    let expected = expected_unread(case, root, &base);

    assert_permission_precondition(case, root, &base);

    for backend in [BackendKind::Firecracker, BackendKind::Lima] {
        let mut cfg = Config::default();
        cfg.vm.name = match case {
            Case::NestedOwnData => "new/nested",
            _ => "new",
        }
        .into();
        cfg.vm.dir = base.to_string_lossy().into_owned();
        cfg.vm.backend = Some(backend);
        // Keep survey, status and uninstall off a developer's installed
        // tooling or real Lima state. Doctor always uses the filesystem
        // reader below, regardless of whether limactl is available.
        cfg.vm.limactl = Some(root.join("missing-limactl").to_string_lossy().into_owned());
        if matches!(case, Case::InaccessibleParent) {
            cfg.herdr.projects_dir = base.to_string_lossy().into_owned();
        }
        let mut vm = Vm::new(&cfg);
        vm.lima_home = Some(lima_home.clone());

        let survey = vm.survey();
        assert_eq!(survey.unread, expected, "{backend} survey failed paths");
        assert!(
            survey.strays.is_empty(),
            "{backend} must not claim an uninspected entry"
        );
        let expected_configured = expected_configured_vm(case, backend);
        assert_eq!(
            survey.present, expected_configured.0,
            "{backend} configured VM presence"
        );
        assert_eq!(
            survey.data, expected_configured.1,
            "{backend} configured data presence"
        );

        let (doctor_strays, doctor_unread) = vm.strays_on_filesystem();
        assert_eq!(
            doctor_unread, expected,
            "{backend} doctor filesystem reader failed paths"
        );
        assert!(
            doctor_strays.is_empty(),
            "doctor must not claim an uninspected entry"
        );

        let status = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(vm.status());
        assert_eq!(status.unread, expected, "{backend} vm status failed paths");
        assert!(
            status.strays.is_empty(),
            "status must not claim an uninspected entry"
        );
        serde_json::to_value(&status).expect("non-UTF-8 unread paths serialize lossily");

        let facts = Facts::gather(&cfg, &vm);
        assert_eq!(
            facts.vm_unread, expected,
            "{backend} uninstall gather failed paths"
        );
        assert!(
            facts.vm_strays.is_empty(),
            "uninstall must not claim an uninspected entry"
        );
        assert_eq!(
            facts.vm_present, expected_configured.0,
            "{backend} gathered VM presence"
        );
        assert_eq!(
            facts.vm_data, expected_configured.1,
            "{backend} gathered data presence"
        );
        if matches!(case, Case::InaccessibleParent) {
            assert_eq!(facts.projects, vec![base.clone()]);
            let project_line = format!(
                "{} (clones and worktrees; may hold unpushed work)",
                base.display()
            );
            assert!(
                kept(&facts, false).contains(&project_line),
                "denied projects directory left the keep list"
            );
        }

        if backend == BackendKind::Lima && matches!(case, Case::InaccessibleSymlinkTarget) {
            let dir = vm.dir.display();
            assert_eq!(facts.vm_removed, format!("{dir} if it is there"));
            let mut answered = facts.clone();
            answered.ssh_answered(&vm);
            assert_eq!(
                answered.vm_removed,
                format!("the lima instance ssf-new and {dir} if it is there")
            );
        }

        assert_renderers(&status, &doctor_strays, &doctor_unread, &expected);
        assert_uninstall_lists(case, &facts, &base, &expected);
    }

    if matches!(case, Case::InaccessibleParent) {
        let inspected =
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(crate::uninstall::inspect_path(
                    base.to_string_lossy().into_owned(),
                ));
        assert_eq!(inspected.state, "unknown");
        assert_eq!(
            inspected.problems,
            vec![format!("{} could not be read", base.display())]
        );
    }
}

fn lima_vm(root: &Path, base: &Path, limactl: &Path) -> (Config, Vm) {
    let mut cfg = Config::default();
    cfg.vm.name = "new".into();
    cfg.vm.dir = base.to_string_lossy().into_owned();
    cfg.vm.backend = Some(BackendKind::Lima);
    cfg.vm.limactl = Some(limactl.to_string_lossy().into_owned());
    let mut vm = Vm::new(&cfg);
    vm.lima_home = Some(root.join("lima"));
    (cfg, vm)
}

fn assert_denied(path: &Path) {
    let error = fs::metadata(path).expect_err("fixture path unexpectedly statted");
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied,
        "{} failed for the wrong reason: {error}",
        path.display()
    );
}

fn exercise_lima_leftovers_completion(root: &Path) {
    let instance = root.join("lima/ssf-new");
    let disk = root.join("lima/_disks/ssf-new");
    assert_denied(&instance);
    assert_denied(&disk);
    let (_, vm) = lima_vm(root, &root.join("vm"), &root.join("missing-limactl"));
    let error = vm
        .lima_destroy()
        .expect_err("denied leftovers must fail completion");
    assert_eq!(
        error.to_string(),
        format!(
            "lima cannot be asked about ssf-new, and {} and {} could not be read",
            instance.display(),
            disk.display()
        )
    );
}

fn exercise_lima_disk_completion(root: &Path) {
    let disk = root.join("lima/_disks/ssf-new");
    assert_denied(&disk);
    let (_, vm) = lima_vm(root, &root.join("vm"), &root.join("limactl"));
    let error = vm
        .lima_destroy()
        .expect_err("denied disk fallback must fail completion");
    assert_eq!(
        error.to_string(),
        format!(
            "lima cannot be asked about disk ssf-new, and {} could not be confirmed absent",
            disk.display()
        )
    );
}

fn exercise_firecracker_completion(root: &Path) {
    let base = root.join("wall/vm");
    let own = base.join("new");
    assert_denied(&own);
    let mut cfg = Config::default();
    cfg.vm.name = "new".into();
    cfg.vm.dir = base.to_string_lossy().into_owned();
    cfg.vm.backend = Some(BackendKind::Firecracker);
    let vm = Vm::new(&cfg);
    let error = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(vm.destroy())
        .expect_err("denied VM directory must fail completion");
    assert_eq!(error.to_string(), format!("removing {}", own.display()));
}

fn exercise_lima_denied_local(root: &Path) {
    let base = root.join("vm");
    let (cfg, vm) = lima_vm(root, &base, &root.join("limactl"));
    assert_denied(&vm.dir);

    let survey = vm.lima_survey();
    assert_eq!((survey.present, survey.data), (None, Some(false)));
    assert_eq!(survey.unread, vec![vm.dir.clone()]);

    let public = vm.survey();
    assert_eq!((public.present, public.data), (None, Some(false)));
    assert_eq!(public.unread, vec![vm.dir.clone()]);
    let facts = Facts::gather(&cfg, &vm);
    assert_eq!((facts.vm_present, facts.vm_data), (None, Some(false)));
    assert_eq!(facts.vm_unread, vec![vm.dir.clone()]);
    assert_eq!(
        facts.vm_removed,
        format!("{} if it is there", vm.dir.display())
    );

    let mut answered = facts;
    answered.ssh_answered(&vm);
    assert_eq!(
        answered.vm_removed,
        format!(
            "the lima instance ssf-new and {} if it is there",
            vm.dir.display()
        )
    );
}

fn exercise_lima_own_symlink(case: Case, root: &Path) {
    let base = root.join("vm");
    let (cfg, vm) = lima_vm(root, &base, &root.join("limactl"));
    let unread_path = match case {
        Case::LimaOwnInstanceSymlink => root.join("lima/ssf-new"),
        Case::LimaOwnDiskSymlink => root.join("lima/_disks/ssf-new"),
        _ => unreachable!(),
    };
    let link = fs::symlink_metadata(&unread_path).unwrap();
    assert!(link.file_type().is_symlink());
    assert_denied(&unread_path);
    let expected = vec![unread_path.clone()];

    let direct = vm.lima_survey();
    assert_eq!((direct.present, direct.data), (Some(false), Some(false)));
    assert_eq!(direct.unread, expected);
    assert!(direct.strays.is_empty());

    let survey = vm.survey();
    assert_eq!((survey.present, survey.data), (Some(false), Some(false)));
    assert_eq!(survey.unread, expected);
    assert!(survey.strays.is_empty());

    let (fallback_strays, fallback_unread) = vm.strays_on_filesystem();
    assert_eq!(fallback_unread, expected);
    assert!(fallback_strays.is_empty());

    let status = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(vm.status());
    assert_eq!(status.unread, expected);
    assert!(status.strays.is_empty());

    let facts = Facts::gather(&cfg, &vm);
    assert_eq!(
        (facts.vm_present, facts.vm_data),
        (Some(false), Some(false))
    );
    assert_eq!(facts.vm_unread, expected);
    assert!(facts.vm_strays.is_empty());
    assert_renderers(&status, &fallback_strays, &fallback_unread, &expected);

    let note = unread_note(&expected);
    let keep = kept(&facts, false);
    assert!(
        keep.iter()
            .any(|line| line.starts_with(unread_path.to_string_lossy().as_ref())),
        "{keep:?}"
    );
    assert_eq!(
        crate::stray_notes(&fallback_strays, &fallback_unread),
        format!("note {note}\n")
    );
    assert!(left_in_place(&facts, false).contains(unread_path.to_string_lossy().as_ref()));
}

fn expected_configured_vm(case: Case, backend: BackendKind) -> (Option<bool>, Option<bool>) {
    match (backend, case) {
        (
            BackendKind::Firecracker,
            Case::InaccessibleParent
            | Case::VmEntries
            | Case::OwnSymlink
            | Case::LimaHome
            | Case::InaccessibleSymlinkTarget,
        ) => (None, None),
        (BackendKind::Firecracker, Case::InaccessibleData | Case::NestedOwnData) => {
            (Some(true), None)
        }
        (
            BackendKind::Firecracker,
            Case::UnreadFirecrackerStray
            | Case::LimaHomeParent
            | Case::LimaHomeEntries
            | Case::LimaDisks
            | Case::LimaDiskEntries
            | Case::DanglingSymlink,
        ) => (Some(false), Some(false)),
        (
            BackendKind::Lima,
            Case::InaccessibleParent
            | Case::VmEntries
            | Case::OwnSymlink
            | Case::InaccessibleSymlinkTarget,
        ) => (None, Some(false)),
        (BackendKind::Lima, Case::InaccessibleData | Case::NestedOwnData) => {
            (Some(true), Some(false))
        }
        (
            BackendKind::Lima,
            Case::LimaHomeParent
            | Case::LimaHome
            | Case::LimaHomeEntries
            | Case::LimaDisks
            | Case::LimaDiskEntries,
        ) => (None, None),
        (BackendKind::Lima, Case::UnreadFirecrackerStray | Case::DanglingSymlink) => {
            (Some(false), Some(false))
        }
        (
            _,
            Case::LimaLeftoversCompletion
            | Case::LimaDiskCompletion
            | Case::FirecrackerCompletion
            | Case::LimaDeniedLocal
            | Case::LimaOwnInstanceSymlink
            | Case::LimaOwnDiskSymlink,
        ) => unreachable!("specialized permission fixture"),
    }
}

fn assert_permission_precondition(case: Case, root: &Path, base: &Path) {
    let answer = match case {
        Case::InaccessibleParent | Case::InaccessibleSymlinkTarget => fs::metadata(base).map(drop),
        Case::VmEntries => fs::symlink_metadata(base.join("new")).map(drop),
        Case::InaccessibleData => {
            for name in ["new", "old"] {
                let error = fs::metadata(base.join(name).join("data.ext4"))
                    .expect_err("fixture data disk unexpectedly statted");
                assert_eq!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied,
                    "{name}/data.ext4 failed for the wrong reason: {error}"
                );
            }
            return;
        }
        Case::OwnSymlink => {
            let link = fs::symlink_metadata(base.join("new")).unwrap();
            assert!(link.file_type().is_symlink(), "own entry is a symlink");
            let error = fs::metadata(base.join("new"))
                .expect_err("inaccessible symlink target unexpectedly statted");
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            return;
        }
        Case::NestedOwnData => {
            let nested = fs::symlink_metadata(base.join("new/nested")).unwrap();
            assert!(nested.is_dir(), "configured nested VM is stattable");
            let error = fs::metadata(base.join("new/nested/data.ext4"))
                .expect_err("nested data disk unexpectedly statted");
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            return;
        }
        Case::UnreadFirecrackerStray => {
            assert_denied(&base.join("old/data.ext4"));
            assert_eq!(
                fs::metadata(base.join("new"))
                    .expect_err("configured VM unexpectedly exists")
                    .kind(),
                std::io::ErrorKind::NotFound
            );
            return;
        }
        Case::LimaHomeParent => fs::read_dir(root.join("lima-wall/lima")).map(drop),
        Case::LimaHome => fs::read_dir(root.join("lima")).map(drop),
        Case::LimaHomeEntries => fs::symlink_metadata(root.join("lima/ssf-new")).map(drop),
        Case::LimaDisks => fs::read_dir(root.join("lima/_disks")).map(drop),
        Case::LimaDiskEntries => fs::symlink_metadata(root.join("lima/_disks/ssf-new")).map(drop),
        Case::DanglingSymlink => {
            let error = fs::metadata(base).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
            return;
        }
        Case::LimaLeftoversCompletion
        | Case::LimaDiskCompletion
        | Case::FirecrackerCompletion
        | Case::LimaDeniedLocal
        | Case::LimaOwnInstanceSymlink
        | Case::LimaOwnDiskSymlink => unreachable!("specialized permission fixture"),
    };
    let error = answer.expect_err("fixture operation unexpectedly succeeded");
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied,
        "fixture failed for the wrong reason: {error}"
    );
}

fn expected_unread(case: Case, root: &Path, base: &Path) -> Vec<PathBuf> {
    let mut paths = match case {
        Case::InaccessibleParent | Case::InaccessibleSymlinkTarget => vec![base.to_path_buf()],
        Case::VmEntries => vec![base.join("new"), base.join("old"), base.join(odd_name())],
        Case::InaccessibleData => {
            vec![base.join("new/data.ext4"), base.join("old/data.ext4")]
        }
        Case::OwnSymlink => vec![base.join("new")],
        Case::NestedOwnData => vec![base.join("new/nested/data.ext4")],
        Case::UnreadFirecrackerStray => vec![base.join("old/data.ext4")],
        Case::LimaHomeParent => vec![root.join("lima-wall/lima")],
        Case::LimaHome => vec![root.join("lima")],
        Case::LimaHomeEntries => vec![
            root.join("lima/_disks"),
            root.join("lima/ssf-new"),
            root.join("lima/ssf-old"),
            root.join("lima").join(odd_name()),
        ],
        Case::LimaDisks => vec![root.join("lima/_disks")],
        Case::LimaDiskEntries => vec![
            root.join("lima/_disks/ssf-new"),
            root.join("lima/_disks/ssf-old"),
            root.join("lima/_disks").join(odd_name()),
        ],
        Case::DanglingSymlink => Vec::new(),
        Case::LimaLeftoversCompletion
        | Case::LimaDiskCompletion
        | Case::FirecrackerCompletion
        | Case::LimaDeniedLocal
        | Case::LimaOwnInstanceSymlink
        | Case::LimaOwnDiskSymlink => unreachable!("specialized permission fixture"),
    };
    paths.sort();
    paths
}

fn assert_renderers(
    status: &super::VmStatus,
    doctor_strays: &[super::Stray],
    doctor_unread: &[PathBuf],
    expected: &[PathBuf],
) {
    let status_text = crate::render_vm_status(status);
    let doctor_text = crate::stray_notes(doctor_strays, doctor_unread);
    if expected.is_empty() {
        assert!(!status_text.contains("unread:"), "{status_text}");
        assert!(doctor_text.is_empty(), "{doctor_text}");
    } else {
        let note = unread_note(expected);
        assert!(
            status_text.contains(&format!("unread:   {note}")),
            "{status_text}"
        );
        assert_eq!(doctor_text, format!("note {note}\n"));
    }
}

fn assert_uninstall_lists(case: Case, facts: &Facts, base: &Path, expected: &[PathBuf]) {
    let keep = kept(facts, false);
    let epilogue = left_in_place(facts, false);
    for path in expected {
        let shown = path.to_string_lossy();
        assert!(
            keep.iter().any(|line| line.contains(shown.as_ref())),
            "{keep:?}"
        );
        assert!(epilogue.contains(shown.as_ref()), "{epilogue}");
    }

    let base_line = keep
        .iter()
        .find(|line| line.starts_with(base.to_string_lossy().as_ref()));
    match case {
        Case::DanglingSymlink
        | Case::LimaHomeParent
        | Case::LimaHomeEntries
        | Case::LimaDisks
        | Case::LimaDiskEntries => {
            assert!(
                base_line.is_none(),
                "a missing keep gate was listed: {keep:?}"
            );
            assert!(!facts.vm_base_may_exist);
        }
        _ => {
            assert!(facts.vm_base_may_exist);
            if expected
                .iter()
                .any(|path| path.starts_with(base) || base.starts_with(path))
            {
                let line = base_line.expect("present or unread base stays on keep list");
                assert!(!line.contains("safe to remove"), "{line}");
            }
        }
    }

    if matches!(
        case,
        Case::VmEntries | Case::InaccessibleData | Case::OwnSymlink | Case::NestedOwnData
    ) {
        let own = match case {
            Case::NestedOwnData => base.join("new/nested"),
            _ => base.join("new"),
        }
        .to_string_lossy()
        .into_owned();
        assert!(
            keep.iter()
                .filter(|line| line.contains(&own))
                .all(|line| !line.contains("untouched")),
            "the configured VM directory is removed by destroy: {keep:?}"
        );
        assert!(
            !epilogue.contains(&format!("{own} (untouched")),
            "{epilogue}"
        );
    }
}
