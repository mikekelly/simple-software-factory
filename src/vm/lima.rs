//! The lima backend of `ssf vm` (`[vm] backend = "lima"`; the default on
//! macOS): the guest is a [lima](https://lima-vm.io) instance, `ssf-<name>`
//! in lima's own home (`~/.lima` or `$LIMA_HOME`), with a lima data disk
//! `ssf-<name>`, and the same guest scripts and units as the
//! Firecracker image. The flow:
//!
//! * `ssf vm build`: preflight (`limactl` at [`MIN_LIMA`] or newer, and
//!   qemu on Linux), the template `<vm.dir>/<name>/lima.yaml` from `[vm]`
//!   (a cloud image lima maintains per architecture, or `vm.image`; the
//!   sizes; the share mount; the data disk; the ssh port), `share/`
//!   written, the data disk created, `limactl create` and a first
//!   `limactl start`. That first boot provisions the guest once: the
//!   template's provision script runs `/mnt/ssf/guest/lima-boot.sh` as
//!   root at every boot, which runs `provision.sh` when
//!   `/etc/ssf-image-built` is missing (packages, the `ssf` user, herdr,
//!   the harness CLIs, the units) and writes the marker on success. The
//!   host waits for the marker over `limactl shell`, then for ssh as
//!   `ssf`, and stops the instance.
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
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use super::{
    HostFacts, Sizes, Survey, Vm, make_executable, plan_grow, scripts_dir, sizes_for, which,
};
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
/// How long the first boot may take to provision the guest. The guest
/// provisions itself inside `limactl start`, so this is also what that
/// call is given as its own `--timeout` ([`start_timeout_arg`]) and what
/// ssf's backstop around it is derived from: one allowance for
/// provisioning, not three rival ones that could cut each other short.
const PROVISION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How long the guest may take to answer on ssh as `ssf` once it has
/// provisioned itself. Not a round number: `/etc/ssf-image-built` means
/// "the image is provisioned", and it is written *before*
/// `ssf-seed.service` is queued, so what the host waits for after it is
/// the seed -- and `vm/guest/seed-lima.sh` may spend 120 seconds waiting
/// for the share plus 120 for the data disk before it copies the home
/// tree and installs `authorized_keys`. A shorter allowance gives up on a
/// slow but healthy guest with "provisioned, but the guest does not
/// answer as ssf".
///
/// The marker was left meaning what it says rather than moved to the end
/// of the seed: the seed runs at *every* boot from its unit, while the
/// marker is what tells a boot whether the image still needs
/// provisioning. Making it wait for the seed would tie a once-per-image
/// fact to a once-per-boot one, and `ssf vm reset` (a fresh root, the old
/// data disk) reads it on a boot whose seed has its own reasons to be
/// slow. The test `the_ssh_wait_outlasts_the_seeds_own_waits` holds this
/// against the script itself.
const SEED_TIMEOUT: Duration = Duration::from_secs(8 * 60);
/// Every `limactl` invocation gets an upper bound, so a `limactl` that
/// never returns (a lost hostagent, a qemu waiting on something) cannot
/// wedge ssf with no output and no child to look at. The bounds below are
/// for a command that is stuck, not for a slow one: a person waiting for a
/// build should never see one.
///
/// A probe or a short read over `limactl shell`, and `limactl --version`.
const PROBE_LIMIT: Duration = Duration::from_secs(60);
/// `limactl list`, `disk` and `edit`: local bookkeeping.
const QUICK_LIMIT: Duration = Duration::from_secs(2 * 60);
/// The liveness question as the forwarding gate asks it
/// ([`Vm::running_now`]): once in front of every command the host sends
/// into the guest, with a person waiting on the answer and the bar
/// widget asking a few times a minute. It reads local bookkeeping, so
/// seconds are already generous, and the gate treats "could not ask" as
/// "cannot tell" and forwards anyway -- so cutting a pathologically slow
/// answer short costs nothing but the answer. The supervisor's own
/// polling keeps [`QUICK_LIMIT`]: it gives up after ten unanswered
/// probes in a row, so a slow answer there must be waited for rather
/// than turned into a "cannot tell".
pub(super) const LIVENESS_LIMIT: Duration = Duration::from_secs(15);
/// The two questions [`Vm::lima_survey`] asks, back to back, before
/// `ssf uninstall` prints its first line. On the listing's own bound a
/// lima that had stopped answering would hold the report silent for
/// twice [`QUICK_LIMIT`] -- and unlike the callers that bound applies
/// to, the survey is built to treat "could not be asked" as an answer
/// of its own, with lima's filesystem behind it. Waiting longer buys it
/// nothing it cannot get another way.
const SURVEY_LIMIT: Duration = PROBE_LIMIT;
/// `limactl create`, which downloads the base image the first time.
const CREATE_LIMIT: Duration = Duration::from_secs(30 * 60);
/// `limactl stop`: lima gives the guest minutes to shut down first.
const STOP_LIMIT: Duration = Duration::from_secs(10 * 60);
/// What ssf adds to a limit something else enforces itself (lima's
/// `--timeout`, the provisioning wait's own deadline), so that the inner
/// limit fires first and a person reads its message rather than ssf's
/// backstop: one limit, plus a margin, not two rival ones.
const OWN_TIMEOUT_MARGIN: Duration = Duration::from_secs(2 * 60);
/// How often [`wait_within`] looks at the child.
const WAIT_POLL: Duration = Duration::from_millis(100);
/// The yq expression `limactl edit` takes to turn the data disk's
/// `format` off once the disk exists (`limactl help yq-restrictions`).
const FORMAT_OFF: &str = ".additionalDisks[0].format = false";

