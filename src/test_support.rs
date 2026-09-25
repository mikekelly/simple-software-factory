//! Helpers shared by tests across the crate.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Write a script a test will then run, such as a fake herdr or gh.
///
/// `std::fs::write` would leave this process holding the file open for
/// writing, and a test on another thread that forks meanwhile gives its
/// child a copy of that descriptor until the child execs. Running the
/// script in that window fails with "Text file busy" (ETXTBSY), which is
/// how #523 flaked. A child process writes the file instead, so this
/// process never holds it open for writing at all.
pub(crate) fn write_executable(path: &Path, script: impl AsRef<[u8]>) {
    let mut child = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawning sh to write a test executable");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_ref())
        .unwrap();
    assert!(
        child.wait().unwrap().success(),
        "writing {}",
        path.display()
    );
}
