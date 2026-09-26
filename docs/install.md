# Install a factory

Read this when someone asks you to set up Simple Software Factory for them, from nothing to a factory that watches one repository and has worked its first issue. Offline copy: `ssf skill setup`.

## 0. Who this is for, and the outcome

You are an agent doing this on behalf of a person, on a machine you have not seen before. The person owns every decision that costs money, creates an account, needs root on their machine, or widens who can drive the factory. You own the probing, the reading, the unprivileged commands, and the diagnosis.

At the end:

- `ssf` and `ssf-server` are installed somewhere the factory can run.
- A separate GitHub bot account is signed in, with Write access on one repository.
- A harness (the coding agent program) is signed in where sessions run.
- One repository is watched, with a harness, model and effort the person chose.
- One small issue assigned to the bot has produced an agent comment on GitHub.
- `ssf doctor` passes.
- When the agents run on a VM or a rented server: that machine is reachable over SSH, the agents can become root on it without asking anyone (so they administer their own environment), and it is saved in the person's local herdr, so the person and any agents on their machine can oversee the sessions there.

Work through the sections in order. Every step says what a good result looks like and what is safe to re-run.

**Run it as a guided install, not a checklist.** Before probing, tell the person in a few lines that you will guide them through setup, name the stages (where it runs → install → bot account → harness sign-in → first repository → first issue → oversight), and say you will ask only what is needed, as you go. Before each step, say in one or two sentences what is about to happen and why, and whether it needs anything from them ("Next I'll build the VM image; this takes a few minutes and needs nothing from you"). After it, give a one-line result.

## 1. How to ask, and what needs consent

**Ask one decision at a time, at the step that needs it; never present the full list of questions up front.** Before the first install command, probe (section 2) and ask only where the factory should run. Every later question lives in the section that needs it:

| Question | Asked in |
|---|---|
| Where the factory runs, after you present the options | section 2 |
| Whether renting a host is acceptable, and at what cost | section 2, only if a rented host is proposed |
| Whether a bot GitHub account exists, or may be created | section 6 |
| Which repository to watch, who owns it, and whether they can grant the bot Write | section 6, Write access |
| Which harness they already pay for, and any metered API spend | section 8 |
| The model and effort | section 9 |
| Who may drive the factory, if not the default | section 7 |

**Hand privileged commands to the person.** Do not run `sudo` yourself: most harnesses have no terminal for a password, and root on their machine is theirs to use. Prepare everything the command needs first (download the package, print its exact path), give the person the exact command, ask them to run it in their own terminal (in Claude Code, typing `! <command>` runs it in the session), and continue once they confirm and you have checked the result. Run it yourself only when you already have non-interactive root there (`sudo -n true` succeeds), such as inside the factory's own VM.

Consent you must obtain explicitly, in words, at the step it applies to:

| Needs consent | Why |
|---|---|
| Creating a GitHub account | it is their identity and their email |
| Renting a host, or any metered API spending | it costs them money |
| The harness, model and effort for the repository | it costs them money and sets quality |
| `--allowed-users '*'` / `--accept-anyone-risk` | it lets anyone on GitHub drive their factory |
| `ssf uninstall --force`, `ssf purge --force` | these can destroy unpushed work |

Decide these yourself, no need to ask: which probe commands to run, which install path fits the measurements, VM sizes (let `ssf vm build` choose), when to re-run a failed idempotent step, how to read `ssf doctor`.

## 2. Choose where the factory runs

Run the probes, then propose. The person picks; this is the only question before installing.

```sh
uname -s -m
nproc 2>/dev/null || sysctl -n hw.ncpu
free -g 2>/dev/null || sysctl -n hw.memsize
df -h "$HOME"
test -r /dev/kvm && test -w /dev/kvm && echo kvm-ok
systemctl --user is-system-running
command -v gh herdr tmux
```

`free`, `/dev/kvm` and `systemctl --user` are Linux only; on macOS `sysctl -n hw.memsize` reports bytes and the VM runs through lima instead of KVM.