/// The oldest lima ssf drives. Three things in this file are pinned to a
/// lima version, and this is the highest of them:
///
/// * The template names its base image `template:_images/archlinux`. That
///   opaque form of the locator is lima 2.0's ("Template locator
///   `template://...` should be written `template:...` since Lima v2.0",
///   lima says on the older spelling); a 1.x lima parses it as an empty
///   filename and dies with `filename "" is invalid`.
/// * The `_images/` templates themselves arrived in lima 1.1 -- and lima
///   2.0.0's release *tarball* ships that directory empty (its Makefile
///   left `TEMPLATE_IMAGES` out of the artifact), so
///   `template:_images/archlinux` is "not found" on the binaries lima
///   published for it, however it is spelled. A 2.0.0 built from source,
///   which is what Homebrew does, has the templates and would work; the
///   floor excludes it anyway rather than admitting a version whose
///   official binaries do not run ssf, and 2.0.1 came four hours later
///   the same day.
/// * The share the guest provisions itself from is mounted before the
///   provision scripts run, which is what leaving `mountType` out of the
///   template buys (see [`render_template`]): 9p for qemu is lima's
///   default only from 1.0, and reverse-sshfs before that, which the
///   host agent mounts around the time the hook starts waiting for it
///   rather than ahead of it (see [`boot_hook`]).
///
/// ssf is tested against 2.2.0.
pub const MIN_LIMA: LimaVersion = LimaVersion(2, 0, 1);

/// A lima version, compared as major, then minor, then patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LimaVersion(pub u32, pub u32, pub u32);

impl std::fmt::Display for LimaVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// The version out of `limactl --version` (`limactl version 2.2.0`), or
/// `None` when the line does not carry one -- a lima built without its
/// version stamped in prints `<unknown>`, and preflight lets that
/// through rather than refusing a lima it merely failed to read.
///
/// Only the numeric core is compared: a build from git describes itself
/// as `2.2.0-15-g1234567`, which is *newer* than 2.2.0, so reading the
/// suffix as semver's pre-release would turn a newer lima into an older
/// one and refuse it. The cost is that `2.0.1-rc.0` passes as 2.0.1,
/// which is the direction to err in.
fn parse_lima_version(out: &str) -> Option<LimaVersion> {
    let word = out
        .lines()
        .find(|l| !l.trim().is_empty())?
        .split_whitespace()
        .last()?;
    let core = word.trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some(LimaVersion(major, minor, patch))
}

/// lima's name for a host architecture (the guest's too).
pub fn lima_arch(arch: &str) -> Result<&'static str> {
    match arch {
        "x86_64" => Ok("x86_64"),
        "aarch64" => Ok("aarch64"),
        other => bail!("no lima guest for the {other} architecture (x86_64 or aarch64)"),
    }
}

