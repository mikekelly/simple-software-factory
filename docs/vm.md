# Inside a microVM (Firecracker)

How `ssf vm` runs the whole factory inside a Firecracker microVM, what gets into the guest, how to reach it and what persists. For whoever wants the agents kept off their own machine; agents only need to know they have `sudo` there, which their first prompt says.

Without it, everything runs on your machine as you: the agents can read
your home directory, your keyring and whatever else you have open.
`ssf vm` moves the whole factory (the daemon, herdr and every agent
session) into a [Firecracker](https://firecracker-microvm.github.io/)
microVM, and leaves the host only what builds, starts, stops and reaches
the guest. The drivers are untouched; the guest runs herdr (Orca is a
desktop app and needs a display the guest does not have, so a repository
that says `driver = "orca"` runs in herdr there). Nothing needs root:
Firecracker runs as you given `/dev/kvm` (world-writable on Omarchy; the
`kvm` group elsewhere), the guest's network is
[gvisor-tap-vsock](https://github.com/containers/gvisor-tap-vsock) (a
user-mode TCP/IP stack on the host end of a vsock, so no tap, bridge or
firewall rule on the host), and the images are made with `fakeroot` and
`mkfs.ext4 -d`. The jailer is not used (it needs root); isolation is KVM
plus Firecracker's seccomp filter.

The host needs `/dev/kvm` usable by you, `fakeroot`, `bsdtar`
(libarchive), `mkfs.ext4` (e2fsprogs), `curl`, `openssh`, and its own
`herdr` binary, which is copied into the image.

```sh
ssf vm build              # once: downloads Firecracker, gvproxy and a guest kernel, makes and provisions the image (a few minutes)
ssf config set vm.enabled true
systemctl --user restart ssf.service   # or: ssf vm start
ssf vm status             # whether the VM runs and its daemon answers
ssf status                # runs inside the guest from now on
ssf vm attach             # herdr in the guest, in this terminal
```

The `[vm]` keys (`name`, `dir`, `vcpus`, `mem_mib`, `data_gib`,
`root_gib`, `ssh_port`, `files`, and the binaries and images to use
instead of the downloaded ones) are in the
[configuration table](configuration.md#every-key).

## The image

`ssf vm build` unpacks the Arch bootstrap tarball, adds the guest scripts
and units, turns the tree into an ext4 image and boots it once with a
provisioning init that installs `base`, `openssh`, `sudo`, `git`,
`github-cli`, `nodejs`, `npm`, `tmux`, the harness CLIs from `ssf agents`
that npm or a release tarball provide (Claude Code, Codex, Gemini,
Copilot, OpenCode, Pi, Grok, Crush; each is best effort and listed at the
end of the build), an `ssf` user that is root through `sudo` (Claude Code
refuses its permission-free mode as root, so nothing runs as root itself),
the host's own herdr binary and herdr's agent integrations (its
state-reporting hooks) for the agents present. The list lives in
`vm/guest/provision.sh` (`/usr/share/ssf/vm/` when installed; `SSF_VM_DIR`
points at another copy); edit it and run `ssf vm build --force` for a new
image. The `ssf` binary is not in the image: every start takes the host's,
so the guest always runs the package you installed.

## What gets in, and what does not

At every start the host writes a small seed disk with the `ssf` binary,
`config.toml` rewritten for the guest (`driver = "herdr"`, clones under
`/var/lib/ssf/projects`, no `repo.path`), the bot token (resolved the way
`ssf token` does, so the host keyring itself is never copied), the bot's
own SSH key if `ssf auth login` enrolled one, the ssh public key the host
uses to reach the guest, and the files `vm.files` lists. That last one is
how a harness login gets in: `files = ["~/.claude/.credentials.json"]`
lands at the same place under the guest user's home; `src:dest` places a
file elsewhere. Nothing else from the host home is visible in the guest:
no `~/.ssh`, no `~/.gitconfig`, no other accounts. Commits are signed only
if the bot key is enrolled.

## What the agent can do there

The `ssf` user has passwordless `sudo` for everything
(`vm/guest/sudoers`), so an agent in the guest installs packages
(`sudo pacman -S ...`), adds tools, edits the units, restarts services and
reboots as it sees fit; the first prompt and `ssf guide` say so with one
line inside the VM (`SSF_VM_GUEST=1` in the guest's environment is how ssf
knows) and say nothing on bare metal, where the agent has whatever the
human running ssf has. The VM is the isolation boundary: whatever the
agent does to the guest stays on that VM's two disks, the host is
untouched, and `ssf vm reset` (a fresh root disk) or `ssf vm destroy`
puts it back.

## Reaching it

gvproxy publishes the guest's sshd on `127.0.0.1:<vm.ssh_port>`, keyed by
a key made per VM. With `vm.enabled` the commands that talk to the daemon
(`status`, `peers`, `sub`, `unsub`, `subs`, `tell`, `release`, `purge`,
`doctor`, `run --once`) run inside the guest over that connection, so the bar widget,
`ssf status --json` and `ssf tell` work as before; `ssf vm run -- <args>`
does it explicitly and `ssf vm ssh [-- cmd]` gives a shell. `ssf vm
attach` attaches to herdr's session in the guest in your terminal;
`ssf vm ssh-config` prints an `~/.ssh/config` entry so `herdr --remote
ssf-default` (herdr's thin client) and plain `ssh ssf-default` work too.
Clicking a session in the bar widget opens a terminal attached to the
guest. `ssf vm logs` follows the guest daemon's journal and `ssf vm
console` shows the serial console.

## Persistence

Each VM has, under `<vm.dir>/<name>/`, a `root.ext4` (a copy-on-write
copy of the image: instant on btrfs, a full copy elsewhere) with the
packages, and a `data.ext4` (`vm.data_gib`, sparse) mounted at
`/var/lib/ssf` with everything that matters: ssf's state, the clones and
the worktrees, and the guest user's home (herdr's session state, the
harness transcripts, caches), which is bind-mounted from there. `ssf vm
stop` shuts the guest down cleanly (Ctrl-Alt-Del through Firecracker's
API); on the next start the guest daemon's own `resume_on_start` brings
the sessions back in herdr, as after a reboot on bare metal.

Editing the config on the host takes `ssf vm sync` (pushes config and
token and restarts the guest daemon) or `ssf vm restart` (a new seed:
needed for a new `ssf` binary or `vm.files`). `ssf vm reset` remakes the
root disk from a rebuilt image and keeps the data disk, so sessions
survive it (the guest home is copied from the image only when the data
disk is new, so a rebuilt image's hooks and `~/.claude.json` reach an
existing VM only through `ssf vm destroy`, or by hand); `ssf vm destroy
--yes` removes the VM and all its disks.

Firecracker and gvproxy are started in a session of their own, so a VM
started from a terminal (`ssf vm start`) outlives that shell and any later
`ssf` command. The systemd service is different: `ssf run` starts the VM
if it is not up and owns it from then on, so stopping or restarting the
service shuts the guest down (cleanly, over Firecracker's API) and a crash
of the host daemon ends it with the service's cgroup. With the VM
stopped, `ssf status` says so instead of forwarding (the bar widget shows
the service as stopped).
