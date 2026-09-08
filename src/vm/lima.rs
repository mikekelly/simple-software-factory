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
    HostFacts, Sizes, Stray, StrayKind, Survey, Vm, make_executable, plan_grow, scripts_dir,
    sizes_for, which,
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

/// Disks last, everything else before them by name: `limactl disk
/// delete` refuses a disk still attached to its instance, so that is the
/// order a person has to work in. Directory strays have no such
/// dependency and sort among the instances. Both ways of finding
/// strays -- lima's listing and lima's filesystem -- report in it, so
/// what `ssf uninstall` prints does not depend on which one answered.
pub(super) fn sort_strays(strays: &mut [Stray]) {
    strays.sort_by(|a, b| {
        (matches!(a.kind, StrayKind::LimaDisk), &a.name)
            .cmp(&(matches!(b.kind, StrayKind::LimaDisk), &b.name))
    });
}

/// Does this name have the shape ssf gives things in lima's home
/// (`ssf-<[vm] name>`)? That prefix is all there is to go on, so a lima
/// instance someone else happened to call `ssf-something` matches too --
/// which costs a line in a report and nothing else, because nothing here
/// removes what it finds. The report says what it observed rather than
/// claiming the thing is ssf's. `ssf-` alone is not one: an empty
/// `[vm] name` is a broken configuration in its own right (#169), and
/// nothing here should describe one as a stray.
pub fn is_ssf_name(name: &str) -> bool {
    name.strip_prefix("ssf-")
        .is_some_and(|rest| !rest.is_empty())
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
/// needs a person. `src/vm.rs`'s `ssh_loop` makes the same call for
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

    /// Run limactl for its output; stderr goes into the error. Every
    /// caller names its own bound: a local lookup, a create that
    /// downloads an image and a probe with a person waiting are not the
    /// same wait.
    fn limactl_output_within(&self, args: &[&str], limit: Duration) -> Result<String> {
        let mut cmd = self.limactl();
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        let label = limactl_label(args);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("running {}", self.limactl_hint()))?;
        // Drain both pipes while it runs: a child blocked writing into a
        // full pipe would look exactly like a hang, and be killed for it.
        let out = drain(child.stdout.take().expect("piped"));
        let err = drain(child.stderr.take().expect("piped"));
        let started = Instant::now();
        debug!(command = %label, "running limactl");
        let status = wait_within(&mut child, &label, limit)?;
        debug!(command = %label, ?status, elapsed = ?started.elapsed(), "limactl exited; draining its output");
        let stdout = out.join().unwrap_or_default();
        let stderr = err.join().unwrap_or_default();
        debug!(command = %label, elapsed = ?started.elapsed(), "limactl output drained");
        if !status.success() {
            bail!(
                "`{label}` failed ({status}): {}",
                String::from_utf8_lossy(&stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&stdout).to_string())
    }

    /// Run limactl with this terminal (its progress lines are worth
    /// seeing), within [`QUICK_LIMIT`].
    fn limactl_run(&self, args: &[&str]) -> Result<()> {
        self.limactl_run_within(args, QUICK_LIMIT)
    }

    /// [`Vm::limactl_run`] with a bound of its own: `create` downloads an
    /// image, `start` provisions a guest, `stop` waits for one.
    fn limactl_run_within(&self, args: &[&str], limit: Duration) -> Result<()> {
        let mut cmd = self.limactl();
        cmd.args(args);
        let label = limactl_label(args);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("running {}", self.limactl_hint()))?;
        let started = Instant::now();
        debug!(command = %label, "running limactl");
        let st = wait_within(&mut child, &label, limit)?;
        debug!(command = %label, status = ?st, elapsed = ?started.elapsed(), "limactl exited");
        if !st.success() {
            bail!("`{label}` failed ({st})");
        }
        Ok(())
    }

    fn limactl_hint(&self) -> String {
        match &self.cfg.limactl {
            Some(p) => format!("[vm] limactl = {p}"),
            None => "limactl (is lima installed and on PATH?)".to_string(),
        }
    }

    /// Which limactl ran, for a message about a limactl that *did* run:
    /// [`Vm::limactl_hint`] is written for one that could not be found,
    /// and reads as a question rather than an answer next to a version.
    fn limactl_where(&self) -> String {
        match &self.cfg.limactl {
            Some(p) => format!("[vm] limactl = {p}"),
            None => which("limactl").map_or_else(
                || "limactl on PATH".to_string(),
                |p| p.display().to_string(),
            ),
        }
    }

    /// The instance, when lima has it.
    pub(super) fn lima_instance(&self) -> Result<Option<Instance>> {
        self.lima_instance_within(QUICK_LIMIT)
    }

    /// [`Vm::lima_instance`] with a bound of its own, for the liveness
    /// question that is asked on a waiting path.
    fn lima_instance_within(&self, limit: Duration) -> Result<Option<Instance>> {
        let name = self.lima_name();
        Ok(self
            .lima_instances_within(limit)?
            .into_iter()
            .find(|i| i.name == name))
    }

    /// [`Vm::lima_instances_within`] at the listing's own bound.
    pub(super) fn lima_instances(&self) -> Result<Vec<Instance>> {
        self.lima_instances_within(QUICK_LIMIT)
    }

    /// Every instance lima has, so the survey can see the ones this
    /// configuration does not name as well as the one it does.
    fn lima_instances_within(&self, limit: Duration) -> Result<Vec<Instance>> {
        let out = self.limactl_output_within(&["list", "--json"], limit)?;
        Ok(parse_instances(&out))
    }

    /// The data disk, when lima has it.
    fn lima_disk(&self) -> Result<Option<Disk>> {
        self.lima_disk_within(QUICK_LIMIT)
    }

    /// [`Vm::lima_disk`] with a bound of its own, for the survey that
    /// asks it on a path with a person waiting.
    fn lima_disk_within(&self, limit: Duration) -> Result<Option<Disk>> {
        let name = self.lima_disk_name();
        Ok(self
            .lima_disks_within(limit)?
            .into_iter()
            .find(|d| d.name == name))
    }

    /// [`Vm::lima_disks_within`] at the listing's own bound.
    pub(super) fn lima_disks(&self) -> Result<Vec<Disk>> {
        self.lima_disks_within(QUICK_LIMIT)
    }

    /// Every disk lima has, for the same reason as
    /// [`Vm::lima_instances_within`].
    fn lima_disks_within(&self, limit: Duration) -> Result<Vec<Disk>> {
        let out = self.limactl_output_within(&["disk", "list", "--json"], limit)?;
        Ok(parse_disks(&out))
    }

    /// Is the instance running, or why could that not be asked? The
    /// probe forks a ~60 MB Go binary, so a fired resource limit or a
    /// fork that failed under load is a plausible answer, and it is not
    /// the same answer as "stopped".
    pub(super) fn lima_running_probe(&self, limit: Duration) -> Result<bool> {
        self.lima_instance_within(limit)
            .map(|inst| inst.is_some_and(|i| i.is_running()))
            .with_context(|| format!("asking lima whether {} is running", self.lima_name()))
    }

    /// [`Vm::lima_running_probe`] for the callers that only want the
    /// answer, with "could not ask" as `None` and a warning in the log.
    /// `Vm::supervise` polls this, and a probe failure read as "stopped"
    /// once ended the supervisor with "the VM exited" over a VM that was
    /// running.
    pub(super) fn lima_running_state(&self) -> Option<bool> {
        match self.lima_running_probe(QUICK_LIMIT) {
            Ok(running) => Some(running),
            Err(e) => {
                warn!("{e:#}");
                None
            }
        }
    }

    /// Where lima keeps this instance: `<lima home>/<name>`.
    fn lima_instance_dir(&self) -> Option<PathBuf> {
        self.lima_home.as_ref().map(|h| h.join(self.lima_name()))
    }

    /// Where lima keeps this VM's external data disk:
    /// `<lima home>/_disks/<disk>`.
    fn lima_disk_dir(&self) -> Option<PathBuf> {
        self.lima_home
            .as_ref()
            .map(|h| h.join("_disks").join(self.lima_disk_name()))
    }

    /// Is either of them on disk? What is left to go on when `limactl`
    /// will not answer. `None` when there is no lima home to look in --
    /// no home directory at all -- which is not the same as "nothing
    /// there".
    fn lima_leftovers(&self) -> Option<bool> {
        let (instance, disk) = (self.lima_instance_dir()?, self.lima_disk_dir()?);
        Some(instance.exists() || disk.exists())
    }

    /// What lima holds of this VM. The data disk is asked about as well
    /// as the instance: `limactl delete` of an instance leaves an
    /// external disk where it is, so a disk full of clones and worktrees
    /// can outlive the instance that mounted it -- and nothing can then
    /// be started to look inside it, which is why that case is not
    /// `startable`.
    pub(super) fn lima_survey(&self) -> Survey {
        let dir = self.dir.exists();
        let (mine, others) = match self.lima_instances_within(SURVEY_LIMIT) {
            Ok(all) => self.split_instances(all),
            Err(e) => return self.lima_unanswered(dir, "the instance", &self.lima_name(), &e),
        };
        let mut strays = others;
        let mut home_unread = false;
        let disk = match self.lima_disks_within(SURVEY_LIMIT) {
            Ok(all) => {
                let (mine, others) = self.split_disks(all);
                strays.extend(others);
                Some(mine)
            }
            Err(e) => {
                warn!(
                    "could not ask lima about the data disk {}: {e:#}",
                    self.lima_disk_name()
                );
                // Lima's own filesystem, for the one answer whose loss
                // cannot be undone -- and for the *other* disks in it,
                // on the same rule. Leaving those out hid an old data
                // disk full of clones in exactly the case where
                // `limactl disk list` is no use to the person either.
                let (disks, unread) = self.disk_strays_read();
                strays.extend(disks);
                home_unread |= unread;
                // A directory nobody could read says nothing about
                // whether the disk is in it.
                if unread {
                    None
                } else {
                    self.lima_disk_dir().map(|p| p.exists())
                }
            }
        };
        sort_strays(&mut strays);
        Survey {
            present: match (mine.is_some(), disk) {
                (true, _) | (false, Some(true)) => Some(true),
                (false, Some(false)) => Some(dir),
                (false, None) => dir.then_some(true),
            },
            running: Some(mine.as_ref().is_some_and(Instance::is_running)),
            startable: mine.is_some(),
            data: disk,
            strays,
            // `[vm] dir` is the caller's question; lima's own home is
            // this one's.
            base_unread: home_unread,
        }
    }

    /// This configuration's instance, and every other `ssf-*` one lima
    /// holds. A `[vm] name` changed in the config file renames nothing
    /// in lima's home: the old instance keeps its old name, and asking
    /// only about the new one is how `ssf uninstall` came to say "no VM"
    /// over a VM that was sitting right there.
    pub(super) fn split_instances(&self, all: Vec<Instance>) -> (Option<Instance>, Vec<Stray>) {
        let name = self.lima_name();
        let mut mine = None;
        let mut strays = Vec::new();
        for i in all {
            if i.name == name {
                mine = Some(i);
            } else if is_ssf_name(&i.name) {
                strays.push(Stray::lima_instance(i.name));
            }
        }
        (mine, strays)
    }

    /// [`Vm::split_instances`] for the data disks, which outlive their
    /// instances and hold the clones and worktrees.
    pub(super) fn split_disks(&self, all: Vec<Disk>) -> (bool, Vec<Stray>) {
        let name = self.lima_disk_name();
        let mut mine = false;
        let mut strays = Vec::new();
        for d in all {
            if d.name == name {
                mine = true;
            } else if is_ssf_name(&d.name) {
                strays.push(Stray::lima_disk(d.name));
            }
        }
        (mine, strays)
    }

    /// A `limactl` question that came back an error.
    ///
    /// An unrunnable or silent `limactl` -- moved by an upgrade, off the
    /// PATH the service runs under, a stale `[vm] limactl`, a locked
    /// lima home -- is evidence about the tool, not about the machine.
    /// So lima's own filesystem answers instead: `<lima home>/<name>`
    /// for the instance, `<lima home>/_disks/<disk>` for the data disk.
    /// Nothing there is a real "no VM", and a Mac that never built one
    /// gets its clean `ssf uninstall`. Something there is a VM that
    /// cannot be asked about -- never a missing binary's licence to
    /// treat a disk full of workspaces as absent.
    fn lima_unanswered(&self, dir: bool, what: &str, name: &str, e: &anyhow::Error) -> Survey {
        warn!("could not ask lima about {what} {name}: {e:#}");
        let instance = self.lima_instance_dir().map(|p| p.exists());
        let disk = self.lima_disk_dir().map(|p| p.exists());
        // Same evidence, same rule: what lima's home holds is what there
        // is to go on. Without this a stray goes unmentioned exactly
        // when the person can least find it themselves -- `limactl list`
        // is the command that just failed.
        let (strays, unread) = self.strays_on_disk_read();
        let here = instance == Some(true) || disk == Some(true);
        // A lima home nobody could read cannot say "nothing of it is
        // here": `p.exists()` on a denied `stat` is a confident `false`
        // about a directory that was never looked in.
        let nothing = !unread && instance == Some(false) && disk == Some(false);
        Survey {
            present: if here || dir {
                Some(true)
            } else if nothing {
                Some(false)
            } else {
                None
            },
            // Nothing of it anywhere is not running; anything else is a
            // question that was never answered, and `ssf vm start` is no
            // cure for it.
            running: nothing.then_some(false),
            startable: false,
            data: disk,
            strays,
            base_unread: unread,
        }
    }

    /// Every `ssf-*` instance and disk in lima's home that this
    /// configuration does not name, read off the filesystem for when
    /// `limactl` will not list them. Reported, never removed -- so
    /// naming a directory lima might disown costs a line, not a VM.
    pub(super) fn strays_on_disk(&self) -> Vec<Stray> {
        self.strays_on_disk_read().0
    }

    /// [`Vm::strays_on_disk`], and whether either of lima's directories
    /// could not be read.
    pub(super) fn strays_on_disk_read(&self) -> (Vec<Stray>, bool) {
        let (mut strays, a) = self.instance_strays_read();
        let (disks, b) = self.disk_strays_read();
        strays.extend(disks);
        sort_strays(&mut strays);
        (strays, a || b)
    }

    /// The instance half of [`Vm::strays_on_disk`].
    pub(super) fn instance_strays_on_disk(&self) -> Vec<Stray> {
        self.instance_strays_read().0
    }

    /// [`Vm::instance_strays_on_disk`], and whether lima's home could be
    /// read at all.
    fn instance_strays_read(&self) -> (Vec<Stray>, bool) {
        let (names, unread) = Self::ssf_dirs_in(self.lima_home.as_deref(), &self.lima_name());
        (
            names.into_iter().map(Stray::lima_instance).collect(),
            unread,
        )
    }

    /// The data-disk half, which is the one that holds clones and
    /// worktrees -- so it is wanted on its own whenever `limactl disk
    /// list` is the call that failed.
    pub(super) fn disk_strays_on_disk(&self) -> Vec<Stray> {
        self.disk_strays_read().0
    }

    /// [`Vm::disk_strays_on_disk`], and whether lima's disk directory
    /// could be read at all.
    fn disk_strays_read(&self) -> (Vec<Stray>, bool) {
        let (names, unread) = Self::ssf_dirs_in(
            self.lima_home.as_ref().map(|h| h.join("_disks")).as_deref(),
            &self.lima_disk_name(),
        );
        (names.into_iter().map(Stray::lima_disk).collect(), unread)
    }

    /// The `ssf-*` directories in one of lima's directories, other than
    /// `ours`, and whether the directory could be read at all.
    ///
    /// The same rule as `[vm] dir`: a `read_dir` that failed is "nobody
    /// looked", not "nothing there". A lima home left root-owned by an
    /// earlier `sudo` is one of the reasons `limactl` fails in the first
    /// place, so the two arrive together -- and reading it as empty
    /// printed `no VM` over `safe to remove` about a home holding an
    /// instance and a disk of clones.
    fn ssf_dirs_in(dir: Option<&Path>, ours: &str) -> (Vec<String>, bool) {
        let Some(dir) = dir else {
            return (Vec::new(), false);
        };
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return (Vec::new(), dir.exists()),
        };
        let names = entries
            .flatten()
            // A symlink is not a lima instance directory, and the same
            // rule `[vm] dir`'s scan uses.
            .filter(|e| std::fs::symlink_metadata(e.path()).is_ok_and(|m| m.is_dir()))
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .filter(|name| name != ours && is_ssf_name(name))
            .collect();
        (names, false)
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
        let name = self.lima_name();
        // Whichever lima wrote, and only if it is there: returning a path
        // to a file that does not exist left `ssf vm console` showing
        // `tail: cannot open ...serial.log`, which reads as a broken
        // command rather than as "this instance has never been booted".
        for log in ["serial.log", "serialv.log"] {
            let p = dir.join(log);
            if p.exists() {
                return Ok(p);
            }
        }
        bail!(
            "lima instance {name} has no console log yet ({}); lima writes serial.log (serialv.log with a virtio console) at the first `ssf vm start`",
            dir.display()
        )
    }

    /// What a build needs: limactl that runs, and qemu for the
    /// architecture wherever lima will drive the VM with it -- always on
    /// Linux (its only driver there), and on macOS when `[vm] vm_type`
    /// asks for qemu. Keying that on the operating system alone let a Mac
    /// with `vm_type = "qemu"` and no qemu through preflight, to fail
    /// inside `limactl create` instead.
    fn lima_preflight(&self) -> Result<()> {
        check_name(&self.cfg.name)?;
        let arch = self.lima_arch()?;
        let os = std::env::consts::OS;
        let vm_type = self.cfg.vm_type.as_deref();
        let version = match self.limactl_output_within(&["--version"], PROBE_LIMIT) {
            Ok(out) => out,
            Err(e) => bail!(
                "limactl does not run ({}): {e:#}; install lima {MIN_LIMA} or newer (`brew install lima` on macOS, the `lima` package on Linux) or set [vm] limactl to it",
                self.limactl_hint()
            ),
        };
        // The version was fetched anyway to see that limactl runs, so it
        // is read: an older lima does not fail here, it fails inside
        // `limactl create` on a base image it could not resolve, which
        // does not name the reason.
        match parse_lima_version(&version) {
            Some(v) if v < MIN_LIMA => bail!(
                "this is lima {v} ({}), and ssf needs {MIN_LIMA} or newer: the VM template names its base image the way lima 2.0 spells a template locator, and leaves the share's mount type to lima, whose default for qemu is 9p -- mounted before the guest provisions itself -- only from lima 1.0. Upgrade lima (`brew upgrade lima` on macOS, your distribution's package or lima's release tarball on Linux), or point [vm] limactl at a newer one",
                self.limactl_where()
            ),
            Some(_) => {}
            None => warn!(
                "no version in `limactl --version` ({}); ssf needs lima {MIN_LIMA} or newer",
                version.trim()
            ),
        }
        if !platform::is_macos() && vm_type == Some("vz") {
            bail!("[vm] vm_type = \"vz\" is macOS only; unset it or use \"qemu\" here");
        }
        if super::lima_uses_qemu(os, vm_type) {
            let qemu = format!("qemu-system-{arch}");
            if which(&qemu).is_none() {
                bail!(
                    "{qemu} is not on PATH; {}",
                    super::qemu_install_hint(os, arch)
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
    /// provision it; `--force` deletes an existing instance first. The
    /// data disk is kept, with one exception: a disk an earlier build
    /// created and no guest ever used is blank and unformattable, and
    /// `--force` re-creates that one (see [`Vm::unproven_disk`]).
    pub(super) async fn lima_build(&self, host: &Config, force: bool) -> Result<()> {
        self.lima_preflight()?;
        let name = self.lima_name();
        if let Some(inst) = self.lima_instance()? {
            if !force {
                // A build that died between `limactl create` and the
                // guest answering ssh left `format: true` behind; this is
                // the run that would otherwise return without repairing
                // it, so it repairs it here -- and says so rather than
                // returning "instance exists" over a flag it could not
                // put right, which is how a later `ssf vm start` came to
                // boot a guest that was still allowed to reformat the
                // factory's disk.
                if let Some(yaml) = self.repair_stale_format(Some(&inst), Why::FoundStale) {
                    return Err(self.stale_format_error(&yaml));
                }
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
        let make_disk = self.take_data_disk(force)?;
        self.write_template(make_disk)?;
        self.write_share(host)?;
        if make_disk {
            self.create_data_disk()?;
        }
        self.lima_create()?;
        // From here the instance exists and boots with `format: true`
        // when this build made the disk. A failure that left it running
        // used to leave that flag behind with nothing able to change it
        // (`limactl edit` refuses a running instance), so the next
        // `ssf vm start` booted a guest that could still reformat a disk
        // by then holding the factory. The first boot is therefore
        // cleaned up after.
        if let Err(e) = self.lima_first_boot().await {
            self.after_failed_build().await;
            return Err(self.blank_disk_hint(e));
        }
        println!("built lima instance {name}; `ssf vm start` boots it");
        Ok(())
    }

    /// The data disk this build will run on, and whether the build has to
    /// make it -- which is the same as whether it may hand lima
    /// `format: true`, since only the build that creates a disk lets lima
    /// format it.
    ///
    /// A disk an earlier build created but never got a guest up on is
    /// marked ([`Vm::unproven_disk`]), and `--force` -- the flag that
    /// already means "make this instance again" -- deletes it and makes a
    /// fresh one. Without that, such a disk is unusable for good: lima's
    /// boot script is the only thing that formats it, no later build
    /// hands lima the flag, and every start waits its two minutes for a
    /// mount that will never come. A disk no marker points at is one some
    /// build saw a guest come up on, so nothing here deletes it.
    ///
    /// This is the one path in ssf that destroys the factory's data disk,
    /// which is why it is a step of its own rather than four lines in the
    /// middle of a build.
    fn take_data_disk(&self, force: bool) -> Result<bool> {
        if self.lima_disk()?.is_none() {
            return Ok(true);
        }
        if !force || !self.unproven_disk().exists() {
            return Ok(false);
        }
        let disk = self.lima_disk_name();
        info!(
            "the data disk {disk} was made by a build that never got a guest up on it; deleting it, and whatever it holds, and making a fresh one"
        );
        self.limactl_run(&["disk", "delete", &disk])?;
        let _ = std::fs::remove_file(self.unproven_disk());
        Ok(true)
    }

    /// Create the data disk, and mark it as one no guest has come up on
    /// yet ([`Vm::unproven_disk`]).
    fn create_data_disk(&self) -> Result<()> {
        let disk = self.lima_disk_name();
        let gib = self.sizes().data_gib;
        info!("creating lima disk {disk} ({gib} GiB)");
        self.limactl_run(&["disk", "create", &disk, "--size", &format!("{gib}GiB")])?;
        self.mark_disk_unproven();
        Ok(())
    }

    /// The first boot of a freshly created instance: the guest provisions
    /// itself inside `limactl start`, the host waits for the marker and
    /// then for ssh, and the instance is stopped again.
    async fn lima_first_boot(&self) -> Result<()> {
        let name = self.lima_name();
        info!("starting {name} for its first boot: the guest provisions itself (a few minutes)");
        self.limactl_start(&["start", "--timeout", &start_timeout_arg(), &name])?;
        self.wait_for_provisioning().await?;
        if let Ok(log) =
            self.limactl_output_within(&["shell", &name, "sudo", "cat", PROVISION_LOG], PROBE_LIMIT)
        {
            for line in log.lines().filter(|l| l.starts_with("provision: ")) {
                eprintln!("  {line}");
            }
        }
        if let Err(e) = self.wait_for_ssh(SEED_TIMEOUT).await {
            bail!(
                "{e:#} (provisioned, but the guest does not answer as {}); `ssf vm console` has its console",
                super::GUEST_USER
            );
        }
        // The guest has provisioned and answered as `ssf`, which it can
        // only do once `seed-lima.sh` has mounted the data disk and
        // written `authorized_keys` onto it: the disk carries a
        // filesystem and the factory now, so it is no longer one a
        // `--force` build may throw away.
        let _ = std::fs::remove_file(self.unproven_disk());
        // Nothing from here on may let lima format it. The template goes
        // first and at once, because it is what
        // `ssf vm reset` builds the next instance from and because
        // everything below can still fail; the instance's own copy
        // follows the stop, since `limactl edit` is for a stopped
        // instance.
        self.write_template(false)?;
        self.limactl_run_within(&["stop", &name], STOP_LIMIT)?;
        let stopped = self.lima_instance().ok().flatten();
        if let Some(yaml) = self.repair_stale_format(stopped.as_ref(), Why::FinishingBuild) {
            return Err(self.stale_format_error(&yaml));
        }
        Ok(())
    }

    /// After a first boot that failed: stop the instance and put
    /// `format: false` back. Both are best effort and nothing here
    /// replaces the build's own error -- the point is only that the next
    /// command finds a stopped instance whose template cannot reformat
    /// the data disk.
    async fn after_failed_build(&self) {
        // The build has already failed; this is the tidying up, and it
        // runs `limactl` on a path where `limactl` may itself be the
        // thing that is stuck. Everything below is bounded, but a person
        // watching a build that failed minutes ago deserves to know why
        // the tool is still working.
        println!(
            "the build failed; putting {} back to a state the next command can use (stopping it, and turning `format` off for the data disk). This can take a few minutes when limactl is not answering; the build's own error follows it.",
            self.lima_name()
        );
        if let Err(e) = self.lima_stop().await {
            warn!(
                "could not stop {} after the build failed ({e:#}); `limactl stop -f {}` does it",
                self.lima_name(),
                self.lima_name()
            );
        }
        let inst = self.lima_instance().ok().flatten();
        if let Some(yaml) = self.repair_stale_format(inst.as_ref(), Why::FoundStale) {
            warn!("{}", self.stale_format_note(&yaml));
        }
        // A disk this build created but never got a filesystem onto is
        // said out loud in the build's own error instead of here, where
        // an `info!` line scrolls past above it: see
        // [`Vm::blank_disk_hint`].
    }

    /// Where a build records that it created the data disk and has not
    /// yet seen a guest use it. lima's boot script is the only thing that
    /// puts a filesystem on that disk and only the creating build lets it,
    /// so a disk left behind by a build that died before the guest came
    /// up can never be formatted by a later one: the seed waits its two
    /// minutes for a mount that will not come and the build fails eight
    /// minutes later pointing at ssh. The marker is what makes that
    /// recoverable, and it is a file of ssf's rather than a question put
    /// to lima because lima has nothing to say about a disk's contents.
    fn unproven_disk(&self) -> PathBuf {
        self.dir.join("disk-unproven")
    }

    fn mark_disk_unproven(&self) {
        let path = self.unproven_disk();
        if let Err(e) = std::fs::write(
            &path,
            format!(
                "This build created the lima disk {}, and ssf never saw a guest come\nup on it. `ssf vm build --force` deletes that disk, and whatever is on it,\nand makes a fresh one while this file is here; ssf removes this file as soon\nas a guest has answered on the disk, and from then on no build deletes it.\n",
                self.lima_disk_name()
            ),
        ) {
            // Not fatal: without it a `--force` build simply keeps the
            // disk, which is what every build did before this marker.
            warn!("could not write {}: {e:#}", path.display());
        }
    }

    /// The build's own error, with what to do about a disk this build
    /// created and no guest ever used.
    fn blank_disk_hint(&self, e: anyhow::Error) -> anyhow::Error {
        if !self.unproven_disk().exists() {
            return e;
        }
        anyhow::anyhow!(
            "{e:#}\n\nthis build created the data disk {disk} and never saw a guest come up on it. ssf removes this mark the moment one does, so as far as ssf knows nothing has been put on that disk -- but it cannot see inside it, and a build interrupted after lima's boot script ran may have left a filesystem and a seeded factory there. No later build, `ssf vm start` or `ssf vm reset` will format it (only the build that creates a disk lets lima do that), so a disk that never got a filesystem stays unusable. `ssf vm build --force` deletes {disk} and everything on it and makes a fresh one; it deletes no disk a build ever saw a guest use.",
            disk = self.lima_disk_name()
        )
    }

    /// `limactl start`, bounded by the allowance lima is given for the
    /// same work plus a margin (see [`OWN_TIMEOUT_MARGIN`]): lima times
    /// the boot out first and says so; ssf's bound is only for a
    /// `limactl` that never returns at all.
    fn limactl_start(&self, args: &[&str]) -> Result<()> {
        self.limactl_run_within(args, PROVISION_TIMEOUT + OWN_TIMEOUT_MARGIN)
    }

    /// Put `format: false` back where a build that did not reach the end
    /// left `format: true`: the template ssf writes (what `ssf vm reset`
    /// creates the next instance from) and, when one is given, the
    /// instance's own copy of it (the only copy lima reads at boot).
    /// Only when the data disk exists -- with no disk there is nothing to
    /// protect, and the build that makes it is the one build that may
    /// hand lima a `format: true`. See [`plan_repair`] for the rules.
    ///
    /// Returns the copy that still says `format: true` afterwards --
    /// lima's, on a running instance (which `limactl edit` refuses) or
    /// after an edit that did not take, and ssf's own when that could not
    /// be rewritten. A caller that is about to boot the instance must not
    /// go on when it does: that boot is exactly what would let lima
    /// reformat a disk the factory is already living on.
    ///
    /// Every probe here is read fail-closed, because on this path the
    /// unknown answer is the dangerous one: "I could not find out" and
    /// "there is nothing to protect" look the same to a caller, and only
    /// one of them is safe to boot on. So a `limactl disk list` that
    /// failed counts as "the disk is there", a template that cannot be
    /// read counts as "it still says `format: true`", a rewrite that
    /// failed leaves that file unrepaired, and an edit lima reported as
    /// successful is believed only after the file it edited has been read
    /// back. The cost of being wrong is a refused boot and a message; the
    /// cost of the other way round is the factory's disk.
    #[must_use = "a stale `format: true` must stop the boot, not be dropped"]
    fn repair_stale_format(&self, inst: Option<&Instance>, why: Why) -> Option<PathBuf> {
        let path = self.template_path();
        let instance_yaml = inst.map(|i| Path::new(&i.dir).join("lima.yaml"));
        let stale = |p: &Path| match std::fs::read_to_string(p) {
            Ok(t) => says_format_true(&t),
            // A file that is not there tells lima nothing, so it is not a
            // failed probe: `ssf vm reset` repairs a template ssf has not
            // written yet, and an instance without its own copy does not
            // boot at all. Any other error is a probe that did not run.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                warn!(
                    "could not read {} ({e}); treating it as still saying `format: true`",
                    p.display()
                );
                true
            }
        };
        let disk_exists = match self.lima_disk() {
            Ok(d) => d.is_some(),
            Err(e) => {
                warn!(
                    "could not ask lima about the data disk {} ({e:#}); treating it as present, so a stale `format: true` still stops the boot",
                    self.lima_disk_name()
                );
                true
            }
        };
        let plan = plan_repair(
            disk_exists,
            stale(&path),
            instance_yaml.as_deref().is_some_and(stale),
            inst.is_some_and(|i| i.is_running()),
        );
        if plan == Repair::default() {
            return None;
        }
        let which = if plan.template {
            path.display().to_string()
        } else {
            format!("lima's copy of {}", path.display())
        };
        let note = format_off_note(why, &self.lima_disk_name(), &which);
        match why {
            Why::FinishingBuild => info!("{note}"),
            Why::FoundStale => warn!("{note}"),
        }
        // What is still stale when this returns. ssf's own template is
        // the lesser of the two (lima boots from its own copy), so lima's
        // copy overwrites it below when both are unrepaired.
        let mut unrepaired = None;
        if plan.template
            && let Err(e) = self.write_template(false)
        {
            warn!(
                "could not rewrite {}: {e:#}; that file is what `ssf vm reset` creates the next instance from, so it is not left behind quietly",
                path.display()
            );
            unrepaired = Some(path.clone());
        }
        let name = self.lima_name();
        if plan.blocked {
            warn!(
                "lima's copy of the template for {name} still says `format: true` and {name} is running, which `limactl edit` refuses; stop it (`ssf vm stop`) and ssf repairs it at the next `ssf vm build` or `ssf vm start`"
            );
            unrepaired = instance_yaml;
        } else if plan.instance {
            match self.stop_formatting_data_disk() {
                Err(e) => {
                    warn!(
                        "could not turn `format` off for {name}'s data disk ({e:#}); `limactl edit {name} --set '{FORMAT_OFF}'` does it with the instance stopped, and until then a boot that cannot find the disk's label would reformat it"
                    );
                    unrepaired = instance_yaml;
                }
                // The edit is believed only once the file says so. An
                // exit status of 0 is limactl's word that its own `--set`
                // ran, not that `.additionalDisks[0].format` is now
                // false: a differently shaped `additionalDisks`, a schema
                // that moved the key, a future lima whose restricted yq
                // silently matches nothing -- each of them would leave
                // ssf reporting a repair it did not make, and the boot
                // that followed is the one this whole path exists to stop.
                Ok(()) if instance_yaml.as_deref().is_some_and(stale) => {
                    warn!(
                        "`limactl edit {name} --set '{FORMAT_OFF}'` reported success, but lima's copy of the template still says `format: true`; ssf does not boot {name} over that -- put `format: false` in it by hand (the `additionalDisks` entry for {}) before starting the VM again",
                        self.lima_disk_name()
                    );
                    unrepaired = instance_yaml;
                }
                Ok(()) => {}
            }
        }
        unrepaired
    }

    /// What to tell a person whose boot has just been refused because a
    /// copy of the template would let lima reformat the data disk.
    /// `yaml` is the copy [`Vm::repair_stale_format`] could not put
    /// right: lima's own (the one it reads at boot) or, when the rewrite
    /// failed, ssf's.
    fn stale_format_error(&self, yaml: &Path) -> anyhow::Error {
        let name = self.lima_name();
        let head = format!(
            "{} still lets lima format the data disk {}, and that disk holds the factory's state; refusing to boot {name}.",
            yaml.display(),
            self.lima_disk_name(),
        );
        if yaml == self.template_path() {
            // ssf's own template: nothing here is lima's to fix, and the
            // warning above says why the rewrite failed.
            return anyhow::anyhow!(
                "{head} ssf could not rewrite that file (the warning above says why); make it writable, or put `format: false` in its `additionalDisks` entry by hand, and run this again"
            );
        }
        anyhow::anyhow!(
            "{head} Stop the instance (`ssf vm stop`, or `limactl stop {name}`) and run this again -- ssf turns the flag off while the instance is stopped -- or do it by hand with `limactl edit {name} --set '{FORMAT_OFF}'`"
        )
    }

    /// The same fact where nothing is being booted: the cleanup after a
    /// failed build, and the look `ssf vm start` takes once the guest is
    /// already up. "Refusing to boot" would be wrong in both -- no boot
    /// is being refused here; what matters is that the next one will be.
    fn stale_format_note(&self, yaml: &Path) -> String {
        let name = self.lima_name();
        format!(
            "{} still lets lima format the data disk {}, and ssf could not turn that off; the next boot of {name} is refused until it is. Stop the instance (`ssf vm stop`, or `limactl stop {name}`) and run `ssf vm start` again -- ssf turns the flag off while the instance is stopped -- or do it by hand with `limactl edit {name} --set '{FORMAT_OFF}'`",
            yaml.display(),
            self.lima_disk_name(),
        )
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
    /// The instance must be stopped: `limactl edit` refuses a running one
    /// ("cannot edit a running instance"), so callers go through
    /// [`Vm::repair_stale_format`], which knows the instance's state,
    /// rather than calling this on an instance they have not checked.
    fn stop_formatting_data_disk(&self) -> Result<()> {
        let name = self.lima_name();
        self.limactl_run(&["edit", &name, "--set", FORMAT_OFF])
    }

    /// `limactl create` from the template written by a build.
    fn lima_create(&self) -> Result<()> {
        let template = self.template_path();
        if !template.exists() {
            bail!("{} does not exist; run `ssf vm build`", template.display());
        }
        let name = self.lima_name();
        info!("creating lima instance {name} from {}", template.display());
        self.limactl_run_within(
            &["create", "--name", &name, &template.to_string_lossy()],
            CREATE_LIMIT,
        )
    }

    /// Wait for `/etc/ssf-image-built` over `limactl shell`: present at
    /// once on a provisioned instance; on a first boot, until
    /// `provision.sh` has written it, or has ended without it (then the
    /// end of its log is the error).
    ///
    /// The loop keeps [`PROVISION_TIMEOUT`] itself, and this wraps the
    /// whole wait in a deadline as well: an `ssf vm build` was once found
    /// parked at an `.await` for fifteen minutes -- past `limactl start`,
    /// no child process, no output, no CPU -- and a limit that lives
    /// inside a loop cannot end a loop that is no longer running. The
    /// deadline starts here, after `limactl start` has returned, so the
    /// minutes the guest spends provisioning inside that call are not
    /// counted twice.
    async fn wait_for_provisioning(&self) -> Result<()> {
        let name = self.lima_name();
        let backstop = PROVISION_TIMEOUT + OWN_TIMEOUT_MARGIN;
        match tokio::time::timeout(backstop, self.provisioning_loop()).await {
            Ok(r) => r,
            Err(_) => bail!(
                "waiting for {name} to provision itself stopped making progress after {}; `limactl shell {name} sudo tail {PROVISION_LOG}` shows where it got to, and `ssf vm console` has its console",
                human_duration(backstop)
            ),
        }
    }

    async fn provisioning_loop(&self) -> Result<()> {
        let name = self.lima_name();
        let probe = provision_probe();
        let deadline = Instant::now() + PROVISION_TIMEOUT;
        let mut idle = 0;
        loop {
            // A probe that cannot run at all is usually transient: ssh in
            // the guest is not up yet, or one `limactl shell` timed out.
            // But `limactl shell` against a stopped or deleted instance
            // ("instance %q is stopped, run `limactl start %s`") is the
            // end, and reading that as "nothing seen" is what parked a
            // wait for half an hour in silence. So a failed probe asks
            // lima what the instance is doing, and only a definite answer
            // other than Running ends the wait.
            let out = match self
                .limactl_output_within(&["shell", &name, "sh", "-c", &probe], PROBE_LIMIT)
            {
                Ok(out) => out,
                Err(e) => {
                    match self.lima_instance() {
                        Ok(state) => {
                            if let Some(why) =
                                terminal_state(state.as_ref().map(|i| i.status.as_str()))
                            {
                                bail!(
                                    "{name} {why} while waiting for it to provision itself ({e:#}); `ssf vm console` has its console, and `limactl shell {name} sudo tail {PROVISION_LOG}` its log once it runs again"
                                );
                            }
                        }
                        // Two failures in a row say nothing definite:
                        // keep waiting, but not silently.
                        Err(list) => warn!(
                            "the provisioning probe did not run ({e:#}) and asking lima about {name} failed too ({list:#}); still waiting"
                        ),
                    }
                    debug!(error = format!("{e:#}"), "provisioning probe did not run");
                    String::new()
                }
            };
            let probed = parse_probe(&out);
            debug!(?probed, idle, "provisioning probe");
            match provision_step(probed, idle) {
                Step::Provisioned => return Ok(()),
                Step::Failed => {
                    let tail = self
                        .limactl_output_within(
                            &["shell", &name, "sudo", "tail", "-50", PROVISION_LOG],
                            PROBE_LIMIT,
                        )
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
        let Some(inst) = self.lima_instance()? else {
            bail!("lima instance {name} does not exist; run `ssf vm build`");
        };
        // The boot below is what a `format: true` left by a build that
        // did not finish would reach, and by now the data disk holds the
        // factory. This is also the one point where the flag can still be
        // put right: the instance is stopped here, which is the only
        // state `limactl edit` accepts. A flag that survives the repair
        // (an instance already running) stops the start rather than
        // booting into it.
        if let Some(yaml) = self.repair_stale_format(Some(&inst), Why::FoundStale) {
            return Err(self.stale_format_error(&yaml));
        }
        self.ensure_key()?;
        self.write_share(host)?;
        self.apply_sizes()?;
        self.limactl_start(&["start", "--timeout", &start_timeout_arg(), &name])?;
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
        if let Err(e) = self.wait_for_ssh(SEED_TIMEOUT).await {
            bail!(
                "{e:#}; the console is in {}",
                self.lima_console_log()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "lima's instance directory".into())
            );
        }
        // The guest has provisioned and answered as `ssf`: the data disk
        // is the factory's. The repair above ran before this boot, which
        // is where it can work; this is the same point `ssf vm build`
        // checks, and it is here so that a flag that came back (an
        // instance edited by hand, a template restored from elsewhere)
        // is said out loud rather than waiting for the next boot to be
        // discovered.
        if let Some(yaml) = self.repair_stale_format(
            self.lima_instance().ok().flatten().as_ref(),
            Why::FoundStale,
        ) {
            warn!("{}", self.stale_format_note(&yaml));
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
        // "Not running" has to come from lima, not from a probe that
        // failed: `.ok().flatten()` printed "VM is not running" over a
        // running VM whenever `limactl list` had a bad moment, and the
        // VM was then left up.
        match self
            .lima_instance()
            .with_context(|| format!("asking lima whether {name} is running"))?
        {
            None => {
                println!("there is no lima instance {name}");
                return Ok(());
            }
            Some(inst) if !inst.is_running() => {
                println!(
                    "VM {} is not running{}",
                    self.cfg.name,
                    if inst.status.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", inst.status)
                    }
                );
                return Ok(());
            }
            Some(_) => {}
        }
        if let Err(e) = self.limactl_run_within(&["stop", &name], STOP_LIMIT) {
            warn!("{e:#}; forcing it");
            self.limactl_run_within(&["stop", "-f", &name], STOP_LIMIT)?;
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
        // The template is what the new instance inherits, so a stale
        // `format: true` in it has to go before the create, not after:
        // the instance that is about to be deleted is not worth fixing,
        // which is why no instance is passed. What can still come back is
        // ssf's own template, when the rewrite of it failed -- and
        // creating an instance from a template that says `format: true`
        // is exactly the boot this whole path exists to stop, over a disk
        // that by now holds the factory. So the reset refuses, as every
        // other caller does.
        if let Some(yaml) = self.repair_stale_format(None, Why::FoundStale) {
            return Err(self.stale_format_error(&yaml));
        }
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
    /// after this). `Ok(false)`: lima had neither.
    ///
    /// A `limactl` that will not answer fails the step only when lima's
    /// home still holds something: there is then a VM here that ssf
    /// cannot delete, and saying so is the point. With nothing there,
    /// the same failure is just a tool that is not around any more, and
    /// `ssf uninstall` carries on.
    pub(super) fn lima_destroy(&self) -> Result<bool> {
        let mut removed = false;
        let name = self.lima_name();
        let instance = match self.lima_instance() {
            Ok(i) => i,
            Err(e) => match self.lima_leftovers() {
                Some(false) => {
                    warn!("could not ask lima about {name} ({e:#}); its home holds nothing of it");
                    return Ok(false);
                }
                Some(true) => {
                    return Err(e).with_context(|| {
                        format!(
                            "lima's home still holds something of {name}, and lima cannot be asked about it"
                        )
                    });
                }
                None => {
                    return Err(e).with_context(|| {
                        format!(
                            "lima cannot be asked about {name}, and there is no home directory to find lima's own in instead"
                        )
                    });
                }
            },
        };
        if instance.is_some() {
            self.limactl_run(&["delete", "-f", &name])?;
            println!("deleted lima instance {name}");
            removed = true;
        }
        let disk = self.lima_disk_name();
        // The same rule as the instance above: a `limactl` that will not
        // answer is not the last word when lima's own filesystem can be
        // looked at. The survey already trusts `<lima home>/_disks/` for
        // the decision that *permits* the destroying, so refusing to
        // trust it for the one that declares the destroying finished
        // failed the step over a disk directory demonstrably not there.
        let held = match self.lima_disk() {
            Ok(d) => d.is_some(),
            Err(e) => match self.lima_disk_dir().map(|p| p.exists()) {
                Some(false) => {
                    warn!(
                        "could not ask lima about disk {disk} ({e:#}); its home holds no such disk"
                    );
                    false
                }
                _ => {
                    return Err(e).with_context(|| {
                        format!("lima cannot be asked about disk {disk}, and its home may hold it")
                    });
                }
            },
        };
        if held {
            self.limactl_run(&["disk", "delete", &disk])?;
            println!("deleted lima disk {disk}");
            removed = true;
        }
        let _ = std::fs::remove_file(self.unproven_disk());
        Ok(removed)
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
    fn the_lima_version_is_read_out_of_what_limactl_prints() {
        // What a release prints.
        assert_eq!(
            parse_lima_version("limactl version 2.2.0\n"),
            Some(LimaVersion(2, 2, 0))
        );
        // A `v` prefix, and a build from git: newer than the release it
        // names, so the suffix is dropped rather than read as semver's
        // pre-release, which would make it older.
        assert_eq!(
            parse_lima_version("limactl version v2.2.0-15-g1234567"),
            Some(LimaVersion(2, 2, 0))
        );
        assert_eq!(
            parse_lima_version("limactl version 2.3.0-beta.0"),
            Some(LimaVersion(2, 3, 0))
        );
        // Two components are a version; a lima with none is not read as
        // one, so that preflight lets it through instead of refusing it.
        assert_eq!(
            parse_lima_version("limactl version 2.1"),
            Some(LimaVersion(2, 1, 0))
        );
        assert_eq!(parse_lima_version("limactl version <unknown>"), None);
        assert_eq!(parse_lima_version("limactl version HEAD"), None);
        assert_eq!(parse_lima_version(""), None);
    }

    /// What `lima_preflight` says with a `limactl` that prints
    /// `line` for `--version` and answers nothing else.
    fn preflight_against(line: &str) -> Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "ssf-lima-ver-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let limactl = dir.join("limactl");
        std::fs::write(&limactl, format!("#!/bin/sh\nprintf '%s\\n' '{line}'\n")).unwrap();
        make_executable(&limactl).unwrap();
        let mut cfg = Config::default();
        cfg.vm.dir = dir.join("vm").to_string_lossy().into_owned();
        cfg.vm.name = "one".into();
        cfg.vm.backend = Some(BackendKind::Lima);
        cfg.vm.limactl = Some(limactl.to_string_lossy().into_owned());
        let out = Vm::new(&cfg).lima_preflight();
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn a_lima_under_the_floor_is_refused_with_its_version_named() {
        // The version limactl was asked for anyway is read: an older
        // lima used to get all the way to a first boot and fail there,
        // over a base image it could not resolve or a share that was
        // mounted too late, saying neither.
        let e = format!(
            "{:#}",
            preflight_against("limactl version 1.2.1").unwrap_err()
        );
        assert!(e.contains("lima 1.2.1"), "{e}");
        assert!(e.contains("2.0.1 or newer"), "{e}");
        assert!(e.contains("Upgrade lima"), "{e}");
        // The version gate is passed on a new enough lima, and on one
        // whose version cannot be read at all -- a lima built without it
        // stamped in prints `<unknown>`, and refusing that would be
        // refusing a lima that was never asked about. Whatever preflight
        // goes on to say about qemu on this machine is not this gate's.
        for line in ["limactl version 2.2.0", "limactl version <unknown>"] {
            let after = preflight_against(line).err().map(|e| format!("{e:#}"));
            assert!(
                !after.as_deref().unwrap_or_default().contains("or newer"),
                "{after:?}"
            );
        }
    }

    #[test]
    fn the_floor_is_the_oldest_lima_that_resolves_the_templates_base() {
        // 2.0.1, not 2.0.0: the opaque locator the template writes is
        // how lima 2.0 spells one, and 2.0.0's release tarball ships
        // `templates/_images/` empty. Held here so the constant and the
        // reason cannot drift apart.
        // The versions the floor was settled against by running them:
        // 1.2.1 and 2.0.0 fail `limactl template validate` on the
        // template ssf renders, 2.0.1 and 2.2.0 pass it.
        assert_eq!(MIN_LIMA.to_string(), "2.0.1");
        assert!(LimaVersion(1, 2, 1) < MIN_LIMA);
        assert!(LimaVersion(2, 0, 0) < MIN_LIMA);
        assert!(LimaVersion(2, 0, 1) >= MIN_LIMA);
        assert!(LimaVersion(2, 2, 0) >= MIN_LIMA);
        // Both bases are named in the opaque form the floor is chosen
        // for -- `template://...`, which every 1.x takes, would mean a
        // different floor.
        for arch in ["x86_64", "aarch64"] {
            let base = base_template(arch);
            assert!(
                base.starts_with("template:_images/"),
                "{base}: the base locator moved; MIN_LIMA is chosen for it"
            );
        }
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
        // No `mountType` field: the type is lima's to pick (see
        // `render_template`). The word itself does occur, in the boot
        // hook's failure message, so this looks for the key rather than
        // the string.
        assert!(
            !y.lines().any(|l| l.trim_start().starts_with("mountType")),
            "{y}"
        );
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
        assert!(y.contains("      exec bash \"$boot\"\n"), "{y}");
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
    fn a_command_that_does_not_return_is_killed_at_its_limit() {
        // The reason every limactl call is bounded: an `ssf vm build`
        // once sat for a quarter of an hour with nothing to show and no
        // child process to look at. A command that sleeps stands in for
        // the limactl that never returned.
        let mut child = Command::new("sh")
            .args(["-c", "sleep 60"])
            .stdin(Stdio::null())
            .spawn()
            .expect("spawning sh");
        let started = Instant::now();
        let err = wait_within(
            &mut child,
            "limactl start ssf-one",
            Duration::from_millis(300),
        )
        .unwrap_err()
        .to_string();
        // It gave up at the limit rather than waiting for the sleep...
        assert!(started.elapsed() < Duration::from_secs(10), "{err}");
        // ...the message says which command and how long it was given...
        assert!(err.contains("`limactl start ssf-one`"), "{err}");
        assert!(err.contains("300 ms"), "{err}");
        // ...and the child is gone, not left behind still running.
        assert!(child.try_wait().unwrap().is_some(), "the child outlived it");

        // A command that does return does so with its own status, and the
        // limit is not waited out.
        let mut quick = Command::new("sh")
            .args(["-c", "exit 3"])
            .stdin(Stdio::null())
            .spawn()
            .expect("spawning sh");
        let started = Instant::now();
        let st = wait_within(&mut quick, "limactl list --json", Duration::from_secs(60)).unwrap();
        assert_eq!(st.code(), Some(3));
        assert!(started.elapsed() < Duration::from_secs(10));

        assert_eq!(human_duration(Duration::from_millis(300)), "300 ms");
        assert_eq!(human_duration(Duration::from_secs(1)), "1 second");
        assert_eq!(human_duration(PROBE_LIMIT), "1 minute");
        assert_eq!(human_duration(QUICK_LIMIT), "2 minutes");
        assert_eq!(
            human_duration(Duration::from_secs(90)),
            "1 minute 30 seconds"
        );
    }

    #[test]
    fn every_limit_leaves_room_for_the_slow_commands() {
        // ssf's bound around `limactl start` is lima's own `--timeout`
        // plus a margin, so that lima times the boot out first and its
        // message is what a person reads; ssf's is the backstop for a
        // limactl that does not return at all.
        assert_eq!(start_timeout_arg(), "30m");
        assert_eq!(PROVISION_TIMEOUT, Duration::from_secs(30 * 60));
        // The guest provisions inside `limactl start`, so lima is given
        // the whole provisioning allowance and ssf's bound around the
        // call is that plus the margin -- neither can cut a slow but
        // healthy provisioning short.
        assert!(PROVISION_TIMEOUT + OWN_TIMEOUT_MARGIN > PROVISION_TIMEOUT);
        // A `limactl create` may download the base image; a probe over
        // `limactl shell` is a one-liner and must not hold up the loop.
        assert!(CREATE_LIMIT > QUICK_LIMIT);
        assert!(PROBE_LIMIT < QUICK_LIMIT);
        assert!(STOP_LIMIT > QUICK_LIMIT);
        // The gate's liveness question is the shortest of all: it is
        // asked in front of every forwarded command, so its bound is
        // what a person waits out when limactl has stopped answering,
        // and being cut short only costs the gate an answer it is
        // willing to do without. The supervisor's polling of the same
        // question keeps the listing's own bound, since it gives up
        // after MAX_UNANSWERED_PROBES rounds with no answer.
        assert!(LIVENESS_LIMIT < QUICK_LIMIT);
        // The survey asks two of these in a row with a person waiting on
        // the report, and has a filesystem fallback when neither
        // answers.
        assert!(SURVEY_LIMIT < QUICK_LIMIT);
    }

    #[test]
    fn the_templates_boot_hook_waits_for_the_share_itself() {
        // The hook is the only part of the boot that is not in the share,
        // so it is the only part that can report the share missing. If it
        // went back to a bare `exec`, a guest that never got its mount
        // would write no log at all and the host would wait out
        // PROVISION_TIMEOUT with nothing to show.
        let hook = boot_hook();
        assert!(hook.starts_with("#!/bin/bash\n"), "{hook}");
        assert!(hook.contains(&format!("log={PROVISION_LOG}")), "{hook}");
        assert!(
            hook.contains(&format!("boot={GUEST_MOUNT}/guest/lima-boot.sh")),
            "{hook}"
        );
        assert!(hook.contains(&format!("i < {SHARE_WAIT_SECS}")), "{hook}");
        // And what it says when the wait runs out names the one thing the
        // version floor cannot settle: a `mountType` in lima's own
        // `_config` that mounts the share only once the guest is up.
        assert!(hook.contains("mountType"), "{hook}");
        assert!(hook.contains("_config/override.yaml"), "{hook}");
        // lima's home is not always `~/.lima`, and the message is read
        // by someone looking for a file.
        assert!(hook.contains("$LIMA_HOME"), "{hook}");
        // The log is emptied before that wait, not after it and not in
        // lima-boot.sh: the host reads "a log and none of the guest
        // scripts running" as this attempt having died, and while the
        // hook waited for the share the log on disk was the previous
        // attempt's, so a probe landing there printed the wrong tail.
        let empties = hook
            .lines()
            .position(|l| l.trim() == ": > \"$log\"")
            .unwrap_or_else(|| panic!("{hook}"));
        let waits = hook
            .lines()
            .position(|l| l.contains(&format!("i < {SHARE_WAIT_SECS}")))
            .expect("the wait");
        assert!(empties < waits, "{hook}");
        assert!(
            !std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("vm/guest/lima-boot.sh")
            )
            .expect("the guest script")
            .contains(": > \"$log\""),
            "lima-boot.sh must not truncate the log as well: it runs after the wait"
        );
        // The failure goes to the log the host watches, not only to
        // lima's output, and it is a failure (`exit 1`), not a fall
        // through into an `exec` that cannot work.
        let fail = hook
            .lines()
            .find(|l| l.contains("FAILED"))
            .unwrap_or_else(|| panic!("{hook}"));
        assert!(fail.contains("tee -a \"$log\""), "{fail}");
        assert!(hook.contains("    exit 1\n"), "{hook}");
        assert!(hook.trim_end().ends_with("exec bash \"$boot\""), "{hook}");
        // It has to be shell that runs: the guest gets it as written.
        let out = std::process::Command::new("bash")
            .args(["-n", "-c", &hook])
            .output()
            .expect("bash runs");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // And the template indents every line of it under `script: |`.
        let y = vm().lima_template(true).unwrap();
        for line in hook.lines().filter(|l| !l.is_empty()) {
            assert!(y.contains(&format!("\n      {line}\n")), "{line} in {y}");
        }
    }

    #[test]
    fn the_ssh_wait_outlasts_the_seeds_own_waits() {
        // The marker the provisioning wait ends on is written before
        // `ssf-seed.service` is queued, so what the ssh wait after it is
        // really waiting for is the seed -- which waits for the share and
        // for the data disk before it writes authorized_keys. Read those
        // waits out of the script itself, so that lengthening one of them
        // and leaving this alone fails here rather than in the field with
        // "provisioned, but the guest does not answer as ssf".
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("vm/guest/seed-lima.sh"),
        )
        .expect("vm/guest/seed-lima.sh");
        let waits: u64 = script
            .lines()
            .filter_map(|l| l.trim().strip_prefix("wait_for "))
            .filter_map(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
            .sum();
        assert!(waits >= 240, "the seed's waits are {waits}s: {script}");
        assert!(
            SEED_TIMEOUT > Duration::from_secs(waits),
            "SEED_TIMEOUT is {}, the seed may wait {waits}s before it writes authorized_keys",
            human_duration(SEED_TIMEOUT)
        );
        // With room left for the home copy that follows them.
        assert!(SEED_TIMEOUT - Duration::from_secs(waits) >= Duration::from_secs(120));
    }

    #[test]
    fn a_running_instance_is_never_edited_in_place() {
        // `limactl edit` refuses a running instance (lima 2.2.0: "cannot
        // edit a running instance"), so a repair that ran it anyway got
        // an error, warned, and left the instance booting with
        // `format: true` over a disk that by then held the factory. The
        // rule is: ssf's own template is always rewritten, the instance's
        // copy only while it is stopped, and a running one is reported.
        assert_eq!(
            plan_repair(true, true, true, true),
            Repair {
                template: true,
                instance: false,
                blocked: true
            }
        );
        assert_eq!(
            plan_repair(true, true, true, false),
            Repair {
                template: true,
                instance: true,
                blocked: false
            }
        );
        // Only lima's copy stale (a build rewrote ssf's own and then died).
        assert_eq!(
            plan_repair(true, false, true, false),
            Repair {
                template: false,
                instance: true,
                blocked: false
            }
        );
        // Only ssf's own stale: nothing to edit, and nothing blocked.
        assert_eq!(
            plan_repair(true, true, false, true),
            Repair {
                template: true,
                instance: false,
                blocked: false
            }
        );
        // Nothing stale, and -- whatever the templates say -- no disk
        // means nothing to protect: the build that makes the disk is the
        // one build that may hand lima `format: true`.
        assert_eq!(plan_repair(true, false, false, false), Repair::default());
        assert_eq!(plan_repair(false, true, true, false), Repair::default());
        assert_eq!(plan_repair(false, true, true, true), Repair::default());
    }

    #[test]
    fn the_repair_edits_a_stopped_instance_and_reports_a_running_one() {
        // The same rules through `limactl` itself: a fake one records
        // what it was asked to do.
        for (status, edited) in [("Stopped", true), ("Running", false)] {
            let t = Fake::new(status);
            let stale = t.vm.repair_stale_format(Some(&t.instance), Why::FoundStale);
            // ssf's own template is a file: it is put right either way.
            assert!(
                !says_format_true(&std::fs::read_to_string(t.vm.template_path()).unwrap()),
                "{status}"
            );
            assert_eq!(
                t.ran("edit"),
                edited,
                "{status}: limactl ran {:?}",
                t.commands()
            );
            if edited {
                assert!(
                    t.commands()
                        .iter()
                        .any(|c| c == &format!("edit ssf-one --set {FORMAT_OFF}")),
                    "{:?}",
                    t.commands()
                );
                // Repaired: nothing for the caller to refuse over.
                assert_eq!(stale, None);
            } else {
                // Not repaired, and said so: lima's copy of the template
                // comes back, and every caller that is about to boot the
                // instance stops on it.
                assert_eq!(stale, Some(t.instance_yaml()));
                let err = t.vm.stale_format_error(&stale.unwrap()).to_string();
                assert!(err.contains("refusing to boot ssf-one"), "{err}");
                assert!(err.contains("ssf vm stop"), "{err}");
                assert!(err.contains(FORMAT_OFF), "{err}");
            }
        }
    }

    #[tokio::test]
    async fn a_start_repairs_the_flag_before_the_boot_and_refuses_what_it_cannot_repair() {
        // The start writes the share tree, which seeds the guest from the
        // bot token and so resolves the config directory (#140).
        let _sandbox = crate::config::test_support::sandbox();
        // `ssf vm start` is the boot that a `format: true` left by a
        // failed build would reach, and by then the data disk holds the
        // factory. A stopped instance is repaired in place and the start
        // goes on (as far as this fake takes it); a running one cannot be
        // edited, and the start stops rather than leaving lima free to
        // reformat the disk.
        let t = Fake::new("Running");
        let err =
            t.vm.lima_start(&Config::default())
                .await
                .expect_err("a start over a stale format flag must not go ahead")
                .to_string();
        assert!(
            err.contains("still lets lima format the data disk"),
            "{err}"
        );
        assert!(err.contains("refusing to boot ssf-one"), "{err}");
        // It stopped before it did anything to the instance.
        assert!(!t.ran("start") && !t.ran("edit"), "{:?}", t.commands());

        // Stopped: the flag is turned off first (`limactl edit` takes a
        // stopped instance) and the start goes on to boot it. This fake's
        // instance never comes up, which is the other fix in the same
        // path: a probe that cannot run against an instance lima says is
        // stopped ends the wait then and there, instead of leaving it
        // parked for PROVISION_TIMEOUT with nothing on screen.
        let t = Fake::new("Stopped");
        let started = Instant::now();
        let err =
            t.vm.lima_start(&Config::default())
                .await
                .expect_err("this instance never provisions itself")
                .to_string();
        assert!(t.ran("edit"), "{:?}", t.commands());
        assert!(
            t.commands()
                .iter()
                .any(|c| c == &format!("edit ssf-one --set {FORMAT_OFF}")),
            "{:?}",
            t.commands()
        );
        assert!(err.contains("is stopped, not running"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the wait sat for {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn an_edit_limactl_called_a_success_is_read_back_before_it_is_believed() {
        // `limactl edit --set` exiting 0 is limactl's word that its own
        // `--set` ran, not that the flag is off: a lima whose restricted
        // yq matched nothing (a schema that moved the key, a differently
        // shaped `additionalDisks`) would exit 0 over an unchanged file,
        // and the whole "no boot over a flag that is still there"
        // guarantee would rest on that exit status alone.
        let t = Fake::with("Stopped", Edit::Ignored, DiskList::Answers);
        let stale = t.vm.repair_stale_format(Some(&t.instance), Why::FoundStale);
        assert!(t.ran("edit"), "{:?}", t.commands());
        assert!(
            says_format_true(&std::fs::read_to_string(t.instance_yaml()).unwrap()),
            "the fake was supposed to leave the file alone"
        );
        assert_eq!(stale, Some(t.instance_yaml()));
        let err = t.vm.stale_format_error(&stale.unwrap()).to_string();
        assert!(err.contains("refusing to boot ssf-one"), "{err}");
    }

    #[tokio::test]
    async fn a_start_refuses_when_the_edit_did_not_take() {
        // The same thing from the caller's end: nothing is booted over a
        // repair that only limactl's exit status said had happened.
        let t = Fake::with("Stopped", Edit::Ignored, DiskList::Answers);
        let err =
            t.vm.lima_start(&Config::default())
                .await
                .expect_err("an edit that changed nothing is not a repair")
                .to_string();
        assert!(err.contains("refusing to boot ssf-one"), "{err}");
        assert!(!t.ran("start"), "{:?}", t.commands());
    }

    #[test]
    fn a_disk_probe_that_failed_is_not_an_answer() {
        // `limactl disk list` erroring used to be read as "there is no
        // data disk", which is the one answer that makes a stale
        // `format: true` harmless -- so a lima that could not be asked
        // let the boot through. The probe that did not run now counts as
        // "the disk is there": the cost is a refused boot and a message.
        let t = Fake::with("Running", Edit::Applies, DiskList::Fails);
        assert_eq!(
            t.vm.repair_stale_format(Some(&t.instance), Why::FoundStale),
            Some(t.instance_yaml())
        );
    }

    #[test]
    fn lima_answers_whether_there_is_a_vm_here_the_directory_does_not() {
        // `ssf uninstall` used to read "is there a VM" off `[vm]
        // dir`/<name>. Under Firecracker that directory *is* the VM;
        // under lima it holds the template, the ssh key and the share,
        // and the instance and the data disk are in lima's home. A
        // directory removed by hand, or a `[vm] dir` that was changed,
        // then read as "no VM" and left both behind.
        let t = Fake::new("Stopped");
        std::fs::remove_dir_all(&t.vm.dir).unwrap();
        assert!(!t.vm.dir.exists());
        assert_eq!(
            t.vm.survey(),
            Survey {
                present: Some(true),
                running: Some(false),
                startable: true,
                data: Some(true),
                strays: Vec::new(),
                base_unread: false,
            }
        );
        let t = Fake::new("Running");
        assert_eq!(t.vm.survey().running, Some(true));
    }

    #[test]
    fn a_data_disk_that_outlived_its_instance_is_a_vm_with_nothing_to_start() {
        // `limactl delete` of an instance leaves an external disk where
        // it is, so the disk holding the clones and worktrees can be all
        // that is left. It is worth not losing silently -- and nothing
        // can be started to mount it, so `ssf vm start` is no remedy.
        let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Answers, Listing::Empty);
        std::fs::remove_dir_all(&t.vm.dir).unwrap();
        assert_eq!(
            t.vm.survey(),
            Survey {
                present: Some(true),
                running: Some(false),
                startable: false,
                data: Some(true),
                strays: Vec::new(),
                base_unread: false,
            }
        );
    }

    #[test]
    fn nothing_in_lima_and_no_directory_is_no_vm() {
        let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Empty, Listing::Empty);
        std::fs::remove_dir_all(&t.vm.dir).unwrap();
        assert_eq!(t.vm.survey().present, Some(false));
        // The directory on its own is still ssf's to remove -- the
        // template, the ssh key and the share are in it -- but none of
        // that is anyone's work, so it is no reason to refuse.
        std::fs::create_dir_all(&t.vm.dir).unwrap();
        let s = t.vm.survey();
        assert_eq!((s.present, s.data), (Some(true), Some(false)));
    }

    #[test]
    fn an_instance_this_config_does_not_name_is_seen_but_never_counted_as_the_vm() {
        // A `[vm] name` changed in the config renames nothing in lima's
        // home, so the old instance keeps its old name and asking only
        // about the new one is how `ssf uninstall` came to say "no VM"
        // over a VM sitting right there. It is reported -- and it must
        // not reach `present`, which is the field that gates the destroy
        // step: a stray that got in there would be a VM deleted because
        // someone edited a name.
        let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Answers, Listing::Strays);
        std::fs::remove_dir_all(&t.vm.dir).unwrap();
        let s = t.vm.survey();
        assert_eq!(s.present, Some(true), "this config's own disk is there");
        assert_eq!(s.strays, vec![Stray::lima_instance("ssf-old".into())]);
        // And it changes none of the answers about *this* VM, which is
        // the whole point: `ssf-one` is still the stopped, startable
        // instance with the data disk, and `ssf-old` is a line in the
        // report.
        assert_eq!(s.running, Some(false));
        assert!(s.startable, "ssf-one, not ssf-old");
        assert_eq!(s.data, Some(true));
    }

    #[test]
    fn a_stray_alone_is_not_a_vm_to_destroy() {
        // Nothing of this configuration anywhere, and an `ssf-old` in
        // lima's home. `present` has to stay `Some(false)` -- there is
        // no VM for `ssf uninstall` to destroy -- while the stray is
        // still reported, which is what stops the report saying a bare
        // "no VM" over an instance lima is holding.
        let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Empty, Listing::Strays);
        std::fs::remove_dir_all(&t.vm.dir).unwrap();
        let mut cfg = Config::default();
        cfg.vm.dir = t.vm.base.to_string_lossy().into_owned();
        cfg.vm.name = "two".into();
        cfg.vm.backend = Some(BackendKind::Lima);
        cfg.vm.limactl = t.vm.cfg.limactl.clone();
        let mut other = Vm::new(&cfg);
        other.lima_home = t.vm.lima_home.clone();
        let s = other.survey();
        assert_eq!(s.present, Some(false), "no VM named ssf-two");
        assert_eq!(s.data, Some(false));
        assert_eq!(
            s.strays.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            ["ssf-old", "ssf-one"]
        );
        assert_eq!(
            s.strays[0].remove, "limactl delete ssf-old",
            "the command carries the name the person no longer has"
        );
    }

    #[test]
    fn a_stray_is_still_named_when_limactl_will_not_list_it() {
        // `limactl list` is the command that just failed, so it is the
        // one thing the person cannot use to find these themselves.
        let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Fails, Listing::Fails);
        let home = t.vm.lima_home.clone().unwrap();
        std::fs::create_dir_all(home.join("ssf-old")).unwrap();
        std::fs::create_dir_all(home.join("_disks").join("ssf-older")).unwrap();
        let named: Vec<_> = t.vm.survey().strays.into_iter().map(|s| s.remove).collect();
        assert_eq!(
            named,
            ["limactl delete ssf-old", "limactl disk delete ssf-older"]
        );
    }

    #[test]
    fn a_failed_disk_listing_does_not_hide_the_other_disks() {
        // The rule that lima's filesystem answers what `limactl` will
        // not, applied to the strays and not just to this VM's own disk.
        // Leaving it out hid an old data disk of clones in exactly the
        // case where `limactl disk list` is no use to the person either.
        let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Fails, Listing::Empty);
        std::fs::remove_dir_all(&t.vm.dir).unwrap();
        let home = t.vm.lima_home.clone().unwrap();
        std::fs::create_dir_all(home.join("_disks").join("ssf-old")).unwrap();
        let s = t.vm.survey();
        assert_eq!(
            s.strays
                .iter()
                .map(|x| x.remove.as_str())
                .collect::<Vec<_>>(),
            ["limactl disk delete ssf-old"]
        );
        // And it is still not this configuration's VM.
        assert_eq!(s.present, Some(false));
        assert_eq!(s.data, Some(false));
    }

    #[tokio::test]
    async fn vm_status_names_every_stray_once_when_limactl_will_not_list() {
        // Through `status()`, under lima, on the path where `limactl`
        // answers nothing and lima's own home is all there is: the two
        // halves of that fallback were once added twice, so every data
        // disk was reported twice and the person went looking for a
        // second one that was not there. `[vm] dir`'s own stray has to
        // be here too, or `ssf vm status` and `ssf uninstall` disagree
        // about the same machine.
        let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Fails, Listing::Fails);
        let home = t.vm.lima_home.clone().unwrap();
        std::fs::create_dir_all(home.join("ssf-old")).unwrap();
        std::fs::create_dir_all(home.join("_disks").join("ssf-old")).unwrap();
        let dir_stray = t.vm.base.join("older");
        std::fs::create_dir_all(&dir_stray).unwrap();
        std::fs::write(dir_stray.join("data.ext4"), b"disk").unwrap();
        let st = t.vm.status().await;
        let named: Vec<&str> = st.strays.iter().map(|s| s.remove.as_str()).collect();
        assert_eq!(named.len(), 3, "each exactly once: {named:?}");
        assert!(named.contains(&"limactl delete ssf-old"), "{named:?}");
        assert!(named.contains(&"limactl disk delete ssf-old"), "{named:?}");
        assert!(
            named
                .iter()
                .any(|c| c.starts_with("rm -rf ") && c.trim_end_matches('\'').ends_with("older")),
            "the [vm] dir stray is missing under lima: {named:?}"
        );
    }

    #[test]
    fn a_vm_directory_under_vm_dir_is_a_stray_under_lima_too() {
        // `[vm] dir` is shared by the backends. A VM built under
        // Firecracker and then switched to lima leaves its directory,
        // and its clones, right where they were -- and reading that as a
        // Firecracker question left `ssf uninstall` calling `[vm] dir`
        // "safe to remove" under lima, which is the same sentence this
        // change exists to stop, on the other backend.
        let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Empty, Listing::Empty);
        let old = t.vm.base.join("old");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("data.ext4"), b"disk").unwrap();
        let named: Vec<_> =
            t.vm.survey()
                .strays
                .into_iter()
                .map(|s| (s.kind, s.name))
                .collect();
        assert_eq!(named, [(StrayKind::Directory, "old".to_string())]);
        // And the filesystem-only path doctor falls back to sees it too.
        assert_eq!(t.vm.strays_on_filesystem().len(), 1);
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_lima_home_nobody_could_read_is_not_an_empty_one() {
        // The reason the two arrive together: a lima home left
        // root-owned by an earlier `sudo` is one of the things that
        // makes `limactl` fail in the first place. Read as empty, it
        // gave `no VM` over `safe to remove` about a home holding an
        // instance and a disk of clones.
        let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Fails, Listing::Fails);
        std::fs::remove_dir_all(&t.vm.dir).unwrap();
        let home = t.vm.lima_home.clone().unwrap();
        std::fs::create_dir_all(home.join("ssf-old")).unwrap();
        std::fs::create_dir_all(home.join("_disks").join("ssf-old")).unwrap();
        assert_eq!(t.vm.survey().strays.len(), 2, "readable while readable");
        set_mode(&home, 0o000);
        let s = t.vm.survey();
        set_mode(&home, 0o755);
        // Running as root reads it anyway, and then there is nothing to
        // assert about not having been able to.
        if s.strays.is_empty() {
            assert!(s.base_unread, "unreadable is unknown, not empty");
            assert_ne!(
                s.present,
                Some(false),
                "`no VM` over a home nobody looked in"
            );
        }
    }

    #[test]
    fn a_vm_name_that_escapes_vm_dir_reports_no_strays() {
        // `ssf vm destroy` removes `[vm] dir`/<name>. The siblings this
        // reports are only safe from that while it is strictly inside
        // `[vm] dir`: `..` makes it the parent, and `remove_dir_all`
        // there takes `[vm] dir` and everything this had just called
        // "untouched". `Path::parent` is not the test -- it reads a
        // trailing `..` as a component, so `<base>/..` has parent
        // `<base>` and passes.
        let base = std::env::temp_dir().join(format!(
            "ssf-escape-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(base.join("old")).unwrap();
        std::fs::write(base.join("old").join("data.ext4"), b"disk").unwrap();
        let survey = |name: &str| {
            let mut cfg = Config::default();
            cfg.vm.name = name.into();
            cfg.vm.backend = Some(BackendKind::Firecracker);
            cfg.vm.dir = base.to_string_lossy().into_owned();
            Vm::new(&cfg).survey().strays.len()
        };
        let (escapes, nested, ordinary) = (
            [survey(""), survey("."), survey(".."), survey("/etc")],
            survey("a/b"),
            survey("new"),
        );
        std::fs::remove_dir_all(&base).unwrap();
        for (name, n) in ["\"\"", ".", "..", "/etc"].iter().zip(escapes) {
            assert_eq!(n, 0, "[vm] name = {name} escapes [vm] dir");
        }
        // A nested name is still inside it, so its siblings are still
        // reported -- the guard must not over-fire.
        assert_eq!(nested, 1, "a/b is inside [vm] dir");
        assert_eq!(ordinary, 1);
    }

    #[test]
    fn only_ssf_names_are_ssfs_to_report() {
        // Someone else's lima instances are none of ssf's business, and
        // a bare `ssf-` is what an empty `[vm] name` would make -- a
        // broken configuration (#169), not a VM to report.
        assert!(is_ssf_name("ssf-default"));
        assert!(is_ssf_name("ssf-x"));
        assert!(!is_ssf_name("ssf-"));
        assert!(!is_ssf_name("ssf"));
        assert!(!is_ssf_name("default"));
        assert!(!is_ssf_name("my-ssf-vm"));
    }

    #[test]
    fn a_lima_that_will_not_answer_falls_back_to_limas_own_filesystem() {
        // `limactl` moved by an upgrade, off the PATH the service runs
        // under, a stale `[vm] limactl`, a locked lima home: evidence
        // about the tool, not about the machine. What settles it is
        // whether `<lima home>/<name>` or `<lima home>/_disks/<disk>` is
        // on disk -- so a missing binary can never be the reason a disk
        // full of workspaces is treated as absent, and a machine with
        // nothing of lima's on it still gets a clean `no VM`.
        let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Empty, Listing::Fails);
        std::fs::remove_dir_all(&t.vm.dir).unwrap();
        let home = t.vm.lima_home.clone().unwrap();
        // The fake's instance directory is there.
        let s = t.vm.survey();
        assert_eq!(
            (s.present, s.running, s.startable, s.data),
            (Some(true), None, false, Some(false))
        );
        // ... and so is a data disk, which is the half that matters.
        std::fs::create_dir_all(home.join("_disks").join("ssf-one")).unwrap();
        assert_eq!(t.vm.survey().data, Some(true));
        // Nothing of lima's anywhere: a real "no VM", not a refusal.
        std::fs::remove_dir_all(&home).unwrap();
        assert_eq!(
            t.vm.survey(),
            Survey {
                present: Some(false),
                running: Some(false),
                startable: false,
                data: Some(false),
                strays: Vec::new(),
                base_unread: false,
            }
        );
    }

    #[test]
    fn a_disk_probe_that_failed_leaves_the_instances_answer_standing() {
        // Only the disk question went unanswered: lima still said there
        // is no instance, so nothing is running and nothing can start.
        // The disk falls back to lima's own filesystem.
        let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Fails, Listing::Empty);
        std::fs::remove_dir_all(&t.vm.dir).unwrap();
        assert_eq!(
            t.vm.survey(),
            Survey {
                present: Some(false),
                running: Some(false),
                startable: false,
                data: Some(false),
                strays: Vec::new(),
                base_unread: false,
            }
        );
        // The direction that matters: the disk is on disk, so it is
        // there to be destroyed and there to stop the destroying.
        let home = t.vm.lima_home.clone().unwrap();
        std::fs::create_dir_all(home.join("_disks").join("ssf-one")).unwrap();
        assert_eq!(
            t.vm.survey(),
            Survey {
                present: Some(true),
                running: Some(false),
                startable: false,
                data: Some(true),
                strays: Vec::new(),
                base_unread: false,
            }
        );
    }

    #[test]
    fn a_disk_that_could_not_be_deleted_fails_the_step_with_the_instance_gone() {
        // The destroy is two deletions and the second can fail on its
        // own. It must stay a failure: `ssf uninstall` only writes
        // `[vm] enabled = false` when the destroy succeeded, and a
        // swallowed error here would turn the next run into a host-mode
        // one over a data disk that is still there.
        let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Fails, Listing::Answers);
        let home = t.vm.lima_home.clone().unwrap();
        std::fs::create_dir_all(home.join("_disks").join("ssf-one")).unwrap();
        let err = t.vm.lima_destroy().unwrap_err().to_string();
        assert!(err.contains("may hold it"), "{err}");
        assert!(t.ran("delete -f ssf-one"), "{:?}", t.commands());
        // With nothing of the disk in lima's home there is nothing to
        // delete, and a limactl that would not say so is no reason to
        // fail the step.
        std::fs::remove_dir_all(home.join("_disks")).unwrap();
        assert!(t.vm.lima_destroy().is_ok());
    }

    #[tokio::test]
    async fn the_vm_directory_goes_even_when_limas_half_of_the_destroy_failed() {
        // `[vm] dir` is ssf's own -- the generated template, the ssh key
        // and the share -- and lima not answering is no reason to leave
        // it behind for the next run to trip over. The failure is still
        // the step's failure, and it has to be, because `ssf uninstall`
        // only writes `[vm] enabled = false` once the destroy succeeded.
        let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Fails, Listing::Fails);
        assert!(t.vm.dir.exists());
        let err = t.vm.destroy().await.unwrap_err().to_string();
        assert!(err.contains("still holds something of ssf-one"), "{err}");
        assert!(!t.vm.dir.exists(), "the directory is ssf's own and goes");
        // Nothing of lima's and no directory: a destroy with nothing to
        // do is not a failure. What it prints is not pinned here --
        // stdout is awkward to capture from a test -- so this asserts
        // only what it can.
        std::fs::remove_dir_all(t.vm.lima_home.clone().unwrap()).unwrap();
        assert!(t.vm.destroy().await.is_ok());
    }

    #[test]
    fn a_limactl_that_cannot_run_does_not_fail_a_destroy_over_nothing() {
        // `ssf uninstall` on a machine with no lima left: the destroy
        // step must not end in a failed step over a binary that is not
        // there any more. With something of lima's still in its home it
        // must, because there is then a VM ssf cannot delete.
        let t = Fake::with_all("Stopped", Edit::Applies, DiskList::Fails, Listing::Fails);
        let home = t.vm.lima_home.clone().unwrap();
        let err = t.vm.lima_destroy().unwrap_err().to_string();
        assert!(err.contains("still holds something of ssf-one"), "{err}");
        std::fs::remove_dir_all(&home).unwrap();
        assert!(!t.vm.lima_destroy().unwrap());
    }

    #[test]
    fn a_copy_of_the_template_that_cannot_be_read_counts_as_stale() {
        // A file that cannot be read says nothing about what lima will do
        // with the disk, and "nothing" is not "clean". A directory where
        // the file should be is that case without a file mode, which a
        // test run as root would ignore.
        let t = Fake::new("Stopped");
        let yaml = t.instance_yaml();
        std::fs::remove_file(&yaml).unwrap();
        std::fs::create_dir(&yaml).unwrap();
        assert_eq!(
            t.vm.repair_stale_format(Some(&t.instance), Why::FoundStale),
            Some(yaml)
        );
    }

    #[test]
    fn a_template_ssf_could_not_rewrite_stops_the_boot_as_well() {
        // The third way this used to fail open: the rewrite of ssf's own
        // template failed, the warning scrolled past, and the function
        // still returned "repaired". That file is what `ssf vm reset`
        // creates the next instance from.
        let t = Fake::new("Stopped");
        let path = t.vm.template_path();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(
            t.vm.repair_stale_format(None, Why::FoundStale),
            Some(path.clone())
        );
        let err = t.vm.stale_format_error(&path).to_string();
        assert!(err.contains("refusing to boot ssf-one"), "{err}");
        assert!(err.contains("could not rewrite"), "{err}");
        // The cure is not `limactl edit`: nothing here is lima's.
        assert!(!err.contains("limactl edit"), "{err}");
    }

    #[test]
    fn only_force_over_a_disk_no_guest_was_ever_seen_on_deletes_it() {
        // The one path in ssf that destroys the factory's data disk, and
        // until now the only one with no test. A disk exists (the fake
        // answers `disk list`), so a plain build keeps it; `--force`
        // alone keeps it too, because a disk with no marker is one a
        // build saw a guest come up on. Only `--force` over the marker
        // deletes it -- and then the build makes a fresh one, marks that
        // one unproven in turn, and is the one build that may hand lima
        // `format: true`.
        for (force, marked, deleted) in [
            (false, false, false),
            (false, true, false),
            (true, false, false),
            (true, true, true),
        ] {
            let t = Fake::new("Stopped");
            if marked {
                t.vm.mark_disk_unproven();
            }
            let make_disk =
                t.vm.take_data_disk(force)
                    .unwrap_or_else(|e| panic!("force={force} marked={marked}: {e:#}"));
            assert_eq!(make_disk, deleted, "force={force} marked={marked}");
            assert_eq!(
                t.ran("disk delete"),
                deleted,
                "force={force} marked={marked}: limactl ran {:?}",
                t.commands()
            );
            if deleted {
                assert!(
                    t.commands().iter().any(|c| c == "disk delete ssf-one"),
                    "{:?}",
                    t.commands()
                );
                // The marker goes with the disk it pointed at; the fresh
                // disk gets its own.
                assert!(!t.vm.unproven_disk().exists());
                t.vm.create_data_disk().unwrap();
                assert!(
                    t.commands()
                        .iter()
                        .any(|c| c.starts_with("disk create ssf-one --size")),
                    "{:?}",
                    t.commands()
                );
                assert!(t.vm.unproven_disk().exists());
            } else {
                // Nothing was destroyed, and the marker (where there was
                // one) still points at the same disk.
                assert_eq!(t.vm.unproven_disk().exists(), marked);
            }
        }
    }

    #[test]
    fn a_finishing_build_states_a_fact_and_every_other_caller_reports_a_repair() {
        // The build that made the data disk is the one build allowed to
        // let lima format it, so that build turning the flag off at the
        // end is not a repair: every first build used to warn that a
        // build had not finished. The two readings differ only in what
        // they say, which is why the saying is a function.
        let finishing = format_off_note(Why::FinishingBuild, "ssf-one", "lima's copy");
        assert!(finishing.contains("carries the factory now"), "{finishing}");
        assert!(
            !finishing.contains("a build that did not finish"),
            "{finishing}"
        );
        let stale = format_off_note(Why::FoundStale, "ssf-one", "lima's copy");
        assert!(stale.contains("a build that did not finish"), "{stale}");
        assert!(stale.contains("putting `format: false` back"), "{stale}");

        // And it is only the wording: a finishing build repairs a stopped
        // instance in place ...
        let t = Fake::new("Stopped");
        assert_eq!(
            t.vm.repair_stale_format(Some(&t.instance), Why::FinishingBuild),
            None
        );
        assert!(
            t.commands()
                .iter()
                .any(|c| c == &format!("edit ssf-one --set {FORMAT_OFF}")),
            "{:?}",
            t.commands()
        );
        assert!(!says_format_true(
            &std::fs::read_to_string(t.instance_yaml()).unwrap()
        ));
        // ... and hands back what it could not repair just the same, so
        // `lima_first_boot` fails rather than leaving the flag behind.
        let t = Fake::with("Stopped", Edit::Ignored, DiskList::Answers);
        assert_eq!(
            t.vm.repair_stale_format(Some(&t.instance), Why::FinishingBuild),
            Some(t.instance_yaml())
        );
    }

    #[test]
    fn a_reset_refuses_a_template_that_would_let_lima_format_the_disk() {
        // `ssf vm reset` creates the next instance from ssf's own
        // template, so a `format: true` still in it is a boot over a disk
        // that holds the factory -- the one thing this path exists to
        // stop. The repair's answer used to be dropped with a comment
        // saying nothing could come back; a rewrite that fails hands back
        // the template, and the reset now refuses on it.
        let t = Fake::new("Stopped");
        let path = t.vm.template_path();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let err =
            t.vm.lima_reset()
                .expect_err(
                    "a reset must not create an instance from a template that says format: true",
                )
                .to_string();
        assert!(
            err.contains("still lets lima format the data disk"),
            "{err}"
        );
        assert!(err.contains("could not rewrite"), "{err}");
        // And it stopped before it touched the instance.
        assert!(!t.ran("delete") && !t.ran("create"), "{:?}", t.commands());
    }

    #[test]
    fn the_console_names_the_instance_when_there_is_no_log_to_show() {
        // `lima_console_log` picked `serial.log` whenever `serialv.log`
        // was missing -- including when neither was there -- so
        // `ssf vm console` on an instance that has never booted ran
        // `tail` on a path that did not exist and showed `tail: cannot
        // open`, which reads as a broken command.
        let t = Fake::new("Stopped");
        let dir = PathBuf::from(&t.instance.dir);
        let err =
            t.vm.lima_console_log()
                .expect_err("no log has been written yet")
                .to_string();
        assert!(err.contains("ssf-one"), "{err}");
        assert!(err.contains("no console log yet"), "{err}");
        // Whichever lima wrote is the one that comes back.
        std::fs::write(dir.join("serialv.log"), "virtio\n").unwrap();
        assert_eq!(t.vm.lima_console_log().unwrap(), dir.join("serialv.log"));
        std::fs::write(dir.join("serial.log"), "serial\n").unwrap();
        assert_eq!(t.vm.lima_console_log().unwrap(), dir.join("serial.log"));
    }

    #[test]
    fn a_cleanup_after_a_failed_build_does_not_talk_about_refusing_a_boot() {
        // `after_failed_build` and the look `ssf vm start` takes once the
        // guest is up both warn with this: no boot is being refused
        // there, and saying so sent people looking for a boot that had
        // not happened.
        let t = Fake::new("Stopped");
        let note = t.vm.stale_format_note(&t.instance_yaml());
        assert!(!note.contains("refusing to boot"), "{note}");
        assert!(
            note.contains("the next boot of ssf-one is refused"),
            "{note}"
        );
        assert!(note.contains(FORMAT_OFF), "{note}");
    }

    #[test]
    fn a_disk_no_guest_ever_used_is_named_in_the_builds_own_error() {
        // A build that died before lima's boot script formatted the disk
        // leaves it blank, and no later build, start or reset formats a
        // disk it did not create. The way out belongs in the error the
        // build fails with -- the failure it actually shows is about ssh,
        // eight minutes later, and points nowhere near the disk.
        let t = Fake::new("Stopped");
        let untouched = format!(
            "{:#}",
            t.vm.blank_disk_hint(anyhow::anyhow!("the guest does not answer as ssf"))
        );
        assert_eq!(untouched, "the guest does not answer as ssf");
        t.vm.mark_disk_unproven();
        assert!(t.vm.unproven_disk().exists());
        let hinted = format!(
            "{:#}",
            t.vm.blank_disk_hint(anyhow::anyhow!("the guest does not answer as ssf"))
        );
        assert!(
            hinted.contains("the guest does not answer as ssf"),
            "{hinted}"
        );
        assert!(hinted.contains("ssf vm build --force"), "{hinted}");
        assert!(hinted.contains("ssf-one"), "{hinted}");
    }

    /// A `Vm` whose `limactl` is a script that answers `list` and `disk
    /// list` and records everything it is asked, with both templates left
    /// saying `format: true` as a build that died would leave them.
    struct Fake {
        vm: Vm,
        instance: Instance,
        dir: PathBuf,
    }

    /// What the fake's `limactl edit --set` does: what lima's does, or
    /// what a lima whose restricted `yq` matched nothing would do -- exit
    /// 0 and leave the file exactly as it was.
    #[derive(Clone, Copy, PartialEq)]
    enum Edit {
        Applies,
        Ignored,
    }

    /// What the fake's `limactl disk list --json` does. `Fails` is a
    /// lima home someone else holds the lock on; `Empty` is a lima that
    /// has no such disk.
    #[derive(Clone, Copy, PartialEq)]
    enum DiskList {
        Answers,
        Empty,
        Fails,
    }

    /// What the fake's `limactl list --json` does. It is both the
    /// liveness question every caller of [`Vm::running_state`] and
    /// [`Vm::running_now`] asks and the "is there an instance at all"
    /// question [`Vm::lima_survey`] asks, so it has to be able to name
    /// the instance, name nothing (lima never had it, or it has been
    /// deleted), fail the way a locked lima home fails, and stop
    /// returning. A fork that failed under load, a lima home under
    /// someone else's lock and a limactl that hangs are all things a
    /// laptop does; none of them is the VM having exited.
    #[derive(Clone, Copy, PartialEq)]
    enum Listing {
        Answers,
        Empty,
        Fails,
        Hangs,
        /// This configuration's instance and an `ssf-old` beside it:
        /// what a changed `[vm] name` leaves in lima's home.
        Strays,
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl Fake {
        fn new(status: &str) -> Self {
            Self::with(status, Edit::Applies, DiskList::Answers)
        }

        fn with(status: &str, edit: Edit, disks: DiskList) -> Self {
            Self::with_all(status, edit, disks, Listing::Answers)
        }

        /// A running instance whose listing behaves like this.
        fn listing(listing: Listing) -> Self {
            Self::with_all("Running", Edit::Applies, DiskList::Answers, listing)
        }

        fn with_all(status: &str, edit: Edit, disks: DiskList, listing: Listing) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "ssf-lima-fake-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let inst_dir = dir.join("lima/ssf-one");
            std::fs::create_dir_all(&inst_dir).unwrap();
            let log = dir.join("limactl.log");
            let json = format!(
                r#"{{"name":"ssf-one","status":"{status}","dir":"{}","sshLocalPort":2222,"cpus":3,"memory":8589934592}}"#,
                inst_dir.display()
            );
            let limactl = dir.join("limactl");
            let yaml = inst_dir.join("lima.yaml");
            let disk_arm = match disks {
                DiskList::Answers => r#"echo '{"name":"ssf-one","size":21474836480,"dir":"/d","mountPoint":"/mnt/lima-ssf-one"}'"#.to_string(),
                DiskList::Empty => ":".to_string(),
                DiskList::Fails => {
                    r#"echo 'FATAL[0000] failed to lock the lima home' >&2; exit 1"#.to_string()
                }
            };
            // lima's own `--set` rewrites the instance's copy in place.
            let list_arm = match listing {
                Listing::Answers => format!("echo '{json}'"),
                Listing::Empty => ":".to_string(),
                Listing::Fails => {
                    r#"echo 'FATAL[0000] failed to lock the lima home' >&2; exit 1"#.to_string()
                }
                // Long enough that no bound this test asks for expires
                // on its own. `exec` so that the fake shell *is* the
                // sleep: what the probe kills is its direct child, and a
                // `sleep` forked under that shell would outlive the test
                // by two minutes, once per run and again under
                // `makepkg`'s check().
                Listing::Hangs => "exec sleep 120".to_string(),
                Listing::Strays => format!(
                    "echo '{json}'; echo '{}'",
                    json.replace("ssf-one", "ssf-old")
                ),
            };
            let edit_arm = match edit {
                Edit::Applies => format!(
                    "sed 's/format: true/format: false/' {y} > {y}.new && mv {y}.new {y}",
                    y = yaml.display()
                ),
                Edit::Ignored => ":".to_string(),
            };
            std::fs::write(
                &limactl,
                // `shell` fails the way limactl fails against an instance
                // that is not running, so a wait that got that far ends
                // on the instance's state instead of sitting out
                // PROVISION_TIMEOUT.
                format!(
                    r#"#!/bin/sh
shift
printf '%s\n' "$*" >> {log}
case "$*" in
  'disk list --json') {disk_arm} ;;
  'list --json') {list_arm} ;;
  edit*--set*) {edit_arm} ;;
  shell*) echo 'instance "ssf-one" is stopped, run `limactl start ssf-one`' >&2; exit 1 ;;
esac
exit 0
"#,
                    log = log.display(),
                ),
            )
            .unwrap();
            make_executable(&limactl).unwrap();
            let mut cfg = Config::default();
            cfg.vm.dir = dir.join("vm").to_string_lossy().into_owned();
            cfg.vm.name = "one".into();
            cfg.vm.backend = Some(BackendKind::Lima);
            cfg.vm.limactl = Some(limactl.to_string_lossy().into_owned());
            // `share/` is written on the way into a start, and the seed
            // tree in it holds the guest's own `ssf` binary. On a Linux
            // host that is this process's binary -- 150 MB of debug build
            // copied into the temporary directory on every run of this
            // test. The fake stands in for it: what is being tested here
            // is the order of the steps, not what the seed carries.
            cfg.vm.guest_binary = Some(limactl.to_string_lossy().into_owned());
            let mut vm = Vm::new(&cfg);
            // lima's home, where the instance directory the fake made
            // lives and where `_disks/` would be: the tests must not
            // reach the person's own `~/.lima`.
            vm.lima_home = Some(dir.join("lima"));
            std::fs::create_dir_all(&vm.dir).unwrap();
            // What a build that died after `limactl create` leaves: both
            // copies of the template still say `format: true`.
            std::fs::write(vm.template_path(), vm.lima_template(true).unwrap()).unwrap();
            std::fs::write(inst_dir.join("lima.yaml"), vm.lima_template(true).unwrap()).unwrap();
            let instance = parse_instances(&json).pop().expect("one instance");
            Self { vm, instance, dir }
        }

        fn instance_yaml(&self) -> PathBuf {
            PathBuf::from(&self.instance.dir).join("lima.yaml")
        }

        fn commands(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.join("limactl.log"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }

        fn ran(&self, verb: &str) -> bool {
            self.commands().iter().any(|c| c.starts_with(verb))
        }
    }

    #[test]
    fn a_liveness_probe_that_could_not_be_made_is_not_a_stopped_vm() {
        // The listing is a fork of a ~60 MB Go binary against a lima
        // home someone else may hold the lock on. When it fails, the one
        // thing that must not happen is the answer "the VM is not
        // running": the gate in front of every forwarded command refuses
        // on that, and the supervisor ends the daemon on it.
        let t = Fake::listing(Listing::Fails);
        let err =
            t.vm.running_now()
                .expect_err("a failed listing is not an answer");
        let text = format!("{err:#}");
        assert!(
            text.contains("asking lima whether ssf-one is running"),
            "{text}"
        );
        assert!(text.contains("failed to lock the lima home"), "{text}");
        assert_eq!(t.vm.running_state(), None);
        // And the lossy form is exactly what neither may use: it says
        // the instance is stopped, over an instance the fake reports as
        // Running.
        assert!(!t.vm.running());
        assert!(t.vm.running_now().is_err());
        // A listing that answers is unchanged by any of this.
        let up = Fake::listing(Listing::Answers);
        assert!(up.vm.running_now().unwrap());
        assert_eq!(up.vm.running_state(), Some(true));
    }

    #[tokio::test]
    async fn status_json_keeps_an_unanswered_lima_probe_unknown() {
        let t = Fake::listing(Listing::Fails);
        let status = t.vm.status().await;

        assert_eq!(status.running, None);
        assert!(status.probe_error.is_some());
        let json = serde_json::to_value(status).unwrap();
        assert_eq!(json["running"], serde_json::Value::Null);
        assert!(
            json["probe_error"]
                .as_str()
                .is_some_and(|error| error.contains("failed to lock the lima home"))
        );
    }

    #[test]
    fn a_liveness_probe_that_never_returns_is_cut_off_rather_than_waited_out() {
        // The gate asks this in front of every forwarded command, with a
        // person waiting on it, so a limactl that has stopped returning
        // is given a bound and its silence becomes "cannot tell" -- the
        // answer the gate is willing to carry on without. The bound
        // itself is LIVENESS_LIMIT; this holds the mechanism to a
        // fraction of it so the suite does not sit out fifteen seconds.
        let t = Fake::listing(Listing::Hangs);
        let started = Instant::now();
        let err =
            t.vm.lima_running_probe(Duration::from_millis(300))
                .expect_err("a listing that never returns is not an answer");
        let text = format!("{err:#}");
        assert!(text.contains("did not finish within"), "{text}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_stopped_or_missing_instance_ends_the_provisioning_wait() {
        // A probe that cannot run is usually transient (ssh in the guest
        // is not up yet). A stopped or deleted instance is not: nothing
        // is going to provision itself, and reading it as "nothing seen"
        // is what parked a wait for the whole PROVISION_TIMEOUT in
        // silence.
        assert_eq!(terminal_state(Some("Running")), None);
        let gone = terminal_state(None).unwrap();
        assert!(gone.contains("not there any more"), "{gone}");
        assert_eq!(
            terminal_state(Some("Stopped")).unwrap(),
            "is stopped, not running"
        );
        assert_eq!(
            terminal_state(Some("Broken")).unwrap(),
            "is broken, not running"
        );
    }

    #[test]
    fn a_stale_format_true_is_recognised_in_either_template() {
        // What `repair_stale_format` reads: ssf's own template, and
        // lima's copy of it in the instance directory (a build that died
        // after `limactl create` leaves `format: true` in both).
        assert!(says_format_true(&vm().lima_template(true).unwrap()));
        assert!(!says_format_true(&vm().lima_template(false).unwrap()));
        // lima's copy indents and lists it as it pleases.
        assert!(says_format_true(
            "additionalDisks:\n- name: ssf-one\n  format: true\n  fsType: ext4\n"
        ));
        assert!(says_format_true("  - format: true\n"));
        assert!(!says_format_true("# format: true is what a build writes\n"));
        assert!(!says_format_true(""));
    }

    #[test]
    fn the_probe_says_what_it_found_in_its_output_not_its_status() {
        // Every test in the probe fails in the ordinary case, so the script
        // must end by forcing a zero status; otherwise the call counts as
        // failed and its output is discarded.
        let probe = provision_probe();
        assert!(probe.trim_end().ends_with("exit 0"), "{probe}");
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(
                // Keep host processes from changing the probe's output: the
                // test's shell-local pgrep always reports no match.
                format!(
                    "pgrep() {{ return 1; }}; {}",
                    probe
                        .replace(PROVISION_MARKER, "/nonexistent/marker")
                        .replace(PROVISION_LOG, "/nonexistent/log")
                ),
            )
            .output()
            .expect("sh runs");
        assert!(
            out.status.success(),
            "a probe that finds nothing still exits 0"
        );
        assert_eq!(
            parse_probe(&String::from_utf8_lossy(&out.stdout)),
            Probe::default()
        );
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
