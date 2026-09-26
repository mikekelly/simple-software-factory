# Inside a VM

For an agent running or repairing a factory that keeps the whole thing inside
a VM: how `ssf vm` builds the guest, sizes it, what is installed there, how to
reach it and what persists. Offline: `ssf skill vm`.

Without a VM, everything runs on the machine as the person who started it: the
agents can read that home directory, its keyring and whatever else is open.
`ssf vm` moves the factory (the daemon, herdr and every agent session) into a
guest and leaves the host only what builds, starts, stops and reaches it. The
guest runs the same herdr-based stack as host mode. Agents in the guest only
need to know they have `sudo` there, which their first prompt says.

## Quickstart

```sh
ssf setup                 # once: creates the public VM target `ssf-server`
ssf vm build              # once: sizes, makes and provisions the guest (a few minutes)
ssf vm status             # whether it runs, its daemon answers, its size, which harnesses are signed in
ssf vm login              # sign a harness in inside the guest
ssf status                # runs inside the guest from now on
ssf vm attach             # herdr in the guest, in this terminal
```

A good `ssf vm build` ends with the sizes it chose, the backend it picked and
one line per harness CLI installed. If it stops on missing host tooling, see
[Backends and host prerequisites](#backends-and-host-prerequisites); if it
stops later, `ssf vm console` and
[troubleshooting.md](troubleshooting.md#vm-and-host) have the remedies.

Several catalog-owned VMs run independently under the same selector:

```sh
ssf server add crucible --vm
ssf --server crucible vm build
ssf --server crucible vm start
ssf --server ssf-server status
```

They need unique runtime names, absolute non-overlapping `[vm] dir`
directories and distinct SSH ports (Firecracker also uses the adjacent port
while building, so leave ten ports between targets). Each can have its own
supervisor: `ssf --server crucible ui service enable`. See the
[server catalog](configuration.md#server-catalog) for a complete example.

## Backends and host prerequisites

`[vm] backend` is `firecracker`, `lima` or `incus`. Unset, it means
Firecracker on Linux and lima on macOS; `ssf vm build` writes the choice next
to the sizes, so a VM keeps its backend once built, and ssf never switches it
by itself. `ssf vm status` names it on its `backend:` line and in `--json`, and
reports host tooling on a `tooling:` line saying where each tool was found or
what is missing. A Firecracker build on anything but Linux x86_64 refuses and
says to set `vm.backend` to `lima` (or `incus` on Linux without KVM).

**`incus` is not a VM.** Its guest is an unprivileged Incus system container:
it shares the host's kernel, isolated by user namespaces, which is weaker than
the KVM or qemu boundary of the other two. `ssf vm status` says so on its
`backend:` line and as `"shares_host_kernel": true` in `--json`. It exists for
Linux hosts without KVM (most cloud VPSes), where lima's qemu falls back to
software emulation and runs agent builds about 30 times slower; under Incus they
run at near-native speed, Docker included.

| backend | where | host needs |
|---|---|---|
| firecracker | Linux x86_64 with `/dev/kvm` | `/dev/kvm` readable and writable by the user running ssf, `fakeroot`, `bsdtar` (libarchive), `mkfs.ext4`, `e2fsck`, `debugfs`, `resize2fs` (e2fsprogs), `curl`, `openssh`, and a `herdr` binary, which is copied into the image |
| lima | macOS, and Linux without KVM or on aarch64 | `limactl` (lima 2.0.1 or newer), `openssh`, `gh` (to fetch the guest's Linux `ssf` on a Mac), and `qemu-system-<arch>` wherever qemu drives the VM: always on Linux, and on a Mac only with `[vm] vm_type = "qemu"` |
| incus | Linux without KVM (a container sharing the host kernel) | `incus` with its daemon running and reachable by this user (the `incus-admin` group), a storage pool and network from `incus admin init`, a kernel with idmapped mounts (5.12 or newer), and `openssh`. ssf never sets Incus up: see [platform-specifics.md](platform-specifics.md#linux-without-kvm) |

Package names per distribution and the macOS Homebrew route are in
[platform-specifics.md](platform-specifics.md#linux-without-kvm) and
[platform-specifics.md#macos](platform-specifics.md#macos).

Nothing needs root with Firecracker or lima; Incus needs root once, to be
installed and set up, and then runs as your user through its daemon. Firecracker runs as the user given
`/dev/kvm`; the jailer is not used, so isolation is KVM plus Firecracker's
seccomp filter. Its guest network is
[gvisor-tap-vsock](https://github.com/containers/gvisor-tap-vsock), a user-mode
TCP/IP stack on the host end of a vsock, so no tap, bridge or firewall rule is
needed. lima runs Apple's Virtualization framework (`vz`, macOS 13.5 or later,
Apple silicon and Intel both) or qemu as the same user; qemu is slower than
Firecracker. `[vm] vm_type` (`vz` or `qemu`) passes through to lima and `vz` is
refused on Linux.

`ssf vm build` refuses a lima older than 2.0.1 by name rather than failing at
the first boot: the template's base-image locator, the `_images` templates and
the share's default mount type all need it. A `limactl` whose version cannot be
read is let through with a warning.

Where things live on the host: `[vm] dir` (`~/.local/share/ssf/vm`) holds one
directory per `name`, with `lima.yaml` or the Firecracker disks, `share/` (the
guest scripts, the seed tree and a herdr binary when the host has one for the
guest), the ssh key and `known_hosts`, and `guest-bin/` for a downloaded guest
binary. `share/` is the only host directory a lima guest sees, mounted
read-only at `/mnt/ssf`. A lima instance itself lives in lima's own home
(`~/.lima`, or `$LIMA_HOME`) as `ssf-<vm.name>`, with its data disk
`ssf-<vm.name>` under `_disks`; because lima labels that disk's filesystem
`lima-ssf-<name>` and an ext4 label holds 16 characters, `vm.name` is at most
7 characters under this backend. Under incus the container and its data
volume are both `ssf-<vm.name>`, the volume in the storage pool the default
Incus profile uses; the container is created from `images:ubuntu/24.04` (or
`vm.image`) with `security.nesting=true` and the `mknod` and `setxattr` syscall
intercepts, so Docker works inside it. `share/` is mounted read-only (and
idmapped) at `/mnt/ssf`, the volume at `/var/lib/ssf`, and a proxy device
publishes the container's sshd on `127.0.0.1:<ssh_port>`. Incus names are per host Incus
daemon, not per user: two host users with the same `vm.name` would collide on
`ssf-<vm.name>`. ssf marks the container and the volume it builds with
`user.ssf.owner=<uid>`, and `build`, `start`, `reset`, `grow`, `destroy` and
`ssf uninstall` refuse an `ssf-<vm.name>` container or volume whose owner key
is missing or names another uid; give each user a distinct `vm.name`. Reset,
grow and destroy find the volume in the pool the container's data device
names (destroy without a container searches every pool), so a later change of
the default profile's pool does not lose track of it.

All `[vm]` keys (`backend`, `name`, `dir`, `vcpus`, `mem_mib`, `data_gib`,
`root_gib`, `ssh_port`, `files`, `guest_binary`, and the binaries and images to
use instead of the downloaded ones) are in the
[configuration table](configuration.md#every-key). `guest_binary` belongs to
neither backend: both seed a Linux `ssf` into the guest.

### What the commands do

| command | Firecracker | lima | incus |
|---|---|---|---|
| `build` | downloads Firecracker, gvproxy, a kernel and a pinned Ubuntu 24.04 minimal-cloud root tarball into `vm.dir`, verifies SHA-256, makes the root image and boots it once to provision | writes `lima.yaml` and `share/`, creates the data disk if absent, `limactl create`, then a first start during which the guest provisions itself; stops the instance afterwards | writes `share/`, creates the data volume if absent, `incus init` and the devices, then a first start in which `incus exec` provisions the guest with the lima guest scripts; stops the container afterwards |
| `start` | boots the microVM, waits for ssh and the guest daemon | writes `share/` fresh, `limactl start`, waits for the provisioning marker, ssh and the daemon | writes `share/` fresh, applies `limits.cpu`/`limits.memory`, `incus start`, provisions a reset container, waits for ssh and the daemon |
| `stop` | Ctrl-Alt-Del through Firecracker's API | `limactl stop`, then `limactl stop -f` if that fails | `incus stop`, then `incus stop --force` if that fails |
| `grow` | `e2fsck -f`, lengthens the file, `resize2fs` | `limactl disk resize`; the guest grows the filesystem at its next boot | sets the volume's `size` (a pool that cannot cap volumes leaves them uncapped, and there is nothing to grow) |
| `reset` | the root disk remade from a rebuilt image | `limactl delete` and `limactl create`; provisioned again at the next start | `incus delete` and a new container on the same volume; provisioned again at the next start |
| `destroy --yes` | the disks and `<vm.dir>/<name>/` | the instance, the data disk and `<vm.dir>/<name>/`; the confirmation names all three | the container, the volume and `<vm.dir>/<name>/` |
| `console` | the serial console log | the instance's `serial.log` in lima's instance directory | `incus console --show-log`, saved to `<vm.dir>/<name>/console.log` |

`status`, `ssh`, `attach`, `login`, `logs`, `run`, `restart` and `ssh-config`
work the same under all three. A lima data disk is formatted only by the build that
creates it; if a build fails and leaves the format flag or a blank disk behind,
see
[troubleshooting.md](troubleshooting.md#lima-disk-unproven-and-the-format-flag).

## Size

A factory running several sessions at once needs most of the machine, so the VM
is sized from the machine rather than from constants. `ssf vm build` reads the
host, fills in every size key left unset in `[vm]`, prints what it chose and
where each value came from, and writes the values where they stay visible and
editable.

| key | rule | floor |
|---|---|---|
| `vcpus` | the host's logical CPUs minus one | 2 |
| `mem_mib` | half the host's RAM, rounded down to 256 MiB | 4096 |
| `data_gib` | half the free space, at build time, of the filesystem that will hold the data disk | 20 |
| `root_gib` | the system only: 8 under Firecracker | 20 under lima, whatever the key says |

Which filesystem is measured depends on the backend: `vm.dir` under
Firecracker, lima's own disk directory (`$LIMA_HOME/_disks`, by default
`~/.lima/_disks`) under lima, which is often another volume. The build names it
in the line above its choice:

```
this machine: 8 CPUs, 32768 MiB RAM, 155 GiB free on /home (measured at /home/you/.lima/_disks, lima's disk directory)
```

A value set by hand always wins, and `ssf vm build --vcpus N --mem-mib N
--data-gib N` writes the given value instead of the rule. A build over an
existing image or instance still does the sizing. With the keys unset and no
build run, `ssf vm start` applies the rule at each start without writing it.

Rule of thumb per parallel session: about one vCPU and 2 GiB of RAM per active
session, plus, on the data disk, one clone per repository and a build tree per
worktree. Change `vcpus` or `mem_mib` and run `ssf vm restart`; under lima the
start hands the changed size to the stopped instance, so no rebuild is needed.

The data disk is sparse: its size is a cap, not a reservation, and it takes
host space only as the guest writes. A cap above the host's free space is a bad
idea, since a host that fills up shows in the guest as I/O errors rather than
"disk full"; that is why the rule takes half the free space and `grow` warns
past it.

`ssf vm grow [--data-gib N]` enlarges an existing data disk without losing what
is on it, then writes the new `data_gib`. Without `N` it grows to the rule for
today's free space. It never shrinks; a smaller disk means a new VM. The VM
must be stopped, and when the service owns it, stop the service first:

```sh
ssf ui service disable               # add --server NAME when several targets exist
ssf vm grow                          # or: ssf vm grow --data-gib 200
ssf ui service enable                # or `ssf vm start` for a VM operated by hand
```

`ssf vm status` shows the sizes and, with the guest reachable, the data disk's
use against its cap. `ssf doctor`, which runs inside the guest, fails its
data-disk line at 85 % used and its memory line when the guest has under a
tenth of its memory available.

## The image and what is provisioned

Under Firecracker, `ssf vm build` unpacks a pinned Ubuntu 24.04 LTS
minimal-cloud root, adds the guest scripts and units, turns the tree into an
ext4 image and boots it once with a provisioning init. Under lima there is no
image step: the instance boots a stock cloud image and lima runs
`/mnt/ssf/guest/lima-boot.sh` as root at every boot, which on the first boot
runs the same provisioning script, logs it to `/var/log/ssf-provision.log` and
writes `/etc/ssf-image-built` on success. `ssf vm build` waits for that marker
and prints the end of the log if provisioning fails. The host's own
distribution never determines the guest's.

Under lima the guest OS follows the architecture: on x86_64 the Arch Linux
cloud image, on aarch64 Ubuntu LTS, because Arch has no official aarch64 cloud
image. `[vm] image` names a cloud-init image of your own instead; it has to be
Arch or Debian/Ubuntu, since the provisioning script installs with `pacman` or
`apt-get`.

Either way the script upgrades the base and installs `openssh`, `sudo`, `git`,
a current GitHub CLI from its own apt repository (`cli.github.com/packages`;
Ubuntu's own `gh` is too old), a pinned upstream Node.js LTS with npm, `tmux`, the harness
CLIs from `ssf agents` that npm or a release tarball provide (each best effort
and listed at the end of the build), an `ssf` user that is root through `sudo`,
herdr, and herdr's agent integrations for the agents present. The list lives in
`vm/guest/provision.sh` (`/usr/share/ssf/vm/` when installed on Linux,
`$(brew --prefix)/share/ssf/vm/` on macOS). To change it, copy that directory,
edit the copy and run `SSF_VM_DIR=<copy> ssf vm build --force`; a file edited
under the installed directory is overwritten by the next package upgrade.

The `ssf` binary is not in the image: every start takes the host's, so the
guest always runs the installed version. herdr is installed when the guest is
provisioned, so a newer host herdr reaches an existing guest through
`ssf vm reset`, not through a restart. On a Mac the host's binaries cannot run
in the Linux guest: the guest's `ssf` is `[vm] guest_binary` when set (its
matching `ssf-server` must sit beside it), otherwise both release assets for
this version and architecture are downloaded once with `gh release download`
into `guest-bin/` and reused at every start. `[vm] herdr` pins a Linux herdr
the same way; with none set and no host herdr, the guest downloads herdr's
latest Linux release while it provisions itself.

## What gets in, and what does not

At every start the host supplies the `ssf` binary, the SSH public key used to
administer the guest, and explicitly selected `vm.files` (a seed disk under
Firecracker, the read-only `seed/` share under lima). Routine starts do not
copy factory configuration, bot credentials or signing keys. No other host home
files, keyring, SSH configuration or git configuration are visible in the
guest.

The guest owns repository configuration, harness/model/effort choices, daemon
policy, allowed users, GitHub bot authentication and git identity. Run
`ssf auth login --web` after starting and enabling the VM: the device flow runs
in the guest and prints a URL and code to approve in a browser, signed in as
the bot. Credentials and the generated signing key stay on the guest data disk,
and `ssf auth status` inspects that guest account. Personal git credentials and
signing keys must be provisioned in the guest too; see
[Committing as a person](identity-and-bylines.md#committing-as-a-person-while-gh-stays-the-bot).

`vm.files` is an explicit file import (`src` or `src:dest`). Do not use it to
maintain a second factory config or to overwrite guest credentials, and read
[Harness logins](#harness-logins) before importing a harness login with it.

## What the agent can do there

The `ssf` user has passwordless `sudo` for everything (`vm/guest/sudoers`), so
an agent in the guest installs packages, adds tools, edits the units, restarts
services and reboots as it sees fit. The first prompt and `ssf guide` say so
with one line inside the VM (`SSF_VM_GUEST=1` in the guest's environment is how
ssf knows) and say nothing on bare metal, where the agent has whatever the
person running ssf has. The VM is the isolation boundary: whatever the agent
does stays on that VM's two disks, and `ssf vm reset` or `ssf vm destroy` puts
it back.

## Harness logins

The agents in the guest need their own sign-in: the guest has no keyring, no
browser and none of the host's home directory. `ssf vm login [<harness>]` runs
the harness inside the guest, in this terminal, from the projects root
(`/var/lib/ssf/projects`). Where the harness has an interactive sign-in, that
is its own TUI: sign in, clear whatever it shows next (theme, onboarding, the
bypass-permissions warning, folder trust), then quit it. Folder trust accepted
in the projects root is meant to cover the worktrees underneath, so a session's
first prompt does not meet a first-run screen. Sign-in uses the flow that works
without a browser next to it: a page to open here and a code to paste back, a
device code, or, for Oh My Pi, a loopback OAuth callback that ssf forwards over
SSH while the login runs. ssf opens the page in the host browser
when it can and prints the URL either way, then says whether the credential
landed. Without a harness it lists those installed in the guest and asks which.
Nothing is copied from this machine.

| harness | what runs in the guest | the person does | credential (guest home) |
|---|---|---|---|
| claude | `claude` | sign in (open the URL, paste the code back), clear its first-run screens, `/exit` | `.claude/.credentials.json` |
| codex | `codex` | pick Sign in with Device Code, enter the code on the page, clear its first-run screens, `/quit` | `.codex/auth.json` |
| gemini | `NO_BROWSER=true gemini` | pick a method, open the URL, paste the code, clear its first-run screens, `/quit` | `.gemini/oauth_creds.json` |
| copilot | `copilot`, then `/login` | enter the code on the page, clear its first-run screens, `/exit` | `.copilot/config.json` |
| opencode | `opencode auth login` | pick provider and method; OAuth prints a URL and takes the code | `.local/share/opencode/auth.json` |
| pi | `pi`, then `/login` | pick method and provider, open the URL, paste the code or redirect URL | `.pi/agent/auth.json` |
| omp | `omp`, then `/login` | pick provider and method, open the loopback `/launch` URL, finish in the host browser, then exit | `.omp/agent/agent.db` |
| grok | `grok login --device-auth` | confirm the code on the page | `.grok/auth.json` |
| crush | `crush login copilot` | Enter, then the code on the page | `.config/github-copilot/apps.json` |

Run the OMP login on the computer running the browser: ssf detects the
`http://localhost:<port>/launch` link and forwards that port from host loopback
to guest loopback, binding only `127.0.0.1` and `::1`. If either cannot be
bound, ssf stops the login with an error; free the port and retry. The tunnel
closes when the login command exits, and ssf forwards bytes without logging or
saving callback URLs, codes or tokens. The same commands take API keys where a
harness offers that, as does the harness's environment variable.

The credential stays in the guest home on the data disk: `ssf vm reset` keeps
it, `ssf vm destroy` removes it. `ssf vm status` shows one entry per harness
(`logins:`, and `logins` in `--json` with `installed` and `logged_in`), and
`ssf doctor` has one line per harness the repositories use.

A credential copied in with `[vm] files` is the same session as the one on the
host, not a second login: a logout on either side, or Claude Code's token
rotation on expiry, ends both, and a guest agent that runs `claude auth logout`
signs the person out on the host. `ssf vm login` gives the guest a login of its
own and never logs anything out.

When a login expires or is revoked under a running session, ssf notices (the
session shows as blocked and its item gets one comment naming
`ssf vm login <harness>`), holds its activity, and resumes on its own once the
guest is signed in again; see
[sessions.md](sessions.md#a-harness-that-is-not-signed-in). The first sign-in
during an install is step 8 of [install.md](install.md#8-sign-in-the-harness).

## Reaching it

The guest's sshd is published on `127.0.0.1:<vm.ssh_port>` (by gvproxy under
Firecracker, by lima's port forwarding under lima), keyed per VM. With
`vm.enabled`, repository, factory configuration and bot authentication
commands run in the guest, as do the daemon commands (`status`, `peers`, `sub`,
`unsub`, `subs`, `handover`, `release`, `purge`, `doctor`, `run --once`).
`ssf config get|set vm.<key>` and `ssf vm ...` stay on the host.

| to do this | run |
|---|---|
| a shell in the guest, or one command there | `ssf vm ssh [-- cmd]` |
| an `ssf` command in the guest explicitly | `ssf vm run -- status --json` |
| herdr's session, in this terminal | `ssf vm attach` |
| the guest daemon's journal | `ssf vm logs` |
| kernel and systemd messages | `ssf vm console` |
| an `~/.ssh/config` entry, for `herdr --remote ssf-<name>` and plain `ssh` | `ssf vm ssh-config` |

Under lima, `limactl shell ssf-<name>` is a second way in, as lima's user
rather than `ssf`.

A stopped or unreachable guest produces an error naming the VM; it never falls
back to editing host factory settings. Start it with `ssf vm start` and retry.
Where ssf cannot tell whether the VM is up (a `limactl` that fails or does not
answer within fifteen seconds), it says so on stderr, forwards the command
anyway and repeats the `ssf vm start` advice. `ssf status --json` always
answers: where the guest answers, that answer is passed through untouched with
its exit status; where it does not, the host writes its own document (with `vm`
as `running`, `stopped` or `unknown`, and no sessions or repositories, which
are the guest's to know) and exits 0. Empty repository and session arrays from
an unreachable factory are unavailable data, not a claim that the guest has
none.

With VM mode disabled, `ssf doctor` reports host backend tooling as a note;
with it enabled, `ssf doctor` inspects the guest factory over SSH while
`ssf vm status` reports host tooling and VM health.

### Optional Tailscale enrolment

`ssf vm tailscale` enrols the running guest in a tailnet; see
[platform-specifics.md#tailscale](platform-specifics.md#tailscale).

## Persistence

Each VM has a disposable root and a persistent data disk. Under Firecracker
they are `root.ext4` (a copy-on-write copy of the image) and `data.ext4`
(`vm.data_gib`, sparse) under `<vm.dir>/<name>/`; under lima they are the
instance's root disk in lima's home and the lima disk `ssf-<name>`, which the
guest's seed unit bind-mounts on `/var/lib/ssf` at every boot. Either way
`/var/lib/ssf` holds everything that matters: ssf's state, the clones and the
worktrees, and the guest user's home (factory config, bot token and signing
key, git identity, harness logins, herdr's session state, transcripts and
caches), bind-mounted from there.

`ssf vm stop` shuts the guest down cleanly; on the next start the guest
daemon's `resume_on_start` brings the sessions back in herdr, as after a reboot
on bare metal. Factory edits go straight to the guest and are picked up on its
next poll. `ssf vm restart` supplies a new binary or explicit `vm.files` and
preserves guest configuration and credentials. `ssf vm reset` gives the guest a
fresh root and keeps the data disk, so sessions survive it (the guest home is
copied from the image only when the data disk is new, so a rebuilt image's
hooks reach an existing VM only through `ssf vm destroy`, or by hand).
`ssf vm destroy --yes` removes the VM and all its disks.

Firecracker and gvproxy are started in a session of their own, and lima's host
agent runs detached, so a VM started with `ssf vm start` outlives that shell.
The service is different: `ssf-server` starts the VM if it is not up and owns
it from then on, so stopping or restarting the service shuts the guest down
cleanly. Moving an older installation's VM into the server catalog, and
recovering an incompatible root, are in
[platform-specifics.md#upgrading-from-an-older-ssf](platform-specifics.md#upgrading-from-an-older-ssf).
