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
fn catalog_adds_isolated_targets_and_remove_only_forgets_them() {
    let root = Temp::new("catalog-mutations");
    #[cfg(target_os = "linux")]
    script(&root.0.join("systemctl"), "exit 1");
    #[cfg(target_os = "macos")]
    script(&root.0.join("launchctl"), "exit 1");

    let output = root
        .client()
        .args([
            "server",
            "add",
            "local",
            "--local",
            "--config-dir",
            root.0.join("local-config").to_str().unwrap(),
            "--state-dir",
            root.0.join("local-state").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shown = root
        .client()
        .args(["server", "show", "local", "--json"])
        .output()
        .unwrap();
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["transport"], "local");
    assert_eq!(shown["implicit"], true);
    assert_eq!(
        shown["config_dir"],
        root.0.join("local-config").to_str().unwrap()
    );

    let vm_dir = root.0.join("vms/crucible");
    let output = root
        .client()
        .args([
            "server",
            "add",
            "crucible",
            "--vm",
            "--runtime-name",
            "crucib",
            "--vm-dir",
            vm_dir.to_str().unwrap(),
            "--ssh-port",
            "42322",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("require `--server NAME`"));

    let output = root.client().arg("status").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("multiple SSF servers"));

    let output = root
        .client()
        .args(["server", "remove", "crucible"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("was not deleted"), "{text}");
    assert!(text.contains(vm_dir.to_str().unwrap()), "{text}");
    assert!(
        !vm_dir.exists(),
        "catalog mutation must not create or delete VM data"
    );

    let list = root
        .client()
        .args(["server", "list", "--json"])
        .output()
        .unwrap();
    let list: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["name"], "local");
    assert_eq!(list[0]["implicit"], true);
}

#[cfg(target_os = "linux")]
#[test]
fn catalog_remove_refuses_a_live_target_service() {
    let root = Temp::new("catalog-remove-live");
    root.catalog("[servers.local]\ntransport = \"local\"\n");
    script(
        &root.0.join("systemctl"),
        "case \"$*\" in *is-active*) exit 0;; *) exit 1;; esac",
    );
    let output = root
        .client()
        .args(["server", "remove", "local"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("ui service disable"));
    assert!(root.0.join("config/servers.toml").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn catalog_remove_fails_closed_when_service_state_cannot_be_inspected() {
    let root = Temp::new("catalog-remove-uninspectable");
    root.catalog("[servers.local]\ntransport = \"local\"\n");
    let output = root
        .client()
        .args(["server", "remove", "local"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("checking ssf@local.service"));
    assert!(root.0.join("config/servers.toml").exists());
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
fn doctor_reports_the_invoking_client_and_selected_server_versions() {
    let root = Temp::new("doctor-version");
    let config = root.0.join("factory/config");
    let state = root.0.join("factory/state");
    root.catalog(&format!(
        "[servers.work]\ntransport = \"local\"\nconfig_dir = {:?}\nstate_dir = {:?}\n",
        config, state
    ));
    root.use_real_server();

    let output = root
        .client()
        .args(["--server", "work", "doctor"])
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.starts_with(&format!(
            "ok   client version {}; server \"work\" version {}\n",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_VERSION")
        )),
        "{text}"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_ssf-server"))
        .env("PATH", &root.0)
        .env("SSF_CONFIG_DIR", &config)
        .env("SSF_STATE_DIR", &state)
        .env("SSF_INTERNAL_CLIENT_VERSION", "0.6.9")
        .env(
            "SSF_INTERNAL_SELECTED_TARGET",
            r#"{"name":"work","transport":"local"}"#,
        )
        .args(["__client", "doctor"])
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.starts_with(&format!(
            "FAIL client version 0.6.9; server \"work\" version {}; update the client or server to the same release and restart the server",
            env!("CARGO_PKG_VERSION")
        )),
        "{text}"
    );
}

#[test]
fn doctor_detects_a_server_from_before_the_version_exchange() {
    let root = Temp::new("old-doctor-version");
    root.catalog("[servers.old]\ntransport = \"local\"\n");
    script(
        &root.0.join("ssf-server"),
        "case \"${1:-}\" in\n  __target-version) echo \"unknown argument __target-version\" >&2; exit 2 ;;\n  --version) echo 'ssf-server 0.6.9' ;;\n  __client) echo 'all good' ;;\nesac",
    );

    let output = root
        .client()
        .args(["--server", "old", "doctor"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.starts_with(&format!(
            "FAIL client version {}; server \"old\" version 0.6.9;",
            env!("CARGO_PKG_VERSION")
        )),
        "{text}"
    );
    assert!(text.ends_with("all good\n"), "{text}");
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

#[cfg(target_os = "linux")]
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

/// A session's pane belongs to a factory it cannot look up: the daemon names it
/// in `SSF_INTERNAL_SELECTED_TARGET`, and `SSF_CONFIG_DIR` is that factory's own
/// directory, which holds no `servers.toml`. A command run there must still
/// answer for it, or the service check asks after the singleton and reports a
/// running factory as a stopped service (#428).
#[cfg(target_os = "linux")]
#[test]
fn a_pane_answers_for_the_factory_the_daemon_named() {
    let root = Temp::new("inherited-target");
    root.use_real_server();
    script(
        &root.0.join("systemctl"),
        "printf '%s\\n' \"$*\" >> \"$TEST_ROOT/systemctl-args\"\ncase \"$*\" in *is-enabled*ssf@local.service*|*is-active*ssf@local.service*) exit 0;; *is-failed*) exit 1;; *is-enabled*ssf.service*|*is-active*ssf.service*) exit 1;; esac",
    );
    let identity = r#"{"name":"local","transport":"local"}"#;
    let pane = || {
        let mut command = root.client();
        command.env("SSF_INTERNAL_SELECTED_TARGET", identity);
        command
    };

    let output = pane()
        .args(["ui", "service", "status", "--json"])
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
    assert_eq!(status["active"], true);
    assert_eq!(status["enabled"], true);

    let output = pane().args(["ui", "service", "enable"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = std::fs::read_to_string(root.0.join("systemctl-args")).unwrap();
    assert!(
        calls.contains("--user enable --now ssf@local.service"),
        "{calls}"
    );
    assert!(!calls.contains("enable --now ssf.service"), "{calls}");

    // Nothing inherited: still the singleton, with no target invented for it.
    let output = root
        .client()
        .env_remove("SSF_INTERNAL_SELECTED_TARGET")
        .args(["ui", "service", "status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(status["server"].is_null());
    assert_eq!(status["unit"], "ssf.service");
    assert_eq!(status["active"], false);
}

/// A pane inherits one factory's identity, but an explicit `--server` (or
/// `SSF_SERVER`) names another: the catalog name selected on the command line
/// wins over whatever the daemon put in the environment, and the rest of what
/// the pane inherited does not follow it. A VM context is the one that would
/// send the command looking for another factory's guest, so it is asserted
/// separately: for a local route the name is set by the route anyway, and only
/// this half notices the pair left behind (`client_main` clears both before
/// setting the route's own).
#[cfg(target_os = "linux")]
#[test]
fn an_explicit_server_overrides_the_inherited_pane_identity() {
    let root = Temp::new("explicit-over-inherited");
    let local = root.0.join("local");
    let other = root.0.join("other");
    root.catalog(&format!(
        "[servers.local]\ntransport = \"local\"\nconfig_dir = {:?}\nstate_dir = {:?}\n\n[servers.other]\ntransport = \"local\"\nconfig_dir = {:?}\nstate_dir = {:?}\n",
        local.join("config"),
        local.join("state"),
        other.join("config"),
        other.join("state"),
    ));
    root.use_real_server();
    // The stub is what the child runs, so recording its own environment is what
    // makes these assertions about what the command received rather than about
    // what the wrapper meant to pass it.
    script(
        &root.0.join("systemctl"),
        "printf '%s | target=%s | vm=%s\\n' \"$*\" \"${SSF_INTERNAL_SELECTED_TARGET:-absent}\" \"${SSF_INTERNAL_SELECTED_VM:-absent}\" >> \"$TEST_ROOT/systemctl-args\"\ncase \"$*\" in *is-enabled*ssf@other.service*|*is-active*ssf@other.service*) exit 0;; esac\nexit 1",
    );
    let identity = r#"{"name":"local","transport":"local"}"#;
    // A pane of a VM-hosted factory inherits both selectors; neither may reach a
    // child that a selection sent elsewhere.
    let pane = || {
        let mut command = root.client();
        command.env("SSF_INTERNAL_SELECTED_TARGET", identity).env(
            "SSF_INTERNAL_SELECTED_VM",
            r#"{"name":"pane-vm","config":{}}"#,
        );
        command
    };
    let run = |command: &mut std::process::Command| {
        let _ = std::fs::remove_file(root.0.join("systemctl-args"));
        let output = command
            .args(["ui", "service", "status", "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let calls = std::fs::read_to_string(root.0.join("systemctl-args")).unwrap();
        assert_eq!(status["server"], "other");
        assert_eq!(status["unit"], "ssf@other.service");
        assert_eq!(status["active"], true);
        assert!(
            !calls.contains("ssf@local.service"),
            "the inherited name reached the service check: {calls}"
        );
        assert!(
            calls.contains(r#"target={"name":"other","transport":"local"}"#),
            "the child did not answer for the selection: {calls}"
        );
        assert!(
            calls.contains("vm=absent"),
            "the pane's VM context reached the child: {calls}"
        );
    };

    let mut command = pane();
    command.args(["--server", "other"]);
    run(&mut command);

    let mut command = pane();
    command.env("SSF_SERVER", "other");
    run(&mut command);
}

/// The same boundary for a destination: `--server HOST` is not a catalog name,
/// so there is no catalog entry to answer for it, and the host that does answer
/// is reached by a command line that carries none of what the pane inherited.
#[cfg(target_os = "linux")]
#[test]
fn an_explicit_destination_carries_none_of_an_inherited_pane_identity() {
    let root = Temp::new("explicit-over-inherited-ssh");
    // No catalog: a bare destination is passed through as the legacy meaning of
    // `--server HOST`, which is the shape that has no name to answer with.
    root.use_real_server();
    script(
        &root.0.join("ssh"),
        "printf '%s\\n' \"$@\" > \"$TEST_ROOT/ssh-args\"; exit 0",
    );

    let output = root
        .client()
        .env(
            "SSF_INTERNAL_SELECTED_TARGET",
            r#"{"name":"local","transport":"local"}"#,
        )
        .env(
            "SSF_INTERNAL_SELECTED_VM",
            r#"{"name":"pane-vm","config":{}}"#,
        )
        .args(["--server", "factory.example", "status", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let argv = std::fs::read_to_string(root.0.join("ssh-args")).unwrap();
    assert!(argv.contains("factory.example"), "{argv}");
    assert!(!argv.contains("SSF_INTERNAL"), "{argv}");
    assert!(!argv.contains("local"), "{argv}");
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

#[test]
fn skill_topics_work_without_server_binary_or_valid_configuration() {
    let root = Temp::new("skill-offline");
    root.catalog("invalid catalog TOML [");
    std::fs::write(root.0.join("config/config.toml"), "invalid config [").unwrap();
    for topic in [
        "",
        "setup",
        "agent",
        "liaison",
        "client-cli",
        "server",
        "config",
        "vm",
        "headless",
        "install-binaries",
        "drivers",
        "sessions",
        "dashboard",
        "uninstall",
    ] {
        let mut command = root.client();
        command
            .env("SSF_SERVER", "unreachable.example")
            .arg("skill");
        if !topic.is_empty() {
            command.arg(topic);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{topic}: {:?}", output);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.starts_with(&format!("SSF {}", env!("CARGO_PKG_VERSION"))));
        assert!(text.contains("\n# "), "{topic} has no document");
        assert!(output.stderr.is_empty());
    }
    assert!(!root.0.join("state").exists());
}

#[test]
fn skill_is_local_even_with_explicit_multiple_servers() {
    let root = Temp::new("skill-explicit");
    let plain = root.client().arg("skill").output().unwrap();
    let selected = root
        .client()
        .args(["--server", "one", "skill", "--server", "two"])
        .output()
        .unwrap();
    assert!(plain.status.success());
    assert!(selected.status.success(), "{selected:?}");
    assert_eq!(plain.stdout, selected.stdout);
}

#[test]
fn skill_help_and_invalid_topics_are_handled_before_catalog_loading() {
    let root = Temp::new("skill-help");
    root.catalog("invalid catalog [");
    let help = root.client().args(["skill", "--help"]).output().unwrap();
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    for topic in ["setup", "client-cli", "server"] {
        assert!(text.contains(topic));
    }
    let invalid = root
        .client()
        .args(["skill", "missing-topic"])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    assert!(
        String::from_utf8(invalid.stderr)
            .unwrap()
            .contains("unrecognized subcommand")
    );
}
