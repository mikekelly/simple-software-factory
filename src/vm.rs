//! The factory inside a VM: the daemon, herdr and every agent session run
//! in a guest, and the host keeps only what builds, starts, stops and
//! reaches it (`ssf vm ...`). Two backends (`[vm] backend`) run the guest:
//!
//! * Firecracker (the default on Linux; this file). Nothing needs root:
//!   Firecracker runs as the user given `/dev/kvm`, the guest's network is
//!   gvisor-tap-vsock (`gvproxy` on the host, a user-mode TCP/IP stack on
//!   the unix socket Firecracker maps to guest vsock port 1024;
//!   `gvforwarder` in the guest), and the images are made with `fakeroot`
//!   and `mkfs.ext4 -d`. Files under `[vm] dir` (`~/.local/share/ssf/vm`):
//!   the downloaded `firecracker`, `gvproxy`, `gvforwarder` and `vmlinux`,
//!   the root image `rootfs.ext4` that `ssf vm build` provisions from the
//!   Arch bootstrap tarball, and one directory per VM with its persistent
//!   `root.ext4` (a copy-on-write copy of the image), `data.ext4` (ssf's
//!   state, the clones and worktrees, mounted at `/var/lib/ssf`), the
//!   `seed.ext4` written at every start (this binary, the config rewritten
//!   for the guest, the bot token, the ssh key, the `[vm] files`),
//!   Firecracker's config and sockets, PID files and the serial console log.
//! * lima (the default on macOS; `lima.rs`): a `limactl` instance from a
//!   cloud image, provisioned by the same guest scripts on its first boot
//!   and seeded from a read-only host directory at every boot.
//!
//! What does not depend on the backend stays here: the seed tree, the
//! sizing rule, the ssh layer, the harness logins, `sync`, `attach`, `logs`
//! and the status. The guest is reached over ssh on `127.0.0.1:<ssh_port>`
//! with a key made per VM. With `[vm] enabled = true` the daemon-facing
//! commands are run inside the guest that way, so `ssf status --json` for
//! the bar widget and `ssf tell` from a terminal work as before; `ssf run`
//! on the host starts the VM and watches it, so the service is unchanged.

mod lima;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use tracing::{info, warn};

pub use crate::config::BackendKind;
use crate::config::{
    Config, Credential, DriverKind, GitConfig, SigningKey, VmConfig, expand_tilde,
};
use crate::platform;

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
/// What a whole-wait deadline adds to the limit the wait's own loop
/// keeps: the loop's message is the one a person normally reads, and the
/// outer deadline only catches a wait that has stopped making progress
/// altogether. See [`Vm::wait_for_ssh`].
const WAIT_BACKSTOP_MARGIN: Duration = Duration::from_secs(30);

/// Commands that act on the daemon and so run inside the guest when the
/// factory is there (`run` only as `run --once`; plain `run` supervises the
/// VM from the host).
pub const FORWARDED: [&str; 11] = [
    "status", "peers", "sub", "unsub", "subs", "tell", "release", "handover", "purge", "doctor",
    "run",
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

/// One VM: its config and the directory its files live in. The public
/// operations (`build`, `start`, `stop`, `running`, `grow`, `reset`,
/// `destroy`, `console_path`, `status`) switch on `backend()`; the
/// Firecracker side is the `fc_*` methods here, the lima side `lima.rs`.
#[derive(Clone)]
pub struct Vm {
    pub cfg: VmConfig,
    /// `[vm] dir`, expanded.
    pub base: PathBuf,
    /// `<base>/<name>`.
    pub dir: PathBuf,
    /// lima's own home (`$LIMA_HOME`, else `~/.lima`), where the
    /// instance and its external data disk live -- not under `[vm] dir`.
    /// `None` when there is no home directory to derive it from.
    pub lima_home: Option<PathBuf>,
    /// The `ssf` binary the guest gets: this one, normally.
    pub binary: Option<PathBuf>,
}

/// What `ssf vm status` reports.
#[derive(Debug, Clone, Serialize)]
pub struct VmStatus {
    pub enabled: bool,
    pub name: String,
    pub dir: String,
    /// `firecracker` or `lima`.
    pub backend: String,
    /// The lima instance (`ssf-<name>`); null under Firecracker.
    pub instance: Option<String>,
    /// lima's own directory for the instance, once it exists.
    pub lima_dir: Option<String>,
    /// Firecracker: the root image is built; lima: the instance exists.
    pub image: bool,
    /// Whether the host knows the VM is running. Null when lima could
    /// not answer the probe; use `probe_error` for why.
    pub running: Option<bool>,
    /// Firecracker's and gvproxy's PIDs; null under lima.
    pub firecracker_pid: Option<u32>,
    pub gvproxy_pid: Option<u32>,
    pub ssh_port: u16,
    pub ssh: bool,
    /// `systemctl is-active ssf` in the guest, when reachable.
    pub daemon: Option<String>,
    /// Per-harness login state in the guest (empty when not reachable).
    pub logins: Vec<LoginState>,
    /// The guest's vCPUs and memory (`[vm]`, or the rule for this machine).
    pub vcpus: u32,
    pub mem_mib: u32,
    /// The data disk's cap: the file's size once it exists, else what a
    /// start would make.
    pub data_gib: u32,
    /// The data disk as the guest sees it (`df`), when reachable.
    pub data: Option<DiskUse>,
    /// The host tooling this backend needs (`limactl` and qemu, or
    /// `/dev/kvm`). Null inside the guest, whose host owns the VM.
    pub tooling: Option<Tooling>,
    /// Why lima could not be asked about the instance, when it could not
    /// be. Absent after a successful probe. On failure, `running` is
    /// null; `instance`, `lima_dir` and `image` must not be read as proof
    /// that the instance is missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_error: Option<String>,
}

/// Where the data disk is mounted in the guest.
pub const GUEST_DATA_DIR: &str = "/var/lib/ssf";

/// What a backend holds of one VM, from [`Vm::survey`].
///
/// Under Firecracker the VM *is* the files under `[vm] dir`, so that
/// directory answers all of it. Under lima it does not: the instance and
/// the data disk live in lima's own home, and the directory holds only
/// the generated template, the ssh key and the share. Reading presence
/// off the directory let `ssf uninstall` report "no VM" over a stopped
/// lima instance whose directory had been removed by hand, or whose
/// `[vm] dir` had since been changed, skip the destroy step, and leave
/// the instance and its data disk -- the clones and worktrees with them
/// -- for the person to find with `limactl list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Survey {
    /// Is there anything [`Vm::destroy`] would take? `None` when the
    /// backend could not be asked -- which is not "no", and must not be
    /// reported as one.
    pub present: Option<bool>,
    /// Is the guest up? `None` when the probe itself could not be made,
    /// as [`Vm::running_state`] means it.
    pub running: Option<bool>,
    /// Is there a guest `ssf vm start` could bring up? Under lima a data
    /// disk outlives a deleted instance: there is then something to
    /// destroy and nothing that can mount it to look inside first, so
    /// "start it and try again" is no remedy.
    pub startable: bool,
    /// Is there a data disk that may hold clones and worktrees -- the
    /// only part of a VM whose loss cannot be undone? `None` when that
    /// could not be established. `ssf uninstall` refuses over anything
    /// but `Some(false)`; the leftovers in `[vm] dir` are ssf's own and
    /// hold nothing of anyone's work.
    pub data: Option<bool>,
}

