# Install a factory

Read this when someone asks you to set up Simple Software Factory for them, from nothing to a factory that watches one repository and has worked its first issue. Offline copy: `ssf skill setup`.

## 0. Who this is for, and the outcome

You are an agent doing this on behalf of a person, on a machine you have not seen before. The person owns every decision that costs money, creates an account, needs `sudo`, or widens who can drive the factory. You own the probing, the reading, the commands, and the diagnosis.

At the end:

- `ssf` and `ssf-server` are installed somewhere the factory can run.
- A separate GitHub bot account is signed in, with Write access on one repository.
- A harness (the coding agent program) is signed in where sessions run.
- One repository is watched, with a harness, model and effort the person chose.
- One small issue assigned to the bot has produced an agent comment on GitHub.
- `ssf doctor` passes.

Work through the sections in order. Every step says what a good result looks like and what is safe to re-run.

## 1. Facts to gather, and consent to obtain

Gather these before running anything. Probe what you can; ask for the rest.

| Fact | How |
|---|---|
| OS and architecture | probe: `uname -s -m` |
| CPU count, RAM, free disk | probe (section 2) |
| Hardware virtualisation | probe: `/dev/kvm` on Linux, the backend on macOS |
| A systemd user session | probe: `systemctl --user is-system-running` |
| `gh` and `herdr` present | probe: `command -v gh herdr` |
| Does a bot GitHub account exist, or may one be created | **ask** |
| Which repository to watch | **ask** |
| Who owns that repository, and can they grant the bot Write | **ask** |
| Which coding harness they already pay for | **ask** |
| How much metered spend is acceptable | **ask** |
| Where the factory should run, when more than one option fits | **ask**, after you present the options |

Consent you must obtain explicitly, in words, before acting:

| Needs consent | Why |
|---|---|
| Creating a GitHub account | it is their identity and their email |
| Running anything with `sudo` | it changes their machine |
| Renting a host, or any metered API spending | it costs them money |
| The harness, model and effort for the repository | it costs them money and sets quality |
| `--allowed-users '*'` / `--accept-anyone-risk` | it lets anyone on GitHub drive their factory |
| `ssf uninstall --force`, `ssf purge --force` | these can destroy unpushed work |

Decide these yourself, no need to ask: which probe commands to run, which install path fits the measurements, VM sizes (let `ssf vm build` choose), when to re-run a failed idempotent step, how to read `ssf doctor`.

## 2. Choose where the factory runs

Run the probes, then propose. The person picks.

```sh
uname -s -m
nproc 2>/dev/null || sysctl -n hw.ncpu
free -g 2>/dev/null || sysctl -n hw.memsize
df -h "$HOME"
test -r /dev/kvm && test -w /dev/kvm && echo kvm-ok
systemctl --user is-system-running
command -v gh herdr
```

`free`, `/dev/kvm` and `systemctl --user` are Linux only; on macOS `sysctl -n hw.memsize` reports bytes and the VM runs through lima instead of KVM.

| What the probes say | Path |
|---|---|
| Linux, `/dev/kvm` readable and writable, `systemctl --user` answers, resources allow a VM | **Local VM** (the default). Section 3.1, then 7. |
| macOS | **Homebrew + lima**. Section 3.2, then 7. |
| Linux, no KVM or no systemd user session, but enough CPU/RAM, and the person accepts that agents see this user's files | **Host mode on this machine**. Section 3.1 or 3.3, then 7. |
| Too few resources here, or the person does not want agents on this machine | **A rented host** runs the factory; this machine only drives it. Section 3.3 on the host, 3.4 here. |
| A factory already runs somewhere else | **Client only**. Section 3.4. |

### Is a VM reasonable here

`ssf vm build` sizes the guest from the host and prints what it chose:

- vCPUs: host CPUs minus one, at least 2.
- Memory: half the RAM, at least 4096 MiB.
- Data disk: half the free space where the disk lands, at least 20 GiB, sparse so it reserves nothing up front.

Rule of thumb for judging "reasonable": each parallel agent session wants about one vCPU and 2 GiB of RAM. A 4-core, 8 GB machine gives a guest of 3 vCPUs and 4 GiB, which is one or two sessions at a time and leaves the person's own machine usable. Below that, propose host mode or a rented host rather than a VM. Allow roughly 30 GB of disk headroom for images and data, plus room for the repositories and their builds.

