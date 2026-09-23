# Platform specifics

This is the single home for everything that depends on a particular
operating system, distribution, hosting vendor, agent harness, or on an
older installation that has to be brought forward. Nothing here is needed
on the generic path: read the section that matches the machine in front of
you, and ignore the rest.

Each section opens with one line saying when it applies. The generic
instructions live in `ssf skill setup` (docs/install.md),
`ssf skill operate` (docs/operate.md) and
`ssf skill troubleshoot` (docs/troubleshooting.md).

## Arch Linux and Omarchy

Applies when installing on Arch or a derivative such as Omarchy.

Install the package from a release asset (**ask before sudo**):

```sh
sudo pacman -U ssf-*.pkg.tar.zst
```

herdr is a dependency the package cannot supply on plain Arch: install
`herdr` or `herdr-bin` from the AUR before ssf. Omarchy's own repositories
already carry herdr.

If `ssf vm build` reports missing image tools:

```sh
sudo pacman -S --needed fakeroot libarchive e2fsprogs curl
```

On Omarchy only, `ssf ui install` adds a **Factory** submenu to the Omarchy
menu: Dashboard, Status, a "Service enabled" toggle, Restart service, and
Logs. `ssf ui uninstall` removes it, and `ssf uninstall` deliberately leaves
it alone because the menu belongs to the desktop. Off Omarchy the command
writes nothing. There is no bar widget; `ssf ui install` and `ssf setup`
disable and remove any copy of the superseded one they find, and
`ssf doctor` says what is left until one of them runs.

## Debian and Ubuntu

Applies on Debian, Ubuntu and derivatives.

```sh
sudo apt update
sudo apt install ./ssf_*_amd64.deb
```

Run `apt update` first, including on minimal images with stale lists. The
`.deb` depends on `systemd`, `git`, `jq`, `gh` and an OpenSSH client.

