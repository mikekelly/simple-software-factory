# Inside a VM

For the proposed additional Docker Sandboxes backend, see the
[feasibility report](plans/docker-sandboxes.md). It is not implemented or enabled.

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
`data_gib`, `root_gib`, `ssh_port`, `files`, `guest_binary`, and the
binaries and images to use instead of the downloaded ones:
`firecracker`, `gvproxy`, `kernel`, `rootfs` for Firecracker;
`limactl`, `image`, `vm_type`, `herdr` for lima) are in the
[configuration table](configuration.md#every-key). `guest_binary`
belongs to neither backend: both seed that Linux `ssf` into the guest,
and it is only on a Mac that it cannot be this binary.

## Backends

`[vm] backend` is `firecracker` or `lima`. Unset, it means Firecracker on
Linux and lima on macOS, and `ssf vm build` writes the choice to
`config.toml` next to the sizes (`VM backend: lima (for this machine)` in
its output), so a VM keeps its backend once built. `ssf vm status` names
it on its `backend:` line, and `--json` carries it as `backend` next to
`instance` and `lima_dir` (the lima instance's name and the directory lima
keeps it in; both null under Firecracker). A `limactl list` that does not
answer is reported as such, not as a missing instance: the `instance:` and
`state:` lines say `unknown` and give lima's error. In `--json`, `running`
is `null` rather than `false` when lima could not answer, and `probe_error`
gives the reason; consumers can therefore distinguish an unknown state from
a VM known to be stopped.

The tooling the backend needs on the host is `limactl`, and
`qemu-system-<arch>` wherever lima will drive the VM with qemu (always on
Linux, and on a Mac with `[vm] vm_type = "qemu"`), under lima; a
`/dev/kvm` you can open under Firecracker. `ssf vm status` reports it on every host, on a
`tooling:` line under `backend:`, saying where each tool was found
(`limactl at /opt/homebrew/bin/limactl`) or, for the ones missing, what
to install; `--json` carries the same as a `tooling` object with `ok`
and `detail`, null inside the guest, whose host owns the VM.

With VM mode disabled, `ssf doctor` reports host backend tooling as a
note. With VM mode enabled, it inspects the guest factory over SSH;
`ssf vm status` separately reports host backend tooling and VM health.
A stopped or unreachable guest makes factory commands fail with a
VM-specific diagnostic. No local factory check or edit is substituted.
`ssf status --json` reports `factory_location: "guest"`,
`factory_reachable`, and a separate `host_vm` object. An unreachable
factory's empty repository/session arrays are unavailable data, not a
claim that the guest has no repositories.

A Firecracker build on anything but Linux x86_64 refuses and says to set
`vm.backend` to `lima`. Either way, `[vm] dir`
(`~/.local/share/ssf/vm`) holds what ssf keeps for the VM, one directory
per `name` under it.

### Firecracker (Linux)

The guest is Ubuntu 24.04 LTS in a
[Firecracker](https://firecracker-microvm.github.io/) microVM, whatever Linux
distribution the host runs. Firecracker runs as your user, given access to
`/dev/kvm` (world-writable on Omarchy, Arch and Fedora; on Debian and Ubuntu a
user logged in at the machine's seat gets access through udev, and a user who
only comes in over ssh needs the `kvm` group: `sudo usermod -aG kvm $USER` and a new
login), the guest's network is
[gvisor-tap-vsock](https://github.com/containers/gvisor-tap-vsock) (a
user-mode TCP/IP stack on the host end of a vsock, so no tap, bridge or
firewall rule on the host), and the images are made with `fakeroot` and
`mkfs.ext4 -d`. The jailer is not used (it needs root); isolation is KVM
plus Firecracker's seccomp filter. It is x86_64 only: the Firecracker,
gvproxy and kernel binaries `ssf vm build` downloads are built for it.

The host needs `/dev/kvm` usable by you, `fakeroot`, `bsdtar`
(libarchive), `mkfs.ext4`, `e2fsck`, `debugfs` and `resize2fs` (e2fsprogs), `curl`,
`openssh`, and its own `herdr` binary, which is copied into the image.
All of that is on a stock Omarchy; elsewhere `sudo pacman -S --needed
fakeroot libarchive e2fsprogs curl openssh`, `sudo apt install fakeroot
libarchive-tools e2fsprogs curl openssh-client` or `sudo dnf install
fakeroot bsdtar e2fsprogs curl openssh-clients` (the .deb and .rpm
recommend them, so apt and dnf bring them with the package).
`ssf vm build` downloads Firecracker, gvproxy, a guest kernel and a pinned
official Ubuntu minimal-cloud root tarball into `vm.dir`, verifies its SHA-256,
then makes the root image there. The tarball is cached in `vm.dir/dl`; a forced
build reuses it.
Each VM's disks are files under `<vm.dir>/<name>/`.

### lima (macOS, and Linux with qemu)

The guest is a [lima](https://lima-vm.io) instance, driven with
`limactl`. On macOS lima uses Apple's Virtualization framework (`vz`,
macOS 13.5 or later; Apple silicon and Intel both work, and nested
virtualisation is not needed); on Linux it uses qemu, so it is the way
to run the VM on a machine without `/dev/kvm` or on aarch64, slower than
Firecracker. Nothing runs as root.

The host needs `limactl`, **lima 2.0.1 or newer** (`brew install lima`
on macOS, which the Homebrew `ssf` formula pulls in; the `lima` package
or lima's release tarball on Linux, or `[vm] limactl` pointing at one
elsewhere), `gh` (the GitHub CLI, to fetch the guest's `ssf` binary on a
Mac; see below), `openssh`, and `qemu-system-x86_64` or
`qemu-system-aarch64` for the machine's architecture wherever qemu is
the driver (Arch: `qemu-full` or `qemu-base`; Debian and Ubuntu:
`qemu-system-x86` or `qemu-system-arm`; Fedora: `qemu-system-x86` or
`qemu-system-aarch64`; macOS: `brew install qemu`). That is every Linux
host, and a Mac only when `[vm] vm_type` is `"qemu"` -- lima's own
default there is `vz`, the Virtualization framework, which needs no qemu
at all. `ssf vm build` checks for them first and names what is missing,
as does the `tooling:` line of `ssf vm status`.

The build also reads what `limactl --version` prints and refuses an
older lima by name, rather than letting it fail at the first boot. Three
things want 2.0.1. The template names its base image the way lima 2.0
spells a template locator, and a 1.x lima dies on that with `filename ""
is invalid`. lima 2.0.0's release tarball ships the `_images` templates
it points at as an empty directory, so the base is not found there
either (a 2.0.0 built from source, which is what Homebrew does, has
them; the floor excludes it anyway, and 2.0.1 came the same day). And
the template leaves the share's mount type to lima, whose default for
qemu is 9p -- mounted before the guest provisions itself -- only from
lima 1.0; reverse-sshfs, the default before that, is mounted by the host
agent instead, around the time the guest starts waiting for the share
rather than ahead of it. A
`limactl` whose version cannot be read (a build with none stamped in
prints `<unknown>`) is let through with a warning. ssf is tested against
lima 2.2.0.

The version check is `ssf vm build`'s: the `tooling:` line reports where
`limactl` was found, not what version it is, so an old lima shows there
as installed and is refused by the build. And the floor settles lima's
*default* mount type, not the person's: `_config/default.yaml` in lima's
home sets it where the template is silent, and `_config/override.yaml`
sets it over any template. Which is why the guest's own failure, when
the share never arrives, names `mountType` and those two files rather
than only the mount point.

Two places hold a lima VM. The instance itself is lima's, named
`ssf-<vm.name>` (`ssf-default`) in lima's own home (`~/.lima`, or
`$LIMA_HOME`), so lima's tools see it: `limactl list` shows it and
`limactl shell ssf-default` opens a shell as lima's user. Its root disk
is `vm.root_gib` with a floor of 20 GiB (a cloud image plus node and the
harness CLIs does not fit in the Firecracker image's 8). The data disk
is a lima disk, `ssf-<vm.name>`, of `vm.data_gib` (`limactl disk list`),
which ssf attaches to the instance and which survives `ssf vm reset` and
`ssf vm build --force` (with one exception, at the end of this section:
a disk no build ever got a filesystem onto). That name is short on purpose: lima labels the
disk's filesystem `lima-<disk>` and an ext4 label holds 16 characters,
which is why `vm.name` is at most 7 characters under this backend. The
disk is formatted only by the build that creates it: that build alone
writes `format: true` for it in the template, and as soon as the disk
carries its filesystem the same build turns the flag off, both in the
template and, with `limactl edit`, in the instance's own copy, which is
the one lima reads at boot. A later build over an existing disk starts
from `format: false`. lima's copy can only be edited while the instance
is stopped, which is why the build stops it first: `limactl edit` refuses
a running instance. If that last edit does not take, the build fails and
says so, rather than reporting success over a flag it could not put
right.

A build that dies between `limactl create` and the guest answering used
to leave the flag on. It now cleans up after itself: a first boot that
fails stops the instance and puts `format: false` back in both copies
before the build returns its error (that cleanup runs `limactl` on a
path where `limactl` may itself be what is stuck, so it says on screen
that it is working, and it never replaces the build's own error). What
it cannot repair -- the instance is running and `limactl edit` refuses
one, or the edit did not take -- it warns about, and the next boot is
refused.

Every command that could boot the instance looks before it goes on all
the same, because a flag can also come back from outside ssf. `ssf vm
build` over an existing instance and `ssf vm start` both repair what
they find (ssf's own template, and lima's copy while the instance is
stopped), and when the flag is still there afterwards they stop with an
error rather than boot: it names the copy that still says it, the data
disk, and the way out, which is to stop the instance (`ssf vm stop`) and
run the command again, or to do the edit by hand (`limactl edit
ssf-default --set '.additionalDisks[0].format = false'`). Nothing on
this path is taken on trust: a `limactl edit` that reported success is
believed only after the file it edited has been read back, and a probe
that failed (`limactl disk list` erroring, a template that cannot be
read) counts as the dangerous answer rather than the convenient one, so
the boot stops instead of going ahead on a question nobody could
answer. The one case that cannot be repaired in place is a running
instance. In practice `ssf vm start` does not meet it -- it prints
"already running" and returns before any of this -- so it is what a
build over a running instance runs into, and what `ssf vm start` warns
about when it looks once more after the guest is up and finds a flag
that has come back (an instance edited by hand, a template restored from
elsewhere), to be repaired at the next stop. `ssf vm reset` repairs
ssf's own template before it creates the new instance, since that
template is what the new one inherits and the instance being deleted is
not worth fixing. All of this applies only once the data disk exists:
with no disk there is nothing to lose, and the build that makes it is
the one build allowed to hand lima a `format: true`.

That last rule has a tail: a build that dies before lima's boot script
has put a filesystem on a disk it just created leaves the disk blank,
and no later build, start or reset will format it (the guest's seed
waits two minutes for a mount that never comes, and the build fails
minutes later pointing at ssh). ssf marks such a disk
(`<vm.dir>/<name>/disk-unproven`, removed as soon as a guest has used
the disk), names it in the error the build fails with, and lets `ssf vm
build --force` delete and re-create that one disk. A disk any finished
build has used carries no marker and no build deletes it.

ssf's own files are under `<vm.dir>/<name>/`: `lima.yaml` (the template
`ssf vm build` writes from `[vm]`; edit the config and rebuild rather
than the file),
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
| `ssf vm build` | checks for `limactl` (and qemu on Linux), writes `lima.yaml` and `share/`, creates the data disk if it does not exist (`format: true` in the template only on that build; an existing disk is attached with `format: false`), `limactl create`, then a first `limactl start` during which the guest provisions itself (see [The image](#the-image)); waits for that, prints the harness lines, waits for ssh as `ssf`, stops the instance and turns `format` off in lima's copy of the template. With an instance already there it repairs that flag, says so and stops; `--force` deletes and re-creates the instance, never the data disk |
| `ssf vm start` | repairs a stale `format: true` on the stopped instance and refuses to boot while one survives (see above), writes `share/` fresh (scripts, seed, herdr), `limactl start`, waits for the provisioning marker (a reset instance provisions itself again here), for ssh and for the guest daemon. Warns when lima forwards ssh to a port other than `vm.ssh_port` (an instance from an older template; `ssf vm build --force` remakes it) |
| `ssf vm stop` | `limactl stop`, and `limactl stop -f` when the clean stop fails |
| `ssf vm grow` | `limactl disk resize` on the data disk, with the VM stopped; the guest grows the filesystem at its next boot (see [Size](#size)) |
| `ssf vm reset` | `limactl delete` and `limactl create` from the template; the data disk stays, and the next start provisions the fresh root again (a few minutes) |
| `ssf vm destroy --yes` | the instance, the data disk and `<vm.dir>/<name>/`; the confirmation names all three |
| `ssf vm console` | the instance's serial console log (`serial.log`, or `serialv.log`, in lima's instance directory); fails naming the instance when it has not been booted yet and neither is there |

Everything else (`status`, `ssh`, `attach`, `login`, `logs`,
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
| `data_gib` | half the free space, at build time, of the filesystem that will hold the data disk | 20 |

Which filesystem that is depends on the backend, because the two keep
the disk in different places: `vm.dir` under Firecracker, and under lima
lima's own disk directory (`$LIMA_HOME/_disks`, by default
`~/.lima/_disks`), which is often on another volume (with no home
directory to work lima's out from, ssf falls back to `vm.dir`). `ssf vm build`
measures the one the disk will live on and names it in the line above
its choice, so you can see which it read:

```
this machine: 8 CPUs, 32768 MiB RAM, 155 GiB free on /home (measured at /home/you/.lima/_disks, lima's disk directory)
```

Under Firecracker the same line ends `(measured at
/home/you/.local/share/ssf/vm, [vm] dir)`. `ssf vm grow` measures the
same directory when it applies the rule or warns that a size is more
than the host has free.

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

Under Firecracker, `ssf vm build` unpacks a pinned Ubuntu 24.04 LTS
[minimal-cloud root](https://cloud-images.ubuntu.com/minimal/releases/noble/),
adds the guest scripts and units, turns the tree into an ext4 image and boots
it once with a provisioning init. The host may itself be Omarchy, Arch, Ubuntu
or Fedora; its distribution does not determine the guest. Under lima there is
no image step: the instance boots a stock cloud image, and lima runs
`/mnt/ssf/guest/lima-boot.sh` as root at every boot, which on the first
boot (no `/etc/ssf-image-built` yet) runs the same provisioning script
with `SSF_VM_BACKEND=lima`, logs it to `/var/log/ssf-provision.log`, and
writes the marker on success; `ssf vm build` waits for the marker and,
should provisioning fail, prints the end of that log (`limactl shell
ssf-default sudo tail /var/log/ssf-provision.log` shows the rest). Later
boots find the marker and do nothing.

Either way the provisioning script upgrades the base and installs `openssh`,
`sudo`, `git`, `github-cli`, a pinned upstream Node.js LTS with npm, `tmux`,
the harness CLIs
from `ssf agents` that npm or a release tarball provide (Claude Code,
Codex, Gemini, Copilot, OpenCode, Pi, Grok, Crush; each is best effort
and listed at the end of the build), an `ssf` user that is root through
`sudo` (Claude Code refuses its permission-free mode as root, so nothing
runs as root itself), herdr (the host's own binary, or the one described
under [Backends](#backends)) and herdr's agent integrations (its
state-reporting hooks) for the agents present. Firecracker installs the list
through `apt-get`; a lima guest uses `apt-get` or `pacman` according to its
image. The list lives in
`vm/guest/provision.sh` (`/usr/share/ssf/vm/` when installed on Linux,
`$(brew --prefix)/share/ssf/vm/` on macOS). To change it, copy that
directory somewhere of your own, edit the copy, and run `SSF_VM_DIR=<copy>
ssf vm build --force` for a new image or instance; a file edited under
the installed directory is overwritten by the next package upgrade. The
`ssf` binary is not in the image: every start takes the host's (or the
release asset under lima on a Mac), so the guest always runs the version
you installed.

## What gets in, and what does not

At every start the host supplies the `ssf` binary, the SSH public key
used to administer the guest, and explicitly selected `vm.files` (a seed
disk under Firecracker, the read-only `seed/` share under lima). Routine
starts do not copy factory configuration, bot credentials or signing keys.
The host needs its separate VM administration key, not the bot's key.

The guest owns repository configuration, harness/model/effort choices,
daemon policy, allowed users, GitHub bot authentication and git identity.
Run `ssf auth login --web` after starting and enabling the VM: the device
flow runs in the guest and prints a URL and code for approval in your
browser, signed in as the bot. Credentials and the generated signing key
stay on the guest data disk. `ssf auth status` inspects that guest account.
Personal git credentials and signing keys must also be provisioned in the
guest; config paths refer to guest files. See [Committing as a
person](identity-and-bylines.md#committing-as-a-person-while-gh-stays-the-bot).

`vm.files` remains an explicit file import (`src` or `src:dest`). Do not
use it to maintain a second factory config or overwrite guest credentials.
Read [Harness logins](#harness-logins) before importing a harness login.
No other host home files, keyring, SSH configuration or git configuration
are visible in the guest.

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
(`vm/guest/sudoers`), so an agent in the Firecracker guest installs packages
with `apt` (a custom lima image may instead use `pacman`), adds tools, edits the
units, restarts services and reboots
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
made per VM. With `vm.enabled`, repository, factory configuration and
bot authentication commands run in the guest, as do commands that talk to the daemon
(`status`, `peers`, `sub`, `unsub`, `subs`, `tell`, `handover`, `release`,
`purge`, `doctor`, `run --once`) run inside the guest over that
connection. A stopped or unreachable guest produces an error; it never
falls back to editing host factory settings. Start it with `ssf vm start`
and retry. `ssf config get|set vm.<key>` and `ssf vm ...` operate on the
host. `ssf vm status` diagnoses host infrastructure; `ssf status`,
`ssf doctor`, `ssf auth status` and repository listing inspect the guest
factory. The bar widget, `ssf status --json` and `ssf tell` work as
before; `ssf vm run -- <args>` does it explicitly and `ssf vm ssh
[-- cmd]` gives a shell. `ssf vm attach` attaches to herdr's session in
the guest in your terminal; `ssf vm ssh-config` prints an `~/.ssh/config`
entry so `herdr --remote ssf-default` (herdr's thin client) and plain `ssh
ssf-default` work too.

`ssf run --once` is a guest command too. While the guest's `ssf.service`
owns its state it refuses; let its next poll do the work. The host adds the
VM name after the guest's refusal.
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
(factory config, bot token and signing key, git identity, harness logins,
herdr's session state, transcripts and caches), which is
bind-mounted from there. Under lima the two are the instance's root disk
in lima's home and the lima disk `ssf-<name>` (see
[lima](#lima-macos-and-linux-with-qemu)), which lima mounts at
`/mnt/lima-ssf-<name>` and the guest's seed unit bind-mounts on
`/var/lib/ssf` at every boot, with the same layout on it. `ssf vm stop`
shuts the guest down cleanly (Ctrl-Alt-Del through Firecracker's API;
`limactl stop` under lima); on the next start the guest daemon's own
`resume_on_start` brings the sessions back in herdr, as after a reboot on
bare metal.

Factory edits go straight to the guest and are picked up on its next poll.
`ssf vm restart` supplies a new binary or explicit `vm.files`; it preserves
established guest factory configuration and credentials. `ssf vm reset` gives the
guest a fresh root (Firecracker: the root disk remade from a rebuilt
image; lima: the instance deleted and created again, provisioned on its
next start) and keeps the data disk, so sessions survive it (the guest
home is copied from the image only when the data disk is new, so a
rebuilt image's hooks and `~/.claude.json` reach an existing VM only
through `ssf vm destroy`, or by hand); `ssf vm destroy --yes` removes the
VM and all its disks.

## Upgrading existing VMs

Select VM mode (`ssf config set vm.enabled true`) before migrating a
legacy VM factory: enable it before `ssf vm build` or `ssf vm start`
when you intend to import host factory settings. VM management with
`vm.enabled = false` leaves the
independent host factory configuration and credentials unchanged; it does
not import them into the guest or strip them from the host. Building while
VM mode is disabled initializes a separate guest factory. If you later
enable VM mode while host factory settings remain, reconcile them
explicitly; they are not silently substituted for that guest.

The first start in VM mode with this version adopts the existing data disk. Until
adoption completes, the host's legacy factory configuration is translated
for guest paths and compared with any existing guest configuration. Parsed
configuration must agree; existing credential and key files must have the
same bytes as any incoming copies. A missing guest config or credential
can be imported once. Migration preserves daemon state, clones, worktrees,
harness logins and the rest of the guest home.

If both versions differ, migration stops before replacing either version.
The guest daemon stays off and SSH remains available for recovery. Read
`ssf vm ssh -- cat /home/ssf/.config/ssf/migration-error` and inspect the
host config and guest `/home/ssf/.config/ssf/config.toml` through
`ssf vm ssh`. Back up both before reconciling their intended settings or
credentials. To explicitly choose the existing guest configuration, back
up the host config, remove its factory sections while retaining `[vm]`,
and run `ssf vm restart`. To retain changes from both, reconcile them
explicitly, accounting for translated guest paths, and restart. Never
remove guest data or its credentials to bypass a conflict.

Successful adoption writes `/home/ssf/.config/ssf/guest-owned` last. Once
that marker exists, guest configuration and credentials take precedence
on every later boot. The host saves its original config as
`config.toml.pre-guest-ownership` (mode 0600), acknowledges adoption in
`<vm.dir>/<name>/guest-owned`, and saves only `[vm]` in its active config.
Migration does not revoke an existing host gh login or delete original
host keys; those legacy credentials are no longer required to run the VM.
Keep the backup for recovery, rather than editing it as a second factory.

Interrupted adoption is retryable with `ssf vm restart`: copies are
atomic and existing files are checked again before the completion marker.
If the guest completed adoption but host acknowledgement was interrupted,
`ssf vm sync` can finish that acknowledgement. This command now only
checks/completes ownership migration; it never pushes ordinary factory
edits. An older guest without the marker must restart to run migration.
Do not manually create or delete ownership markers.

Legacy roots contain a seed script that can overwrite persistent guest
configuration. Both backends refuse to boot an incompatible root. For
Firecracker, start first runs `e2fsck` on the stopped disposable root to
replay its filesystem journal, then uses `debugfs` read-only to verify that
the installed seed script matches this binary and the root is Ubuntu 24.04
LTS. An old Arch root or incompatible script stops startup before the data
disk is attached; startup never patches that root in place.

For Firecracker recovery, including migration from the former Arch guest,
install the matching ssf package (including its guest scripts), then run
`ssf vm build --force`, `ssf vm reset`, and `ssf vm start`. Reset alone would
reuse the old root image and is not sufficient. These steps replace only the
disposable root; the independent data disk containing factory configuration,
credentials, repositories and worktrees is retained. If `vm.rootfs` selects a
custom image, replace it with an Ubuntu 24.04 image built with the matching
guest scripts before resetting. For Lima, run `ssf vm reset`, then `ssf vm
start`; the new root provisions the current scripts and preserves the data
disk. The next start performs the migration above;
enable VM mode first when importing legacy host settings.

After successful adoption, `ssf vm reset` and `ssf vm build --force`
preserve the factory on the data disk. Host mode (`vm.enabled = false`)
continues using local configuration and credentials without this migration.

Firecracker and gvproxy are started in a session of their own, and lima's
host agent runs detached too, so a VM started from a terminal (`ssf vm
start`) outlives that shell and any later `ssf` command. The service
(the systemd user unit on Linux, `brew services` on macOS) is different:
`ssf run` starts the VM if it is not up and owns it from then on, so
stopping or restarting the service shuts the guest down cleanly, and a
crash of the host daemon ends it with the service's cgroup on Linux.
With the VM stopped, `ssf status` says so instead of forwarding (the bar
widget shows the service as stopped), and the other forwarded commands
refuse with `the factory runs in VM <name>, which is not running`. Only a
definite answer does that. The liveness question forks `limactl` under
lima, and a `limactl` that fails, or does not answer within fifteen
seconds, leaves ssf unable to tell. It then says so on stderr -- the
reason it could not ask, whatever backend tooling this host is missing,
and that it is sending the command anyway -- and forwards the command,
which the guest answers if the VM is in fact up and which fails as an
ssh error if it is not; the note carries the same `ssf vm start` advice
the refusal would have given, since an ssh error carries none.

`status --json` always answers, whatever the guest does, so nothing
parsing it is left with no document at all. Where the guest answers,
that answer is passed through untouched and with its exit status. Where
it does not, the host writes one of its own and exits 0, the way it does
for a VM it knows is stopped: it carries what the host could see (`vm`
is `running`, `stopped` or `unknown`, and the service line is this
machine's) and no sessions or repositories, which are the guest's to
know. That covers the window after `limactl start` when lima says
`Running` before the guest's sshd does, as well as a probe that could
not be made at all. The bar widget shows the same panel either way --
it reads sessions, not `vm` -- but its service toggle reads the truth
rather than the empty object it falls back to, and a `jq` over the
command gets a field rather than a parse error.

A probe that could not be made is never read as a stopped factory:
reading it that way refused `tell`, `release`, `purge` and `doctor` over
a running VM and showed the widget an idle one. The supervisor inside
`ssf run` asks the same question on its own loop, where a slow answer is
waited out rather than cut short at fifteen seconds, since it gives up on
a VM only after ten rounds with no answer at all.
