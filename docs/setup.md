# Setup

From a fresh Linux machine (Omarchy, Arch, Debian or Ubuntu, Fedora) or Mac to the first issue worked by an agent: prerequisites, the package, the bot account, where the agents run, the first repository, upgrading, stopping and uninstalling. For whoever installs ssf, or the coding agent they ask to; read it top to bottom the first time. Once the bot is signed in, the later steps stand on their own for changing a factory.

Installed, this file is `/usr/share/doc/ssf/docs/setup.md` on Omarchy
and `$(brew --prefix)/share/doc/ssf/docs/setup.md` on macOS, next to the
[README](../README.md) (what ssf is, the everyday commands) and the rest
of `docs/` (one file per area, linked from each step). The `ssf-setup`
skill that ships in the repository is a pointer at this document, so an
agent asked to "set up ssf" follows the same steps; the lines that say
**you** do something are the ones an agent cannot do for you (type a sudo
password, create an account, sign in in a browser).

The path recommended here is the default: the whole factory (daemon,
herdr, agents) inside a VM, so the agents never see your home directory.
The VM is a Firecracker microVM on Linux and a lima instance on macOS;
the steps are the same, and where a command differs between the two the
macOS form follows the Linux one. The alternatives (the agents on this
machine, in herdr or in Orca; a driver per repository; commits under
your own name) are in the same steps, marked as such. The bar widget and
the **Factory** menu exist only on Omarchy; a Mac has the CLI and the
service.

## 1. Before you start

- **Linux on x86_64, or a Mac.** On Linux: Omarchy, Arch, Debian 12 or
  later, Ubuntu 24.04 or later, or Fedora; there is a package for each
  (step 2), and the Firecracker microVM image is x86_64 only. ssf runs
  as a per-user systemd unit, so your user needs a systemd user session
  (every desktop login has one; a server gets one with `loginctl
  enable-linger`, step 2, which serves the .deb and .rpm unit only: the
  Arch and Omarchy package's unit needs a Wayland login and never runs on
  a headless machine). The bar widget and the **Factory** menu are
  Omarchy's; elsewhere the CLI and the service are the whole of it. On
  macOS, ssf is a Homebrew formula with a `brew services` (launchd)
  service; the VM is a lima instance and needs macOS 13.5 or later for
  Apple's Virtualization framework. Apple silicon and Intel both work;
  no KVM or nested virtualisation is involved.
