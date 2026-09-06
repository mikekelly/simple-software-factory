//! The factory inside a Firecracker microVM: the daemon, herdr and every
//! agent session run in a guest, and the host keeps only what builds,
//! starts, stops and reaches it (`ssf vm ...`). Nothing here needs root:
//! Firecracker runs as the user given `/dev/kvm`, the guest's network is
//! gvisor-tap-vsock (`gvproxy` on the host, a user-mode TCP/IP stack on the
//! unix socket Firecracker maps to guest vsock port 1024; `gvforwarder` in
//! the guest), and the images are made with `fakeroot` and `mkfs.ext4 -d`.
//!
//! Files under `[vm] dir` (`~/.local/share/ssf/vm`): the downloaded
//! `firecracker`, `gvproxy`, `gvforwarder` and `vmlinux`, the root image
//! `rootfs.ext4` that `ssf vm build` provisions from the Arch bootstrap
//! tarball, and one directory per VM with its persistent `root.ext4` (a
//! copy-on-write copy of the image), `data.ext4` (ssf's state, the clones
//! and worktrees, mounted at `/var/lib/ssf`), the `seed.ext4` written at
//! every start (this binary, the config rewritten for the guest, the bot
//! token, the ssh key, the `[vm] files`), Firecracker's config and sockets,
//! PID files and the serial console log.
//!
//! The guest is reached over ssh on `127.0.0.1:<ssh_port>` (gvproxy
//! publishes the guest's sshd there) with a key made per VM. With `[vm]
//! enabled = true` the daemon-facing commands are run inside the guest that
//! way, so `ssf status --json` for the bar widget and `ssf tell` from a
//! terminal work as before; `ssf run` on the host starts the VM and watches
//! it, so the systemd unit is unchanged.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::config::{Config, DriverKind, VmConfig, expand_tilde};

pub const FIRECRACKER_VERSION: &str = "v1.16.1";
pub const GVPROXY_VERSION: &str = "v0.8.9";
/// A Firecracker CI guest kernel: virtio-blk, vsock, tun and overlayfs built in.
pub const KERNEL_URL: &str = "https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/20260902-a6146c8bb213-0/x86_64/vmlinux-6.1.182";
pub const BOOTSTRAP_URL: &str =
    "https://geo.mirror.pkgbuild.com/iso/latest/archlinux-bootstrap-x86_64.tar.zst";

/// The unprivileged user everything runs as in the guest.
pub const GUEST_USER: &str = "ssf";
pub const GUEST_HOME: &str = "/home/ssf";
pub const GUEST_PROJECTS_DIR: &str = "/var/lib/ssf/projects";
pub const GUEST_HERDR: &str = "/usr/local/bin/herdr";
const GUEST_CID: u32 = 3;
/// The vsock port gvforwarder dials; Firecracker turns it into `v.sock_1024`.
const NET_PORT: u32 = 1024;

/// Commands that act on the daemon and so run inside the guest when the
/// factory is there.
pub const FORWARDED: [&str; 9] = [
    "status", "peers", "sub", "unsub", "subs", "tell", "release", "purge", "doctor",
];

/// Set in the guest so a forwarded command never forwards again.
pub const GUEST_ENV: &str = "SSF_VM_GUEST";

fn firecracker_url() -> String {
    format!(
        "https://github.com/firecracker-microvm/firecracker/releases/download/{v}/firecracker-{v}-x86_64.tgz",
        v = FIRECRACKER_VERSION
    )
}

fn gvproxy_url(name: &str) -> String {
    format!(
        "https://github.com/containers/gvisor-tap-vsock/releases/download/{GVPROXY_VERSION}/{name}"
    )
}

/// One VM: its config and the directory its files live in.
#[derive(Clone)]
pub struct Vm {
    pub cfg: VmConfig,
    /// `[vm] dir`, expanded.
    pub base: PathBuf,
    /// `<base>/<name>`.
    pub dir: PathBuf,
    /// The `ssf` binary the guest gets: this one, normally.
    pub binary: Option<PathBuf>,
}

/// What `ssf vm status` reports.
#[derive(Debug, Clone, Serialize)]
pub struct VmStatus {
    pub enabled: bool,
    pub name: String,
    pub dir: String,
    pub image: bool,
    pub running: bool,
    pub firecracker_pid: Option<u32>,
    pub gvproxy_pid: Option<u32>,
    pub ssh_port: u16,
    pub ssh: bool,
    /// `systemctl is-active ssf` in the guest, when reachable.
    pub daemon: Option<String>,
}

impl Vm {
    pub fn new(cfg: &Config) -> Self {
        let base = expand_tilde(&cfg.vm.dir);
        let dir = base.join(&cfg.vm.name);
        Self {
            cfg: cfg.vm.clone(),
            base,
            dir,
            binary: None,
        }
    }

    fn binary(&self) -> Result<PathBuf> {
        match &self.binary {
            Some(p) => Ok(p.clone()),
            None => std::env::current_exe()
                .and_then(std::fs::canonicalize)
                .context("locating the ssf binary"),
        }
    }

    // ---- files ----

