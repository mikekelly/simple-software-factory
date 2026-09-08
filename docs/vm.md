# Inside a VM

How `ssf vm` runs the whole factory inside a VM (a Firecracker microVM on Linux, a lima instance on macOS), what gets into the guest, how to reach it and what persists. For whoever wants the agents kept off their own machine; agents only need to know they have `sudo` there, which their first prompt says.

Without it, everything runs on your machine as you: the agents can read
your home directory, your keyring and whatever else you have open.
`ssf vm` moves the whole factory (the daemon, herdr and every agent
session) into a VM, and leaves the host only what builds, starts, stops
and reaches the guest. The drivers are untouched; the guest runs herdr
(Orca is a desktop app and needs a display the guest does not have, so a
repository that says `driver = "orca"` runs in herdr there). Two backends
run the guest, chosen by `[vm] backend` (see [Backends](#backends)):
[Firecracker](https://firecracker-microvm.github.io/), the original, on
Linux with KVM, and [lima](https://lima-vm.io), on macOS and on Linux
with qemu. Nothing needs root with either: Firecracker runs as you given
`/dev/kvm`, lima runs Apple's Virtualization framework or qemu as you,
and the images are made or provisioned without it. The guest is the same
either way: the same scripts provision it, the same units run in it, and
the same `ssf vm` commands drive it.

```sh
ssf vm build              # once: makes and provisions the guest (a few minutes); picks the backend for this machine
ssf config set vm.enabled true
systemctl --user restart ssf.service   # macOS: brew services start ssf; or, by hand: ssf vm start
ssf vm status             # whether the VM runs and its daemon answers, and which harnesses are logged in there
ssf vm login              # sign a harness in inside the guest (see below)
ssf status                # runs inside the guest from now on
ssf vm attach             # herdr in the guest, in this terminal
```

The `[vm]` keys (`backend`, `name`, `dir`, `vcpus`, `mem_mib`,
`data_gib`, `root_gib`, `ssh_port`, `files`, and the binaries and images
to use instead of the downloaded ones: `firecracker`, `gvproxy`,
`kernel`, `rootfs` for Firecracker; `limactl`, `image`, `vm_type`,
`guest_binary`, `herdr` for lima) are in the
[configuration table](configuration.md#every-key).

## Backends

`[vm] backend` is `firecracker` or `lima`. Unset, it means Firecracker on
Linux and lima on macOS, and `ssf vm build` writes the choice to
`config.toml` next to the sizes (`VM backend: lima (for this machine)` in
its output), so a VM keeps its backend once built. `ssf vm status` names
it on its `backend:` line, and `--json` carries it as `backend` next to
`instance` and `lima_dir` (the lima instance's name and the directory lima
keeps it in; both null under Firecracker). `ssf doctor` on the host has a
line for the backend's own tooling: `limactl`, and `qemu-system-<arch>` on
Linux, under lima; `/dev/kvm` under Firecracker. A Firecracker build on
anything but Linux x86_64 refuses and says to set `vm.backend` to `lima`.
Either way, `[vm] dir` (`~/.local/share/ssf/vm`) holds what ssf keeps for
the VM, one directory per `name` under it.

### Firecracker (Linux)

The guest is a [Firecracker](https://firecracker-microvm.github.io/)
microVM: Firecracker runs as you given `/dev/kvm` (world-writable on
Omarchy, Arch and Fedora; on Debian and Ubuntu a user logged in at the
machine's seat gets access through udev, and a user who only comes in
over ssh needs the `kvm` group: `sudo usermod -aG kvm $USER` and a new
login), the guest's network is
[gvisor-tap-vsock](https://github.com/containers/gvisor-tap-vsock) (a
user-mode TCP/IP stack on the host end of a vsock, so no tap, bridge or
firewall rule on the host), and the images are made with `fakeroot` and
`mkfs.ext4 -d`. The jailer is not used (it needs root); isolation is KVM
plus Firecracker's seccomp filter. It is x86_64 only: the Firecracker,
gvproxy and kernel binaries `ssf vm build` downloads are built for it.

The host needs `/dev/kvm` usable by you, `fakeroot`, `bsdtar`
(libarchive), `mkfs.ext4`, `e2fsck` and `resize2fs` (e2fsprogs), `curl`,
`openssh`, and its own `herdr` binary, which is copied into the image.
All of that is on a stock Omarchy; elsewhere `sudo pacman -S --needed
fakeroot libarchive e2fsprogs curl openssh`, `sudo apt install fakeroot
libarchive-tools e2fsprogs curl openssh-client` or `sudo dnf install
fakeroot bsdtar e2fsprogs curl openssh-clients` (the .deb and .rpm
recommend them, so apt and dnf bring them with the package).
`ssf vm build` downloads Firecracker, gvproxy and a guest kernel into
`vm.dir` and makes the root image there; each VM's disks are files under
`<vm.dir>/<name>/`.

### lima (macOS, and Linux with qemu)

The guest is a [lima](https://lima-vm.io) instance, driven with
`limactl`. On macOS lima uses Apple's Virtualization framework (`vz`,
macOS 13.5 or later; Apple silicon and Intel both work, and nested
virtualisation is not needed); on Linux it uses qemu, so it is the way
to run the VM on a machine without `/dev/kvm` or on aarch64, slower than
Firecracker. Nothing runs as root.

The host needs `limactl` (`brew install lima` on macOS, which the
Homebrew `ssf` formula pulls in; the `lima` package or lima's release
tarball on Linux, or `[vm] limactl` pointing at one elsewhere), `gh`
(the GitHub CLI, to fetch the guest's `ssf` binary on a Mac; see below),
`openssh`, and on Linux `qemu-system-x86_64` or `qemu-system-aarch64`
for the machine's architecture (Arch: `qemu-full` or `qemu-base`;
Debian and Ubuntu: `qemu-system-x86` or `qemu-system-arm`; Fedora:
`qemu-system-x86` or `qemu-system-aarch64`).
`ssf vm build` checks for them first and names what is missing, as does
the backend line of `ssf doctor` on the host.

Two places hold a lima VM. The instance itself is lima's, named
`ssf-<vm.name>` (`ssf-default`) in lima's own home (`~/.lima`, or
`$LIMA_HOME`), so lima's tools see it: `limactl list` shows it and
`limactl shell ssf-default` opens a shell as lima's user. Its root disk
is `vm.root_gib` with a floor of 20 GiB (a cloud image plus node and the
harness CLIs does not fit in the Firecracker image's 8). The data disk
is a lima disk, `ssf-<vm.name>`, of `vm.data_gib` (`limactl disk list`),
which ssf attaches to the instance and which survives `ssf vm reset` and
`ssf vm build --force`. That name is short on purpose: lima labels the
disk's filesystem `lima-<disk>` and an ext4 label holds 16 characters,
which is why `vm.name` is at most 7 characters under this backend. Only
the build that creates the disk writes `format: true` for it in the
template; once the disk exists every build writes `format: false`, so lima
can never reformat a disk that already holds the factory. ssf's own files
are under `<vm.dir>/<name>/`: `lima.yaml` (the template `ssf vm build`
writes from `[vm]`; edit the config and rebuild rather than the file),
`share/` (the guest scripts, the seed tree, and a herdr binary when the
host has one for the guest), the ssh key and `known_hosts` for reaching
the guest, and `guest-bin/` with a downloaded guest binary. `share/` is
the only host directory the guest sees, mounted read-only at `/mnt/ssf`;
nothing else on the host is visible.

The guest OS follows the architecture: on x86_64 the Arch Linux cloud
image (the same distribution the Firecracker image is made from), on
aarch64 (Apple silicon) Ubuntu LTS, because Arch has no official aarch64
cloud image. lima keeps the URL and digest of each. `[vm] image` names a
cloud-init image of your own (a URL or a path) instead; it has to be Arch
or Debian/Ubuntu, since the provisioning script installs with `pacman`
or `apt-get`. `[vm] vm_type` (`vz` or `qemu`) passes straight through to
lima; unset is lima's default for the machine, and `vz` is refused on
Linux.

The guest binaries come from somewhere other than the host on a Mac, as
a macOS `ssf` cannot run in the Linux guest. The guest's `ssf` is `[vm]
guest_binary` when set (a Linux build of your own, for a dev build);
else, on a Linux host, the host's own binary, as under Firecracker;
else the release asset for this version and architecture,
`ssf-<version>-linux-<x86_64|aarch64>`, downloaded once with `gh release
download` into `guest-bin/` and reused at every start. A version with no
such asset on its release fails there, and the message says to build a
Linux binary and set `guest_binary` to it. herdr is `[vm] herdr` when set
(a Linux herdr binary, to pin one); else, on a Linux host, the host's own
`herdr`; else the guest downloads herdr's latest Linux release
(`herdr-linux-<arch>`) while it provisions itself. herdr is installed
when the guest is provisioned, so a newer host herdr reaches an existing
guest through `ssf vm reset`, not through a restart.

What the commands do under lima:

| command | under lima |
|---|---|
| `ssf vm build` | checks for `limactl` (and qemu on Linux), writes `lima.yaml` and `share/`, creates the data disk if it does not exist (`format: true` in the template only on that build; an existing disk is attached with `format: false`), `limactl create`, then a first `limactl start` during which the guest provisions itself (see [The image](#the-image)); waits for that, prints the harness lines, waits for ssh as `ssf`, and stops the instance. With an instance already there it says so and stops; `--force` deletes and re-creates the instance, never the data disk |
| `ssf vm start` | writes `share/` fresh (scripts, seed, herdr), `limactl start`, waits for the provisioning marker (a reset instance provisions itself again here), for ssh and for the guest daemon. Warns when lima forwards ssh to a port other than `vm.ssh_port` (an instance from an older template; `ssf vm build --force` remakes it) |
| `ssf vm stop` | `limactl stop`, and `limactl stop -f` when the clean stop fails |
| `ssf vm grow` | `limactl disk resize` on the data disk, with the VM stopped; the guest grows the filesystem at its next boot (see [Size](#size)) |
| `ssf vm reset` | `limactl delete` and `limactl create` from the template; the data disk stays, and the next start provisions the fresh root again (a few minutes) |
| `ssf vm destroy --yes` | the instance, the data disk and `<vm.dir>/<name>/`; the confirmation names all three |
| `ssf vm console` | the instance's serial console log (`serial.log`, or `serialv.log`, in lima's instance directory) |

Everything else (`status`, `ssh`, `attach`, `login`, `sync`, `logs`,
`run`, `ssh-config`) goes over ssh and works the same under both.

## Size

A factory running several coding sessions at once needs most of the
machine, so the VM is sized from the machine rather than from constants.
`ssf vm build` reads the host (on macOS, `sysctl hw.memsize` for the RAM)
and fills in every size key left unset in `[vm]`, prints what it chose
and where each value came from, and writes the values to `config.toml`,
where they stay visible and editable:

| key | rule | floor |
|---|---|---|
| `vcpus` | the host's logical CPUs minus one | 2 |
| `mem_mib` | half the host's RAM, rounded down to 256 MiB | 4096 |
| `data_gib` | half the free space of the filesystem holding `vm.dir`, at build time | 20 |

`root_gib` stays 8 under Firecracker: the root image only holds the
system. Under lima it is the instance's root disk and is at least 20
whatever the key says. A value set in `[vm]` by hand always wins, and
`ssf vm build --vcpus N --mem-mib N --data-gib N` writes the given value
instead of the rule. A build over an existing image or instance (without
`--force`) still does the sizing, so an installation from before the
rule gets its sizes recorded by running `ssf vm build` once. With the
keys unset and no build run, `ssf vm start` applies the rule at each
start without writing it.

A rule of thumb per parallel session: about one vCPU and 2 GiB of RAM
per active session, plus, on the data disk, the size of one clone per
repository and a build tree per worktree. Change `vcpus` or `mem_mib` in
`config.toml` and `ssf vm restart` to apply them. That is the same under
both backends: `ssf vm start` gives a changed `vcpus` or `mem_mib` to the
stopped lima instance with `limactl edit` before it starts it, so no
rebuild is needed.

The data disk is sparse: its size is a cap, not a reservation, and it
takes host space only as the guest writes. Both backends give the guest
a block device, not a shared directory, and ext4 needs its size when it
is made, so "no cap" means a cap that follows the host and grows later. A
cap above the host's free space is a bad idea: a host that fills up shows
in the guest as I/O errors, not as "disk full", which is why the rule
takes half the free space and `grow` warns past it.

`ssf vm grow [--data-gib N]` enlarges an existing VM's data disk without
losing what is on it. Under Firecracker, with the VM stopped, it checks
the filesystem (`e2fsck -f`), lengthens the file and resizes the
filesystem to fill it (`resize2fs`); under lima it runs `limactl disk
resize`, and the guest's seed script runs `resize2fs` at the next boot,
so the space appears after `ssf vm start`. Then it writes the new
`data_gib`. Without `N` it grows to the rule for today's free space; it
never shrinks (a smaller disk means a new VM). When the service owns the
VM, stop the service first, since it restarts a VM that goes away under
it:

```sh
systemctl --user stop ssf.service    # macOS: brew services stop ssf; or `ssf vm stop` for a VM started by hand
ssf vm grow                          # or: ssf vm grow --data-gib 200
systemctl --user start ssf.service   # macOS: brew services start ssf; or `ssf vm start`
```

`ssf vm status` shows the sizes and, with the guest reachable, the data
disk's use against its cap (`size:` line; `vcpus`, `mem_mib`, `data_gib`
and `data` in `--json`). `ssf doctor`, which runs inside the guest, fails
its data-disk line at 85 % used and its memory line when the guest has
under a tenth of its memory available (or anything swapped out, should
swap be added), each naming what to run (both service commands, since
the guest does not know the host's OS).

## The image

Under Firecracker, `ssf vm build` unpacks the Arch bootstrap tarball,
adds the guest scripts and units, turns the tree into an ext4 image and
boots it once with a provisioning init. Under lima there is no image
step: the instance boots a stock cloud image, and lima runs
`/mnt/ssf/guest/lima-boot.sh` as root at every boot, which on the first
boot (no `/etc/ssf-image-built` yet) runs the same provisioning script
with `SSF_VM_BACKEND=lima`, logs it to `/var/log/ssf-provision.log`, and
writes the marker on success; `ssf vm build` waits for the marker and,
should provisioning fail, prints the end of that log (`limactl shell
ssf-default sudo tail /var/log/ssf-provision.log` shows the rest). Later
boots find the marker and do nothing.

Either way the provisioning script installs `base` (Arch), `openssh`,
`sudo`, `git`, `github-cli`, `nodejs`, `npm`, `tmux`, the harness CLIs
from `ssf agents` that npm or a release tarball provide (Claude Code,
Codex, Gemini, Copilot, OpenCode, Pi, Grok, Crush; each is best effort
and listed at the end of the build), an `ssf` user that is root through
`sudo` (Claude Code refuses its permission-free mode as root, so nothing
runs as root itself), herdr (the host's own binary, or the one described
under [Backends](#backends)) and herdr's agent integrations (its
state-reporting hooks) for the agents present. On the Ubuntu guest the
same list comes through `apt-get`. The list lives in
`vm/guest/provision.sh` (`/usr/share/ssf/vm/` when installed on Linux,
`$(brew --prefix)/share/ssf/vm/` on macOS). To change it, copy that
directory somewhere of your own, edit the copy, and run `SSF_VM_DIR=<copy>
ssf vm build --force` for a new image or instance; a file edited under
the installed directory is overwritten by the next package upgrade. The
`ssf` binary is not in the image: every start takes the host's (or the
release asset under lima on a Mac), so the guest always runs the version
you installed.

## What gets in, and what does not

At every start the host writes a small seed (a disk under Firecracker;
the `seed/` tree of the read-only share under lima, read by the guest's
seed unit at boot) with the `ssf` binary,
`config.toml` rewritten for the guest (`driver = "herdr"`, clones under
`/var/lib/ssf/projects`, no `repo.path`), the bot token (resolved the way
`ssf token` does, so the host keyring itself is never copied), the bot's
own SSH key if `ssf auth login` enrolled one, whatever a `[git]` or
`[repo.git]` table names (a `signing_key` is copied to
`~/.config/ssf/keys/`, a `token:<login>` is resolved from gh here and
written to `~/.config/ssf/git-tokens/<login>`, a `file:` token is copied
there too, and the guest config points at the copies; see [Committing as a
person](identity-and-bylines.md#committing-as-a-person-while-gh-stays-the-bot)),
the ssh public key the host uses to reach the guest, and the files `vm.files` lists
(`files = ["~/.claude/.credentials.json"]` lands at the same place under
the guest user's home; `src:dest` places a file elsewhere; see [Harness
logins](#harness-logins) before copying a login that way). Nothing else
from the host home is visible in the guest: no `~/.ssh`, no
`~/.gitconfig`, no other accounts. Commits are signed only if the bot key
is enrolled or `[git]` names a key. `ssf vm sync` moves `[git]` name and
email into the running guest; a new key or token needs `ssf vm restart`,
which writes the seed again (sync says so when the guest lacks one).

## Harness logins

The agents in the guest need their own sign-in: the guest has no keyring,
no browser and none of your home directory. `ssf vm login [<harness>]`
runs the harness's login inside the guest, in your terminal, using the
flow that works without a browser next to it: a page to open here and a
code to paste back (Claude Code, Gemini, OpenCode, Pi, Oh My Pi) or a
device code (Codex, Copilot, Grok, Crush). ssf opens the page in your
browser when it can (`xdg-open` on Linux, `open` on macOS; the URL is
printed either way) and, once the login
exits, says whether the credential landed. Without a harness it lists
those installed in the guest and asks which. Nothing is copied from this
machine and nothing is port-forwarded; the harnesses' browser-callback
variants are their desktop defaults only. The credential stays in the
guest's home, on the data disk: `ssf vm reset` keeps it, `ssf vm destroy`
removes it with everything else. `ssf vm status` shows one entry per
harness (`logins:`, and `logins` in `--json` with `installed` and
`logged_in`).

| harness | what runs in the guest | you do | credential (guest home) |
|---|---|---|---|
| claude | `claude auth login` | sign in on the page, paste the code back | `.claude/.credentials.json` |
| codex | `codex login --device-auth` | enter the code on the page | `.codex/auth.json` |
| gemini | `NO_BROWSER=true gemini` | pick a method in its dialog, open the URL, paste the code, `/quit` | `.gemini/oauth_creds.json` |
| copilot | `copilot login --device-code` | enter the code on the page | `.copilot/config.json` |
| opencode | `opencode auth login` | pick provider and method; OAuth prints a URL and takes the code | `.local/share/opencode/auth.json` |
| pi | `pi`, then `/login` | pick method and provider, open the URL, paste the code or redirect URL | `.pi/agent/auth.json` |
| omp | `omp`, then `/login` | as Pi | `.omp/agent/auth.json` |
| grok | `grok login --device-auth` | confirm the code on the page | `.grok/auth.json` |
| crush | `crush login copilot` | Enter, then the code on the page | `.config/github-copilot/apps.json` |

Copilot's and Crush's files are what their documentation names; the
others were watched being written. API keys go through the same commands
(each offers the option) or the harness's environment variable.

`[vm] files` remains the way to copy an existing login in, with one
warning: a copied credential is the same session as the one on your
machine, not a second login. A logout on either side, or the token
rotation Claude Code does on expiry, ends both; a guest agent that runs
`claude auth logout` signs you out on the host. `ssf vm login` gives the
guest a login of its own and never logs anything out.

When a login expires or is revoked under a running session, ssf notices
(the session shows as blocked in `ssf status` and the widget, and its
item gets one comment naming `ssf vm login <harness>`), holds its
activity, and resumes the session on its own once the guest is signed in
again (see [A harness that is not signed
in](sessions.md#a-harness-that-is-not-signed-in)). `ssf doctor`, which
runs inside the guest when `[vm] enabled`, has one line per harness the
repositories use saying whether it is signed in there, from the same
table.

## What the agent can do there

The `ssf` user has passwordless `sudo` for everything
(`vm/guest/sudoers`), so an agent in the guest installs packages with
the guest's package manager (`pacman` on the Arch guest, `apt` on the
Ubuntu one), adds tools, edits the units, restarts services and reboots
as it sees fit; the first prompt and `ssf guide` say so with one line
inside the VM (`SSF_VM_GUEST=1` in the guest's environment is how ssf
knows) and say nothing on bare metal, where the agent has whatever the
human running ssf has. The VM is the isolation boundary: whatever the
agent does to the guest stays on that VM's two disks, the host is
untouched, and `ssf vm reset` (a fresh root) or `ssf vm destroy` puts it
back.

## Reaching it

The guest's sshd is published on `127.0.0.1:<vm.ssh_port>` (by gvproxy
under Firecracker, by lima's port forwarding under lima), keyed by a key
made per VM. With `vm.enabled` the commands that talk to the daemon
(`status`, `peers`, `sub`, `unsub`, `subs`, `tell`, `handover`, `release`,
`purge`, `doctor`, `run --once`) run inside the guest over that
connection, so the bar widget, `ssf status --json` and `ssf tell` work as
before; `ssf vm run -- <args>` does it explicitly and `ssf vm ssh
[-- cmd]` gives a shell. `ssf vm attach` attaches to herdr's session in
the guest in your terminal;
`ssf vm ssh-config` prints an `~/.ssh/config` entry so `herdr --remote
ssf-default` (herdr's thin client) and plain `ssh ssf-default` work too.
Clicking a session in the bar widget (Omarchy) opens a terminal attached
to the guest. `ssf vm logs` follows the guest daemon's journal and `ssf
vm console` shows the serial console. Under lima, `limactl shell
ssf-default` is a second way in, as lima's own user rather than `ssf`.

## Persistence

Under Firecracker each VM has, under `<vm.dir>/<name>/`, a `root.ext4` (a
copy-on-write copy of the image: instant on btrfs, a full copy elsewhere)
with the packages, and a `data.ext4` (`vm.data_gib`, sparse; see
[Size](#size)) mounted at `/var/lib/ssf` with everything that matters:
ssf's state, the clones and the worktrees, and the guest user's home
(herdr's session state, the harness transcripts, caches), which is
bind-mounted from there. Under lima the two are the instance's root disk
in lima's home and the lima disk `ssf-<name>` (see
[lima](#lima-macos-and-linux-with-qemu)), which lima mounts at
`/mnt/lima-ssf-<name>` and the guest's seed unit bind-mounts on
`/var/lib/ssf` at every boot, with the same layout on it. `ssf vm stop`
shuts the guest down cleanly (Ctrl-Alt-Del through Firecracker's API;
`limactl stop` under lima); on the next start the guest daemon's own
`resume_on_start` brings the sessions back in herdr, as after a reboot on
bare metal.

Editing the config on the host takes `ssf vm sync` (pushes config and
token and restarts the guest daemon) or `ssf vm restart` (a new seed:
needed for a new `ssf` binary or `vm.files`). `ssf vm reset` gives the
guest a fresh root (Firecracker: the root disk remade from a rebuilt
image; lima: the instance deleted and created again, provisioned on its
next start) and keeps the data disk, so sessions survive it (the guest
home is copied from the image only when the data disk is new, so a
rebuilt image's hooks and `~/.claude.json` reach an existing VM only
through `ssf vm destroy`, or by hand); `ssf vm destroy --yes` removes the
VM and all its disks.

Firecracker and gvproxy are started in a session of their own, and lima's
host agent runs detached too, so a VM started from a terminal (`ssf vm
start`) outlives that shell and any later `ssf` command. The service
(the systemd user unit on Linux, `brew services` on macOS) is different:
`ssf run` starts the VM if it is not up and owns it from then on, so
stopping or restarting the service shuts the guest down cleanly, and a
crash of the host daemon ends it with the service's cgroup on Linux.
With the VM stopped, `ssf status` says so instead of forwarding (the bar
widget shows the service as stopped).