- **`/dev/kvm`** (Linux) usable by your user for the microVM:
  world-writable on Omarchy, Arch and Fedora. On Debian and Ubuntu a
  user logged in at the machine's seat gets access through udev, and a
  user who only comes in over ssh needs the `kvm` group (`sudo usermod
  -aG kvm $USER`, then log in again) (**you**). Without it, the factory
  runs on the host (step 6), or in lima over qemu (see
  [Backends](vm.md#backends)).
- **About 30 GB free** under `~/.local/share/ssf/vm` for the VM: on
  Linux an 8 GB root image plus a copy of it per VM, a 20 GB data disk
  (sparse, grows with use), the guest kernel and the Firecracker and
  gvproxy binaries; on macOS a 20 GB root disk and the data disk, both
  sparse, under `~/.lima`.
- **A coding agent and a way to pay for it**: a subscription or an API key
  for the harness you use (Claude Code, Codex, Gemini, Copilot, OpenCode,
  Pi, Oh My Pi, Grok, Crush). The agent is signed in where it runs, in
  step 7.
- **`gh` and `herdr`.** The package depends on the GitHub CLI, 2.40 or
  newer: `github-cli` on Omarchy and Arch, `gh` on Ubuntu 24.04 and
  Fedora, all from the distribution's own repositories; Debian 12 ships
  2.23, too old, so there `gh` comes from [GitHub's apt
  repository](https://github.com/cli/cli/blob/trunk/docs/install_linux.md),
  added before the package. herdr comes with the package on Omarchy
  (its repository has it) and, on Arch, from the AUR (`herdr` or
  `herdr-bin`, installed before the package, which depends on it). The
  .deb and .rpm do not depend on it, since no apt or dnf package exists:
  install it by hand, before or after the package, one of

  ```sh
  sudo curl -fsSL -o /usr/local/bin/herdr https://github.com/herdrdev/herdr/releases/latest/download/herdr-linux-x86_64 && sudo chmod +x /usr/local/bin/herdr
  curl -fsSL https://herdr.dev/install.sh | sh     # herdr's own installer, into ~/.local/bin; ssf finds it there
  ```

  While it is missing, `ssf doctor` prints ``FAIL herdr driver: CLI
  `herdr` not found; install it: ...`` with the command for this machine.
  On macOS the Homebrew formula pulls in `gh` and `lima`, not herdr: in
  VM mode herdr runs inside the guest, and a Mac needs it on the host
  only for the host alternative in step 6 or for `herdr --remote`
  (`brew install herdr`). Orca (`orca-ide-bin`) is only for the host
  alternative in step 6 and is installed by hand.
- `ssh` (openssh) is a dependency of every package and of the formula:
  `ssf auth login` makes the bot's key with `ssh-keygen` and the bot
  pushes over ssh. The microVM image (Linux) is built with `fakeroot`,
  `bsdtar` (libarchive), `mkfs.ext4` (e2fsprogs) and `curl`, all present
  on a stock Omarchy and recommended by the .deb and .rpm, so apt and dnf
  install them with the package. If `ssf vm build` says one is missing:
  `sudo pacman -S --needed fakeroot libarchive e2fsprogs curl`, `sudo apt
  install fakeroot libarchive-tools e2fsprogs curl` or `sudo dnf install
  fakeroot bsdtar e2fsprogs curl`. On macOS nothing beyond the formula's
  dependencies.

## 2. Install the package

The packages come from the latest release on GitHub (the repository's
Releases page; a `vX.Y.Z` tag builds and attaches them). Download the
one for this machine and install it (**you**: the package manager asks
for your sudo password):

- **Omarchy**: `ssf-<version>-1-x86_64.pkg.tar.zst`, `sudo pacman -U
  ssf-*.pkg.tar.zst`; `github-cli` and `herdr` come from Omarchy's
  repositories. Once ssf is in Omarchy's package repository, `sudo
  pacman -S ssf` instead.
- **Arch**: the same `.pkg.tar.zst`, after `github-cli` (`extra`) and
  `herdr` or `herdr-bin` (AUR), which it depends on: `sudo pacman -U
  ssf-*.pkg.tar.zst`.
- **Debian 12+, Ubuntu 24.04+**: `ssf_<version>-1_amd64.deb`, `sudo apt
  install ./ssf_*_amd64.deb` (apt resolves `gh`, `git` and `jq` from the
  repositories; `dpkg -i` would not). herdr by hand, step 1.
- **Fedora**: `ssf-<version>-1.x86_64.rpm`, `sudo dnf install
  ./ssf-*.x86_64.rpm`. herdr by hand, step 1.

The binary in the .deb and .rpm is static, so one file serves every
release of the distribution; the release also carries it bare, as
`ssf-<version>-linux-x86_64`, for anything that is not one of these
packages.

**What starts when.** The user unit `ssf.service` is enabled for every
user and started in your session by the install hook, so there is
nothing to enable. On Omarchy and Arch it starts with the graphical
(Wayland) session, which the bar widget lives in; the .deb and .rpm
ship a unit that starts with your systemd user manager at first login
(`default.target`), display or not. On a machine nobody logs in to, a
server, `loginctl enable-linger $USER` (**you**: sudo may be needed)
keeps the user manager, and so the factory, running with no session at
all; that is the .deb and .rpm unit only, the Arch and Omarchy package's
unit needs a Wayland login and never runs on a headless machine.

*Arch without a Wayland session* (an X11 desktop, a server reached over
ssh): the unit's `ConditionEnvironment=WAYLAND_DISPLAY` is never met, so
give it a drop-in that clears the condition and pull it into
`default.target`, the way the .deb and .rpm unit starts:

```sh
mkdir -p ~/.config/systemd/user/ssf.service.d
printf '[Unit]\nConditionEnvironment=\nConditionPathExists=!%%h/.local/state/ssf/disabled\n' > ~/.config/systemd/user/ssf.service.d/no-wayland.conf
systemctl --user daemon-reload && systemctl --user add-wants default.target ssf.service && systemctl --user start ssf.service
```

An empty `ConditionEnvironment=` resets every condition of the unit, so
the drop-in puts back the one for the disabled marker, and `ssf ui
service enable|disable` (the toggle) keeps working. `add-wants` makes the
symlink under `~/.config/systemd/user/default.target.wants/`, and from
then on `loginctl enable-linger $USER` applies to this machine too. When
the hook finds no running session it says so; `systemctl
--user daemon-reload && systemctl --user start ssf.service` starts it
now.

The package installs `/usr/bin/ssf` (the daemon and management CLI),
`/usr/bin/ssf-ui` (the helper behind the bar widget and the **Factory**
menu: service toggle, log, status terminal, open a workspace; installed
everywhere, useful only with the menu), the user unit above, the bar
widget under `/usr/share/ssf/omarchy-plugin/` (Omarchy: copied into
`~/.config/omarchy/plugins/ssf.factory` on the service's first start; it
shows the state of the factory, and the service toggle is its one
control), `/usr/share/ssf/SSF.example.md` and `config.example.toml`, the
microVM scripts under `/usr/share/ssf/vm/`, and this documentation under
`/usr/share/doc/ssf/`; the same paths on every distribution. Nothing
else: no config, no state, no account. Off Omarchy there is no widget
and no menu (`ssf ui install` says `not on Omarchy: no bar widget or
menu to install`); `ssf ui service enable|disable` works everywhere.

**On macOS** the package is a Homebrew formula in the tap
`mikekelly/homebrew-ssf` (**you**: Homebrew is yours to install first,
from `https://brew.sh`):

```sh
brew install mikekelly/ssf/ssf
```

It builds `ssf` from the release tarball and installs `ssf` under
`$(brew --prefix)/bin`, the VM scripts under `$(brew --prefix)/share/ssf/vm`
(with `config.example.toml` and `SSF.example.md` next to them), this
documentation under `$(brew --prefix)/share/doc/ssf`, and a `brew
services` definition that runs `ssf run` under launchd with its log at
`$(brew --prefix)/var/log/ssf.log`. It pulls in `gh` and `lima` as
dependencies. No `ssf-ui`, no widget, no menu (Omarchy only), and no
herdr: on a Mac the factory runs in the lima VM, where the guest installs
herdr for itself; `brew install herdr` is only for the host alternative
in step 6 or for `herdr --remote`. Nothing is started at install; the
service is started in step 6, once there is a bot to run as. The paths
are the same as on Linux: `~/.config/ssf` for the config and keys,
`~/.local/share/ssf` for the VM's files, `~/.local/state/ssf` for the
state, not `~/Library`, so one path holds everywhere in this
documentation and in the guest, the same way `gh` and the harness CLIs
keep their dotfiles.

Check:

```
$ systemctl --user status ssf.service | head -3     # macOS: brew services info ssf
$ ssf doctor
ok   config readable at /home/you/.config/ssf/config.toml
FAIL no bot account signed in; run `ssf auth login`
ok   herdr driver: CLI at herdr
FAIL herdr: herdr is not answering (is a herdr session running?): ...
note the daemon is not answering (`ssf sub|unsub|tell` need it): ...
FAIL 0 repositories configured
ok   GitHub CLI at /usr/bin/gh
FAIL gh and ssf links in /home/you/.config/ssf/bin not installed yet (ssf launch creates them when an agent starts)
ok   ssf on PATH at /usr/bin/ssf is this binary
ok   every post by the bot carried an origin tag
FAIL ssf.service running
note firecracker backend: /dev/kvm usable; [vm] enabled is false, so nothing here needs it until you turn the VM on
ok   bar widget enabled in ~/.config/omarchy/shell.json
ok   new clones go under /home/you/ssf/projects
Error: 5 problem(s) found
```

That is Omarchy. Elsewhere the widget line is a note, and until herdr is
installed (step 1) its line fails, naming the command for this machine:

```
FAIL herdr driver: CLI `herdr` not found; install it: sudo curl -fsSL -o /usr/local/bin/herdr https://github.com/herdrdev/herdr/releases/latest/download/herdr-linux-x86_64 && sudo chmod +x /usr/local/bin/herdr
note bar widget: not on Omarchy, nothing to enable
```

Every other `FAIL` is expected: there is no bot yet, so the service
cannot start (it exits and systemd retries it every 15 s until step 4),
no herdr session is running on the host (none is needed once the factory
is in the VM), no repository is watched, and the links are made when the
first agent starts. What has to be `ok` now is the config line, the
`herdr driver: CLI` line, the GitHub CLI, `ssf on PATH` and, on Omarchy,
the bar widget. The `note ... backend:` line is not a check but a
statement of what this machine has for the VM backend it would use, and
`ssf doctor` prints it on every host (inside the guest it is left out,
since the guest runs no VM of its own); while `[vm] enabled` is still
false it ends by saying so. Logs, at any point: `journalctl --user -fu
ssf.service`.

On a Mac the same output differs in five lines, none of them a problem:
the `herdr driver: CLI` line fails too until step 6 (no herdr on the
host; the guest brings its own), the `ssf on PATH` line names
`/opt/homebrew/bin/ssf` (or `/usr/local/bin/ssf` on Intel), the service
line names the launchd service rather than the systemd one and reads
`FAIL the ssf Homebrew service running` (`ssf doctor` checks it with
`launchctl`), failing until step 6 starts it, the backend line is `note
lima backend: limactl at /opt/homebrew/bin/limactl; ...` because lima is
the backend there and `brew` installed `limactl` alongside ssf, and the
widget line is the `note bar widget: not on Omarchy, nothing to enable`
above rather than a check, since that check runs on Omarchy only. Logs,
at any point: `tail -f $(brew --prefix)/var/log/ssf.log`.

## 3. Create the bot account

The bot is a GitHub account of its own. Every agent post is made as the
bot with a byline naming its session (`🤖#N says:`); a post by the bot
*without* a byline is treated as typed by a person, so sharing your own
account muddles who said what. The bot identity is a default, not a
security boundary: agents run as the Unix user (or as the guest user in
the VM), so the account only limits what `gh` does by default.

**Decide whose bot it is** and follow that column:

| | An individual | An organisation |
|---|---|---|
| Account | a fresh personal account, `<you>-bot` | a *machine user*: a fresh personal account owned by the organisation (`<org>-bot`), signed up with an address the organisation controls |
| Terms | GitHub allows one free machine account next to a personal one | the same; one machine user can serve every repository |
| Access | added as a collaborator with **Write** on each watched repository | made an organisation member (a team with **Write** on the repositories, or per-repository) or an outside collaborator with **Write** on each |
| Boards | a collaborator on your projects | access to the organisation's projects (project **Settings → Manage access**), or the team's |
| Organisation settings | none | if the organisation restricts OAuth apps, an owner approves **GitHub CLI** (the browser sign-in in step 4 is an OAuth token from gh's app); if it restricts classic personal access tokens, allow them; with SAML SSO, the token and the bot's SSH key have to be authorised for the organisation |

**3a. Create the account** (**you**). In a private browser window, sign
up at `https://github.com/signup` with a separate email address
(plus-addressing, `ann+bot@example.com`, works), verify the address, and
turn on two-factor authentication (GitHub requires it for accounts that
contribute code). Nothing else: no repositories, no keys, ssf enrolls
what it needs. Note the login; the next steps use it.

**3b. Give it access** (**you**). On each repository the factory should
watch, **Settings → Collaborators** (or the organisation's teams), add
the bot with **Write**: it pushes branches and opens pull requests. Give
it access to
any project board it should keep up to date (step 9). Accept the
invitation as the bot, in the private window, at
`https://github.com/notifications` or from the invitation email. Check,
with your own `gh`:

```sh
gh api repos/OWNER/NAME/collaborators/BOT/permission --jq .permission   # write or admin
```

## 4. Sign the bot in

**Decide: through gh (the normal way), or a pasted token.**

- **Through gh.** `ssf auth login --web` runs gh's device flow with the
  scopes ssf needs and records the result. **You**: the terminal prints a
  one-time code and `https://github.com/login/device`; open it in the
  private window where the bot is logged in, enter the code, approve the
  scopes, then answer `Use @<bot> as the bot account?` in the terminal.
  Over ssh, or when a browser must not open, `BROWSER=true ssf auth login
  --web`. Plain `ssf auth login` lists the accounts gh already holds and
  offers the browser flow; `ssf auth login --user <bot> -y` takes an
  account gh already knows without questions (the form an agent can run,
  once the bot is in gh's keyring). ssf reads the token from gh's keyring
  when it needs it and switches gh back to your own account afterwards.
- **A pasted token.** **You**: logged in as the bot, at
  `https://github.com/settings/tokens` create a *classic* personal access
  token with the scopes below (a fine-grained token shows up as missing
  all of them, since ssf checks the classic scope list gh reports), then
  `printf '%s' "$TOKEN" | ssf auth login --token`. It is stored in
  `~/.config/ssf/token` (mode 0600). Choose this when the organisation
  forbids OAuth apps.

**Scopes** the token needs: `repo` (issues, PRs, pushes), `project`
(boards), `admin:public_key` and `admin:ssh_signing_key` (key
enrollment). When the gh token lacks some, `ssf auth login` asks gh to
add them (another browser approval); `ssf doctor` reports what is missing
later. `--no-keys` skips the key and needs only `repo`.

**What login records**: `github.login` and `github.email`
(`id+login@users.noreply.github.com`; `--email`, or edit `email` in
`config.toml`, if the bot has a public address), and a dedicated ed25519
key under `~/.config/ssf/keys/`, enrolled on the bot account as both an
SSH key and a commit signing key. Agents then push over HTTPS with the
token or over SSH with that key, and every commit is signed with it; with
no key enrolled, signing is off rather than falling back to your key.
`ssf auth logout` revokes the keys and forgets the bot; the gh sign-in
itself stays. `ssf token` prints the token for anything else that needs
it. The service, which could not start in step 2, starts on its next
retry now that there is a token (on a Mac it is not started until step
6, so its line stays failed for now).

Check:

```
$ ssf auth status
Bot account: @acme-bot (User, id 12345678), token from gh keyring
Bot commit identity: acme-bot <12345678+acme-bot@users.noreply.github.com>
SSH key: /home/you/.config/ssf/keys/acme-bot_ed25519 (auth key id 1234, signing key id 567)
$ ssf doctor
ok   config readable at /home/you/.config/ssf/config.toml
ok   GitHub token belongs to @acme-bot
ok   bot SSH key /home/you/.config/ssf/keys/acme-bot_ed25519
ok   herdr driver: CLI at herdr
FAIL herdr: herdr is not answering (is a herdr session running?): ...
ok   daemon answering on /run/user/1000/ssf-....sock as @acme-bot
FAIL 0 repositories configured
...
```

Two failures left, both expected: no herdr session on the host (step 6
moves the factory into the VM, where herdr runs on its own) and no
repository (step 8).

**Alternative: commits under your own name.** By default the commits
are the bot's too (`acme-bot <id+acme-bot@users.noreply.github.com>`,
signed with its key). To have the history and your contribution graph
show you, while `gh`, the posts and the daemon stay the bot, set a `[git]`
table, instance-wide or per repository:

```sh
ssf config set git '{ name = "Ann Person", email = "ann@example.com" }'   # both at once; one alone is refused
ssf config set git.signing_key ~/.ssh/id_ed25519      # optional: sign with your key (unsigned otherwise)
ssf repo set acme/widgets --git-credential token:ann  # optional: push as @ann with the token gh holds for her
```

Before choosing it: the email must be verified on your GitHub account
(or be your `id+login@users.noreply.github.com`) for the avatar and the
graph; a signing key only shows *Verified* if it is registered on that
same account, so never sign a person's commits with the bot's key;
`credential = token:<login>` needs that account signed in to `gh` on the
machine the agents run on (in the VM, the token is copied to the seed
disk) and an HTTPS `clone_url`, since SSH remotes always use the bot's
key, and it puts your token within the agent's reach. Author and
committer are always the same identity. `ssf doctor` prints, per
repository, who commits, signed with what and who pushes, and fails when
the key or the token is missing. Details: [Committing as a
person](identity-and-bylines.md#committing-as-a-person-while-gh-stays-the-bot).

## 5. Who may drive the factory

Everything that reaches the bot on GitHub comes from whoever can write on
the repository, and a comment is relayed straight into a running agent's
terminal. By default the agents act only on assignments, mentions, review
requests, labels and comments from the repository's collaborators with
push access, which is GitHub's **Write** role or higher (**Settings →
Collaborators and teams**); the daemon fetches the list once per pass and
`ssf doctor` prints it per repository. Your own account must be on it to
assign issues to the bot. Nothing to set for the default; to narrow or
widen it:

```sh
ssf config set daemon.allowed_users '["alice", "bob"]'    # for every repository
ssf repo set acme/widgets --allowed-users alice,bob        # for one, replacing the instance list; [] is nobody but the bot
```

`"*"` means anyone on GitHub and is refused unless you type `yes` at the
terminal or pass `--accept-anyone-risk`; an agent setting this up must
not pass that flag for you. App accounts such as `github-actions[bot]`
count only when listed. If the collaborators cannot be fetched (an
organisation repository where the token lacks `read:org`, say) nothing is
acted on for that repository until a list is configured, and `ssf doctor`
says so. Details: [Who may drive the
factory](configuration.md#who-may-drive-the-factory).

## 6. Where the agents run: the VM, or the host

**Decide: the VM (default), or the host.** On the host the agents
run as your Unix user and can read your home directory, keyring and SSH
agent. `ssf vm` moves the daemon, herdr and every agent session into a
VM (a Firecracker microVM on Linux, a lima instance on macOS); the host
keeps only what builds, starts and reaches the guest. Inside, the driver
is always herdr (Orca is a desktop app), so a repository that says `orca`
runs in herdr there. Sessions run as the guest's `ssf` user, which has
passwordless `sudo` for everything: an agent there installs packages,
edits units and reboots the guest as it likes, and its first prompt says
so. The VM is the boundary; `ssf vm reset` or `ssf vm destroy` undoes
whatever it did. Take the host path when you want to watch the agents in
Orca, or a Linux machine has no `/dev/kvm` or is not x86_64 (lima over
qemu is the other way out there; see [Backends](vm.md#backends)).

### The VM (default)

Nothing here needs you at the keyboard, and nothing needs root.

```sh
ssf vm build                 # once, a few minutes: makes and provisions the guest (Linux: downloads Firecracker, gvproxy and a kernel, makes the image; macOS: creates the lima instance and boots it once)
ssf config set vm.enabled true
systemctl --user restart ssf.service   # the service starts the VM and owns it from now on; macOS: brew services start ssf
```

`ssf vm build` starts by settling the backend (`firecracker` on Linux,
`lima` on macOS, written to `config.toml` as `vm.backend` so the VM
keeps it) and sizing the VM from this machine, writing the sizes to
`config.toml` under `[vm]`: `vcpus` (the CPUs minus one, at least 2),
`mem_mib` (half the RAM, at least 4096) and `data_gib` (half the free
space of the filesystem the data disk will land on, at least 20; the
disk is sparse, so this reserves nothing). Which filesystem that is
depends on the backend: `vm.dir` under Firecracker, and under lima the
directory lima keeps its own disks in (`$LIMA_HOME/_disks`, by default
`~/.lima/_disks`), which can be another volume. The line it prints names
the path it measured:

```
VM backend: firecracker (for this machine)
this machine: 8 CPUs, 32768 MiB RAM, 500 GiB free on /home (measured at /home/you/.local/share/ssf/vm, [vm] dir)
VM size: 7 vCPUs (from this machine), 16384 MiB RAM (from this machine), 250 GiB data disk (from this machine; sparse, so it takes host space only as the guest writes)
written to /home/you/.config/ssf/config.toml under [vm] (backend, vcpus, mem_mib, data_gib); edit them there. The data disk itself is made once and only enlarged by `ssf vm grow`
```

On a Mac, where the backend is lima, that line ends `(measured at
/Users/you/.lima/_disks, lima's disk directory)` instead, and the free
space it reports is the one lima's disks draw on. The guest's `ssf`
binary there is the release asset for the installed version
(`ssf-<version>-linux-<arch>`), fetched once with `gh release download`,
and the guest downloads herdr's Linux release while it
provisions itself; the guest OS is Arch on Intel and Ubuntu LTS on Apple
silicon. `ssf vm build` ends with the instance stopped (`built lima
instance ssf-default; ssf vm start boots it`); the service start above
boots it. What differs under lima, key by key and command by command, is
in [Backends](vm.md#backends).

A value already in `[vm]` is kept, and `--vcpus`, `--mem-mib` and
`--data-gib` write a value of your own. Rule of thumb per parallel
session: about one vCPU and 2 GiB of RAM per active session, plus one
clone per repository and a build tree per worktree on the data disk;
`ssf vm grow` enlarges the data disk later without losing anything (see
[Size](vm.md#size)).

`ssf vm build` prints, at the end, one line per harness it could install
in the guest (whatever npm or a release tarball provide; best effort)
with its version or the failure; make sure yours is on it. Check:

```
$ ssf vm status
vm:       default (/home/you/.local/share/ssf/vm/default)
backend:  firecracker
tooling:  /dev/kvm usable
image:    built
state:    running (firecracker pid 12345)
ssh:      127.0.0.1:2222 answers
daemon:   active
size:     7 vCPUs, 16384 MiB; data disk 250 GiB, 0.4 of 250 GiB used (0%)
logins:   claude not logged in
$ ssf doctor
ok   config readable at /home/ssf/.config/ssf/config.toml
ok   GitHub token belongs to @acme-bot
ok   bot SSH key /home/ssf/.config/ssf/keys/acme-bot_ed25519
ok   herdr driver: CLI at herdr
ok   herdr reachable and ready
ok   daemon answering on /run/user/1000/ssf-....sock as @acme-bot
FAIL 0 repositories configured
...
```

On a Mac the `backend:` line says `lima`, `tooling:` says where lima is
(`limactl at /opt/homebrew/bin/limactl`), the `image:` line is
`instance: ssf-default (/Users/you/.lima/ssf-default)` and `state:` is
`running` without a pid; the rest is the same. The `tooling:` line is
the host's own, so it is where you see a missing `limactl` or, on a
Linux lima host, a missing `qemu-system-<arch>`; `ssf doctor` does not
answer that question here, because with the VM enabled it runs inside
the guest. `ssf doctor`, `status`, `peers`, `tell`, `sub`, `release`
and `purge` now run inside the guest
(the paths in their output are the guest's, and its service line is the
guest's systemd unit on either host OS), and the herdr line is `ok`: the
guest runs its own herdr session. The one failure left is the repository
(step 8). The sizes and `ssf vm grow`, the port (`vm.ssh_port`), what
gets into the guest and what persists, reaching it (`ssf vm attach`,
`ssf vm ssh`, `ssf vm logs`), and changing the image are in [Inside a
VM](vm.md).

### Alternative: on the host, in herdr or in Orca

The `driver` key picks where workspaces and terminals live; herdr when
unset. A `[[repo]]` can override it (`ssf repo add ... --driver orca`),
so one daemon can run some repositories in Orca and others in herdr; the
per-repository driver is ignored in the VM.

- **herdr** (the default): needs a running herdr session (start `herdr`
  in a terminal and leave it; where herdr comes from is in step 1). ssf clones under `herdr.projects_dir`
  (`~/ssf/projects`) and makes a worktree per item in `<name>.worktrees/`
  next to the clone. herdr only runs the agents it recognises (`herdr
  agent start --help`; `crush` is not among them), and `ssf repo add`
  warns about one it does not.
- **Orca**: needs `orca-ide-bin` installed (not in the Omarchy
  repository), signed in and running; `ssf config set driver orca`. The
  daemon waits for it at start. Workspaces are Orca worktrees linked to
  the issue number. `orca.command` defaults to
  `/usr/lib/orca-ide/bin/orca-ide` (the CLI; `/usr/bin/orca-ide` launches
  the app, so do not point at that).

On the host the agent's permission-free command (step 8, `command`) is
the whole of its sandbox. Check: `ssf doctor` says `herdr reachable and
ready` (or `orca reachable and ready`). Details: [Drivers](drivers.md).

## 7. Sign in the harness where the agents run

Each harness (the agent program: `claude`, `codex`, `gemini`, `grok`,
`pi`, `omp`, `opencode`, `copilot`, `crush`) is signed in once, by hand,
where the agents run. ssf does not handle first-run onboarding.

**In the VM** nothing from your home directory is visible, so the guest
needs a sign-in of its own (**you**, at the terminal and in a browser,
logged in to the harness's provider as yourself):

```sh
ssf vm login claude          # or codex, gemini, ...; without a harness it lists those installed in the guest and asks
```

It runs the harness's own browser-less login inside the guest, in your
terminal: a page to open here and a code to paste back (Claude Code,
Gemini, OpenCode, Pi, Oh My Pi) or a device code (Codex, Copilot, Grok,
Crush); the exact flow per harness is the table under [Harness
logins](vm.md#harness-logins). The credential lives on the guest's data
disk (`ssf vm reset` keeps it, `ssf vm destroy` removes it). API keys go
through the same commands (each offers the option). The alternative,
copying an existing login in with `vm.files`, makes the guest share
*your* session: a logout on either side, or Claude Code's token rotation,
ends both, so prefer `ssf vm login`.

**On the host**, use the harness's own login (`claude auth login`, `codex
login`, ...), which opens a browser.

Check: `ssf vm status` shows the harness as `logged in` on its `logins:`
line, and, once a repository names it, `ssf doctor` says `Claude Code
signed in inside the VM` (or `on the host`). A login that later expires
blocks the session; the fix is the same command again (step 10).

## 8. Watch a repository

One `[[repo]]` per watched repository in `~/.config/ssf/config.toml`;
`ssf repo add` writes it, and the CLI validates harness, model and effort
ids, so prefer it to editing the file. The bot needs Write on the
repository (step 3b). `ssf agents` lists the harness ids and which are
installed.

```sh
ssf repo add acme/widgets --harness claude
ssf repo set acme/widgets --model opus --effort high    # optional; ssf models claude lists the ids
ssf repo list --json
```

Decisions, per repository:

- **`harness`** (required): the agent program signed in in step 7.
- **`model`, `effort`**: optional. Claude, Codex, Gemini and Grok take
  Orca's model ids (`opus`, `sonnet`, `gpt-5.5`, ...) and effort levels;
  Pi, Oh My Pi, OpenCode and Copilot take their own `provider/model` ids.
  `ssf models <harness>` prints the choices. Changing the harness resets
  both. See [Models and effort
  levels](configuration.md#models-and-effort-levels).
- **`command`**: unless set, every harness starts with its
  permission-free command (`ssf agents --json` shows it as
  `launch_command`, e.g. `claude --dangerously-skip-permissions
  --disallowedTools AskUserQuestion`), because nobody sits at the terminal
  to approve anything and an agent that asks waits forever. In the VM
  that is fine: the VM is the wall. On the host it is the whole of the
  agent's sandbox, so decide whether that is acceptable, or set a
  permission mode or tool deny list in the agent's own syntax (`ssf repo
  set acme/widgets --command "..."`; `--clear command` to go back). See
  [Permissions](configuration.md#permissions). Behavioural limits (do not
  merge, do not close issues) go in `SSF.md` (step 9), not here.
- **`driver`**: only when this repository should run in a different
  driver than the default (host only).
- **`path`**: an existing checkout instead of a clone (host only; the
  guest clones for itself). **`clone_url`**: an SSH URL for private
  repositories (the bot's enrolled key is used). **`base_branch`** for
  issue worktrees.
- **`instructions`**: a line or two appended to this repository's initial
  prompts; anything longer belongs in `SSF.md`.

`[daemon]` keys worth a look (`ssf config set daemon.<key> <value>`):

| Key | Default | Decide |
|-----|---------|--------|
| `poll_interval_secs` | `10` | Unchanged listings cost nothing against the rate limit, so the default is fine; raise it on a busy token |
| `instructions` | | House rules for every repository this daemon watches; per-repository ones go in `SSF.md` |
| `resume_on_start` | `true` | Bring interrupted sessions back after a reboot; `startup_driver_wait_secs` (`120`) is how long to wait for the driver first |
| `allowed_users` | the collaborators with Write | Step 5 |
| `event_comments` | `true` | The daemon posts a short `ssf` block on an issue when it attaches a session to it, brings one back, holds it for a sign-in, gives up on it or releases its workspace; `false` (or `ssf repo set <owner/name> --event-comments false`) if the timeline should hold only what agents and people write |

Every key, with its default: [Configuration](configuration.md);
`/usr/share/ssf/config.example.toml` (macOS:
`$(brew --prefix)/share/ssf/config.example.toml`) has each with a
comment. Changes are picked up on the next poll; no restart needed.
Check:

```
$ ssf doctor
ok   config readable at /home/ssf/.config/ssf/config.toml
ok   GitHub token belongs to @acme-bot
ok   bot SSH key /home/ssf/.config/ssf/keys/acme-bot_ed25519
ok   herdr driver: CLI at herdr
ok   herdr reachable and ready
ok   Claude Code signed in inside the VM (claude auth status: signed in (claude.ai))
ok   daemon answering on /run/user/1000/ssf-....sock as @acme-bot
ok   1 repository configured
ok   acme/widgets: allowed users: @ann, @acme-bot (collaborators with push access)
ok   acme/widgets: harness `claude` installed
ok   acme/widgets: no checkout yet (the first session clones it under /var/lib/ssf/projects)
ok   acme/widgets: commits as acme-bot <12345678+acme-bot@users.noreply.github.com> (the bot), signed with /home/ssf/.config/ssf/keys/acme-bot_ed25519, pushes as @acme-bot (the bot)
ok   acme/widgets: signing key /home/ssf/.config/ssf/keys/acme-bot_ed25519 present
ok   GitHub CLI at /usr/bin/gh
FAIL gh and ssf links in /home/ssf/.config/ssf/bin not installed yet (ssf launch creates them when an agent starts)
ok   ssf on PATH at /usr/bin/ssf is this binary
ok   every post by the bot carried an origin tag
ok   ssf.service running
note bar widget: checked on the host, not inside the VM
ok   new clones go under /var/lib/ssf/projects
Error: 1 problem(s) found
$ ssf status
bot:     acme-bot
service: running
polled:  2026-09-07T10:12:03Z
driver:  0 workspaces
config:  /home/ssf/.config/ssf/config.toml
```

The links line clears itself when the first agent starts. Everything
else `ok` means the chain is complete: token, key, driver, harness signed
in where the agents run, the repository and who commits on it.

## 9. Project notes and boards

**`SSF.md`.** ssf's own prompts carry only what ssf owns (which bot the
agent is, that the terminal is unmanned, that `gh` acts as the bot, `ssf
guide`). How the repository wants work done goes in an `SSF.md` at its
root, appended to every initial prompt: comment when starting, when a
decision is needed and when done; ask on the item rather than guess (the
agent is woken when someone answers); branch and PR conventions (work on
the item's branch, `Closes #N`, do not merge or close, who merges);
what to run before a PR; what the board columns mean; the gauntlet.
Start from `/usr/share/ssf/SSF.example.md` (macOS:
`$(brew --prefix)/share/ssf/SSF.example.md`). `CLAUDE.md` and `AGENTS.md`
stay for what every user of the repository wants; `SSF.md` is for what
only ssf agents need. `ssf doctor` reports a repository without the file
(`FAIL no SSF.md in owner/name; start from /usr/share/ssf/SSF.example.md`),
read through the GitHub API, so the check needs no clone. Details: [The
per-project prompt file](configuration.md#the-per-project-prompt-file).

**The gauntlet.** ssf runs one session per item and starts no reviewer
for an agent's own pull request (it used to, on a `review` label; that
went with #115). The boilerplate's gauntlet rule is what stands in: the
agent that did the work hands the diff, the issue and its claim of what
the change does to a fresh agent that has not seen its reasoning, asks it
to break the work, fixes what it finds and repeats until nothing that
matters is left, then says on the item what was found. A subagent of its
own harness is the default; for complex, risky or important work it uses
herdr for a different agent and model, with the invocation from `ssf
guide`. Keep the rule, or write your own; nobody re-reviews after the
agent. Details: [Second
opinions](sessions.md#second-opinions-the-gauntlet).

**Autonomy.** How far the agents go on their own is a line in `SSF.md`,
and the choice is yours: at one end, everything is approved by a person
(open the pull request, say the gauntlet passed, and stop; a person
reviews and merges); at the other, no approval is needed (use your
judgment and gauntlet loops to address the issue and close it out); in
between, approval for merges only, say. The boilerplate ships the
cautious end, with the other end in a comment next to it (comments are
stripped before the notes reach an agent), so flipping it is an edit of
that one line. The boilerplate also says whose issue it is:
the agent is in charge of it, and its job is to clarify the intended
outcome, plan the delivery and orchestrate subagents that do the work,
keeping its own context for managing the issue rather than for
implementation detail.

**Boards.** No setup: if the item is on a GitHub project (v2) board, the
agent's prompt lists the board, the card's Status and the command that
changes it, and the agent is told to keep it accurate. ssf never moves
cards; put the conventions in `SSF.md`. The bot needs access to the board
(step 3b).

## 10. The first issue, and what to expect

A good first issue is small and self-contained, says what "done" looks
like (a test that passes, a file that changes, a command that works), and
names what to run before opening a pull request. Assign it to the bot on
GitHub (@mentioning it, or a review request, works too).

- **Within a poll interval** (10 s) `ssf status` lists the item and a
  workspace named after it appears in herdr (`ssf vm attach` shows the
  guest's herdr; on the host, your own herdr or Orca). The clone happens
  first, so the first item on a repository takes a little longer.
- **Within a couple of minutes** the agent comments on the issue with
  what it is about to do, under a `🤖#N says:` byline. That comment is
  the check that the whole chain works. `ssf peers` shows the session and
  what it is doing.
- **Then a pull request**, from the issue's branch, `Closes #N` in its
  description, and a comment on the issue with the link and what the
  gauntlet found. Read it, answer or merge as you would for a colleague,
  and close the issue when it is done; the agent pushes
  what is left, comments once more and gives its workspace back with
  `ssf release`.
- **If nothing happens**: `ssf doctor` first (it names most causes:
  token, driver, harness login, allowed users), then `ssf status` (the
  repository's last error is on it), then `journalctl --user -fu
  ssf.service` (macOS: `tail -f $(brew --prefix)/var/log/ssf.log`; in
  the VM, `ssf vm logs` for the guest daemon). An issue
  assigned by an account without Write is logged once and ignored (step
  5).
- **A workspace was closed by hand** (a herdr tab, an Orca worktree):
  the git checkout under `<checkout>.worktrees/` survives, and so does
  whatever it holds. `ssf doctor` prints a `WARN` line per repository
  naming every such checkout with commits on no other branch and not on
  origin, or uncommitted changes, and no agent on it. For an active
  item `ssf tell <item> "..."` brings the session back in that checkout;
  a retired item's branch is pushed by hand. Do not delete the directory
  or `ssf purge --force` first: that loses the uncommitted changes and
  leaves the commits on a local branch nothing lists. See
  [Workspaces after
  close](sessions.md#workspaces-after-close-release-and-purge).
- **A session shows as blocked** when its harness login expired or was
  revoked under it. `ssf status` prints a `BLOCKED:` line naming the
  harness and the fix, the item gets one `blocked` post from the daemon
  (`🤖 ssf`, with the fix), and nothing is lost:
  sign in again (`ssf vm login <harness>`, or the harness's own login on
  the host) and ssf resumes the session and delivers what it held. See
  [A harness that is not signed
  in](sessions.md#a-harness-that-is-not-signed-in).

The everyday commands (`status`, `peers`, `tell`, `sub`, `release`,
`purge`) are in the [README](../README.md#everyday-commands); agents get
their own reference from `ssf guide`.

## 11. Upgrading

Upgrade the package like any other: the next release's file with the
command from step 2 (`sudo pacman -U ssf-*.pkg.tar.zst`, `sudo apt
install ./ssf_*_amd64.deb`, `sudo dnf install ./ssf-*.x86_64.rpm`); on
Omarchy, once ssf is in its repository, `sudo pacman -Syu`. The
package's hook restarts `ssf.service` in every running user session (or
tells you to, when it finds none). On macOS it is `brew upgrade ssf`,
then `brew services restart ssf`, since Homebrew restarts nothing on its
own; the restart takes the guest down and up with the new binary
(fetched from the release as the guest's `ssf-<version>-linux-<arch>`
at that start), the same as `ssf vm restart` for a VM started by hand.
What that restart means:

- **On the host, nothing for the agents.** A daemon restart is invisible
  to them: their terminals stay where they are, and the daemon delivers
  what they missed when it is back.
- **In the VM, the guest is restarted** with the new `ssf` binary (every
  start takes the host's), which shuts the guest down cleanly, and with
  it the agent sessions inside. On the way back up the guest daemon's
  startup pass starts every interrupted session again, resuming its
  conversation, as after a reboot on bare metal. A VM you started by hand
  (`ssf vm start`, no service) needs `ssf vm restart` yourself.

Your config, state, keys and the VM's disks are untouched by an upgrade;
a renamed key keeps loading under its old name. `ssf doctor` after the
upgrade should look as it did before.

## 12. Stopping and uninstalling

**Stopping.** `ssf ui service disable` (the same as the bar widget's
toggle) stops the service and keeps it from starting at the next login;
`enable` turns it back on. It holds on both platforms, by different
means: it writes `~/.local/state/ssf/disabled` either way, and then on
Linux runs `systemctl --user stop ssf.service` -- the marker is what
keeps the next login from starting it, through the unit's
`ConditionPathExists` -- and on macOS runs `brew services stop ssf`,
which unloads the launchd agent until `brew services start ssf`. By hand,
`systemctl --user stop ssf.service` stops it only until the next login,
while `brew services stop ssf` does hold; what it does not do is leave
the marker, so `ssf doctor` and the bar widget report the daemon as
merely not running rather than as disabled. Running agents are
left where they are: nothing reaches them while the daemon is down, and
it delivers what they missed when it comes back. With the factory in the
VM, stopping the service shuts the guest down cleanly and its sessions
come back with it.

**Uninstalling** is one command and one step for you:

1. `ssf uninstall`: reports what it will stop, remove and revoke, lists
   the items and the state of their workspaces, and asks once. Then, in
   the order the pieces depend on each other: `purge` of the clean and
   pushed workspaces of closed items (needs the running daemon; skipped
   when it is down), `ui service disable` (with `vm.enabled` that shuts
   the guest down; on macOS this is `brew services stop ssf`), `ui
   uninstall` (the bar widget and menu, Omarchy only), `auth logout`
   (revokes the bot's keys on GitHub and forgets it), `vm destroy` (under
   the lima backend the lima instance `ssf-default` and its disk
   `ssf-default` too, on whichever OS you run it). Each step tolerates
   the thing being gone already, so a second run, or a run on a
   half-uninstalled machine, is fine. With the factory in the VM the
   report and the purge come from the guest, before it goes. The one
   step that can end the run early is `ui service disable`: everything
   after it destroys something, and none of it may happen while the
   daemon might still be working, so a service that would not stop
   leaves the machine as it was and tells you to stop it by hand
   (`systemctl --user stop ssf.service`, or `brew services stop ssf`)
   and run `ssf uninstall` again.
2. `sudo pacman -R ssf`, `sudo apt remove ssf` or `sudo dnf remove ssf`
   (**you**: sudo; nothing in ssf runs it); on macOS `brew uninstall
   ssf`, then `brew untap mikekelly/ssf` (`gh` and `lima` stay unless
   you `brew uninstall` them). The command prints the one for this
   machine last.

What stops it: a workspace with uncommitted or unpushed work (an open
item's too), one that cannot be checked (no origin, a git error), or a
VM that is stopped so the clones on its data disk cannot be checked. Push or discard the work (`ssf vm start` to check a
stopped VM), or pass `--force` to go ahead: on the host the work stays
where it is; in the VM the clones live on its data disk and are
destroyed with it, checked or not. `--yes` skips the question for
scripted use.

What it keeps, and lists at the end: the clones and worktrees under
`~/ssf/projects` (or Orca's projects; may hold unpushed work), the `[vm]
dir` (the image and downloads, safe to remove), and, unless you pass
`--data`, `~/.config/ssf` (config and the bot's key) and
`~/.local/state/ssf` (state, and the marker that keeps a disabled service
off, so a reinstall stays stopped until `ssf ui service enable`; with
`--data` gone, a reinstall starts the service). Under lima the instance
and the data disk go out of lima's own home with `vm destroy`, but
`~/.lima` itself stays, holding lima's cache of downloaded images; the
report does not name it, so remove it by hand once nothing else of yours
uses lima. The bot GitHub account itself is not touched, nor its gh
sign-in. `ssf status` afterwards says not signed in and stopped; the
watched repositories and the records of past items still show until
`--data` (or a reinstall from scratch) clears them. With the VM gone the
config's `vm.enabled` is cleared, so `status` does not go looking for it.

## Checklist

1. The package for this machine (**you**): `sudo pacman -U
   ssf-*.pkg.tar.zst`, `sudo apt install ./ssf_*_amd64.deb`, `sudo dnf
   install ./ssf-*.x86_64.rpm` or, on macOS, `brew install
   mikekelly/ssf/ssf`, plus herdr by hand off Omarchy on Linux; `ssf
   doctor` fails only on the bot, herdr, the repository and the links
   (on a Mac, the service line too: nothing is started at install).
2. Bot account created (**you**), with Write on each repository and
   access to the boards.
3. `ssf auth login --web` as the bot (**you**, in a private window);
   `ssf auth status` names it.
4. Commits as the bot (default) or as you (`[git]` table).
5. `ssf vm build`, `ssf config set vm.enabled true`, `systemctl --user
   restart ssf.service` (macOS: `brew services start ssf`); `ssf vm
   status` running, daemon active. (Host alternative: a herdr session
   running, or Orca with `driver = "orca"`.)
6. `ssf vm login <harness>` (**you**, at the terminal), or the harness's
   own login on the host; `logged in` on `ssf vm status`.
7. `ssf repo add owner/name --harness <id>`; `ssf doctor` clean but for
   the links line.
8. `SSF.md` at the repository root.
9. Assign an issue to the bot; a workspace appears and the agent
   comments.
10. To undo all of it later: `ssf uninstall` (add `--data` to drop
    config and state too), then the package manager's remove command it
    prints (**you**).
