//! The lima backend of `ssf vm` (`[vm] backend = "lima"`; the default on
//! macOS): the guest is a [lima](https://lima-vm.io) instance, `ssf-<name>`
//! in lima's own home (`~/.lima` or `$LIMA_HOME`), with a lima data disk
//! `ssf-<name>`, and the same guest scripts and units as the
//! Firecracker image. The flow:
//!
//! * `ssf vm build`: preflight (`limactl`, and qemu on Linux), the
//!   template `<vm.dir>/<name>/lima.yaml` from `[vm]` (a cloud image lima
//!   maintains per architecture, or `vm.image`; the sizes; the share
//!   mount; the data disk; the ssh port), `share/` written, the data disk
//!   created, `limactl create` and a first `limactl start`. That first
//!   boot provisions the guest once: the template's provision script runs
//!   `/mnt/ssf/guest/lima-boot.sh` as root at every boot, which runs
//!   `provision.sh` when `/etc/ssf-image-built` is missing (packages, the
//!   `ssf` user, herdr, the harness CLIs, the units) and writes the marker
//!   on success. The host waits for the marker over `limactl shell`, then
//!   for ssh as `ssf`, and stops the instance.
//! * `ssf vm start`: `share/` written fresh (the guest scripts, the seed
//!   tree, `lima.env`, a herdr binary when the host has one for the
//!   guest), `limactl start`, the marker checked (a reset instance
//!   provisions itself again here), ssh and the daemon awaited.
//! * Every boot: `ssf-seed.service` runs `seed-lima.sh`, which mounts the
//!   data disk on `/var/lib/ssf` (growing its filesystem after `ssf vm
//!   grow`) and seeds the guest from `/mnt/ssf/seed` the way the
//!   Firecracker seed disk is read from `/seed`.
//!
//! `<vm.dir>/<name>/share/` is the only host directory the guest sees,
//! read-only at `/mnt/ssf`: `guest/` (a copy of the scripts), `seed/` (the
//! seed tree plus `lima.env`) and, when present, `herdr`.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tracing::{info, warn};

use super::{HostFacts, Sizes, Vm, make_executable, plan_grow, scripts_dir, sizes_for, which};
use crate::config::{Config, HerdrConfig, expand_tilde};
use crate::platform;

/// Where the guest sees `share/`.
pub const GUEST_MOUNT: &str = "/mnt/ssf";
/// Written by `lima-boot.sh` once `provision.sh` has succeeded.
pub const PROVISION_MARKER: &str = "/etc/ssf-image-built";
/// Where `lima-boot.sh` sends provisioning's output.
pub const PROVISION_LOG: &str = "/var/log/ssf-provision.log";
/// The instance's root disk is at least this (a cloud image plus node and
/// the harness CLIs does not fit in the Firecracker image's 8 GiB).
pub const ROOT_GIB_FLOOR: u32 = 20;
/// How long the first boot may take to provision the guest.
const PROVISION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// `limactl start` waits this long for the instance on its first boot.
const START_TIMEOUT: &str = "15m";
/// The yq expression `limactl edit` takes to turn the data disk's
/// `format` off once the disk exists (`limactl help yq-restrictions`).
const FORMAT_OFF: &str = ".additionalDisks[0].format = false";

/// lima's name for a host architecture (the guest's too).
pub fn lima_arch(arch: &str) -> Result<&'static str> {
    match arch {
        "x86_64" => Ok("x86_64"),
        "aarch64" => Ok("aarch64"),
        other => bail!("no lima guest for the {other} architecture (x86_64 or aarch64)"),
    }
}

/// The image lima boots for an architecture when `[vm] image` is unset:
/// Arch's cloud image for x86_64 (what the Firecracker image is), Ubuntu
/// LTS for aarch64 (Arch has no official aarch64 cloud image). lima keeps
/// the URL and digest.
pub fn base_template(arch: &str) -> &'static str {
    match arch {
        "x86_64" => "template:_images/archlinux",
        _ => "template:_images/ubuntu-lts",
    }
}

/// What the template is made of.
#[derive(Debug, Clone)]
pub struct Template<'a> {
    pub disk: &'a str,
    pub share: &'a Path,
    pub ssh_port: u16,
    pub sizes: Sizes,
    pub root_gib: u32,
    pub arch: &'a str,
    pub image: Option<&'a str>,
    pub vm_type: Option<&'a str>,
    /// Whether lima may format the data disk on a boot that cannot find
    /// its `lima-<disk>` label: true only for the build that creates the
    /// disk. See [`Vm::lima_template`].
    pub format_disk: bool,
}

