use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Temp(PathBuf);

impl Temp {
    fn new(label: &str) -> Self {
        let path = Path::new(env!("CARGO_BIN_EXE_ssf"))
            .parent()
            .unwrap()
            .join(format!(
                "ssf-server-catalog-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(path.join("config")).unwrap();
        std::fs::hard_link(env!("CARGO_BIN_EXE_ssf"), path.join("ssf")).unwrap();
        Self(path)
    }

    fn client(&self) -> Command {
        let mut command = Command::new(self.0.join("ssf"));
        command
            .env("PATH", &self.0)
            .env("TEST_ROOT", &self.0)
            .env("SSF_CONFIG_DIR", self.0.join("config"))
            .env("SSF_STATE_DIR", self.0.join("state"))
            .env_remove("SSF_SERVER");
        command
    }

    fn catalog(&self, body: &str) {
        std::fs::write(self.0.join("config/servers.toml"), body).unwrap();
    }

    fn use_real_server(&self) {
        std::fs::hard_link(env!("CARGO_BIN_EXE_ssf-server"), self.0.join("ssf-server")).unwrap();
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn catalog_commands_are_client_wide_and_sorted() {
    let root = Temp::new("list");
    root.catalog(
        "[servers.ssf-server]\ntransport = \"vm\"\n\n[servers.cloud]\ntransport = \"ssh\"\ndestination = \"person@cloud.example\"\n",
    );
    let output = root
        .client()
        .env("SSF_SERVER", "cloud")
        .args(["server", "list"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "cloud\tssh\nssf-server\tvm\n"
    );

    let output = root
        .client()
        .args(["server", "show", "cloud", "--json"])
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["name"], "cloud");
    assert_eq!(value["transport"], "ssh");
    assert_eq!(value["destination"], "person@cloud.example");
}

#[test]
fn one_named_ssh_server_is_implicit_and_an_explicit_name_overrides_the_environment() {
    let root = Temp::new("ssh");
    root.catalog("[servers.cloud]\ntransport = \"ssh\"\ndestination = \"person@cloud.example\"\n");
    script(
        &root.0.join("ssh"),
        "printf '%s\\n' \"$@\" > \"$TEST_ROOT/ssh-args\"",
    );

    let status = root.client().arg("status").status().unwrap();
    assert!(status.success());
    let args = std::fs::read_to_string(root.0.join("ssh-args")).unwrap();
    assert!(
        args.contains("person@cloud.example\nssf-server __client 'status'"),
        "{args}"
    );

    let status = root
        .client()
        .env("SSF_SERVER", "misspelled")
        .args(["--server", "cloud", "status"])
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn several_servers_require_selection_before_starting_a_transport() {
    let root = Temp::new("ambiguous");
    root.catalog(
        "[servers.local]\ntransport = \"local\"\n\n[servers.cloud]\ntransport = \"ssh\"\ndestination = \"cloud.example\"\n",
    );
    script(&root.0.join("ssf-server"), ": > \"$TEST_ROOT/local-ran\"");
    script(&root.0.join("ssh"), ": > \"$TEST_ROOT/ssh-ran\"");

    let output = root.client().arg("status").output().unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains("multiple SSF servers are configured"),
        "{error}"
    );
    assert!(error.contains("cloud\n  local"), "{error}");
    assert!(!root.0.join("local-ran").exists());
    assert!(!root.0.join("ssh-ran").exists());

    let status = root
        .client()
        .args(["--server", "local", "status"])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(root.0.join("local-ran").exists());
}

#[test]
fn unknown_catalog_names_are_not_used_as_ssh_destinations() {
    let root = Temp::new("unknown");
    root.catalog("[servers.local]\ntransport = \"local\"\n");
    script(&root.0.join("ssh"), ": > \"$TEST_ROOT/ssh-ran\"");

    let output = root
        .client()
        .args(["--server", "typo.example", "status"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("unknown SSF server")
    );
    assert!(!root.0.join("ssh-ran").exists());
}

#[test]
fn namespaced_local_servers_pass_distinct_config_and_state_contexts() {
    let root = Temp::new("local-context");
    let one_config = root.0.join("one/config");
    let one_state = root.0.join("one/state");
    let two_config = root.0.join("two/config");
    let two_state = root.0.join("two/state");
    root.catalog(&format!(
        "[servers.one]\ntransport = \"local\"\nconfig_dir = {:?}\nstate_dir = {:?}\n\n[servers.two]\ntransport = \"local\"\nconfig_dir = {:?}\nstate_dir = {:?}\n",
        one_config, one_state, two_config, two_state
    ));
    // The selected target must not parse or fall back to the ambient factory.
    std::fs::write(root.0.join("config/config.toml"), "not = [valid").unwrap();
    root.use_real_server();

    for (name, expected) in [("one", &one_config), ("two", &two_config)] {
        let output = root
            .client()
            .args(["--server", name, "config", "path"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            expected.join("config.toml").to_string_lossy()
        );
    }

    let output = root
        .client()
        .args([
            "--server",
            "one",
            "config",
            "set",
            "daemon.poll_interval_secs",
            "31",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(one_config.join("config.toml").is_file());
    assert!(!two_config.join("config.toml").exists());

    let output = root
        .client()
        .args(["--server", "one", "vm", "status"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("installation-wide service or VM")
    );
}
