//! The optional action launches the ordinary TUI in a normal Herdr tab.
#[cfg(unix)]
#[test]
fn shortcut_creates_a_tab_and_runs_client_with_inherited_remote_selection() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("ssf-herdr-dashboard-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for (name, body) in [
        ("ssf", "exit 0"),
        (
            "herdr",
            r#"printf '%s\n' "$@" >> "$TEST_ROOT/arguments"
if [ "$1" = tab ]; then printf '%s\n' '{"result":{"root_pane":{"pane_id":"w9:p7"}}}'; fi"#,
        ),
    ] {
        let path = root.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = std::process::Command::new("sh")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/herdr-plugin/dashboard.sh"
        ))
        .env("PATH", format!("{}:/usr/bin:/bin", root.display()))
        .env("TEST_ROOT", &root)
        .env("SSF_SERVER", "customer@factory.example")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let args = std::fs::read_to_string(root.join("arguments")).unwrap();
    assert_eq!(
        args,
        "tab\ncreate\n--label\nSSF dashboard\n--focus\n--env\nSSF_SERVER=customer@factory.example\npane\nrun\nw9:p7\nssf dashboard\n"
    );
    std::fs::remove_dir_all(root).unwrap();
}