- **gh on Debian 12:** its bundled GitHub CLI is too old. Add [GitHub's apt
  repository](https://github.com/cli/cli/blob/trunk/docs/install_linux.md)
  and install `gh` from there before ssf. Recent Ubuntu and newer Debian
  releases supply a recent enough `gh` themselves.
- **herdr** is not a package dependency here. For sessions on the host,
  install it by hand with `curl -fsSL https://herdr.dev/install.sh | sh`;
  ssf also looks in `~/.local/bin`. A VM installs its own herdr, so a
  VM-mode machine does not need one on the host.
- **KVM access:** if `/dev/kvm` is not readable and writable by the account
  that will run the VM, `sudo usermod -aG kvm "$USER"` and a fresh login
  fixes it (**ask before sudo**).
- **VM image tools:** `sudo apt install fakeroot libarchive-tools e2fsprogs curl`.
- **Containers:** installing the `systemd` package does not make it PID 1 or
  give you a working `systemctl --user`. On such a host use host mode with
  standalone binaries instead.

## Fedora family

Applies on Fedora, RHEL derivatives and other rpm distributions.

Install the `.rpm` from the release assets with `dnf install ./ssf-*.rpm`,
and remove it with `sudo dnf remove ssf`. The rpm depends on `gh`, `git`,
`jq`, `systemd` and `openssh-clients`, and recommends `fakeroot`, `bsdtar`,
`e2fsprogs` and `curl` for building a VM image. Packages are built for
x86_64; on another architecture use the standalone binaries. herdr is not
packaged here either: install it by hand as for Debian, unless the factory
runs in a VM.

## macOS

Applies on any Mac.

```sh
brew install mikekelly/tap/ssf
```

The formula installs both the `ssf` client and the `ssf-server` daemon, and
pulls in `gh` and `lima`. It is macOS only and is not for Linuxbrew. There
is no Omarchy menu and no herdr on the host: on a Mac the factory runs in a
lima VM and herdr lives in the guest.

The background service is a launchd agent per target, `dev.ssf.server.NAME`,
managed with `ssf --server NAME ui service enable|disable|status`. Its log
is `~/Library/Logs/ssf/NAME.log`.

The VM backend on macOS is lima. lima's own default there is Apple's
Virtualization framework (`vz`), which needs no qemu and no nested
virtualisation, on Apple silicon and Intel alike. qemu is only needed when
`[vm] vm_type` is set to `"qemu"`; then `brew install qemu`. `ssf vm build`
checks `limactl`, `gh`, `openssh` and any required qemu before it starts,
names what is missing, and refuses a lima older than the floor it prints;
the `tooling:` line of `ssf vm status` reports where each was found.

The lima instance is named `ssf-<vm.name>` in lima's own home, so
`limactl list` and `limactl shell ssf-default` see it. Its data disk is a
lima disk of the same name and survives `ssf vm reset` and
`ssf vm build --force`. Because lima labels the disk's filesystem and ext4
labels are short, `vm.name` is at most 7 characters under this backend.

Do not use `brew services` to run the factory: `ssf setup` and
`ssf ui service` drive a launchd agent per target. The formula's own
`brew services start ssf` is the older installation-wide service, and ssf
refuses to enable a target's agent while it runs (see [Upgrading from an
older ssf](#upgrading-from-an-older-ssf)).

## Linux without KVM

Applies on a Linux machine with no usable `/dev/kvm`, or on aarch64, that
should still run the VM.

Set the lima backend (`[vm] backend`), which runs the guest under qemu as
your own user. It is slower than Firecracker and needs no root. Install
`limactl` (the distribution's `lima` package, lima's release tarball, or
`[vm] limactl` pointing at one elsewhere) and the qemu binary for the
machine's architecture:

| Distribution | Package |
|---|---|
| Arch | `qemu-full` or `qemu-base` |
| Debian, Ubuntu | `qemu-system-x86` or `qemu-system-arm` |
| Fedora | `qemu-system-x86` or `qemu-system-aarch64` |

`ssf vm build` checks for them and names what is missing. If the machine
cannot run a VM at all, use host mode or a rented host instead.

## Rented hosts

Applies when the factory should run on a machine the person rents rather
than on their own computer: a bot account's dedicated server, or a VPS from
Hetzner, Linode, OVH or similar.

The host runs the factory in host mode with the standalone binaries; the
person operates it from their own machine as a client over SSH. Nothing
here needs KVM, Docker or a desktop session, though it does need a Unix
account the agents will run as: in host mode agents can reach that user's
files and credentials, so give the factory its own account.

1. Check the host's real capabilities rather than the product description:
   architecture, RAM, free disk, whether `/dev/kvm` is usable, and whether
   `systemctl --user` works. A vendor name settles none of these.
2. Install prerequisites with the host's package manager: CA certificates,
   curl, git, jq, a recent `gh`, and an OpenSSH client (`ssh-keygen` is
   needed to enrol the bot's key). Refresh package indexes first on minimal
   images.
3. Install herdr, and the harness CLIs the repositories will use, as the
   same Unix user that will run the factory.
4. Put `ssf` and `ssf-server` **from the same release** into
   `~/.local/bin` on that account, keep the two together, and persist that
   directory on `PATH`. The download commands are in `ssf skill setup`
   (docs/install.md).
5. Leave the server catalog on the host empty, so the client there and a
   foreground `ssf-server` share one configuration and state. Do not set
   `SSF_SERVER` on the host.
6. Sign the bot in (`ssf auth login`), watch a repository
   (`ssf repo add owner/repo --harness ID`), then run herdr and
   `ssf-server` under whatever keeps processes alive on that host.
7. From the person's own machine, reach it as a client:
   `ssf --server user@host status`, or give it a catalog name with
   `ssf server add NAME --ssh user@host`. SSH starts the same command
   endpoint on the remote machine; ssf opens no TCP listener. The SSH
   account must be the one that can operate that factory, and needs
   `ssf-server` on its `PATH`.

Renting a machine costs money and usually means creating an account: both
are the person's decision, not yours.

## Tailscale

Applies when the person wants to reach a VM guest, or the browser
dashboard, from another of their devices.

`ssf vm tailscale` installs Tailscale inside the running guest, starts its
daemon and prints the login URL that enrols it in the tailnet. Nothing is
installed in the base image or on the host. The requested machine name is
`ssf-vm`; Tailscale keeps names unique, so an existing one becomes
`ssf-vm-1`, and the command prints the name and address actually assigned.
The package, node key and preferences live on the disposable root disk:
they survive restarts, but a root reset removes them, so enrol again after
one. The command enables neither Tailscale SSH nor route advertisement;
those stay explicit decisions made from a shell in the guest.

The optional browser dashboard is off by default and its bind address is
restricted: only loopback (including `::1`) and Tailscale addresses (IPv4
in `100.64.0.0/10`, IPv6 in `fd7a:115c:a1e0::/48`) are accepted, and a
direct bind to any other address is refused. Over a tailnet the transport
is plain HTTP, encrypted by Tailscale itself. Exposing it beyond the
tailnet is a separate, deliberate step. `ssf skill dashboard`
(docs/dashboard.md) has the rest.

## Harness notes

Applies when a specific coding-agent harness needs interactive sign-in or
behaves unusually inside a session.

**Signing a harness in.** A harness that is not signed in blocks its
sessions: ssf answers first-run trust dialogs from the pane, but a login
prompt is the one dialog it cannot answer. Sign in as the same Unix user
and `HOME` that runs herdr; in VM mode use `ssf vm login [HARNESS]`, which
runs the harness's own login inside the guest in your terminal and writes
the credential there, copying nothing from the host.

**Loopback enrolment.** Some harnesses (OMP among them) complete their
login by opening a short `http://localhost:<port>/...` URL that the browser
must reach. Run the login on the computer with the browser; ssf detects the
link and forwards that port from host loopback to guest loopback, binding
only `127.0.0.1` and `::1`. If the port is taken, ssf stops the login with
an error: free it and retry. Keep the login terminal open until enrolment
finishes, then exit the harness to check the result. Running the login on a
remote SSH host does not forward to the browser's computer. Device-code and
pasted-API-key flows do not need any of this.

**OMP and OpenRouter.** An API key in the daemon's environment does not
reach herdr panes: ssf forwards no such key and has no environment-file
loader. Either save the credential through the harness's own login into the
shared home, or make sure the pane environment itself receives the key, and
verify a real request in a herdr-launched pane. A passing `ssf doctor` only
says a credential is present, not that it is valid or funded.

**First-run wizards.** After authenticating, run the harness once
interactively as the factory's Unix user (inside the guest in VM mode) and
finish or Esc through its first-run setup before letting ssf spawn
sessions. A session held at that wizard is reported as setup incomplete;
completing it in any terminal releases the session at the next check. A
provider row that already says logged in does not call for another login,
only for the wizard to be finished.

**Delivery quirks worth knowing.** ssf delivers an item's later activity
into the running session through the harness's own channel where one
exists, and falls back to typing into the terminal where it does not. Two
consequences for an operator: after upgrading ssf, restart already-running
sessions so they are relaunched with the current delivery setup, and a
custom `command` for a repository replaces the whole default, so it must
keep the launcher and flags the default supplies or that session loses
native delivery. `ssf doctor` reports a session whose channel is
unavailable. `ssf skill drivers` (docs/drivers.md) has the detail.

## Liaison on a bot-account assistant

Applies only when the person also wants an always-on assistant watching the
factory's GitHub activity and reporting to them, on a platform such as
Cursor's Grok Bot.

The liaison is separate from the factory: connect GitHub to the account
that owns the assistant, even where `gh api user` and `ssf auth status`
already succeed on the host. Prefer the person's own account for that
second enrolment, with access to each repository the liaison watches. That
keeps the useful separation: the liaison sees what the person sees, while
the factory remains the actor that delivers work as its bot account. The
event connection a routine uses is also separate from any GitHub plugin the
assistant uses to read or change GitHub during a run; grant each requested
permission according to what the liaison actually needs.

Then ask that assistant to create a routine on the GitHub events that need
the person's attention for `owner/repo`: when it runs, it inspects the item,
summarises what changed with links, says whether the factory is already
handling it, and names the next decision needed, without commenting,
merging, closing, assigning or changing fields without approval. Narrow the
event selection: broad listeners create noise and consume the assistant's
usage. Turn the routine active, then test it with a real matching event
rather than assuming a manual run proves the listener is subscribed. Verify
the platform's current interface before configuring it; `ssf skill liaison`
(docs/liaison.md) has the full recipe.

## Upgrading from an older ssf

Applies only on a machine whose ssf predates named servers. A fresh
installation needs none of this.

**One singleton service became one service per target.** The old
installation-wide unit is `ssf.service` (or the Homebrew `ssf` service on
macOS). ssf refuses to enable a target's service while it is enabled or
active, because two supervisors must not own one factory. Stop it first:

```sh
systemctl --user disable --now ssf.service   # Linux
brew services stop ssf                        # macOS
```

Then enable the target's own service with
`ssf --server NAME ui service enable`.

**An enabled `[vm]` table becomes a named VM target.** Confirm the VM is
healthy (`ssf vm status`, and the guest factory answering), then:

```sh
ssf server migrate-vm            # names the target ssf-server by default
ssf --server ssf-server vm status
ssf --server ssf-server status
```

The migration copies the host-owned VM settings into the catalog entry and
removes the legacy `[vm]` table. The instance, its disks, guest
configuration, credentials and workspaces are not moved or rebuilt, and the
operation is idempotent: conflicting old and new settings are kept and
reported rather than resolved by recency. Read and change the migrated
settings afterwards with `ssf --server NAME config get|set vm.<key>`.
Migrate before adding any other VM target: a legacy `[vm]` wrapper cannot
coexist with another managed VM.

**The copied-config VM layout.** On an installation where the host held the
factory configuration and the guest received a copy, the first start after
upgrading adopts the data disk: the host configuration is translated for
guest paths and compared with what the guest already has. Parsed
configuration must agree and credential files must match byte for byte; a
missing guest config or credential can be imported once. If they differ,
migration stops before replacing either side, the guest daemon stays off,
and SSH remains open for recovery. Read
`ssf vm ssh -- cat /home/ssf/.config/ssf/migration-error`, inspect both
configurations, back them up, and reconcile them deliberately. Never delete
guest data or credentials to get past a conflict, and never create or
delete the ownership markers by hand. An interrupted adoption is retried
with `ssf vm restart`; an interrupted host acknowledgement is finished with
`ssf vm sync`, which does nothing else. After adoption the guest owns its
configuration and credentials on every later boot, and ordinary edits need
no sync.

**An incompatible guest root is refused before boot** rather than patched
in place, because an old root's seed script could overwrite the guest's
configuration. Under Firecracker, install the matching ssf package with its
guest scripts, then `ssf vm build --force`, `ssf vm reset`, `ssf vm start`;
a reset alone would reuse the old image. Under lima, `ssf vm reset` then
`ssf vm start`. Both replace only the disposable root: the data disk with
the factory's configuration, credentials, repositories and worktrees is
kept. If `vm.rootfs` points at a custom image, replace it with one built
from the matching guest scripts first.

**Uninstall on an old installation.** `ssf uninstall` is still
installation-wide and refuses a migrated VM or namespaced local target. If
it cannot stop the service it stops there and changes nothing, telling you
to stop it by hand (`systemctl --user stop ssf.service`, or
`brew services stop ssf`) and run it again.
