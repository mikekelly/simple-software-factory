//! The daemon runs as `ssf-server`, and its clones use the credential
//! helper string `unattended()` sets for it (#682). Run that string the way
//! git does, against the built binaries: it must reach a `git-credential`
//! subcommand rather than fail on an unknown argument.

#[cfg(unix)]
#[test]
fn daemon_credential_helper_runs() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let server = std::path::Path::new(env!("CARGO_BIN_EXE_ssf-server"));
    let helper = ssf::credential_helper(server);
    let command = helper.strip_prefix('!').expect("a shell helper");
    let home = std::env::temp_dir().join(format!("ssf-682-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(format!("{command} get"))
        .env("HOME", home.as_path())
        .env("XDG_CONFIG_HOME", home.as_path().join(".config"))
        .env_remove("SSF_CONFIG_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Another host: the helper answers nothing, and reads no token.
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"protocol=https\nhost=example.invalid\n\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&home);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{helper} failed: {stderr}");
    assert!(stderr.is_empty(), "{helper} complained: {stderr}");
}