/// Is it there? `None` when nobody could tell: only `NotFound` means
/// absent, and a denied or failing `stat` is not an answer.
pub(crate) fn there(p: &Path) -> Option<bool> {
    match std::fs::metadata(p) {
        Ok(_) => Some(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// What the sizing rule reads off this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFacts {
    /// Logical CPUs.
    pub cpus: u32,
    /// RAM in MiB.
    pub mem_mib: u64,
    /// Free space, in bytes, on the filesystem that holds the directory
    /// [`HostFacts::probe`] was given (see [`sizing_dir`]: not the same
    /// directory under both backends).
    pub free_bytes: u64,
    /// That filesystem's mount point, for the message.
    pub mount: String,
}

impl HostFacts {
    /// Read this machine: the CPUs this process may use, `/proc/meminfo`
    /// (`sysctl hw.memsize` on macOS), and the free space where `dir` is
    /// (or would be: its nearest existing ancestor).
    // The statvfs field types differ between libc targets.
    #[allow(clippy::useless_conversion)]
    pub fn probe(dir: &Path) -> Result<Self> {
        let cpus = std::thread::available_parallelism()
            .map(|n| u32::try_from(n.get()).unwrap_or(u32::MAX))
            .unwrap_or(1);
        let mem_mib = if platform::is_macos() {
            let out = Command::new("sysctl")
                .args(["-n", "hw.memsize"])
                .output()
                .context("running sysctl hw.memsize")?;
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse::<u64>()
                .context("reading hw.memsize")?
                >> 20
        } else {
            let meminfo =
                std::fs::read_to_string("/proc/meminfo").context("reading /proc/meminfo")?;
            parse_meminfo(&meminfo)
                .context("no MemTotal in /proc/meminfo")?
                .total_kib
                / 1024
        };
        let here = existing_ancestor(dir);
        let st = statvfs(&here)?;
        Ok(Self {
            cpus,
            mem_mib,
            free_bytes: u64::from(st.f_bavail) * u64::from(st.f_frsize),
            mount: mount_point_of(&here),
        })
    }
}

/// The guest's sizes: what `[vm]` says, or what the rule chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Sizes {
    pub vcpus: u32,
    pub mem_mib: u32,
    pub data_gib: u32,
}

impl Sizes {
    /// The floor under every rule, and what a machine that cannot be read
    /// gets.
    pub const MIN: Sizes = Sizes {
        vcpus: 2,
        mem_mib: 4096,
        data_gib: 20,
    };
}

/// The sizing rule: the host's CPUs minus one, half its RAM (rounded down
/// to 256 MiB), half the free space where the VM lives; never under
/// `Sizes::MIN`. The data disk is sparse, so its size reserves nothing.
pub fn sizes_for(facts: &HostFacts) -> Sizes {
    let mem = u32::try_from(facts.mem_mib / 2 / 256 * 256).unwrap_or(u32::MAX);
    let data = u32::try_from((facts.free_bytes / 2) >> 30).unwrap_or(u32::MAX);
    Sizes {
        vcpus: facts.cpus.saturating_sub(1).max(Sizes::MIN.vcpus),
        mem_mib: mem.max(Sizes::MIN.mem_mib),
        data_gib: data.max(Sizes::MIN.data_gib),
    }
}

/// Where the sizing rule and `ssf vm grow` measure free space, and how a
/// message names it. Under Firecracker the VM's own directory: its disks
/// are files under `[vm] dir`. Under lima the directory lima keeps its
/// disks in (`$LIMA_HOME/_disks`, else `~/.lima/_disks`), which can be on
/// another volume entirely — measuring `[vm] dir` there would size the
/// data disk against a filesystem that never fills up. Falls back to the
/// VM's directory when lima's home cannot be worked out.
pub fn sizing_dir_for(
    backend: BackendKind,
    base: &Path,
    lima_disks: Option<PathBuf>,
) -> (PathBuf, &'static str) {
    match (backend, lima_disks) {
        (BackendKind::Lima, Some(d)) => (d, "lima's disk directory"),
        _ => (base.to_path_buf(), "[vm] dir"),
    }
}

/// [`sizing_dir_for`] on this machine.
pub fn sizing_dir(backend: BackendKind, base: &Path) -> (PathBuf, &'static str) {
    sizing_dir_for(backend, base, lima::disks_dir())
}

/// How often [`Vm::supervise`] asks whether the guest is still up.
///
/// Under Firecracker the question is a PID file and a `/proc` lookup, so
/// five seconds costs nothing. Under lima it forks a ~60 MB Go binary
/// (`limactl list --json`) and takes a lock in lima's home, which is too
/// much to do every five seconds on a laptop for the whole time the
/// factory runs -- and every one of those forks is a chance to fail and
/// be misread. Half a minute is still far quicker than a person notices a
/// dead VM, and `ssf status` answers the same question on demand.
pub fn supervise_interval(backend: BackendKind) -> Duration {
    match backend {
        BackendKind::Firecracker => Duration::from_secs(5),
        BackendKind::Lima => Duration::from_secs(30),
    }
}

/// How many rounds in a row [`Vm::supervise`] may fail to get an answer
/// out of the probe before it gives up. A probe that could not be made
/// says nothing, so one of them must not end the supervision -- but a
/// probe that can never be made says nothing for ever, and the loop that
/// only warned left `ssf run` "supervising" a VM it had not heard about
/// for hours while the service read active. Ten rounds is under a minute
/// under Firecracker and five minutes under lima: long enough to sit out
/// a busy laptop or a lima home someone else has locked, short enough
/// that the daemon does not pretend all day.
const MAX_UNANSWERED_PROBES: u32 = 10;

/// What the supervisor says when the probe has stopped answering: the
/// question, the tool that could not answer it, and how long it has been
/// like that. The warnings from the probe itself are above it in the log.
fn cannot_tell_error(backend: BackendKind, rounds: u32, every: Duration) -> anyhow::Error {
    let tool = match backend {
        BackendKind::Firecracker => "reading the VM's pid file",
        BackendKind::Lima => "`limactl list --json`",
    };
    anyhow::anyhow!(
        "cannot tell whether the VM is running: {tool} has not answered for {rounds} tries in a row ({}), and supervising a VM that cannot be asked about is not supervising anything. `ssf vm status` asks the same question by hand",
        lima::human_duration(every * rounds)
    )
}

/// A tool the host needs to run the VM under a backend, for `ssf doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    /// A program to find on PATH (or an absolute path from `[vm]
    /// limactl`), or a device to open.
    pub name: String,
    /// Open it rather than look for it: `/dev/kvm` is there on every
    /// Linux with the module loaded, and the question is whether this
    /// user may use it.
    pub device: bool,
    /// What to do when it is not usable.
    pub install: String,
}

/// Does lima drive this VM with qemu? On Linux always: qemu is lima's
/// only driver there. On macOS only when `[vm] vm_type` asks for it --
/// lima's own default is `vz`, the Virtualization framework, which needs
/// no qemu at all. Keyed on what the VM will use, not on the operating
/// system: a Mac with `vm_type = "qemu"` and no qemu installed used to
/// pass every check ssf makes and then fail inside `limactl create`.
pub fn lima_uses_qemu(os: &str, vm_type: Option<&str>) -> bool {
    if os == "macos" {
        vm_type == Some("qemu")
    } else {
        true
    }
}

/// What to install when `qemu-system-<arch>` is missing.
fn qemu_install_hint(os: &str, arch: &str) -> String {
    if os == "macos" {
        return "install qemu (`brew install qemu`), or unset [vm] vm_type to let lima use the Virtualization framework".to_string();
    }
    format!(
        "install qemu (Arch: `qemu-full` or `qemu-base`; Debian/Ubuntu: `qemu-system-{}`; Fedora: `qemu-system-{}`)",
        if arch == "x86_64" { "x86" } else { "arm" },
        if arch == "x86_64" { "x86" } else { "aarch64" },
    )
}

/// What a host running `os` on `arch` needs for `backend`: `limactl` and,
/// whenever lima will drive the VM with qemu ([`lima_uses_qemu`]), qemu
/// for the architecture; a usable `/dev/kvm` under Firecracker.
/// `limactl` is `[vm] limactl` where that is set. Pure: [`probe_tools`]
/// goes looking.
pub fn backend_tools(
    backend: BackendKind,
    os: &str,
    arch: &str,
    limactl: Option<&str>,
    vm_type: Option<&str>,
) -> Vec<Tool> {
    match backend {
        BackendKind::Firecracker => vec![Tool {
            name: "/dev/kvm".into(),
            device: true,
            install: "Firecracker runs the guest through KVM: on Debian and Ubuntu `sudo usermod -aG kvm $USER` and a new login, elsewhere check that the kvm module is loaded; a machine without KVM needs [vm] backend = \"lima\"".into(),
        }],
        BackendKind::Lima => {
            let mut v = vec![Tool {
                // `[vm] limactl` is a path, and `~/bin/limactl` is a path
                // the run-time side expands (`Vm::limactl`) -- so it is
                // expanded here too, or `ssf doctor` and `ssf vm status`
                // would look a tilde up on PATH and report a limactl that
                // works as "not installed".
                name: limactl.map_or_else(
                    || "limactl".to_string(),
                    |p| expand_tilde(p).to_string_lossy().into_owned(),
                ),
                device: false,
                install: format!(
                    "install lima {} or newer (`brew install lima` on macOS, the `lima` package or lima's release tarball on Linux) or set [vm] limactl to it",
                    lima::MIN_LIMA
                ),
            }];
            if lima_uses_qemu(os, vm_type) {
                v.push(Tool {
                    name: format!("qemu-system-{arch}"),
                    device: false,
                    install: qemu_install_hint(os, arch),
                });
            }
            v
        }
    }
}

/// Where each tool is, or `None` when it is not usable here.
pub fn probe_tools(tools: &[Tool]) -> Vec<Option<String>> {
    tools
        .iter()
        .map(|t| {
            if t.device {
                // Firecracker opens it read-write; being able to do the
                // same is the whole question.
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&t.name)
                    .ok()
                    .map(|_| t.name.clone())
            } else {
                which(&t.name).map(|p| p.display().to_string())
            }
        })
        .collect()
}

/// What `ssf doctor` and `ssf vm status` say about a backend's host
/// tooling: whether it is all there, and where each tool was found, or
/// what to install for the ones that are missing. The backend is not
/// named here: both callers have said it already.
pub fn backend_tooling_line(tools: &[Tool], found: &[Option<String>]) -> (bool, String) {
    let detail = |t: &Tool, f: &Option<String>| match (t.device, f) {
        (true, Some(_)) => format!("{} usable", t.name),
        (true, None) => format!("{} not usable by you", t.name),
        (false, Some(p)) => format!("{} at {p}", t.name),
        (false, None) => format!("{} not installed", t.name),
    };
    let pairs: Vec<(&Tool, &Option<String>)> = tools.iter().zip(found.iter()).collect();
    let missing: Vec<_> = pairs.iter().filter(|(_, f)| f.is_none()).collect();
    if missing.is_empty() {
        let all = pairs
            .iter()
            .map(|(t, f)| detail(t, f))
            .collect::<Vec<_>>()
            .join(", ");
        return (true, all);
    }
    let bad = missing
        .iter()
        .map(|(t, f)| format!("{}; {}", detail(t, f), t.install))
        .collect::<Vec<_>>()
        .join("; ");
    (false, bad)
}

/// The backend's host tooling as `ssf vm status` reports it.
#[derive(Debug, Clone, Serialize)]
pub struct Tooling {
    /// Every tool the backend needs is here.
    pub ok: bool,
    /// Where each tool is, or what to install for the ones missing.
    pub detail: String,
}

/// What `ssf vm build` settled on, and whether the file changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    pub sizes: Sizes,
    /// Where each of vcpus, mem_mib, data_gib came from.
    pub sources: [&'static str; 3],
    pub changed: bool,
}

/// Settle the `[vm]` sizes for a build: a flag (`--vcpus`, `--mem-mib`,
/// `--data-gib`, in that order) is written; a key set in the file stays;
/// a key set nowhere gets `rule` and is written.
pub fn choose_sizes(cfg: &mut VmConfig, flags: [Option<u32>; 3], rule: Sizes) -> Chosen {
    let mut changed = false;
    let mut pick = |slot: &mut Option<u32>, flag: Option<u32>, rule: u32| -> (u32, &'static str) {
        match (flag, *slot) {
            (Some(f), was) => {
                changed |= was != Some(f);
                *slot = Some(f);
                (f, "from the flag")
            }
            (None, Some(v)) => (v, "set in config.toml"),
            (None, None) => {
                *slot = Some(rule);
                changed = true;
                (rule, "from this machine")
            }
        }
    };
    let (vcpus, vs) = pick(&mut cfg.vcpus, flags[0], rule.vcpus);
    let (mem_mib, ms) = pick(&mut cfg.mem_mib, flags[1], rule.mem_mib);
    let (data_gib, ds) = pick(&mut cfg.data_gib, flags[2], rule.data_gib);
    Chosen {
        sizes: Sizes {
            vcpus,
            mem_mib,
            data_gib,
        },
        sources: [vs, ms, ds],
        changed,
    }
}