    fn asset(&self, chosen: &Option<String>, name: &str) -> PathBuf {
        match chosen {
            Some(p) => expand_tilde(p),
            None => self.base.join(name),
        }
    }
    pub fn firecracker(&self) -> PathBuf {
        self.asset(&self.cfg.firecracker, "firecracker")
    }
    pub fn gvproxy(&self) -> PathBuf {
        self.asset(&self.cfg.gvproxy, "gvproxy")
    }
    pub fn gvforwarder(&self) -> PathBuf {
        self.base.join("gvforwarder")
    }
    pub fn kernel(&self) -> PathBuf {
        self.asset(&self.cfg.kernel, "vmlinux")
    }
    pub fn rootfs(&self) -> PathBuf {
        self.asset(&self.cfg.rootfs, "rootfs.ext4")
    }
    fn root_disk(&self) -> PathBuf {
        self.dir.join("root.ext4")
    }
    fn data_disk(&self) -> PathBuf {
        self.dir.join("data.ext4")
    }
    fn seed_disk(&self) -> PathBuf {
        self.dir.join("seed.ext4")
    }
    fn fc_pid(&self) -> PathBuf {
        self.dir.join("firecracker.pid")
    }
    fn gv_pid(&self) -> PathBuf {
        self.dir.join("gvproxy.pid")
    }
    pub fn console_log(&self) -> PathBuf {
        self.dir.join("console.log")
    }
    fn key(&self) -> PathBuf {
        self.dir.join("id_ed25519")
    }
    fn known_hosts(&self) -> PathBuf {
        self.dir.join("known_hosts")
    }

    // ---- processes ----

    fn pid_of(&self, file: &Path, program: &str) -> Option<u32> {
        let pid: u32 = std::fs::read_to_string(file).ok()?.trim().parse().ok()?;
        pid_runs(pid, program).then_some(pid)
    }

    pub fn firecracker_pid(&self) -> Option<u32> {
        self.pid_of(&self.fc_pid(), "firecracker")
    }

    pub fn gvproxy_pid(&self) -> Option<u32> {
        self.pid_of(&self.gv_pid(), "gvproxy")
    }

    pub fn running(&self) -> bool {
        self.firecracker_pid().is_some()
    }

    // ---- build ----

    /// Download what is missing, make the base image from the bootstrap
    /// tarball and boot it once to provision it.
    pub async fn build(&self, force: bool) -> Result<()> {
        let scripts = scripts_dir()?;
        std::fs::create_dir_all(self.base.join("dl"))
            .with_context(|| format!("creating {}", self.base.display()))?;
        self.fetch_assets().await?;
        if self.rootfs().exists() && !force {
            println!(
                "{} exists; `ssf vm build --force` makes a new one",
                self.rootfs().display()
            );
            return Ok(());
        }
        let herdr = which(&crate::config::HerdrConfig::default().command)
            .or_else(|| which("herdr"))
            .context(
                "herdr is not installed on this machine; the image takes its binary from here",
            )?;
        let build = self.base.join("build");
        std::fs::create_dir_all(&build)?;
        let base = build.join("base.ext4");
        let tarball = self.base.join("dl/bootstrap.tar.zst");
        info!("making the base image from {}", tarball.display());
        let st = Command::new(scripts.join("make-base.sh"))
            .arg(&tarball)
            .arg(&base)
            .arg(self.cfg.root_gib.to_string())
            .arg(scripts.join("guest"))
            .arg(&herdr)
            .arg(self.gvforwarder())
            .status()
            .context("running make-base.sh (are fakeroot, bsdtar and mkfs.ext4 installed?)")?;
        if !st.success() {
            bail!("make-base.sh failed ({st})");
        }
        // The provisioning boot: root writable, the provisioning init as
        // PID 1, network through a gvproxy of its own.
        let console = build.join("console.log");
        let _ = std::fs::remove_file(&console);
        let boot = self.boot_files(&build);
        std::fs::write(
            &boot.config,
            serde_json::to_string_pretty(&self.fc_config_json(
                &[(&base, false)],
                &boot,
                Some("/usr/local/lib/ssf/provision-init.sh"),
            ))?,
        )?;
        clean_sockets(&boot);
        let gv = self.spawn_gvproxy(&boot, self.cfg.ssh_port + 1)?;
        let result = self.provision(&boot, &console).await;
        kill(gv, libc::SIGTERM);
        result?;
        std::fs::rename(&base, self.rootfs())
            .with_context(|| format!("moving the image to {}", self.rootfs().display()))?;
        println!("built {}", self.rootfs().display());
        Ok(())
    }