/// The lima template for a VM. `mountType` is left to lima (9p on qemu,
/// virtiofs on vz: both are mounted before the provision scripts run;
/// reverse-sshfs would not be).
pub fn render_template(t: &Template) -> String {
    let mut y = String::from(
        "# written by ssf; edit config.toml [vm] and run `ssf vm build --force` instead\n",
    );
    if let Some(vt) = t.vm_type {
        y.push_str(&format!("vmType: {vt}\n"));
    }
    match t.image {
        Some(img) => y.push_str(&format!(
            "images:\n  - location: {}\n    arch: {}\n",
            yaml_str(img),
            t.arch
        )),
        None => y.push_str(&format!("base:\n  - {}\n", base_template(t.arch))),
    }
    y.push_str(&format!(
        "arch: {arch}\ncpus: {cpus}\nmemory: \"{mem}MiB\"\ndisk: \"{root}GiB\"\n",
        arch = t.arch,
        cpus = t.sizes.vcpus,
        mem = t.sizes.mem_mib,
        root = t.root_gib.max(ROOT_GIB_FLOOR),
    ));
    y.push_str(&format!(
        "mounts:\n  - location: {}\n    mountPoint: {GUEST_MOUNT}\n    writable: false\n",
        yaml_str(&t.share.to_string_lossy())
    ));
    y.push_str("containerd:\n  system: false\n  user: false\n");
    y.push_str(&format!(
        "ssh:\n  localPort: {}\n  loadDotSSHPubKeys: false\n",
        t.ssh_port
    ));
    y.push_str(&format!(
        "additionalDisks:\n  - name: {}\n    format: {}\n    fsType: ext4\n",
        t.disk, t.format_disk
    ));
    y.push_str(&format!(
        "provision:\n  - mode: system\n    script: |\n      #!/bin/bash\n      # every boot, as root: provision the guest once, then seed it\n      exec bash {GUEST_MOUNT}/guest/lima-boot.sh\n"
    ));
    y
}

/// A double-quoted YAML scalar.
fn yaml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// One line of `limactl list --json`.
#[derive(Debug, Clone, Deserialize)]
pub struct Instance {
    pub name: String,
    /// `Running`, `Stopped`, `Broken`, ...
    #[serde(default)]
    pub status: String,
    /// lima's directory for it (`serial.log` is there).
    #[serde(default)]
    pub dir: String,
    #[serde(default, rename = "sshLocalPort")]
    pub ssh_local_port: u16,
    #[serde(default)]
    pub cpus: u32,
    /// Bytes.
    #[serde(default)]
    pub memory: u64,
}

impl Instance {
    pub fn is_running(&self) -> bool {
        self.status == "Running"
    }
}

/// `limactl list --json` prints one JSON object per line (and nothing, with
/// a warning on stderr, when there is none).
pub fn parse_instances(text: &str) -> Vec<Instance> {
    text.lines()
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect()
}

/// One line of `limactl disk list --json`.
#[derive(Debug, Clone, Deserialize)]
pub struct Disk {
    pub name: String,
    /// Bytes.
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub dir: String,
    #[serde(default, rename = "mountPoint")]
    pub mount_point: String,
}

pub fn parse_disks(text: &str) -> Vec<Disk> {
    text.lines()
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect()
}

/// Bytes as whole GiB, rounded up.
pub fn gib_ceil(bytes: u64) -> u32 {
    u32::try_from(bytes.div_ceil(1 << 30)).unwrap_or(u32::MAX)
}

/// `share/seed/lima.env`: what the guest scripts source.
pub fn lima_env(name: &str) -> String {
    format!("SSF_VM_DATA_DISK={}\nSSF_VM_NAME={name}\n", disk_name(name))
}

pub fn instance_name(name: &str) -> String {
    format!("ssf-{name}")
}

/// The lima data disk, `ssf-<name>`. Short on purpose: lima labels the
/// filesystem `lima-<disk>`, an ext4 label holds 16 characters, and lima
/// looks the disk up by the untruncated label at every boot and formats it
/// again when it finds none (lima 2.2.0, `boot.Linux/05-lima-disks.sh`).
/// So `lima-ssf-<name>` must fit: [`check_name`] refuses a longer name.
pub fn disk_name(name: &str) -> String {
    format!("ssf-{name}")
}

/// The longest `[vm] name` whose data-disk label fits (`lima-ssf-<name>`).
pub const MAX_NAME_LEN: usize = 16 - "lima-ssf-".len();

/// Refuse a `[vm] name` the lima backend cannot label a disk for.
pub fn check_name(name: &str) -> Result<()> {
    if name.len() > MAX_NAME_LEN {
        bail!(
            "[vm] name \"{name}\" is too long for the lima backend: lima labels the data disk `lima-ssf-<name>` and an ext4 label holds 16 characters, so the name can have at most {MAX_NAME_LEN}"
        );
    }
    Ok(())
}

/// The shell the first boot's readiness probe runs in the guest: is the
/// marker there, has provisioning written to its log, is one of the guest
/// scripts still running?
///
/// The bracket classes in the `pgrep -f` pattern are load-bearing.
/// `pgrep -f` matches a process's whole command line, and the shell
/// running this probe is such a process: `limactl shell <name> sh -c
/// "<probe>"` puts the pattern in its own argv, so a pattern written
/// plainly would always find itself, "running" would be reported forever,
/// and the failure branch below could never fire — a failed provisioning
/// would hang for the whole timeout instead of printing its log.
/// `[l]ima-boot[.]sh` matches the running script but not this string.
fn provision_probe() -> String {
    format!(
        "test -f {PROVISION_MARKER} && echo done; test -s {PROVISION_LOG} && echo log; pgrep -f '{PROVISION_PGREP}' >/dev/null 2>&1 && echo running"
    )
}

/// The `pgrep -f` pattern of [`provision_probe`]: the guest's two scripts,
/// each with a bracket class so the pattern does not occur literally in
/// the probe (see there).
const PROVISION_PGREP: &str = "[l]ima-boot[.]sh|[p]rovision[.]sh";

/// What one round of [`provision_probe`] saw.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Probe {
    /// The marker is there: the guest provisioned itself.
    done: bool,
    /// Provisioning has written to its log.
    log: bool,
    /// `lima-boot.sh` or `provision.sh` is running.
    running: bool,
}