/// What `ssf vm grow` does with a disk of `current` GiB: `want`, or the
/// rule for today. Smaller than today is refused; the same size is
/// nothing to do.
pub fn plan_grow(current: u32, want: Option<u32>, rule: u32) -> Result<Option<u32>> {
    let target = want.unwrap_or(rule);
    match target.cmp(&current) {
        std::cmp::Ordering::Less if want.is_some() => bail!(
            "the data disk is {current} GiB and {target} GiB would shrink it, which `ssf vm grow` does not do (a smaller disk means a new VM: `ssf vm destroy`)"
        ),
        std::cmp::Ordering::Less | std::cmp::Ordering::Equal => Ok(None),
        std::cmp::Ordering::Greater => Ok(Some(target)),
    }
}

/// The data disk is called full from here on (`ssf doctor`).
pub const DATA_FULL_PCT: u8 = 85;

/// A filesystem's use, as `df` counts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DiskUse {
    pub used_bytes: u64,
    pub avail_bytes: u64,
    pub size_bytes: u64,
}

impl DiskUse {
    /// `df`'s Use%: used against what is used or still available (the
    /// reserved blocks count for neither).
    pub fn pct(&self) -> u8 {
        let total = self.used_bytes + self.avail_bytes;
        if total == 0 {
            return 0;
        }
        u8::try_from(self.used_bytes * 100 / total).unwrap_or(100)
    }

    pub fn is_full(&self) -> bool {
        self.pct() >= DATA_FULL_PCT
    }

    pub fn describe(&self) -> String {
        format!(
            "{:.1} of {:.0} GiB used ({}%)",
            gib(self.used_bytes),
            gib(self.size_bytes),
            self.pct()
        )
    }
}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / f64::from(1u32 << 30)
}

/// The last line of `df -B1 --output=used,avail,size <path>`.
pub fn parse_df(text: &str) -> Option<DiskUse> {
    let line = text.lines().rev().find(|l| !l.trim().is_empty())?;
    let mut f = line.split_whitespace().map(|n| n.parse::<u64>().ok());
    Some(DiskUse {
        used_bytes: f.next()??,
        avail_bytes: f.next()??,
        size_bytes: f.next()??,
    })
}

/// The use of the filesystem holding `path`, here.
// The statvfs field types differ between libc targets.
#[allow(clippy::useless_conversion)]
pub fn disk_use(path: &Path) -> Result<DiskUse> {
    let st = statvfs(path)?;
    let frsize = u64::from(st.f_frsize);
    let blocks = u64::from(st.f_blocks);
    let bfree = u64::from(st.f_bfree);
    Ok(DiskUse {
        used_bytes: blocks.saturating_sub(bfree) * frsize,
        avail_bytes: u64::from(st.f_bavail) * frsize,
        size_bytes: blocks * frsize,
    })
}

/// What `/proc/meminfo` says, in KiB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemInfo {
    pub total_kib: u64,
    pub available_kib: u64,
    pub swap_total_kib: u64,
    pub swap_free_kib: u64,
}

impl MemInfo {
    pub fn swapped_kib(&self) -> u64 {
        self.swap_total_kib.saturating_sub(self.swap_free_kib)
    }

    /// Short of memory: under a tenth available, or anything swapped out.
    pub fn is_short(&self) -> bool {
        self.total_kib > 0 && (self.available_kib * 10 < self.total_kib || self.swapped_kib() > 0)
    }

    pub fn describe(&self) -> String {
        let mut s = format!(
            "{} of {} MiB available",
            self.available_kib / 1024,
            self.total_kib / 1024
        );
        if self.swapped_kib() > 0 {
            s.push_str(&format!(", {} MiB swapped out", self.swapped_kib() / 1024));
        }
        s
    }
}

pub fn parse_meminfo(text: &str) -> Option<MemInfo> {
    let mut m = MemInfo::default();
    let mut total = false;
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let Ok(n) = v.trim().trim_end_matches("kB").trim().parse::<u64>() else {
            continue;
        };
        match k {
            "MemTotal" => {
                m.total_kib = n;
                total = true;
            }
            "MemAvailable" => m.available_kib = n,
            "SwapTotal" => m.swap_total_kib = n,
            "SwapFree" => m.swap_free_kib = n,
            _ => {}
        }
    }
    total.then_some(m)
}

fn statvfs(path: &Path) -> Result<libc::statvfs> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: a valid C string and a zeroed struct statvfs fills in.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("statvfs {}", path.display()));
    }
    Ok(st)
}

/// `p`, or the nearest ancestor that exists.
fn existing_ancestor(p: &Path) -> PathBuf {
    let mut q = p;
    while !q.exists() {
        q = q.parent().unwrap_or(Path::new("/"));
    }
    q.to_path_buf()
}

/// The mount point of the filesystem holding `p` (the longest one in
/// `/proc/self/mounts` that is a prefix of it; on macOS, the highest
/// ancestor on the same filesystem), or `p` itself.
fn mount_point_of(p: &Path) -> String {
    let p = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    if platform::is_macos() {
        let Ok(here) = statvfs(&p) else {
            return p.to_string_lossy().to_string();
        };
        let mut top = p.clone();
        while let Some(parent) = top.parent() {
            match statvfs(parent) {
                Ok(st) if st.f_fsid == here.f_fsid => top = parent.to_path_buf(),
                _ => break,
            }
        }
        return top.to_string_lossy().to_string();
    }
    std::fs::read_to_string("/proc/self/mounts")
        .ok()
        .and_then(|m| {
            m.lines()
                .filter_map(|l| l.split_whitespace().nth(1))
                .map(|mp| mp.replace("\\040", " "))
                .filter(|mp| p.starts_with(mp))
                .max_by_key(|mp| mp.len())
        })
        .unwrap_or_else(|| p.to_string_lossy().to_string())
}

/// Make the ext4 image at `disk` `bytes` long: check it, lengthen the
/// file, resize the filesystem to fill it.
pub fn grow_image(disk: &Path, bytes: u64) -> Result<()> {
    let out = Command::new("e2fsck")
        .args(["-f", "-p"])
        .arg(disk)
        .output()
        .context("running e2fsck (is e2fsprogs installed?)")?;
    // 0 clean, 1 fixed something, 2 fixed and would want a reboot (an
    // offline image: nothing to us).
    if !matches!(out.status.code(), Some(0..=2)) {
        bail!(
            "e2fsck {} failed ({}): {}{}; run `e2fsck -f {}` by hand",
            disk.display(),
            out.status,
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim(),
            disk.display()
        );
    }
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(disk)
        .with_context(|| format!("opening {}", disk.display()))?;
    f.set_len(bytes)?;
    drop(f);
    run_ok(Command::new("resize2fs").arg(disk), "resize2fs")
}

impl Vm {
    pub fn new(cfg: &Config) -> Self {
        let base = expand_tilde(&cfg.vm.dir);
        let dir = base.join(&cfg.vm.name);
        Self {
            cfg: cfg.vm.clone(),
            base,
            dir,
            lima_home: lima::lima_home(),
            binary: None,
        }
    }

    /// The backend in effect: `[vm] backend`, else Firecracker on Linux
    /// and lima on macOS.
    pub fn backend(&self) -> BackendKind {
        self.cfg
            .backend
            .unwrap_or_else(BackendKind::platform_default)
    }

