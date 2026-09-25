//! The incus backend of `ssf vm` (`[vm] backend = "incus"`; Linux only,
//! for hosts without KVM). The guest is **not a VM**: it is an
//! unprivileged [Incus](https://linuxcontainers.org/incus/) system
//! container, `ssf-<name>`, that shares the host's kernel behind user
//! namespaces. `ssf vm status` and the docs say so, since that boundary is
//! weaker than a VM's. It runs the same guest scripts as the lima backend:
//!
//! * `ssf vm build`: preflight (`incus` found, the daemon answers, the
//!   default profile names a storage pool), `share/` written as for lima,
//!   the custom volume `ssf-<name>` created in that pool at `[vm]
//!   data_gib`, and the container created from `images:ubuntu/24.04` (or
//!   `[vm] image`) with `security.nesting` and the `mknod`/`setxattr`
//!   syscall intercepts (Docker inside works), `limits.cpu`,
//!   `limits.memory` and the root disk size from `[vm]`, and three
//!   devices: `share/` read-only at `/mnt/ssf` (idmapped with `shift`, so
//!   the container's root can read the host user's private files), the
//!   volume at `/var/lib/ssf`, and a proxy from `127.0.0.1:<ssh_port>` on
//!   the host to the container's sshd. The first start provisions it:
//!   the host runs `lima-boot.sh` through `incus exec` with
//!   `SSF_VM_BACKEND=incus`, which runs `provision.sh` once and writes the
//!   same marker as under lima. Then ssh as `ssf`, and the container stops.
//! * `ssf vm start`: `share/` written fresh, the sizes applied, `incus
//!   start`, `lima-boot.sh` run again (it returns at once when the marker
//!   is there; a reset container provisions itself here), ssh and the
//!   daemon awaited.
//! * Every boot: `ssf-seed.service` runs `seed-lima.sh`, which finds the
//!   data volume Incus has mounted on `/var/lib/ssf` and seeds the guest
//!   from `/mnt/ssf/seed`.
//!
//! `ssf vm reset` deletes and re-creates the container and keeps the
//! volume; `ssf vm grow` sets the volume's `size`; `ssf vm destroy`
//! deletes both. ssf never sets Incus up: installing it, the
//! `incus-admin` group and `incus admin init` need root and are
//! documented instead (docs/platform-specifics.md).

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;
use tracing::{debug, info, warn};

use super::lima::{
    CREATE_LIMIT, GUEST_MOUNT, OWN_TIMEOUT_MARGIN, PROBE_LIMIT, PROVISION_LOG, PROVISION_MARKER,
    PROVISION_TIMEOUT, QUICK_LIMIT, ROOT_GIB_FLOOR, SEED_TIMEOUT, STOP_LIMIT, drain, gib_ceil,
    wait_within,
};
use super::{GUEST_DATA_DIR, HostFacts, Survey, Vm, plan_grow, sizes_for, which};
use crate::config::Config;

/// What an unset `[vm] image` launches.
pub const DEFAULT_IMAGE: &str = "images:ubuntu/24.04";
/// The devices ssf adds to the container.
pub const SHARE_DEVICE: &str = "ssf-share";
pub const DATA_DEVICE: &str = "ssf-data";
pub const SSH_DEVICE: &str = "ssf-ssh";
/// The key ssf sets on the container and the volume at build: the uid of
/// the host user who built them. Incus names are per daemon, not per
/// user, so another user's `ssf-<name>` must never be adopted or deleted.
pub const OWNER_KEY: &str = "user.ssf.owner";
/// The liveness question in front of a forwarded command (see
/// `lima::LIVENESS_LIMIT`).
pub(super) const LIVENESS_LIMIT: Duration = Duration::from_secs(15);
/// How long the container may take to resolve a name before provisioning
/// starts anyway (and says why apt will fail).
const NETWORK_WAIT_SECS: u32 = 120;

/// The container, and the custom volume that holds its data: both
/// `ssf-<name>`.
pub fn instance_name(name: &str) -> String {
    format!("ssf-{name}")
}