fn parse_probe(out: &str) -> Probe {
    let has = |w: &str| out.lines().any(|l| l.trim() == w);
    Probe {
        done: has("done"),
        log: has("log"),
        running: has("running"),
    }
}

/// What the wait does after a probe round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Provisioned,
    Failed,
    /// Look again, with this many finished-but-unmarked rounds behind us.
    Wait(u32),
}

/// How many rounds in a row must look finished-but-unmarked before the
/// wait calls provisioning dead: a pause between the guest's scripts
/// shows the same way for a moment.
const IDLE_ROUNDS: u32 = 3;

/// Provisioning wrote a log, nothing of it runs and the marker is not
/// there: it died. Anything else keeps the wait going.
fn provision_step(seen: Probe, idle: u32) -> Step {
    if seen.done {
        return Step::Provisioned;
    }
    let idle = if seen.log && !seen.running {
        idle + 1
    } else {
        0
    };
    if idle >= IDLE_ROUNDS {
        Step::Failed
    } else {
        Step::Wait(idle)
    }
}

/// lima's home, where the instances and the disks live: `$LIMA_HOME`,
/// else `~/.lima` (what lima itself does).
pub fn lima_home_from(env: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
    match env.map(str::trim).filter(|s| !s.is_empty()) {
        Some(h) => Some(expand_tilde(h)),
        None => home.map(|h| h.join(".lima")),
    }
}

/// Where lima keeps the data disks (`<lima home>/_disks`): the filesystem
/// a lima VM's data disk fills, which is not the one `[vm] dir` is on.
pub fn disks_dir_from(env: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
    lima_home_from(env, home).map(|h| h.join("_disks"))
}

/// [`disks_dir_from`] for this process.
pub fn disks_dir() -> Option<PathBuf> {
    disks_dir_from(
        std::env::var("LIMA_HOME").ok().as_deref(),
        dirs::home_dir().as_deref(),
    )
}

impl Vm {
    /// The lima instance: `ssf-<name>`.
    pub fn lima_name(&self) -> String {
        instance_name(&self.cfg.name)
    }

    /// The lima data disk: `ssf-<name>`.
    pub fn lima_disk_name(&self) -> String {
        disk_name(&self.cfg.name)
    }

    /// The host directory the guest mounts at `/mnt/ssf`.
    pub fn share_dir(&self) -> PathBuf {
        self.dir.join("share")
    }

    pub fn template_path(&self) -> PathBuf {
        self.dir.join("lima.yaml")
    }

