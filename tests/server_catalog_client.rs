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

    let output = root
        .client()
        .args(["--server", "cloud", "vm", "status"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("not a managed VM")
    );
    assert!(!root.0.join("ssh-ran").exists());

    let output = root
        .client()
        .args(["--server", "local", "vm", "status"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!root.0.join("local-ran").exists());

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
            .contains("not a managed VM")
    );
}

#[test]
fn migrated_vm_configuration_drives_the_selected_endpoint() {
    let root = Temp::new("vm-migration");
    let vm_dir = root.0.join("vm-storage");
    std::fs::write(
        root.0.join("config/config.toml"),
        format!(
            "[vm]\nenabled = true\nname = \"crucible\"\ndir = {:?}\nbackend = \"firecracker\"\nssh_port = 2444\n",
            vm_dir
        ),
    )
    .unwrap();
    root.use_real_server();

    let output = root
        .client()
        .args(["server", "migrate-vm"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let factory_config = std::fs::read_to_string(root.0.join("config/config.toml")).unwrap();
    assert!(!factory_config.contains("[vm]"), "{factory_config}");
    let catalog = std::fs::read_to_string(root.0.join("config/servers.toml")).unwrap();
    assert!(catalog.contains("[servers.ssf-server.config]"), "{catalog}");
    assert!(catalog.contains("ssh_port = 2444"), "{catalog}");

    let output = root
        .client()
        .args(["--server", "ssf-server", "vm", "status", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["name"], "crucible");
    assert_eq!(status["ssh_port"], 2444);

    let output = root
        .client()
        .args([
            "--server",
            "ssf-server",
            "config",
            "set",
            "vm.ssh_port",
            "2555",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let factory_config = std::fs::read_to_string(root.0.join("config/config.toml")).unwrap();
    assert!(!factory_config.contains("[vm]"), "{factory_config}");
    let catalog = std::fs::read_to_string(root.0.join("config/servers.toml")).unwrap();
    assert!(catalog.contains("ssh_port = 2555"), "{catalog}");

    let output = root
        .client()
        .args(["--server", "ssf-server", "uninstall", "--report"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("uninstall is not yet target-aware")
    );

    let output = root
        .client()
        .args(["server", "migrate-vm"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("already migrated")
    );
}

#[test]
fn two_owned_vms_are_selected_independently() {
    let root = Temp::new("two-owned-vms");
    let crucible_dir = root.0.join("vm-crucible");
    let factory_dir = root.0.join("vm-factory");
    root.catalog(&format!(
        "[servers.crucible]\ntransport = \"vm\"\nruntime_name = \"crucible\"\n[servers.crucible.config]\nenabled = true\nname = \"crucible\"\ndir = {:?}\nssh_port = 2444\n\n[servers.ssf-server]\ntransport = \"vm\"\nruntime_name = \"factory\"\n[servers.ssf-server.config]\nenabled = true\nname = \"factory\"\ndir = {:?}\nssh_port = 2555\n",
        crucible_dir, factory_dir
    ));
    root.use_real_server();

    let ambiguous = root
        .client()
        .args(["vm", "status", "--json"])
        .output()
        .unwrap();
    assert!(!ambiguous.status.success());
    assert!(
        String::from_utf8_lossy(&ambiguous.stderr).contains("multiple SSF servers are configured")
    );

    for (server, runtime, port) in [
        ("crucible", "crucible", 2444),
        ("ssf-server", "factory", 2555),
    ] {
        let output = root
            .client()
            .args(["--server", server, "vm", "status", "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(status["server"], server);
        assert_eq!(status["name"], runtime);
        assert_eq!(status["ssh_port"], port);
    }

    let output = root
        .client()
        .args(["--server", "crucible", "status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["server"], "crucible");
    assert_eq!(status["transport"], "vm");
    assert_eq!(status["host_vm"]["server"], "crucible");

    let output = root
        .client()
        .args(["--server", "crucible", "ui", "service", "status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["server"], "crucible");
    assert_eq!(status["unit"], "ssf@crucible.service");
}

#[test]
fn a_named_local_service_uses_only_its_target_unit() {
    let root = Temp::new("target-service");
    let config = root.0.join("factory/config");
    let state = root.0.join("factory/state");
    root.catalog(&format!(
        "[servers.local]\ntransport = \"local\"\nconfig_dir = {:?}\nstate_dir = {:?}\n",
        config, state
    ));
    root.use_real_server();
    script(
        &root.0.join("systemctl"),
        "printf '%s\\n' \"$*\" >> \"$TEST_ROOT/systemctl-args\"\ncase \"$*\" in *is-enabled*ssf@local.service*) exit 0;; *is-active*ssf@local.service*) exit 0;; *is-failed*) exit 1;; *is-enabled*ssf.service*|*is-active*ssf.service*) exit 1;; esac",
    );

    let output = root
        .client()
        .args(["--server", "local", "ui", "service", "status", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["server"], "local");
    assert_eq!(status["unit"], "ssf@local.service");

    let output = root
        .client()
        .args(["--server", "local", "status", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["server"], "local");
    assert_eq!(status["transport"], "local");

    let output = root
        .client()
        .args(["--server", "local", "ui", "service", "enable"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let calls = std::fs::read_to_string(root.0.join("systemctl-args")).unwrap();
    assert!(
        calls.contains("--user enable --now ssf@local.service"),
        "{calls}"
    );
    assert!(!calls.contains("enable --now ssf.service"), "{calls}");

    script(
        &root.0.join("systemctl"),
        "printf '%s\\n' \"$*\" >> \"$TEST_ROOT/refusal-calls\"\ncase \"$*\" in *is-active*ssf.service*) exit 0;; *is-enabled*ssf.service*) exit 0;; esac",
    );
    let output = root
        .client()
        .args(["--server", "local", "ui", "service", "enable"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("legacy singleton service is still"));
    let calls = std::fs::read_to_string(root.0.join("refusal-calls")).unwrap();
    assert!(!calls.contains("enable --now ssf@local.service"), "{calls}");
}

#[test]
fn server_target_is_resolved_before_factory_configuration() {
    let root = Temp::new("server-target-context");
    let config = root.0.join("factory/config");
    let state = root.0.join("factory/state");
    std::fs::create_dir_all(&config).unwrap();
    root.catalog(&format!(
        "[servers.local]\ntransport = \"local\"\nconfig_dir = {:?}\nstate_dir = {:?}\n\n[servers.cloud]\ntransport = \"ssh\"\ndestination = \"cloud.example\"\n",
        config, state
    ));
    std::fs::write(root.0.join("config/config.toml"), "ambient = [broken").unwrap();
    std::fs::write(config.join("config.toml"), "selected = [broken").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_ssf-server"))
        .env("SSF_CONFIG_DIR", root.0.join("config"))
        .env("SSF_STATE_DIR", root.0.join("state"))
        .args(["--target", "local", "--once"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains(&config.join("config.toml").to_string_lossy().to_string()),
        "{error}"
    );
    assert!(!error.contains("ambient ="), "{error}");

    let output = Command::new(env!("CARGO_BIN_EXE_ssf-server"))
        .env("SSF_CONFIG_DIR", root.0.join("config"))
        .env("SSF_STATE_DIR", root.0.join("state"))
        .args(["--target", "cloud"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot have a service on this host"));
}
