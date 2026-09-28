//! The guest's web dashboard on host loopback.
//!
//! The factory's daemon serves the dashboard wherever it runs (#653): in a
//! VM that is the guest daemon, bound by the guest's own `[dashboard]` to a
//! guest loopback or Tailscale address. A tailnet address is reached over
//! the tailnet. A loopback one is put on the same host address and port:
//!
//! * lima forwards a guest's loopback ports to the host's on its own.
//! * Incus: a `proxy` device like the ssh one, which connects inside the
//!   container, so a loopback-bound listener answers it.
//! * Firecracker: gvproxy user-mode networking can only expose a port on
//!   the guest's network address, which a loopback-bound listener does not
//!   answer on. So the supervisor listens on the host address itself and
//!   relays each connection over vsock (`crate::dashboard_relay`).
use super::*;

/// Where the guest keeps its own configuration, `[dashboard]` included.
const GUEST_CONFIG: &str = "/home/ssf/.config/ssf/config.toml";
/// How often a missing forward is looked at again: each try reads the
/// guest's configuration over ssh.
const RETRY_EVERY: Duration = Duration::from_secs(60);

/// The guest's `[dashboard]`, from the text of its config file.
pub(in crate::vm) fn guest_dashboard(config: &str) -> Result<crate::config::DashboardConfig> {
    let mut table: toml::Table = toml::from_str(config).context("parsing the guest config")?;
    match table.remove("dashboard") {
        Some(section) => section
            .try_into()
            .context("parsing the guest's [dashboard]"),
        None => Ok(crate::config::DashboardConfig::default()),
    }
}

/// The `-L` spec that puts a loopback-bound guest dashboard on the same
/// host address; `None` when there is nothing to forward (disabled, or
/// bound to a Tailscale address, which is reached over the tailnet).
pub(in crate::vm) fn forward_spec(dashboard: &crate::config::DashboardConfig) -> Option<String> {
    if !dashboard.enabled || !dashboard.bind.is_loopback() {
        return None;
    }
    let address = match dashboard.bind {
        std::net::IpAddr::V4(v4) => v4.to_string(),
        std::net::IpAddr::V6(v6) => format!("[{v6}]"),
    };
    let port = dashboard.port;
    Some(format!("{address}:{port}:{address}:{port}"))
}

/// The Incus device that carries the dashboard, beside the ssh one.
pub(in crate::vm) const INCUS_DEVICE: &str = "ssf-dashboard";

/// `incus config device add` for the dashboard proxy of a `-L`-style spec
/// (`addr:port:addr:port`, the same address and port on both sides).
pub(in crate::vm) fn incus_device_args(instance: &str, spec: &str) -> Vec<String> {
    // The spec's two halves are the same `addr:port`, so it splits in the
    // middle; an IPv6 address is bracketed and has colons of its own.
    let endpoint = &spec[..spec.len() / 2];
    [
        "config",
        "device",
        "add",
        instance,
        INCUS_DEVICE,
        "proxy",
        &format!("listen=tcp:{endpoint}"),
        &format!("connect=tcp:{endpoint}"),
    ]
    .iter()
    .map(|a| a.to_string())
    .collect()
}

/// How long one ssh read of the guest config or one `incus` call may take:
/// the supervisor awaits them between its signal checks.
const CALL_LIMIT: Duration = Duration::from_secs(20);
/// What the forward should become after a read of the guest's config:
/// `Some(new)` when it changes, `None` to keep it. A read that failed says
/// nothing about the guest's `[dashboard]`, so it never tears a forward down.
pub(in crate::vm) fn after_read(
    applied: &Option<Option<String>>,
    read: &Result<Option<String>>,
) -> Option<Option<String>> {
    match read {
        Ok(spec) if applied.as_ref() != Some(spec) => Some(spec.clone()),
        _ => None,
    }
}

/// A task aborted when dropped: the Firecracker relay lives exactly as long
/// as the [`DashboardTunnel`] holding it, that is the VM run.
struct Relay(tokio::task::JoinHandle<()>);

impl Drop for Relay {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The supervisor's forward of the guest's dashboard: the Firecracker
/// vsock relay, or the Incus proxy device. The guest's `[dashboard]` is
/// read again every [`RETRY_EVERY`], and `applied` is the forward set up
/// for the last successful read.
#[derive(Default)]
pub(in crate::vm) struct DashboardTunnel {
    relay: Option<Relay>,
    next_read: Option<Instant>,
    applied: Option<Option<String>>,
    next_bind: Option<Instant>,
}

impl DashboardTunnel {
    /// Keep the forward up: called on every supervision round. Every call
    /// it makes is bounded by [`CALL_LIMIT`]. Anything that goes wrong is
    /// logged and tried again later; the dashboard is never a reason to
    /// stop supervising the VM.
    pub(in crate::vm) async fn keep(&mut self, vm: &Vm) {
        match vm.backend() {
            BackendKind::Firecracker => self.keep_vsock(vm).await,
            BackendKind::Incus => self.keep_incus(vm).await,
            BackendKind::Lima => {}
        }
    }

    /// The forward the guest's configuration asks for, read over ssh when
    /// a read is due; `None` when none is due or the read failed.
    async fn read(&mut self, vm: &Vm) -> Option<Option<String>> {
        if self.next_read.is_some_and(|at| Instant::now() < at) {
            return None;
        }
        self.next_read = Some(Instant::now() + RETRY_EVERY);
        let read = read_wanted(vm).await;
        if let Err(e) = &read {
            warn!("reading the guest's [dashboard]: {e:#}; keeping the dashboard forward as it is");
        }
        after_read(&self.applied, &read)
    }

