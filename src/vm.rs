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

use crate::config::{
    Config, Credential, DriverKind, GitConfig, SigningKey, VmConfig, expand_tilde,
};

pub const FIRECRACKER_VERSION: &str = "v1.16.1";
pub const GVPROXY_VERSION: &str = "v0.8.9";
/// A Firecracker CI guest kernel: virtio-blk, vsock, tun and overlayfs built
/// in. These dated CI artifacts get pruned eventually; when the download
/// fails, `[vm] kernel` points at a kernel of your own (any x86_64 vmlinux
/// with those drivers built in does).
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
/// factory is there (`run` only as `run --once`; plain `run` supervises the
/// VM from the host).
pub const FORWARDED: [&str; 10] = [
    "status", "peers", "sub", "unsub", "subs", "tell", "release", "purge", "doctor", "run",
];

/// How a harness signs in inside the guest: the flow that works from a
/// terminal with no browser next to it (a URL and a code to paste back, or
/// a device code), the file it writes under the guest home, and a status
/// command where it has one. `ssf vm login` runs `argv` over ssh with a
/// tty; `ssf vm status` (and `doctor`) use `check()` to say who is logged
/// in. Every harness has such a flow, so nothing is port-forwarded: the
/// browser-callback variants bind the guest's loopback on random ports
/// (Codex's is 1455) and are their defaults on a desktop only.
#[derive(Debug, Clone, Copy)]
pub struct Login {
    pub harness: &'static str,
    /// The login command line, run in the guest home with a tty.
    pub argv: &'static [&'static str],
    /// The credential file, relative to the guest home.
    pub credential: &'static str,
    /// The file only counts when it contains this (Copilot's config file
    /// exists before any login).
    pub must_contain: Option<&'static str>,
    /// A status command to show after the login, when the harness has one.
    pub status: &'static [&'static str],
    /// Whether ssf may open the first URL the login prints in the host
    /// browser: only the plain-text flows; a full-screen TUI wraps its URL
    /// across lines and the person drives it anyway.
    pub open_url: bool,
    /// What the person does once it starts.
    pub hint: &'static str,
}

pub const LOGINS: &[Login] = &[
    Login {
        harness: "claude",
        argv: &["claude", "auth", "login"],
        credential: ".claude/.credentials.json",
        must_contain: None,
        status: &["claude", "auth", "status"],
        open_url: true,
        hint: "sign in on the page, then paste the code it shows back here",
    },
    Login {
        harness: "codex",
        argv: &["codex", "login", "--device-auth"],
        credential: ".codex/auth.json",
        must_contain: None,
        status: &["codex", "login", "status"],
        open_url: true,
        hint: "enter the one-time code on the page",
    },
    Login {
        harness: "gemini",
        argv: &["env", "NO_BROWSER=true", "gemini"],
        credential: ".gemini/oauth_creds.json",
        must_contain: None,
        status: &[],
        open_url: false,
        hint: "Gemini starts: pick \"Sign in with Google\" (or an API key), open the URL it prints, paste the code back, then /quit",
    },
    Login {
        harness: "copilot",
        argv: &["copilot", "login", "--device-code"],
        credential: ".copilot/config.json",
        must_contain: Some("token"),
        status: &[],
        open_url: true,
        hint: "enter the one-time code on the page",
    },
    Login {
        harness: "opencode",
        argv: &["opencode", "auth", "login"],
        credential: ".local/share/opencode/auth.json",
        must_contain: None,
        status: &["opencode", "auth", "list"],
        open_url: false,
        hint: "pick the provider and method; OAuth methods print a URL and take the code back, API keys are pasted",
    },
    Login {
        harness: "pi",
        argv: &["pi"],
        credential: ".pi/agent/auth.json",
        must_contain: None,
        status: &[],
        open_url: false,
        hint: "Pi starts: type /login, pick the method and provider, open the URL it prints, paste the code or the redirect URL back, then ctrl+d",
    },
    Login {
        harness: "omp",
        argv: &["omp"],
        credential: ".omp/agent/auth.json",
        must_contain: None,
        status: &[],
        open_url: false,
        hint: "Oh My Pi starts: type /login, pick the method and provider, open the URL it prints, paste the code back, then ctrl+d",
    },
    Login {
        harness: "grok",
        argv: &["grok", "login", "--device-auth"],
        credential: ".grok/auth.json",
        must_contain: None,
        status: &[],
        open_url: true,
        hint: "confirm the code on the page",
    },
    Login {
        harness: "crush",
        argv: &["crush", "login", "copilot"],
        credential: ".config/github-copilot/apps.json",
        must_contain: None,
        status: &[],
        open_url: true,
        hint: "press Enter, then enter the one-time code on the page",
    },
];

