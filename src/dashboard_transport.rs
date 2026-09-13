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
    control_dir: Option<PathBuf>,
    child: Option<Child>,
    output: Option<BufReader<ChildStdout>>,
}

impl StatusSource {
    pub(crate) fn new(server: Option<String>) -> Result<Self> {
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

fn status_command(server: Option<&str>, executable: &Path, control_dir: Option<&Path>) -> Command {
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
            .arg(crate::remote_client_command(&[
                "status".into(),
                "--json".into(),
                "--watch".into(),
            ]));
        command
    } else {
        let mut command = Command::new(executable);
        command.args(["__client", "status", "--json", "--watch"]);
        command
    };
    command.env_remove("SSF_SERVER");
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
        let command = status_command(None, Path::new("/package/ssf-server"), None);
        assert_eq!(command.as_std().get_program(), "/package/ssf-server");
        assert_eq!(args(&command), ["__client", "status", "--json", "--watch"]);
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
        let first = status_command(source.server.as_deref(), Path::new("unused"), Some(dir));
        let second = status_command(source.server.as_deref(), Path::new("unused"), Some(dir));
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