/// Refuse a `[vm] name` Incus cannot name a container after: an instance
/// name is a hostname label (letters, digits and `-`, at most 63, not
/// starting or ending with `-`).
pub fn check_name(name: &str) -> Result<()> {
    let full = instance_name(name);
    let ok = full.len() <= 63
        && !full.ends_with('-')
        && full.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !ok {
        bail!(
            "[vm] name \"{name}\" cannot name an Incus container: `ssf-<name>` must be at most 63 letters, digits and `-`, and not end with `-`"
        );
    }
    Ok(())
}

/// What the container is made of.
#[derive(Debug, Clone)]
pub struct Spec<'a> {
    pub instance: &'a str,
    pub image: &'a str,
    pub pool: &'a str,
    pub volume: &'a str,
    pub share: &'a str,
    pub ssh_port: u16,
    pub vcpus: u32,
    pub mem_mib: u32,
    /// [`OWNER_KEY`]'s value: this user's uid.
    pub owner: &'a str,
}

/// This host user's uid, as [`OWNER_KEY`] holds it.
pub fn owner_uid() -> String {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() }.to_string()
}

/// Refuse to touch an Incus `what` (`container` or `volume`) `name` whose
/// [`OWNER_KEY`] is missing or is not `uid`.
pub fn check_owner(what: &str, name: &str, got: Option<&str>, uid: &str) -> Result<()> {
    match got.map(str::trim) {
        Some(o) if o == uid => Ok(()),
        Some(o) => bail!(
            "the Incus {what} {name} belongs to uid {o}, not this user (uid {uid}); Incus names are shared by every user of this Incus daemon, so ssf leaves it alone: set another `[vm] name`"
        ),
        None => bail!(
            "the Incus {what} {name} has no {OWNER_KEY} key, so ssf did not build it for this user (uid {uid}) and leaves it alone: set another `[vm] name`, or remove it by hand if it is yours"
        ),
    }
}

/// The `limits.*` keys for a size, as `incus init -c` and `incus config
/// set` take them.
pub fn limit_keys(vcpus: u32, mem_mib: u32) -> [String; 2] {
    [
        format!("limits.cpu={vcpus}"),
        format!("limits.memory={mem_mib}MiB"),
    ]
}

/// `incus init`: the container, unprivileged (Incus's default), with
/// nesting and the syscall intercepts Docker needs inside it.
pub fn init_args(s: &Spec) -> Vec<String> {
    let mut v: Vec<String> = ["init", s.image, s.instance]
        .iter()
        .map(|a| a.to_string())
        .collect();
    let mut keys = vec![
        "security.nesting=true".to_string(),
        "security.syscalls.intercept.mknod=true".to_string(),
        "security.syscalls.intercept.setxattr=true".to_string(),
        format!("{OWNER_KEY}={}", s.owner),
    ];
    keys.extend(limit_keys(s.vcpus, s.mem_mib));
    for k in keys {
        v.push("-c".into());
        v.push(k);
    }
    v
}

/// `incus config device add` for each device: the share, the data volume
/// and the ssh proxy.
pub fn device_args(s: &Spec) -> Vec<Vec<String>> {
    let add = |dev: &str, kind: &str, props: &[String]| {
        let mut v: Vec<String> = ["config", "device", "add", s.instance, dev, kind]
            .iter()
            .map(|a| a.to_string())
            .collect();
        v.extend(props.iter().cloned());
        v
    };
    vec![
        add(
            SHARE_DEVICE,
            "disk",
            &[
                format!("source={}", s.share),
                format!("path={GUEST_MOUNT}"),
                "readonly=true".into(),
                // Idmapped: the host user's files keep their owner in the
                // container, so its root can read the private ones.
                "shift=true".into(),
            ],
        ),
        add(
            DATA_DEVICE,
            "disk",
            &[
                format!("pool={}", s.pool),
                format!("source={}", s.volume),
                format!("path={GUEST_DATA_DIR}"),
            ],
        ),
        add(
            SSH_DEVICE,
            "proxy",
            &[
                format!("listen=tcp:127.0.0.1:{}", s.ssh_port),
                "connect=tcp:127.0.0.1:22".into(),
            ],
        ),
    ]
}