| What the probes say | Path |
|---|---|
| Linux, `/dev/kvm` readable and writable, `systemctl --user` answers, resources allow a VM | **Local VM** (the default). Section 3.1, then 4 and 5. |
| macOS, resources allow a VM | **Homebrew + lima VM**. Section 3.2, then 4 and 5. |
| Linux without KVM or a systemd user session, or a Mac too small for a VM, with enough CPU/RAM for the sessions, and the person accepts that agents see this user's files | **Host mode on this machine**. Section 3.1, 3.2 or 3.3, then 5 (host mode). |
| Too few resources here, or the person does not want agents on this machine | **A rented host** runs the factory; this machine only drives it. Section 3.3 on the host, 3.4 here. |
| A factory already runs somewhere else | **Client only**. Section 3.4. |

### Is a VM reasonable here

`ssf vm build` sizes the guest from the host and prints what it chose:

- vCPUs: host CPUs minus one, at least 2.
- Memory: half the RAM, at least 4096 MiB.
- Data disk: half the free space where the disk lands, at least 20 GiB, sparse so it reserves nothing up front.

Rule of thumb for judging "reasonable": each parallel agent session wants about one vCPU and 2 GiB of RAM, and the person's own desktop needs to keep about 4 GB. A 4-core, 8 GB machine gives a guest of 3 vCPUs and 4 GiB, which is one or two sessions at a time and leaves the machine usable. An 8-core, 8 GB machine gets the same 4 GiB but 7 vCPUs by the rule, which is more CPU than that memory can use: pass `--vcpus 2` or `--vcpus 3` to `ssf vm build` on a small machine rather than accept the rule. With 8 GB or less in total, present all three options and recommend host mode or a rented host over a VM; below 8 GB, the VM is not reasonable. Allow roughly 30 GB of disk headroom for images and data, plus room for the repositories and their builds.

Say to the person, in one line each, what the options cost them: the VM keeps agents away from their files but takes half the machine; host mode takes only what the sessions use but the agents run as their user with permission prompts bypassed; a rented host costs money and puts the factory on a machine they administer over SSH.

### The rented-host option

If this machine cannot host the factory, the factory can live on a server the person rents and this machine talks to it over SSH. Suitable hosts include the dedicated server that comes with a Grok Bot account, or a VPS from Hetzner, Linode, OVH or a similar provider. Requirements: Linux, a non-root user, outbound HTTPS, and enough RAM for the sessions by the rule above. KVM is not required, because that host runs in host mode.

Renting costs money: get explicit consent before proposing a specific product, and do not create the account for them.

## 3. Install

### 3.1 Linux package