/// The login table entry for a harness id (`claude`, `codex`, ...).
pub fn login(harness: &str) -> Option<&'static Login> {
    LOGINS.iter().find(|l| l.harness == harness)
}

impl Login {
    /// A shell test that succeeds when the credential is in place, run in
    /// the guest home.
    pub fn check(&self) -> String {
        let file = shell_join(&[self.credential.to_string()]);
        match self.must_contain {
            Some(s) => format!("grep -qs {} {file}", shell_join(&[s.to_string()])),
            None => format!("test -s {file}"),
        }
    }
}

/// One harness's login state in the guest.
#[derive(Debug, Clone, Serialize)]
pub struct LoginState {
    pub harness: String,
    pub installed: bool,
    pub logged_in: bool,
}

/// Does `ssf <name>` run in the guest when the factory is in a VM?
pub fn forwards(name: &str) -> bool {
    FORWARDED.contains(&name)
}

/// Set in the guest (the units, `/etc/environment`, and the prefix of a
/// forwarded command) so ssf knows it is inside the VM: a forwarded command
/// never forwards again, and the prompts say the agent has root there.
pub const GUEST_ENV: &str = "SSF_VM_GUEST";

/// Is this process inside the factory's VM?
pub fn in_guest() -> bool {
    std::env::var_os(GUEST_ENV).is_some()
}

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
    /// Per-harness login state in the guest (empty when not reachable).
    pub logins: Vec<LoginState>,
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
        if std::env::consts::ARCH != "x86_64" {
            bail!(
                "the VM image is x86_64 only for now (Firecracker, gvproxy and the guest kernel are downloaded for it); this machine is {}",
                std::env::consts::ARCH
            );
        }
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
        // The build's own gvproxy, next to a VM that may be running.
        let port = self
            .cfg
            .ssh_port
            .checked_add(1)
            .unwrap_or_else(|| self.cfg.ssh_port.saturating_sub(1));
        let gv = self.spawn_gvproxy(&boot, port)?;
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
        let host_name = host.github.git_host();
        guest_git(
            host,
            &mut guest,
            &tree.join("config/keys"),
            &tree.join("config/git-tokens"),
            &|login| crate::ghcli::token_for(&host_name, login),
        )?;
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
            let mut missing = keep_guest_git(&mut guest.git, &cur.git);
            for r in &mut guest.repos {
                if let Some(c) = cur
                    .repos
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(&r.name))
                {
                    missing.extend(
                        keep_guest_git(&mut r.git, &c.git)
                            .into_iter()
                            .map(|m| format!("{} ({})", m, r.name)),
                    );
                }
            }
            if !missing.is_empty() {
                warn!(
                    "the guest does not have {} yet; `ssf vm restart` seeds it",
                    missing.join(", ")
                );
            }
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
            logins: if ssh {
                self.logins().unwrap_or_default()
            } else {
                Vec::new()
            },
        }
    }

    // ---- harness logins ----

    /// Every harness's login state in the guest, in one ssh round trip.
    pub fn logins(&self) -> Result<Vec<LoginState>> {
        let script: String = LOGINS
            .iter()
            .map(|l| {
                format!(
                    "i=0; s=0; command -v {h} >/dev/null 2>&1 && i=1; {check} && s=1; echo {h} $i $s; ",
                    h = l.harness,
                    check = l.check()
                )
            })
            .collect();
        let out = self.ssh_output(&["sh", "-c", &script])?;
        Ok(parse_login_states(&out))
    }

    /// Run a harness's login in the guest with this terminal. The first
    /// URL it prints is opened in the host browser when the flow is plain
    /// text and the host has a display (it stays on screen either way).
    /// Returns whether the credential is in place afterwards.
    pub fn login(&self, l: &Login) -> Result<bool> {
        if !self.ssh_ok() {
            bail!("the VM is not reachable; `ssf vm start` first");
        }
        let remote: Vec<String> = l.argv.iter().map(|s| s.to_string()).collect();
        println!(
            "{}: running `{}` in the VM; {}.",
            l.harness,
            remote.join(" "),
            l.hint
        );
        let mut cmd = self.ssh(&remote, true);
        let status = if l.open_url && host_has_display() {
            // ssh keeps the terminal raw and the remote pty from stdin;
            // its output passes through here to be watched for the URL.
            let mut child = cmd.stdout(Stdio::piped()).spawn().context("running ssh")?;
            let mut pipe = child.stdout.take().expect("piped stdout");
            let mut out = std::io::stdout().lock();
            let mut scan = UrlScanner::default();
            let mut buf = [0u8; 4096];
            loop {
                let n = std::io::Read::read(&mut pipe, &mut buf)?;
                if n == 0 {
                    break;
                }
                std::io::Write::write_all(&mut out, &buf[..n])?;
                std::io::Write::flush(&mut out)?;
                if let Some(url) = scan.feed(&buf[..n])
                    && open_in_browser(&url)
                {
                    // The terminal is raw while ssh runs.
                    let _ = std::io::Write::write_all(
                        &mut out,
                        b"\r\n(ssf: opened that URL in your browser)\r\n",
                    );
                }
            }
            child.wait()?
        } else {
            cmd.status().context("running ssh")?
        };
        if !status.success() {
            eprintln!("{}: login exited with {status}", l.harness);
        }
        let ok = self.ssh_output(&["sh", "-c", &l.check()]).is_ok();
        if ok
            && !l.status.is_empty()
            && let Ok(s) = self.ssh_output(l.status)
        {
            println!("{s}");
        }
        Ok(ok)
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
    g.driver = Some(DriverKind::Herdr);
    g.herdr.command = GUEST_HERDR.to_string();
    g.herdr.projects_dir = GUEST_PROJECTS_DIR.to_string();
    for r in &mut g.repos {
        r.driver = None;
        r.path = None;
    }
    g.vm = VmConfig::default();
    g.daemon.startup_driver_wait_secs = 0;
    g
}