### The rented-host option

If this machine cannot host the factory, the factory can live on a server the person rents and this machine talks to it over SSH. Suitable hosts include the dedicated server that comes with a Grok Bot account, or a VPS from Hetzner, Linode, OVH or a similar provider. Requirements: Linux, a non-root user, outbound HTTPS, and enough RAM for the sessions by the rule above. KVM is not required, because that host runs in host mode.

Renting costs money: get explicit consent before proposing a specific product, and do not create the account for them.

## 3. Install

### 3.1 Linux package

Download the matching asset from [GitHub Releases](https://github.com/mikekelly/simple-software-factory/releases) and install it (`sudo`, so **ask first**):

| Family | Command |
|---|---|
| Arch | `sudo pacman -U ssf-*.pkg.tar.zst` |
| Debian / Ubuntu | `sudo apt install ./ssf_*_amd64.deb` |
| Fedora / RHEL | `sudo dnf install ./ssf-*.x86_64.rpm` |

One package holds both the `ssf` client and the `ssf-server` daemon. Packages are x86_64.

Prerequisites the package does not always bring: GitHub CLI 2.40 or newer, Git, jq, an OpenSSH client (`ssh-keygen` enrolls the bot's key), and, for host mode, herdr. The VM installs its own herdr in the guest. Distro-specific commands and quirks are in [platform-specifics.md](platform-specifics.md).

```sh
ssf --version
command -v ssf-server
```

### 3.2 macOS, Homebrew

```sh
brew install mikekelly/tap/ssf
ssf --version
```

The formula brings `gh` and `lima`. On macOS the factory runs in a lima VM; `ssf setup` (section 4) enables a launchd agent per target, so `brew services` is not used. Install `herdr` with Homebrew as well only if the factory will run on the host rather than in the VM.

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

Persist that `PATH` line for future shells. Supply the prerequisites yourself with the host's package manager: CA certificates, curl, Git, jq, GitHub CLI 2.40 or newer, an OpenSSH client, herdr, and the harness. The bare binaries carry no service units, no VM scripts and no configuration examples, so skip `ssf setup` on this path and leave the server catalog empty, so that the client and the foreground daemon share one configuration and state directory.

Run the two processes under the same Unix user, HOME and PATH, each in its own persistent terminal or under the host's process supervisor:

```sh
herdr server        # terminal one
ssf-server          # terminal two
```

### 3.4 Client only, driving a factory elsewhere

Install the client the same way (package, Homebrew, or the `ssf` binary alone) and reach the remote factory over SSH. SSF opens no TCP listener; SSH starts the server-side endpoint on the far machine, which needs `ssf-server` on its noninteractive SSH PATH.

```sh
ssf --server user@host status
```

For a stable name instead of a destination, see the server catalog in [configuration.md#server-catalog](configuration.md#server-catalog). A client-only machine needs no daemon and no `ssf setup`.

## 4. `ssf setup` and the service

On the package and Homebrew paths only:

```sh
ssf setup
```

It validates any existing configuration, creates the conventional VM server `ssf-server` when nothing is configured yet (selected automatically while it is the only one), enables that server's background service, and on Linux enables login linger through `sudo` so the factory survives logout and starts at boot. Expect it to end with `ssf setup complete; selected service enabled` and a `next:` line naming `ssf vm build` (VM) or `ssf auth login --web` (host).

For host mode, create the local target first so setup prepares that shape:

```sh
ssf server add local --local
ssf setup
```

Do not create both a VM server and a local server just to compare: with more than one configured, unqualified commands require `--server NAME`.

`ssf setup` is idempotent and safe to re-run at any time. It creates no bot, watches no repository, and never removes configuration. If it stops on linger, run the `sudo loginctl enable-linger $USER` command it prints, then run it again.

```sh
ssf doctor
```

At this point failures for the bot, driver, harness and repositories are expected. Only unreadable configuration or a missing `gh` needs fixing now.

## 5. The bot account

The factory acts on GitHub as an account of its own. Every agent post carries a byline naming the session and what it runs, and a post from the bot *without* a byline is read as typed by a person, so sharing the person's own account confuses who said what. The bot is a default, not a security boundary: agents run as a Unix user and the account only bounds what `gh` does by default.

**Ask first, then let them create it.** In a private browser window they sign up at `https://github.com/signup` with a separate address (plus-addressing works), verify it, and turn on two-factor authentication. For an organisation, the same thing owned by the organisation as a machine user. Nothing else is needed: no repositories, no keys.

Sign it in where the factory runs:

```sh
ssf auth login --web
ssf auth status
```

`--web` runs gh's device flow: the terminal prints a one-time code and `https://github.com/login/device`, which the person opens in the window where the bot is signed in. Over SSH, or where no browser should open, prefix `BROWSER=true`. `--user <bot>` checks the approved account is the intended one. In VM mode the credential is written inside the guest. Where OAuth apps are forbidden, use a classic personal access token instead: `printf '%s' "$TOKEN" | ssf auth login --token`. A fine-grained token reads as missing every scope.

Scopes: `repo`, `project` (boards), `admin:public_key` and `admin:ssh_signing_key` (key enrollment). `--no-keys` skips the key and needs only `repo`, at the cost of unsigned commits.

Login records `github.login` and `github.email`, and enrolls a dedicated ed25519 key on the bot account as both an SSH key and a signing key. `ssf auth logout` revokes those keys and forgets the bot.

**Write access.** An invitation is not access. The repository owner invites the bot with Write, from their own account:

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

## 6. Who may drive the factory

Whatever reaches the bot on GitHub is relayed into a running agent's terminal, so this is a real trust boundary. By default the agents act only on assignments, mentions, review requests, labels and comments from collaborators with push access, which GitHub calls Write or higher. The daemon fetches that list each pass and `ssf doctor` prints it per repository. The person's own account must be on it to assign issues to the bot. Nothing needs setting for the default.

To narrow or widen it:

```sh
ssf config set daemon.allowed_users '["alice", "bob"]'
ssf repo set OWNER/NAME --allowed-users alice,bob
```

`"*"` means anyone on GitHub. It is refused unless someone types `yes` at the terminal or passes `--accept-anyone-risk`. **An agent must never pass that flag on the person's behalf.** If the collaborator list cannot be fetched, nothing is acted on for that repository until a list is configured, and `ssf doctor` says so.

## 7. Where the agents run

### The VM

```sh
ssf vm build
ssf vm status
```

`ssf vm build` picks the backend (Firecracker on Linux, lima on macOS and on Linux with qemu), sizes the guest by the rule in section 2, writes those sizes to the selected target, and provisions git, gh, herdr and the harness CLIs. Read the printed sizes and the harness installation results: a harness that failed to install cannot be signed in. `--vcpus`, `--mem-mib` and `--data-gib` override the rule; an already-set size is kept.

Expect `ssf vm status` to report a running VM and working SSH. From here, `ssf doctor`, `ssf auth`, `ssf repo` and `ssf status` operate inside the guest even when typed on the host. `ssf vm logs` shows the guest daemon's journal.

Re-running `ssf vm build` after a failure is safe; it keeps an existing image unless `--force` is given, and it keeps the data disk either way. Sessions run as the guest's `ssf` user with passwordless sudo; the VM is the isolation boundary. More in [vm.md](vm.md) (`ssf skill vm`).

### Host mode

Agents run as this Unix user and can reach this user's files and credentials, and the default launch commands bypass the harness's permission prompts because the terminals are unattended. Say this plainly to the person before choosing it.

herdr provides the workspaces and terminals. Start `herdr server` for headless operation, or leave an interactive `herdr` running. ssf clones under `herdr.projects_dir` (`~/ssf/projects`) and makes a worktree per item beside the clone.

```sh
ssf doctor
```

Expect doctor to say the driver (herdr, the only one) is reachable and ready. Details in [drivers.md](drivers.md).

## 8. Sign in the harness

Each harness is signed in once, by hand, where the agents run. ssf does not do first-run onboarding.

```sh
ssf vm login claude      # or codex, gemini, copilot, opencode, pi, omp, grok, crush
```

Without a harness argument it lists those installed in the guest and asks. The harness's own login then runs inside the guest in this terminal: a URL to open here and a code to paste back, or a device code. The credential is written on the guest's data disk; nothing is copied from this machine. `ssf vm reset` keeps it, `ssf vm destroy` removes it. API keys go through the same command.

In host mode, use the harness's own login as the Unix user that runs the sessions, and verify a real request in a herdr-launched pane: a passing login check in your own shell does not prove that a pane can authenticate.

Per-harness flows and their quirks are in [platform-specifics.md](platform-specifics.md).

Check: `ssf vm status` lists the harness as logged in on its `logins:` line. `ssf vm login` is safe to re-run, and re-running it is also the fix when a login later expires under a running session.

## 9. Watch the first repository

Get the person's explicit choice of harness, model and effort first. Examples and recommendations are not consent. List what the installation actually offers, in the place the sessions will run:

```sh
ssf agents
ssf models HARNESS
ssf vm run -- models HARNESS      # VM: ask the guest directly
```

`ssf models` names what answered: the harness's own catalogue, its listing command, or ssf's built-in table. `ssf agents --json` adds the supported effort levels and launch commands. Then:

```sh
ssf repo add OWNER/NAME --harness HARNESS --model MODEL --effort EFFORT
ssf repo list --json
```

`--model` and `--effort` are required in the resulting configuration wherever the harness supports them; omit one only where it is unsupported. Use `ssf repo set` with the same options to change a setting later; changes apply to the next started or resumed session.

Before the first issue, the repository also needs `SSF.md` committed at the root of its **default branch**, and any items that predate this factory need reviewing with `ssf candidates` and adopting with `ssf adopt`. The full path, including choosing a first issue and what to expect from it, is [repositories.md](repositories.md) (`ssf skill repo`).

`ssf repo add` rewrites the entry for a repository that is already there, so it is safe to re-run after a mistake; it is also how you correct a harness or model chosen in error.

## 10. Verify

```sh
ssf doctor
ssf status
```

Healthy looks like: the token belongs to the bot; the driver is reachable and ready; the harness is installed and signed in where sessions run; each repository shows its GitHub identity, its allowed users, the commit identity, and its SSF agent guidance. `ssf status` names the configured account, then the repositories and their tracked items with no last error.

Two failures are normal before the first issue: a missing checkout, and the `gh` and `ssf` command links, which are created when the first agent starts. Anything else, work through [troubleshooting.md](troubleshooting.md) (`ssf skill troubleshoot`).

## 11. Upgrading, stopping, uninstalling

Upgrade by installing the next release's package the same way it was installed; the package restarts the active service, which in VM mode takes the guest down and up on the new binary and resumes the interrupted sessions. Standalone binaries are replaced in pairs with the daemon stopped. Configuration, state, keys and VM disks survive an upgrade. See [operate.md](operate.md) (`ssf skill operate`).

`ssf ui service disable` stops the service and keeps it stopped across logins; `ssf ui service enable` brings it back.

`ssf uninstall` reports first and asks once. It refuses while workspaces hold uncommitted or unpushed work. `--force` bypasses that and destroys the work with the data disk, so leave that decision to the person. See [uninstall.md](uninstall.md) (`ssf skill uninstall`).

## 12. Checklist

- [ ] Probes run; the person chose where the factory runs.
- [ ] Consent recorded for accounts, `sudo`, spending, and the model choice.
- [ ] `ssf --version` and `ssf-server` both present, from the same release.
- [ ] `ssf setup` complete and the service enabled (package and Homebrew paths).
- [ ] Bot account created; `ssf auth status` names it.
- [ ] Bot has Write on the repository, verified with `push: true`, and board access if there is a board.
- [ ] Allowed users are deliberate; `*` only with the person's consent.
- [ ] `ssf vm status` reports a running VM, or herdr is reachable in host mode.
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
| `ssf vm login` | re-run; it is also the fix for an expired login |
| `ssf repo add` | re-run; it replaces that repository's settings |
| `ssf doctor`, `ssf status`, `ssf models`, `ssf agents` | read-only, re-run at any time |

Destructive and not to be re-run casually: `ssf vm destroy`, `ssf uninstall`, and anything with `--force`.
