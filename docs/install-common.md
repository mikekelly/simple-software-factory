# Install: the steps every route shares

The steps each route document links in order, after [install.md](install.md) (`ssf skill setup`) has chosen where the factory runs. Each route document ([install-local.md](install-local.md) `ssf skill setup-local`, [install-server.md](install-server.md) `ssf skill setup-server`, [install-client.md](install-client.md) `ssf skill setup-client`) names, for its route, the machine the daemon runs on (where you run `ssf setup` and the commands after it) and, when the agents are not on the machine you are working on, the SSH host that reaches them (HOST below). The steps here use those names and do not repeat the routes.

## 3. Choose the runtime

Run [the probes](install.md#2-choose-where-the-factory-runs) on the machine the daemon will run on, as your route says, then take the first rung below whose conditions hold, in this order. Your route document adds any conditions of its own. Tell the person the rung's one-line tradeoff before going on.

### VM rung

Conditions: Linux with `/dev/kvm` readable and writable (bare-metal servers usually; some VPSes offer nested virtualisation) and `systemctl --user` answering, or macOS (lima); and the machine can spare a VM by the rule in [install.md](install.md#is-a-vm-reasonable-here) (`ssf skill setup`). This is the default.

Tell the person: the VM keeps the agents away from everything else on the machine, but takes a share of its CPU and memory.

Then: [4.1 Linux package](#41-linux-package) or [4.2 macOS, Homebrew](#42-macos-homebrew), and later [6.1 The VM](#61-the-vm).

### Incus guest rung

Conditions: Linux without usable `/dev/kvm`, `systemctl --user` answering, a distribution package fits ([4.1](#41-linux-package)), and root is available once to set up Incus on a kernel that supports it.

Tell the person: an Incus guest is a user-namespaced system container sharing the host kernel, weaker isolation than a VM but still separate from the host's users and files; it needs root once for the Incus setup.

Then, before installing: set Incus up with the commands in [platform-specifics.md#incus](platform-specifics.md#incus) (`ssf skill specifics`). They need root: give them to the person to run on that machine (over SSH when it is not this one), or run them yourself when you already have non-interactive root there (`sudo -n true` succeeds). Then select the backend, before `ssf vm build`:

```sh
ssf config set vm.backend incus
```

Then [4.1 Linux package](#41-linux-package), and later [6.1 The VM](#61-the-vm), which builds the Incus guest.

### Docker container rung

Conditions: Linux, no rung above fits (no usable KVM, and Incus cannot be set up or its networking does not work), and Docker runs here or root is available once to install it.

Tell the person: the factory runs in host mode inside a Docker container, as that container's own user with root inside it, so agents administer their own environment while the person's files stay outside; it shares the host kernel and is weaker isolation than a VM or an Incus guest.

Then: build and start the container with [platform-specifics.md#docker-container](platform-specifics.md#docker-container) (`ssf skill specifics`), which installs the package in it, and follow your route's Docker steps.

### Host mode rung

Conditions: no rung above fits (no usable KVM, and neither Incus nor Docker can be set up, for want of root or a supported kernel; no systemd user session; no distribution package for a guest; or a Mac too small for a VM), the machine has enough CPU and RAM for the sessions, and the person accepts the tradeoff below.

Tell the person: the agents run as a Unix user on the machine itself, see that user's files and credentials, run with the harness's permission prompts bypassed, and are kept from anything else on the machine only by Unix permissions.

Then: [4.1 Linux package](#41-linux-package), [4.2 macOS, Homebrew](#42-macos-homebrew) or [4.3 Standalone binaries](#43-standalone-binaries), and later [6.2 Host mode](#62-host-mode).

## 4. Install ssf

### 4.1 Linux package

Download the matching asset from [GitHub Releases](https://github.com/mikekelly/simple-software-factory/releases) yourself, print its absolute path, then give the person the install command for their family with that path filled in, and wait for them to confirm it ran:

| Family | Command |
|---|---|
| Arch | `sudo pacman -U ssf-*.pkg.tar.zst` |
| Debian / Ubuntu | `sudo apt install ./ssf_*_$(dpkg --print-architecture).deb` |
| Fedora / RHEL | `sudo dnf install ./ssf-*.$(uname -m).rpm` |

On a fresh server `gh` is not there yet (the package brings it) and is not signed in, so download the public asset with `curl`:

```sh
tag=$(curl -fsSL https://api.github.com/repos/mikekelly/simple-software-factory/releases/latest | grep -o '"tag_name": *"[^"]*"' | cut -d'"' -f4)
curl -fsSLO "https://github.com/mikekelly/simple-software-factory/releases/download/$tag/ASSET"   # ASSET: the file name for the family, from the release page
```

Where `gh` is already installed, `gh release download --repo mikekelly/simple-software-factory --pattern PATTERN` does the same.

One package holds both the `ssf` client and the `ssf-server` daemon. The `.deb` and `.rpm` are built for x86_64 and aarch64; the Arch package for x86_64 only. For a guest, install nothing else on that machine: the `.deb` and `.rpm` bring `gh`, Git and jq, and `ssf vm build` provisions the guest.

Prerequisites the package does not always bring: GitHub CLI 2.40 or newer, Git, jq, an OpenSSH client (`ssh-keygen` enrolls the bot's key), and, for host mode, herdr, which only Omarchy's repositories carry as a package. The VM installs its own herdr in the guest. Where to get herdr and the other distro-specific commands and quirks are in [platform-specifics.md](platform-specifics.md). tmux is optional: scratch sessions run in herdr, and tmux is used only to reach one still running in tmux from an older ssf until it next starts ([sessions.md](sessions.md#scratch-sessions)).

```sh
ssf --version
command -v ssf-server
```

### 4.2 macOS, Homebrew

```sh
brew install mikekelly/tap/ssf
ssf --version
```

The formula brings `gh` and `lima`. [5. `ssf setup`](#5-ssf-setup-and-the-service) enables a launchd agent per target, so `brew services` is not used. The VM path needs nothing more; for host mode on the Mac, `brew install herdr` as well. Details in [platform-specifics.md](platform-specifics.md#macos).

### 4.3 Standalone binaries

For host mode only, on a server or on the person's machine, when no package fits. A guest needs the package.

Releases publish static musl Linux binaries for `x86_64` and `aarch64`. Take the client and the server from the **same release** and install both under unversioned names in the same directory. Do not pin a version here: pick the current release.

```sh
arch=$(uname -m)                 # x86_64 or aarch64
dir=$(mktemp -d)
tag=$(curl -fsSL https://api.github.com/repos/mikekelly/simple-software-factory/releases/latest | grep -o '"tag_name": *"[^"]*"' | cut -d'"' -f4)
base=https://github.com/mikekelly/simple-software-factory/releases/download/$tag
curl -fsSL "$base/SHA256SUMS" -o "$dir/SHA256SUMS"
for f in $(grep -o "ssf-\(server-\)\?[0-9][^ ]*-linux-$arch\$" "$dir/SHA256SUMS"); do curl -fsSL "$base/$f" -o "$dir/$f"; done
(cd "$dir" && sha256sum --check --ignore-missing SHA256SUMS)
install -Dm755 "$dir"/ssf-[0-9]*-linux-$arch   "$HOME/.local/bin/ssf"
install -Dm755 "$dir"/ssf-server-*-linux-$arch "$HOME/.local/bin/ssf-server"
export PATH="$HOME/.local/bin:$PATH"
ssf --version && ssf-server --version
```

The `SHA256SUMS` check stops on a tampered or truncated download; a release older than the one that introduced it has no `SHA256SUMS`, so drop those two lines there. Persist that `PATH` line for future shells. Supply the prerequisites yourself with the host's package manager (refresh its indexes first on minimal images): CA certificates, curl, Git, jq, GitHub CLI 2.40 or newer, an OpenSSH client, herdr, and the harness. The bare binaries carry no service units, no VM scripts and no configuration examples, so skip `ssf setup` on this path and leave the server catalog empty and `SSF_SERVER` unset, so that the client and the foreground daemon share one configuration and state directory.

Run `ssf-server` in a persistent terminal or under the host's process supervisor:

```sh
ssf-server
```

ssf runs its agents in its own herdr session, `ssf`, and starts that session's
server itself, detached, with a herdr config it writes (see
[drivers.md](drivers.md#host-or-guest)). Attach to it with `herdr session attach ssf`.

With no service unit, `ssf status` and `ssf doctor` report the daemon itself
(`ssf.sock`) and name the unit's absence as detail, and the dashboards draw no
warning while it answers: a factory run this way — a container, another
supervisor, a foreground `ssf-server` — is running, not inactive (#463).

### 4.4 Client only, driving a factory elsewhere

Install the client the same way ([4.1 package](#41-linux-package), [4.2 Homebrew](#42-macos-homebrew), or [the `ssf` binary alone](#43-standalone-binaries)) and reach the remote factory over SSH. SSF opens no TCP listener; SSH starts the server-side endpoint on the far machine. The SSH account must be the one that operates that factory, and needs `ssf-server` on its noninteractive SSH PATH.

```sh
ssf --server user@host status
```

For a stable name instead of a destination (`ssf server add NAME --ssh user@host`), see the server catalog in [configuration.md#server-catalog](configuration.md#server-catalog). A client-only machine needs no daemon and no `ssf setup`.

## 5. `ssf setup` and the service

*Runs on the machine the daemon runs on, as the user your route names; not on the client-only or standalone-binaries paths.*

On the package and Homebrew paths only. On Linux, first check linger with `loginctl show-user "$USER" -p Linger --value`; if it is not `yes`, have the person run `sudo loginctl enable-linger USER` (their user name filled in), because `ssf setup` would otherwise try `sudo` itself. Then:

```sh
ssf setup
```

It validates any existing configuration, creates the conventional VM server `ssf-server` when nothing is configured yet (selected automatically while it is the only one), enables that server's background service, and on Linux needs login linger so the factory survives logout and starts at boot. Expect it to end with `ssf setup complete; selected service enabled` and a `next:` line naming `ssf vm build` (VM) or `ssf auth login --web` (host).

For host mode, create the local target first so setup prepares that shape:

```sh
ssf server add local --local
ssf setup
```

Do not create both a VM server and a local server just to compare: with more than one configured, unqualified commands require `--server NAME`.

`ssf setup` is idempotent and safe to re-run at any time. It creates no bot, watches no repository, and never removes configuration. If it stops on linger, give the person the `sudo loginctl enable-linger $USER` line it prints (with the user name filled in), and run `ssf setup` again once they confirm.

```sh
ssf doctor
```

At this point failures for the bot, driver, harness and repositories are expected. Only unreadable configuration or a missing `gh` needs fixing now.

## 6. Where the agents run

### 6.1 The VM

```sh
ssf vm build
ssf vm status
```

`ssf vm build` picks the backend (Firecracker on Linux, lima on macOS and on Linux with qemu, Incus when `vm.backend` says so), sizes the guest by the rule in [install.md](install.md#is-a-vm-reasonable-here) (`ssf skill setup`), writes those sizes to the selected target, and provisions git, gh, herdr and the harness CLIs. Read the printed sizes and the harness installation results: a harness that failed to install cannot be signed in. `--vcpus`, `--mem-mib` and `--data-gib` override the rule; an already-set size is kept.

The build prints one line for the host it measured and one per size it chose, in the shape

```
this machine: 8 CPUs, 32768 MiB RAM, 155 GiB free on /home (measured at /home/you/.local/share/ssf/vm, [vm] dir)
```

followed by the provisioning log and the harnesses installed. Expect `ssf vm status` afterwards to report a running VM and working SSH. From here, `ssf doctor`, `ssf auth`, `ssf repo` and `ssf status` operate inside the guest even when typed on the host. `ssf vm logs` shows the guest daemon's journal.

Re-running `ssf vm build` after a failure is safe; it keeps an existing image unless `--force` is given, and it keeps the data disk either way. Sessions run as the guest's `ssf` user with passwordless sudo; the VM is the isolation boundary. More in [vm.md](vm.md) (`ssf skill vm`).

### 6.2 Host mode

Agents run as this Unix user and can reach this user's files and credentials, and the default launch commands bypass the harness's permission prompts because the terminals are unattended. Say this plainly to the person before choosing it.

herdr provides the workspaces and terminals. ssf runs its agents in a herdr session of its own named `ssf`, never the person's own herdr session, with a herdr config ssf writes that turns herdr's own agent restore off. The daemon starts that session's server itself when it is not running, so there is nothing to start by hand. To watch and type in the agents' panes:

```sh
herdr session attach ssf
```

`ssf status` and `ssf doctor` name the session. An install from before #602 whose agents ran in the person's default herdr session: see [drivers.md](drivers.md#moving-an-existing-host-install-to-the-ssf-session). ssf clones under `herdr.projects_dir` (`~/ssf/projects`) and makes a worktree per item beside the clone.

```sh
ssf doctor
```

Expect doctor to say the driver (herdr, the only one) is reachable and ready. Details in [drivers.md](drivers.md).

## 7. Oversee the agents from the person's machine

Do not skip this step. The machine the agents run on must be reachable over SSH and visible in the person's own herdr, beside Local in the sidebar. Your route names HOST, the SSH host that reaches the agents, and whether they live in ssf's herdr session `ssf` (host mode). When the agents run as the person's own user on their own machine there is no HOST: that machine is already Local, and only the paragraph on root below applies.

When HOST is the entry `ssf vm ssh-config` writes for a VM on this machine (`ssf-default`, `ssf-NAME` for a VM named otherwise), add it to `~/.ssh/config` first, creating the file if it is missing and appending otherwise (skip if `grep -q '^Host ssf-default$' ~/.ssh/config` already finds it):

```sh
mkdir -p ~/.ssh && chmod 700 ~/.ssh
ssf vm ssh-config >> ~/.ssh/config
chmod 600 ~/.ssh/config
ssh ssf-default true
```

Then save the machine in the person's herdr. Run it yourself with input closed first: closed input can never answer yes to replacing the remote server.

```sh
herdr machine add HOST --label factory </dev/null
# host mode: the agents live in the herdr session `ssf`:
# herdr machine add HOST --label factory --remote-session ssf </dev/null
herdr machine list
```

Expect `Remote server is ready.` If it stops at a prompt instead (a version mismatch, or an offer to install or update the remote herdr), hand the same command, without `</dev/null`, to the person to run interactively, answering No to replacing a running server unless they ask: its panes are live sessions. `herdr --remote HOST` then attaches (`herdr --remote HOST --session ssf` in host mode). The sessions then appear beside Local in the person's sidebar. Details and the limits of what a local herdr command reaches are in [liaison.md](liaison.md#inspect-the-factorys-herdr-server).

Root: the agents should be able to `sudo` without a password on an account that exists only for them, so they can install what their work needs without stopping. The guest's `ssf` user already has it. A factory account in host mode gets it from the person, with the commands your route gives. Never grant it to an account that also serves someone else: not to the account on a machine that hosts a guest (the guest needs nothing there), and not to the person's own user in host mode on their machine, where the agents already run as the person.

### Install the working-with-ssf skill

Run this yourself; it needs no `sudo` and no questions. Tell the person in one line what it is for: any agent on that machine can then set up, operate and troubleshoot ssf.

On the machine you are running on:

```sh
npx -y skills add mikekelly/simple-software-factory -g -y
```

A guest needs nothing more: `ssf vm build` installs the skill for the guest's `ssf` user (a failure is reported in the build log, not fatal). When the sessions run in host mode on HOST, install it there too, as the Unix user that runs them:

```sh
ssh HOST 'npx -y skills add mikekelly/simple-software-factory -g -y'
```

If `npx` is missing on that machine, install Node.js there first (`mise use -g node`, or the system package). A `✗ PromptScript does not support global skill installation` line in the output is harmless. Expect `working-with-ssf` in `npx skills ls -g` (or `~/.agents/skills/working-with-ssf`) on each machine.


## 8. The bot account

The factory acts on GitHub as an account of its own. Every agent post carries a byline naming the session and what it runs, and a post from the bot *without* a byline is read as typed by a person, so sharing the person's own account confuses who said what. The bot is a default, not a security boundary: agents run as a Unix user and the account only bounds what `gh` does by default.

**Ask now whether a bot account exists; if not, ask whether one may be created, then let them create it.** In a private browser window they sign up at `https://github.com/signup` with a separate address (plus-addressing works), verify it, and turn on two-factor authentication. For an organisation, the same thing owned by the organisation as a machine user. Nothing else is needed: no repositories, no keys.

Sign it in where the factory runs. On a VM target the command runs in the guest, so the VM ([6.1](#61-the-vm)) must be up:

```sh
ssf auth login --web
ssf auth status
```

`--web` runs gh's device flow: the terminal prints a one-time code and `https://github.com/login/device`, which the person opens in the window where the bot is signed in. Over SSH, or where no browser should open, prefix `BROWSER=true`. Start it only when the person is ready to approve, finish it before [10. Sign in the harness](#10-sign-in-the-harness), and tell them the code lasts about 15 minutes. `--user <bot>` checks the approved account is the intended one. In VM mode the credential is written inside the guest. Where OAuth apps are forbidden, use a classic personal access token instead: `printf '%s' "$TOKEN" | ssf auth login --token`. A fine-grained token reads as missing every scope.

Scopes: `repo`, `workflow` (pushes that touch `.github/workflows/`), `project` (boards), `admin:public_key` and `admin:ssh_signing_key` (key enrollment); a pasted classic token needs the same. `--no-keys` skips the key and needs only `repo` and `workflow`, at the cost of unsigned commits. `ssf doctor` and `ssf auth status` name any scope the token lacks; `ssf auth login` again adds it.

Login records `github.login` and `github.email`, and enrolls a dedicated ed25519 key on the bot account as both an SSH key and a signing key. `ssf auth logout` revokes those keys and forgets the bot.

**Write access.** Ask now which repository the factory should watch, who owns it, and whether that owner can grant the bot Write. An invitation is not access. The repository owner invites the bot with Write, from their own account:

```sh
gh api repos/OWNER/NAME/collaborators/BOT -X PUT -f permission=push
```

Then the bot accepts. After `ssf auth login` the bot's token is in ssf's own token file, not in `gh`, so where ssf runs (inside the guest in VM mode) pass it explicitly:

```sh
export GH_TOKEN=$(cat ~/.config/ssf/token)
gh api user/repository_invitations --jq '.[] | {id, repository: .repository.full_name}'
gh api user/repository_invitations/ID -X PATCH
gh api repos/OWNER/NAME --jq '{repository: .full_name, push: .permissions.push}'
```

Or accept from the bot's own notifications in a browser signed in as the bot.

The last command must name the repository with `push: true`. A pending invitation makes a private repository return 404, which looks like a missing repository.

The daemon can accept invitations itself, from owners the person names:

```sh
ssf config set github.auto_accept_invitations_from '["OWNER"]'
```

The login match is case-insensitive. This accepts the GitHub invitation only; it does not add the repository to the factory.

**Board access.** For a Projects (v2) board, its owner grants the bot Write under the board's **Settings -> Manage access**. Repository Write alone does not authorise card moves, and the token needs `project` scope.

`ssf auth login` is safe to re-run: a failed or half-finished login can simply be run again.

## 9. Who may drive the factory

Whatever reaches the bot on GitHub is relayed into a running agent's terminal, so this is a real trust boundary. By default the agents act only on assignments, mentions, review requests, labels and comments from collaborators with push access, which GitHub calls Write or higher. The daemon fetches that list each pass and `ssf doctor` prints it per repository. The person's own account must be on it to assign issues to the bot. Nothing needs setting for the default.

To narrow or widen it:

```sh
ssf config set daemon.allowed_users '["alice", "bob"]'
ssf repo set OWNER/NAME --allowed-users alice,bob
```

`"*"` means anyone on GitHub. It is refused unless someone types `yes` at the terminal or passes `--accept-anyone-risk`. **An agent must never pass that flag on the person's behalf.** If the collaborator list cannot be fetched, nothing is acted on for that repository until a list is configured, and `ssf doctor` says so.

## 10. Sign in the harness

Ask now which harness the person already pays for, and whether any metered API spend is acceptable. Each harness is signed in once, where the agents run, with the harness's own sign-in. The credential lands in the home directory there (for example `~/.claude/.credentials.json` in the guest), and every later session uses it. ssf does not sign harnesses in.

Do this step with the person, one harness at a time, through herdr. In VM mode run each `herdr` command below inside the guest with `ssf vm ssh herdr ...`; in host mode (over SSH when the daemon runs on HOST), as the Unix user that runs the sessions, against ssf's own herdr session: prefix each `herdr` command with `HERDR_SESSION=ssf` (only that session's server is running, so plain `herdr` answers `server_not_running`).

1. **Open the harness** in a pane in the projects root (`/var/lib/ssf/projects` in the VM; `herdr.projects_dir` elsewhere). Folder trust given there does not cover the repositories under it; first-run screens that are global, such as sign-in and preference screens, are cleared once here:

   ```sh
   herdr workspace create --cwd /var/lib/ssf/projects --label "claude login" --no-focus
   herdr pane run PANE claude     # PANE: result.root_pane.pane_id in the JSON printed above
   ```

2. **Clear the first-run screens.** Read the pane with `herdr pane read PANE` and answer with `herdr pane send-keys PANE KEY...`. Take the default on preference screens, or ask the person in one line if the choice matters; for Claude Code's "Try the new fullscreen renderer?" pick **Not now**; finish Oh My Pi's setup wizard here too, since it is kept per user and covers every later repository. Grant folder trust explicitly: Claude Code's trust screen defaults to "No, exit", so select the trust option rather than pressing enter. Codex's first launch asks to review and trust hooks: the one listed is herdr's `SessionStart` agent-state hook, which provisioning installed and sessions rely on, so trust it. Read again after every key: screens change with every harness release.

3. **Start the harness's own sign-in** (for example `/login`, typed with `herdr pane run PANE /login`) only when the person is ready for it; never run two sign-ins at once, since each code expires while the person is busy with the other.
   - **The link.** `herdr pane read` returns screen rows, so a long URL is split across lines. Rejoin it before handing it over, for example `herdr pane read PANE | tr -d '\n' | grep -o 'https://[^ ]*'`, and check it against the screen. The person's browser may be on another computer, so do not open it on the factory machine: give the URL as text, alone in a fenced code block so it copies whole even where the terminal wraps it.
   - **The code.** Many sign-ins, Claude Code's included, show a code in the browser after the person approves (Claude's looks like `<code>#<state>`) while the harness waits at a prompt such as `Paste code here if prompted >`. Approving in the browser is not enough: ask for the code, type it with `herdr pane send-text PANE '<code>'` and `herdr pane send-keys PANE enter`, and tell the person it passes through this chat and expires within minutes. Check the result (`claude auth status` shows `loggedIn: true`, or `ssf doctor`) before going on.
   - A sign-in that needs the browser to reach a `localhost` callback on the factory machine (OMP's loopback OAuth) cannot finish from the person's browser: pick a method that takes a pasted code or redirect URL, or an API key.

4. **API-key harnesses.** Ask the person for the key only after they have agreed to metered spend, and say it passes through the chat.
   - OpenCode: `opencode auth login` in the pane, pick the provider and paste the key; it is saved in `~/.local/share/opencode/auth.json`.
   - Grok: `grok login --device-auth` signs in an xAI account; for a key, enter it through Grok's own interface when it asks.
   - Crush: pick the provider and paste the key in Crush's own first-run screen; `crush login copilot` signs in with GitHub Copilot instead.

   Enter keys through the harness's interface rather than writing its files by hand, so the harness writes the format it reads.

5. **Relaunch, confirm and quit.** Some first-run screens appear only on a later launch (Claude Code's fullscreen-renderer question came on the second), so quit the harness and run it again in the same pane, clearing any screen as in step 2, until a launch reaches the prompt with nothing to answer. Confirm the sign-in with the harness's own status (`claude auth status`, `codex login status`), or in VM mode `ssf vm status` on the host, which lists signed-in harnesses on its `logins:` line. Then quit the harness (`/exit`, `/quit` or `ctrl+c`, whatever it takes) and close the workspace. Codex can leave its app-server daemon running after it quits; ssf's sessions do not use it, so stop it with `codex app-server daemon stop` in the same place.

6. **If a screen makes no sense**, stop sending keys and tell the person where to look: the herdr machine (HOST, saved in [7](#7-oversee-the-agents-from-the-persons-machine), or Local in host mode on their machine), the workspace label (`claude login`) and the pane. They can finish it by hand there.

Without an agent, the person does the same by hand: `ssf vm ssh` (or a shell on the host), run the harness, and use its own sign-in. Per-harness quirks are in [platform-specifics.md](platform-specifics.md#harness-notes). Signing in again is also the fix when a login later expires under a running session: ssf holds that session and resumes it once the harness is signed in.

## 11. Watch the first repository

Ask now for the person's explicit choice of model and effort, and confirm the harness. Examples and recommendations are not consent. List what the installation actually offers, in the place the sessions will run:

```sh
ssf agents
ssf models HARNESS
```

On a VM target both run inside the guest, where the sessions run. `ssf models` names what answered: the harness's own catalogue, its listing command, or ssf's built-in table — and for Claude Code it starts the CLI once to refresh a catalogue that is missing or expired, so the listing names models released since that file was written. `ssf agents --json` adds the supported effort levels and launch commands. Then:

```sh
ssf repo add OWNER/NAME --harness HARNESS --model MODEL --effort EFFORT
ssf repo list --json
```

`--model` and `--effort` are required in the resulting configuration wherever the harness supports them; omit one only where it is unsupported. Use `ssf repo set` with the same options to change a setting later; changes apply to the next started or resumed session.

Before the first issue, the repository also needs `SSF.md` committed at the root of its **default branch**, and any items that predate this factory need reviewing with `ssf candidates` and adopting with `ssf adopt`. The full path, including choosing a first issue and what to expect from it, is [repositories.md](repositories.md) (`ssf skill repo`).

`ssf repo add` rewrites the entry for a repository that is already there, so it is safe to re-run after a mistake; it is also how you correct a harness or model chosen in error.

## 12. Other devices, then verify

Verify first (below), then make the offer in [12.1](#121-offer-remote-access-the-dashboard-and-terminal-access-together).

```sh
ssf doctor
ssf status
```

Also check `herdr machine list` shows HOST ([7](#7-oversee-the-agents-from-the-persons-machine)), that `npx skills ls -g | grep working-with-ssf` finds the skill on this machine and, when separate, where the sessions run (`ssh HOST 'npx skills ls -g' | grep working-with-ssf`), and run any checks your route document adds. Healthy looks like: the token belongs to the bot; the driver is reachable and ready; the harness is installed and signed in where sessions run; each repository shows its GitHub identity, its allowed users, the commit identity, and its SSF agent guidance. `ssf status` names the configured account, then the repositories and their tracked items with no last error.

Two lines are expected to fail before the first issue and need no action: the repository's checkout (cloned when the first session starts) and the `gh`, `git` and `ssf` command links (written when the first agent starts). Anything else, work through [troubleshooting.md](troubleshooting.md) (`ssf skill troubleshoot`).

### 12.1 Offer remote access, the dashboard and terminal access together

The install is not finished until this is asked. Ask one question covering all three, because the answers depend on each other (the dashboard's bind address follows the Tailscale answer):

> "Last, some optional extras. Do you want: (a) to reach the factory from other devices over Tailscale; (b) the web dashboard and its Chrome extension, which shows each agent's state on GitHub issue and pull request pages; (c) to watch and type into an agent's terminal from them? Any, all or none."

Set up what they accept, in this order.

**Remote access (a).** Tailscale goes where the daemon runs. With a guest, run `ssf vm tailscale` where you ran `ssf setup` and pass the login URL it prints to the person; it prints the machine name and address once enrolled. That enrolls the guest, which serves SSH, herdr and the web dashboard: that one step is all remote access needs, and Tailscale never goes on the machine that hosts the guest. In host mode it goes on the machine itself: give the person the install command from [tailscale.com/download](https://tailscale.com/download) and `sudo tailscale up`, and pass along its login URL. Details in [platform-specifics.md](platform-specifics.md#tailscale).

**Dashboard and extension (b).** Where the browser reaches it depends on (a):

| Tailscale | Factory | Browser reaches the dashboard at |
| --- | --- | --- |
| yes | any | the factory's tailnet address (bind to it, below) |
| no | the daemon runs on the person's own machine (host mode, or a guest there) | loopback, `http://127.0.0.1:PORT` |
| no | the daemon runs on another machine (host mode there, or a guest on it) | an SSH tunnel from the person's machine to that machine's loopback port (a guest's dashboard is forwarded there): `ssh -N -L PORT:127.0.0.1:PORT MACHINE`, then `http://127.0.0.1:PORT` while it runs; or no dashboard |

The tunnel must stay open for the dashboard and the extension to work, so say that and let the person choose between it and no dashboard (or Tailscale after all). Never bind a public address instead.

The factory's daemon serves it wherever the daemon runs: the guest when there is one, the machine itself in host mode. Nothing about the dashboard goes on a machine that hosts a guest, and with Tailscale no SSH tunnel is needed: bind it to the guest's Tailscale address. Run the commands below where you ran `ssf setup`; they reach the daemon's config. The setup is the same in every mode, and `ssf config` reaches the right config. Bind it to the factory's Tailscale address when it is on the tailnet (in VM mode, the guest's, after `ssf vm tailscale`), so only tailnet devices can reach it; otherwise keep the loopback default. A loopback dashboard in a VM is also on the host's loopback at the same port (lima forwards it; for Firecracker and Incus the ssf supervisor does, see [dashboard.md](dashboard.md#optional-server-web-dashboard)). Never bind anything else; [dashboard.md](dashboard.md#bind-rules) has the rules.

```sh
ssf config set dashboard.enabled true
ssf config set dashboard.bind TAILSCALE_ADDRESS      # tailnet; skip for loopback. VM: the address `ssf vm tailscale` printed
# VM mode: restart the guest daemon and read its URL
ssf vm ssh -- sudo systemctl restart ssf
ssf vm logs | grep 'Server web dashboard'
# host mode: restart the unit ssf setup made (e.g. ssf@ssf-server.service)
systemctl --user list-units 'ssf*.service'
systemctl --user restart UNIT
journalctl --user -u UNIT | grep 'Server web dashboard'
```

A capability URL a host-side listener handed out before ssf served the dashboard from the guest no longer works: add the guest's URL to the extension once. A leftover `[dashboard]` in the host config is unused; `ssf doctor` notes it.

**Terminal access (c).** Both settings default to off; if accepted, set them before the restart above:

```sh
ssf config set daemon.item_pane_input true      # the factory's config (the guest in VM mode); repo.item_pane_input per repository
ssf config set dashboard.terminal_input true    # this page's own terminal (the factory's config too); the extension needs only the line above
```

Without `item_pane_input`, neither offers **Show agent TUI**, and people speak to an agent by commenting on the item.

On macOS, restart with `launchctl kickstart -k gui/$(id -u)/dev.ssf.server.NAME` and find the URL with `grep 'Server web dashboard' ~/Library/Logs/ssf/NAME.log` ([operate.md](operate.md) names the agent and log).

Hand the capability URL it logs (`http://ADDRESS:8787/<secret>/`) to the person, as a secret: it grants access to the factory. Then, in Chrome on their machine:

1. Open the URL and use its **Download Chrome extension** link. Chrome warns about an insecure download because it is plain HTTP; choose **Keep**. (`ssf chrome-extension` writes the same zip on the command line.)
2. Unzip it.
3. Open `chrome://extensions`, turn on **Developer mode**, choose **Load unpacked** and select the unzipped directory, the one holding `manifest.json`.
4. The extension's options page opens by itself: **Add a factory**, paste the capability URL, **Save and allow**, and accept Chrome's permission prompt.

Opening any issue in a watched repository then shows the overlay. Details in [dashboard.md](dashboard.md) and the [extension README](../chrome-extension/README.md).

## 13. Upgrading, stopping, uninstalling

Upgrade by installing the next release's package the same way it was installed. The Arch, `.deb` and `.rpm` packages restart running services (`ssf.service`, `ssf@NAME.service`) on the new version; on macOS run `launchctl kickstart -k gui/$(id -u)/dev.ssf.server.NAME`. Upgrades never stop or restart a VM or agent sessions: the new service reattaches to the running guest and herdr session, and the guest keeps its own version until `ssf vm upgrade`. A service started by ssf 0.19 or earlier, which still holds its VM, is left running with a message. Standalone binaries are replaced in pairs with the daemon stopped. Configuration, state, keys and VM disks survive an upgrade. See [operate.md](operate.md) (`ssf skill operate`).

`ssf ui service disable` stops the service and keeps it stopped across logins; `ssf ui service enable` brings it back.

`ssf uninstall` reports first and asks once. It refuses while workspaces hold uncommitted or unpushed work. `--force` bypasses that and destroys the work with the data disk, so leave that decision to the person. See [uninstall.md](uninstall.md) (`ssf skill uninstall`).

## 14. Checklist

- [ ] Probes run; the person chose where the factory runs, before anything else was asked.
- [ ] Consent recorded at each step for accounts, spending, and the model choice; every `sudo` command was run by the person.
- [ ] `ssf --version` and `ssf-server` both present, from the same release.
- [ ] `ssf setup` complete and the service enabled (package and Homebrew paths).
- [ ] `ssf vm status` reports a running VM, or herdr is reachable in host mode.
- [ ] HOST, when the agents are not the person's own user on their machine: reachable over SSH, agents have passwordless `sudo` on an account only they use, and it is saved in the person's herdr (`herdr machine add`, shown by `herdr machine list`).
- [ ] The items your route document adds to this list.
- [ ] `working-with-ssf` skill installed on this machine and, when separate, where the sessions run, as the sessions' user.
- [ ] The person was offered remote access, the dashboard with the Chrome extension, and terminal access in one question ([12.1](#121-offer-remote-access-the-dashboard-and-terminal-access-together)); Tailscale enrolled if accepted; if the dashboard was accepted, it binds to loopback or a Tailscale address (a remote guest without Tailscale is reached through an SSH tunnel) and the extension has the factory saved.
- [ ] Bot account created; `ssf auth status` names it.
- [ ] Bot has Write on the repository, verified with `push: true`, and board access if there is a board.
- [ ] Allowed users are deliberate; `*` only with the person's consent.
- [ ] Harness signed in where the sessions run.
- [ ] `SSF.md` on the default branch; repository added with a chosen harness, model and effort.
- [ ] `ssf doctor` clean apart from the pre-first-issue exceptions.
- [ ] A small issue assigned to the bot produced an agent comment on GitHub.

## What is safe to re-run

| Step | After a failure |
|---|---|
| `ssf setup` | re-run freely; it validates, never destroys, and skips what is already done |
| `ssf auth login` | re-run; a half-finished device flow leaves nothing behind. `ssf auth logout` first only if the wrong account was approved |
| `ssf vm build` | re-run; it keeps an existing image and the data disk. `--force` remakes the image, and still keeps the data disk |
| [Sign in the harness](#10-sign-in-the-harness) | repeat; it is also the fix for an expired login |
| `ssf repo add` | re-run; it replaces that repository's settings |
| `ssf doctor`, `ssf status`, `ssf models`, `ssf agents` | re-run at any time; reads, except that `ssf models claude` refreshes Claude Code's own catalogue |

Destructive and not to be re-run casually: `ssf vm destroy`, `ssf uninstall`, and anything with `--force`.
