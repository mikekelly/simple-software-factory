---
name: ssf-setup
description: Install runbook and guided configuration for Simple Software Factory (ssf), the Omarchy daemon that turns GitHub issues assigned to a bot account into coding-agent sessions in herdr or Orca. Use when a person says "install ssf" or "set up ssf" (install.sh, the package, the service), when creating or signing in the bot GitHub account (a fresh account or an organisation's machine user, its access, token scopes and keys, committing as the bot or as the person), when running the factory inside the Firecracker microVM (ssf vm build, vm.enabled, ssf vm login) or on the host with herdr or Orca, when writing or changing ~/.config/ssf/config.toml ([github], [git], [[repo]], [daemon], [vm]), writing a repository's SSF.md, setting up the review label or project board conventions, or operating a running factory (ssf status, doctor, tell, sub, release, purge), including a session blocked on an expired harness login.
license: MIT
metadata:
  source: https://github.com/mikekelly/simple-software-factory
---

# Setting up Simple Software Factory (ssf)

ssf is a per-user daemon for Omarchy. It polls GitHub for issues and pull
requests that involve a bot account, gives each one a workspace (a git
worktree on its own branch) in herdr or Orca, starts the coding agent the
repository is configured with, and pastes every later comment, review, label
or push into that agent's terminal. Nothing runs in the cloud. By default
the whole factory (daemon, herdr, agents) runs inside a Firecracker
microVM on the person's machine, so the agents never touch their home
directory.

This skill is the runbook an agent follows when a person asks it to
install ssf, and the reference for changing a factory later. Each step
says what to decide, what to run, and how to check it. The reference is
the README (orientation, install, the everyday commands) and the files
under `docs/` (one per area); links below point at them rather than
repeating them: <https://github.com/mikekelly/simple-software-factory#readme>.
When ssf is installed, `/usr/share/doc/ssf/` holds the same README and
`docs/`.

## How to use this skill

1. **"Install ssf" means Steps 1 to 6, in order**, on the default path:
   the package and the service (Step 1), the bot account (Step 2), the
   microVM with herdr inside (Step 3), the harness signed in there (Step
   4), a repository (Step 5) and its `SSF.md` (Step 6). The alternative
   at Step 3 is to run the agents on the host, in herdr or in Orca; take
   it only when the person asks for it or the machine cannot run the VM.
   The checklist at the end is the short form.
2. **Stop where only the person can act.** Lines marked **Person:** are
   things an agent cannot do: type a sudo password, create a GitHub
   account, sign in in a browser, approve a token. Give the exact command
   or URL, say what they will see, and wait; carry on when they say it is
   done. Everything else in the steps is for the agent to run.
3. For anything other than a first install (adding a repository, switching
   driver, moving into the VM, fixing something), jump to that step; the
   steps are independent once ssf is installed and the bot is signed in.
4. If ssf is already installed, run `ssf doctor` and `ssf status` first and
   read them before changing anything. `doctor` checks the GitHub token and
   its scopes, every driver in use, the configured harnesses and whether
   each is signed in where it runs, the git identity each repository's
   agents commit and push with, the `gh` wrapper and the daemon socket;
   most setup problems show up there.
5. Prefer the CLI (`ssf repo add`, `ssf config set`, `ssf auth login`) over
   editing `config.toml` by hand: the CLI validates harness, model and
   effort ids, and the daemon picks changes up on its next poll without a
   restart. Never write `github.token` into the file; `ssf config set`
   refuses it on purpose.
6. Never sign in as the person or use their token, key or account for the
   bot; never pass `--accept-anyone-risk` on their behalf.

## Step 1: install

**Decide: the package (default) or a dev build.** The package is the
normal way; the dev build is for working on ssf itself or running an
unreleased branch.

Both need Omarchy (Arch-based; the bar widget and menu entries are
Omarchy-specific, the daemon and CLI are not). Everything else the default
setup needs is an Arch package: `herdr` (the driver, from the Omarchy
repository), `github-cli`, and `fakeroot`, `libarchive`, `e2fsprogs`,
`openssh`, `curl` for the VM image. Orca (`orca-ide-bin`) is only for the
host alternative in Step 3.

**Run `install.sh`** from the repository. It refuses on anything that is
not Arch-based, clones the repository into `~/.local/src/simple-software-factory`
(or fast-forwards it, or builds the checkout it is run from), builds and
installs the package with `makepkg -si`, makes sure `ssf.service` is
running, installs this skill for the person's coding agent with `npx
skills add`, runs `ssf doctor` and prints the next step. Re-running it
upgrades. It touches nothing under `~/.config/ssf`.

```sh
bash <(curl -fsSL https://raw.githubusercontent.com/mikekelly/simple-software-factory/master/install.sh) --deps
# or, from a checkout:
./install.sh --deps
```

| Option | When |
|--------|------|
| `--deps` | also install the packages above with `sudo pacman -S --needed`; without it, missing ones are named and the script goes on |
| `--dry-run` | print what would run and change nothing; run this first to show the person the plan |
| `--dev` | build `./target/release/ssf` and run the service from it through a drop-in (below) |
| `--src DIR`, `--ref REF` | another clone location; a branch or tag |
| `--nocheck` | skip the package's test run (faster; the package is the same) |
| `--no-skill`, `--agent LIST` | skip the skill, or install it for named agents (comma-separated, `claude-code,codex`; the script passes one `-a` per agent to the skills CLI) instead of the ones the skills CLI detects |

**Person:** `makepkg -si` (and `--deps`) call `sudo`, which asks for
their password; an agent's shell cannot answer it. Run the script where
the person can type it: in Claude Code, have them run `! ./install.sh
--deps` from the prompt, or run it in their own terminal. Show them the
`--dry-run` output first.

Check: `systemctl --user status ssf.service` is active, the **Software
Factory** icon is in the bar next to Omarchy's Agents widget, and `ssf
doctor` fails only on the bot (`bot identity not recorded`) until Step 2.
`ssf` is `/usr/bin/ssf`; the skill landed under the agent's skills
directory (`~/.claude/skills/ssf-setup` for Claude Code, a symlink into
`~/.agents/skills/ssf-setup`, which is a copy, so it survives the clone
moving; re-run the script to update it). Logs:
`journalctl --user -fu ssf.service`.

**By hand**, the same thing is `git clone
https://github.com/mikekelly/simple-software-factory && cd
simple-software-factory/packaging && makepkg -si`, then `npx skills add
mikekelly/simple-software-factory`. The package installs `/usr/bin/ssf`,
`/usr/bin/ssf-ui`, the user unit `ssf.service` (enabled for every user
through `graphical-session.target.wants`, started in the running session
by the install hook), the bar widget and `/usr/share/ssf/SSF.example.md`.
Nothing to enable.

**Dev build:** `./install.sh --dev` builds `target/release/ssf` in the
checkout, installs the package once if it is not (for the unit and the
widget), writes
`~/.config/systemd/user/ssf.service.d/dev-build.conf` pointing
`ExecStartPre` and `ExecStart` at the build, reloads and restarts the
service. Keep the build outside any herdr or Orca worktree that an agent
might release. Remove the drop-in and `systemctl --user daemon-reload` to
go back to the package (a plain `./install.sh` says when the drop-in is
there). Run `./target/release/ssf doctor` rather than the packaged `ssf
doctor`: `doctor` says whether the `ssf` on PATH is the binary running
it, so only the dev build's own `doctor` shows the two differ (agents run
the daemon's binary either way). `cargo build --release && ./target/release/ssf
run` runs one in the foreground instead; it resumes interrupted sessions
on start like the service does, but nothing restarts it for you.

Scratch runs that touch nothing of the real factory use their own
directories: `SSF_CONFIG_DIR=/tmp/ssf-dev SSF_STATE_DIR=/tmp/ssf-dev
SSF_GITHUB_TOKEN=$(gh auth token) ./target/release/ssf run --once`.

README: [Install](https://github.com/mikekelly/simple-software-factory#install);
docs: [Development](https://github.com/mikekelly/simple-software-factory/blob/master/docs/development.md).

## Step 2: the bot account

The bot is a GitHub account of its own. Every agent post is made as the
bot with a byline naming its session (`🤖#N says:`); a post by the bot
*without* a byline is treated as typed by a person, so sharing the
person's account muddles who said what. The bot identity is a default,
not a security boundary: agents run as the Unix user (or as the guest
user in the VM), so the account only limits what `gh` does by default.

**Decide: whose bot.** Ask which of these the person is, and follow that
column:

| | An individual | An organisation |
|---|---|---|
| Account | a fresh personal account, `<you>-bot` | a *machine user*: a fresh personal account owned by the organisation (`<org>-bot`), signed up with an address the organisation controls |
| Terms | GitHub allows one free machine account next to a personal one | the same; one machine user can serve every repository |
| Access | added as a collaborator with **Write** on each watched repository | made an organisation member (a team with **Write** on the repositories, or per-repository) or an outside collaborator with **Write** on each |
| Boards | a collaborator on the person's projects | access to the organisation's projects (project **Settings → Manage access**), or the team's |
| Organisation settings | none | if the organisation restricts OAuth apps, an owner approves **GitHub CLI** (the browser sign-in below is an OAuth token from gh's app); if it restricts classic personal access tokens, allow them; with SAML SSO, the token and the bot's SSH key have to be authorised for the organisation |

**2a. Create the account. Person:** in a private browser window, sign up at
<https://github.com/signup> with a separate email address (plus-addressing,
`ann+bot@example.com`, works), verify the address, and turn on two-factor
authentication (GitHub requires it for accounts that contribute code).
Nothing else: no repositories, no keys, ssf enrolls what it needs. Ask
them to tell you the login when it exists.

**2b. Give it access. Person:** on each repository the factory should
watch, **Settings → Collaborators** (or the organisation's teams), add the
bot with **Write**: it pushes branches, opens PRs and removes the `review`
label (triage is part of Write). Give it access to any project board it
should keep up to date (Step 7). The person's own account must be able to
drive the factory too: by default assignments and comments are acted on
only when they come from a collaborator with push access (Step 5,
`allowed_users`). Check, with the person's own `gh`: `gh api
repos/OWNER/NAME/collaborators/BOT/permission --jq .permission` prints
`write` or `admin` once the invitation is accepted (**Person:** the bot
accepts it, in the private window, at
<https://github.com/notifications> or the invitation email).

**2c. Sign in the bot on this machine.** **Decide: through gh (the normal
way), or a pasted token.**

- **Through gh:** `ssf auth login --web` runs gh's device flow with the
  scopes ssf needs and records the result. **Person:** the terminal
  prints a one-time code and <https://github.com/login/device>; open it
  in the private window where the bot is logged in, enter the code, and
  approve the scopes. Back in the terminal ssf asks `Use @<bot> as the bot
  account?`. Over ssh, or when a browser must not open, `BROWSER=true ssf
  auth login --web`. Do not run it for the person: it needs them at the
  terminal and in the browser. Plain `ssf auth login` lists the accounts
  `gh` already holds and offers the browser flow; `ssf auth login --user
  <bot> -y` takes an account gh already knows without questions, which is
  the form an agent can run. ssf reads the token from gh's keyring when
  needed and switches gh back to the person's own account afterwards. The
  bar widget's **Sign in bot account** and `ssf-ui login` are the same
  flow with menus.
- **A pasted token:** **Person:** logged in as the bot, at
  <https://github.com/settings/tokens> create a *classic* personal access
  token with the scopes below (a fine-grained token shows up as missing
  all of them, since ssf checks the classic scope list gh reports), and
  paste it: `printf '%s' "$TOKEN" | ssf auth login --token`. It is stored
  in `~/.config/ssf/token` (mode 0600). Choose this when gh is not
  installed or the organisation forbids OAuth apps.
- `SSF_GITHUB_TOKEN` in the daemon's environment overrides both, for
  scratch runs.

**Scopes** the token needs: `repo` (issues, PRs, pushes), `project`
(boards), `admin:public_key` and `admin:ssh_signing_key` (key
enrollment). When the gh token lacks some, `ssf auth login` asks gh to
add them (**Person:** another browser approval); `ssf doctor` reports what
is missing later. `--no-keys` skips the key and needs only `repo`.

**What login records.** `github.login` and `github.email`
(`id+login@users.noreply.github.com`; `--email` or edit `email` in
`config.toml` if the bot has a public address), and a dedicated ed25519
key under `~/.config/ssf/keys/`, enrolled on the bot account as both an
SSH key and a commit signing key. Agents then push over HTTPS with the
token (a git credential helper) or over SSH with that key, and every
commit is signed with it; with no key enrolled, signing is switched off
rather than falling back to the person's key. `ssf auth logout` revokes
the keys and forgets the bot; the gh sign-in itself stays. `ssf token`
prints the token for anything else that needs it.

Check: `ssf auth status` names the bot; `ssf doctor` says `GitHub token
belongs to @<bot>` and finds the key, and later, per repository, that the
bot can push.

**2d. Decide: commit as the bot, or as the person.** By default the
commits are the bot's too (`acme-bot <id+acme-bot@users.noreply.github.com>`,
signed with its key). If the person wants the history and their
contribution graph to show them, while `gh`, the posts and the daemon
stay the bot, set a `[git]` table, instance-wide or per repository:

```sh
ssf config set git '{ name = "Ann Person", email = "ann@example.com" }'   # both at once; one alone is refused
ssf config set git.signing_key ~/.ssh/id_ed25519      # optional: sign with the person's key (unsigned otherwise)
ssf repo set acme/widgets --git-credential token:ann  # optional: push as @ann with the token gh holds for her
```

Ask before choosing: the email must be verified on the person's GitHub
account (or be their `id+login@users.noreply.github.com`) for the avatar
and the graph; a signing key only shows *Verified* if it is registered on
that same account, so do not sign a person's commits with the bot's key;
`credential = token:<login>` needs that account signed in to `gh` on the
machine the agents run on (in the VM, the token is copied to the seed
disk) and an HTTPS `clone_url`, since SSH remotes always use the bot's
key, and it puts the person's token within the agent's reach. Author and
committer are always the same identity. `ssf doctor` prints, per
repository, who commits, signed with what and who pushes, and fails when
the key or the token is missing; `ssf config show` prints the same lines
from the file. Docs: [Committing as a
person](https://github.com/mikekelly/simple-software-factory/blob/master/docs/identity-and-bylines.md#committing-as-a-person-while-gh-stays-the-bot).

README: [Set up](https://github.com/mikekelly/simple-software-factory#set-up);
docs: [Identity and bylines](https://github.com/mikekelly/simple-software-factory/blob/master/docs/identity-and-bylines.md).

## Step 3: where the agents run: the microVM (default), or the host

**Decide: the microVM, or the host.** On the host the agents run as the
person's Unix user and can read their home directory, keyring and SSH
agent. `ssf vm` moves the daemon, herdr and every agent session into a
Firecracker microVM; the host keeps only what builds, starts and reaches
the guest. Inside, the driver is always herdr (Orca is a desktop app), so
a repository that says `orca` runs in herdr there. Sessions run as the
guest's `ssf` user, which has passwordless `sudo` for everything: an agent
there installs packages, edits units and reboots the guest as it likes,
and its first prompt says so. The VM is the boundary; `ssf vm reset` or
`ssf vm destroy` undoes whatever it did. Take the host path when the
person asks for it, wants to watch the agents in Orca, or the machine has
no `/dev/kvm` or is not x86_64 (the image is x86_64 only for now).

### The microVM (default)

Needs: `/dev/kvm` usable by the user (world-writable on Omarchy), and
`fakeroot`, `bsdtar` (libarchive), `mkfs.ext4` (e2fsprogs), `curl`,
`openssh`, plus the host's own `herdr` binary, which is copied into the
image (`install.sh --deps` installed all of it). No root. The agent runs
all of this; nothing needs the person.

```sh
ssf vm build                 # once, a few minutes: downloads Firecracker, gvproxy, a kernel; makes and provisions the image
ssf config set vm.enabled true
systemctl --user restart ssf.service   # or ssf vm start when running by hand
ssf vm status                # up, daemon answering, and a logins: line per harness
ssf status                   # runs inside the guest from now on
```

Check: `ssf vm status` says the VM is up and its daemon answers; `ssf
doctor` now runs in the guest and reports herdr there. The build's
harness list is best effort (whatever npm or a release tarball provide;
Oh My Pi gets the glibc release binary, the musl one does not run in the
guest) and is printed at the end of `ssf vm build`, one line per harness
with its version or the failure; make sure the person's harness is on it.

Other decisions in `[vm]`:

- **`vcpus`, `mem_mib`, `data_gib`, `root_gib`**: size (`2`, `4096`,
  `20`, `8`). The data disk (state, clones, worktrees, the guest home with
  the harness logins) persists across `reset`; the root disk is remade
  from the image.
- **`ssh_port`** (`2222` on `127.0.0.1`) if it clashes.
- **`files`**: host files copied in at every start; see Step 4 before
  using it for a login.

Later: edit `/usr/share/ssf/vm/guest/provision.sh` and `ssf vm build
--force` for a different image. An image built before the guest had
`sudo`, the `ssf` user or a working harness needs the same `ssf vm build
--force` followed by `ssf vm reset` (a fresh root disk; the data disk and
its logins stay). `ssf vm sync` pushes a changed config and token into
the running guest; `ssf vm restart` is needed for a new `ssf` binary or
`vm.files`; `ssf vm reset` remakes the root disk and keeps the data; `ssf
vm destroy --yes` removes everything. `ssf vm attach` opens herdr in the
guest, `ssf vm ssh` a shell, `ssf vm logs` the guest daemon's journal,
`ssf vm console` the serial console, `ssf vm ssh-config` an
`~/.ssh/config` entry. With `vm.enabled`, `status`, `peers`, `tell`,
`sub`, `release`, `purge` and `doctor` run inside the guest. Docs:
[Inside a microVM](https://github.com/mikekelly/simple-software-factory/blob/master/docs/vm.md).

### On the host: herdr or Orca

The `driver` key (top-level; `ssf config set driver orca`) picks where
workspaces and terminals live; herdr when unset. A `[[repo]]` can override
it, so one daemon can run some repositories in Orca and others in herdr.
`ssf doctor` checks every driver in use, and when `driver` is unset both
it and `ssf config show` print a note saying which driver the repositories
run in and how to pin it.

- **`herdr`** (default): the herdr terminal workspace manager. Needs a
  running herdr session (start `herdr` in a terminal and leave it). ssf
  clones under `herdr.projects_dir` (`~/ssf/projects`) and makes a worktree
  per item in `<name>.worktrees/` next to the clone. herdr only runs the
  agents it recognises (`herdr agent start --help`; `crush` is not among
  them), and `ssf repo add` / `ssf repo set` warn about a harness it does
  not for any repository that ends up in herdr (by the default or
  `--driver herdr`). Pick herdr for a terminal-only machine or over ssh.
- **`orca`**: the Orca desktop app. Needs `orca-ide-bin` installed (not in
  the Omarchy repository; install it by hand), signed in and running (the
  daemon waits for it at start). Workspaces are Orca worktrees linked to
  the issue number; the bar widget opens them in Orca. `orca.command`
  defaults to `/usr/lib/orca-ide/bin/orca-ide` (the CLI; `/usr/bin/orca-ide`
  launches the app, so do not point at that). Repositories Orca has no
  project for are cloned under `orca.projects_dir` (`~/orca/projects`).
  Pick Orca to watch agents work in a GUI and take over a terminal.

**Upgrading an install from before 2026-09-06:** the default was Orca until
then. A `config.toml` that never set `driver` moves to herdr on upgrade
without any edit of its own; the daemon logs a warning at start (when
Orca's CLI is installed) and `ssf doctor` / `ssf config show` print the
note. Run `ssf config set driver orca` before or right after the upgrade
to keep the factory on Orca; `ssf config set driver herdr` makes the new
default explicit and silences the note.

**Switching drivers** (either way, by the default or a repository's own
`driver`): every item keeps its record, and at its next activity its
workspace is re-created on the new driver, in the new driver's worktree
directory, from the item's branch. The old checkouts stay where they are
(Orca's worktrees, or `<name>.worktrees/` next to the herdr clone) for you
to clean up; the daemon logs `workspace was made by orca; re-creating it
on herdr` per item as it happens.

Either way the agent is started through `ssf launch`, which supplies the
bot identity, the `gh` wrapper that adds the byline, and the `ssf`
commands, so nothing changes for the agent. Check: `ssf doctor` reports
the driver reachable and ready. Docs:
[Drivers](https://github.com/mikekelly/simple-software-factory/blob/master/docs/drivers.md).

## Step 4: sign in the harness where the agents run

Each harness (the agent program: `claude`, `codex`, `gemini`, `grok`,
`pi`, `omp`, `opencode`, `copilot`, `crush`) is signed in once, by hand,
where the agents run. ssf does not handle first-run onboarding. Ask which
harness the person uses; that is the one to sign in and to name in Step 5.

**In the VM** (the default), nothing from the host home is visible, so
each harness needs a sign-in of its own. **Decide: `ssf vm login`, or
copy a credential with `files`.**

- **`ssf vm login <harness>`** (the normal way) runs the harness's own
  browser-less login inside the guest, in the person's terminal: a page to
  open on the host and a code to paste back (Claude Code, Gemini, OpenCode,
  Pi, Oh My Pi) or a device code (Codex, Copilot, Grok, Crush). Without a
  harness it lists those installed in the guest and asks. **Person:** it
  needs them at the terminal and in a browser, logged in to the harness's
  provider as themselves; give them the command and say what to expect
  (the table under [Harness
  logins](https://github.com/mikekelly/simple-software-factory/blob/master/docs/vm.md#harness-logins)
  has the exact flow per harness). The credential lives on the data disk
  (`reset` keeps it, `destroy` removes it). Check: `ssf vm status` shows
  the harness under `logins:` (`--json`: `logins` with `installed` and
  `logged_in`), and, once a repository names it, `ssf doctor` says it is
  signed in there.
- **`[vm] files`** copies an existing login in instead, e.g. `ssf config
  set vm.files '["~/.claude/.credentials.json"]'`, landing at the same
  path under the guest user's home (`src:dest` places one elsewhere);
  copied at every start, change it and `ssf vm restart`. Warn before
  suggesting it: a copied credential *is* the host's session, not a second
  login. A logout on either side, or Claude Code's token rotation on
  expiry, ends both, so a guest agent that runs `claude auth logout` signs
  the person out on the host. `ssf vm login` never logs anything out;
  prefer it.

**On the host**, use the harness's own login (`claude auth login`, `codex
login`, ...). **Person:** these open a browser. Check: the harness's own
status command, and `ssf doctor` once a repository names it.

A login that later expires blocks the session (Step 8); the fix is the
same command again.

## Step 5: watch a repository (`[[repo]]`)

One `[[repo]]` per watched repository in `~/.config/ssf/config.toml`;
`ssf repo add` writes it. `ssf agents` lists the harness ids Omarchy knows
and which are installed. The bot needs Write on it (Step 2b).

```sh
ssf repo add acme/widgets --harness claude
ssf repo set acme/widgets --model opus --effort high
ssf repo list --json
```

Decisions, per repository:

- **`harness`** (required): the agent program from Step 4. `ssf doctor`
  prints one line per harness in use saying whether it is signed in
  where the agents run, or that it cannot tell (Copilot keeps its login
  in a keyring).
- **`model`, `effort`**: optional. Claude, Codex, Gemini and Grok take Orca's
  model ids (`opus`, `sonnet`, `gpt-5.5`, ...) and effort levels; Pi, Oh My
  Pi, OpenCode and Copilot take their own `provider/model` ids. `ssf models
  <harness>` prints the choices, asking the installed agent where it can.
  Changing the harness resets both. Do not also put `--model`/`--effort`
  in `command`. See the table under [Models and effort
  levels](https://github.com/mikekelly/simple-software-factory/blob/master/docs/configuration.md#models-and-effort-levels).
- **`command`**: **decide whether the agent may run unattended with every
  permission granted.** Unless `command` is set, every harness starts with
  its permission-free command (`ssf agents --json` shows it as
  `launch_command`; the table is under
  [Permissions](https://github.com/mikekelly/simple-software-factory/blob/master/docs/configuration.md#permissions),
  e.g. `claude --dangerously-skip-permissions --disallowedTools
  AskUserQuestion`), because nobody sits at the terminal to approve
  anything and an agent that asks waits forever. In the VM that is fine:
  the VM is the wall. On the host it is the whole of the agent's sandbox
  (it runs as the person's Unix user). Set `command` (`ssf repo set
  owner/name --command "..."`, `--clear command` to go back) for a
  permission mode or tool deny list in the agent's own syntax, e.g.
  `--disallowedTools 'Bash(git push:*)'` for Claude Code, `--deny` for
  Grok, `--exclude-tools` for Pi. Behavioural limits (do not merge, do not
  close issues) go in `SSF.md` (Step 6), not here.
- **`driver`**: only when this repository should run in a different driver
  than the top-level default (Step 3, host path; ignored in the VM).
- **`path`**: register an existing checkout instead of cloning (host
  only; the guest clones for itself). **`clone_url`**: an SSH URL for
  private repositories (the bot's enrolled key is used). **`base_branch`**
  for issue worktrees.
- **`instructions`**: a line or two appended to this repository's initial
  prompts; anything longer belongs in `SSF.md`. **`prompt_file`** names a
  file other than `SSF.md`.

**`[github]`** is written by login; only `api_url` is set by hand, for
GitHub Enterprise (`https://ghe.example.com/api/v3`).

**`[daemon]`** keys worth deciding on (`ssf config set daemon.<key> <value>`):

| Key | Default | Decide |
|-----|---------|--------|
| `poll_interval_secs` | `10` | Unchanged listings cost nothing against the rate limit, so the default is fine; raise it on a busy token |
| `instructions` | | House rules for every repository this daemon watches; per-repository ones go in `SSF.md` |
| `review_label` | `review` | The label that asks for a review of a bot-opened PR (Step 7); `""` turns the label trigger off |
| `cleanup_grace_secs` | `900` | How long a reviewer session gets to finish after its PR closes before its read-only workspace is removed anyway; item workspaces are never removed by ssf |
| `resume_on_start` | `true` | Bring interrupted sessions back after a reboot; `startup_orca_wait_secs` (`120`) is how long to wait for the driver first |
| `include_own_events` | `false` | Leave off: on, each agent sees its own commits and posts echoed back |
| `allowed_users` | the collaborators with push access | Whose assignments, mentions, review requests, labels and comments the agents act on, e.g. `'["mikekelly"]'`; a `[[repo]]` can set its own, replacing this one (`ssf repo set owner/name --allowed-users alice,bob`; `[]` is nobody but the bot). `ssf doctor` prints the list in effect per repository. `["*"]` is anyone on GitHub and is refused without `--accept-anyone-risk` (or a `yes` at the terminal); never pass that flag on the user's behalf |

`daemon.cleanup_on_close` is accepted but does nothing. Check: `ssf doctor`
is clean (token, key, driver, harness signed in, the repository's identity
line) and `ssf status` lists the repository; then assign an issue to the
bot and a workspace should appear within a poll interval. Suggest a first
issue that is small and self-contained, says what "done" looks like and
names what to run before a PR; the agent's first comment on it (what it
is about to do) is the check that the whole chain works. Full key table:
[Configuration](https://github.com/mikekelly/simple-software-factory/blob/master/docs/configuration.md);
[Who may drive the factory](https://github.com/mikekelly/simple-software-factory/blob/master/docs/configuration.md#who-may-drive-the-factory)
for the allow list; every key with a comment: `config.example.toml`.

## Step 6: per-project notes (`SSF.md`)

ssf's own prompts carry only what ssf owns (which bot the agent is, that
the terminal is unmanned, that `gh` acts as the bot, `ssf guide`). How the
repository wants work done goes in an `SSF.md` at its root, appended to
every initial prompt under "Project notes". Start from
[`SSF.example.md`](https://github.com/mikekelly/simple-software-factory/blob/master/SSF.example.md)
(installed as `/usr/share/ssf/SSF.example.md`) and decide, per repository:

- comment on the item when starting, when a decision is needed, when done;
- ask on the item rather than guess (the agent is woken when someone
  answers);
- branch and PR conventions: work on the item's branch, `Closes #N`, do not
  merge or close, who reviews;
- test, lint and packaging commands to run before a PR;
- what the project board columns mean, and who to ask about what;
- how to review, for reviewer sessions (they get the file too).

It is read from the item's own checkout, so a PR that changes it is seen
with its own version. `repo.prompt_file` points at another file, inside the
worktree (`.github/ssf.md`) or an absolute path for notes not to commit.
`CLAUDE.md`/`AGENTS.md` stay for what every user of the repository wants;
`SSF.md` is for what only ssf agents need. Docs: [The per-project prompt
file](https://github.com/mikekelly/simple-software-factory/blob/master/docs/configuration.md#the-per-project-prompt-file).

## Step 7: boards and the `review` label

**Project boards.** No setup: if the item is on a GitHub project (v2)
board, the agent's prompt lists the board, the card's Status, the options
and the `gh project item-edit` command that changes it, and the agent is
told to keep the Status accurate. ssf never moves cards and prescribes no
mapping; put the conventions (which column means what) in `SSF.md`. The
lookup uses the bot token's `project` scope, so the bot needs access to
the board (Step 2b).

**Getting a bot PR reviewed.** A session must not review its own PR, and
GitHub refuses a review request from a PR's own author, so the label is the
request: create a `review` label in the repository (`gh label create
review --repo owner/name`, or set `daemon.review_label` to one that
exists), and a person, or the author's agent, adds it (`gh pr edit N
--add-label review`). ssf starts a separate reviewer session on a
read-only checkout, the review arrives on the PR (as a comment review,
since the bot cannot approve its own PR), ssf removes the label, and
adding it again asks for another look. The bot needs triage access to
remove the label. A review *request* to the bot works for PRs the bot did
not open. Docs: [Reviewer
sessions](https://github.com/mikekelly/simple-software-factory/blob/master/docs/sessions.md#reviewer-sessions),
[Project boards](https://github.com/mikekelly/simple-software-factory/blob/master/docs/prompts.md#project-boards).

## Step 8: day to day

Triggers: assign an issue or PR to the bot, @mention it, or add the
`review` label to a bot-opened PR. Within a poll interval a workspace
appears and the agent starts with the whole story so far.

```sh
ssf status [--json]           # every tracked item, its session and what the driver reports
ssf peers [--all]             # the agent sessions and what each is doing
ssf doctor                    # token, scopes, drivers, harnesses and their sign-in, gh wrapper, daemon socket
ssf tell 12 "stop, I'm changing the spec"   # paste into that session's terminal (12:reviewer for a reviewer)
ssf sub 12 | ssf unsub 12     # follow an item from a shell (--as owner/repo#N to act as a session)
ssf release 12 [--force]      # remove a closed item's workspace once its branch is on origin
ssf purge [--dry-run] [--older-than DAYS] [--force]   # sweep the clean workspaces of closed items
ssf ui service disable|enable|toggle|status           # the bar toggle, from a shell
ssf vm status | attach | ssh | logs                   # the guest, when vm.enabled
journalctl --user -fu ssf.service
```

Things to know when operating it:

- ssf never removes a workspace on its own. Closing an item tells the agent
  to push, comment and run `ssf release`; `ssf purge` is the person's sweep
  for what was left. Both refuse when anything is not on origin unless
  `--force`.
- `tell` is not mirrored to GitHub; decisions go on the item as comments,
  which the agent receives like any other activity.
- A daemon restart is invisible to agents; a reboot triggers the startup
  pass that relaunches interrupted sessions. `ssf run --once` runs one
  pass for scratch tests.
- With `vm.enabled`, `status`, `peers`, `tell`, `sub`, `release`, `purge`
  and `doctor` run inside the guest; `ssf vm status` says whether it is up.
- **Upgrading ssf**: re-run `install.sh` (pull, rebuild, reinstall; the
  install hook restarts the service, and with `vm.enabled` that restarts
  the guest with the new binary; a VM started by hand needs `ssf vm
  restart`).
- **A session shows as blocked** when its harness login expired or was
  revoked under it (Claude Code sits at `Login expired`, the others at
  their sign-in screens). `ssf status` prints a `BLOCKED:` line naming the
  harness, since when and the fix (`--json`: `blocked` on the session,
  `blocked_sessions` at the top); the widget turns urgent with the same
  line; the item got one comment. ssf holds the item's activity and refuses
  `tell` meanwhile, and nothing is lost. The fix is the sign-in where the
  agents run: `ssf vm login <harness>` in the guest, `claude auth login`
  (or the harness's own login) on the host. ssf checks every pass and,
  once signed in, restarts the harness with its conversation resumed and
  delivers what was held; a person running `/login` in the terminal lifts
  it too. `ssf doctor` prints one line per harness in use saying whether it
  is signed in, or that it cannot tell (in the guest, when `vm.enabled`),
  so run it first.
- `ssf doctor` also lists untagged posts by the bot (posts made without
  the byline, i.e. typed by a person or made outside the wrapper).
- **Stopping the factory**: `ssf ui service disable` (or the bar toggle)
  stops the service and keeps it stopped across logins; `enable` turns it
  back on; `systemctl --user stop ssf.service` stops it until the next
  login. Agents already running are left where they are and get what
  they missed when the daemon is back. With `vm.enabled`, stopping the
  service shuts the guest down cleanly.
- **Uninstalling**: `ssf ui uninstall` (widget and menu entries), `ssf
  auth logout` (revokes the bot's keys on GitHub and forgets it), `ssf vm
  destroy --yes` (the VM and its disks), then `sudo pacman -R ssf`
  (**Person:** sudo). Left behind on purpose, for the person to remove by
  hand once they have checked them: `~/.config/ssf`, `~/.local/state/ssf`,
  and the clones and worktrees under `~/ssf/projects` (or Orca's
  projects), which may hold unpushed work; `ssf purge --dry-run` lists
  them first. The bot GitHub account itself is not touched.

README: [Everyday
commands](https://github.com/mikekelly/simple-software-factory#everyday-commands); docs: [Workspaces after
close](https://github.com/mikekelly/simple-software-factory/blob/master/docs/sessions.md#workspaces-after-close-release-and-purge),
[A harness that is not signed
in](https://github.com/mikekelly/simple-software-factory/blob/master/docs/sessions.md#a-harness-that-is-not-signed-in).

## Checklist for a first install

1. `install.sh --deps` (**Person** types the sudo password);
   `systemctl --user status ssf.service` active.
2. Bot account created (**Person**), with Write on each repository and
   access to the boards; a `review` label exists.
3. `ssf auth login --web` as the bot (**Person** in a private window);
   `ssf auth status` names it, `ssf doctor` finds the token and key.
4. Commits as the bot (default) or as the person (`[git]` table); `ssf
   doctor` shows the identity per repository.
5. `ssf vm build`, `ssf config set vm.enabled true`, `systemctl --user
   restart ssf.service`; `ssf vm status` up. (Host alternative: a herdr
   session running, or Orca signed in with `driver = "orca"`.)
6. The harness signed in where the agents run: `ssf vm login <harness>`
   (**Person** at the terminal), or by hand on the host; `ssf doctor`
   says so per harness.
7. `ssf repo add owner/name --harness <id>`; `ssf status` lists it and
   `ssf doctor` is clean.
8. `SSF.md` at the repository root.
9. Assign an issue to the bot; a workspace appears and the agent comments.
