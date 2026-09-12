use super::*;

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

pub(in crate::vm) fn firecracker_url() -> String {
    format!(
        "https://github.com/firecracker-microvm/firecracker/releases/download/{v}/firecracker-{v}-x86_64.tgz",
        v = FIRECRACKER_VERSION
    )
}

pub(in crate::vm) fn gvproxy_url(name: &str) -> String {
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
pub(in crate::vm) fn there(p: &Path) -> Option<bool> {
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
/// only warned left `ssf-server` "supervising" a VM it had not heard about
/// for hours while the service read active. Ten rounds is under a minute
/// under Firecracker and five minutes under lima: long enough to sit out
/// a busy laptop or a lima home someone else has locked, short enough
/// that the daemon does not pretend all day.
pub(in crate::vm) const MAX_UNANSWERED_PROBES: u32 = 10;

/// What the supervisor says when the probe has stopped answering: the
/// question, the tool that could not answer it, and how long it has been
/// like that. The warnings from the probe itself are above it in the log.
pub(in crate::vm) fn cannot_tell_error(
    backend: BackendKind,
    rounds: u32,
    every: Duration,
) -> anyhow::Error {
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
pub(in crate::vm) fn qemu_install_hint(os: &str, arch: &str) -> String {
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