/// The image lima boots for an architecture when `[vm] image` is unset:
/// Arch's cloud image for x86_64, Ubuntu LTS for aarch64 (Arch has no
/// official aarch64 cloud image). Firecracker's fixed Ubuntu guest is
/// independent of this lima default. lima keeps the URL and digest.
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
/// reverse-sshfs would not be). That default is what [`MIN_LIMA`] holds
/// the floor for, rather than the type being written here: naming one
/// would have to name the right one per `vmType`, and would override
/// lima's own fallback for a guest whose kernel cannot do 9p. It leaves
/// one case the floor cannot reach -- a `mountType` set for every
/// instance in lima's `~/.lima/_config` -- which is why [`boot_hook`]'s
/// failure names `mountType` rather than only the mount point.
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
    y.push_str("provision:\n  - mode: system\n    script: |\n");
    for line in boot_hook().lines() {
        if line.is_empty() {
            y.push('\n');
        } else {
            y.push_str(&format!("      {line}\n"));
        }
    }
    y
}

/// How long the template's hook waits for the share before it gives up.
pub const SHARE_WAIT_SECS: u32 = 120;

/// The template's `provision` script: what lima runs as root at every
/// boot, before the guest's own boot script, which lives in the share.
///
/// The wait for the share is here rather than in `lima-boot.sh` because
/// `lima-boot.sh` is *in* the share: a hook that only exec'd it could not
/// report the share missing at all -- the exec would fail, nothing would
/// be written to [`PROVISION_LOG`], and the host would wait out
/// [`PROVISION_TIMEOUT`] with nothing to show. So the one failure the
/// guest cannot delegate is handled here, in the log the host reads.
///
/// What it says when the share never arrives names `mountType`, because
/// that decides *when* the share is there and it is not ssf's to decide
/// alone. 9p and virtiofs are mounted from cloud-init's fstab and
/// lima's own boot scripts, both before this hook runs. reverse-sshfs
/// is mounted by the host agent instead, once the guest's boot scripts
/// have satisfied its essential requirements -- which is about when
/// this hook starts, so it races [`SHARE_WAIT_SECS`] rather than
/// beating it, and usually wins. [`MIN_LIMA`] settles lima's default
/// (9p for qemu since lima 1.0, virtiofs for vz), but the person's own
/// `_config/default.yaml` and `_config/override.yaml` in lima's home
/// set it too, the latter over any template, and no version floor
/// reaches those. So the message says where to look rather than what to
/// conclude.
///
/// The log is truncated in the first line of the hook, before that wait,
/// and not in `lima-boot.sh`: the host's rule is that a log with none of
/// the guest scripts running means *this* boot's attempt died, and while
/// the hook waited for the share the log on disk was still the previous
/// attempt's -- a probe landing in that window read a stale failure and
/// printed the wrong tail.
fn boot_hook() -> String {
    format!(
        r#"#!/bin/bash
# every boot, as root: provision the guest once, then seed it. The work is
# in lima-boot.sh, which is in the host's share -- so waiting for that
# share, and saying so when it never arrives, has to happen here, in the
# log the host watches (src/vm/lima.rs): an `exec` of a script that is not
# there leaves no log at all.
log={PROVISION_LOG}
# This boot's attempt starts with an empty log, and it starts here, before
# anything can be waited for: the host reads a non-empty log with none of
# the guest scripts running as "this attempt died", so a log left by the
# previous boot must not still be there while this one gets going.
: > "$log"
boot={GUEST_MOUNT}/guest/lima-boot.sh
for ((i = 0; i < {SHARE_WAIT_SECS}; i++)); do
    [ -f "$boot" ] && break
    sleep 1
done
if [ ! -f "$boot" ]; then
    printf 'ssf-provision: FAILED: %s is not there after {SHARE_WAIT_SECS}s: the {GUEST_MOUNT} share never arrived. Check mountType on the host -- 9p and virtiofs are mounted before this runs, reverse-sshfs while it waits -- in the template and in _config/default.yaml and _config/override.yaml under lima home (~/.lima, or $LIMA_HOME).\n' "$boot" | tee -a "$log"
    exit 1
fi
exec bash "$boot"
"#
    )
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
/// The template's boot hook truncates the log in its first line and
/// `lima-boot.sh`, which it execs, writes to it before it does anything
/// else, so "a log and nothing running" means this attempt died —
/// including the failure the script reports itself, which used to go to
/// stdout only and left the host waiting out [`PROVISION_TIMEOUT`] for a
/// guest that had already given up.
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
    // `exit 0` is load-bearing: what the probe found is in its output, not
    // in its status, and every one of these tests fails in the ordinary
    // case (no marker yet, or nothing running any more). Without it the
    // last test decides the status, the run counts as a failed limactl
    // call, and its output is thrown away -- which is how a build once sat
    // waiting for a marker the probe had already seen.
    format!(
        "test -f {PROVISION_MARKER} && echo done; test -s {PROVISION_LOG} && echo log; pgrep -f '{PROVISION_PGREP}' >/dev/null 2>&1 && echo running; exit 0"
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
/// wait calls provisioning dead. `lima-boot.sh` is the parent of
/// `provision.sh`, so one of the two matches for the whole attempt and a
/// single idle round is already a strong signal; three (fifteen seconds)
/// is the margin for a probe that lands on the moment the boot script
/// exits, or on a guest whose `pgrep` came back empty for a reason of its
/// own. It is a delay, not a hang: the failure still arrives in seconds
/// instead of after [`PROVISION_TIMEOUT`].
const IDLE_ROUNDS: u32 = 3;

/// Provisioning wrote a log, nothing of it runs and the marker is not
/// there: it died. Anything else keeps the wait going -- including a
/// round with no log at all, which is a boot still on its way into
/// `lima-boot.sh` (the boot hook truncates the log in its first line, so
/// the log a failed attempt left cannot be read as this attempt's).
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

/// A probe that could not run has to be told apart from a guest that is
/// never going to answer. Given what `limactl list` says about the
/// instance -- `None` when lima does not list it at all -- this returns
/// how to describe the end, or `None` when the wait should go on.
///
/// Only `Running` is worth waiting for: a `Stopped` instance would need
/// `limactl start` (which is not this wait's job), and a `Broken` one
/// needs a person. `src/vm/guest.rs`'s `ssh_loop` makes the same call for
/// Firecracker by looking at the process.
///
/// Every other status is read as terminal, transient-looking ones
/// included, and that is only sound because of where this is called
/// from: the wait runs after `limactl start` has *returned*, so lima has
/// finished bringing the instance up and a status of its own that is not
/// `Running` is not a stage this wait can sit through. A caller that
/// polled while `limactl start` was still running would have to treat a
/// status it does not know as "keep waiting" instead.
fn terminal_state(status: Option<&str>) -> Option<String> {
    match status {
        None => Some("is not there any more (lima does not list it)".to_string()),
        Some("Running") => None,
        Some(s) => Some(format!("is {}, not running", s.to_lowercase())),
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

/// [`lima_home_from`] for this process.
pub fn lima_home() -> Option<PathBuf> {
    lima_home_from(
        std::env::var("LIMA_HOME").ok().as_deref(),
        dirs::home_dir().as_deref(),
    )
}

/// [`disks_dir_from`] for this process.
pub fn disks_dir() -> Option<PathBuf> {
    disks_dir_from(
        std::env::var("LIMA_HOME").ok().as_deref(),
        dirs::home_dir().as_deref(),
    )
}

/// The provisioning allowance as `limactl start --timeout` takes it: the
/// guest provisions inside that call, so lima must wait for it at least
/// as long as ssf's own wait for the marker would.
fn start_timeout_arg() -> String {
    format!("{}m", PROVISION_TIMEOUT.as_secs() / 60)
}

/// Does this lima YAML let lima format the data disk? A line test rather
/// than a YAML parse: ssf writes the template itself, and lima's copy of
/// it keeps the key on a line of its own.
fn says_format_true(yaml: &str) -> bool {
    yaml.lines()
        .any(|l| l.trim_start().trim_start_matches("- ").trim() == "format: true")
}

/// What can be done about a `format: true` that outlived the build that
/// wrote it, given what says it and whether the instance is running.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Repair {
    /// Rewrite ssf's own template: a plain file, always possible.
    template: bool,
    /// Turn `format` off in the instance's own copy with `limactl edit`.
    instance: bool,
    /// The instance's copy says `format: true` and cannot be changed
    /// here: `limactl edit` refuses a running instance (lima 2.2.0,
    /// "cannot edit a running instance"), so pretending it was repaired
    /// is how a guest ends up booting from a template that lets lima
    /// reformat the disk the factory lives on.
    blocked: bool,
}

/// Why a `format: false` is being written: the build that created the
/// disk finishing its job, or a later command finding a flag an
/// unfinished build left behind. Only the second is worth a warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    FinishingBuild,
    FoundStale,
}

/// What the write of a `format: false` says on its way past: `disk` is
/// the data disk, `which` the copy of the template being changed. The
/// build that made the disk is the one build allowed to let lima format
/// it, so that build turning the flag off is stating a fact, not putting
/// anything right; every other caller has found a flag an unfinished
/// build left behind, which is a warning. Kept out of the logging so the
/// difference can be held to in a test.
fn format_off_note(why: Why, disk: &str, which: &str) -> String {
    match why {
        Why::FinishingBuild => {
            format!("the data disk {disk} carries the factory now; turning `format` off in {which}")
        }
        Why::FoundStale => format!(
            "the data disk {disk} exists, but {which} still lets lima format it (a build that did not finish); putting `format: false` back"
        ),
    }
}

/// The decision behind [`Vm::repair_stale_format`], kept separate from
/// the file and process work so it can be held to the rules:
///
/// * with no data disk there is nothing to protect -- the build that
///   creates the disk is the one build that may hand lima `format: true`;
/// * ssf's own template is rewritten whenever it is stale, because it is
///   what `ssf vm reset` creates the next instance from;
/// * the instance's copy is edited only while the instance is stopped,
///   and a running instance is reported, never quietly skipped.
fn plan_repair(
    disk_exists: bool,
    template_stale: bool,
    instance_stale: bool,
    running: bool,
) -> Repair {
    if !disk_exists {
        return Repair::default();
    }
    Repair {
        template: template_stale,
        instance: instance_stale && !running,
        blocked: instance_stale && running,
    }
}

/// What an error calls the command that ran.
fn limactl_label(args: &[&str]) -> String {
    format!("limactl {}", args.join(" "))
}

/// Read a child's pipe to the end on a thread of its own.
fn drain<R: Read + Send + 'static>(mut r: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        buf
    })
}

