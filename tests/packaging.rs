//! The packaging manifests must ship every file under `vm/`.
//!
//! `ssf vm build` runs `vm/make-base.sh` from the installed
//! `/usr/share/ssf/vm/`, and that script installs the guest scripts, the
//! units and their drop-ins from `<share>/vm/guest/`. A guest file that a
//! manifest does not ship is a broken `ssf vm build` on every packaged
//! host, which is what happened when `vm/guest/seed-common.sh` and
//! `vm/guest/units/lima/` were added: the two Linux manifests listed the
//! guest files one by one, and the new ones were not on the list.
//!
//! So both manifests now install the tree wholesale, and these tests hold
//! them to it: no enumeration of `vm/` paths that a new file can fall out
//! of, plus (for the rpm, whose packager only owns what is listed) a
//! `type: dir` entry for every directory under `vm/`.

use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const PKGBUILD: &str = "packaging/release/PKGBUILD";
const NFPM: &str = "packaging/linux/nfpm.yaml";

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo().join(rel)).unwrap_or_else(|e| panic!("reading {rel}: {e}"))
}

/// Every file and directory under `vm/`, as repository-relative paths.
fn vm_tree() -> (Vec<String>, Vec<String>) {
    let mut files = Vec::new();
    let mut dirs = vec!["vm".to_string()];
    walk(&repo().join("vm"), "vm", &mut files, &mut dirs);
    files.sort();
    dirs.sort();
    (files, dirs)
}

fn walk(dir: &Path, rel: &str, files: &mut Vec<String>, dirs: &mut Vec<String>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|e| e.expect("directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let child = format!("{rel}/{name}");
        if path.is_dir() {
            dirs.push(child.clone());
            walk(&path, &child, files, dirs);
        } else {
            files.push(child);
        }
    }
}

/// The walk itself has to be real: if this ever comes back empty the other
/// tests would pass on nothing.
#[test]
fn the_vm_tree_is_what_the_guest_needs() {
    let (files, dirs) = vm_tree();
    for expected in [
        "vm/make-base.sh",
        "vm/guest/provision.sh",
        "vm/guest/seed.sh",
        "vm/guest/seed-common.sh",
        "vm/guest/units/ssf-seed.service",
    ] {
        assert!(
            files.iter().any(|f| f == expected),
            "{expected} is missing from the repository; the packaging guard walks vm/ and found {files:?}"
        );
    }
    assert!(dirs.iter().any(|d| d == "vm/guest/units"));
}

/// The PKGBUILD installs the tree with a loop, not a list of files.
#[test]
fn pkgbuild_installs_the_whole_vm_tree() {
    let pkgbuild = read(PKGBUILD);
    let how = format!(
        "install every file under vm/ in package() in {PKGBUILD} with\n\
         \x20 while IFS= read -r -d '' f; do\n\
         \x20   if [ -x \"$f\" ]; then _m=755; else _m=644; fi\n\
         \x20   install -Dm$_m \"$f\" \"$pkgdir/usr/share/ssf/$f\"\n\
         \x20 done < <(find vm -type f -print0)\n\
         so that a new guest file, unit or drop-in ships without a line of its own"
    );
    assert!(
        pkgbuild.contains("find vm -type f -print0"),
        "{PKGBUILD} does not walk vm/: {how}"
    );
    assert!(
        pkgbuild.contains(r#"install -Dm$_m "$f" "$pkgdir/usr/share/ssf/$f""#),
        "{PKGBUILD} walks vm/ but does not install what it finds under /usr/share/ssf/: {how}"
    );
    // An `install` line naming a path inside vm/ is exactly the enumeration
    // that went stale; the loop above replaces all of them.
    let stale: Vec<_> = pkgbuild
        .lines()
        .filter(|l| {
            let l = l.trim_start();
            !l.starts_with('#') && l.starts_with("install ") && l.contains("vm/")
        })
        .collect();
    assert!(
        stale.is_empty(),
        "{PKGBUILD} still installs vm/ paths one by one, which is what a new file falls out of:\n{}\n\nDelete those lines; the loop covers them ({how})",
        stale.join("\n")
    );
}

/// nfpm ships the tree with one directory entry (nfpm expands a directory
/// `src` recursively and keeps each file's mode from the working tree).
#[test]
fn nfpm_ships_the_whole_vm_tree() {
    let nfpm = read(NFPM);
    let how = format!(
        "give {NFPM} one contents entry\n\
         \x20 - src: vm\n\
         \x20   dst: /usr/share/ssf/vm\n\
         (nfpm copies a directory src recursively, with each file's own mode) \
         so that a new guest file, unit or drop-in ships without an entry of its own"
    );
    assert!(
        nfpm.contains("- src: vm\n    dst: /usr/share/ssf/vm\n"),
        "{NFPM} has no whole-tree entry for vm/: {how}"
    );
    let stale: Vec<_> = nfpm
        .lines()
        .filter(|l| {
            let l = l.trim_start();
            !l.starts_with('#') && l.starts_with("- src: vm/")
        })
        .collect();
    assert!(
        stale.is_empty(),
        "{NFPM} still lists vm/ files one by one, which is what a new file falls out of:\n{}\n\nDelete those entries; the directory entry covers them ({how})",
        stale.join("\n")
    );
}

/// nfpm's rpm packager only owns the directories it is told about, so every
/// directory under `vm/` needs a `type: dir` entry to be removed on erase.
#[test]
fn nfpm_owns_every_vm_directory_in_the_rpm() {
    let nfpm = read(NFPM);
    let (_, dirs) = vm_tree();
    let missing: Vec<_> = dirs
        .iter()
        .filter(|d| !nfpm.contains(&format!("- dst: /usr/share/ssf/{d}\n    type: dir\n")))
        .collect();
    assert!(
        missing.is_empty(),
        "{NFPM} does not own {missing:?} in the rpm; add to contents:, next to the other directories,\n\
         \x20 - dst: /usr/share/ssf/<dir>\n\
         \x20   type: dir\n\
         \x20   packager: rpm\n\
         for each (nfpm's rpm packager only owns what is listed, so an unlisted directory is left behind on erase)"
    );
}
