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
//!   answer on. So the supervisor keeps an ssh local forward open over the
//!   port gvproxy already exposes for ssh (`-ssh-port`).
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

/// The supervisor's forward of the guest's dashboard: an ssh child for
/// Firecracker, started again when it ends, or the Incus proxy device.
/// The guest's `[dashboard]` is read again every [`RETRY_EVERY`], and
/// `applied` is the forward last set up for it.
#[derive(Default)]
pub(in crate::vm) struct DashboardTunnel {
    child: Option<tokio::process::Child>,
    tried: Option<Instant>,
    applied: Option<Option<String>>,
}

impl DashboardTunnel {
    /// Keep the forward up: called on every supervision round. Anything
    /// that goes wrong is logged and tried again later; the dashboard is
    /// never a reason to stop supervising the VM.
    pub(in crate::vm) fn keep(&mut self, vm: &Vm) {
        match vm.backend() {
            BackendKind::Firecracker => self.keep_ssh(vm),
            BackendKind::Incus => self.keep_incus(vm),
            BackendKind::Lima => {}
        }
    }

    /// The forward the guest's configuration asks for, read over ssh.
    fn wanted(vm: &Vm) -> Option<String> {
        let config = vm.ssh_output(&["cat", GUEST_CONFIG]).unwrap_or_default();
        match guest_dashboard(&config) {
            Ok(dashboard) => forward_spec(&dashboard),
            Err(e) => {
                warn!("{e:#}");
                None
            }
        }
    }

    fn keep_incus(&mut self, vm: &Vm) {
        if self.tried.is_some_and(|at| at.elapsed() < RETRY_EVERY) {
            return;
        }
        self.tried = Some(Instant::now());
        let spec = Self::wanted(vm);
        if self.applied.as_ref() == Some(&spec) {
            return;
        }
        let name = vm.incus_name();
        // Replaced rather than edited: a missing device is not an error.
        let _ = vm.incus_run_within(
            &["config", "device", "remove", &name, INCUS_DEVICE],
            lima::QUICK_LIMIT,
        );
        if let Some(spec) = &spec {
            let args = incus_device_args(&name, spec);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            if let Err(e) = vm.incus_run_within(&args, lima::QUICK_LIMIT) {
                warn!("could not forward the guest's web dashboard: {e:#}");
                return;
            }
            info!("forwarding the guest's web dashboard to host {spec}");
        }
        self.applied = Some(spec);
    }

    fn keep_ssh(&mut self, vm: &Vm) {
        let running = match self.child.as_mut().map(|child| child.try_wait()) {
            Some(Ok(None)) => true,
            Some(Ok(Some(status))) => {
                warn!("the dashboard forward ended ({status})");
                false
            }
            Some(Err(e)) => {
                warn!("the dashboard forward: {e}");
                false
            }
            None => false,
        };
        if !running {
            self.child = None;
            self.applied = None;
        }
        if self.tried.is_some_and(|at| at.elapsed() < RETRY_EVERY) {
            return;
        }
        self.tried = Some(Instant::now());
        // Read again while it runs, so a changed `[dashboard]` moves it.
        let spec = Self::wanted(vm);
        if self.applied.as_ref() == Some(&spec) {
            return;
        }
        self.child = None;
        self.applied = Some(spec.clone());
        let Some(spec) = spec else {
            return;
        };
        let mut command = Command::new("ssh");
        command
            .args(vm.ssh_args(true))
            .args(["-N", "-o", "ExitOnForwardFailure=yes", "-L", &spec])
            .arg(vm.target())
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        match command.spawn() {
            Ok(child) => {
                info!("forwarding the guest's web dashboard to host {spec}");
                self.child = Some(child);
            }
            Err(e) => {
                warn!("could not start the dashboard forward: {e}");
                self.applied = None;
            }
        }
    }
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
}