/// Wait for a child, and kill it when `limit` passes. The point is that
/// no external command can leave ssf waiting for ever with nothing to
/// show for it (an `ssf vm build` once sat for a quarter of an hour with
/// no output and no child process); the error names the command and the
/// limit, so what timed out is in the message rather than in a debugger.
fn wait_within(child: &mut Child, label: &str, limit: Duration) -> Result<ExitStatus> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(st) = child
            .try_wait()
            .with_context(|| format!("waiting for `{label}`"))?
        {
            return Ok(st);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "`{label}` did not finish within {}; ssf stopped it",
                human_duration(limit)
            );
        }
        std::thread::sleep(WAIT_POLL);
    }
}

/// A limit as an error says it.
pub(super) fn human_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs == 0 {
        return format!("{} ms", d.as_millis());
    }
    match (secs / 60, secs % 60) {
        (0, s) => plural(s, "second"),
        (m, 0) => plural(m, "minute"),
        (m, s) => format!("{} {}", plural(m, "minute"), plural(s, "second")),
    }
}

fn plural(n: u64, unit: &str) -> String {
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

fn require_safe_root(dir: &Path) -> Result<()> {
    if dir.join("ssf-safe-root-v2").is_file() || dir.join("ssf-fresh-root-v2").is_file() {
        return Ok(());
    }
    bail!(
        "this legacy Lima root still contains the old destructive factory seed script; refusing to boot. Run ssf vm reset, then ssf vm start to provision a new disposable root. The persistent data disk, configuration, credentials and worktrees are preserved"
    );
}

mod lifecycle;
mod query;

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
mod tests;
