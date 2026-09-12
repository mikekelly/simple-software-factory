//! The factory inside a VM: the daemon, herdr and every agent session run
//! in a guest, and the host keeps only what builds, starts, stops and
//! reaches it (`ssf vm ...`). Two backends (`[vm] backend`) run the guest:
//!
//! * Firecracker (the default on Linux; `firecracker.rs`). Nothing needs root:
//!   Firecracker runs as the user given `/dev/kvm`, the guest's network is
//!   gvisor-tap-vsock (`gvproxy` on the host, a user-mode TCP/IP stack on
//!   the unix socket Firecracker maps to guest vsock port 1024;
//!   `gvforwarder` in the guest), and the images are made with `fakeroot`
//!   and `mkfs.ext4 -d`. Files under `[vm] dir` (`~/.local/share/ssf/vm`):
//!   the downloaded `firecracker`, `gvproxy`, `gvforwarder` and `vmlinux`,
//!   the root image `rootfs.ext4` that `ssf vm build` provisions from an
//!   Ubuntu 24.04 LTS minimal cloud root, and one directory per VM with
//!   its persistent `root.ext4` (a copy-on-write copy of the image),
//!   `data.ext4` (ssf's
//!   state, the clones and worktrees, mounted at `/var/lib/ssf`), the
//!   `seed.ext4` written at every start (this binary, bootstrap defaults,
//!   host access public key and `[vm] files`; legacy state only during migration),
//!   Firecracker's config and sockets, PID files and the serial console log.
//! * lima (the default on macOS; `lima.rs`): a `limactl` instance from a
//!   cloud image, provisioned by the same guest scripts on its first boot
//!   and seeded from a read-only host directory at every boot.
//!
//! Shared types and sizing rules live in `types.rs`, guest operations
//! (seeding, SSH, harness logins, `sync`, `attach`, and `logs`) in
//! `guest.rs`, and host utilities in `support.rs`. The guest is reached over
//! ssh on `127.0.0.1:<ssh_port>`
//! with a key made per VM. With `[vm] enabled = true` the daemon-facing
//! commands are run inside the guest that way, so `ssf status --json` for
//! the bar widget and `ssf tell` from a terminal work as before; `ssf-server`
//! on the host starts the VM and watches it, so the service is unchanged.

mod lima;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use std::io::Write;
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
/// A dated Ubuntu 24.04 LTS minimal cloud root. Pinning the released build
/// keeps a clean build reproducible; apt upgrades it during provisioning.
pub const UBUNTU_ROOT_URL: &str = "https://cloud-images.ubuntu.com/minimal/releases/noble/\
release-20260905/ubuntu-24.04-minimal-cloudimg-amd64-root.tar.xz";
pub const UBUNTU_ROOT_SHA256: &str =
    "094dc0afc6ded1c3e5ce71f7d0b48d5db922155097bc8fb1ec19db2ebdd17ece";

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
pub const FORWARDED: [&str; 17] = [
    "status", "peers", "sub", "unsub", "subs", "tell", "release", "handover", "purge", "doctor",
    "run", "repo", "config", "auth", "token", "agents", "models",
];

mod firecracker;
mod guest;
mod support;
mod types;

pub use support::*;
pub use types::*;

#[cfg(test)]
mod tests;