/// What the host runs in the container, as root, at every start: wait for
/// the network (apt needs it), then `lima-boot.sh`, which provisions once
/// and returns at once when the marker is there. An attempt starts with an
/// empty log, as under lima's boot hook.
pub fn boot_script() -> String {
    format!(
        r#"if [ ! -f {PROVISION_MARKER} ]; then
    : > {PROVISION_LOG}
    for i in $(seq {NETWORK_WAIT_SECS}); do getent hosts archive.ubuntu.com >/dev/null 2>&1 && break; sleep 1; done
    getent hosts archive.ubuntu.com >/dev/null 2>&1 || echo "ssf-provision: the container resolves no names after {NETWORK_WAIT_SECS}s; check the Incus network (docs/platform-specifics.md, Linux without KVM)" | tee -a {PROVISION_LOG}
fi
exec bash {GUEST_MOUNT}/guest/lima-boot.sh"#
    )
}

/// One entry of `incus list --format json`.
#[derive(Debug, Clone, Deserialize)]
pub struct Instance {
    pub name: String,
    /// `Running`, `Stopped`, `Frozen`, `Error`, ...
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub config: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub devices: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
}

impl Instance {
    pub fn is_running(&self) -> bool {
        self.status == "Running"
    }

    pub fn owner(&self) -> Option<&str> {
        self.config.get(OWNER_KEY).map(String::as_str)
    }

    /// The pool of the data device: where this container's volume is.
    pub fn data_pool(&self) -> Option<&str> {
        self.devices
            .get(DATA_DEVICE)
            .and_then(|d| d.get("pool"))
            .map(String::as_str)
            .filter(|p| !p.is_empty())
    }
}

pub fn parse_instances(text: &str) -> Result<Vec<Instance>> {
    serde_json::from_str(text.trim()).context("reading `incus list --format json`")
}

/// One entry of `incus storage volume list --format json`.
#[derive(Debug, Clone, Deserialize)]
pub struct Volume {
    pub name: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub config: std::collections::HashMap<String, String>,
}

impl Volume {
    /// The volume's `size`, when it has one (a pool that cannot cap it
    /// leaves it unset).
    pub fn size_bytes(&self) -> Option<u64> {
        self.config.get("size").and_then(|s| parse_size(s))
    }

    pub fn owner(&self) -> Option<&str> {
        self.config.get(OWNER_KEY).map(String::as_str)
    }
}

/// One entry of `incus storage list --format json`.
#[derive(Debug, Clone, Deserialize)]
pub struct Pool {
    pub name: String,
}

pub fn parse_pools(text: &str) -> Result<Vec<Pool>> {
    serde_json::from_str(text.trim()).context("reading `incus storage list --format json`")
}

pub fn parse_volumes(text: &str) -> Result<Vec<Volume>> {
    serde_json::from_str(text.trim()).context("reading `incus storage volume list --format json`")
}

/// An Incus size (`20GiB`, `10GB`, `1073741824`) in bytes.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mult: f64 = match unit.trim() {
        "" | "B" => 1.0,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((n * mult) as u64)
}

/// Where the Incus daemon's socket is: `$INCUS_SOCKET`, else
/// `$INCUS_DIR/unix.socket`, else `/var/lib/incus/unix.socket`.
pub fn socket_path_from(socket: Option<&str>, dir: Option<&str>) -> PathBuf {
    let set = |v: Option<&str>| {
        v.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    };
    set(socket).unwrap_or_else(|| {
        set(dir)
            .unwrap_or_else(|| PathBuf::from("/var/lib/incus"))
            .join("unix.socket")
    })
}

pub fn socket_path() -> PathBuf {
    socket_path_from(
        std::env::var("INCUS_SOCKET").ok().as_deref(),
        std::env::var("INCUS_DIR").ok().as_deref(),
    )
}

