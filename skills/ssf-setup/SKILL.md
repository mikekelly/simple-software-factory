---
name: ssf-setup
description: Guided setup and configuration of Simple Software Factory (ssf), the Omarchy daemon that turns GitHub issues assigned to a bot account into coding-agent sessions in Orca or herdr. Use when installing ssf, signing in the bot account, writing or changing ~/.config/ssf/config.toml ([github], [[repo]], [daemon], [vm]), choosing between the Orca and herdr drivers, running the factory inside the Firecracker microVM (ssf vm) and signing harnesses in there (ssf vm login), writing a repository's SSF.md, setting up the review label or project board conventions, or operating a running factory (ssf status, doctor, tell, sub, release, purge), including a session blocked on an expired harness login.
license: MIT
metadata:
  source: https://github.com/mikekelly/simple-software-factory
---

# Setting up Simple Software Factory (ssf)

ssf is a per-user daemon for Omarchy. It polls GitHub for issues and pull
requests that involve a bot account, gives each one a workspace (a git
worktree on its own branch) in Orca or herdr, starts the coding agent the
repository is configured with, and pastes every later comment, review, label
or push into that agent's terminal. Nothing runs in the cloud.

This skill walks a person through the decisions, in order. Each step says
what to decide, what to run, and how to check it. The reference is the
README (orientation, install, the everyday commands) and the files under
`docs/` (one per area); links below point at them rather than repeating
them: <https://github.com/mikekelly/simple-software-factory#readme>. When
ssf is installed, `/usr/share/doc/ssf/` holds the same README and `docs/`.

## How to use this skill

1. Ask what the person is doing: a first install, adding a repository to a
   running factory, switching driver, moving into the VM, or fixing
   something. Jump to that step; the steps are independent once ssf is
   installed and the bot is signed in.
2. If ssf is already installed, run `ssf doctor` and `ssf status` first and
   read them before changing anything. `doctor` checks the GitHub token and
   its scopes, every driver in use, the configured harnesses and whether
   each is signed in where it runs, the `gh` wrapper and the daemon socket;
   most setup problems show up there.
3. Prefer the CLI (`ssf repo add`, `ssf config set`, `ssf auth login`) over
   editing `config.toml` by hand: the CLI validates harness, model and
   effort ids, and the daemon picks changes up on its next poll without a
   restart. Never write `github.token` into the file; `ssf config set`
   refuses it on purpose.
4. Do not run `ssf auth login` for the person: it opens a browser device
   flow and has to be done by them. Tell them the exact command and what
   they will see.

## Step 1: install

**Decide: the package or a dev build.** The package is the normal way; the
dev build is for working on ssf itself or running an unreleased branch.

Both need Omarchy (Arch-based; the bar widget and menu entries are
Omarchy-specific, the daemon and CLI are not), `gh` (github-cli), and one
driver installed: herdr (the default) or Orca (`orca-ide-bin`, signed in).
See Step 4 for which.

**Package** (from a checkout of the repository):

```sh
git clone https://github.com/mikekelly/simple-software-factory
cd simple-software-factory/packaging && makepkg -si
```

This installs `/usr/bin/ssf`, `/usr/bin/ssf-ui`, the user unit
`ssf.service` (enabled for every user through
`graphical-session.target.wants`, started in the running session by the
install hook), the bar widget and `/usr/share/ssf/SSF.example.md`. Nothing
to enable. Check: `systemctl --user status ssf.service` is active, and the
**Software Factory** icon is in the bar next to Omarchy's Agents widget.
Logs: `journalctl --user -fu ssf.service`.

**Dev build:**

```sh
cargo build --release          # ./target/release/ssf
./target/release/ssf run       # foreground; Ctrl-C stops it
```

To run a dev build as the service instead, install the package once (for
the unit and the widget) and point the unit at the build with a drop-in:

```ini
# ~/.config/systemd/user/ssf.service.d/dev-build.conf
[Service]
ExecStartPre=
ExecStartPre=-/home/you/src/simple-software-factory/target/release/ssf ui install --quiet
ExecStart=
ExecStart=/home/you/src/simple-software-factory/target/release/ssf run
```

Then `systemctl --user daemon-reload && systemctl --user restart
ssf.service`. Keep the build outside any Orca or herdr worktree that an
agent might release. Remove the drop-in when the package is reinstalled
from master. Run `./target/release/ssf doctor` rather than the packaged
`ssf doctor`: `doctor` says whether the `ssf` on PATH is the binary running
it, so only the dev build's own `doctor` shows the two differ (agents run
the daemon's binary either way). A dev build started by hand resumes
interrupted sessions on start like the service does, but nothing restarts
it for you.

