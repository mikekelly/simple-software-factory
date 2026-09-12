//! Exercise the installed client boundary without reaching a real factory or browser.
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Process(std::process::Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn dashboard_stays_on_client_for_local_remote_and_environment_routes() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();
    let root = std::env::temp_dir().join(format!("ssf-dashboard-routing-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    // A copied client must find its adjacent local server, not one on PATH.
    std::fs::copy(env!("CARGO_BIN_EXE_ssf"), root.join("ssf")).unwrap();
    script(
        &root.join("ssf-server"),
        r#"printf '%s\n' "$@" > "$TEST_ROOT/local-args"
printf '%s\n' '{"sessions":[],"factory_reachable":false,"host_vm":{"state":"stopped"}}'"#,
    );
    script(
        &root.join("ssh"),
        r#"printf '%s\n' "$@" >> "$TEST_ROOT/ssh-args"
if [ "$TEST_UNREACHABLE" = 1 ]; then echo 'connection refused' >&2; exit 255; fi
printf '%s\n' '{"sessions":[],"factory_reachable":true}'"#,
    );
    for opener in ["open", "xdg-open"] {
        script(
            &root.join(opener),
            r#"printf '%s\n' "$@" > "$TEST_ROOT/browser-url""#,
        );
    }
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    for route in ["local", "remote", "environment", "unreachable"] {
        let mut command = Command::new(root.join("ssf"));
        command
            .env("PATH", &root)
            .env("TEST_ROOT", &root)
            .env_remove("SSF_SERVER");
        if route == "remote" || route == "unreachable" {
            command.args(["--server", "customer@cloud.example"]);
        } else if route == "environment" {
            command.env("SSF_SERVER", "environment-host");
        }
        if route == "unreachable" {
            command.env("TEST_UNREACHABLE", "1");
        }
        command
            .arg("dashboard")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = Process(command.spawn().unwrap());
        let stdout = child.0.stdout.take().unwrap();
        let mut url = String::new();
        BufReader::new(stdout).read_line(&mut url).unwrap();
        let url = url.trim();
        assert!(url.starts_with("http://127.0.0.1:"), "{url}");
        let index = http.get(url).send().await.unwrap();
        assert!(index.status().is_success());
        assert!(index.text().await.unwrap().contains("dashboard.js"));
        assert_eq!(
            std::fs::read_to_string(root.join("browser-url"))
                .unwrap()
                .trim(),
            url
        );
        for _ in 0..2 {
            let response = http.get(format!("{url}api/status")).send().await.unwrap();
            if route == "unreachable" {
                assert_eq!(response.status(), 502);
                assert!(
                    response
                        .text()
                        .await
                        .unwrap()
                        .contains("connection refused")
                );
            } else {
                assert!(response.status().is_success());
                let value: serde_json::Value = response.json().await.unwrap();
                assert_eq!(value["cards"], serde_json::json!([]));
                if route == "local" {
                    assert!(value["warning"].as_str().unwrap().contains("stopped"));
                }
            }
        }
        drop(child);
    }
    assert_eq!(
        std::fs::read_to_string(root.join("local-args")).unwrap(),
        "__client\nstatus\n--json\n"
    );
    let ssh = std::fs::read_to_string(root.join("ssh-args")).unwrap();
    assert!(ssh.contains("customer@cloud.example\nssf-server __client 'status' '--json'"));
    assert!(ssh.contains("environment-host\nssf-server __client 'status' '--json'"));
    let paths: Vec<_> = ssh
        .lines()
        .filter(|line| line.starts_with("ControlPath="))
        .collect();
    assert_eq!(paths.len(), 6);
    for pair in paths.as_chunks::<2>().0 {
        assert_eq!(pair[0], pair[1]);
    }
    std::fs::remove_dir_all(root).unwrap();
}