/// What to do when the daemon cannot be reached.
pub const DAEMON_HINT: &str = "the Incus daemon must be running and this user in the `incus-admin` group (`sudo usermod -aG incus-admin $USER`, then log in again), with a storage pool and network from `sudo incus admin init --minimal`; see docs/platform-specifics.md";
/// What to do when `incus` is missing.
pub const INSTALL_HINT: &str = "install Incus (Debian 13 and Ubuntu 24.04: `sudo apt install incus`; Arch: `sudo pacman -S incus`; Fedora: `sudo dnf install incus`), then see docs/platform-specifics.md, Linux without KVM";

impl Vm {
    /// The container and its data volume: `ssf-<name>`.
    pub fn incus_name(&self) -> String {
        instance_name(&self.cfg.name)
    }

    fn incus_image(&self) -> &str {
        self.cfg.image.as_deref().unwrap_or(DEFAULT_IMAGE)
    }

    // ---- incus ----

    fn incus_label(args: &[&str]) -> String {
        format!("incus {}", args.join(" "))
    }

    /// Run `incus` for its output, within `limit`; stderr goes into the
    /// error.
    fn incus_output_within(&self, args: &[&str], limit: Duration) -> Result<String> {
        let label = Self::incus_label(args);
        let mut child = Command::new("incus")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("running incus (is Incus installed and on PATH?)")?;
        let out = drain(child.stdout.take().expect("piped"));
        let err = drain(child.stderr.take().expect("piped"));
        debug!(command = %label, "running incus");
        let status = wait_within(&mut child, &label, limit)?;
        let stdout = out.join().unwrap_or_default();
        let stderr = err.join().unwrap_or_default();
        if !status.success() {
            bail!(
                "`{label}` failed ({status}): {}",
                String::from_utf8_lossy(&stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&stdout).to_string())
    }

    /// Run `incus` with this terminal, within `limit`.
    fn incus_run_within(&self, args: &[&str], limit: Duration) -> Result<()> {
        let label = Self::incus_label(args);
        let mut child = Command::new("incus")
            .args(args)
            .stdin(Stdio::null())
            .spawn()
            .context("running incus (is Incus installed and on PATH?)")?;
        let st = wait_within(&mut child, &label, limit)?;
        if !st.success() {
            bail!("`{label}` failed ({st})");
        }
        Ok(())
    }

    fn incus_run(&self, args: &[String], limit: Duration) -> Result<()> {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.incus_run_within(&args, limit)
    }

    /// The container, when Incus has it.
    pub(in crate::vm) fn incus_instance_within(&self, limit: Duration) -> Result<Option<Instance>> {
        let name = self.incus_name();
        let out =
            self.incus_output_within(&["list", &format!("^{name}$"), "--format", "json"], limit)?;
        Ok(parse_instances(&out)?.into_iter().find(|i| i.name == name))
    }

    pub(in crate::vm) fn incus_instance(&self) -> Result<Option<Instance>> {
        self.incus_instance_within(QUICK_LIMIT)
    }

    /// The storage pool the default profile puts roots in: where the data
    /// volume lives too.
    pub(in crate::vm) fn incus_pool(&self) -> Result<String> {
        let pool = self
            .incus_output_within(
                &["profile", "device", "get", "default", "root", "pool"],
                QUICK_LIMIT,
            )
            .with_context(|| {
                format!("finding the default profile's storage pool; {DAEMON_HINT}")
            })?;
        let pool = pool.trim().to_string();
        if pool.is_empty() {
            bail!("the default Incus profile has no root disk pool; {DAEMON_HINT}");
        }
        Ok(pool)
    }

    /// The pool this VM's volume is in: the existing container's data
    /// device says, so a later change of the default profile's pool
    /// cannot send a reset or a destroy to the wrong one; without a
    /// container, the default profile's.
    pub(in crate::vm) fn incus_data_pool(&self, inst: Option<&Instance>) -> Result<String> {
        match inst {
            Some(i) => i.data_pool().map(str::to_string).with_context(|| {
                format!(
                    "the Incus container {} has no {DATA_DEVICE} device naming a pool, so ssf cannot tell where its volume is; `ssf vm build --force` re-creates it",
                    i.name
                )
            }),
            None => self.incus_pool(),
        }
    }

    /// Every pool holding a custom volume `ssf-<name>`, with it.
    fn incus_volumes_anywhere(&self) -> Result<Vec<(String, Volume)>> {
        let out =
            self.incus_output_within(&["storage", "list", "--format", "json"], QUICK_LIMIT)?;
        let mut found = Vec::new();
        for p in parse_pools(&out)? {
            if let Some(v) = self.incus_volume(&p.name)? {
                found.push((p.name, v));
            }
        }
        Ok(found)
    }

    fn incus_check_instance(&self, inst: &Instance) -> Result<()> {
        check_owner("container", &inst.name, inst.owner(), &owner_uid())
    }

    fn incus_check_volume(&self, pool: &str, vol: &Volume) -> Result<()> {
        check_owner(
            "volume",
            &format!("{} (pool {pool})", vol.name),
            vol.owner(),
            &owner_uid(),
        )
    }

    /// The data volume, when the pool has it.
    pub(in crate::vm) fn incus_volume(&self, pool: &str) -> Result<Option<Volume>> {
        let name = self.incus_name();
        let out = self.incus_output_within(
            &["storage", "volume", "list", pool, "--format", "json"],
            QUICK_LIMIT,
        )?;
        Ok(parse_volumes(&out)?
            .into_iter()
            .find(|v| v.kind == "custom" && v.name == name))
    }

    pub(in crate::vm) fn incus_running_probe(&self, limit: Duration) -> Result<bool> {
        self.incus_instance_within(limit)
            .map(|i| i.is_some_and(|i| i.is_running()))
            .with_context(|| format!("asking Incus whether {} is running", self.incus_name()))
    }

    pub(in crate::vm) fn incus_running_state(&self) -> Option<bool> {
        match self.incus_running_probe(QUICK_LIMIT) {
            Ok(r) => Some(r),
            Err(e) => {
                warn!("{e:#}");
                None
            }
        }
    }

    fn require_linux() -> Result<()> {
        if std::env::consts::OS != "linux" {
            bail!(
                "the incus backend runs the guest as a Linux container sharing the host kernel, so it is Linux only; this machine is {}. Use `ssf config set vm.backend lima`",
                std::env::consts::OS
            );
        }
        Ok(())
    }

    /// What a build needs: Linux, `incus`, a daemon that answers this
    /// user, and a storage pool in the default profile.
    pub(in crate::vm) fn incus_preflight(&self) -> Result<String> {
        Self::require_linux()?;
        check_name(&self.cfg.name)?;
        if which("incus").is_none() {
            bail!("incus is not on PATH; {INSTALL_HINT}");
        }
        if let Err(e) = self.incus_output_within(&["info"], PROBE_LIMIT) {
            bail!("`incus info` failed ({e:#}); {DAEMON_HINT}");
        }
        self.incus_pool()
    }

    // ---- build / start / stop ----

    pub(in crate::vm) async fn incus_build(&self, host: &Config, force: bool) -> Result<()> {
        let mut pool = self.incus_preflight()?;
        let name = self.incus_name();
        if let Some(inst) = self.incus_instance()? {
            self.incus_check_instance(&inst)?;
            if let Some(p) = inst.data_pool() {
                pool = p.to_string();
            }
            if !force {
                println!("Incus container {name} exists; `ssf vm build --force` makes a new one");
                return Ok(());
            }
            if inst.is_running() {
                self.incus_stop().await?;
            }
            info!("deleting Incus container {name}");
            self.incus_run_within(&["delete", "-f", &name], QUICK_LIMIT)?;
        }
        self.ensure_key()?;
        let _ = std::fs::remove_file(self.known_hosts());
        self.write_share(host)?;
        match self.incus_volume(&pool)? {
            Some(v) => self.incus_check_volume(&pool, &v)?,
            None => {
                let gib = self.sizes().data_gib;
                info!("creating Incus volume {name} in pool {pool} ({gib} GiB)");
                self.incus_run_within(
                    &[
                        "storage",
                        "volume",
                        "create",
                        &pool,
                        &name,
                        &format!("{OWNER_KEY}={}", owner_uid()),
                    ],
                    QUICK_LIMIT,
                )?;
                self.set_volume_size(&pool, gib);
            }
        }
        self.incus_create(&pool)?;
        if let Err(e) = self.incus_first_boot().await {
            if let Err(stop) = self.incus_stop().await {
                warn!("could not stop {name} after the build failed: {stop:#}");
            }
            return Err(e);
        }
        println!(
            "built Incus container {name} (it shares this host's kernel: a user-namespaced container, not a VM); `ssf vm start` starts it"
        );
        Ok(())
    }

    /// Cap the data volume. A pool that cannot (a `dir` pool on a
    /// filesystem without project quotas) leaves it uncapped, which is
    /// said rather than failed on.
    fn set_volume_size(&self, pool: &str, gib: u32) {
        let name = self.incus_name();
        if let Err(e) = self.incus_output_within(
            &[
                "storage",
                "volume",
                "set",
                pool,
                &name,
                &format!("size={gib}GiB"),
            ],
            QUICK_LIMIT,
        ) {
            warn!(
                "could not cap the volume {name} at {gib} GiB ({e:#}); the pool {pool} does not support it, so the volume can fill the pool's filesystem"
            );
        }
    }

    /// `incus init` and the devices, from `[vm]`.
    pub(in crate::vm) fn incus_create(&self, pool: &str) -> Result<()> {
        let name = self.incus_name();
        let share = self.share_dir().to_string_lossy().to_string();
        let sizes = self.sizes();
        let owner = owner_uid();
        let spec = Spec {
            instance: &name,
            image: self.incus_image(),
            pool,
            volume: &name,
            share: &share,
            ssh_port: self.cfg.ssh_port,
            vcpus: sizes.vcpus,
            mem_mib: sizes.mem_mib,
            owner: &owner,
        };
        info!("creating Incus container {name} from {}", spec.image);
        self.incus_run(&init_args(&spec), CREATE_LIMIT)?;
        // A container without its devices would pass for a built one
        // ("exists") at the next build: remove it rather than leave it.
        if let Err(e) = self.incus_configure(&spec) {
            if let Err(d) = self.incus_run_within(&["delete", "-f", &name], QUICK_LIMIT) {
                warn!("could not remove the half-made container {name}: {d:#}");
            }
            return Err(e);
        }
        Ok(())
    }

    /// The root size and the devices of a container `incus init` made.
    fn incus_configure(&self, spec: &Spec) -> Result<()> {
        let name = spec.instance;
        let root = self.cfg.root_gib.max(ROOT_GIB_FLOOR);
        if let Err(e) = self.incus_output_within(
            &[
                "config",
                "device",
                "override",
                name,
                "root",
                &format!("size={root}GiB"),
            ],
            QUICK_LIMIT,
        ) {
            warn!("could not cap {name}'s root disk at {root} GiB ({e:#}); it stays uncapped");
        }
        for args in device_args(spec) {
            self.incus_run(&args, QUICK_LIMIT)?;
        }
        Ok(())
    }

    /// Run the boot script in the container (see [`boot_script`]): the
    /// first boot of a container provisions it; later ones return at once.
    async fn incus_provision(&self) -> Result<()> {
        let name = self.incus_name();
        let script = boot_script();
        let args = [
            "exec",
            &name,
            "--env",
            "SSF_VM_BACKEND=incus",
            "--",
            "bash",
            "-c",
            &script,
        ];
        if let Err(e) = self.incus_run_within(&args, PROVISION_TIMEOUT + OWN_TIMEOUT_MARGIN) {
            let tail = self
                .incus_output_within(
                    &["exec", &name, "--", "tail", "-50", PROVISION_LOG],
                    PROBE_LIMIT,
                )
                .unwrap_or_default();
            bail!(
                "provisioning failed in {name} ({e:#}); the end of {PROVISION_LOG}:\n{}",
                tail.trim_end()
            );
        }
        Ok(())
    }

    async fn incus_first_boot(&self) -> Result<()> {
        let name = self.incus_name();
        info!("starting {name} for its first boot: the guest provisions itself (a few minutes)");
        self.incus_run_within(&["start", &name], QUICK_LIMIT)?;
        self.incus_provision().await?;
        if let Err(e) = self.wait_for_ssh(SEED_TIMEOUT).await {
            bail!(
                "{e:#} (provisioned, but the guest does not answer as {}); `incus console {name} --show-log` has its console",
                super::GUEST_USER
            );
        }
        self.incus_run_within(&["stop", &name], STOP_LIMIT)
    }

    /// `limits.*` from `[vm]`, so a change and `ssf vm restart` apply them
    /// as under the other backends.
    fn incus_apply_sizes(&self) -> Result<()> {
        let name = self.incus_name();
        let sizes = self.sizes();
        let mut args = vec!["config".to_string(), "set".into(), name];
        args.extend(limit_keys(sizes.vcpus, sizes.mem_mib));
        self.incus_run(&args, QUICK_LIMIT)
    }

    pub(in crate::vm) async fn incus_start(&self, host: &Config) -> Result<()> {
        Self::require_linux()?;
        let name = self.incus_name();
        match self.incus_instance()? {
            None => bail!("Incus container {name} does not exist; run `ssf vm build`"),
            Some(i) => self.incus_check_instance(&i)?,
        }
        self.ensure_key()?;
        self.write_share(host)?;
        self.incus_apply_sizes()?;
        self.incus_run_within(&["start", &name], QUICK_LIMIT)?;
        self.incus_provision().await?;
        if let Err(e) = self.wait_for_ssh(SEED_TIMEOUT).await {
            bail!("{e:#}; `incus console {name} --show-log` has its console");
        }
        let daemon = self.wait_for_daemon(Duration::from_secs(60)).await;
        self.report_up(daemon.as_deref());
        Ok(())
    }

    pub(in crate::vm) async fn incus_stop(&self) -> Result<()> {
        let name = self.incus_name();
        match self
            .incus_instance()
            .with_context(|| format!("asking Incus whether {name} is running"))?
        {
            None => {
                println!("there is no Incus container {name}");
                return Ok(());
            }
            Some(i) if !i.is_running() => {
                println!("VM {} is not running ({})", self.cfg.name, i.status);
                return Ok(());
            }
            Some(_) => {}
        }
        if let Err(e) = self.incus_run_within(&["stop", &name, "--timeout", "120"], STOP_LIMIT) {
            warn!("{e:#}; forcing it");
            self.incus_run_within(&["stop", "--force", &name], STOP_LIMIT)?;
        }
        println!("VM {} stopped", self.cfg.name);
        Ok(())
    }

    /// Set the volume's size (the container stopped); Incus grows the
    /// filesystem on it.
    pub(in crate::vm) fn incus_grow(&self, want: Option<u32>) -> Result<Option<u32>> {
        let inst = self.incus_instance()?;
        if let Some(i) = &inst {
            self.incus_check_instance(i)?;
        }
        let pool = self.incus_data_pool(inst.as_ref())?;
        let name = self.incus_name();
        let vol = self.incus_volume(&pool)?.with_context(|| {
            format!(
                "Incus volume {name} does not exist yet in pool {pool}; `ssf vm build` makes it at [vm] data_gib"
            )
        })?;
        self.incus_check_volume(&pool, &vol)?;
        let Some(size) = vol.size_bytes() else {
            bail!(
                "the Incus volume {name} has no size cap (the pool {pool} cannot set one), so there is nothing to grow: it can already use the pool's free space"
            );
        };
        let current = gib_ceil(size);
        let facts = HostFacts::probe(&self.sizing_dir().0)?;
        let rule = sizes_for(&facts).data_gib;
        let Some(target) = plan_grow(current, want, rule)? else {
            println!("{name} stays at {current} GiB");
            return Ok(None);
        };
        self.incus_run_within(
            &[
                "storage",
                "volume",
                "set",
                &pool,
                &name,
                &format!("size={target}GiB"),
            ],
            QUICK_LIMIT,
        )?;
        println!("Incus volume {name} grown from {current} to {target} GiB");
        Ok(Some(target))
    }

    /// Delete the container and create it again; the volume stays, and
    /// the next start provisions the fresh root.
    pub(in crate::vm) fn incus_reset(&self) -> Result<()> {
        let inst = self.incus_instance()?;
        if let Some(i) = &inst {
            self.incus_check_instance(i)?;
        }
        let pool = self.incus_data_pool(inst.as_ref())?;
        let name = self.incus_name();
        // The new container mounts the volume; without it there is no
        // container to make, so the old one is not deleted for nothing.
        let vol = self.incus_volume(&pool)?.with_context(|| {
            format!("Incus volume {name} is not in pool {pool}; `ssf vm build` makes it")
        })?;
        self.incus_check_volume(&pool, &vol)?;
        if inst.is_some() {
            self.incus_run_within(&["delete", "-f", &name], QUICK_LIMIT)?;
        }
        self.incus_create(&pool)?;
        println!(
            "Incus container {name} re-created; the volume {name} stays; `ssf vm start` provisions it again (a few minutes)"
        );
        Ok(())
    }

    /// Delete the container and the volume. `Ok(false)`: Incus had
    /// neither.
    pub(in crate::vm) fn incus_destroy(&self) -> Result<bool> {
        let name = self.incus_name();
        let inst = self.incus_instance()?;
        if let Some(i) = &inst {
            self.incus_check_instance(i)?;
        }
        // The container's own device names the pool; without a container
        // every pool is searched, so a volume in a pool the default
        // profile no longer uses is not reported as gone.
        let volumes = match inst.as_ref().and_then(Instance::data_pool) {
            Some(p) => self
                .incus_volume(p)?
                .map(|v| (p.to_string(), v))
                .into_iter()
                .collect(),
            None => self.incus_volumes_anywhere()?,
        };
        // Every owner checked before anything is deleted.
        for (pool, v) in &volumes {
            self.incus_check_volume(pool, v)?;
        }
        let mut removed = false;
        if inst.is_some() {
            self.incus_run_within(&["delete", "-f", &name], QUICK_LIMIT)?;
            println!("deleted Incus container {name}");
            removed = true;
        }
        for (pool, _) in &volumes {
            self.incus_run_within(&["storage", "volume", "delete", pool, &name], QUICK_LIMIT)?;
            println!("deleted Incus volume {name} in pool {pool}");
            removed = true;
        }
        Ok(removed)
    }

    /// What Incus holds of this VM, for `ssf uninstall`. A daemon that
    /// cannot be asked is not "nothing there".
    pub(in crate::vm) fn incus_survey(&self) -> Survey {
        let dir = self.dir.exists();
        let unknown = |what: &str, e: anyhow::Error| {
            warn!("could not ask Incus about {what}: {e:#}");
            Survey {
                present: dir.then_some(true),
                running: None,
                startable: false,
                data: None,
            }
        };
        let instance = match self.incus_instance_within(PROBE_LIMIT) {
            Ok(i) => i,
            Err(e) => return unknown("the container", e),
        };
        let data = match self.incus_volumes_anywhere() {
            Ok(v) => !v.is_empty(),
            Err(e) => return unknown("the data volume", e),
        };
        Survey {
            present: Some(instance.is_some() || data || dir),
            running: Some(instance.as_ref().is_some_and(Instance::is_running)),
            startable: instance.is_some(),
            data: Some(data),
        }
    }

    /// The volume's size cap, else what a build would make.
    pub(in crate::vm) fn incus_data_cap_gib(&self) -> u32 {
        self.incus_instance()
            .and_then(|i| self.incus_data_pool(i.as_ref()))
            .and_then(|p| self.incus_volume(&p))
            .ok()
            .flatten()
            .and_then(|v| v.size_bytes())
            .map_or_else(|| self.sizes().data_gib, gib_ceil)
    }

    /// The container's console log, written next to the VM's files so
    /// `ssf vm console` can tail it.
    pub(in crate::vm) fn incus_console_log(&self) -> Result<PathBuf> {
        let name = self.incus_name();
        let log = self.incus_output_within(&["console", &name, "--show-log"], PROBE_LIMIT)?;
        std::fs::create_dir_all(&self.dir)?;
        let path = self.console_log();
        std::fs::write(&path, log).with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests;
