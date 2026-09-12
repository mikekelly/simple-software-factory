//! The legacy herdr action delegates to the packaged client and releases its slot.
#[cfg(unix)]
#[test]
fn compatibility_action_launches_client_with_inherited_remote_selection() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    struct TempDir(std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let dir = TempDir(std::env::temp_dir().join(format!(
        "ssf-herdr-dashboard-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )));
    std::fs::create_dir(&dir.0).unwrap();
    let executable = dir.0.join("ssf");
    let result = dir.0.join("arguments");
    std::fs::write(
        &executable,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" \"$SSF_SERVER\" > \"$DASHBOARD_TEST_RESULT\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = std::process::Command::new("sh")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/herdr-plugin/dashboard.sh"
        ))
        .env("PATH", format!("{}:/usr/bin:/bin", dir.0.display()))
        .env("SSF_SERVER", "customer@factory.example")
        .env("DASHBOARD_TEST_RESULT", &result)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if std::fs::read_to_string(&result).unwrap_or_default()
            == "dashboard\ncustomer@factory.example\n"
        {
            break;
        }
        assert!(Instant::now() < deadline, "client was not launched");
        std::thread::sleep(Duration::from_millis(10));
    }
}