    /// The serial console log: Firecracker's `console.log` next to the
    /// disks, or lima's `serial.log` in the instance directory.
    pub fn console_path(&self) -> Result<PathBuf> {
        match self.backend() {
            BackendKind::Firecracker => Ok(self.console_log()),
            BackendKind::Lima => self.lima_console_log(),
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

    // ---- sizes ----

    /// The directory whose free space this VM is sized against, and how
    /// to name it: [`sizing_dir`] for this VM's backend.
    pub fn sizing_dir(&self) -> (PathBuf, &'static str) {
        sizing_dir(self.backend(), &self.base)
    }

    /// The sizes this VM runs at: `[vm]` where set, the rule for this
    /// machine where not (the minimums when the machine cannot be read).
    pub fn sizes(&self) -> Sizes {
        let c = &self.cfg;
        let rule = if c.vcpus.is_none() || c.mem_mib.is_none() || c.data_gib.is_none() {
            HostFacts::probe(&self.sizing_dir().0)
                .map(|f| sizes_for(&f))
                .unwrap_or(Sizes::MIN)
        } else {
            Sizes::MIN
        };
        Sizes {
            vcpus: c.vcpus.unwrap_or(rule.vcpus),
            mem_mib: c.mem_mib.unwrap_or(rule.mem_mib),
            data_gib: c.data_gib.unwrap_or(rule.data_gib),
        }
    }

    /// The data disk's cap in GiB: the disk's size once it exists, else
    /// what a start would make.
    pub fn data_cap_gib(&self) -> u32 {
        match self.backend() {
            BackendKind::Firecracker => match std::fs::metadata(self.data_disk()) {
                Ok(m) => u32::try_from(m.len().div_ceil(1 << 30)).unwrap_or(u32::MAX),
                Err(_) => self.sizes().data_gib,
            },
            BackendKind::Lima => self.lima_data_cap_gib(),
        }
    }

    /// Enlarge the data disk to `want` GiB, or to the rule for today's
    /// free space, keeping what is on it, with the VM stopped. Never
    /// shrinks. Returns the new size, or `None` when there was nothing to
    /// do. Firecracker: the filesystem checked, the file lengthened, the
    /// filesystem resized to fill it. lima: `limactl disk resize`; the
    /// guest's seed script resizes the filesystem on the next boot.
    pub fn grow(&self, want: Option<u32>) -> Result<Option<u32>> {
        if self.running() {
            bail!(
                "VM {} is running; stop it first (`{}` when the service owns it, else `ssf vm stop`), grow, then start it again",
                self.cfg.name,
                platform::service_hint("stop")
            );
        }
        match self.backend() {
            BackendKind::Firecracker => self.fc_grow(want),
            BackendKind::Lima => self.lima_grow(want),
        }
    }

    fn fc_grow(&self, want: Option<u32>) -> Result<Option<u32>> {
        let disk = self.data_disk();
        let meta = std::fs::metadata(&disk).with_context(|| {
            format!(
                "{} does not exist yet; `ssf vm start` makes the data disk at [vm] data_gib",
                disk.display()
            )
        })?;
        let current = u32::try_from(meta.len().div_ceil(1 << 30)).unwrap_or(u32::MAX);
        let facts = HostFacts::probe(&self.base)?;
        let rule = sizes_for(&facts).data_gib;
        let Some(target) = plan_grow(current, want, rule)? else {
            println!(
                "{} stays at {current} GiB{}",
                disk.display(),
                if want.is_none() {
                    format!(" (the rule for today's free space gives {rule} GiB)")
                } else {
                    String::new()
                }
            );
            return Ok(None);
        };
        // What the guest could still write, against what the host has.
        let allocated = std::os::unix::fs::MetadataExt::blocks(&meta) * 512;
        if (u64::from(target) << 30).saturating_sub(allocated) > facts.free_bytes {
            warn!(
                "{target} GiB is more than the host has free ({} GiB on {}); a guest that fills the disk would see I/O errors before \"disk full\"",
                facts.free_bytes >> 30,
                facts.mount
            );
        }
        info!("checking and resizing {}", disk.display());
        grow_image(&disk, u64::from(target) << 30)?;
        println!("{} grown from {current} to {target} GiB", disk.display());
        Ok(Some(target))
    }

    /// `df` of the data disk inside the guest.
    pub fn guest_disk_use(&self) -> Result<DiskUse> {
        let out = self.ssh_output(&["df", "-B1", "--output=used,avail,size", GUEST_DATA_DIR])?;
        parse_df(&out).context("unexpected df output")
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

    /// Is the guest up? Firecracker: its PID file names a live
    /// firecracker; lima: `limactl list` says `Running`. A probe that
    /// could not be made counts as not running; use
    /// [`Vm::running_state`] where that difference matters.
    pub fn running(&self) -> bool {
        self.running_state().unwrap_or(false)
    }

    /// [`Vm::running`], with "the question could not be asked" kept apart
    /// from "no": `None` when the probe itself failed. Under Firecracker
    /// the probe is a PID file read, which answers either way; under lima
    /// it forks `limactl`, and one fork that fails is not the guest
    /// exiting. The callers that act on "the VM is gone" -- the
    /// supervisor and the ssh wait -- use this, so that a transient
    /// `limactl` failure cannot end them with "the VM exited".
    pub fn running_state(&self) -> Option<bool> {
        match self.backend() {
            BackendKind::Firecracker => Some(self.firecracker_pid().is_some()),
            BackendKind::Lima => self.lima_running_state(),
        }
    }

    /// The same question as [`Vm::running_state`], asked the way the gate
    /// in front of every forwarded command needs it: the reason comes
    /// back with a failure, so the command can say why it cannot tell
    /// rather than leaving it in the log, and the lima probe is held to
    /// [`lima::LIVENESS_LIMIT`] rather than the listing's own bound,
    /// because a person is waiting on this one and an answer it does not
    /// get is one it carries on without.
    pub fn running_now(&self) -> Result<bool> {
        match self.backend() {
            BackendKind::Firecracker => Ok(self.firecracker_pid().is_some()),
            BackendKind::Lima => self.lima_running_probe(lima::LIVENESS_LIMIT),
        }
    }

    /// What is here of this VM, asked of the backend in one pass:
    /// `ssf uninstall` needs every answer and each one costs a `limactl`
    /// fork under lima, so they are taken together.
    pub fn survey(&self) -> Survey {
        match self.backend() {
            BackendKind::Firecracker => {
                let dir = self.dir.exists();
                let running = self.firecracker_pid().is_some();
                Survey {
                    present: Some(dir || running),
                    running: Some(running),
                    startable: dir,
                    // `there`, not `exists()`: a `[vm] dir` that
                    // could not be read is not a VM without a data
                    // disk. `present` keeps `exists()` -- `Some(false)`
                    // there skips the destroy rather than running it.
                    data: there(&self.data_disk()),
                }
            }
            BackendKind::Lima => self.lima_survey(),
        }
    }

    // ---- build ----

    /// Make the guest: Firecracker downloads what is missing, makes the
    /// base image from the bootstrap tarball and boots it once to
    /// provision it; lima creates the instance from a cloud image and
    /// boots it once so the guest scripts provision it.
    pub async fn build(&self, host: &Config, force: bool) -> Result<()> {
        match self.backend() {
            BackendKind::Firecracker => self.fc_build(force).await,
            BackendKind::Lima => self.lima_build(host, force).await,
        }
    }

    async fn fc_build(&self, force: bool) -> Result<()> {
        if std::env::consts::OS != "linux" || std::env::consts::ARCH != "x86_64" {
            bail!(
                "the Firecracker backend is Linux x86_64 only (Firecracker, gvproxy and the guest kernel are downloaded for it); this machine is {} {}; use `ssf config set vm.backend lima`",
                std::env::consts::OS,
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
        let herdr = which(
            &crate::config::herdr_command_path(&crate::config::HerdrConfig::default().command)
                .to_string_lossy(),
        )
        .or_else(|| which(&crate::config::herdr_command_path("herdr").to_string_lossy()))
        .context("herdr is not installed on this machine; the image takes its binary from here")?;
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
        let sizes = self.sizes();
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
                "vcpu_count": sizes.vcpus,
                "mem_size_mib": sizes.mem_mib,
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

    /// Start the VM with a fresh seed and wait for ssh and the daemon.
    pub async fn start(&self, host: &Config) -> Result<()> {
        if self.running() {
            println!("VM {} is already running", self.cfg.name);
            return Ok(());
        }
        match self.backend() {
            BackendKind::Firecracker => self.fc_start(host).await,
            BackendKind::Lima => self.lima_start(host).await,
        }
    }

    /// Stop the VM cleanly.
    pub async fn stop(&self) -> Result<()> {
        match self.backend() {
            BackendKind::Firecracker => self.fc_stop().await,
            BackendKind::Lima => self.lima_stop().await,
        }
    }

    /// The per-VM ssh key, made on first use.
    fn ensure_key(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        set_mode(&self.dir, 0o700)?;
        if !self.key().exists() {
            run_ok(
                Command::new("ssh-keygen")
                    .args(["-q", "-t", "ed25519", "-N", "", "-C", "ssf-vm", "-f"])
                    .arg(self.key()),
                "ssh-keygen",
            )?;
        }
        Ok(())
    }

    /// Print how the guest came up, after `wait_for_daemon`.
    fn report_up(&self, daemon: Option<&str>) {
        println!(
            "VM {} is up: ssh -p {} (ssf vm ssh), daemon {}",
            self.cfg.name,
            self.cfg.ssh_port,
            daemon.unwrap_or("unknown")
        );
        if daemon != Some("active") {
            eprintln!("the guest daemon is not active; `ssf vm logs` has its journal");
        }
    }

    /// Firecracker: make the disks if missing, write the seed disk, start
    /// gvproxy and Firecracker detached.
    async fn fc_start(&self, host: &Config) -> Result<()> {
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
        self.ensure_key()?;
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
            let gib = self.sizes().data_gib;
            info!("making {} ({gib} GiB, sparse)", self.data_disk().display());
            let f = std::fs::File::create(self.data_disk())?;
            f.set_len(u64::from(gib) << 30)?;
            drop(f);
            run_ok(
                Command::new("mkfs.ext4")
                    .args(["-q", "-L", "ssf-data"])
                    .arg(self.data_disk()),
                "mkfs.ext4",
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
        self.report_up(daemon.as_deref());
        Ok(())
    }

    /// Shut the guest down (Ctrl-Alt-Del through Firecracker's API, which
    /// systemd turns into a reboot that ends the VM), then gvproxy.
    async fn fc_stop(&self) -> Result<()> {
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
        let every = supervise_interval(self.backend());
        let mut unanswered = 0u32;
        loop {
            tokio::select! {
                _ = term.recv() => break,
                _ = int.recv() => break,
                _ = tokio::time::sleep(every) => {
                    // Only a definite "not running" ends the supervision.
                    // A probe that could not be made says nothing (it has
                    // logged itself); ending on one killed the daemon
                    // with "the VM exited" over a live VM.
                    match self.running_state() {
                        Some(true) => unanswered = 0,
                        None => {
                            unanswered += 1;
                            if unanswered >= MAX_UNANSWERED_PROBES {
                                return Err(cannot_tell_error(
                                    self.backend(),
                                    unanswered,
                                    every,
                                ));
                            }
                            warn!(
                                "could not tell whether the VM is running ({unanswered} probe(s) in a row); still supervising it"
                            );
                        }
                        Some(false) => {
                            warn!("the VM exited");
                            if let Some(gv) = self.gvproxy_pid() {
                                kill(gv, libc::SIGTERM);
                            }
                            bail!(
                                "the VM exited; see {}",
                                self.console_path()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_else(|_| "`ssf vm console`".into())
                            );
                        }
                    }
                }
            }
        }
        info!("stopping the VM");
        self.stop().await
    }

    // ---- seed ----

    /// Firecracker: the seed tree as an ext4 disk (`seed.ext4`), made with
    /// `mkfs.ext4 -d` at every start.
    fn write_seed(&self, host: &Config) -> Result<()> {
        let tree = self.dir.join("seed");
        self.seed_tree(host, &tree)?;
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

    /// Build the seed tree at `tree` (replacing what was there): the guest
    /// `ssf` binary, the config rewritten for the guest, the token, the
    /// bot's ssh key, our public key and the `[vm] files`. Regenerated at
    /// every start so the guest follows the host; each backend then
    /// publishes it its own way.
    fn seed_tree(&self, host: &Config, tree: &Path) -> Result<()> {
        let _ = std::fs::remove_dir_all(tree);
        std::fs::create_dir_all(tree.join("config"))?;
        set_mode(tree, 0o700)?;
        let binary = self.guest_binary()?;
        std::fs::copy(&binary, tree.join("ssf"))
            .with_context(|| format!("copying {}", binary.display()))?;
        make_executable(&tree.join("ssf"))?;
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
        Ok(())
    }

    /// The Linux `ssf` binary the guest runs: `[vm] guest_binary`, this
    /// binary on a Linux host of the guest's architecture, else the
    /// release asset for this version, downloaded once with `gh`.
    fn guest_binary(&self) -> Result<PathBuf> {
        match guest_binary_source(
            std::env::consts::OS,
            std::env::consts::ARCH,
            self.cfg.guest_binary.as_deref(),
        ) {
            GuestBinary::Configured(p) => {
                let p = expand_tilde(&p);
                if !p.is_file() {
                    bail!("[vm] guest_binary {} does not exist", p.display());
                }
                Ok(p)
            }
            GuestBinary::Own => self.binary(),
            GuestBinary::Download { asset } => {
                let dir = self.dir.join("guest-bin");
                let path = dir.join(&asset);
                if path.is_file() {
                    return Ok(path);
                }
                std::fs::create_dir_all(&dir)?;
                info!(asset, "downloading the guest ssf binary with gh");
                let out = Command::new("gh")
                    .args([
                        "release",
                        "download",
                        &format!("v{}", env!("CARGO_PKG_VERSION")),
                    ])
                    .args(["-R", RELEASE_REPO, "--pattern", &asset, "-D"])
                    .arg(&dir)
                    .stdin(Stdio::null())
                    .output()
                    .context("running gh (is the GitHub CLI installed?)")?;
                if !out.status.success() || !path.is_file() {
                    bail!(
                        "no guest binary: `gh release download v{} -R {RELEASE_REPO} --pattern {asset}` failed ({}); build one for Linux and set [vm] guest_binary to it",
                        env!("CARGO_PKG_VERSION"),
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
                make_executable(&path)?;
                Ok(path)
            }
        }
    }

    // ---- ssh ----

    /// The ssh options that reach the guest.
    /// ssh's own options: the VM's key, port and hosts file, and the
    /// limits that keep one attempt from lasting for ever. `batch` (every
    /// probe and forwarded command; not an interactive session) adds the
    /// keepalives, so an ssh whose connection has gone silent gives up
    /// after a minute instead of holding the waits below open.
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
            v.push("-o".into());
            v.push("ServerAliveInterval=15".into());
            v.push("-o".into());
            v.push("ServerAliveCountMax=4".into());
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

    /// Wait for the guest to answer as `ssf`, `timeout` at the most.
    ///
    /// The loop below watches its own deadline; this wraps the whole wait
    /// in one too, because a loop can stop reaching its deadline check at
    /// all: an `ssf vm build` was once found parked at an `.await` for
    /// fifteen minutes with no child process, no output and no CPU. The
    /// inner check is what a person normally sees, the outer one is the
    /// backstop, and each attempt is bounded in turn by ssh's own
    /// `ConnectTimeout` and keepalives ([`Vm::ssh_args`]).
    async fn wait_for_ssh(&self, timeout: Duration) -> Result<()> {
        let waited = tokio::time::timeout(timeout + WAIT_BACKSTOP_MARGIN, self.ssh_loop(timeout));
        match waited.await {
            Ok(r) => r,
            Err(_) => bail!(
                "the guest did not answer on ssh in {}s, and the wait itself stopped making progress; `ssf vm console` has its console",
                timeout.as_secs()
            ),
        }
    }

    async fn ssh_loop(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            // Only a definite "not running" ends the wait: a probe that
            // could not be made (under lima, a `limactl` that did not
            // run) is not the guest exiting.
            if self.running_state() == Some(false) {
                bail!("the VM exited");
            }
            if self.ssh_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        bail!("the guest did not answer on ssh in {}s", timeout.as_secs())
    }

    /// `systemctl is-active ssf` in the guest once it stops `activating`,
    /// with the same backstop around the whole wait as [`Vm::wait_for_ssh`].
    async fn wait_for_daemon(&self, timeout: Duration) -> Option<String> {
        let waited =
            tokio::time::timeout(timeout + WAIT_BACKSTOP_MARGIN, self.daemon_loop(timeout));
        waited.await.unwrap_or_else(|_| {
            warn!(
                "waiting for the daemon in the guest stopped making progress after {}s; `ssf vm ssh -- systemctl status ssf` says where it is",
                (timeout + WAIT_BACKSTOP_MARGIN).as_secs()
            );
            None
        })
    }

    async fn daemon_loop(&self, timeout: Duration) -> Option<String> {
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

    /// `ssf <args>` as the guest runs it: the guest marker, so the
    /// command in there knows it is the factory and does not forward
    /// again, and the arguments as given.
    fn ssf_remote(&self, args: &[String]) -> Vec<String> {
        let mut remote = vec![format!("{GUEST_ENV}=1"), "ssf".to_string()];
        remote.extend(args.iter().cloned());
        remote
    }

    /// Run an `ssf` command inside the guest with this terminal, and exit
    /// with its status.
    pub fn exec_ssf(&self, args: &[String]) -> Result<ExitStatus> {
        let remote = self.ssf_remote(args);
        let tty = stdin_is_tty();
        let st = self
            .ssh(&remote, tty)
            .status()
            .context("running ssh (is the VM up? `ssf vm status`)")?;
        Ok(st)
    }

    /// [`Vm::exec_ssf`] with the guest's stdout captured, its stderr left
    /// on this terminal. For the caller that has to answer even when the
    /// guest does not: `status --json`, which the bar widget parses.
    pub fn capture_ssf(&self, args: &[String]) -> Result<std::process::Output> {
        self.ssh(&self.ssf_remote(args), false)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .output()
            .context("running ssh (is the VM up? `ssf vm status`)")
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
        let backend = self.backend();
        // One `limactl list` for everything lima knows -- and, when it
        // did not answer, that fact rather than `.ok().flatten()`. Read
        // as "no such instance" it printed `instance: ssf-<name> missing
        // (ssf vm build)` over a VM that exists, which is the same
        // conflation `lima_stop` was fixed for.
        let (inst, probe_error) = match backend {
            BackendKind::Lima => match self.lima_instance() {
                Ok(i) => (i, None),
                Err(e) => (None, Some(format!("{e:#}"))),
            },
            BackendKind::Firecracker => (None, None),
        };
        let running = match backend {
            BackendKind::Firecracker => Some(self.running()),
            BackendKind::Lima => probe_error
                .is_none()
                .then(|| inst.as_ref().is_some_and(|i| i.is_running())),
        };
        let ssh = running == Some(true) && self.ssh_ok();
        let sizes = self.sizes();
        VmStatus {
            vcpus: sizes.vcpus,
            mem_mib: sizes.mem_mib,
            data_gib: self.data_cap_gib(),
            data: if ssh {
                self.guest_disk_use().ok()
            } else {
                None
            },
            enabled: self.cfg.enabled,
            name: self.cfg.name.clone(),
            dir: self.dir.to_string_lossy().to_string(),
            backend: backend.to_string(),
            instance: match backend {
                BackendKind::Lima => Some(self.lima_name()),
                BackendKind::Firecracker => None,
            },
            lima_dir: inst.as_ref().map(|i| i.dir.clone()),
            image: match backend {
                BackendKind::Firecracker => self.rootfs().exists(),
                BackendKind::Lima => inst.is_some(),
            },
            running,
            firecracker_pid: match backend {
                BackendKind::Firecracker => self.firecracker_pid(),
                BackendKind::Lima => None,
            },
            gvproxy_pid: match backend {
                BackendKind::Firecracker => self.gvproxy_pid(),
                BackendKind::Lima => None,
            },
            ssh_port: self.cfg.ssh_port,
            ssh,
            daemon: if ssh { self.daemon_state() } else { None },
            logins: if ssh {
                self.logins().unwrap_or_default()
            } else {
                Vec::new()
            },
            tooling: (!in_guest()).then(|| self.tooling()),
            probe_error,
        }
    }

    /// Look for the tooling this VM's backend needs on the machine this
    /// runs on: what `ssf doctor` and `ssf vm status` report.
    pub fn tooling(&self) -> Tooling {
        let backend = self.backend();
        let tools = backend_tools(
            backend,
            std::env::consts::OS,
            std::env::consts::ARCH,
            self.cfg.limactl.as_deref(),
            self.cfg.vm_type.as_deref(),
        );
        let (ok, detail) = backend_tooling_line(&tools, &probe_tools(&tools));
        Tooling { ok, detail }
    }

    // ---- harness logins ----

    /// Every harness's login state in the guest, in one ssh round trip.
    ///
    /// What the script found is in its output, not in its status: every
    /// test in it fails for a harness that is not installed or not logged
    /// in, which is the ordinary case. [`Vm::ssh_output`] throws stdout
    /// away when the status is not zero, so a script ending in a failed
    /// test would come back as an error with the answer discarded. The
    /// closing `exit 0` is what keeps that from happening -- nothing may
    /// be appended after it, and a test appended before it must not be
    /// the last command of the script.
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
            .chain(std::iter::once("exit 0".to_string()))
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

    /// A fresh root at the next start (Firecracker: the root disk remade
    /// from the image; lima: the instance re-created from its template);
    /// the data disk (state, clones, worktrees) stays.
    pub async fn reset(&self) -> Result<()> {
        if self.running() {
            self.stop().await?;
        }
        let _ = std::fs::remove_file(self.known_hosts());
        match self.backend() {
            BackendKind::Firecracker => {
                let _ = std::fs::remove_file(self.root_disk());
                println!("root disk removed; `ssf vm start` makes a fresh one from the image");
                Ok(())
            }
            BackendKind::Lima => self.lima_reset(),
        }
    }

    /// Remove the VM and everything in it. Every part of it may be gone
    /// already -- a second `ssf uninstall`, a half-uninstalled machine
    /// -- so each is removed only if it is there, and the command says
    /// so rather than claiming a removal it did not make.
    pub async fn destroy(&self) -> Result<()> {
        // Firecracker must have its process gone before its disks:
        // removing them under a live firecracker leaves it running on
        // unlinked inodes, so a stop that fails ends the destroy. Lima's
        // `limactl delete -f` force-stops the instance itself, so
        // neither a stop that fails nor a running state that could not
        // be read is a reason to leave a VM standing for ever -- which
        // is what a guest that would not shut down did, on every run.
        match (self.backend(), self.running_state()) {
            (_, Some(false)) => {}
            (BackendKind::Firecracker, _) => self.stop().await?,
            (BackendKind::Lima, Some(true)) => {
                if let Err(e) = self.stop().await {
                    warn!(
                        "could not stop {} before destroying it: {e:#}; deleting it anyway",
                        self.cfg.name
                    );
                }
            }
            // A lima that could not be asked is not asked twice more for
            // nothing: `lima_stop` re-runs the same listing and
            // `lima_destroy` runs it again after that, so a limactl that
            // hangs rather than fails bought this step minutes of
            // silence and a warning that explains none of it. The
            // graceful shutdown is worth its wait where the guest is
            // known to be up; where it is not, `limactl delete -f` stops
            // whatever is there.
            (BackendKind::Lima, None) => {}
        }
        let lima = match self.backend() {
            BackendKind::Lima => self.lima_destroy(),
            BackendKind::Firecracker => Ok(false),
        };
        let mut removed = lima.as_ref().copied().unwrap_or(false);
        // Under lima this directory is often the only thing that was
        // ever here, and under either backend it may be gone already. It
        // is ssf's own either way, so it goes even when lima's half
        // failed: leaving it behind only makes the next run harder to
        // read, and the failure is still the step's failure below.
        let dir = if self.dir.exists() {
            match std::fs::remove_dir_all(&self.dir) {
                Ok(()) => {
                    println!("removed {}", self.dir.display());
                    removed = true;
                    Ok(())
                }
                Err(e) => {
                    Err(anyhow::Error::new(e).context(format!("removing {}", self.dir.display())))
                }
            }
        } else {
            Ok(())
        };
        // Both halves are reported. A directory that would not go is no
        // reason to lose the news that lima still holds an instance and
        // a data disk.
        match (lima, dir) {
            (Ok(_), Ok(())) => {}
            (Err(l), Err(d)) => bail!("{l:#}; and {d:#}"),
            (Err(e), Ok(())) | (Ok(_), Err(e)) => return Err(e),
        }
        if !removed {
            // Not "the directory is not there": under lima that
            // directory is precisely what the question did not turn on.
            println!("nothing to remove");
        }
        Ok(())
    }
}

/// Where `gh release download` finds the guest binary.
pub const RELEASE_REPO: &str = "mikekelly/simple-software-factory";

/// Where the guest's `ssf` binary comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestBinary {
    /// `[vm] guest_binary`.
    Configured(String),
    /// This binary: a Linux host of the guest's architecture.
    Own,
    /// The release asset for this version and the guest's architecture.
    Download { asset: String },
}

/// Which binary a host running `os` on `arch` (the guest's architecture
/// too) seeds into the guest: what `[vm] guest_binary` names, else its own
/// when it is Linux, else the release asset `ssf-<version>-linux-<arch>`.
pub fn guest_binary_source(os: &str, arch: &str, configured: Option<&str>) -> GuestBinary {
    match configured {
        Some(p) => GuestBinary::Configured(p.to_string()),
        None if os == "linux" => GuestBinary::Own,
        None => GuestBinary::Download {
            asset: format!("ssf-{}-linux-{arch}", env!("CARGO_PKG_VERSION")),
        },
    }
}

/// Settle `[vm] backend` for a build the way `choose_sizes` settles the
/// sizes: a value in the file stays; none gets `default` and is written.
/// Returns the backend, where it came from, and whether the file changed.
pub fn choose_backend(
    cfg: &mut VmConfig,
    default: BackendKind,
) -> (BackendKind, &'static str, bool) {
    match cfg.backend {
        Some(b) => (b, "set in config.toml", false),
        None => {
            cfg.backend = Some(default);
            (default, "for this machine", true)
        }
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

/// Where the guest scripts are: `SSF_VM_DIR`, the package's
/// `/usr/share/ssf/vm`, `share/ssf/vm` under the prefix this binary is
/// installed in (Homebrew), or `vm/` next to a source build.
pub fn scripts_dir() -> Result<PathBuf> {
    if let Ok(d) = std::env::var("SSF_VM_DIR") {
        return Ok(PathBuf::from(d));
    }
    let mut candidates = vec![PathBuf::from("/usr/share/ssf/vm")];
    if let Ok(exe) = std::env::current_exe()
        && let Some(d) = exe.parent()
    {
        candidates.push(d.join("../share/ssf/vm"));
        candidates.push(d.join("../../vm"));
    }
    candidates.push(PathBuf::from("vm"));
    candidates
        .into_iter()
        .find(|p| p.join("guest/provision.sh").exists())
        .map(|p| p.canonicalize().unwrap_or(p))
        .context("the VM scripts (vm/guest/provision.sh) are not installed; set SSF_VM_DIR")
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
    // Anything with a separator in it is a path, not a name to look up:
    // `PATH` is searched for `limactl`, never for `~/bin/limactl` (which
    // reaches here expanded) or `./limactl`.
    if name.contains('/') {
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

/// Whether a browser can open on this host (a macOS desktop always has one).
fn host_has_display() -> bool {
    if platform::is_macos() {
        return true;
    }
    (std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some())
        && which("xdg-open").is_some()
}

/// Open a URL in the host browser, detached; false when that failed.
fn open_in_browser(url: &str) -> bool {
    let opener = if platform::is_macos() {
        "open"
    } else {
        "xdg-open"
    };
    Command::new(opener)
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
        cfg.vm.vcpus = Some(2);
        cfg.vm.mem_mib = Some(4096);
        cfg.vm.data_gib = Some(20);
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
    fn a_directory_nobody_could_look_in_is_not_a_directory_with_nothing_in_it() {
        // A denied `stat` answered a confident "no data disk" about a
        // directory nobody looked into, and that is the one value that
        // lets `ssf uninstall` past its refusal.
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!(
            "ssf-unread-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut cfg = Config::default();
        cfg.vm.name = "one".into();
        cfg.vm.backend = Some(BackendKind::Firecracker);
        cfg.vm.dir = base.to_string_lossy().into_owned();
        let vm = Vm::new(&cfg);
        std::fs::create_dir_all(&vm.dir).unwrap();
        let disk = vm.dir.join("data.ext4");
        std::fs::write(&disk, b"clones").unwrap();
        // The two answers that are answers, so what follows is about
        // the third and not about the fixture.
        assert_eq!(there(&disk), Some(true));
        assert_eq!(there(&vm.dir.join("nothing-here")), Some(false));
        assert_eq!(vm.survey().data, Some(true));

        let mut perm = std::fs::metadata(&vm.dir).unwrap().permissions();
        perm.set_mode(0o000);
        std::fs::set_permissions(&vm.dir, perm).unwrap();
        let seen = std::fs::metadata(&disk).map_err(|e| e.kind());
        // Taken while the mode is still 0o000, because that is the only
        // moment it says anything: after the restore below it is true
        // for everyone, and a guard that is true for everyone is not a
        // guard.
        let readable_anyway = std::fs::read_dir(&vm.dir).is_ok();
        // Both readings taken while the denial is in force: the cleanup
        // below removes the directory, and a `there` called after it
        // would answer `Some(false)` about a path that really is gone.
        let denied = there(&disk);
        let survey = vm.survey();
        // Restored before the assertions, so a failure still leaves the
        // temporary directory removable.
        let mut perm = std::fs::metadata(&vm.dir).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&vm.dir, perm).unwrap();
        std::fs::remove_dir_all(&base).unwrap();

        match seen {
            Err(std::io::ErrorKind::NotFound) => {
                panic!("the fixture is wrong: the disk was written above")
            }
            Err(_) => {
                assert_eq!(denied, None, "a denied stat is not an answer");
                assert_eq!(
                    survey.data, None,
                    "and the refusal has to see it as one that was never answered"
                );
            }
            // euid 0 ignores the mode, so this case cannot be built here and
            // this run proves nothing about it. The assertion is the
            // narrow one that is true: the directory really was
            // readable, so the stat above was allowed to succeed.
            Ok(_) => assert!(
                readable_anyway,
                "a stat succeeded through a directory nothing should have been able to read"
            ),
        }
    }

    #[test]
    fn under_firecracker_the_directory_is_the_vm() {
        // The disks are the VM, so the directory (or a guest running off
        // them) is the whole answer -- there is no bookkeeping of
        // lima's kind to ask on top of it, and no question that could
        // fail to be asked.
        let mut cfg = Config::default();
        cfg.vm.name = "one".into();
        cfg.vm.backend = Some(BackendKind::Firecracker);
        let dir = std::env::temp_dir().join(format!(
            "ssf-fc-present-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        cfg.vm.dir = dir.to_string_lossy().into_owned();
        let vm = Vm::new(&cfg);
        assert!(!vm.dir.exists());
        let empty = vm.survey();
        std::fs::create_dir_all(&vm.dir).unwrap();
        let built = vm.survey();
        // Removed before the assertions, so a failure leaves nothing in
        // the temporary directory behind it.
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            empty,
            Survey {
                present: Some(false),
                running: Some(false),
                startable: false,
                data: Some(false),
            }
        );
        // The directory is the VM, but only the data disk in it holds
        // anyone's work, and a build that got no further than the
        // directory has none.
        assert_eq!(
            built,
            Survey {
                present: Some(true),
                running: Some(false),
                startable: true,
                data: Some(false),
            }
        );
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
        // Every attempt is bounded: a connection that never comes up
        // gives up after ConnectTimeout, and one that goes silent after
        // the keepalives, so the waits that call ssh in a loop cannot be
        // held open by a single attempt. An interactive session keeps the
        // connect timeout but not the keepalives.
        assert!(joined.contains("ConnectTimeout=5"));
        assert!(joined.contains("ServerAliveInterval=15"));
        assert!(joined.contains("ServerAliveCountMax=4"));
        assert!(!vm.ssh_args(false).join(" ").contains("ServerAlive"));
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

    fn facts(cpus: u32, mem_mib: u64, free_gib: u64) -> HostFacts {
        HostFacts {
            cpus,
            mem_mib,
            free_bytes: free_gib << 30,
            mount: "/home".into(),
        }
    }

    #[test]
    fn sizing_rule_follows_the_machine_down_to_the_floors() {
        // A desktop: 8 CPUs, 32 GiB, 500 GiB free.
        assert_eq!(
            sizes_for(&facts(8, 32768, 500)),
            Sizes {
                vcpus: 7,
                mem_mib: 16384,
                data_gib: 250
            }
        );
        // Half the RAM lands on a 256 MiB boundary.
        assert_eq!(sizes_for(&facts(4, 31922, 160)).mem_mib, 15872);
        assert_eq!(sizes_for(&facts(4, 31922, 160)).data_gib, 80);
        // A small machine never goes under the old fixed sizes.
        assert_eq!(sizes_for(&facts(2, 4096, 30)), Sizes::MIN);
        assert_eq!(sizes_for(&facts(1, 1024, 0)), Sizes::MIN);
        assert_eq!(sizes_for(&facts(3, 8192, 41)).vcpus, 2);
        assert_eq!(sizes_for(&facts(3, 8192, 41)).data_gib, 20);
        assert_eq!(sizes_for(&facts(3, 8192, 42)).data_gib, 21);
    }

    #[test]
    fn build_writes_unset_sizes_and_keeps_hand_set_ones() {
        let rule = Sizes {
            vcpus: 7,
            mem_mib: 16384,
            data_gib: 250,
        };
        // Nothing set: everything from the machine, and the file changes.
        let mut cfg = VmConfig::default();
        let c = choose_sizes(&mut cfg, [None, None, None], rule);
        assert_eq!(c.sizes, rule);
        assert!(c.changed);
        assert_eq!(c.sources, ["from this machine"; 3]);
        assert_eq!(cfg.data_gib, Some(250));
        // Set by hand: kept, nothing to write.
        let mut cfg = VmConfig {
            vcpus: Some(2),
            mem_mib: Some(4096),
            data_gib: Some(20),
            ..VmConfig::default()
        };
        let c = choose_sizes(&mut cfg, [None, None, None], rule);
        assert_eq!(c.sizes, Sizes::MIN);
        assert!(!c.changed);
        assert_eq!(c.sources, ["set in config.toml"; 3]);
        // A flag beats the file and the machine, and is written.
        let c = choose_sizes(&mut cfg, [None, Some(8192), None], rule);
        assert_eq!(c.sizes.mem_mib, 8192);
        assert_eq!(c.sources[1], "from the flag");
        assert!(c.changed);
        assert_eq!(cfg.mem_mib, Some(8192));
        // The same flag again changes nothing.
        assert!(!choose_sizes(&mut cfg, [None, Some(8192), None], rule).changed);
        // The effective sizes follow the file where set.
        let mut whole = Config {
            vm: cfg.clone(),
            ..Config::default()
        };
        assert_eq!(Vm::new(&whole).sizes().mem_mib, 8192);
        whole.vm.vcpus = None;
        assert!(Vm::new(&whole).sizes().vcpus >= Sizes::MIN.vcpus);
    }

    #[test]
    fn grow_plans_refuse_to_shrink_and_skip_the_same_size() {
        assert_eq!(plan_grow(20, Some(40), 80).unwrap(), Some(40));
        assert_eq!(plan_grow(20, None, 80).unwrap(), Some(80));
        assert_eq!(plan_grow(20, Some(20), 80).unwrap(), None);
        // The rule is below today: nothing to do, not an error.
        assert_eq!(plan_grow(100, None, 80).unwrap(), None);
        let e = plan_grow(20, Some(10), 80).unwrap_err().to_string();
        assert!(e.contains("shrink"), "{e}");
        assert!(plan_grow(20, Some(0), 80).is_err());
    }

    #[test]
    fn sizing_measures_the_filesystem_the_disks_are_on() {
        let base = Path::new("/home/me/.local/share/ssf/vm");
        let disks = PathBuf::from("/other/.lima/_disks");
        // Firecracker's disks are files under `[vm] dir`.
        assert_eq!(
            sizing_dir_for(BackendKind::Firecracker, base, Some(disks.clone())),
            (base.to_path_buf(), "[vm] dir")
        );
        // lima's are in lima's home, which can be another volume: the
        // "half the free space" rule and grow's warning have to be about
        // that one.
        assert_eq!(
            sizing_dir_for(BackendKind::Lima, base, Some(disks.clone())),
            (disks, "lima's disk directory")
        );
        // No home, no lima directory: the VM's own is the honest answer.
        assert_eq!(
            sizing_dir_for(BackendKind::Lima, base, None),
            (base.to_path_buf(), "[vm] dir")
        );
    }

    #[test]
    fn the_supervisor_polls_a_lima_vm_less_often_than_a_firecracker_one() {
        // Firecracker's "is it up?" is a PID file and a /proc lookup;
        // lima's forks a ~60 MB Go binary and takes a lock in lima's
        // home. Doing that every five seconds for as long as the factory
        // runs is more than the question is worth on a laptop, and every
        // fork is another chance to fail and be misread as "the VM
        // exited".
        assert_eq!(
            supervise_interval(BackendKind::Firecracker),
            Duration::from_secs(5)
        );
        assert!(supervise_interval(BackendKind::Lima) >= Duration::from_secs(30));
        assert!(
            supervise_interval(BackendKind::Lima) > supervise_interval(BackendKind::Firecracker)
        );
        // Still well inside the wait a person would sit through before
        // asking what happened.
        assert!(supervise_interval(BackendKind::Lima) <= Duration::from_secs(60));
    }

    #[test]
    fn a_probe_that_can_never_be_made_ends_the_supervision() {
        // A probe that could not be made says nothing, so one of them
        // must not end the supervision -- but the loop that only ever
        // warned left `ssf run` "supervising" a VM it had not heard about
        // for hours, with the service reading active the whole time.
        const { assert!(MAX_UNANSWERED_PROBES > 1) };
        for backend in [BackendKind::Lima, BackendKind::Firecracker] {
            let every = supervise_interval(backend);
            // Long enough to sit out a busy laptop, short enough that the
            // daemon does not pretend all day.
            let gave_up_after = every * MAX_UNANSWERED_PROBES;
            assert!(gave_up_after >= Duration::from_secs(30), "{backend:?}");
            assert!(gave_up_after <= Duration::from_secs(15 * 60), "{backend:?}");
            let err = cannot_tell_error(backend, MAX_UNANSWERED_PROBES, every).to_string();
            assert!(
                err.contains("cannot tell whether the VM is running"),
                "{err}"
            );
            // It names the tool that stopped answering, so the next thing
            // to look at is not a guess.
            let tool = match backend {
                BackendKind::Lima => "limactl",
                BackendKind::Firecracker => "pid file",
            };
            assert!(err.contains(tool), "{err}");
        }
    }

    #[test]
    fn a_firecracker_vm_answers_the_running_question_either_way() {
        // Under Firecracker the probe is a file read, so it always has an
        // answer -- `None` (the probe could not be made) is lima's case,
        // and it is what keeps a transient `limactl` failure from ending
        // the supervisor and the ssh wait with "the VM exited".
        let mut cfg = Config::default();
        cfg.vm.dir = std::env::temp_dir().to_string_lossy().into_owned();
        cfg.vm.name = "no-such-vm".into();
        cfg.vm.backend = Some(BackendKind::Firecracker);
        let vm = Vm::new(&cfg);
        assert_eq!(vm.running_state(), Some(false));
        assert!(!vm.running());
    }

    #[test]
    fn the_doctor_line_names_the_backend_tooling_and_what_to_install() {
        // lima needs limactl, and qemu too on Linux (its only driver
        // there); a Mac runs the Virtualization framework instead.
        let mac = backend_tools(BackendKind::Lima, "macos", "aarch64", None, None);
        assert_eq!(mac.len(), 1);
        assert_eq!(mac[0].name, "limactl");
        // The floor is in what it says to install. That string is only
        // rendered for a tool that is missing, so it is what someone
        // with no lima at all is told to get -- the version of a lima
        // that *is* installed is `ssf vm build`'s to check, not this
        // line's.
        assert!(
            mac[0].install.contains(&lima::MIN_LIMA.to_string()),
            "{:?}",
            mac[0]
        );
        // ... unless the config asks for qemu, which is the driver that
        // has to be installed. Keyed on the OS alone, a Mac with
        // `[vm] vm_type = "qemu"` passed every check ssf makes and then
        // failed inside `limactl create`.
        let mac_qemu = backend_tools(BackendKind::Lima, "macos", "aarch64", None, Some("qemu"));
        assert_eq!(mac_qemu.len(), 2);
        assert_eq!(mac_qemu[1].name, "qemu-system-aarch64");
        assert!(
            mac_qemu[1].install.contains("brew install qemu"),
            "{:?}",
            mac_qemu[1]
        );
        assert!(mac_qemu[1].install.contains("vm_type"), "{:?}", mac_qemu[1]);
        // `vz` is the Virtualization framework: no qemu.
        assert_eq!(
            backend_tools(BackendKind::Lima, "macos", "aarch64", None, Some("vz")).len(),
            1
        );
        assert!(lima_uses_qemu("macos", Some("qemu")));
        assert!(!lima_uses_qemu("macos", None));
        assert!(!lima_uses_qemu("macos", Some("vz")));
        // qemu is lima's only Linux driver, whatever the config says.
        assert!(lima_uses_qemu("linux", None));
        assert!(lima_uses_qemu("linux", Some("qemu")));
        let linux = backend_tools(BackendKind::Lima, "linux", "x86_64", None, None);
        assert_eq!(linux.len(), 2);
        assert_eq!(linux[1].name, "qemu-system-x86_64");
        assert!(
            linux[1].install.contains("qemu-system-x86"),
            "{:?}",
            linux[1]
        );
        // `[vm] limactl` is what doctor looks for when it is set.
        let set = backend_tools(
            BackendKind::Lima,
            "macos",
            "aarch64",
            Some("/opt/l/limactl"),
            None,
        );
        assert_eq!(set[0].name, "/opt/l/limactl");
        // And a `~` in it is expanded, as `Vm::limactl` expands it when
        // it runs the thing: doctor and `ssf vm status` used to look
        // "~/bin/limactl" up on PATH -- which `which` only searches for a
        // bare name -- and report a limactl that works as not installed.
        if let Some(home) = dirs::home_dir() {
            let tilde = backend_tools(
                BackendKind::Lima,
                "macos",
                "aarch64",
                Some("~/bin/limactl"),
                None,
            );
            assert_eq!(
                tilde[0].name,
                home.join("bin/limactl").to_string_lossy().to_string()
            );
        }
        // A path is a path, whatever it starts with; a bare name is
        // looked up on PATH.
        assert_eq!(which("/nonexistent/limactl"), None);
        assert_eq!(which("./nonexistent-limactl"), None);
        assert!(which("sh").is_some());
        assert_eq!(which("definitely-not-a-program-on-this-path"), None);
        // Firecracker asks one question: may this user use KVM?
        let fc = backend_tools(BackendKind::Firecracker, "linux", "x86_64", None, None);
        assert_eq!(fc.len(), 1);
        assert!(fc[0].device);
        assert_eq!(fc[0].name, "/dev/kvm");
        assert!(fc[0].install.contains("usermod -aG kvm"), "{:?}", fc[0]);

        let found = vec![
            Some("/usr/bin/limactl".to_string()),
            Some("/usr/bin/qemu-system-x86_64".to_string()),
        ];
        let (ok, msg) = backend_tooling_line(&linux, &found);
        assert!(ok);
        assert_eq!(
            msg,
            "limactl at /usr/bin/limactl, qemu-system-x86_64 at /usr/bin/qemu-system-x86_64"
        );
        // Only what is missing is reported, with what to install.
        let (ok, msg) = backend_tooling_line(&linux, &[Some("/usr/bin/limactl".to_string()), None]);
        assert!(!ok);
        assert!(msg.starts_with("qemu-system-x86_64 not installed; install qemu"));
        assert!(!msg.contains("limactl at"), "{msg}");
        let (ok, msg) = backend_tooling_line(&fc, &[None]);
        assert!(!ok);
        assert!(msg.starts_with("/dev/kvm not usable by you; "));
        let (ok, msg) = backend_tooling_line(&fc, &[Some("/dev/kvm".into())]);
        assert!(ok);
        assert_eq!(msg, "/dev/kvm usable");
    }

    #[test]
    fn df_and_meminfo_parse() {
        let d = parse_df(
            "    Used    Avail    1B-blocks
17000000000 3000000000 21000000000
",
        )
        .unwrap();
        assert_eq!(d.used_bytes, 17_000_000_000);
        assert_eq!(d.pct(), 85);
        assert!(d.is_full());
        assert!(d.describe().starts_with("15.8 of 20 GiB used (85%)"));
        let d = parse_df("1 9 10").unwrap();
        assert_eq!(d.pct(), 10);
        assert!(!d.is_full());
        assert!(parse_df("garbage").is_none());
        assert!(parse_df("").is_none());
        let m = parse_meminfo(
            "MemTotal:       16384000 kB\nMemFree:  100 kB\nMemAvailable:    8192000 kB\nSwapTotal:  0 kB\nSwapFree:   0 kB\n",
        )
        .unwrap();
        assert_eq!(m.total_kib, 16_384_000);
        assert!(!m.is_short());
        assert_eq!(m.describe(), "8000 of 16000 MiB available");
        let m = parse_meminfo("MemTotal: 1000 kB\nMemAvailable: 99 kB\n").unwrap();
        assert!(m.is_short());
        let m = parse_meminfo(
            "MemTotal: 1000 kB\nMemAvailable: 900 kB\nSwapTotal: 2048 kB\nSwapFree: 1024 kB\n",
        )
        .unwrap();
        assert!(m.is_short());
        assert!(m.describe().ends_with(", 1 MiB swapped out"));
        assert!(parse_meminfo("MemFree: 1 kB\n").is_none());
        // This machine reads.
        let f = HostFacts::probe(Path::new("/nonexistent/deeper/still")).unwrap();
        assert!(f.cpus >= 1 && f.mem_mib > 0);
        assert_eq!(f.mount, "/");
        let here = disk_use(Path::new("/")).unwrap();
        assert!(here.size_bytes > 0);
    }

    /// A tiny ext4 image grows, a VM's disk grows with the config's
    /// arithmetic, and a running VM is refused. Needs e2fsprogs.
    #[test]
    fn grow_resizes_the_image_and_refuses_a_running_vm() {
        if which("mkfs.ext4").is_none() || which("resize2fs").is_none() {
            eprintln!("skipped: e2fsprogs not installed");
            return;
        }
        let dir = std::env::temp_dir().join(format!(
            "ssf-grow-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("one")).unwrap();
        let block_count = |img: &Path| -> u64 {
            let out = Command::new("dumpe2fs")
                .arg("-h")
                .arg(img)
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            let field = |k: &str| -> u64 {
                text.lines()
                    .find_map(|l| l.strip_prefix(k))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap()
            };
            field("Block count:") * field("Block size:")
        };
        // MiB scale, straight through the image helper.
        let img = dir.join("small.ext4");
        let f = std::fs::File::create(&img).unwrap();
        f.set_len(16 << 20).unwrap();
        drop(f);
        run_ok(
            Command::new("mkfs.ext4").args(["-q", "-F"]).arg(&img),
            "mkfs",
        )
        .unwrap();
        assert_eq!(block_count(&img), 16 << 20);
        grow_image(&img, 48 << 20).unwrap();
        assert_eq!(block_count(&img), 48 << 20);
        assert_eq!(std::fs::metadata(&img).unwrap().len(), 48 << 20);
        // Through the VM: a 1 GiB sparse data disk to 2 GiB.
        let mut cfg = Config::default();
        cfg.vm.dir = dir.to_string_lossy().to_string();
        cfg.vm.name = "one".into();
        cfg.vm.data_gib = Some(1);
        let vm = Vm::new(&cfg);
        let e = vm.grow(Some(2)).unwrap_err().to_string();
        assert!(e.contains("does not exist yet"), "{e}");
        let f = std::fs::File::create(vm.data_disk()).unwrap();
        f.set_len(1 << 30).unwrap();
        drop(f);
        run_ok(
            Command::new("mkfs.ext4")
                .args(["-q", "-F", "-L", "ssf-data"])
                .arg(vm.data_disk()),
            "mkfs",
        )
        .unwrap();
        assert_eq!(vm.data_cap_gib(), 1);
        assert!(vm.grow(Some(0)).is_err(), "shrinking is refused");
        assert_eq!(vm.grow(Some(1)).unwrap(), None, "same size: nothing to do");
        assert_eq!(vm.grow(Some(2)).unwrap(), Some(2));
        assert_eq!(vm.data_cap_gib(), 2);
        assert_eq!(block_count(&vm.data_disk()), 2 << 30);
        // Sparse: the file takes a little space, not 2 GiB.
        let blocks =
            std::os::unix::fs::MetadataExt::blocks(&std::fs::metadata(vm.data_disk()).unwrap());
        assert!(blocks * 512 < 1 << 30, "{blocks} blocks");
        // Running (a process whose name says firecracker): refused.
        let fake = dir.join("firecracker");
        std::fs::copy("/bin/sleep", &fake).unwrap();
        let mut child = Command::new(&fake).arg("60").spawn().unwrap();
        std::fs::write(vm.fc_pid(), child.id().to_string()).unwrap();
        // spawn may return before the child has exec'd its new name.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !vm.running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            vm.running(),
            "{:?}",
            std::fs::read(format!("/proc/{}/cmdline", child.id()))
        );
        let e = vm.grow(Some(4)).unwrap_err().to_string();
        assert!(e.contains("is running"), "{e}");
        assert_eq!(vm.data_cap_gib(), 2, "untouched");
        child.kill().unwrap();
        child.wait().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backend_is_the_platform_default_unless_configured() {
        let vm = vm();
        assert_eq!(vm.backend(), BackendKind::platform_default());
        if std::env::consts::OS == "linux" {
            assert_eq!(vm.backend(), BackendKind::Firecracker);
        }
        let mut cfg = Config::default();
        cfg.vm.backend = Some(BackendKind::Lima);
        assert_eq!(Vm::new(&cfg).backend(), BackendKind::Lima);
        cfg.vm.backend = Some(BackendKind::Firecracker);
        assert_eq!(Vm::new(&cfg).backend(), BackendKind::Firecracker);
        // A build writes the platform default once; a hand-set value wins.
        let mut c = VmConfig::default();
        assert_eq!(
            choose_backend(&mut c, BackendKind::Lima),
            (BackendKind::Lima, "for this machine", true)
        );
        assert_eq!(c.backend, Some(BackendKind::Lima));
        assert_eq!(
            choose_backend(&mut c, BackendKind::Firecracker),
            (BackendKind::Lima, "set in config.toml", false)
        );
    }

    #[test]
    fn guest_binary_comes_from_the_config_the_host_or_the_release() {
        assert_eq!(
            guest_binary_source("macos", "aarch64", Some("~/dl/ssf")),
            GuestBinary::Configured("~/dl/ssf".into())
        );
        assert_eq!(
            guest_binary_source("linux", "x86_64", Some("/opt/ssf")),
            GuestBinary::Configured("/opt/ssf".into())
        );
        assert_eq!(
            guest_binary_source("linux", "x86_64", None),
            GuestBinary::Own
        );
        assert_eq!(
            guest_binary_source("linux", "aarch64", None),
            GuestBinary::Own
        );
        assert_eq!(
            guest_binary_source("macos", "aarch64", None),
            GuestBinary::Download {
                asset: format!("ssf-{}-linux-aarch64", env!("CARGO_PKG_VERSION"))
            }
        );
        assert_eq!(
            guest_binary_source("macos", "x86_64", None),
            GuestBinary::Download {
                asset: format!("ssf-{}-linux-x86_64", env!("CARGO_PKG_VERSION"))
            }
        );
        // A configured path that is missing is an error naming the key.
        let mut cfg = Config::default();
        cfg.vm.guest_binary = Some("/nonexistent/ssf-linux".into());
        let e = Vm::new(&cfg).guest_binary().unwrap_err().to_string();
        assert!(e.contains("guest_binary"), "{e}");
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
        cfg.vm.data_gib = Some(2);
        cfg.github.login = Some("test-bot".into());
        // Seeding the guest resolves the bot token out of the config
        // directory, which the test build otherwise refuses (#140). This
        // test is run by hand against the factory installed here, so it
        // says so: whatever this machine's factory signs in as is what
        // the guest gets, a token file or the gh keyring behind it.
        let _machine = crate::config::test_support::the_machine_itself();
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
