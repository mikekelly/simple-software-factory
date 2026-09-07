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
(libarchive), `mkfs.ext4`, `e2fsck` and `resize2fs` (e2fsprogs), `curl`,
`openssh`, and its own `herdr` binary, which is copied into the image.

```sh
ssf vm build              # once: downloads Firecracker, gvproxy and a guest kernel, makes and provisions the image (a few minutes)
ssf config set vm.enabled true
systemctl --user restart ssf.service   # or: ssf vm start
ssf vm status             # whether the VM runs and its daemon answers, and which harnesses are logged in there
ssf vm login              # sign a harness in inside the guest (see below)
ssf status                # runs inside the guest from now on
ssf vm attach             # herdr in the guest, in this terminal
```

The `[vm]` keys (`name`, `dir`, `vcpus`, `mem_mib`, `data_gib`,
`root_gib`, `ssh_port`, `files`, and the binaries and images to use
instead of the downloaded ones) are in the
[configuration table](configuration.md#every-key).

## Size

A factory running several coding sessions at once needs most of the
machine, so the VM is sized from the machine rather than from constants.
`ssf vm build` reads the host and fills in every size key left unset in
`[vm]`, prints what it chose and where each value came from, and writes
the values to `config.toml`, where they stay visible and editable:

| key | rule | floor |
|---|---|---|
| `vcpus` | the host's logical CPUs minus one | 2 |
| `mem_mib` | half the host's RAM, rounded down to 256 MiB | 4096 |
| `data_gib` | half the free space of the filesystem holding `vm.dir`, at build time | 20 |

`root_gib` stays 8: the root image only holds the system. A value set in
`[vm]` by hand always wins, and `ssf vm build --vcpus N --mem-mib N
--data-gib N` writes the given value instead of the rule. A build over an
existing image (without `--force`) still does the sizing, so an
installation from before the rule gets its sizes recorded by running
`ssf vm build` once. With the keys unset and no build run, `ssf vm start`
applies the rule at each start without writing it.

A rule of thumb per parallel session: about one vCPU and 2 GiB of RAM
per active session, plus, on the data disk, the size of one clone per
repository and a build tree per worktree. Change `vcpus` or `mem_mib` in
`config.toml` and `ssf vm restart` to apply them.

The data disk is a sparse file: its size is a cap, not a reservation, and
it takes host space only as the guest writes. Firecracker gives the guest
block devices, not a shared directory, and ext4 needs its size when it is
made, so "no cap" means a cap that follows the host and grows later. A
cap above the host's free space is a bad idea: a host that fills up shows
in the guest as I/O errors, not as "disk full", which is why the rule
takes half the free space and `grow` warns past it.

`ssf vm grow [--data-gib N]` enlarges an existing VM's data disk without
losing what is on it: with the VM stopped it checks the filesystem
(`e2fsck -f`), lengthens the file and resizes the filesystem to fill it
(`resize2fs`), then writes the new `data_gib`. Without `N` it grows to the
rule for today's free space; it never shrinks (a smaller disk means a new
VM). When the service owns the VM, stop the service first, since it
restarts a VM that goes away under it:

```sh
systemctl --user stop ssf.service    # or `ssf vm stop` for a VM started by hand
ssf vm grow                          # or: ssf vm grow --data-gib 200
systemctl --user start ssf.service   # or `ssf vm start`
```

`ssf vm status` shows the sizes and, with the guest reachable, the data
disk's use against its cap (`size:` line; `vcpus`, `mem_mib`, `data_gib`
and `data` in `--json`). `ssf doctor`, which runs inside the guest, fails
its data-disk line at 85 % used and its memory line when the guest has
under a tenth of its memory available (or anything swapped out, should
swap be added), each naming what to run.

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
`vm/guest/provision.sh` (`/usr/share/ssf/vm/` when installed). To change
it, copy that directory somewhere of your own, edit the copy, and run
`SSF_VM_DIR=<copy> ssf vm build --force` for a new image; a file edited
under `/usr/share/ssf/` is overwritten by the next package upgrade. The `ssf` binary is not in the image: every start takes the host's,
so the guest always runs the package you installed.

## What gets in, and what does not

At every start the host writes a small seed disk with the `ssf` binary,
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
browser when it can (the URL is printed either way) and, once the login
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
(`status`, `peers`, `sub`, `unsub`, `subs`, `tell`, `handover`, `release`,
`purge`, `doctor`, `run --once`) run inside the guest over that
connection, so the bar widget, `ssf status --json` and `ssf tell` work as
before; `ssf vm run -- <args>` does it explicitly and `ssf vm ssh
[-- cmd]` gives a shell. `ssf vm attach` attaches to herdr's session in
the guest in your terminal;
`ssf vm ssh-config` prints an `~/.ssh/config` entry so `herdr --remote
ssf-default` (herdr's thin client) and plain `ssh ssf-default` work too.
Clicking a session in the bar widget opens a terminal attached to the
guest. `ssf vm logs` follows the guest daemon's journal and `ssf vm
console` shows the serial console.

## Persistence

Each VM has, under `<vm.dir>/<name>/`, a `root.ext4` (a copy-on-write
copy of the image: instant on btrfs, a full copy elsewhere) with the
packages, and a `data.ext4` (`vm.data_gib`, sparse; see [Size](#size)) mounted at
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
