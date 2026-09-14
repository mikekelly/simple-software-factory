//! Dashboard reads the same command endpoint as the ordinary client. SSH
//! multiplexing keeps polling on one authenticated connection.
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::{Child, ChildStdout, Command},
};

pub(crate) struct StatusSource {
    server: Option<String>,
    local_context: Option<crate::server_catalog::LocalContext>,
    vm_context: Option<crate::server_catalog::SelectedVmContext>,
    identity: Option<crate::server_catalog::TargetIdentity>,
    control_dir: Option<PathBuf>,
    child: Option<Child>,
    output: Option<BufReader<ChildStdout>>,
}

impl StatusSource {
    pub(crate) fn new(server: Option<String>) -> Result<Self> {
        Self::new_with_context(server, None, None, None)
    }

    pub(crate) fn new_with_context(
        server: Option<String>,
        local_context: Option<crate::server_catalog::LocalContext>,
        vm_context: Option<crate::server_catalog::SelectedVmContext>,
        identity: Option<crate::server_catalog::TargetIdentity>,
    ) -> Result<Self> {
        let control_dir = if server.is_some() {
            // Short path also fits macOS's Unix socket path limit. Atomic
            // creation and mode 0700 prevent another local user taking it over.
            let mut bytes = [0u8; 16];
            std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
            let suffix: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let dir = PathBuf::from(format!("/tmp/ssf-dashboard-{suffix}"));
            std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
            Some(dir)
        } else {
            None
        };
        Ok(Self {
            server,
            local_context,
            vm_context,
            identity,
            control_dir,
            child: None,
            output: None,
        })
    }

    /// Read the next snapshot from one long-running local process or SSH
    /// channel. A reconnect creates a new stream; ordinary refreshes do not.
    pub(crate) async fn next_snapshot(&mut self) -> Result<Value> {
        if self.output.is_none() {
            let executable = crate::server_executable()?;
            let mut command = status_command(
                self.server.as_deref(),
                &executable,
                self.control_dir.as_deref(),
                self.local_context.as_ref(),
                self.vm_context.as_ref(),
                self.identity.as_ref(),
            );
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
            let mut child = command
                .spawn()
                .context("could not start SSF status stream")?;
            let output = child
                .stdout
                .take()
                .context("SSF status stream has no output")?;
            self.child = Some(child);
            self.output = Some(BufReader::new(output));
        }
        let mut line = String::new();
        let read = tokio::time::timeout(
            Duration::from_secs(30),
            self.output.as_mut().unwrap().read_line(&mut line),
        )
        .await
        .context("SSF status stream timed out")?
        .context("reading SSF status stream")?;
        if read == 0 {
            self.output = None;
            let mut child = self.child.take().context("SSF status stream stopped")?;
            let mut detail = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = stderr.read_to_string(&mut detail).await;
            }
            let status = child
                .wait()
                .await
                .context("waiting for SSF status stream")?;
            bail!(
                "SSF status stream stopped ({status}): {}",
                detail.trim().chars().take(2000).collect::<String>()
            );
        }
        let payload: Value =
            serde_json::from_str(line.trim()).context("SSF returned invalid status JSON")?;
        if !payload.is_object() {
            bail!("SSF returned status data in an unexpected format");
        }
        Ok(payload)
    }
}