    async fn provision(&self, boot: &BootFiles, console: &Path) -> Result<()> {
        info!("booting the image once to provision it (a few minutes)");
        let out = std::fs::File::create(console)?;
        let mut child = Command::new(self.firecracker())
            .arg("--api-sock")
            .arg(&boot.api)
            .arg("--config-file")
            .arg(&boot.config)
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .spawn()
            .with_context(|| format!("starting {}", self.firecracker().display()))?;
        // Show the guest's progress (not the kernel's boot messages).
        let path = console.to_path_buf();
        let tail = tokio::spawn(async move {
            let mut seen = 0usize;
            loop {
                if let Ok(text) = tokio::fs::read_to_string(&path).await {
                    for line in text[seen.min(text.len())..].lines() {
                        if !line.starts_with('[') && !line.trim().is_empty() {
                            eprintln!("  {line}");
                        }
                    }
                    seen = text.len();
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
        let status = loop {
            if let Some(st) = child.try_wait()? {
                break st;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        tail.abort();
        let log = std::fs::read_to_string(console).unwrap_or_default();
        if !log.contains("ssf-provision: DONE") {
            let tail: Vec<&str> = log.lines().rev().take(30).collect::<Vec<_>>();
            bail!(
                "provisioning failed (firecracker {status}); the end of {}:\n{}",
                console.display(),
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            );
        }
        Ok(())
    }

    async fn fetch_assets(&self) -> Result<()> {
        let dl = self.base.join("dl");
        if !self.firecracker().exists() {
            let tgz = dl.join("firecracker.tgz");
            download(&firecracker_url(), &tgz).await?;
            let out = Command::new("tar")
                .arg("-xzf")
                .arg(&tgz)
                .arg("-C")
                .arg(&dl)
                .status()?;
            if !out.success() {
                bail!("unpacking {} failed", tgz.display());
            }
            let bin = dl
                .join(format!("release-{FIRECRACKER_VERSION}-x86_64"))
                .join(format!("firecracker-{FIRECRACKER_VERSION}-x86_64"));
            std::fs::copy(&bin, self.firecracker())
                .with_context(|| format!("no {} in the release tarball", bin.display()))?;
            make_executable(&self.firecracker())?;
        }
        if !self.gvproxy().exists() {
            download(&gvproxy_url("gvproxy-linux-amd64"), &self.gvproxy()).await?;
            make_executable(&self.gvproxy())?;
        }
        if !self.gvforwarder().exists() {
            download(&gvproxy_url("gvforwarder"), &self.gvforwarder()).await?;
            make_executable(&self.gvforwarder())?;
        }
        if !self.kernel().exists() {
            download(KERNEL_URL, &self.kernel()).await?;
        }
        let tarball = dl.join("bootstrap.tar.zst");
        if !tarball.exists() {
            download(BOOTSTRAP_URL, &tarball).await?;
        }
        Ok(())
    }

    // ---- start / stop ----

    fn boot_files(&self, dir: &Path) -> BootFiles {
        BootFiles {
            config: dir.join("firecracker.json"),
            api: dir.join("firecracker.sock"),
            vsock: dir.join("v.sock"),
            gv_api: dir.join("gvproxy.sock"),
            gv_log: dir.join("gvproxy.log"),
        }
    }

    /// Firecracker's config: the kernel, the drives in order (`/dev/vda`
    /// first), the machine size and the vsock device.
    pub fn fc_config_json(
        &self,
        drives: &[(&Path, bool)],
        boot: &BootFiles,
        init: Option<&str>,
    ) -> Value {
        let mut args = String::from(
            "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw random.trust_cpu=on",
        );
        if let Some(i) = init {
            args.push_str(&format!(" init={i}"));
        }
        let drives: Vec<Value> = drives
            .iter()
            .enumerate()
            .map(|(i, (path, ro))| {
                json!({
                    "drive_id": format!("drive{i}"),
                    "path_on_host": path.to_string_lossy(),
                    "is_root_device": i == 0,
                    "is_read_only": ro,
                })
            })
            .collect();
        json!({
            "boot-source": {
                "kernel_image_path": self.kernel().to_string_lossy(),
                "boot_args": args,
            },
            "drives": drives,
            "machine-config": {
                "vcpu_count": self.cfg.vcpus,
                "mem_size_mib": self.cfg.mem_mib,
            },
            "vsock": {
                "guest_cid": GUEST_CID,
                "uds_path": boot.vsock.to_string_lossy(),
            },
        })
    }

    fn spawn_gvproxy(&self, boot: &BootFiles, ssh_port: u16) -> Result<u32> {
        let net = format!("unix://{}_{NET_PORT}", boot.vsock.display());
        let api = format!("unix://{}", boot.gv_api.display());
        let mut cmd = Command::new(self.gvproxy());
        cmd.args(["-listen", &net, "-listen", &api, "-mtu", "1500"])
            .args(["-ssh-port", &ssh_port.to_string()])
            .arg("-log-file")
            .arg(&boot.gv_log);
        let pid = spawn_detached(&mut cmd, None)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !boot
            .vsock
            .with_extension(format!("sock_{NET_PORT}"))
            .exists()
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(100));
        }
        if !pid_runs(pid, "gvproxy") {
            bail!(
                "gvproxy exited at once (port {ssh_port} taken?); see {}",
                boot.gv_log.display()
            );
        }
        Ok(pid)
    }

    /// Start the VM: make its disks if missing, write a fresh seed, start
    /// gvproxy and Firecracker detached, and wait for ssh and the daemon.
    pub async fn start(&self, host: &Config) -> Result<()> {
        if self.running() {
            println!("VM {} is already running", self.cfg.name);
            return Ok(());
        }
        for (what, path) in [
            ("Firecracker", self.firecracker()),
            ("gvproxy", self.gvproxy()),
            ("the guest kernel", self.kernel()),
            ("the root image", self.rootfs()),
        ] {
            if !path.exists() {
                bail!("{what} is missing ({}); run `ssf vm build`", path.display());
            }
        }
        std::fs::create_dir_all(&self.dir)?;
        set_mode(&self.dir, 0o700)?;
        if !self.root_disk().exists() {
            info!("making {} from the image", self.root_disk().display());
            let st = Command::new("cp")
                .args(["--reflink=auto", "--sparse=always"])
                .arg(self.rootfs())
                .arg(self.root_disk())
                .status()?;
            if !st.success() {
                bail!("copying the image failed");
            }
        }
        if !self.data_disk().exists() {
            info!(
                "making {} ({} GiB, sparse)",
                self.data_disk().display(),
                self.cfg.data_gib
            );
            let f = std::fs::File::create(self.data_disk())?;
            f.set_len(u64::from(self.cfg.data_gib) << 30)?;
            drop(f);
            run_ok(
                Command::new("mkfs.ext4")
                    .args(["-q", "-L", "ssf-data"])
                    .arg(self.data_disk()),
                "mkfs.ext4",
            )?;
        }
        if !self.key().exists() {
            run_ok(
                Command::new("ssh-keygen")
                    .args(["-q", "-t", "ed25519", "-N", "", "-C", "ssf-vm", "-f"])
                    .arg(self.key()),
                "ssh-keygen",
            )?;
        }
        self.write_seed(host)?;
        let boot = self.boot_files(&self.dir);
        std::fs::write(
            &boot.config,
            serde_json::to_string_pretty(&self.fc_config_json(
                &[
                    (&self.root_disk(), false),
                    (&self.data_disk(), false),
                    (&self.seed_disk(), true),
                ],
                &boot,
                None,
            ))?,
        )?;
        clean_sockets(&boot);
        let _ = std::fs::remove_file(self.console_log());
        let gv = self.spawn_gvproxy(&boot, self.cfg.ssh_port)?;
        std::fs::write(self.gv_pid(), gv.to_string())?;
        let mut cmd = Command::new(self.firecracker());
        cmd.arg("--api-sock")
            .arg(&boot.api)
            .arg("--config-file")
            .arg(&boot.config);
        let fc = match spawn_detached(&mut cmd, Some(&self.console_log())) {
            Ok(pid) => pid,
            Err(e) => {
                kill(gv, libc::SIGTERM);
                return Err(e);
            }
        };
        std::fs::write(self.fc_pid(), fc.to_string())?;
        info!(pid = fc, "firecracker started");
        if let Err(e) = self.wait_for_ssh(Duration::from_secs(90)).await {
            bail!("{e:#}; the console is in {}", self.console_log().display());
        }
        let daemon = self.wait_for_daemon(Duration::from_secs(60)).await;
        println!(
            "VM {} is up: ssh -p {} (ssf vm ssh), daemon {}",
            self.cfg.name,
            self.cfg.ssh_port,
            daemon.as_deref().unwrap_or("unknown")
        );
        if daemon.as_deref() != Some("active") {
            eprintln!("the guest daemon is not active; `ssf vm logs` has its journal");
        }
        Ok(())
    }

    /// Shut the guest down (Ctrl-Alt-Del through Firecracker's API, which
    /// systemd turns into a reboot that ends the VM), then gvproxy.
    pub async fn stop(&self) -> Result<()> {
        let Some(fc) = self.firecracker_pid() else {
            if let Some(gv) = self.gvproxy_pid() {
                kill(gv, libc::SIGTERM);
            }
            println!("VM {} is not running", self.cfg.name);
            return Ok(());
        };
        let boot = self.boot_files(&self.dir);
        match fc_api(
            &boot.api,
            "PUT",
            "/actions",
            json!({"action_type": "SendCtrlAltDel"}),
        )
        .await
        {
            Ok(()) => {}
            Err(e) => warn!("could not ask the guest to shut down ({e:#}); killing it"),
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        while pid_runs(fc, "firecracker") && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if pid_runs(fc, "firecracker") {
            warn!("the guest did not shut down in time; killing it");
            kill(fc, libc::SIGKILL);
        }
        if let Some(gv) = self.gvproxy_pid() {
            kill(gv, libc::SIGTERM);
        }
        let _ = std::fs::remove_file(self.fc_pid());
        let _ = std::fs::remove_file(self.gv_pid());
        clean_sockets(&boot);
        println!("VM {} stopped", self.cfg.name);
        Ok(())
    }

    /// `ssf run` on the host with `[vm] enabled`: start the VM and stay
    /// until it ends or we are told to stop, shutting it down cleanly then.
    pub async fn supervise(&self, host: &Config) -> Result<()> {
        self.start(host).await?;
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut int = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        loop {
            tokio::select! {
                _ = term.recv() => break,
                _ = int.recv() => break,
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    if !self.running() {
                        warn!("the VM exited");
                        if let Some(gv) = self.gvproxy_pid() {
                            kill(gv, libc::SIGTERM);
                        }
                        bail!("the VM exited; see {}", self.console_log().display());
                    }
                }
            }
        }
        info!("stopping the VM");
        self.stop().await
    }

    // ---- seed ----

    /// Write the seed disk: this binary, the config rewritten for the
    /// guest, the token, the bot's ssh key, our public key and the `[vm]
    /// files`. Regenerated at every start so the guest follows the host.
    fn write_seed(&self, host: &Config) -> Result<()> {
        let tree = self.dir.join("seed");
        let _ = std::fs::remove_dir_all(&tree);
        std::fs::create_dir_all(tree.join("config"))?;
        set_mode(&tree, 0o700)?;
        std::fs::copy(self.binary()?, tree.join("ssf"))?;
        let mut guest = guest_config(host);
        if let Some(key) = host
            .github
            .ssh_key_path
            .as_deref()
            .map(expand_tilde)
            .filter(|p| p.exists())
        {
            let name = key
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "bot_ed25519".into());
            let keys = tree.join("config/keys");
            std::fs::create_dir_all(&keys)?;
            std::fs::copy(&key, keys.join(&name))?;
            let pubkey = crate::keys::public_path(&key);
            if pubkey.exists() {
                std::fs::copy(&pubkey, keys.join(format!("{name}.pub")))?;
            }
            guest.github.ssh_key_path = Some(format!("{GUEST_HOME}/.config/ssf/keys/{name}"));
        } else {
            guest.github.ssh_key_path = None;
            guest.github.ssh_key_id = None;
            guest.github.signing_key_id = None;
        }
        let toml = toml::to_string_pretty(&guest).context("serialising the guest config")?;
        write_private(&tree.join("config/config.toml"), toml.as_bytes())?;
        match host.github_token() {
            Ok(t) => write_private(&tree.join("config/token"), t.as_bytes())?,
            Err(e) => warn!("no bot token for the guest ({e:#}); run `ssf auth login` first"),
        }
        std::fs::copy(
            self.key().with_extension("pub"),
            tree.join("authorized_keys"),
        )?;
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let mut list = String::new();
        let files = tree.join("files");
        std::fs::create_dir_all(&files)?;
        for (i, spec) in self.cfg.files.iter().enumerate() {
            let (src, dest) = parse_file_spec(spec, &home);
            if !src.exists() {
                warn!(
                    spec,
                    "[vm] files: {} does not exist; skipped",
                    src.display()
                );
                list.push('\n');
                continue;
            }
            std::fs::copy(&src, files.join((i + 1).to_string()))
                .with_context(|| format!("copying {}", src.display()))?;
            list.push_str(&dest);
            list.push('\n');
        }
        std::fs::write(tree.join("files.list"), list)?;
        // The disk: what the tree takes plus room, at least 16 MiB.
        let bytes = dir_size(&tree)?;
        let size = (bytes + bytes / 4 + (8 << 20)).max(16 << 20);
        let size = size.div_ceil(1 << 20) << 20;
        let disk = self.seed_disk();
        let _ = std::fs::remove_file(&disk);
        let f = std::fs::File::create(&disk)?;
        f.set_len(size)?;
        drop(f);
        set_mode(&disk, 0o600)?;
        run_ok(
            Command::new("mkfs.ext4")
                .args(["-q", "-L", "ssf-seed", "-d"])
                .arg(&tree)
                .arg(&disk),
            "mkfs.ext4 -d",
        )?;
        let _ = std::fs::remove_dir_all(&tree);
        Ok(())
    }

    // ---- ssh ----

    /// The ssh options that reach the guest.
    pub fn ssh_args(&self, batch: bool) -> Vec<String> {
        let mut v = vec![
            "-i".to_string(),
            self.key().to_string_lossy().to_string(),
            "-p".to_string(),
            self.cfg.ssh_port.to_string(),
            "-o".to_string(),
            "StrictHostKeyChecking=no".to_string(),
            "-o".to_string(),
            format!("UserKnownHostsFile={}", self.known_hosts().display()),
            "-o".to_string(),
            "LogLevel=ERROR".to_string(),
            "-o".to_string(),
            "ConnectTimeout=5".to_string(),
        ];
        if batch {
            v.push("-o".into());
            v.push("BatchMode=yes".into());
        }
        v
    }

    fn target(&self) -> String {
        format!("{GUEST_USER}@127.0.0.1")
    }

    /// An ssh command running `remote` in the guest (`tty` for interactive use).
    pub fn ssh(&self, remote: &[String], tty: bool) -> Command {
        let mut cmd = Command::new("ssh");
        cmd.args(self.ssh_args(!tty));
        if tty {
            cmd.arg("-t");
        }
        cmd.arg(self.target());
        if !remote.is_empty() {
            cmd.arg("--");
            cmd.arg(shell_join(remote));
        }
        cmd
    }

    /// Run a command in the guest and return its stdout.
    pub fn ssh_output(&self, remote: &[&str]) -> Result<String> {
        let remote: Vec<String> = remote.iter().map(|s| s.to_string()).collect();
        let out = self
            .ssh(&remote, false)
            .stdin(Stdio::null())
            .output()
            .context("running ssh")?;
        if !out.status.success() {
            bail!(
                "ssh {} failed: {}",
                remote.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn ssh_ok(&self) -> bool {
        self.ssh_output(&["true"]).is_ok()
    }

    async fn wait_for_ssh(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !self.running() {
                bail!("firecracker exited");
            }
            if self.ssh_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        bail!("the guest did not answer on ssh in {}s", timeout.as_secs())
    }

    /// `systemctl is-active ssf` in the guest once it stops `activating`.
    async fn wait_for_daemon(&self, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let state = self.daemon_state();
            if state.as_deref() != Some("activating") || Instant::now() >= deadline {
                return state;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    fn daemon_state(&self) -> Option<String> {
        let remote = ["systemctl", "is-active", "ssf"];
        let remote: Vec<String> = remote.iter().map(|s| s.to_string()).collect();
        let out = self
            .ssh(&remote, false)
            .stdin(Stdio::null())
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    /// Run an `ssf` command inside the guest with this terminal, and exit
    /// with its status.
    pub fn exec_ssf(&self, args: &[String]) -> Result<ExitStatus> {
        let mut remote = vec![format!("{GUEST_ENV}=1"), "ssf".to_string()];
        remote.extend(args.iter().cloned());
        let tty = stdin_is_tty();
        let st = self
            .ssh(&remote, tty)
            .status()
            .context("running ssh (is the VM up? `ssf vm status`)")?;
        Ok(st)
    }

    /// A shell, or a command line passed to the guest's shell as given
    /// (`ssf vm ssh -- ls -la /var/lib/ssf`).
    pub fn shell(&self, args: &[String]) -> Result<ExitStatus> {
        let tty = stdin_is_tty();
        let mut cmd = Command::new("ssh");
        cmd.args(self.ssh_args(!tty));
        if tty {
            cmd.arg("-t");
        }
        cmd.arg(self.target());
        if !args.is_empty() {
            cmd.arg("--").arg(args.join(" "));
        }
        Ok(cmd.status()?)
    }

    /// Attach to herdr's persistent session in the guest, in this terminal.
    pub fn attach(&self) -> Result<ExitStatus> {
        self.ssh(&["herdr".to_string()], true)
            .status()
            .context("running ssh")
    }

    /// An `~/.ssh/config` entry for the guest, for `herdr --remote <name>`
    /// and plain `ssh <name>`.
    pub fn ssh_config(&self) -> String {
        format!(
            "Host ssf-{name}\n  HostName 127.0.0.1\n  Port {port}\n  User {GUEST_USER}\n  IdentityFile {key}\n  IdentitiesOnly yes\n  StrictHostKeyChecking no\n  UserKnownHostsFile {kh}\n  LogLevel ERROR\n",
            name = self.cfg.name,
            port = self.cfg.ssh_port,
            key = self.key().display(),
            kh = self.known_hosts().display(),
        )
    }

    /// Push the host's config and token into the running guest and restart
    /// its daemon (a new binary or `[vm] files` need `ssf vm restart`).
    pub fn sync(&self, host: &Config) -> Result<()> {
        if !self.ssh_ok() {
            bail!("the VM is not reachable; `ssf vm start` first");
        }
        let mut guest = guest_config(host);
        // The key path stays whatever the seed set; only the settings move.
        if let Ok(current) =
            self.ssh_output(&["cat", &format!("{GUEST_HOME}/.config/ssf/config.toml")])
            && let Ok(cur) = toml::from_str::<Config>(&current)
        {
            guest.github.ssh_key_path = cur.github.ssh_key_path;
        }
        let toml = toml::to_string_pretty(&guest)?;
        let token = host.github_token().ok();
        let mut script = format!(
            "umask 077; mkdir -p ~/.config/ssf; cat > ~/.config/ssf/config.toml <<'SSF_EOF'\n{toml}\nSSF_EOF\n"
        );
        if let Some(t) = token {
            script.push_str(&format!("printf '%s\\n' '{t}' > ~/.config/ssf/token\n"));
        }
        script.push_str("sudo systemctl restart ssf\n");
        let mut cmd = self.ssh(&["bash".to_string(), "-s".to_string()], false);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;
        use std::io::Write;
        child
            .stdin
            .take()
            .context("ssh stdin")?
            .write_all(script.as_bytes())?;
        let st = child.wait()?;
        if !st.success() {
            bail!("sync failed ({st})");
        }
        println!("config synced; guest daemon restarted");
        Ok(())
    }

    /// The guest daemon's journal.
    pub fn logs(&self, follow: bool, lines: u32) -> Result<ExitStatus> {
        let mut remote = vec![
            "sudo".to_string(),
            "journalctl".to_string(),
            "-u".to_string(),
            "ssf".to_string(),
            "-n".to_string(),
            lines.to_string(),
            "--no-pager".to_string(),
        ];
        if follow {
            remote.push("-f".into());
        }
        Ok(self.ssh(&remote, follow).status()?)
    }

    pub async fn status(&self) -> VmStatus {
        let running = self.running();
        let ssh = running && self.ssh_ok();
        VmStatus {
            enabled: self.cfg.enabled,
            name: self.cfg.name.clone(),
            dir: self.dir.to_string_lossy().to_string(),
            image: self.rootfs().exists(),
            running,
            firecracker_pid: self.firecracker_pid(),
            gvproxy_pid: self.gvproxy_pid(),
            ssh_port: self.cfg.ssh_port,
            ssh,
            daemon: if ssh { self.daemon_state() } else { None },
        }
    }

    /// Remake the root disk from the image at the next start; the data
    /// disk (state, clones, worktrees) stays.
    pub async fn reset(&self) -> Result<()> {
        if self.running() {
            self.stop().await?;
        }
        for f in [self.root_disk(), self.known_hosts()] {
            let _ = std::fs::remove_file(f);
        }
        println!("root disk removed; `ssf vm start` makes a fresh one from the image");
        Ok(())
    }

    /// Remove the VM and everything in it.
    pub async fn destroy(&self) -> Result<()> {
        if self.running() {
            self.stop().await?;
        }
        if self.dir.exists() {
            std::fs::remove_dir_all(&self.dir)
                .with_context(|| format!("removing {}", self.dir.display()))?;
        }
        println!("removed {}", self.dir.display());
        Ok(())
    }
}

/// Paths of one boot (the VM's, or the build's).
pub struct BootFiles {
    pub config: PathBuf,
    pub api: PathBuf,
    pub vsock: PathBuf,
    pub gv_api: PathBuf,
    pub gv_log: PathBuf,
}

fn clean_sockets(boot: &BootFiles) {
    for p in [
        boot.api.clone(),
        boot.vsock.clone(),
        boot.vsock.with_extension(format!("sock_{NET_PORT}")),
        boot.gv_api.clone(),
    ] {
        let _ = std::fs::remove_file(p);
    }
}

/// The host config as the guest runs it: herdr only (Orca is a desktop
/// app; the guest has no display), paths on the data disk, no host
/// checkouts, and the VM section off so nothing forwards again.
pub fn guest_config(host: &Config) -> Config {
    let mut g = host.clone();
    g.driver = DriverKind::Herdr;
    g.herdr.command = GUEST_HERDR.to_string();
    g.herdr.projects_dir = GUEST_PROJECTS_DIR.to_string();
    for r in &mut g.repos {
        r.driver = None;
        r.path = None;
    }
    g.vm = VmConfig::default();
    g.daemon.startup_orca_wait_secs = 0;
    g
}

/// Repositories the host config runs in Orca: worth a warning, since the
/// guest runs them all in herdr.
pub fn orca_repos(host: &Config) -> Vec<String> {
    host.repos
        .iter()
        .filter(|r| host.driver_for(r) == DriverKind::Orca)
        .map(|r| r.name.clone())
        .collect()
}

/// `src` or `src:dest` from `[vm] files`: the host file and where it goes
/// in the guest (relative to the guest user's home unless absolute; a bare
/// `src` under the host home keeps its place there).
pub fn parse_file_spec(spec: &str, home: &Path) -> (PathBuf, String) {
    let (src, dest) = match spec.split_once(':') {
        Some((s, d)) if !d.is_empty() => (s, Some(d)),
        _ => (spec, None),
    };
    let src = expand_tilde(src);
    let dest = match dest {
        Some(d) if d.starts_with('/') => d.to_string(),
        Some(d) => d.trim_start_matches("~/").to_string(),
        None => match src.strip_prefix(home) {
            Ok(rel) => rel.to_string_lossy().to_string(),
            Err(_) => src.to_string_lossy().to_string(),
        },
    };
    (src, dest)
}

/// Where the image scripts are: `SSF_VM_DIR`, the package's
/// `/usr/share/ssf/vm`, or `vm/` next to a source build.
pub fn scripts_dir() -> Result<PathBuf> {
    if let Ok(d) = std::env::var("SSF_VM_DIR") {
        return Ok(PathBuf::from(d));
    }
    let mut candidates = vec![PathBuf::from("/usr/share/ssf/vm")];
    if let Ok(exe) = std::env::current_exe()
        && let Some(d) = exe.parent()
    {
        candidates.push(d.join("../../vm"));
    }
    candidates.push(PathBuf::from("vm"));
    candidates
        .into_iter()
        .find(|p| p.join("make-base.sh").exists())
        .map(|p| p.canonicalize().unwrap_or(p))
        .context("the VM scripts (vm/make-base.sh) are not installed; set SSF_VM_DIR")
}

/// Is `pid` alive and running `program`?
pub fn pid_runs(pid: u32, program: &str) -> bool {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|c| {
            c.split(|b| *b == 0)
                .next()
                .map(|a| String::from_utf8_lossy(a).contains(program))
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

fn kill(pid: u32, sig: i32) {
    // SAFETY: kill(2) with a pid we started; a wrong pid only fails.
    unsafe {
        libc::kill(pid as i32, sig);
    }
}

/// Start `cmd` in a session of its own so it outlives us, with stdin
/// closed and its output appended to `log` (or dropped).
fn spawn_detached(cmd: &mut Command, log: Option<&Path>) -> Result<u32> {
    use std::os::unix::process::CommandExt;
    cmd.stdin(Stdio::null());
    match log {
        Some(p) => {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)?;
            cmd.stdout(f.try_clone()?).stderr(f);
        }
        None => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    // SAFETY: setsid is async-signal-safe and touches no shared state.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("starting {:?}", cmd.get_program()))?;
    Ok(child.id())
}

/// One request to Firecracker's API socket.
async fn fc_api(sock: &Path, method: &str, path: &str, body: Value) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::UnixStream::connect(sock)
        .await
        .with_context(|| format!("connecting to {}", sock.display()))?;
    let body = body.to_string();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await?;
    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf)).await??;
    let reply = String::from_utf8_lossy(&buf[..n]);
    let status = reply.lines().next().unwrap_or("");
    if !status.contains(" 204") && !status.contains(" 200") {
        bail!("firecracker answered {status}");
    }
    Ok(())
}

async fn download(url: &str, to: &Path) -> Result<()> {
    info!(url, "downloading");
    if let Some(p) = to.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = to.with_extension("part");
    let st = tokio::process::Command::new("curl")
        .args(["-fSL", "--retry", "3", "-o"])
        .arg(&tmp)
        .arg(url)
        .status()
        .await
        .context("running curl")?;
    if !st.success() {
        let _ = std::fs::remove_file(&tmp);
        bail!("downloading {url} failed ({st})");
    }
    std::fs::rename(&tmp, to)?;
    Ok(())
}

fn make_executable(p: &Path) -> Result<()> {
    set_mode(p, 0o755)
}

fn set_mode(p: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("chmod {}", p.display()))
}

fn write_private(p: &Path, data: &[u8]) -> Result<()> {
    std::fs::write(p, data).with_context(|| format!("writing {}", p.display()))?;
    set_mode(p, 0o600)
}

fn dir_size(p: &Path) -> Result<u64> {
    let mut total = 0;
    for e in std::fs::read_dir(p)? {
        let e = e?;
        let m = e.metadata()?;
        total += if m.is_dir() {
            dir_size(&e.path())?
        } else {
            m.len()
        };
    }
    Ok(total)
}

fn run_ok(cmd: &mut Command, what: &str) -> Result<()> {
    let out = cmd
        .output()
        .with_context(|| format!("running {what} (is it installed?)"))?;
    if !out.status.success() {
        bail!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

pub fn stdin_is_tty() -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(0) == 1 }
}

fn which(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.is_absolute() {
        return p.exists().then(|| p.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|c| c.is_file())
}

/// A command line for the remote shell.
pub fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c))
            {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RepoConfig;

    fn vm() -> Vm {
        let mut cfg = Config::default();
        cfg.vm.dir = "/v".into();
        cfg.vm.name = "one".into();
        Vm::new(&cfg)
    }

    #[test]
    fn files_and_assets_follow_the_config() {
        let vm = vm();
        assert_eq!(vm.dir, PathBuf::from("/v/one"));
        assert_eq!(vm.firecracker(), PathBuf::from("/v/firecracker"));
        assert_eq!(vm.rootfs(), PathBuf::from("/v/rootfs.ext4"));
        let mut cfg = Config::default();
        cfg.vm.kernel = Some("~/k/vmlinux".into());
        let vm = Vm::new(&cfg);
        assert!(vm.kernel().ends_with("k/vmlinux"));
        assert!(!vm.kernel().starts_with("~"));
    }

    #[test]
    fn firecracker_config_lists_drives_in_order() {
        let vm = vm();
        let boot = vm.boot_files(&vm.dir);
        let v = vm.fc_config_json(
            &[
                (Path::new("/v/one/root.ext4"), false),
                (Path::new("/v/one/data.ext4"), false),
                (Path::new("/v/one/seed.ext4"), true),
            ],
            &boot,
            None,
        );
        let drives = v["drives"].as_array().unwrap();
        assert_eq!(drives.len(), 3);
        assert_eq!(drives[0]["is_root_device"], true);
        assert_eq!(drives[1]["is_root_device"], false);
        assert_eq!(drives[2]["is_read_only"], true);
        assert_eq!(drives[2]["path_on_host"], "/v/one/seed.ext4");
        assert_eq!(v["vsock"]["uds_path"], "/v/one/v.sock");
        assert_eq!(v["vsock"]["guest_cid"], 3);
        assert_eq!(v["machine-config"]["vcpu_count"], 2);
        assert_eq!(v["machine-config"]["mem_size_mib"], 4096);
        let args = v["boot-source"]["boot_args"].as_str().unwrap();
        assert!(args.contains("root=/dev/vda rw"));
        assert!(!args.contains("init="));
        let p = vm.fc_config_json(
            &[(Path::new("/b/base.ext4"), false)],
            &boot,
            Some("/x/init"),
        );
        assert!(
            p["boot-source"]["boot_args"]
                .as_str()
                .unwrap()
                .ends_with(" init=/x/init")
        );
    }

    #[test]
    fn guest_config_is_herdr_only_on_the_data_disk() {
        let mut host = Config::default();
        host.vm.enabled = true;
        host.vm.files = vec!["~/.claude/.credentials.json".into()];
        host.herdr.projects_dir = "~/ssf/projects".into();
        host.repos.push(RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            driver: Some(DriverKind::Orca),
            path: Some("/home/me/r".into()),
            ..RepoConfig::default()
        });
        host.repos.push(RepoConfig {
            name: "o/s".into(),
            harness: "codex".into(),
            ..RepoConfig::default()
        });
        assert_eq!(
            orca_repos(&host),
            vec!["o/r".to_string(), "o/s".to_string()]
        );
        let g = guest_config(&host);
        assert_eq!(g.driver, DriverKind::Herdr);
        assert!(!g.vm.enabled);
        assert!(g.vm.files.is_empty());
        assert_eq!(g.herdr.projects_dir, GUEST_PROJECTS_DIR);
        assert_eq!(g.herdr.command, GUEST_HERDR);
        assert!(
            g.repos
                .iter()
                .all(|r| r.driver.is_none() && r.path.is_none())
        );
        assert_eq!(g.repos[0].harness, "claude");
        assert_eq!(g.daemon.startup_orca_wait_secs, 0);
        // It round-trips through TOML with nothing unknown.
        let text = toml::to_string_pretty(&g).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.driver_for(&back.repos[0]), DriverKind::Herdr);
    }

    #[test]
    fn file_specs_land_under_the_guest_home() {
        let home = Path::new("/home/me");
        assert_eq!(
            parse_file_spec("/home/me/.claude/.credentials.json", home),
            (
                PathBuf::from("/home/me/.claude/.credentials.json"),
                ".claude/.credentials.json".to_string()
            )
        );
        assert_eq!(
            parse_file_spec("/etc/thing:/etc/thing", home),
            (PathBuf::from("/etc/thing"), "/etc/thing".to_string())
        );
        assert_eq!(
            parse_file_spec("/etc/thing", home),
            (PathBuf::from("/etc/thing"), "/etc/thing".to_string())
        );
        assert_eq!(
            parse_file_spec("/tmp/key:.config/x/key", home),
            (PathBuf::from("/tmp/key"), ".config/x/key".to_string())
        );
        assert_eq!(
            parse_file_spec("/tmp/key:~/.config/x/key", home),
            (PathBuf::from("/tmp/key"), ".config/x/key".to_string())
        );
        let (src, dest) = parse_file_spec("~/.codex/auth.json", home);
        assert!(!src.starts_with("~"));
        assert!(dest.ends_with(".codex/auth.json"));
    }

    #[test]
    fn ssh_args_pin_the_key_port_and_hosts_file() {
        let vm = vm();
        let args = vm.ssh_args(true);
        let joined = args.join(" ");
        assert!(joined.contains("-i /v/one/id_ed25519"));
        assert!(joined.contains("-p 2222"));
        assert!(joined.contains("UserKnownHostsFile=/v/one/known_hosts"));
        assert!(joined.contains("BatchMode=yes"));
        assert!(!vm.ssh_args(false).join(" ").contains("BatchMode"));
        let cfg = vm.ssh_config();
        assert!(cfg.starts_with("Host ssf-one\n"));
        assert!(cfg.contains("Port 2222"));
        assert!(cfg.contains("User ssf"));
        assert_eq!(
            shell_join(&[
                "ssf".into(),
                "tell".into(),
                "o/r#1".into(),
                "hi there".into()
            ]),
            "ssf tell 'o/r#1' 'hi there'"
        );
        assert_eq!(shell_join(&["it's".into()]), "'it'\\''s'");
    }

    #[test]
    fn pid_files_name_the_program() {
        assert!(pid_runs(std::process::id(), "ssf") || pid_runs(std::process::id(), "vm"));
        assert!(!pid_runs(std::process::id(), "firecracker"));
        assert!(!pid_runs(u32::MAX - 1, "firecracker"));
    }

    /// Against the built image: starts the VM, reaches the guest daemon
    /// over ssh, stops it. Needs `ssf vm build` done and port 2299 free.
    /// `cargo test vm_live -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn vm_live() {
        let mut cfg = Config::default();
        cfg.vm.name = "live-test".into();
        cfg.vm.ssh_port = 2299;
        cfg.vm.data_gib = 2;
        cfg.github.login = Some("test-bot".into());
        let mut vm = Vm::new(&cfg);
        // Under `cargo test` this process is the test harness, not ssf.
        let exe = std::env::current_exe().unwrap();
        vm.binary = Some(exe.parent().unwrap().join("../ssf").canonicalize().unwrap());
        assert!(vm.rootfs().exists(), "no image; run `ssf vm build` first");
        vm.destroy().await.unwrap();
        vm.start(&cfg).await.unwrap();
        assert!(vm.running());
        let st = vm.status().await;
        eprintln!("status: {st:?}");
        assert!(st.ssh);
        let who = vm.ssh_output(&["id", "-un"]).unwrap();
        assert_eq!(who, GUEST_USER);
        let cfg_text = vm
            .ssh_output(&["cat", &format!("{GUEST_HOME}/.config/ssf/config.toml")])
            .unwrap();
        assert!(cfg_text.contains("driver = \"herdr\""));
        let herdr = vm.ssh_output(&["herdr", "status", "server"]).unwrap();
        assert!(herdr.contains("running"), "{herdr}");
        let status = vm
            .ssh_output(&[&format!("{GUEST_ENV}=1"), "ssf", "status", "--json"])
            .unwrap();
        let v: Value = serde_json::from_str(&status).unwrap();
        eprintln!("guest status: {v}");
        assert!(
            v.get("sessions").is_some() || v.get("repos").is_some(),
            "{v}"
        );
        let net = vm
            .ssh_output(&[
                "curl",
                "-fsS",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "https://api.github.com/",
            ])
            .unwrap();
        assert_eq!(net, "200");
        vm.stop().await.unwrap();
        assert!(!vm.running());
        vm.destroy().await.unwrap();
    }
}
