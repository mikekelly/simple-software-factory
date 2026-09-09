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

#[derive(Clone, Copy)]
enum Case {
    InaccessibleParent,
    VmEntries,
    InaccessibleData,
    LimaHomeParent,
    LimaHome,
    LimaHomeEntries,
    LimaDisks,
    LimaDiskEntries,
    DanglingSymlink,
    InaccessibleSymlinkTarget,
}

impl Case {
    fn id(self) -> &'static str {
        match self {
            Self::InaccessibleParent => "inaccessible-parent",
            Self::VmEntries => "vm-entries",
            Self::InaccessibleData => "inaccessible-data",
            Self::LimaHomeParent => "lima-home-parent",
            Self::LimaHome => "lima-home",
            Self::LimaHomeEntries => "lima-home-entries",
            Self::LimaDisks => "lima-disks",
            Self::LimaDiskEntries => "lima-disk-entries",
            Self::DanglingSymlink => "dangling-symlink",
            Self::InaccessibleSymlinkTarget => "inaccessible-symlink-target",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "inaccessible-parent" => Self::InaccessibleParent,
            "vm-entries" => Self::VmEntries,
            "inaccessible-data" => Self::InaccessibleData,
            "lima-home-parent" => Self::LimaHomeParent,
            "lima-home" => Self::LimaHome,
            "lima-home-entries" => Self::LimaHomeEntries,
            "lima-disks" => Self::LimaDisks,
            "lima-disk-entries" => Self::LimaDiskEntries,
            "dangling-symlink" => Self::DanglingSymlink,
            "inaccessible-symlink-target" => Self::InaccessibleSymlinkTarget,
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
fn stattable_vm_entry_names_its_inaccessible_data_disk() {
    run(Case::InaccessibleData);
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

/// Invoked by the named parent tests above.  Ignoring it in an ordinary test
/// run prevents a second, environment-free invocation from masquerading as
/// permission coverage.
#[test]
#[ignore = "permission fixture child; invoked by the parent tests"]
fn permission_fixture_child() {
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
    let base = match case {
        Case::InaccessibleParent => root.join("wall/vm"),
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
        cfg.vm.name = "new".into();
        cfg.vm.dir = base.to_string_lossy().into_owned();
        cfg.vm.backend = Some(backend);
        // Force Lima through the on-disk fallback without consulting a
        // developer's installed tooling or real Lima state.
        cfg.vm.limactl = Some(root.join("missing-limactl").to_string_lossy().into_owned());
        let mut vm = Vm::new(&cfg);
        vm.lima_home = Some(lima_home.clone());

        let survey = vm.survey();
        assert_eq!(survey.unread, expected, "{backend} survey failed paths");
        assert!(
            survey.strays.is_empty(),
            "{backend} must not claim an uninspected entry"
        );
        if backend == BackendKind::Lima && lima_access_is_denied(case) {
            assert_eq!(survey.data, None, "denied Lima data presence stays unknown");
        }

        let (fallback_strays, fallback_unread) = vm.strays_on_filesystem();
        assert_eq!(
            fallback_unread, expected,
            "{backend} doctor fallback failed paths"
        );
        assert!(
            fallback_strays.is_empty(),
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
        if backend == BackendKind::Lima && lima_access_is_denied(case) {
            assert_eq!(facts.vm_data, None, "uninstall keeps denied data unknown");
        }

        assert_renderers(&status, &fallback_strays, &fallback_unread, &expected);
        assert_uninstall_lists(case, &facts, &base, &expected);
    }
}

fn lima_access_is_denied(case: Case) -> bool {
    matches!(
        case,
        Case::LimaHomeParent
            | Case::LimaHome
            | Case::LimaHomeEntries
            | Case::LimaDisks
            | Case::LimaDiskEntries
    )
}

fn assert_permission_precondition(case: Case, root: &Path, base: &Path) {
    let answer = match case {
        Case::InaccessibleParent | Case::InaccessibleSymlinkTarget => fs::metadata(base).map(drop),
        Case::VmEntries => fs::symlink_metadata(base.join("new")).map(drop),
        Case::InaccessibleData => fs::metadata(base.join("old/data.ext4")).map(drop),
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
        Case::InaccessibleData => vec![base.join("old/data.ext4")],
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
        | Case::LimaHome
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
            if expected.iter().any(|path| path.starts_with(base)) {
                let line = base_line.expect("present or unread base stays on keep list");
                assert!(!line.contains("safe to remove"), "{line}");
            }
        }
    }

    if matches!(case, Case::VmEntries) {
        let own = base.join("new").to_string_lossy().into_owned();
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