    async fn keep_incus(&mut self, vm: &Vm) {
        let Some(spec) = self.read(vm).await else {
            return;
        };
        let name = vm.incus_name();
        // Replaced rather than edited: a missing device is not an error.
        let _ = incus(&["config", "device", "remove", &name, INCUS_DEVICE]).await;
        if let Some(spec) = &spec {
            let args = incus_device_args(&name, spec);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            if let Err(e) = incus(&args).await {
                warn!("could not forward the guest's web dashboard: {e:#}");
                // Tried again at the next read.
                self.applied = None;
                return;
            }
            info!("forwarding the guest's web dashboard to host {spec}");
        }
        self.applied = Some(spec);
    }

    async fn keep_vsock(&mut self, vm: &Vm) {
        if let Some(spec) = self.read(vm).await {
            self.relay = None;
            self.applied = Some(spec);
            self.next_bind = None;
        }
        if self.relay.is_some() || self.next_bind.is_some_and(|at| Instant::now() < at) {
            return;
        }
        let Some(Some(spec)) = &self.applied else {
            return;
        };
        let address: std::net::SocketAddr = spec[..spec.len() / 2]
            .parse()
            .expect("a forward spec starts with its address and port");
        match crate::dashboard_relay::bind_host(address).await {
            Ok(listener) => {
                info!("relaying the guest's web dashboard to host {address} over vsock");
                self.relay = Some(Relay(tokio::spawn(crate::dashboard_relay::serve_host(
                    listener,
                    vm.boot_files(&vm.dir).vsock,
                    crate::dashboard_relay::VSOCK_PORT,
                ))));
            }
            Err(e) => {
                tracing::error!("{e:#}; trying again in a minute");
                self.next_bind = Some(Instant::now() + RETRY_EVERY);
            }
        }
    }
}

/// The guest's wanted forward, from its config read over ssh. A config
/// file that is not there is the default, off; any other failure is an
/// error, not an answer.
async fn read_wanted(vm: &Vm) -> Result<Option<String>> {
    let remote = [
        "sh".to_string(),
        "-c".to_string(),
        format!("[ ! -e {GUEST_CONFIG} ] || cat {GUEST_CONFIG}"),
    ];
    let mut command = tokio::process::Command::from(vm.ssh(&remote, false));
    command.stdin(Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(CALL_LIMIT, command.output())
        .await
        .context("reading the guest config timed out")?
        .context("running ssh")?;
    if !out.status.success() {
        bail!(
            "ssh failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(forward_spec(&guest_dashboard(&String::from_utf8_lossy(
        &out.stdout,
    ))?))
}

/// One `incus` call, bounded by [`CALL_LIMIT`].
async fn incus(args: &[&str]) -> Result<()> {
    let mut command = tokio::process::Command::new("incus");
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(CALL_LIMIT, command.output())
        .await
        .with_context(|| format!("`incus {}` timed out", args.join(" ")))?
        .context("running incus")?;
    if !out.status.success() {
        bail!(
            "`incus {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_enabled_loopback_dashboard_is_forwarded() {
        let dashboard =
            guest_dashboard("[dashboard]\nenabled = true\nport = 9090\n").expect("parses");
        assert_eq!(
            forward_spec(&dashboard).as_deref(),
            Some("127.0.0.1:9090:127.0.0.1:9090")
        );
        let v6 = guest_dashboard("[dashboard]\nenabled = true\nbind = '::1'\n").unwrap();
        assert_eq!(forward_spec(&v6).as_deref(), Some("[::1]:8787:[::1]:8787"));
        let off = guest_dashboard("[daemon]\npoll_interval_secs = 30\n").unwrap();
        assert_eq!(forward_spec(&off), None);
        let tailnet =
            guest_dashboard("[dashboard]\nenabled = true\nbind = '100.64.1.2'\n").unwrap();
        assert_eq!(forward_spec(&tailnet), None);
        assert_eq!(forward_spec(&guest_dashboard("").unwrap()), None);
    }

    #[test]
    fn the_incus_device_listens_and_connects_on_the_guest_bind() {
        assert_eq!(
            incus_device_args("ssf-factory", "127.0.0.1:9090:127.0.0.1:9090")[4..],
            [
                "ssf-dashboard",
                "proxy",
                "listen=tcp:127.0.0.1:9090",
                "connect=tcp:127.0.0.1:9090"
            ]
        );
        assert_eq!(
            incus_device_args("ssf-factory", "[::1]:8787:[::1]:8787")[6..],
            ["listen=tcp:[::1]:8787", "connect=tcp:[::1]:8787"]
        );
    }

    #[test]
    fn a_failed_read_keeps_the_forward() {
        let up = Some(Some("127.0.0.1:8787:127.0.0.1:8787".to_string()));
        assert_eq!(
            after_read(&up, &Err(anyhow::anyhow!("ssh timed out"))),
            None
        );
        assert_eq!(after_read(&up, &Ok(up.clone().unwrap())), None);
        assert_eq!(after_read(&up, &Ok(None)), Some(None));
        assert_eq!(
            after_read(&None, &Err(anyhow::anyhow!("no answer yet"))),
            None
        );
        assert_eq!(after_read(&None, &Ok(None)), Some(None));
    }
}