Download the matching asset from [GitHub Releases](https://github.com/mikekelly/simple-software-factory/releases) yourself (`gh release download --repo mikekelly/simple-software-factory --pattern PATTERN`), print its absolute path, then give the person the install command for their family with that path filled in, and wait for them to confirm it ran:

| Family | Command |
|---|---|
| Arch | `sudo pacman -U ssf-*.pkg.tar.zst` |
| Debian / Ubuntu | `sudo apt install ./ssf_*_amd64.deb` |
| Fedora / RHEL | `sudo dnf install ./ssf-*.x86_64.rpm` |

One package holds both the `ssf` client and the `ssf-server` daemon. Packages are x86_64.

Prerequisites the package does not always bring: GitHub CLI 2.40 or newer, Git, jq, an OpenSSH client (`ssh-keygen` enrolls the bot's key), tmux (scratch sessions run in it; the packages depend on it), and, for host mode, herdr, which only Omarchy's repositories carry as a package. The VM installs its own herdr in the guest. Where to get herdr and the other distro-specific commands and quirks are in [platform-specifics.md](platform-specifics.md).

```sh
ssf --version
command -v ssf-server
```

### 3.2 macOS, Homebrew

```sh
brew install mikekelly/tap/ssf
ssf --version
```

The formula brings `gh` and `lima`. `ssf setup` (section 4) enables a launchd agent per target, so `brew services` is not used. The VM path needs nothing more; for host mode on the Mac, `brew install herdr tmux` as well. Details in [platform-specifics.md](platform-specifics.md#macos).

### 3.3 Standalone binaries on a rented host

Releases publish static musl Linux binaries for `x86_64` and `aarch64`. Take the client and the server from the **same release** and install both under unversioned names in the same directory. Do not pin a version here: pick the current release.

```sh
arch=$(uname -m)                 # x86_64 or aarch64
dir=$(mktemp -d)
gh release download --repo mikekelly/simple-software-factory \
  --pattern "ssf-[0-9]*-linux-$arch" --pattern "ssf-server-[0-9]*-linux-$arch" --dir "$dir"
install -Dm755 "$dir"/ssf-[0-9]*-linux-$arch   "$HOME/.local/bin/ssf"
install -Dm755 "$dir"/ssf-server-*-linux-$arch "$HOME/.local/bin/ssf-server"
export PATH="$HOME/.local/bin:$PATH"
ssf --version && ssf-server --version
```

Persist that `PATH` line for future shells. Supply the prerequisites yourself with the host's package manager: CA certificates, curl, Git, jq, GitHub CLI 2.40 or newer, an OpenSSH client, tmux, herdr, and the harness. The bare binaries carry no service units, no VM scripts and no configuration examples, so skip `ssf setup` on this path and leave the server catalog empty, so that the client and the foreground daemon share one configuration and state directory.

Run the two processes under the same Unix user, HOME and PATH, each in its own persistent terminal or under the host's process supervisor:

```sh
herdr server        # terminal one
ssf-server          # terminal two
```

With no service unit, `ssf status` and `ssf doctor` report the daemon itself
(`ssf.sock`) and name the unit's absence as detail, and the dashboards draw no
warning while it answers: a factory run this way — a container, another
supervisor, a foreground `ssf-server` — is running, not inactive (#463).

### 3.4 Client only, driving a factory elsewhere

Install the client the same way (package, Homebrew, or the `ssf` binary alone) and reach the remote factory over SSH. SSF opens no TCP listener; SSH starts the server-side endpoint on the far machine, which needs `ssf-server` on its noninteractive SSH PATH.

```sh
ssf --server user@host status
```

For a stable name instead of a destination, see the server catalog in [configuration.md#server-catalog](configuration.md#server-catalog). A client-only machine needs no daemon and no `ssf setup`.

## 4. `ssf setup` and the service

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

## 5. Where the agents run

### The VM

```sh
ssf vm build
ssf vm status
```

`ssf vm build` picks the backend (Firecracker on Linux, lima on macOS and on Linux with qemu), sizes the guest by the rule in section 2, writes those sizes to the selected target, and provisions git, gh, herdr and the harness CLIs. Read the printed sizes and the harness installation results: a harness that failed to install cannot be signed in. `--vcpus`, `--mem-mib` and `--data-gib` override the rule; an already-set size is kept.

The build prints one line for the host it measured and one per size it chose, in the shape

```
this machine: 8 CPUs, 32768 MiB RAM, 155 GiB free on /home (measured at /home/you/.local/share/ssf/vm, [vm] dir)
```

followed by the provisioning log and the harnesses installed. Expect `ssf vm status` afterwards to report a running VM and working SSH. From here, `ssf doctor`, `ssf auth`, `ssf repo` and `ssf status` operate inside the guest even when typed on the host. `ssf vm logs` shows the guest daemon's journal.

Re-running `ssf vm build` after a failure is safe; it keeps an existing image unless `--force` is given, and it keeps the data disk either way. Sessions run as the guest's `ssf` user with passwordless sudo; the VM is the isolation boundary. More in [vm.md](vm.md) (`ssf skill vm`).

### Host mode

Agents run as this Unix user and can reach this user's files and credentials, and the default launch commands bypass the harness's permission prompts because the terminals are unattended. Say this plainly to the person before choosing it.

herdr provides the workspaces and terminals. Start `herdr server` for headless operation, or leave an interactive `herdr` running. ssf clones under `herdr.projects_dir` (`~/ssf/projects`) and makes a worktree per item beside the clone.

```sh
ssf doctor
```

Expect doctor to say the driver (herdr, the only one) is reachable and ready. Details in [drivers.md](drivers.md).

### Oversee the agents from the person's machine

Do not skip this step. The machine the agents run on must be reachable over SSH and visible in the person's own herdr, beside Local in the sidebar. The SSH host name is `ssf-default` for the local VM (the entry `ssf vm ssh-config` writes, `ssf-NAME` for a VM named otherwise), or `user@host` for a rented host.

For a local VM, add the SSH entry to `~/.ssh/config`, creating the file if it is missing and appending otherwise (skip if `grep -q '^Host ssf-default$' ~/.ssh/config` already finds it):

```sh
mkdir -p ~/.ssh && chmod 700 ~/.ssh
ssf vm ssh-config >> ~/.ssh/config
chmod 600 ~/.ssh/config
ssh ssf-default true
```

Then save the machine in the person's herdr. Run it yourself with input closed first: closed input can never answer yes to replacing the remote server.

```sh
herdr machine add ssf-default --label factory </dev/null   # or user@host
herdr machine list
```

Expect `Remote server is ready.` If it stops at a prompt instead (a version mismatch, or an offer to install or update the remote herdr), hand the same command, without `</dev/null`, to the person to run interactively, answering No to replacing a running server unless they ask: its panes are live sessions. `herdr --remote ssf-default` then attaches. Details and the limits of what a local herdr command reaches are in [liaison.md](liaison.md#inspect-the-factorys-herdr-server).

Root: agents in a VM or on a rented server should be able to `sudo` without a password, so they can install what their work needs without stopping. The guest's `ssf` user already has it. On a rented host the person grants it to the factory account; give them the exact commands ([Rented hosts](platform-specifics.md#rented-hosts)). Never grant it on the person's own machine in host mode: there, the agents are already running as the person.

## 6. The bot account

The factory acts on GitHub as an account of its own. Every agent post carries a byline naming the session and what it runs, and a post from the bot *without* a byline is read as typed by a person, so sharing the person's own account confuses who said what. The bot is a default, not a security boundary: agents run as a Unix user and the account only bounds what `gh` does by default.

**Ask now whether a bot account exists; if not, ask whether one may be created, then let them create it.** In a private browser window they sign up at `https://github.com/signup` with a separate address (plus-addressing works), verify it, and turn on two-factor authentication. For an organisation, the same thing owned by the organisation as a machine user. Nothing else is needed: no repositories, no keys.

Sign it in where the factory runs. On a VM target the command runs in the guest, so the VM from section 5 must be up:

```sh
ssf auth login --web
ssf auth status
```

`--web` runs gh's device flow: the terminal prints a one-time code and `https://github.com/login/device`, which the person opens in the window where the bot is signed in. Over SSH, or where no browser should open, prefix `BROWSER=true`. Start it only when the person is ready to approve, finish it before section 8's harness sign-in, and tell them the code lasts about 15 minutes. `--user <bot>` checks the approved account is the intended one. In VM mode the credential is written inside the guest. Where OAuth apps are forbidden, use a classic personal access token instead: `printf '%s' "$TOKEN" | ssf auth login --token`. A fine-grained token reads as missing every scope.

Scopes: `repo`, `project` (boards), `admin:public_key` and `admin:ssh_signing_key` (key enrollment). `--no-keys` skips the key and needs only `repo`, at the cost of unsigned commits.

Login records `github.login` and `github.email`, and enrolls a dedicated ed25519 key on the bot account as both an SSH key and a signing key. `ssf auth logout` revokes those keys and forgets the bot.

**Write access.** Ask now which repository the factory should watch, who owns it, and whether that owner can grant the bot Write. An invitation is not access. The repository owner invites the bot with Write, from their own account:

```sh
gh api repos/OWNER/NAME/collaborators/BOT -X PUT -f permission=push
```

Then the bot accepts, in a bot-authenticated `gh` session or from its own notifications:

```sh
gh api user/repository_invitations --jq '.[] | {id, repository: .repository.full_name}'
gh api user/repository_invitations/ID -X PATCH
gh api repos/OWNER/NAME --jq '{repository: .full_name, push: .permissions.push}'
```

The last command must name the repository with `push: true`. A pending invitation makes a private repository return 404, which looks like a missing repository.

The daemon can accept invitations itself, from owners the person names:

```sh
ssf config set github.auto_accept_invitations_from '["OWNER"]'
```

The login match is case-insensitive. This accepts the GitHub invitation only; it does not add the repository to the factory.

**Board access.** For a Projects (v2) board, its owner grants the bot Write under the board's **Settings -> Manage access**. Repository Write alone does not authorise card moves, and the token needs `project` scope.

`ssf auth login` is safe to re-run: a failed or half-finished login can simply be run again.

## 7. Who may drive the factory

Whatever reaches the bot on GitHub is relayed into a running agent's terminal, so this is a real trust boundary. By default the agents act only on assignments, mentions, review requests, labels and comments from collaborators with push access, which GitHub calls Write or higher. The daemon fetches that list each pass and `ssf doctor` prints it per repository. The person's own account must be on it to assign issues to the bot. Nothing needs setting for the default.

To narrow or widen it:

```sh
ssf config set daemon.allowed_users '["alice", "bob"]'
ssf repo set OWNER/NAME --allowed-users alice,bob
```

`"*"` means anyone on GitHub. It is refused unless someone types `yes` at the terminal or passes `--accept-anyone-risk`. **An agent must never pass that flag on the person's behalf.** If the collaborator list cannot be fetched, nothing is acted on for that repository until a list is configured, and `ssf doctor` says so.

## 8. Sign in the harness

Ask now which harness the person already pays for, and whether any metered API spend is acceptable. Each harness is signed in once, where the agents run, with the harness's own sign-in. The credential lands in the home directory there (for example `~/.claude/.credentials.json` in the guest), and every later session uses it. ssf does not sign harnesses in.

Do this step with the person, one harness at a time, through herdr. In VM mode run each `herdr` command below inside the guest with `ssf vm ssh herdr ...`; on a rented host, over SSH on that host; in host mode, on the host's own herdr as the Unix user that runs the sessions.

1. **Open the harness** in a pane in the projects root (`/var/lib/ssf/projects` in the VM; `herdr.projects_dir` elsewhere). Folder trust given there does not cover the repositories under it; first-run screens that are global, such as sign-in and preference screens, are cleared once here:

   ```sh
   herdr workspace create --cwd /var/lib/ssf/projects --label "claude login" --no-focus
   herdr pane run PANE claude     # PANE: result.root_pane.pane_id in the JSON printed above
   ```

2. **Clear the first-run screens.** Read the pane with `herdr pane read PANE` and answer with `herdr pane send-keys PANE KEY...`. Take the default on preference screens, or ask the person in one line if the choice matters; for Claude Code's "Try the new fullscreen renderer?" pick **Not now**; finish Oh My Pi's setup wizard here too, since it is kept per user and covers every later repository. Accept folder trust. Read again after every key: screens change with every harness release.

3. **Start the harness's own sign-in** (for example `/login`, typed with `herdr pane run PANE /login`) only when the person is ready for it; never run two sign-ins at once, since each code expires while the person is busy with the other.
   - **The link.** `herdr pane read` returns screen rows, so a long URL is split across lines. Rejoin it before handing it over, for example `herdr pane read PANE | tr -d '\n' | grep -o 'https://[^ ]*'`, and check it against the screen. The person's browser may be on another computer, so do not open it on the factory machine: give the URL as text, alone in a fenced code block so it copies whole even where the terminal wraps it.
   - **The code.** Many sign-ins, Claude Code's included, show a code in the browser after the person approves (Claude's looks like `<code>#<state>`) while the harness waits at a prompt such as `Paste code here if prompted >`. Approving in the browser is not enough: ask for the code, type it with `herdr pane send-text PANE '<code>'` and `herdr pane send-keys PANE enter`, and tell the person it passes through this chat and expires within minutes. Check the result (`claude auth status` shows `loggedIn: true`, or `ssf doctor`) before going on.
   - A sign-in that needs the browser to reach a `localhost` callback on the factory machine (OMP's loopback OAuth) cannot finish from the person's browser: pick a method that takes a pasted code or redirect URL, or an API key.

4. **API-key harnesses.** Ask the person for the key only after they have agreed to metered spend, and say it passes through the chat.
   - OpenCode: `opencode auth login` in the pane, pick the provider and paste the key; it is saved in `~/.local/share/opencode/auth.json`.
   - Grok: `grok login --device-auth` signs in an xAI account; for a key, enter it through Grok's own interface when it asks.
   - Crush: pick the provider and paste the key in Crush's own first-run screen; `crush login copilot` signs in with GitHub Copilot instead.

   Enter keys through the harness's interface rather than writing its files by hand, so the harness writes the format it reads.

5. **Relaunch, confirm and quit.** Some first-run screens appear only on a later launch (Claude Code's fullscreen-renderer question came on the second), so quit the harness and run it again in the same pane, clearing any screen as in step 2, until a launch reaches the prompt with nothing to answer. `ssf doctor` shows the harness signed in where the sessions run (in VM mode, `ssf vm status` also lists it on its `logins:` line). Then quit the harness (`/exit`, `/quit` or `ctrl+c`, whatever it takes) and close the workspace.

6. **If a screen makes no sense**, stop sending keys and tell the person where to look: the herdr machine (`ssf-default` for the local VM, saved in section 5, or Local in host mode), the workspace label (`claude login`) and the pane. They can finish it by hand there.

Without an agent, the person does the same by hand: `ssf vm ssh` (or a shell on the host), run the harness, and use its own sign-in. Per-harness quirks are in [platform-specifics.md](platform-specifics.md#harness-notes). Signing in again is also the fix when a login later expires under a running session: ssf holds that session and resumes it once the harness is signed in.

## 9. Watch the first repository

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

## 10. Other devices, then verify

Ask one yes/no question: "Do you want to reach the factory or the dashboard from other devices?" If yes, on a VM target run `ssf vm tailscale` and pass the login URL it prints to the person; it prints the machine name and address once enrolled. In host mode or on a rented host, Tailscale goes on that machine itself: give the person the install command from [tailscale.com/download](https://tailscale.com/download) and `sudo tailscale up`, and pass along its login URL. Details in [platform-specifics.md](platform-specifics.md#tailscale). If no, move on.

```sh
ssf doctor
ssf status
```

Also check `herdr machine list` shows the factory machine (section 5). Healthy looks like: the token belongs to the bot; the driver is reachable and ready; the harness is installed and signed in where sessions run; each repository shows its GitHub identity, its allowed users, the commit identity, and its SSF agent guidance. `ssf status` names the configured account, then the repositories and their tracked items with no last error.

Two lines are expected to fail before the first issue and need no action: the repository's checkout (cloned when the first session starts) and the `gh`, `git` and `ssf` command links (written when the first agent starts). Anything else, work through [troubleshooting.md](troubleshooting.md) (`ssf skill troubleshoot`).

## 11. Upgrading, stopping, uninstalling

Upgrade by installing the next release's package the same way it was installed; the package restarts the active service, which in VM mode takes the guest down and up on the new binary and resumes the interrupted sessions. Standalone binaries are replaced in pairs with the daemon stopped. Configuration, state, keys and VM disks survive an upgrade. See [operate.md](operate.md) (`ssf skill operate`).

`ssf ui service disable` stops the service and keeps it stopped across logins; `ssf ui service enable` brings it back.

`ssf uninstall` reports first and asks once. It refuses while workspaces hold uncommitted or unpushed work. `--force` bypasses that and destroys the work with the data disk, so leave that decision to the person. See [uninstall.md](uninstall.md) (`ssf skill uninstall`).

## 12. Checklist

- [ ] Probes run; the person chose where the factory runs, before anything else was asked.
- [ ] Consent recorded at each step for accounts, spending, and the model choice; every `sudo` command was run by the person.
- [ ] `ssf --version` and `ssf-server` both present, from the same release.
- [ ] `ssf setup` complete and the service enabled (package and Homebrew paths).
- [ ] `ssf vm status` reports a running VM, or herdr is reachable in host mode.
- [ ] VM or rented host: reachable over SSH, agents have passwordless `sudo`, and it is saved in the person's herdr (`herdr machine add`, shown by `herdr machine list`).
- [ ] The person was asked about reaching the factory from other devices; Tailscale enrolled if yes.
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
| Sign in the harness (section 8) | repeat; it is also the fix for an expired login |
| `ssf repo add` | re-run; it replaces that repository's settings |
| `ssf doctor`, `ssf status`, `ssf models`, `ssf agents` | re-run at any time; reads, except that `ssf models claude` refreshes Claude Code's own catalogue |

Destructive and not to be re-run casually: `ssf vm destroy`, `ssf uninstall`, and anything with `--force`.