    /// The guest's architecture: this machine's.
    fn lima_arch(&self) -> Result<&'static str> {
        lima_arch(std::env::consts::ARCH)
    }

    /// The template for this VM from `[vm]`. `format_disk` says whether
    /// lima may format the data disk: only the build that creates the
    /// disk passes true. lima's guest boot script formats a disk whose
    /// `lima-<disk>` label it cannot find and `format` is true, and then
    /// mounts the partition by device either way, so with it false a
    /// healthy disk still mounts and a damaged or mislabelled one fails
    /// loudly instead of being wiped (a label too long for ext4 once cost
    /// this VM its data disk). `vm/guest/seed.sh` states the same policy
    /// for Firecracker: checked and mounted, never formatted.
    pub fn lima_template(&self, format_disk: bool) -> Result<String> {
        Ok(render_template(&Template {
            disk: &self.lima_disk_name(),
            share: &self.share_dir(),
            ssh_port: self.cfg.ssh_port,
            sizes: self.sizes(),
            root_gib: self.cfg.root_gib,
            arch: self.lima_arch()?,
            image: self.cfg.image.as_deref(),
            vm_type: self.cfg.vm_type.as_deref(),
            format_disk,
        }))
    }

    // ---- limactl ----

    /// `limactl --tty=false ...`, from `[vm] limactl` or PATH, with this
    /// environment (`LIMA_HOME` and the rest are the person's).
    fn limactl(&self) -> Command {
        let program = match &self.cfg.limactl {
            Some(p) => expand_tilde(p),
            None => PathBuf::from("limactl"),
        };
        let mut cmd = Command::new(program);
        cmd.arg("--tty=false").stdin(Stdio::null());
        cmd
    }

    /// Run limactl for its output; stderr goes into the error.
    fn limactl_output(&self, args: &[&str]) -> Result<String> {
        let mut cmd = self.limactl();
        cmd.args(args);
        let out = cmd
            .output()
            .with_context(|| format!("running {}", self.limactl_hint()))?;
        if !out.status.success() {
            bail!(
                "`limactl {}` failed ({}): {}",
                args.join(" "),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// Run limactl with this terminal (its progress lines are worth seeing).
    fn limactl_run(&self, args: &[&str]) -> Result<()> {
        let mut cmd = self.limactl();
        cmd.args(args);
        let st = cmd
            .status()
            .with_context(|| format!("running {}", self.limactl_hint()))?;
        if !st.success() {
            bail!("`limactl {}` failed ({st})", args.join(" "));
        }
        Ok(())
    }

    fn limactl_hint(&self) -> String {
        match &self.cfg.limactl {
            Some(p) => format!("[vm] limactl = {p}"),
            None => "limactl (is lima installed and on PATH?)".to_string(),
        }
    }

    /// The instance, when lima has it.
    pub(super) fn lima_instance(&self) -> Result<Option<Instance>> {
        let name = self.lima_name();
        let out = self.limactl_output(&["list", "--json"])?;
        Ok(parse_instances(&out).into_iter().find(|i| i.name == name))
    }

    /// The data disk, when lima has it.
    fn lima_disk(&self) -> Result<Option<Disk>> {
        let name = self.lima_disk_name();
        let out = self.limactl_output(&["disk", "list", "--json"])?;
        Ok(parse_disks(&out).into_iter().find(|d| d.name == name))
    }

    pub(super) fn lima_running(&self) -> bool {
        self.lima_instance()
            .ok()
            .flatten()
            .is_some_and(|i| i.is_running())
    }

    /// The data disk's size as lima has it, else what a build would make.
    pub(super) fn lima_data_cap_gib(&self) -> u32 {
        match self.lima_disk() {
            Ok(Some(d)) => gib_ceil(d.size),
            _ => self.sizes().data_gib,
        }
    }

    /// The instance's serial console (`serial.log`; `serialv.log` where
    /// lima writes the virtio console instead).
    pub(super) fn lima_console_log(&self) -> Result<PathBuf> {
        let inst = self.lima_instance()?.with_context(|| {
            format!(
                "lima instance {} does not exist; run `ssf vm build`",
                self.lima_name()
            )
        })?;
        let dir = PathBuf::from(inst.dir);
        let serial = dir.join("serial.log");
        let virtio = dir.join("serialv.log");
        Ok(if !serial.exists() && virtio.exists() {
            virtio
        } else {
            serial
        })
    }

    /// What a build needs: limactl that runs, and on Linux qemu for the
    /// architecture (lima's only Linux driver), naming what to install.
    fn lima_preflight(&self) -> Result<()> {
        check_name(&self.cfg.name)?;
        let arch = self.lima_arch()?;
        if self.limactl().arg("--version").output().is_err() {
            bail!(
                "limactl is not installed ({}); install lima (`brew install lima` on macOS, the `lima` package on Linux) or set [vm] limactl to it",
                self.limactl_hint()
            );
        }
        if !platform::is_macos() {
            if self.cfg.vm_type.as_deref() == Some("vz") {
                bail!("[vm] vm_type = \"vz\" is macOS only; unset it or use \"qemu\" here");
            }
            let qemu = format!("qemu-system-{arch}");
            if which(&qemu).is_none() {
                bail!(
                    "{qemu} is not on PATH; install qemu (Arch: `qemu-full` or `qemu-base`; Debian/Ubuntu: `qemu-system-{}`; Fedora: `qemu-system-{}`)",
                    if arch == "x86_64" { "x86" } else { "arm" },
                    if arch == "x86_64" { "x86" } else { "aarch64" },
                );
            }
        }
        Ok(())
    }

    // ---- share ----

    /// Write `share/` fresh: the guest scripts, the seed tree with
    /// `lima.env`, and a herdr binary for the guest when the host has one.
    fn write_share(&self, host: &Config) -> Result<()> {
        let share = self.share_dir();
        std::fs::create_dir_all(&share)?;
        let guest = share.join("guest");
        let _ = std::fs::remove_dir_all(&guest);
        copy_dir(&scripts_dir()?.join("guest"), &guest)?;
        let seed = share.join("seed");
        self.seed_tree(host, &seed)?;
        std::fs::write(seed.join("lima.env"), lima_env(&self.cfg.name))?;
        let herdr = share.join("herdr");
        let _ = std::fs::remove_file(&herdr);
        if let Some(src) = self.guest_herdr()? {
            std::fs::copy(&src, &herdr).with_context(|| format!("copying {}", src.display()))?;
            make_executable(&herdr)?;
        }
        Ok(())
    }

    /// A herdr binary for the guest: `[vm] herdr`, or the host's own on a
    /// Linux host (the guest's architecture is the host's); `None` lets
    /// `provision.sh` download herdr's release.
    fn guest_herdr(&self) -> Result<Option<PathBuf>> {
        if let Some(p) = &self.cfg.herdr {
            let p = expand_tilde(p);
            if !p.is_file() {
                bail!("[vm] herdr {} does not exist", p.display());
            }
            return Ok(Some(p));
        }
        if std::env::consts::OS != "linux" {
            return Ok(None);
        }
        Ok(which(&HerdrConfig::default().command).or_else(|| which("herdr")))
    }

    // ---- build / start / stop ----

    /// Create the instance and boot it once so the guest scripts
    /// provision it; `--force` deletes an existing instance first (the
    /// data disk is never deleted by a build).
    pub(super) async fn lima_build(&self, host: &Config, force: bool) -> Result<()> {
        self.lima_preflight()?;
        let name = self.lima_name();
        if let Some(inst) = self.lima_instance()? {
            if !force {
                println!(
                    "lima instance {name} exists ({}); `ssf vm build --force` makes a new one",
                    inst.dir
                );
                return Ok(());
            }
            if inst.is_running() {
                self.lima_stop().await?;
            }
            info!("deleting lima instance {name}");
            self.limactl_run(&["delete", "-f", &name])?;
        }
        self.ensure_key()?;
        let _ = std::fs::remove_file(self.known_hosts());
        // Only the build that makes the data disk lets lima format it.
        let disk = self.lima_disk_name();
        let disk_exists = self.lima_disk()?.is_some();
        self.write_template(!disk_exists)?;
        self.write_share(host)?;
        if !disk_exists {
            let gib = self.sizes().data_gib;
            info!("creating lima disk {disk} ({gib} GiB)");
            self.limactl_run(&["disk", "create", &disk, "--size", &format!("{gib}GiB")])?;
        }
        self.lima_create()?;
        info!("starting {name} for its first boot: the guest provisions itself (a few minutes)");
        self.limactl_run(&["start", "--timeout", START_TIMEOUT, &name])?;
        self.wait_for_provisioning().await?;
        if let Ok(log) = self.limactl_output(&["shell", &name, "sudo", "cat", PROVISION_LOG]) {
            for line in log.lines().filter(|l| l.starts_with("provision: ")) {
                eprintln!("  {line}");
            }
        }
        if let Err(e) = self.wait_for_ssh(Duration::from_secs(120)).await {
            bail!(
                "{e:#} (provisioned, but the guest does not answer as {}); `ssf vm console` has its console",
                super::GUEST_USER
            );
        }
        self.limactl_run(&["stop", &name])?;
        // The disk exists and carries its filesystem now: neither this
        // instance nor one `ssf vm reset` makes from the template may
        // format it again.
        self.write_template(false)?;
        self.stop_formatting_data_disk();
        println!("built lima instance {name}; `ssf vm start` boots it");
        Ok(())
    }

    /// Write `lima.yaml` for this VM.
    fn write_template(&self, format_disk: bool) -> Result<()> {
        std::fs::write(self.template_path(), self.lima_template(format_disk)?)
            .with_context(|| format!("writing {}", self.template_path().display()))
    }

    /// Turn `format` off for the data disk in the instance's own copy of
    /// the template (`limactl create` took a copy, and only lima's copy
    /// is read at boot). A build made the disk with `format: true`
    /// because there was nothing to lose yet; from here on a boot that
    /// cannot find the disk's label must fail rather than reformat it.
    /// Not fatal: the build succeeded, and the message says how to do it
    /// by hand.
    fn stop_formatting_data_disk(&self) {
        let name = self.lima_name();
        if let Err(e) = self.limactl_run(&["edit", &name, "--set", FORMAT_OFF]) {
            warn!(
                "could not turn `format` off for {}'s data disk ({e:#}); `limactl edit {name} --set '{FORMAT_OFF}'` does it, and until then a boot that cannot find the disk's label would reformat it",
                name
            );
        }
    }

    /// `limactl create` from the template written by a build.
    fn lima_create(&self) -> Result<()> {
        let template = self.template_path();
        if !template.exists() {
            bail!("{} does not exist; run `ssf vm build`", template.display());
        }
        let name = self.lima_name();
        info!("creating lima instance {name} from {}", template.display());
        self.limactl_run(&["create", "--name", &name, &template.to_string_lossy()])
    }

    /// Wait for `/etc/ssf-image-built` over `limactl shell`: present at
    /// once on a provisioned instance; on a first boot, until
    /// `provision.sh` has written it, or has ended without it (then the
    /// end of its log is the error).
    async fn wait_for_provisioning(&self) -> Result<()> {
        let name = self.lima_name();
        let probe = provision_probe();
        let deadline = Instant::now() + PROVISION_TIMEOUT;
        let mut idle = 0;
        loop {
            let out = self
                .limactl_output(&["shell", &name, "sh", "-c", &probe])
                .unwrap_or_default();
            match provision_step(parse_probe(&out), idle) {
                Step::Provisioned => return Ok(()),
                Step::Failed => {
                    let tail = self
                        .limactl_output(&["shell", &name, "sudo", "tail", "-50", PROVISION_LOG])
                        .unwrap_or_default();
                    bail!(
                        "provisioning failed in {name} (no {PROVISION_MARKER}); the end of {PROVISION_LOG}:\n{}",
                        tail.trim_end()
                    );
                }
                Step::Wait(n) => idle = n,
            }
            if Instant::now() >= deadline {
                bail!(
                    "provisioning did not finish in {} minutes; `limactl shell {name} sudo tail {PROVISION_LOG}` shows where it is",
                    PROVISION_TIMEOUT.as_secs() / 60
                );
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    /// Write `share/` fresh, start the instance, wait for the guest.
    pub(super) async fn lima_start(&self, host: &Config) -> Result<()> {
        let name = self.lima_name();
        if self.lima_instance()?.is_none() {
            bail!("lima instance {name} does not exist; run `ssf vm build`");
        }
        self.ensure_key()?;
        self.write_share(host)?;
        self.apply_sizes()?;
        self.limactl_run(&["start", &name])?;
        // The template pins the port; an instance made from an older
        // template (or edited by hand) may listen elsewhere.
        if let Ok(Some(inst)) = self.lima_instance()
            && inst.ssh_local_port != 0
            && inst.ssh_local_port != self.cfg.ssh_port
        {
            warn!(
                "lima forwards {name}'s ssh to port {} but [vm] ssh_port is {}; `ssf vm build --force` remakes the instance to match",
                inst.ssh_local_port, self.cfg.ssh_port
            );
        }
        self.wait_for_provisioning().await?;
        if let Err(e) = self.wait_for_ssh(Duration::from_secs(120)).await {
            bail!(
                "{e:#}; the console is in {}",
                self.lima_console_log()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "lima's instance directory".into())
            );
        }
        let daemon = self.wait_for_daemon(Duration::from_secs(60)).await;
        self.report_up(daemon.as_deref());
        Ok(())
    }

    /// `vcpus` and `mem_mib` from `config.toml` reach a stopped instance
    /// through `limactl edit`, so a change and `ssf vm restart` apply them
    /// as they do under Firecracker (the template itself is only rendered
    /// by `ssf vm build`).
    fn apply_sizes(&self) -> Result<()> {
        let Some(inst) = self.lima_instance()? else {
            return Ok(());
        };
        if inst.is_running() {
            return Ok(());
        }
        let sizes = self.sizes();
        let want_mem = u64::from(sizes.mem_mib) << 20;
        let mut args = vec!["edit".to_string(), inst.name.clone()];
        if inst.cpus != 0 && inst.cpus != sizes.vcpus {
            args.push(format!("--cpus={}", sizes.vcpus));
        }
        if inst.memory != 0 && inst.memory != want_mem {
            args.push(format!("--memory={}", sizes.mem_mib as f64 / 1024.0));
        }
        if args.len() == 2 {
            return Ok(());
        }
        println!(
            "applying [vm] vcpus = {} and mem_mib = {} to lima instance {}",
            sizes.vcpus, sizes.mem_mib, inst.name
        );
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.limactl_run(&args)
    }

    /// `limactl stop`, and `-f` when that fails (lima gives the guest a
    /// few minutes to shut down first).
    pub(super) async fn lima_stop(&self) -> Result<()> {
        let name = self.lima_name();
        if !self.lima_running() {
            println!("VM {} is not running", self.cfg.name);
            return Ok(());
        }
        if let Err(e) = self.limactl_run(&["stop", &name]) {
            warn!("{e:#}; forcing it");
            self.limactl_run(&["stop", "-f", &name])?;
        }
        println!("VM {} stopped", self.cfg.name);
        Ok(())
    }

    /// `limactl disk resize` (the VM stopped); the guest's seed script
    /// grows the filesystem on the next boot.
    pub(super) fn lima_grow(&self, want: Option<u32>) -> Result<Option<u32>> {
        let disk = self.lima_disk_name();
        let d = self.lima_disk()?.with_context(|| {
            format!("lima disk {disk} does not exist yet; `ssf vm build` makes it at [vm] data_gib")
        })?;
        let current = gib_ceil(d.size);
        // The lima disks are lima's, not under `[vm] dir`: the warning
        // below has to be about the filesystem they are on.
        let (dir, _) = self.sizing_dir();
        let facts = HostFacts::probe(&dir)?;
        let rule = sizes_for(&facts).data_gib;
        let Some(target) = plan_grow(current, want, rule)? else {
            println!(
                "{disk} stays at {current} GiB{}",
                if want.is_none() {
                    format!(" (the rule for today's free space gives {rule} GiB)")
                } else {
                    String::new()
                }
            );
            return Ok(None);
        };
        if (u64::from(target) << 30).saturating_sub(d.size) > facts.free_bytes {
            warn!(
                "{target} GiB is more than the host has free ({} GiB on {}); a guest that fills the disk would see I/O errors before \"disk full\"",
                facts.free_bytes >> 30,
                facts.mount
            );
        }
        info!("resizing lima disk {disk}");
        self.limactl_run(&["disk", "resize", &disk, "--size", &format!("{target}GiB")])?;
        println!(
            "{disk} ({}) grown from {current} to {target} GiB; the guest grows its filesystem ({}) to match at the next boot (`ssf vm start`)",
            d.dir, d.mount_point
        );
        Ok(Some(target))
    }

    /// Delete the instance and create it again from its template; the
    /// data disk stays, and the next start provisions the fresh root.
    pub(super) fn lima_reset(&self) -> Result<()> {
        let name = self.lima_name();
        if self.lima_instance()?.is_some() {
            self.limactl_run(&["delete", "-f", &name])?;
        }
        self.lima_create()?;
        println!(
            "lima instance {name} re-created from {}; the data disk {} stays; `ssf vm start` provisions it again (a few minutes)",
            self.template_path().display(),
            self.lima_disk_name()
        );
        Ok(())
    }

    /// Delete the instance and the data disk (the VM's directory goes
    /// after this).
    pub(super) fn lima_destroy(&self) -> Result<()> {
        let name = self.lima_name();
        if self.lima_instance()?.is_some() {
            self.limactl_run(&["delete", "-f", &name])?;
            println!("deleted lima instance {name}");
        }
        let disk = self.lima_disk_name();
        if self.lima_disk()?.is_some() {
            self.limactl_run(&["disk", "delete", &disk])?;
            println!("deleted lima disk {disk}");
        }
        Ok(())
    }
}

/// Copy a directory tree (files keep their modes).
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for e in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let e = e?;
        let dest = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &dest)?;
        } else {
            std::fs::copy(e.path(), &dest)
                .with_context(|| format!("copying {}", e.path().display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BackendKind;

    fn vm() -> Vm {
        let mut cfg = Config::default();
        cfg.vm.dir = "/v".into();
        cfg.vm.name = "one".into();
        cfg.vm.backend = Some(BackendKind::Lima);
        cfg.vm.vcpus = Some(3);
        cfg.vm.mem_mib = Some(8192);
        cfg.vm.data_gib = Some(40);
        Vm::new(&cfg)
    }

    #[test]
    fn a_name_whose_disk_label_would_not_fit_is_refused() {
        assert_eq!(MAX_NAME_LEN, 7);
        assert!(check_name("default").is_ok());
        assert!(check_name("e2e").is_ok());
        let err = check_name("factory1").unwrap_err().to_string();
        assert!(err.contains("at most 7"), "{err}");
    }

    #[test]
    fn names_and_files_follow_the_vm_name() {
        let vm = vm();
        assert_eq!(vm.lima_name(), "ssf-one");
        assert_eq!(vm.lima_disk_name(), "ssf-one");
        assert_eq!(vm.share_dir(), PathBuf::from("/v/one/share"));
        assert_eq!(vm.template_path(), PathBuf::from("/v/one/lima.yaml"));
        assert_eq!(
            lima_env("one"),
            "SSF_VM_DATA_DISK=ssf-one\nSSF_VM_NAME=one\n"
        );
        assert_eq!(lima_arch("x86_64").unwrap(), "x86_64");
        assert_eq!(lima_arch("aarch64").unwrap(), "aarch64");
        assert!(lima_arch("riscv64").is_err());
        assert_eq!(gib_ceil(1 << 30), 1);
        assert_eq!(gib_ceil((1 << 30) + 1), 2);
        assert_eq!(gib_ceil(0), 0);
    }

    #[test]
    fn template_has_the_base_per_arch_the_sizes_the_mount_and_the_disk() {
        let vm = vm();
        let t = Template {
            disk: "ssf-one",
            share: Path::new("/v/one/share"),
            ssh_port: 2222,
            sizes: vm.sizes(),
            root_gib: 8,
            arch: "x86_64",
            image: None,
            vm_type: None,
            format_disk: true,
        };
        let y = render_template(&t);
        assert!(y.starts_with("# written by ssf;"), "{y}");
        assert!(y.contains("base:\n  - template:_images/archlinux\n"), "{y}");
        assert!(!y.contains("images:"), "{y}");
        assert!(!y.contains("vmType"), "{y}");
        assert!(!y.contains("mountType"), "{y}");
        assert!(y.contains("arch: x86_64\n"), "{y}");
        assert!(y.contains("cpus: 3\n"), "{y}");
        assert!(y.contains("memory: \"8192MiB\"\n"), "{y}");
        // root_gib under the floor is lifted to it.
        assert!(y.contains("disk: \"20GiB\"\n"), "{y}");
        assert!(
            y.contains("mounts:\n  - location: \"/v/one/share\"\n    mountPoint: /mnt/ssf\n    writable: false\n"),
            "{y}"
        );
        assert!(y.contains("localPort: 2222\n"), "{y}");
        assert!(y.contains("loadDotSSHPubKeys: false"), "{y}");
        assert!(
            y.contains("additionalDisks:\n  - name: ssf-one\n    format: true\n    fsType: ext4\n"),
            "{y}"
        );
        assert!(
            y.contains("containerd:\n  system: false\n  user: false\n"),
            "{y}"
        );
        assert!(y.contains("  - mode: system\n"), "{y}");
        assert!(
            y.contains("      exec bash /mnt/ssf/guest/lima-boot.sh\n"),
            "{y}"
        );
        // aarch64 boots Ubuntu; a set image replaces the base; vmType
        // and a larger root pass through.
        let y = render_template(&Template {
            arch: "aarch64",
            image: Some("https://example.com/arch.qcow2"),
            vm_type: Some("vz"),
            root_gib: 30,
            ..t.clone()
        });
        assert!(y.contains("vmType: vz\n"), "{y}");
        assert!(!y.contains("base:"), "{y}");
        assert!(
            y.contains(
                "images:\n  - location: \"https://example.com/arch.qcow2\"\n    arch: aarch64\n"
            ),
            "{y}"
        );
        assert!(y.contains("disk: \"30GiB\"\n"), "{y}");
        assert!(
            render_template(&Template {
                arch: "aarch64",
                ..t.clone()
            })
            .contains("template:_images/ubuntu-lts"),
        );
        // Through the VM: its own share dir and port.
        let y = vm.lima_template(true).unwrap();
        assert!(y.contains("location: \"/v/one/share\""), "{y}");
        assert!(y.contains("name: ssf-one"), "{y}");
    }

    #[test]
    fn only_the_build_that_makes_the_data_disk_lets_lima_format_it() {
        // lima's guest boot script formats an additional disk when it
        // cannot find the `lima-<disk>` label and `format` is true, then
        // mounts the partition by device either way. So the build that
        // creates the disk asks for a filesystem, and every template
        // after that (and the instance's own copy, through
        // `limactl edit --set`) says false: a disk that has lost its
        // label must then fail to mount rather than be reformatted.
        let vm = vm();
        let made = vm.lima_template(true).unwrap();
        assert!(
            made.contains(
                "additionalDisks:\n  - name: ssf-one\n    format: true\n    fsType: ext4\n"
            ),
            "{made}"
        );
        let kept = vm.lima_template(false).unwrap();
        assert!(
            kept.contains(
                "additionalDisks:\n  - name: ssf-one\n    format: false\n    fsType: ext4\n"
            ),
            "{kept}"
        );
        // Everything else about the two is the same.
        assert_eq!(made.replace("format: true", "format: false"), kept);
        // What flips the instance's own copy (yq syntax; `limactl help
        // yq-restrictions`).
        assert_eq!(FORMAT_OFF, ".additionalDisks[0].format = false");
    }

    #[test]
    fn the_provision_probe_cannot_match_itself() {
        let probe = provision_probe();
        assert!(
            probe.contains(&format!("pgrep -f '{PROVISION_PGREP}'")),
            "{probe}"
        );
        // `pgrep -f` matches whole command lines, and `limactl shell
        // <name> sh -c "<probe>"` puts this string in one. The bracket
        // classes exist so that what pgrep looks for does not occur in
        // what it is looking through: with them gone, the probe would
        // always report "running" and a dead provisioning would never be
        // caught. Undo the brackets to get what pgrep matches, and hold
        // that neither is in the probe.
        for plain in [
            PROVISION_PGREP
                .replace("[l]", "l")
                .replace("[.]", ".")
                .split('|')
                .next()
                .unwrap()
                .to_string(),
            "provision.sh".to_string(),
        ] {
            assert!(!probe.contains(&plain), "{probe} contains {plain}");
        }
        assert_eq!(PROVISION_PGREP, "[l]ima-boot[.]sh|[p]rovision[.]sh");
    }

    #[test]
    fn the_wait_ends_on_the_marker_and_on_three_idle_looks() {
        let seen = |out: &str| parse_probe(out);
        assert_eq!(
            seen("done\nlog\n"),
            Probe {
                done: true,
                log: true,
                running: false
            }
        );
        assert_eq!(seen(""), Probe::default());
        assert_eq!(
            seen("  log \n running \n"),
            Probe {
                done: false,
                log: true,
                running: true
            }
        );
        // The marker ends it whatever else the round saw.
        assert_eq!(
            provision_step(seen("done\nlog\nrunning\n"), 2),
            Step::Provisioned
        );
        // A log and nothing running: three rounds in a row, then failed.
        assert_eq!(provision_step(seen("log\n"), 0), Step::Wait(1));
        assert_eq!(provision_step(seen("log\n"), 1), Step::Wait(2));
        assert_eq!(provision_step(seen("log\n"), 2), Step::Failed);
        // A script running (or no log yet) puts the count back.
        assert_eq!(provision_step(seen("log\nrunning\n"), 2), Step::Wait(0));
        assert_eq!(provision_step(seen(""), 2), Step::Wait(0));
    }

    #[test]
    fn lima_home_and_the_disk_directory_follow_lima() {
        let home = Path::new("/home/me");
        assert_eq!(
            lima_home_from(None, Some(home)),
            Some(PathBuf::from("/home/me/.lima"))
        );
        assert_eq!(
            disks_dir_from(None, Some(home)),
            Some(PathBuf::from("/home/me/.lima/_disks"))
        );
        // LIMA_HOME wins, and a tilde in it is expanded.
        assert_eq!(
            disks_dir_from(Some("/elsewhere/lima"), Some(home)),
            Some(PathBuf::from("/elsewhere/lima/_disks"))
        );
        assert_eq!(
            disks_dir_from(Some("  "), Some(home)),
            Some(PathBuf::from("/home/me/.lima/_disks"))
        );
        assert_eq!(disks_dir_from(None, None), None);
    }

    #[test]
    fn limactl_json_lines_parse() {
        let text = r#"{"name":"ssf-default","status":"Running","dir":"/Users/me/.lima/ssf-default","vmType":"vz","arch":"aarch64","cpus":7,"memory":17179869184,"disk":21474836480,"sshLocalPort":2222,"sshAddress":"127.0.0.1","hostAgentPID":4242,"driverPID":4243}
{"name":"other","status":"Stopped","dir":"/Users/me/.lima/other","sshLocalPort":0}
not json at all
"#;
        let v = parse_instances(text);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "ssf-default");
        assert!(v[0].is_running());
        assert_eq!(v[0].dir, "/Users/me/.lima/ssf-default");
        assert_eq!(v[0].ssh_local_port, 2222);
        assert!(!v[1].is_running());
        assert!(v.iter().all(|i| i.name != "missing"));
        assert!(parse_instances("").is_empty());
        let d = parse_disks(
            r#"{"name":"ssf-default","size":21474836480,"format":"qcow2","dir":"/Users/me/.lima/_disks/ssf-default","instance":"","instanceDir":"","mountPoint":"/mnt/lima-ssf-default"}
"#,
        );
        assert_eq!(d.len(), 1);
        assert_eq!(gib_ceil(d[0].size), 20);
        assert_eq!(d[0].mount_point, "/mnt/lima-ssf-default");
        assert!(parse_disks("").is_empty());
    }

    #[test]
    fn grow_under_lima_plans_with_the_shared_rule() {
        // The same planner as Firecracker's, over lima's byte sizes.
        let current = gib_ceil(21474836480);
        assert_eq!(current, 20);
        assert_eq!(plan_grow(current, Some(40), 80).unwrap(), Some(40));
        assert_eq!(plan_grow(current, None, 80).unwrap(), Some(80));
        assert_eq!(plan_grow(current, Some(20), 80).unwrap(), None);
        assert!(plan_grow(current, Some(10), 80).is_err());
    }

    #[test]
    fn copy_dir_copies_the_tree_with_modes() {
        let dir = std::env::temp_dir().join(format!(
            "ssf-copy-dir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let from = dir.join("from");
        std::fs::create_dir_all(from.join("units")).unwrap();
        std::fs::write(from.join("lima-boot.sh"), "#!/bin/bash\n").unwrap();
        make_executable(&from.join("lima-boot.sh")).unwrap();
        std::fs::write(from.join("units/ssf.service"), "[Unit]\n").unwrap();
        let to = dir.join("to/guest");
        copy_dir(&from, &to).unwrap();
        assert_eq!(
            std::fs::read_to_string(to.join("units/ssf.service")).unwrap(),
            "[Unit]\n"
        );
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(to.join("lima-boot.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "{mode:o}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