impl Drop for StatusSource {
    fn drop(&mut self) {
        if let Some(dir) = &self.control_dir {
            // OpenSSH closes the detached master after 60 seconds without
            // channels, including abrupt client termination. Removing its
            // private socket prevents accidental reuse after this UI exits.
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn status_command(
    server: Option<&str>,
    executable: &Path,
    control_dir: Option<&Path>,
    local_context: Option<&crate::server_catalog::LocalContext>,
    vm_context: Option<&crate::server_catalog::SelectedVmContext>,
    identity: Option<&crate::server_catalog::TargetIdentity>,
) -> Command {
    let mut command = if let Some(host) = server {
        let mut command = Command::new("ssh");
        command.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=2",
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPersist=60",
        ]);
        command.arg("-o").arg(format!(
            "ControlPath={}",
            control_dir
                .expect("remote source has private control directory")
                .join("ssh")
                .display()
        ));
        command
            .arg("--")
            .arg(host)
            .arg(crate::remote_client_command(
                &["status".into(), "--json".into(), "--watch".into()],
                None,
                None,
            ));
        command
    } else {
        let mut command = Command::new(executable);
        command.args(["__client", "status", "--json", "--watch"]);
        command
    };
    if server.is_none()
        && let Some(context) = local_context
    {
        command
            .env("SSF_CONFIG_DIR", &context.config_dir)
            .env("SSF_STATE_DIR", &context.state_dir);
    }
    command
        .env_remove("SSF_SERVER")
        .env_remove(crate::server_catalog::SELECTED_VM_ENV)
        .env_remove(crate::server_catalog::SELECTED_TARGET_ENV);
    if server.is_none()
        && let Some(identity) = identity
    {
        command.env(
            crate::server_catalog::SELECTED_TARGET_ENV,
            serde_json::to_string(identity).expect("serializing selected target identity"),
        );
    }
    if server.is_none()
        && let Some(context) = vm_context
    {
        command.env(
            crate::server_catalog::SELECTED_VM_ENV,
            serde_json::to_string(context).expect("serializing selected VM context"),
        );
    }
    command.stdin(Stdio::null()).kill_on_drop(true);
    command
}

#[cfg(test)]
async fn read_status(command: &mut Command, timeout: Duration) -> Result<Value> {
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .context("SSF status request timed out")?
        .context("could not start SSF status request")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        bail!(
            "SSF status request failed: {}",
            detail.trim().chars().take(2000).collect::<String>()
        );
    }
    let payload: Value =
        serde_json::from_slice(&output.stdout).context("SSF returned invalid status JSON")?;
    if !payload.is_object() {
        bail!("SSF returned status data in an unexpected format");
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(command: &Command) -> Vec<String> {
        command
            .as_std()
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn local_uses_canonical_server_endpoint() {
        let command = status_command(
            None,
            Path::new("/package/ssf-server"),
            None,
            None,
            None,
            None,
        );
        assert_eq!(command.as_std().get_program(), "/package/ssf-server");
        assert_eq!(args(&command), ["__client", "status", "--json", "--watch"]);
    }

    #[test]
    fn named_local_status_uses_its_own_config_and_state() {
        let context = crate::server_catalog::LocalContext {
            config_dir: "/factory/one/config".into(),
            state_dir: "/factory/one/state".into(),
        };
        let identity = crate::server_catalog::TargetIdentity {
            name: "one".into(),
            transport: "local".into(),
        };
        let command = status_command(
            None,
            Path::new("/package/ssf-server"),
            None,
            Some(&context),
            None,
            Some(&identity),
        );
        let environment: std::collections::BTreeMap<_, _> = command
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert_eq!(
            environment.get("SSF_CONFIG_DIR"),
            Some(&Some("/factory/one/config".into()))
        );
        assert_eq!(
            environment.get("SSF_STATE_DIR"),
            Some(&Some("/factory/one/state".into()))
        );
        assert_eq!(environment.get("SSF_SERVER"), Some(&None));
        let encoded = environment
            .get(crate::server_catalog::SELECTED_TARGET_ENV)
            .and_then(Option::as_ref)
            .unwrap();
        let decoded: crate::server_catalog::TargetIdentity = serde_json::from_str(encoded).unwrap();
        assert_eq!(decoded, identity);
    }

    #[test]
    fn named_vm_status_uses_its_owned_vm_configuration() {
        let config = crate::config::VmConfig {
            enabled: true,
            name: "crucible".into(),
            ssh_port: 2444,
            ..Default::default()
        };
        let context = crate::server_catalog::SelectedVmContext {
            name: "ssf-server".into(),
            config,
        };
        let command = status_command(
            None,
            Path::new("/package/ssf-server"),
            None,
            None,
            Some(&context),
            None,
        );
        let encoded = command
            .as_std()
            .get_envs()
            .find(|(key, _)| *key == crate::server_catalog::SELECTED_VM_ENV)
            .and_then(|(_, value)| value)
            .unwrap();
        let decoded: crate::server_catalog::SelectedVmContext =
            serde_json::from_slice(encoded.as_encoded_bytes()).unwrap();
        assert_eq!(decoded, context);
    }

    #[test]
    fn remote_reuses_one_private_control_path_and_passes_host_as_argument() {
        use std::os::unix::fs::PermissionsExt;
        let source = StatusSource::new(Some("customer@cloud.example".into())).unwrap();
        let dir = source.control_dir.as_deref().unwrap();
        assert_eq!(
            std::fs::metadata(dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let first = status_command(
            source.server.as_deref(),
            Path::new("unused"),
            Some(dir),
            None,
            None,
            None,
        );
        let second = status_command(
            source.server.as_deref(),
            Path::new("unused"),
            Some(dir),
            None,
            None,
            None,
        );
        assert_eq!(args(&first), args(&second));
        let arguments = args(&first);
        assert!(arguments.contains(&"ControlMaster=auto".into()));
        assert!(arguments.contains(&"ControlPersist=60".into()));
        assert_eq!(
            &arguments[arguments.len() - 3..],
            [
                "--",
                "customer@cloud.example",
                "ssf-server __client 'status' '--json' '--watch'"
            ]
        );
        let dir = dir.to_owned();
        drop(source);
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn unreachable_server_is_an_error_not_an_empty_factory() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo 'ssh: connection refused' >&2; exit 255"]);
        let error = read_status(&mut command, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("connection refused"));
    }

    #[tokio::test]
    async fn vm_unreachable_status_passes_through_unchanged() {
        let expected = serde_json::json!({"factory_reachable":false,"host_vm":{"state":"stopped"}});
        let mut command = Command::new("printf");
        command.args(["%s", &expected.to_string()]);
        assert_eq!(
            read_status(&mut command, Duration::from_secs(1))
                .await
                .unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn invalid_and_stalled_responses_are_errors() {
        let mut invalid = Command::new("printf");
        invalid.arg("not json");
        assert!(
            read_status(&mut invalid, Duration::from_secs(1))
                .await
                .is_err()
        );
        let mut stalled = Command::new("sleep");
        stalled.arg("2").kill_on_drop(true);
        assert!(
            read_status(&mut stalled, Duration::from_millis(20))
                .await
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
    }
}