Scratch runs that touch nothing of the real factory use their own
directories: `SSF_CONFIG_DIR=/tmp/ssf-dev SSF_STATE_DIR=/tmp/ssf-dev
SSF_GITHUB_TOKEN=$(gh auth token) ./target/release/ssf run --once`.

README: [Install](https://github.com/mikekelly/simple-software-factory#install);
docs: [Development](https://github.com/mikekelly/simple-software-factory/blob/master/docs/development.md).

## Step 2: the bot account

**Decide: which GitHub account is the bot.** Use a separate account
(`acme-bot`), not the person's own. Every agent post is made as the bot with
a byline naming its session (`🤖#N says:`); a post by the bot *without* a
byline is treated as typed by a person, so sharing the person's account
muddles who said what. The bot identity is a default, not a security
boundary: agents run as the Unix user, so the account only limits what
`gh` does by default. The bot account needs write access to every watched
repository (it pushes branches, opens PRs, and removes the `review` label),
and access to any project boards it should keep up to date.

**Decide: sign in through gh, or paste a token.**

- Through gh (the normal way): `ssf auth login` lists the accounts `gh`
  already holds and offers to sign in another in the browser. Use a private
  browser window so GitHub does not reuse the person's own session. `ssf auth
  login --web` goes straight to the device flow and prints the URL and code,
  so it works over ssh (`BROWSER=true` stops gh opening a browser). `ssf auth
  login --user acme-bot` takes an account gh already knows, no questions. ssf
  reads the token from gh's keyring when needed and switches gh back to the
  person's own account afterwards. The bar widget's **Sign in bot account**
  and `ssf-ui login` are the same flow with menus.
- A pasted token: `printf '%s' "$TOKEN" | ssf auth login --token`. It is
  stored in `~/.config/ssf/token` (mode 0600). Use a classic personal access
  token with the scopes below: ssf checks the classic scope list gh reports,
  so a fine-grained token shows up as missing all of them.
- `SSF_GITHUB_TOKEN` in the daemon's environment overrides both, for
  scratch runs.

**Scopes** the token needs: `repo` (issues, PRs, pushes), `project` (boards),
`admin:public_key` and `admin:ssh_signing_key` (key enrollment). When the
gh token lacks some, `ssf auth login` asks gh to add them; `ssf doctor`
reports what is missing later.

**What login records.** `github.login` and `github.email`
(`id+login@users.noreply.github.com`; edit `email` in `config.toml` if the
bot has a public address), and a dedicated ed25519 key under
`~/.config/ssf/keys/`, enrolled on the bot account as both an SSH key and
a commit signing key. Agents then push over HTTPS with the token (a git
credential helper) or over SSH with that key, and every commit is signed
with it; with no key enrolled, signing is switched off rather than falling
back to the person's key. `ssf auth logout` revokes the keys and forgets the
bot; the gh sign-in itself stays.

`ssf token` prints the token for anything else that needs it (an agent runs
`GH_TOKEN="$(ssf token)" gh ...`). Check: `ssf auth status` names the bot
and `ssf doctor` is happy with the scopes.

README: [Set up](https://github.com/mikekelly/simple-software-factory#set-up);
docs: [Identity and bylines](https://github.com/mikekelly/simple-software-factory/blob/master/docs/identity-and-bylines.md).

## Step 3: watch a repository (`[[repo]]`)

One `[[repo]]` per watched repository in `~/.config/ssf/config.toml`;
`ssf repo add` writes it. `ssf agents` lists the harness ids Omarchy knows
and which are installed.

```sh
ssf repo add acme/widgets --harness claude
ssf repo set acme/widgets --model opus --effort high
ssf repo list --json
```

Decisions, per repository:

- **`harness`** (required): the agent program. `claude`, `codex`, `gemini`,
  `grok`, `pi`, `omp`, `opencode`, `copilot`, `crush`. Sign each harness in
  once, by hand, where the agents run: on the host, with the harness's own
  login (`claude auth login`, ...); inside the VM, with `ssf vm login
  <harness>` (Step 5). ssf does not handle first-run onboarding. `ssf
  doctor` prints one line per harness in use saying whether it is signed
  in there, or that it cannot tell (Copilot keeps its login in a keyring);
  a login that later expires blocks the session (Step 8).
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
  anything and an agent that asks waits forever. That is the whole of the
  agent's sandbox on the host (it runs as the person's Unix user; the VM in
  Step 5 is the wall). Set `command` (`ssf repo set owner/name --command
  "..."`, `--clear command` to go back) for a permission mode or tool deny
  list in the agent's own syntax, e.g. `--disallowedTools 'Bash(git
  push:*)'` for Claude Code, `--deny` for Grok, `--exclude-tools` for Pi.
  Behavioural limits (do not merge, do not close issues) go in `SSF.md`
  (Step 6), not here.
- **`driver`**: only when this repository should run in a different driver
  than the top-level default (Step 4).
- **`path`**: register an existing checkout instead of cloning.
  **`clone_url`**: an SSH URL for private repositories (the bot's enrolled
  key is used). **`base_branch`** for issue worktrees.
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

`daemon.cleanup_on_close` is accepted but does nothing. Check: `ssf status`
lists the repository; then assign an issue to the bot and a workspace should
appear within a poll interval. Full key table:
[Configuration](https://github.com/mikekelly/simple-software-factory/blob/master/docs/configuration.md);
[Who may drive the factory](https://github.com/mikekelly/simple-software-factory/blob/master/docs/configuration.md#who-may-drive-the-factory)
for the allow list; every key with a comment: `config.example.toml`.

## Step 4: choose the driver (Orca or herdr)

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
  `--driver herdr`). Pick herdr for a terminal-only machine, over ssh, or
  as the driver inside the VM (Step 5).
- **`orca`**: the Orca desktop app. Needs `orca-ide-bin`
  installed, signed in and running (the daemon waits for it at start).
  Workspaces are Orca worktrees linked to the issue number; the bar widget
  opens them in Orca. `orca.command` defaults to
  `/usr/lib/orca-ide/bin/orca-ide` (the CLI; `/usr/bin/orca-ide` launches
  the app, so do not point at that). Repositories Orca has no project for
  are cloned under `orca.projects_dir` (`~/orca/projects`). Pick Orca to
  watch agents work in a GUI and take over a terminal.

**Upgrading an install from before 2026-09-06:** the default was Orca until
then. A `config.toml` that never set `driver` moves to herdr on upgrade
without any edit of its own; the daemon logs a warning at start (when
Orca's CLI is installed) and `ssf doctor` / `ssf config show` print the
note. Run `ssf config set driver orca` before or right after the upgrade
to keep the factory on Orca; `ssf config set driver herdr` makes the new
default explicit and silences the note.

Either way the agent is started through `ssf launch`, which supplies the
bot identity, the `gh` wrapper that adds the byline, and the `ssf`
commands, so nothing changes for the agent. Docs:
[Drivers](https://github.com/mikekelly/simple-software-factory/blob/master/docs/drivers.md).

## Step 5: inside a microVM (optional)

**Decide: run on the host, or in a Firecracker microVM.** On the host the
agents run as the person's Unix user and can read their home directory,
keyring and SSH agent. `ssf vm` moves the daemon, herdr and every agent
session into a microVM; the host keeps only what builds, starts and reaches
the guest. Inside, the driver is always herdr (Orca is a desktop app), so a
repository that says `orca` runs in herdr there. Sessions run as the guest's
`ssf` user, which has passwordless `sudo` for everything: an agent there
installs packages, edits units and reboots the guest as it likes, and its
first prompt says so. The VM is the boundary; `ssf vm reset` or `ssf vm
destroy` undoes whatever it did.

Needs: `/dev/kvm` usable by the user (world-writable on Omarchy), and
`fakeroot`, `bsdtar` (libarchive), `mkfs.ext4` (e2fsprogs), `curl`,
`openssh`, plus the host's own `herdr` binary, which is copied into the
image. No root.

```sh
ssf vm build                 # once, a few minutes: downloads Firecracker, gvproxy, a kernel; makes and provisions the image
ssf config set vm.enabled true
systemctl --user restart ssf.service   # or ssf vm start when running by hand
ssf vm status                # up, daemon answering, and a logins: line per harness
ssf vm login claude          # sign the harness in inside the guest (one per harness in use)
ssf status                   # runs inside the guest from now on
```

**Harness logins in the guest.** Nothing from the host home is visible
there, so each harness needs a sign-in of its own. **Decide: `ssf vm
login`, or copy a credential with `files`.**

- **`ssf vm login <harness>`** (the normal way) runs the harness's own
  browser-less login inside the guest, in the person's terminal: a page to
  open on the host and a code to paste back (Claude Code, Gemini, OpenCode,
  Pi, Oh My Pi) or a device code (Codex, Copilot, Grok, Crush). Without a
  harness it lists those installed in the guest and asks. Like `ssf auth
  login`, it needs the person at the terminal: give them the command and
  say what to expect. The credential lives on the data disk (`reset` keeps
  it, `destroy` removes it). Check: `ssf vm status` shows the harness under
  `logins:` (`--json`: `logins` with `installed` and `logged_in`), and
  `ssf doctor` says it is signed in there.
- **`[vm] files`** copies an existing login in instead, e.g. `files =
  ["~/.claude/.credentials.json"]`, landing at the same path under the guest
  user's home (`src:dest` places one elsewhere); copied at every start,
  change it and `ssf vm restart`. Warn before suggesting it: a copied
  credential *is* the host's session, not a second login. A logout on
  either side, or Claude Code's token rotation on expiry, ends both, so a
  guest agent that runs `claude auth logout` signs the person out on the
  host. `ssf vm login` never logs anything out; prefer it.

Other decisions in `[vm]`:

- **`vcpus`, `mem_mib`, `data_gib`, `root_gib`**: size. The data disk
  (state, clones, worktrees, the guest home with the harness logins)
  persists across `reset`; the root disk is remade from the image.
- **`ssh_port`** (`2222` on `127.0.0.1`) if it clashes.

The build's harness list is best effort (whatever npm or a release tarball
provide; Oh My Pi gets the glibc release binary, the musl one does not run
in the guest) and is printed at the end of `ssf vm build`, one line per
harness with its version or the failure. Edit
`/usr/share/ssf/vm/guest/provision.sh` and `ssf vm build --force` for a
different image. An image built before the guest had `sudo`, the `ssf`
user or a working harness needs the same `ssf vm build --force` followed by
`ssf vm reset` (a fresh root disk; the data disk and its logins stay).
After that: `ssf vm sync` pushes a changed config and token into the
running guest; `ssf vm restart` is needed for a new `ssf` binary or
`vm.files`; `ssf vm reset` remakes the root disk and keeps the data; `ssf
vm destroy --yes` removes everything. `ssf vm attach` opens herdr in the
guest, `ssf vm ssh` a shell, `ssf vm logs` the guest daemon's journal,
`ssf vm console` the serial console, `ssf vm ssh-config` an `~/.ssh/config`
entry. Docs: [Inside a
microVM](https://github.com/mikekelly/simple-software-factory/blob/master/docs/vm.md),
[Harness
logins](https://github.com/mikekelly/simple-software-factory/blob/master/docs/vm.md#harness-logins).

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
the board.

**Getting a bot PR reviewed.** A session must not review its own PR, and
GitHub refuses a review request from a PR's own author, so the label is the
request: create a `review` label in the repository (or set
`daemon.review_label` to one that exists), and a person, or the author's
agent, adds it (`gh pr edit N --add-label review`). ssf starts a separate
reviewer session on a read-only checkout, the review arrives on the PR
(as a comment review, since the bot cannot approve its own PR), ssf removes
the label, and adding it again asks for another look. The bot needs triage
access to remove the label. A review *request* to the bot works for PRs
the bot did not open. Docs: [Reviewer
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
- **A session shows as blocked** when its harness login expired or was
  revoked under it (Claude Code sits at `Login expired`, the others at
  their sign-in screens). `ssf status` prints a `BLOCKED:` line naming the
  harness, since when and the fix (`--json`: `blocked` on the session,
  `blocked_sessions` at the top); the widget turns urgent with the same
  line; the item got one comment. ssf holds the item's activity and refuses
  `tell` meanwhile, and nothing is lost. The fix is the sign-in where the
  agents run: `claude auth login` (or the harness's own login) on the host,
  `ssf vm login <harness>` in the guest. ssf checks every pass and, once
  signed in, restarts the harness with its conversation resumed and
  delivers what was held; a person running `/login` in the terminal lifts
  it too. `ssf doctor` prints one line per harness in use saying whether it
  is signed in, or that it cannot tell (in the guest, when `vm.enabled`),
  so run it first.
- `ssf doctor` also lists untagged posts by the bot (posts made without
  the byline, i.e. typed by a person or made outside the wrapper).

README: [Everyday
commands](https://github.com/mikekelly/simple-software-factory#everyday-commands); docs: [Workspaces after
close](https://github.com/mikekelly/simple-software-factory/blob/master/docs/sessions.md#workspaces-after-close-release-and-purge),
[A harness that is not signed
in](https://github.com/mikekelly/simple-software-factory/blob/master/docs/sessions.md#a-harness-that-is-not-signed-in).

## Checklist for a first install

1. Orca signed in and running, or a herdr session running.
2. `makepkg -si`; `systemctl --user status ssf.service` active.
3. `ssf auth login` as the bot; `ssf auth status`, `ssf doctor` clean.
4. Bot has write access to the repository; a `review` label exists.
5. Each harness signed in where the agents run: by hand on this machine, or
   `ssf vm login <harness>` in the guest; `ssf doctor` says so per harness.
6. `ssf repo add owner/name --harness <id>`; `ssf status` lists it.
7. `SSF.md` at the repository root.
8. Assign an issue to the bot; a workspace appears and the agent comments.
