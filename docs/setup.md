# Setup

Install ssf, connect a bot account, and get an agent working on its first
issue. Follow steps 1–10 for a first installation; use steps 11–12 for
upgrades and removal. Commands marked **you** require your account,
browser sign-in, or sudo password.

**Choose your path first:**

- **A dedicated Linux host, VPS, or container**, including hosts without
  KVM or a systemd user session:
  use the [headless-host guide](headless-host.md), which replaces the VM and
  package-service steps below with standalone binaries and foreground processes.
- **Your local Linux machine, inside a microVM:** follow the package steps below.
- **Client-only SSH access to an existing factory:** use the
  [standalone client](install-binaries.md#client-only-operate-an-existing-factory-over-ssh).

SSF release packages support Arch-family Linux (including Omarchy) and
Debian-family Linux (including Ubuntu), on x86_64. macOS support is planned
next but is not supported yet. macOS-specific notes later in this guide describe
work in progress, not a supported installation path. The recommended local
setup runs the daemon, herdr and agents inside a Firecracker microVM. In the
host alternative, agents run as your Unix user and can access your home
directory and credentials. That alternative is in
[step 6](#alternative-on-the-host-in-herdr).
The bar widget and **Factory** menu are available only on Omarchy.

For everyday commands see the [README](../README.md#everyday-commands).
The installed guide is `/usr/share/doc/ssf/docs/setup.md`. The
`working-with-ssf` skill points to `ssf skill`; `ssf skill setup` prints this
document from the running binary. Agents should also read `ssf skill agent`.

## 1. Before you start

| Platform | Requirements for the recommended VM setup |
|----------|-------------------------------------------|
| Linux x86_64 | Omarchy, Arch, Debian 12+, or Ubuntu 24.04+; a systemd user session and usable `/dev/kvm` |

On Linux, check KVM access with `test -r /dev/kvm && test -w /dev/kvm`.
If access is missing, check the device permissions; on Debian/Ubuntu an
SSH-only user may need `sudo usermod -aG kvm "$USER"`, followed by a new
login (**you**). Linux without KVM or on another architecture can use
[lima with qemu](vm.md#backends); the release packages below are x86_64.

Allow roughly 30 GB of initial disk headroom for images and data, plus
space for repositories and builds. VM disks are sparse and grow with use;
`ssf vm build` chooses capacity from the machine's available resources.
See [VM sizing](vm.md#size).

You also need a subscription or API key for a supported coding harness.
Plan the choice using [step 8](#choosing-the-harness-and-the-model), then sign it in
where the agents run in step 7.

## 2. Install the package

Download the matching package from [GitHub Releases](https://github.com/mikekelly/simple-software-factory/releases)
and run the command for your platform (**you** for sudo):

| Platform | Install |
|----------|---------|
| Omarchy / Arch | `sudo pacman -U ssf-*.pkg.tar.zst` |
| Debian / Ubuntu | `sudo apt install ./ssf_*_amd64.deb` |

On Debian/Ubuntu, run `sudo apt update` before installing packages, including
on minimal images with stale or absent apt lists. The `.deb` depends on
`systemd`; installing that package does not make it PID 1 or create a working
systemd user session in a container. Use
[standalone + host mode](headless-host.md) there.

Ubuntu 24.04 and Debian 13 (Trixie) use the same amd64 `.deb`; their standard
repositories provide a sufficiently recent GitHub CLI. Debian 12 needs the
additional GitHub apt repository described below.

Package installation only installs SSF's files. It does not create user
configuration, authenticate a bot, enable the service, or install the optional
Omarchy widget. Prepare the current user explicitly after installation. The
recommended path creates one managed VM server named `ssf-server`, selected
implicitly because it is the only target, and enables only its target service:

```sh
ssf setup
```

If you have already chosen the less-isolated host alternative, create it as the
sole target before setup instead: `ssf server add local --local && ssf setup`.
Do not create both merely to compare them; adding the second target intentionally
makes unqualified commands require a selection.

The package contains the `ssf` client and the `ssf-server` daemon; there are no
separate client/server packages. For a client-only SSH installation or a Linux
platform without a package, see [standalone binaries](install-binaries.md). A local
client executes the adjacent server-side command endpoint. From any
machine with the package and SSH access to the factory account, add
`--server HOST` to the same command (or set `SSF_SERVER=HOST`):

```sh
ssf --server factory.example status
```

SSH starts the same endpoint on the remote machine; SSF opens no TCP listener.
The remote machine needs the package installed and `ssf-server` on `PATH`.
For repeat use, destinations can be assigned stable client-side names in
`~/.config/ssf/servers.toml`. One configured name is implicit; several require
`--server NAME`, with no persistent default. See the
[server catalog](configuration.md#server-catalog). Catalog-owned managed VMs
can also coexist when their runtime names, absolute VM directories and ports are
distinct; select each VM lifecycle command by name. The compatibility service
refuses to guess between several VMs. Once targets are migrated, stop the legacy
singleton and enable each
selected service with `ssf --server NAME ui service enable`; Linux uses
`ssf@NAME.service`. Setup follows the selected target; uninstall remains
installation-wide and refuses named local/VM targets.
An established enabled VM can be adopted without rebuilding or moving it with
`ssf server migrate-vm`; first confirm `ssf vm status` and the guest factory,
then run the migration and confirm `ssf --server ssf-server vm status` and
`ssf --server ssf-server status` before adding another target.

On Linux this creates the conventional target and enables
`ssf@ssf-server.service` for `default.target`. It asks before enabling systemd linger so
the service can start at boot and remain available after logout.

Before running the install command, supply any prerequisites the package
cannot provide:

- **Arch:** install `herdr` or `herdr-bin` from the AUR first. Omarchy's
  repositories supply herdr.
- **Debian 12:** install GitHub CLI 2.40+ from [GitHub's apt repository](https://github.com/cli/cli/blob/trunk/docs/install_linux.md)
  before ssf; Debian 12's bundled version is too old.
- **Debian and Ubuntu:** herdr is not a package dependency. For
  host sessions, install it with `curl -fsSL https://herdr.dev/install.sh | sh`.
  ssf also searches `~/.local/bin`. The VM installs its own herdr.
- **Linux VM image tools:** if `ssf vm build` reports a missing tool,
  install `fakeroot`, `curl`, e2fsprogs and libarchive's `bsdtar`:
  `sudo pacman -S --needed fakeroot libarchive e2fsprogs curl`,
  or `sudo apt install fakeroot libarchive-tools e2fsprogs curl`.
  The Firecracker guest is Ubuntu 24.04 LTS on every supported Linux host;
  the host does not need to run Ubuntu.

### Service and optional Omarchy widget

`ssf setup` is the only package setup step. The Linux unit works in Wayland,
X11, and headless sessions and starts through the user's `default.target`.
The service can start before bot login; in VM mode the guest starts without a
credential and becomes healthy after step 4.

On Omarchy, install the widget separately if you want its status display and
service controls:

```sh
omarchy plugin add https://github.com/mikekelly/simple-software-factory.git --enable
```

The widget never installs, upgrades, starts, or removes the SSF package.

For a live terminal view of active agents, run `ssf dashboard` in any terminal.
The Linux client includes it; no plugin or Python installation is
needed. Without `--server` or `SSF_SERVER`, it connects to all configured servers.
For a remote factory, use `ssf --server HOST dashboard` or set
`SSF_SERVER=HOST`. For a local VM, run the client on the host. See the
[dashboard guide](dashboard.md) for keyboard/mouse navigation, optional Herdr pane
focus, and explicitly enabling the server browser UI. The browser UI is off by
default and requires a server restart after changing its configuration.

Config and credentials live in `~/.config/ssf`, state in
`~/.local/state/ssf`, and VM files in `~/.local/share/ssf/vm` (lima also
uses `~/.lima`). Package examples are in `/usr/share/ssf`.

**Check:** run `ssf doctor`. Before setup is complete, failures for the
bot, service, driver, repositories and agent command links are expected.
Resolve unreadable config or a missing GitHub CLI / ssf binary now. A
missing host herdr is expected if you will use the VM.

Logs: `journalctl --user -fu ssf@ssf-server.service`.

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
| Access | added as a collaborator with **Write** on each watched repository | made an organisation member (a team with **Write** on the repositories, or per-repository) or an outside collaborator with **Write** on each |
| Boards | a collaborator on your projects | access to the organisation's projects (project **Settings → Manage access**), or the team's |
| Organisation settings | none | if the organisation restricts OAuth apps, an owner approves **GitHub CLI** (the browser sign-in in step 4 is an OAuth token from gh's app); if using a classic personal access token, check that organisation policy permits it; with SAML SSO, the token and the bot's SSH key have to be authorised for the organisation |

**3a. Create the account** (**you**). In a private browser window, sign
up at `https://github.com/signup` with a separate email address
(plus-addressing, `ann+bot@example.com`, works), verify the address, and
turn on two-factor authentication (GitHub requires it for accounts that
contribute code). Nothing else: no repositories, no keys, ssf enrolls
what it needs. Note the login; the next steps use it.

**3b. Owner invites the bot — on every watched repository** (**you**, using
an owner/admin session on your own computer). Under **Settings → Collaborators**,
invite the bot with **Write**, or run:

```sh
gh api repos/OWNER/NAME/collaborators/BOT -X PUT -f permission=push
```

**3c. Bot accepts — an invitation is not access.** After signing the bot in
(step 4), use a **bot-authenticated gh session** to list invitations, choose
the ID for the intended repository, and accept it. Verify `gh api user --jq
.login` names BOT first: SSF login may leave ordinary host gh using your own
account, and VM credentials stay in the guest. If you have no bot gh session,
use the bot's browser acceptance below instead:

```sh
gh api user/repository_invitations --jq '.[] | {id, repository: .repository.full_name}'
gh api user/repository_invitations/ID -X PATCH
gh api repos/OWNER/NAME --jq '{repository: .full_name, push: .permissions.push}'
```

The last command must show the intended repository and `push: true`. A pending
invitation can make private repositories return **404**, as if they were missing.
Alternatively accept in the bot's browser from the invitation email or
[notifications](https://github.com/notifications). See GitHub's
[invitation API](https://docs.github.com/en/rest/collaborators/invitations).

A server can instead accept invitations from trusted repository owners on each
poll:

```sh
ssf config set github.auto_accept_invitations_from '["OWNER"]'
```

The login match is case-insensitive. This only accepts the GitHub invitation;
it does not add the repository to the factory, select a named server, or enable
its service. Configure the intended factory separately with `ssf repo add`,
which is important when the same bot account is shared by multiple servers.

**3d. Project owner grants board access** (**you**). For each Projects (v2)
board, open **Settings → Manage access** and grant the bot **Write** (directly
or through a team). Repository Write alone does not authorize board moves;
the bot token also needs the `project` scope. See step 9 for board conventions.

**3e. Repository owner adds `SSF.md` on the default branch.** Follow step 9
before expecting a clean `ssf doctor`; a file only on a local or feature branch
is not enough.

## 4. Sign the bot in

**VM setup:** complete [the VM instructions in step 6](#the-vm-default)
first, then return here. Build, enable and start the VM before bot login or
factory configuration. The guest can start without a bot token; its daemon
will become healthy after onboarding. All the following auth and factory
commands then operate inside the guest, even when typed on the host.
A stopped or unreachable VM returns an error; start it and retry.

**Decide: browser device flow (the normal way), or a pasted token.**

- **Browser login.** `ssf auth login --web` runs the device flow with the
  scopes ssf needs and records the result. **You**: the terminal prints a
  one-time code and `https://github.com/login/device`; open it in the
  private window where the bot is logged in, enter the code and approve the
  scopes. Host mode may then ask `Use @<bot> as the bot account?` in the terminal.
  Over ssh, or when a browser must not open, `BROWSER=true ssf auth login
  --web`. In VM mode, plain `ssf auth login` also runs the device flow;
  `--user <bot>` checks that the approved account is the intended bot.
  The guest saves the result in `~/.config/ssf/token` on its persistent
  data disk, with no host bot keyring entry or seed copy.
  In host mode, plain login lists gh's accounts and offers the browser
  flow; `ssf auth login --user <bot> -y` selects an existing gh account.
  Host mode reads gh's keyring and switches gh back to your own account
  afterwards.
- **A pasted token.** **You**: logged in as the bot, at
  `https://github.com/settings/tokens` create a *classic* personal access
  token with the scopes below (a fine-grained token shows up as missing
  all of them, since ssf checks the classic scope list gh reports), then
  `printf '%s' "$TOKEN" | ssf auth login --token`. It is stored in
  `~/.config/ssf/token` (mode 0600). Choose this when the organisation
  forbids OAuth apps.

**Older gh compatibility (host mode).** If `ssf auth login --web` fails with
`unknown flag: --skip-ssh-key` (reported with gh 2.46), or SSF cannot pick up the
account after login, use the [direct gh device flow and token handoff](headless-host.md#3-sign-in-as-the-bot).
`--skip-ssh-key` is a gh option SSF passes internally; SSF's `--no-keys` skips
SSF key enrollment, but does not remove that gh flag from the web flow. Install
`openssh-client` on Debian/Ubuntu before enrolling keys (`ssh-keygen`).

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
Login and logout change this configuration and the credential only; they
leave the daemon's live session state alone. In VM mode they restart the
guest daemon to apply the credential change; repository and ordinary
configuration edits need no restart.
`ssf auth logout` revokes the keys and forgets the bot; the gh sign-in
itself stays where gh holds the account. `ssf token` prints the token for anything else that needs
it. The daemon authenticates on its next retry now that there is a token. VM
users already started the host service before this step. `ssf status` names the configured
account before the daemon first starts, then the account the daemon last
authenticated as. Removing the credential makes status report not signed in.

**Check:** `ssf auth status` must name the bot and its commit identity.
Run `ssf doctor` to check the token and enrolled key. Driver, repository
and agent-link failures can remain until the following steps.

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
machine the agents run on (inside the guest in VM mode) and an HTTPS `clone_url`, since SSH remotes always use the bot's
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

### The VM (default)

The VM runs its own daemon and herdr. Sessions run as the guest's `ssf`
user with passwordless sudo; the VM is the isolation boundary.

```sh
ssf vm build
```

The target service enabled by setup owns the VM and retries until the build is
available. If you previously disabled it, use
`ssf --server ssf-server ui service enable` (or unqualified `ssf ui service
enable` while it is the only target).

`ssf vm build` selects the platform's backend, sizes the VM, and records
those choices in `[vm]`. Existing sizes are preserved; use `--vcpus`,
`--mem-mib` or `--data-gib` for an explicit allocation. Read the printed
sizes and harness installation results. A failed harness installation
needs attention before you can sign it in. See [Backends](vm.md#backends)
and [Size](vm.md#size) for alternatives and disk growth.

**Check:** `ssf vm status` should report a running VM and working SSH.
The guest daemon may still need bot login from step 4. `ssf doctor` now runs in the guest: its paths are guest
paths, and herdr should be reachable. Repository and agent-link failures
can remain. Use `ssf vm status` to diagnose host VM tooling and
`ssf vm logs` for the guest daemon's log.

See [Inside a VM](vm.md) for shared files, persistence, attaching to
herdr, SSH access, rebuilding and resetting.

### Alternative: on the host, in herdr

With a package and a working user service manager, choose this shape before
the first setup by creating the sole local target:

```sh
ssf server add local --local
ssf setup
```

This uses isolated paths outside the legacy `~/.config/ssf` and
`~/.local/state/ssf` trees. An established host-mode installation is instead
registered in place as `local` when its next `ssf setup` runs.

On Linux without systemd user units, follow [the headless-host guide](headless-host.md):
on a fresh standalone install leave the server catalog empty, skip `ssf setup`,
and run `ssf-server` in the foreground under the same Unix user and environment
as the client. Keep it running in a separate terminal, or use your host supervisor.

SSF is built on top of herdr, which provides its workspaces and terminals.
The optional `driver = "herdr"` setting makes that choice explicit.

- **herdr** (the default): needs a running herdr server: start `herdr server`
  for headless operation, or interactive `herdr` in a terminal and leave it
  running (installation is in step 2). ssf clones under `herdr.projects_dir`
  (`~/ssf/projects`) and makes a worktree per item in `<name>.worktrees/`
  next to the clone. herdr only runs the agents it recognises (`herdr
  agent start --help`; `crush` is not among them), and `ssf repo add`
  warns about one it does not.
On the host, the default launch commands bypass harness permission prompts;
ssf adds no isolation boundary. Check: `ssf doctor` says `herdr reachable and
ready`. Details: [Workspaces and terminals](drivers.md).

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

Optionally, `ssf vm tailscale` installs Tailscale only inside the running guest
and starts its browser enrolment flow. See [Optional Tailscale
enrolment](vm.md#optional-tailscale-enrolment).

It runs the harness's own login inside the guest, in your terminal:
a page to open here and a code to paste back (Claude Code, Gemini,
OpenCode, Pi), a loopback OAuth callback (Oh My Pi), or a device code
(Codex, Copilot, Grok, Crush); the exact flow per harness is the table under [Harness
logins](vm.md#harness-logins). The credential lives on the guest's data
disk (`ssf vm reset` keeps it, `ssf vm destroy` removes it). API keys go
through the same commands (each offers the option). The alternative,
copying an existing login in with `vm.files`, makes the guest share
*your* session: a logout on either side, or Claude Code's token rotation,
ends both, so prefer `ssf vm login`.

For OMP, run `ssf vm login omp` on the computer with your browser, type
`/login`, and select the provider. Open OMP's short loopback `/launch` URL
after ssf establishes forwarding. Complete authorization in the host
browser and exit OMP; ssf carries the callback into the guest automatically.
Keep that terminal open throughout enrollment. OMP 18.1.18 stores its
credentials in `~/.omp/agent/agent.db`; a missing `auth.json` is normal.

**On the host**, use the harness's own login (`claude auth login`, `codex
login`, ...), which opens a browser.

Check: `ssf vm status` shows the harness as `logged in` on its `logins:`
line, and, once a repository names it, `ssf doctor` says `Claude Code
signed in inside the VM` (or `on the host`). A login that later expires
blocks the session; the fix is the same command again (step 10).

## 8. Watch a repository

Choose the harness and model below, then add the repository using the IDs
reported for your installation:

```sh
ssf repo add OWNER/NAME --harness HARNESS --model MODEL --effort EFFORT
ssf repo list --json
ssf candidates --repo OWNER/NAME
```

Replace the uppercase placeholders; omit `--effort` for a harness that
has no effort setting, and `--model` only for a harness without model support.
Both are required when supported. The bot needs Write
access, and the harness must be installed and signed in where it runs.
The daemon records GitHub's immutable repository id on its first pass. Later
renames and transfers should happen through GitHub as usual; ssf periodically
detects the new canonical name and repairs its configuration and checkouts.

The first successful poll after `ssf repo add` does not automatically start
allocations that are already assigned to, mention, or request review from the
bot. It records them as candidates instead. This avoids accidentally competing
with another factory when a repository is moved or a server is enabled in
error. Review them with `ssf candidates`, verify that no other factory owns the
selected work, then opt in only the items wanted here:

```sh
ssf adopt OWNER/NAME#3 [OWNER/NAME#8 ...]
```

Adoption creates or reuses the item's workspace, starts a fresh agent
conversation, and supplies the complete GitHub issue and timeline history. It
does not copy another harness's private conversation transcript. New
allocations arriving after enrollment continue to start normally. Removing and
adding the repository again creates a new enrollment and therefore a new
candidate snapshot; `ssf repo set` does not.

Use `ssf repo set OWNER/NAME` with the same options to change settings.
Changing the harness requires a fresh model and effort selection where supported.
Other edits retain existing selections; incomplete legacy entries must be completed
before `repo set` can save. Changes apply
to the next started or resumed session; an already running session keeps
its selection, and when the repository has running sessions `repo set`
says so on its output rather than leaving an operator to assume the change
took effect. `ssf status` and `ssf peers` then show both sides until the
session next launches: `harness codex → omp next launch (model
deepseek/deepseek-flash, effort high)`, and `next_launch` beside `harness`
in `--json`. `ssf handover <item> --harness <id> ...` moves one session
onto the new stack now. A
[per-item override](configuration.md#per-item-overrides)
takes precedence until that item's workspace is released: `ssf assign`
sets one for an item that has no session yet, `ssf handover` for one that
does.

### Choosing the harness and the model

Ask which subscriptions or API keys are available, what metered spending
is acceptable, and how much allowance must remain for interactive use.
Check the actual installation before proposing a model:

```sh
# Factory in the VM:
ssf vm status
ssf vm run -- agents
ssf vm run -- models HARNESS
ssf vm run -- agents --json

# Factory on the host:
ssf agents
ssf models HARNESS
ssf agents --json
```

`ssf agents` and `ssf models` are not automatically forwarded into the VM.
The guest commands matter especially for Pi, Oh My Pi and OpenCode,
whose model lists come from the installed harness. The JSON agent list
includes supported effort levels and launch commands.

Use current provider documentation for availability, pricing and plan
limits. [Artificial Analysis](https://artificialanalysis.ai/models)
can supplement this with capability and cost-per-task comparisons;
API costs do not measure consumption of a subscription allowance. If you
cannot verify a recommendation, say what remains unknown and let the
person choose. Before adding a repository, obtain their explicit choice of
model and effort (where supported). An agent must pause this setup step
until the person answers; examples and recommendations are not consent.

ssf selects the main session's model and effort. Optional subagents are
configured by the harness or requested through `SSF.md`; they may inherit
the session's model. Delegate only when an independent task benefits from
it, and check the harness's model-selection rules before budgeting for
subagents. There is no required three-tier agent hierarchy.

Keep model and effort out of `repo.command`: ssf appends those flags
itself. Clearing a supported model or effort setting is rejected.
Existing files with missing settings still load and run, but `ssf doctor`
fails with a repair command. SSF cannot establish who chose values already
present in a config file; setup agents must obtain human confirmation.
Unknown model IDs are passed through to the harness, rather than checked
against a fixed catalogue. See [Models and effort levels](configuration.md#models-and-effort-levels)
for IDs, aliases and harness-specific restrictions.

### Other repository settings

- **`command`:** defaults to a launch command that bypasses permission
  prompts, because terminals are unattended. See `ssf agents --json`
  and [Permissions](configuration.md#permissions) before changing it;
  an interactive approval prompt can leave a session waiting indefinitely.
- **`driver`:** optionally declares `herdr` for one repository.
- **`path`:** uses an existing host checkout; the VM clones for itself.
- **`clone_url`, `base_branch`:** override the clone URL and issue
  worktree base. SSH uses the bot's enrolled key; HTTPS uses a token.
- **`instructions`:** short additions to initial prompts. Put SSF-session
  workflow and board rules in `SSF.md`; put repository-wide working rules in
  `AGENTS.md`.

`ssf repo add` writes `[[repo]]` entries in the active factory's
`~/.config/ssf/config.toml`: inside the guest in VM mode, locally in host
mode. `ssf repo list` reads that same configuration.
Prefer the CLI for validation. For factory-wide settings, use
`ssf config set daemon.<key> <value>`; see [Configuration](configuration.md)
for polling, startup, instructions and event comments. Repository and
ordinary daemon settings are picked up on the next poll; changing VM or
service setup can require a restart as described above.

**Check:** `ssf doctor` should confirm the bot, driver, harness login,
repository access, allowed users, and commit identity. A missing checkout
is normal before the first issue. The `gh` and `ssf` command links are
created when the first agent starts, so that failure can remain until
step 10.

## 9. SSF agent guidance and boards

**`SSF.md`.** The operating contract for ssf-spawned agents belongs in this
file at the repository root: issue ownership and communication, board workflow,
delegation and handoffs, bounded review, completion and merge authority. ssf
appends it to the initial prompt; keep it short. Repository-wide build, test,
implementation, architecture, domain and safety policy belongs in `AGENTS.md`.
SSF injects `SSF.md` into the issue-owning main session only, not harness-created
subagents, so use it to define that agent's orchestration and completion role
without polluting delegated task contexts.
For optional context shared by every watched repository on this factory, add
`~/.ssf/SSF.md`; add `~/.ssf/SSF.codex.md`, `~/.ssf/SSF.claude.md`, or the
corresponding harness name for machine-wide harness-specific context. These
files live inside the guest in VM mode and on the host in host mode. They are
read before repository guidance and are not required by `ssf doctor`.
Add optional `SSF.codex.md`, `SSF.claude.md`, or `SSF.pi.md` at the root
for instructions appended only when that harness starts the session.
Describe one outcome per issue, where the plan lives, and who may merge. Keep
implementation tasks on that issue. Use `Refs #N`
for ongoing tracking and `Closes #N` only for complete delivery.

Start from `/usr/share/ssf/SSF.example.md` and adapt it, or use the checkout’s
[SSF.example.md](../SSF.example.md)
for a standalone installation. Commit it as `SSF.md` at the root of the
repository’s **default branch** before checking `ssf doctor`. `CLAUDE.md` and
`AGENTS.md` remain the place for repository policy shared by every agent,
whether or not ssf started it. `ssf doctor` reports missing SSF guidance
through the GitHub API; no clone is needed. See [The SSF agent guidance
file](configuration.md#the-ssf-agent-guidance-file).

**Review.** ssf does not start a separate reviewer session for an agent’s
own PR. The template uses self-review for documentation/tests and one
independent review for behavior changes, with at most one focused follow-up
for substantive fixes. Unresolved defects require simplification or a hold,
not an expanding loop. Validate the final change; package builds are for
packaging or installation changes. See [Second
opinions](sessions.md#second-opinions-the-gauntlet).

**Autonomy.** The template makes the agent owning the issue responsible for
merging after required validation and review, unless project rules or a
maintainer reserve merging for a human. Adapt that authority to your project;
when the agent cannot complete the next action, it must explicitly request a
human collaborator's review or decision. Delegation is optional and useful
only when an independent task warrants it.

**Boards.** First complete the separate board access grant in step 3d; repo
Write alone is insufficient. If the item is on a GitHub project (v2) board, the
agent's prompt lists the board, the card's Status and the command that
changes it, and the agent is told to keep it accurate. ssf never moves
cards; put board choices and status mappings in `SSF.md`. The bot needs access
to the board and a token with `project` scope.

## 10. The first issue, and what to expect

A good first issue is small and self-contained, says what "done" looks
like (a test that passes, a file that changes, a command that works), and
names what to run before opening a pull request. Assign it to the bot on
GitHub (@mentioning it, or a review request, works too).

- **Within a poll interval** (10 s) `ssf status` lists the item and a
  workspace labelled `<repo>-<issue-number>` appears in herdr (`ssf vm attach` shows the
  guest's herdr; on the host, your own herdr). The clone happens
  first, so the first item on a repository takes a little longer.
- **Within a couple of minutes** the agent comments on the issue with
  what it is about to do, under a `🤖#N says:` byline. That comment is
  the check that the whole chain works. `ssf peers` shows the session and
  what it is doing.
- **Then a pull request**, from the issue's branch, `Closes #N` in its
  description, and a comment on the issue with the link and the
  validation results. Read it, answer or merge as you would for a colleague,
  and close the issue when it is done; the agent pushes
  what is left, comments once more and gives its workspace back with
  `ssf release`.
- **If nothing happens**: `ssf doctor` first (it names most causes:
  token, driver, harness login, allowed users), then `ssf status` (the
  repository's last error is on it), then `journalctl --user -fu
  ssf@NAME.service` (in the VM, `ssf vm logs` for the guest daemon). An issue
  assigned by an account without Write is logged once and ignored (step
  5).
- **A workspace was closed by hand** (a herdr tab):
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
command from step 2 (`sudo pacman -U ssf-*.pkg.tar.zst` or `sudo apt
install ./ssf_*_amd64.deb`); on
Omarchy, once ssf is in its repository, `sudo pacman -Syu`. The
package's hook reloads systemd and restarts every active package-owned legacy
or target service in each running user session. The restart takes the guest
down and up with the new binary
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

For upgrades from the copied-config VM layout, follow [VM migration and
recovery](vm.md#upgrading-existing-vms). Legacy roots are refused before
boot. For Firecracker, install the matching ssf package and guest scripts,
then run `ssf vm build --force`, `ssf vm reset`, and `ssf vm start`.
Reset alone reuses the old image. For Lima, run `ssf vm reset`, then
`ssf vm start`. These workflows preserve the data disk. Conflicts stop
migration for an explicit choice; do not delete either config to force
an upgrade through.

Your config, state, keys and the VM's disks are preserved by an upgrade. After
the upgrade, `ssf doctor` should look as it did before.

## 12. Stopping and uninstalling

### Stop or restart later

`ssf ui service disable` stops the service and keeps it stopped across
logins. `ssf ui service enable` enables it again on Linux, including machines
without the Omarchy widget.

Host agent terminals survive a daemon stop; activity is delivered when
the daemon returns. In VM mode, stopping the host service shuts down the
guest too, and interrupted sessions resume when it starts again.

### Uninstall

1. Run `ssf uninstall`. Read its report and confirm once. It cleans up
   eligible closed-item workspaces, stops the service, removes the
   Omarchy UI, revokes the bot's enrolled keys, forgets its credential,
   and destroys the VM instance and its data disk.
2. Remove the package with the command it prints (**you** for sudo):
   `sudo pacman -R ssf` or `sudo apt remove ssf`.

Uninstall refuses if workspaces contain uncommitted or unpushed work,
or if VM work cannot be checked. Follow the refusal's specific remedy:
a stopped VM may need starting, while an orphaned lima disk or a disk
left by a backend switch needs different recovery. A failed service stop
halts removal before credentials or VM data are destroyed.

`--force` bypasses work-preservation checks: **VM clones and worktrees
are destroyed with the data disk, including unchecked or unpushed work.**
An agent must leave that decision to the person. `--yes` skips confirmation;
it does not replace `--force`.

By default, uninstall keeps host clones/worktrees, the VM image/download
directory, and ssf config/state. Add `--data` only if you also want config
and state removed. Retained state includes the disabled marker, so a
reinstall remains stopped until `ssf ui service enable`. The GitHub bot
account and its gh sign-in remain; lima's cache and separately installed
dependencies also remain. Inspect retained directories before deleting
them yourself. See the [uninstall reference](uninstall.md) for the full
cleanup sequence and disk-recovery cases.

## Checklist

- Package installed; `ssf doctor` can read config and find GitHub CLI.
- Bot account has repository Write access and any required board access.
- `ssf auth status` names the bot; commit identity is intentional.
- VM and guest daemon are running, or host herdr is ready.
- Harness is installed and signed in where sessions run.
- Repository is configured with a deliberate harness/model choice.
- `SSF.md` describes SSF issue ownership, board workflow, review and merge
  authority; `AGENTS.md` holds repository-wide working policy.
- First assigned issue produces an agent comment; `ssf doctor` is clean.
