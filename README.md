# Simple Software Factory (ssf)

Assign a GitHub issue to a bot account and a coding agent picks it up on your
machine: it gets a workspace of its own, works on the issue, opens a pull
request, answers the review, and reports back on the issue. Every issue gets
its own agent. The agents know about each other, about the project board, and
about the notes your repository keeps for them.

ssf is a small daemon for [Omarchy](https://omarchy.org/). It runs the
agents in [herdr](https://herdr.dev/) by default, or in
[Orca](https://onorca.dev/), so you can watch them work, take over, or
nudge them at any time (see [Drivers](docs/drivers.md)). Nothing
runs in the cloud: the daemon polls GitHub and drives the multiplexer, and
the agents are the ones you already have installed (Claude Code, Codex,
...). If you would rather keep the agents off your machine altogether, the
whole factory can run [inside a microVM](docs/vm.md).

## One issue, start to finish

This repository is built by ssf, so its own history shows what you get.
Here is issue [#18](https://github.com/mikekelly/simple-software-factory/issues/18)
on 2026-09-04, from assignment to merge, as it appears on GitHub. The bot
account is @OverlayBot; every comment below is from that account, and the
timestamps are UTC.

**20:24 — the issue is assigned to the bot** (for the second time; the
first assignment at 20:21 was undone and redone). Within a minute a workspace
named after the issue appears in Orca and an agent starts in it. Two
minutes after the assignment it posts:

> **OverlayBot** commented at 20:26
>
> Starting on this. Plan, following the decisions from #16:
>
> - A startup pass in the engine: for every active session that owns its
>   workspace (...) if the Orca workspace still exists but has no live agent
>   terminal, relaunch it through the existing delivery path (...)
> - Relaunches are sequential; each waits for the harness to settle before
>   the next starts.
> - At daemon start, `orca status` is retried every 10 s for up to two
>   minutes (configurable). (...)
> - README and config example updated (...)
>
> Will report back with the PR.

**20:35 — a pull request appears.** [#29](https://github.com/mikekelly/simple-software-factory/pull/29)
is opened by the bot from the issue's branch. Its description starts with
`Closes #18.` and ends with what was verified and how. On the issue:

> **OverlayBot** commented at 20:35
>
> Done in #29 (https://github.com/mikekelly/simple-software-factory/pull/29).
>
> Summary: a startup pass runs once when Orca first answers and starts again
> every active session that owns its workspace (...). README and
> `config.example.toml` updated; package rebuilt and pkgver bumped.
>
> Two calls I made that were not spelled out in the issue, say if either
> should change: reviewer sessions are included in the pass (...), and the
> bounded wait defers the first poll but never drops the pass.

**20:35 — the `review` label goes on the pull request.** That asks for a
review. The agent that wrote the code must not review it, so ssf starts a
second agent in a read-only checkout of the pull request. Four minutes later
it posts a review:

> **OverlayBot** reviewed at 20:39
>
> **Verdict: changes requested** (posted as a comment review because GitHub
> does not let the bot request changes on its own pull request).
>
> The design is right and the code does what #18 asked: (...) `cargo test`
> (75 passing), `cargo fmt --check` and `cargo clippy` are clean here (...)
>
> One gap I would like fixed before merging, then this is good to go.
>
> **Candidates miss a retired owner whose delegated items are still active**
> (`src/engine.rs:611-627`). The item filter is
> `active && shares_workspace_of.is_none()`. When a parent closes while items
> bound to its workspace are still open, retirement (...) keeps its workspace
> for the dependents (...). After a reboot that harness is the one that needs
> starting again, but the pass skips it twice over (...). Suggested shape:
> (...) The existing test `startup_pass_looks_at_owning_active_sessions_only`
> can grow one case (...)
>
> Observations, no change needed: (...)

ssf removes the label once the review is posted. The review lands in the
author's session as new activity, and the author answers on the pull request:

> **OverlayBot** commented at 20:42
>
> Thanks, the gap was real: retirement keeps a closed parent's workspace
> while items bound to it are open, and their deliveries still route to it,
> so its harness is the one the pass should bring back. Fixed in 777e390:
> `resume_candidates` now collects the session that acts on every active,
> seeded item (...). The test grew the case you suggested (...)
>
> Also rebased onto master (...), reran `cargo fmt --check`, `cargo clippy`
> (nothing new) and `cargo test` (76 passing), and rebuilt the package for
> the pkgver bump. Force-pushed with lease.

It pushes and puts the `review` label back on. The same reviewer comes back
with what happened in between:

> **OverlayBot** reviewed at 20:43
>
> **Verdict: approved** (posted as a comment review because GitHub does not
> let the bot approve its own pull request).
>
> Looked at 777e390 and the rebase. This is mergeable. (...)

**20:44 — the pull request is merged and the issue closes.** Merging is
not the agent's call. The closure reaches the author's agent, which leaves
a last comment on the issue:

> **OverlayBot** commented at 20:44
>
> Wrapped up: #29 is merged, nothing uncommitted here, board card moved to
> Done. After the next package install, the daemon will resume interrupted
> sessions on start.

Once the agent is done and everything is on GitHub, it gives its workspace
back with `ssf release`; ssf never removes one on its own. The branch stays
on GitHub. Twenty minutes passed between the assignment and the merge. The
human's part was the assignment, the label and the merge; on this
repository even those were done by a project-management agent on the
maintainer's instructions.

**Which agent said what.** GitHub shows the same bot account for every
agent, so each post an agent makes starts with a byline naming its issue,
linked to it. A post from the agent on issue #31 looks like this on GitHub:

> **OverlayBot** commented
>
> 🤖#31 says:
>
> Merged in #32 (c73d0fd). Final note: two commits landed on the branch
> after the merge (...)

A reviewer's byline reads `🤖#29 (reviewer) says:`. The posts quoted above
from #18 predate the byline (it arrived with #32 on 2026-09-05, and the
`says:` with #42) and carried the same mark out of sight at the end of the
body; every post since carries it on the first line. A post by the bot
account *without* a byline was typed by a person. How the byline works,
and how the daemon reads it, is in
[Identity and bylines](docs/identity-and-bylines.md).

## The key ideas

- **One agent per issue or pull request.** Each gets its own workspace
  (a git worktree on its own branch, in Orca or herdr) and its own agent
  session, from the moment the bot is assigned, @mentioned, or asked to
  review until the item is closed. Comment on the issue and the agent hears
  it. Close the issue and the agent wraps up; its workspace stays until the
  agent, or a person, says it can go.
- **Agents know the project.** An agent is told the issue, everything that
  has happened on it, the project boards it is on and their columns, and
  the notes your repository keeps in `SSF.md` (how you want work done,
  who to ask, what the columns mean). It can see who else is working on the
  repository, follow other issues, hand work off by opening an issue
  assigned to the bot, and get its own pull request reviewed by a separate
  reviewer agent.
- **Everything is on GitHub.** Agents talk to people, and to each other,
  through issue and pull request comments. Every post carries the byline of
  the session that made it. The one exception is `ssf tell`, a message
  typed straight into an agent's terminal, kept for nudges that would be
  noise on the item.
- **Nothing runs in the cloud.** The daemon polls GitHub, creates workspaces
  in Orca (or herdr), and starts the agents you have installed, with the
  bot's credentials, so what the agents do on GitHub is done as the bot.
  On your own machine that is a default rather than a wall (the agents run
  as you); the [microVM](docs/vm.md) is the wall.

## How it works

Every few seconds ssf asks GitHub for the open issues and pull requests that
involve the bot. For a new one it creates a workspace (in Orca or in herdr,
depending on the [driver](docs/drivers.md)), checked out on a
branch for the issue (or on the pull request's branch, so pushes update
the pull request), and starts the agent there with the whole story so far.
From then on every new comment, review, label or push on the item is pasted
into that agent's terminal as a message: it steers the agent if it is busy
and wakes it if it is idle. If a terminal is gone, or the whole workspace,
ssf brings it back and resumes the same conversation, including after a
reboot. When the item is closed the agent is told to push what is worth
keeping and, only then, to release its workspace; ssf never deletes one
that might hold work (see
[Workspaces after close](docs/sessions.md#workspaces-after-close-release-and-purge)).

Only people you allow can drive it: by default the repository's
collaborators with push access (GitHub's Write role or higher), or a list
you set (see
[Who may drive the factory](docs/configuration.md#who-may-drive-the-factory)).

## Install

Run the install script on Omarchy. It builds and installs the package,
starts the service, and installs the `ssf-setup` skill for your coding
agent, which then walks you through the rest (`/ssf-setup` in Claude
Code, or just ask it to set up ssf):

```sh
bash <(curl -fsSL https://raw.githubusercontent.com/mikekelly/simple-software-factory/master/install.sh) --deps
```

[`install.sh`](install.sh) refuses on anything that is not Arch-based,
clones the repository under `~/.local/src` (or builds the checkout it is
run from), runs `makepkg -si` (the one step that asks for your sudo
password), and with `--deps` also installs what the default setup needs:
[herdr](https://herdr.dev/), `github-cli` and the tools that build the
microVM image. `--dry-run` shows the plan, `--dev` runs the service from a
dev build, and re-running it upgrades. The skill,
[`skills/ssf-setup`](skills/ssf-setup/SKILL.md), is the runbook an agent
follows: the package, the bot account, the microVM with herdr inside
(the default; or the agents on this machine, in herdr or in
[Orca](https://onorca.dev/)), the harness sign-in, a repository and its
`SSF.md`. Without the script, `npx skills add
mikekelly/simple-software-factory` installs the skill, and this does the
build from a checkout:

```sh
cd packaging && makepkg -si
```

The package installs:

| Path | What |
|------|------|
| `/usr/bin/ssf` | the daemon and management CLI |
| `/usr/bin/ssf-ui` | Omarchy menu flows (sign in, add repo, ...) |
| `/usr/lib/systemd/user/ssf.service` | background service, enabled for every user via `graphical-session.target.wants` |
| `/usr/share/ssf/omarchy-plugin/` | the bar widget, copied into `~/.config/omarchy/plugins/ssf.factory` on first start |
| `/usr/share/ssf/SSF.example.md` | a starting point for your repository's `SSF.md` |
| `/usr/share/ssf/config.example.toml` | every configuration key, with a comment |
| `/usr/share/ssf/vm/` | the scripts and units that build the microVM image |
| `/usr/share/doc/ssf/` | this file and `docs/` |

The service starts with the graphical session, and the package's install
hook also starts it in any session that is running at install time, so there
is nothing to enable. (If it was installed with nobody logged in, the first
login starts it, or `systemctl --user start ssf.service` does.) On its first
run it installs the **Software Factory** bar widget (next to Omarchy's Agents
widget) and a **Factory** submenu in the Omarchy menu.

The service is the intended way to run the factory: it comes back with the
next login after a reboot (unless it was switched off with the toggle),
waits for the driver, and resumes the agent sessions the reboot cut off. A dev
build started by hand (`ssf run`) does the same on start, but nothing
restarts it for you (see [Development](docs/development.md)).

## Set up

Click the factory icon in the bar, or open the Omarchy menu and pick
**Factory**. From there:

- **Sign in bot account**: the bot is a GitHub account of its own, created
  for the factory rather than yours (every agent post is made as it, and a
  post by the bot *without* a byline reads as a person's), with Write
  access on each repository it works and to its project boards. The
  skill's Step 2 walks through creating one. Sign it in with the GitHub
  CLI: the flow lists the accounts `gh` already holds and offers "sign in
  another account in the browser", which runs gh's device flow (use a private
  window so GitHub does not reuse your own session); whoever signs in becomes
  the bot. ssf never stores the token: it reads it from gh's keyring when it
  needs it, and switches gh back to your own account afterwards. It then
  records the bot's commit identity (`login <id+login@users.noreply.github.com>`),
  generates a dedicated ed25519 key under `~/.config/ssf/keys/` and enrolls it
  on the bot account as both an SSH key and a commit signing key. If the gh
  token lacks the scopes for that (`repo`, `project`,
  `admin:public_key`, `admin:ssh_signing_key`), ssf asks gh to add them.
  `ssf auth logout` revokes the keys and forgets the bot; the gh sign-in
  itself stays. If the commits should carry your own name rather than the
  bot's, a `[git]` table in the config says so while `gh` stays the bot
  (see [Committing as a person](docs/identity-and-bylines.md#committing-as-a-person-while-gh-stays-the-bot)).
- **Watch a repository**: type `owner/name` and pick the agent that works it.
  The agent list comes from Omarchy's agent catalogue and only shows agents
  that are installed. (The configuration calls the agent program the
  *harness*: `claude`, `codex`, `gemini`, ...) Sign each agent in once, by
  hand, on this machine; ssf starts them with the flags that let them run
  unattended (see [Permissions](docs/configuration.md#permissions)).
- **Manage repositories**: change the agent, model or effort level for a
  repository, or stop watching it.
- The toggle in the panel header enables or disables the service.

Then assign an issue or pull request to the bot on GitHub, @mention it, or
put the `review` label on a pull request the bot opened. Within a poll
interval (10 s by default) a workspace shows up in herdr (or in Orca, with
`driver = "orca"`), and in the widget
under "Sessions": one row per agent session with the issue or PR (click the
title for GitHub), its GitHub state (open, closed, merged, draft), the
agent's state (working, waiting, idle, done), what it last said or the tool
it is running, the branch and when it was last active. Clicking a row opens
the workspace in the driver. The bar icon turns urgent while an agent is
waiting
for input.

A good first issue is small and self-contained, says what "done" looks
like (a test that passes, a file that changes, a command that works), and
names what to run before opening a pull request. Assign it to the bot and
watch the issue: within a couple of minutes the agent comments with what
it is about to do, and later with the pull request. Put the `review`
label on the pull request for a second agent's review, answer or merge as
you would for a colleague, and close the issue when it is done; the agent
then pushes what is left, comments once more and gives its workspace back.

The same can be done from a terminal:

```sh
ssf-ui login                      # menu-driven; or in a terminal:
ssf auth login                    # pick an account gh knows, or sign in another in the browser
ssf auth login --web              # straight to the browser flow; prints the URL and code,
                                  # so it also works over ssh (set BROWSER=true to stop gh opening one)
ssf auth login --user overlay-bot # an account gh already knows, no questions
printf '%s' "$TOKEN" | ssf auth login --token   # a pasted token instead of gh
ssf auth status
ssf agents                        # which agents Omarchy knows and which are installed
ssf repo add acme/widgets --harness claude
ssf config set driver orca        # sessions in Orca instead of herdr (the default)
ssf status
ssf peers                         # the agent sessions and what each is doing
ssf doctor                        # token and scopes, drivers, harness logins, gh wrapper, daemon socket
```

Put an `SSF.md` at the root of the repository to tell agents how you want
them to work: when to comment, what to run before a PR, what the board
columns mean, who reviews. [`SSF.example.md`](SSF.example.md) is a starting
point, and this repository's own [`SSF.md`](SSF.md) is what produced the
comments quoted above (see
[The per-project prompt file](docs/configuration.md#the-per-project-prompt-file)).

## Everyday commands

Everything except the credentials is scriptable, so an agent on the machine
can reconfigure the factory:

```sh
ssf repo list --json
ssf repo add acme/widgets --harness codex --instructions "Run make test before opening a PR."
ssf repo set acme/widgets --model opus --effort high    # ssf models <agent> lists the ids
ssf repo set acme/widgets --harness pi --model openrouter/anthropic/claude-sonnet-4 --effort high
ssf repo set acme/widgets --clear model --clear effort
ssf repo set acme/widgets --allowed-users alice,bob     # who may drive this repository
ssf repo remove acme/widgets
ssf config get daemon.poll_interval_secs
ssf config set daemon.poll_interval_secs 60
ssf config set daemon.instructions "Always open PRs as drafts."
ssf status --json                 # every tracked item, its session and what the driver reports
ssf peers [--repo owner/name] [--all] [--json]
ssf sub 12 | ssf sub acme/widgets#12   # follow an item (inside a session, or --as owner/repo#N)
ssf unsub 12
ssf subs                          # what this session follows, who follows its items
ssf tell 12 "stop, I'm changing the spec"   # steer that session from your shell: pastes into its terminal
ssf release [12 | --as acme/widgets#12] [--force]   # remove a session's workspace once its work is on origin
ssf purge [--dry-run] [--older-than DAYS] [--force] # remove the clean workspaces of closed items; list the rest
ssf guide                         # the reference for agents (the initial prompt points at it)
ssf ui service disable|enable|toggle|status
journalctl --user -fu ssf.service
```

Config changes are picked up on the next poll; no restart needed. Every
key, with its default, is in [Configuration](docs/configuration.md).

Things to know when operating it:

- ssf never removes a workspace on its own. Closing an item tells the agent
  to push, comment and run `ssf release`; `ssf purge` is your sweep for what
  was left. Both refuse when anything is not on origin unless `--force`.
- `tell` is not mirrored to GitHub; decisions go on the item as comments,
  which the agent receives like any other activity.
- A daemon restart is invisible to agents; a reboot triggers the startup
  pass that relaunches interrupted sessions.
- A review of a bot-opened pull request is asked for with the `review`
  label (GitHub refuses a review request from a PR's own author); a
  separate reviewer agent posts it and ssf takes the label off.

**Stopping it.** The toggle in the bar widget, or `ssf ui service
disable`, stops the service and keeps it from starting at the next login
(`enable` turns it back on); `systemctl --user stop ssf.service` stops it
until the next login. Running agents are left where they are: nothing
reaches them while the daemon is down, and it delivers what they missed
when it comes back. With the factory in a microVM, stopping the service
shuts the guest down cleanly.

**Uninstalling.** While the daemon is still running, `ssf purge
--dry-run` lists the workspaces of closed items and whether each is clean
and pushed, and `ssf status` shows the open ones; deal with anything
unpushed first. Then, in this order: `ssf ui service disable` (stops the
service, and the guest with it), `ssf ui uninstall` (the bar widget and
menu entries), `ssf auth logout` (revokes the bot's keys on GitHub and
forgets it), `ssf vm destroy --yes` (the microVM and its disks), then
`sudo pacman -R ssf`. Left for you to remove by hand: `~/.config/ssf`
(config and the bot's key), `~/.local/state/ssf` (state, and the marker
that keeps a disabled service off, so a reinstall stays stopped until
`ssf ui service enable`), and the clones and worktrees under
`~/ssf/projects` (or Orca's projects), which may hold unpushed work.

## The rest of the story

The reference, one file per area. Each starts with a line saying what it
covers and who needs it; they are installed under `/usr/share/doc/ssf/docs/`.

| Read | When you want to know |
|------|-----------------------|
| [Configuration](docs/configuration.md) | every key in `config.toml`; the `SSF.md` prompt file; models and effort levels; the permission-free command each agent is started with; who may drive the factory |
| [Drivers](docs/drivers.md) | Orca versus herdr, and what each one does with workspaces and terminals |
| [Inside a microVM](docs/vm.md) | running the whole factory in a Firecracker VM: the image, what gets in, reaching it, what persists |
| [What the agent is told](docs/prompts.md) | the first prompt, the messages an agent receives, project boards, and what is left to `SSF.md` |
| [Identity and bylines](docs/identity-and-bylines.md) | how `gh` and `git` act as the bot inside a session, and how the byline and origin tag say which session posted |
| [Sessions](docs/sessions.md) | which session owns an item, reviewer sessions, following and messaging other sessions, release and purge |
| [Under the hood](docs/internals.md) | polling, delivery, resume and restarts; the `ssf status --json` fields; known limits |
| [Development](docs/development.md) | building, scratch runs, a dev build as the service, the source layout |

Agents get their own reference from `ssf guide`, printed by the running
binary so it cannot drift from the daemon.