/// Where the seed puts a person's signing key and token in the guest.
pub const GUEST_KEYS_DIR: &str = "/home/ssf/.config/ssf/keys";
pub const GUEST_TOKENS_DIR: &str = "/home/ssf/.config/ssf/git-tokens";

/// Rewrite the `[git]` tables (instance and per repository) for the guest,
/// which has none of the host's files: a signing key the host names is
/// copied into `keys_dir` and the guest path put in its place; a
/// `token:<login>` is resolved here with `resolve_token` (gh's keyring)
/// and written to `tokens_dir` as a file the guest reads (`file:...`), and
/// a `file:<path>` is copied the same way. A helper string passes through
/// as it is. A key or file that is missing here is reported and the
/// setting turned off (unsigned) or left for `ssf doctor` in the guest.
pub fn guest_git(
    host: &Config,
    guest: &mut Config,
    keys_dir: &Path,
    tokens_dir: &Path,
    resolve_token: &dyn Fn(&str) -> Result<String>,
) -> Result<()> {
    let mut copied: std::collections::BTreeMap<PathBuf, String> = Default::default();
    let mut tables: Vec<(String, &GitConfig, &mut GitConfig)> =
        vec![("[git]".to_string(), &host.git, &mut guest.git)];
    for (h, g) in host.repos.iter().zip(guest.repos.iter_mut()) {
        tables.push((format!("repo {}", h.name), &h.git, &mut g.git));
    }
    for (where_, from, to) in tables {
        if let Some(SigningKey::Path(p)) = &from.signing_key {
            let key = expand_tilde(p);
            if key.exists() {
                let name = place(&key, keys_dir, &mut copied)?;
                let pubkey = crate::keys::public_path(&key);
                if pubkey.exists() {
                    std::fs::copy(&pubkey, keys_dir.join(format!("{name}.pub")))?;
                }
                to.signing_key = Some(SigningKey::Path(format!("{GUEST_KEYS_DIR}/{name}")));
            } else {
                warn!(
                    "{where_}: signing key {} does not exist; commits in the VM go out unsigned",
                    key.display()
                );
                to.signing_key = Some(SigningKey::Off(false));
            }
        }
        match from.credential.as_deref().map(Credential::parse) {
            Some(Ok(Credential::Token(login))) => match resolve_token(&login) {
                Ok(t) => {
                    std::fs::create_dir_all(tokens_dir)?;
                    write_private(
                        &tokens_dir.join(&login),
                        format!("{}\n", t.trim()).as_bytes(),
                    )?;
                    to.credential = Some(format!("file:{GUEST_TOKENS_DIR}/{login}"));
                }
                Err(e) => warn!(
                    "{where_}: no token for @{login} here ({e:#}); pushes in the VM as @{login} will fail until it is signed in to gh on the host and the VM restarted"
                ),
            },
            Some(Ok(Credential::File(path))) => {
                if path.exists() {
                    let name = place(&path, tokens_dir, &mut copied)?;
                    to.credential = Some(format!("file:{GUEST_TOKENS_DIR}/{name}"));
                } else {
                    warn!(
                        "{where_}: token file {} does not exist; pushes in the VM with it will fail",
                        path.display()
                    );
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Copy `src` into `dir` under its own file name (a numbered one when two
/// different files share a name), once per host path.
fn place(
    src: &Path,
    dir: &Path,
    copied: &mut std::collections::BTreeMap<PathBuf, String>,
) -> Result<String> {
    if let Some(name) = copied.get(src) {
        return Ok(name.clone());
    }
    std::fs::create_dir_all(dir)?;
    let base = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    let mut name = base.clone();
    let mut n = 1;
    while copied.values().any(|v| v == &name) {
        n += 1;
        name = format!("{base}.{n}");
    }
    std::fs::copy(src, dir.join(&name)).with_context(|| format!("copying {}", src.display()))?;
    set_mode(&dir.join(&name), 0o600)?;
    copied.insert(src.to_path_buf(), name.clone());
    Ok(name)
}

/// `ssf vm sync` moves settings, not files: where the host names a key or
/// a token that the seed carried in, the guest keeps the copy's path.
/// Returns what the guest does not have yet (a new key, another login's
/// token), which `ssf vm restart` seeds; `false`, `bot` and helper strings
/// sync as they are.
pub fn keep_guest_git(host: &mut GitConfig, guest: &GitConfig) -> Vec<String> {
    let mut missing = Vec::new();
    let stem = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    // The seed names a copy after the host file, with `.N` when two files
    // share a name.
    let is_copy_of = |guest_path: &str, dir: &str, name: &str| {
        let expected = format!("{dir}/{name}");
        guest_path == expected
            || guest_path
                .strip_prefix(&format!("{expected}."))
                .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()))
    };
    if let Some(SigningKey::Path(p)) = &host.signing_key {
        let name = stem(&expand_tilde(p));
        match &guest.signing_key {
            Some(SigningKey::Path(g)) if is_copy_of(g, GUEST_KEYS_DIR, &name) => {
                host.signing_key = guest.signing_key.clone();
            }
            _ => missing.push(format!("signing key {p}")),
        }
    }
    let wanted = match host.credential.as_deref().map(Credential::parse) {
        Some(Ok(Credential::Token(login))) => Some((login.clone(), format!("token for @{login}"))),
        Some(Ok(Credential::File(path))) => {
            Some((stem(&path), format!("token file {}", path.display())))
        }
        _ => None,
    };
    if let Some((name, what)) = wanted {
        match guest.credential.as_deref().map(Credential::parse) {
            Some(Ok(Credential::File(g)))
                if is_copy_of(&g.to_string_lossy(), GUEST_TOKENS_DIR, &name) =>
            {
                host.credential = guest.credential.clone();
            }
            _ => missing.push(what),
        }
    }
    missing
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
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
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

/// `<harness> <installed> <logged in>` lines from `Vm::logins`' script.
pub fn parse_login_states(out: &str) -> Vec<LoginState> {
    out.lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            Some(LoginState {
                harness: f.next()?.to_string(),
                installed: f.next()? == "1",
                logged_in: f.next()? == "1",
            })
        })
        .collect()
}

/// Finds the first complete `https://` URL in a byte stream (terminal
/// escapes stripped), once.
#[derive(Default)]
pub struct UrlScanner {
    text: String,
    esc: Esc,
    done: bool,
}

/// Where the scanner is inside a terminal escape sequence.
#[derive(Default, PartialEq)]
enum Esc {
    #[default]
    None,
    /// Just after ESC: the next byte says what follows.
    Start,
    /// CSI (`ESC [`): runs to a final byte in `@`..`~`.
    Csi,
    /// OSC, DCS, APC, PM (`ESC ]`, `P`, `_`, `^`): runs to BEL or `ESC \`.
    Str,
    /// ESC inside a string: `\` ends it, anything else continues it.
    StrEnd,
    /// Charset selection (`ESC (`, `ESC )`): one more byte.
    One,
}

impl UrlScanner {
    pub fn feed(&mut self, bytes: &[u8]) -> Option<String> {
        if self.done {
            return None;
        }
        for &b in bytes {
            self.esc = match self.esc {
                Esc::None if b == 0x1b => Esc::Start,
                Esc::None => {
                    self.text
                        .push(if b.is_ascii_control() { ' ' } else { b as char });
                    Esc::None
                }
                // Anything else after ESC is a two-byte sequence (ESC 7,
                // ESC =, ...).
                Esc::Start => match b {
                    b'[' => Esc::Csi,
                    b']' | b'P' | b'_' | b'^' => Esc::Str,
                    b'(' | b')' => Esc::One,
                    _ => Esc::None,
                },
                Esc::Csi if (0x40..=0x7e).contains(&b) => Esc::None,
                Esc::Csi => Esc::Csi,
                Esc::Str if b == 0x07 => Esc::None,
                Esc::Str if b == 0x1b => Esc::StrEnd,
                Esc::Str => Esc::Str,
                Esc::StrEnd if b == b'\\' => Esc::None,
                Esc::StrEnd => Esc::Str,
                Esc::One => Esc::None,
            };
        }
        let Some(start) = self.text.find("https://") else {
            // Nothing pending: keep only the tail that could begin a URL.
            let keep = self.text.rfind(char::is_whitespace).map_or(0, |i| i + 1);
            self.text.drain(..keep);
            return None;
        };
        let rest = &self.text[start..];
        let end = rest.find(|c: char| c.is_whitespace() || "\"'<>".contains(c))?;
        let url = rest[..end].to_string();
        self.done = true;
        self.text.clear();
        Some(url)
    }
}

/// Whether a browser can open on this host.
fn host_has_display() -> bool {
    (std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some())
        && which("xdg-open").is_some()
}

/// Open a URL in the host browser, detached; false when that failed.
fn open_in_browser(url: &str) -> bool {
    Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
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
        host.driver = Some(DriverKind::Orca);
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
        assert_eq!(g.driver, Some(DriverKind::Herdr));
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
        assert_eq!(g.daemon.startup_driver_wait_secs, 0);
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
    fn every_known_agent_has_a_login_flow() {
        for a in crate::agents::list() {
            let l = login(&a.id).unwrap_or_else(|| panic!("no login for {}", a.id));
            assert!(!l.argv.is_empty());
            assert!(
                !l.credential.starts_with('/'),
                "{} is home-relative",
                l.credential
            );
            assert!(!l.hint.is_empty());
        }
        assert!(login("cursor").is_none());
    }

    #[test]
    fn login_checks_are_shell_tests_in_the_home() {
        assert_eq!(
            login("claude").unwrap().check(),
            "test -s .claude/.credentials.json"
        );
        assert_eq!(
            login("copilot").unwrap().check(),
            "grep -qs token .copilot/config.json"
        );
    }

    #[test]
    fn login_states_parse_the_script_output() {
        let s = parse_login_states("claude 1 1\ncodex 1 0\nomp 0 0\nbroken line\n");
        assert_eq!(s.len(), 3);
        assert!(s[0].installed && s[0].logged_in);
        assert!(s[1].installed && !s[1].logged_in);
        assert!(!s[2].installed && !s[2].logged_in);
    }

    #[test]
    fn url_scanner_finds_the_first_complete_url_once() {
        let mut sc = UrlScanner::default();
        assert_eq!(
            sc.feed(b"If the browser didn't open, visit: https://claude.com/oauth?code=tr"),
            None
        );
        assert_eq!(
            sc.feed(b"ue&state=x\r\nPaste code here >"),
            Some("https://claude.com/oauth?code=true&state=x".into())
        );
        assert_eq!(sc.feed(b"https://second.example/\n"), None);
        // Colour escapes around and inside the URL are dropped.
        let mut sc = UrlScanner::default();
        assert_eq!(
            sc.feed(b"\x1b[1mvisit \x1b[4mhttps://auth.openai.com/codex/device\x1b[0m\n"),
            Some("https://auth.openai.com/codex/device".into())
        );
        let mut sc = UrlScanner::default();
        assert_eq!(sc.feed(b"no url here\n"), None);
        // Two-byte escapes (cursor save, keypad mode) and an OSC title do
        // not swallow the URL; the buffer stays bounded while nothing is pending.
        let mut sc = UrlScanner::default();
        assert_eq!(
            sc.feed(b"\x1b7\x1b=\x1b]0;title\x07\x1b[?25lvisit https://x.example/a\n"),
            Some("https://x.example/a".into())
        );
        let mut sc = UrlScanner::default();
        for _ in 0..1000 {
            assert_eq!(sc.feed(b"some plain output line\n"), None);
        }
        assert!(sc.text.len() < 64, "{}", sc.text.len());
        assert_eq!(
            sc.feed(b"then https://y.example/ done"),
            Some("https://y.example/".into())
        );
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
        // Root through sudo, no password, and the guest knows it is one.
        let root = vm.ssh_output(&["sudo", "-n", "id", "-u"]).unwrap();
        assert_eq!(root, "0");
        let guide = vm.ssh_output(&["ssf", "guide"]).unwrap();
        assert!(guide.contains(crate::prompt::VM_GUEST_LINE), "{guide}");
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

    #[test]
    fn guest_git_carries_keys_and_tokens_in_and_rewrites_the_paths() {
        let dir = std::env::temp_dir().join(format!(
            "ssf-guest-git-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("id_ed25519");
        std::fs::write(&key, "private").unwrap();
        std::fs::write(crate::keys::public_path(&key), "public").unwrap();
        let other_key = dir.join("other").join("id_ed25519");
        std::fs::create_dir_all(other_key.parent().unwrap()).unwrap();
        std::fs::write(&other_key, "other private").unwrap();
        let token_file = dir.join("pat");
        std::fs::write(&token_file, "ghp_file\n").unwrap();
        let mut host = Config {
            git: GitConfig {
                name: Some("Ann".into()),
                email: Some("ann@example.com".into()),
                signing_key: Some(SigningKey::Path(key.to_string_lossy().to_string())),
                credential: Some("token:ann".into()),
            },
            ..Config::default()
        };
        host.repos.push(RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            git: GitConfig {
                signing_key: Some(SigningKey::Path(other_key.to_string_lossy().to_string())),
                credential: Some(format!("file:{}", token_file.display())),
                ..GitConfig::default()
            },
            ..RepoConfig::default()
        });
        host.repos.push(RepoConfig {
            name: "o/s".into(),
            harness: "claude".into(),
            git: GitConfig {
                signing_key: Some(SigningKey::Path(
                    dir.join("missing").to_string_lossy().to_string(),
                )),
                credential: Some("!gh auth git-credential".into()),
                ..GitConfig::default()
            },
            ..RepoConfig::default()
        });
        let mut guest = guest_config(&host);
        let keys = dir.join("seed/keys");
        let tokens = dir.join("seed/tokens");
        guest_git(&host, &mut guest, &keys, &tokens, &|login| {
            assert_eq!(login, "ann");
            Ok("ghp_keyring".to_string())
        })
        .unwrap();
        // Name and email go through untouched; the key and its .pub are copied.
        assert_eq!(guest.git.name.as_deref(), Some("Ann"));
        assert_eq!(
            guest.git.signing_key,
            Some(SigningKey::Path(format!("{GUEST_KEYS_DIR}/id_ed25519")))
        );
        assert_eq!(
            std::fs::read_to_string(keys.join("id_ed25519")).unwrap(),
            "private"
        );
        assert_eq!(
            std::fs::read_to_string(keys.join("id_ed25519.pub")).unwrap(),
            "public"
        );
        // The keyring token becomes a file the guest reads.
        assert_eq!(
            guest.git.credential.as_deref(),
            Some(format!("file:{GUEST_TOKENS_DIR}/ann").as_str())
        );
        assert_eq!(
            std::fs::read_to_string(tokens.join("ann")).unwrap(),
            "ghp_keyring\n"
        );
        // The repo's key shares a name with the instance one: numbered.
        assert_eq!(
            guest.repos[0].git.signing_key,
            Some(SigningKey::Path(format!("{GUEST_KEYS_DIR}/id_ed25519.2")))
        );
        assert_eq!(
            std::fs::read_to_string(keys.join("id_ed25519.2")).unwrap(),
            "other private"
        );
        assert_eq!(
            guest.repos[0].git.credential.as_deref(),
            Some(format!("file:{GUEST_TOKENS_DIR}/pat").as_str())
        );
        assert_eq!(
            std::fs::read_to_string(tokens.join("pat")).unwrap(),
            "ghp_file\n"
        );
        // A missing key turns signing off; a helper string passes through.
        assert_eq!(guest.repos[1].git.signing_key, Some(SigningKey::Off(false)));
        assert_eq!(
            guest.repos[1].git.credential.as_deref(),
            Some("!gh auth git-credential")
        );
        // sync: the guest keeps the copies the host's settings stand for,
        // and says what a restart would bring.
        let mut synced = host.git.clone();
        assert!(keep_guest_git(&mut synced, &guest.git).is_empty());
        assert_eq!(synced.signing_key, guest.git.signing_key);
        assert_eq!(synced.credential, guest.git.credential);
        let mut synced = host.repos[0].git.clone();
        assert!(keep_guest_git(&mut synced, &guest.repos[0].git).is_empty());
        assert_eq!(synced.signing_key, guest.repos[0].git.signing_key);
        let mut changed = host.git.clone();
        changed.credential = Some("token:bob".into());
        changed.signing_key = Some(SigningKey::Path("/elsewhere/new_key".into()));
        let missing = keep_guest_git(&mut changed, &guest.git);
        assert_eq!(missing.len(), 2, "{missing:?}");
        assert!(missing.iter().any(|m| m.contains("@bob")), "{missing:?}");
        assert_eq!(
            changed.credential.as_deref(),
            Some("token:bob"),
            "left for the guest's doctor to report"
        );
        // Settings that need no file sync as they are.
        let mut off = GitConfig {
            signing_key: Some(SigningKey::Off(false)),
            credential: Some("bot".into()),
            ..GitConfig::default()
        };
        assert!(keep_guest_git(&mut off, &guest.git).is_empty());
        assert_eq!(off.signing_key, Some(SigningKey::Off(false)));
        assert_eq!(off.credential.as_deref(), Some("bot"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
