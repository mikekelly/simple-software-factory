# Simple Software Factory (ssf)

Assign a GitHub issue to a bot account and a coding agent picks it up on your
machine: it gets a workspace of its own, works on the issue, opens a pull
request, answers the review, and reports back on the issue. Every issue gets
its own agent. The agents know about each other, about the project board, and
about the SSF operating guidance your repository keeps for them.

ssf is a small daemon for Linux. Release packages support the Arch family
(including [Omarchy](https://omarchy.org/)) and the Debian family (including
Ubuntu). macOS support is next on the roadmap, but is not supported yet.

It is built on top of
[herdr](https://herdr.dev/), which runs the agent workspaces and terminals so
you can watch them work, take over, or nudge them at any time (see
[Workspaces and terminals](docs/drivers.md)). The daemon polls GitHub and drives
the multiplexer, and
the agents are the ones you already have installed (Claude Code, Codex,
...). If you would rather keep the agents off your machine altogether, the
whole factory can run [inside a microVM](docs/vm.md).

## Where to run a factory

There are two main ways to run an ssf factory on Linux:

- **On a VPS:** run the factory directly on a dedicated Linux host or
  container. Start with the [headless-host guide](docs/headless-host.md).
- **On your local machine, inside a microVM:** keep the daemon, herdr and coding
  agents isolated from your host. This is the recommended local setup; start
  with [Setup](docs/setup.md).

## One issue, start to finish

This repository is built by ssf, so its own history shows what you get.
Here is issue [#18](https://github.com/mikekelly/simple-software-factory/issues/18)
on 2026-09-04, from assignment to merge, as it appears on GitHub. The bot
account is @OverlayBot; every comment below is from that account, and the
timestamps are UTC.

**20:24 — the issue is assigned to the bot** (for the second time; the
first assignment at 20:21 was undone and redone). Within a minute a workspace
named after the issue appears in herdr and an agent starts in it. Two minutes after the assignment
it posts:

> **OverlayBot** commented at 20:26
>
> Starting on this. Plan, following the decisions from #16:
>
> - A startup pass in the engine: for every active session that owns its
>   workspace (...) if the workspace still exists but has no live agent
>   terminal, relaunch it through the existing delivery path (...)
> - Relaunches are sequential; each waits for the harness to settle before
>   the next starts.
> - At daemon start, driver status is retried every 10 s for up to two
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
> Summary: a startup pass runs once when the driver first answers and starts again
> every active session that owns its workspace (...). README and
> `config.example.toml` updated; package rebuilt and pkgver bumped.
>
> Two calls I made that were not spelled out in the issue, say if either
> should change: reviewer sessions are included in the pass (...), and the
> bounded wait defers the first poll but never drops the pass.

**20:35 to 20:43 — a second pair of eyes.** In this run a separate
reviewer agent, which ssf at the time started on a `review` label, found
one real gap (the startup pass skipped a retired session whose delegated
items were still open), the author fixed it in 777e390, rebased, reran
the tests and answered on the pull request, and the reviewer came back
with "mergeable". ssf no longer starts that second agent: it runs one
session per item, and the agent that did the work runs that loop itself
before saying it is done. This repository's `SSF.md` asks for a gauntlet:
hand the diff, the issue and your claim of what it does to a fresh agent
that has not seen your reasoning (a subagent, or a different agent and
model through herdr), ask it to break it, fix what it finds, and go again
until nothing that matters is left; then say on the issue what it found.

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
human's part was the assignment and the merge; on this repository even
those were done by a project-management agent on the maintainer's
instructions.

**Which agent said what.** GitHub shows the same bot account for every
agent, so each post an agent makes starts with a byline naming its issue,
linked to it. A post from the agent on issue #31 looks like this on GitHub:

> **OverlayBot** commented
>
> 🤖#31 says:
>
> Merged in #32 (c73d0fd). Final note: two commits landed on the branch
> after the merge (...)

The posts quoted above from #18 predate the byline (it arrived with #32 on 2026-09-05, and the
`says:` with #42) and carried the same mark out of sight at the end of the
body; every post since carries it on the first line. A post by the bot
account *without* a byline was typed by a person. A post whose byline is
`🤖 ssf`, followed by a fenced `ssf` block (`ssf attaching agent to
issue:` and a few `key: value` lines), is the daemon itself, saying that
it attached a session to the issue, brought it back, held it for a
sign-in, gave up on it or released its workspace, so the issue's timeline
shows what ssf did as well as what the agent did. How the byline works,
and how the daemon reads it, is in
[Identity and bylines](docs/identity-and-bylines.md); the daemon's events
are listed in [Sessions](docs/sessions.md#what-ssf-says-on-the-item).

## The key ideas

- **One agent per issue or pull request.** Each gets its own workspace
  (a git worktree on its own branch, in herdr) and its own agent
  session, from the moment the bot is assigned, @mentioned, or asked to
  review until the item is closed. Comment on the issue and the agent hears
  it. Close the issue and the agent wraps up; its workspace stays until the
  agent, or a person, says it can go.
- **Agents know the project.** An agent is told the issue, everything that
  has happened on it, the project boards it is on and their columns, and
  the SSF operating contract your repository keeps in `SSF.md` (how it owns
  and communicates work, who to ask, what the columns mean). Repository-wide
  build, test and implementation policy remains in `AGENTS.md`. `SSF.md` is
  injected into the issue-owning main agent only, never its harness-created
  subagents: the one place to give the orchestrating agent its strategy
  (what it keeps, what it delegates) without a subagent reading it as its
  own ([Writing SSF.md](docs/ssf-md.md)).
  Optional `~/.ssf/SSF.md` and `~/.ssf/SSF.<harness>.md` files give every
  session on one factory machine shared context before those repository files.
  The main agent can see who else is working on the
  repository, follow other issues, hand work off by opening an issue
  assigned to the bot, and is expected to put its own work through a
  gauntlet (a fresh agent it arranges itself) before calling it done: one
  session per item, the rule for the second pair of eyes in `SSF.md`.
- **Everything is on GitHub.** Agents talk to people, and to each other,
  through issue and pull request comments. Every post carries the byline of
  the session that made it, and the item is the only channel between
  sessions: nobody reaches an agent's terminal behind the record. Debugging
  or rescuing a session is done at its terminal through herdr.
- **The server is driven from a terminal.** `ssf handover` and `ssf assign`
  set the harness, model and effort an item's session runs with; a comment on
  an item is activity for that session, never a command to ssf itself
  ([Talking to the factory](docs/sessions.md#talking-to-the-factory)).
- **Repository renames reconnect.** ssf records GitHub's immutable repository
  id and periodically resolves its current name. Rename or transfer a watched
  repository through GitHub and ssf repairs its configuration, session state,
  historical origin aliases and managed checkout remotes before polling it.
- **Nothing runs in the cloud.** The daemon polls GitHub, creates workspaces
  in herdr, and starts the agents you have installed, with the
  bot's credentials, so what the agents do on GitHub is done as the bot.
  On your own machine that is a default rather than a wall (the agents run
  as you); the [VM](docs/vm.md) is the wall.

## How it works

Run `ssf dashboard` for the [session dashboard](docs/dashboard.md): active
agents in a live terminal view, originating and assigned issues, activity and summaries.
With no server selection, it connects to all configured servers.
Use `ssf --server HOST dashboard` (or `SSF_SERVER`) for a remote factory, or
repeat `--server` to group several factories; the adaptive card TUI runs in your
terminal. Herdr panes optionally support agent navigation,
and local VM factories use normal forwarding. The server can also serve an
[optional browser dashboard](docs/dashboard.md#optional-server-web-dashboard), disabled by default.

Every few seconds ssf asks GitHub for the open issues and pull requests that
involve the bot. For a new one it creates a workspace in herdr, checked out on a
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

## Agent skills

Agents can discover SSF through the [skills CLI](https://github.com/vercel-labs/skills):

```sh
npx skills add mikekelly/simple-software-factory -g
```

`-g` installs the skill globally, so every project's agent can find it; drop it
to install for one project. The `working-with-ssf` skill is a thin pointer: it
names the affordances — operating a factory, working as an ssf-spawned agent,
liaising for a person, writing `SSF.md`, auditing project guidance, install and
upgrade — links to installation instructions, then directs agents to `ssf
skill`. The binary bundles a command overview and deeper topics such as
`ssf skill setup`, `ssf skill client-cli`, `ssf skill server`, and
`ssf skill config`. These print locally without configuration, a daemon, or
network access; `--server` and `SSF_SERVER` do not select documentation from a
remote factory. Guidance matches the executing binary. Use `ssf guide` inside
a factory session for its session-specific collaboration reference.

## Install

Choose the installation for the Linux machine:

- **A VPS, stripped container, or Linux without KVM / a systemd user
  session:** follow [VPS / headless host](docs/headless-host.md) for
  standalone binaries and host mode, from prerequisites to a watching factory.
- **Omarchy, Arch, Debian or Ubuntu with VM and service support:**
  use the packages below and [Setup](docs/setup.md).
- **Only controlling an existing factory over SSH:** install the
  [standalone client](docs/install-binaries.md#client-only-operate-an-existing-factory-over-ssh).

If an always-on assistant should also act as your liaison for SSF-tracked
work, continue with the [liaison guide](docs/liaison.md): it covers an
assistant on the factory host, and one on your own machine, which reaches the
factory over SSH and saves its herdr server locally. The liaison's GitHub access
and event delivery are separate from factory bot enrollment, and the guide
includes an optional Grok Bot example.

Until ssf is published in Omarchy's package repository, download the current
Arch package and install it with pacman. This is a normal package-manager
install: pacman resolves `github-cli` and `herdr` from Omarchy's configured
repositories and records every installed file.

```sh
sudo pacman -U ./ssf-<version>-1-x86_64.pkg.tar.zst
ssf setup
```

`ssf setup` is the explicit per-user step. It creates the conventional managed
VM server `ssf-server`, enables `ssf@ssf-server.service` for `default.target`, and asks visibly for
authorization to enable systemd linger. Linger lets the user service start at
boot and remain available after logout. It does not sign in a bot account;
follow [Setup](docs/setup.md) for `ssf auth login --web` and repository setup.

The optional Omarchy widget is installed separately:

```sh
omarchy plugin add https://github.com/mikekelly/simple-software-factory.git --enable
```

It is only a status and control interface. Adding, updating, enabling or
removing it never builds, installs, upgrades, starts or removes ssf. If the
package or setup is missing, the widget shows the command needed to fix that
state. `mise` is used only by developers building ssf from source.

Release packages are available for x86_64 Arch-family systems (including
Omarchy) and Debian-family systems (including Ubuntu). macOS support is planned
next, but there is no supported macOS package yet.
Any macOS-specific notes elsewhere in the documentation describe the
work-in-progress implementation, not a supported installation path.

- **Omarchy**: use a release newer than `v0.1.0` and
  install `ssf-<version>-1-x86_64.pkg.tar.zst` with `sudo pacman -U
  ssf-*.pkg.tar.zst`; `github-cli` and `herdr` come from Omarchy's repositories.
- **Arch**: the same `.pkg.tar.zst`, after `github-cli` (`extra`) and
  `herdr` or `herdr-bin` (AUR), which it depends on. Without a Wayland
  session (X11, a server), see [Setup](docs/setup.md) step 2.
- **Debian 12 and 13 (Trixie), Ubuntu 24.04+**: `ssf_<version>-1_amd64.deb`, `sudo apt update`, then `sudo apt
  install ./ssf_*_amd64.deb`; `gh`, `git` and `jq` come from the
  repositories (Debian 12 needs GitHub's apt repository for a new enough
  `gh`), and herdr is installed by hand ([Setup, step
  1](docs/setup.md#1-before-you-start)).
Separate static Linux x86_64 and ARM64 client/server binaries are also release
assets; see [standalone installation](docs/install-binaries.md) for client-only
SSH use and installation without a package. ARM64 assets are best effort.

The Linux packages install the same paths on every distribution:

| Path | What |
|------|------|
| `/usr/bin/ssf` | management client; locally invokes `ssf-server`, or reaches one over SSH with `--server` |
| `/usr/bin/ssf-server` | daemon and the server-side command endpoint |
| `/usr/bin/ssf-ui` | the bar widget's and menu's helper: service toggle, log, status terminal, open a workspace |
| `/usr/lib/systemd/user/ssf.service` | legacy singleton service retained for unmigrated installations |
| `/usr/lib/systemd/user/ssf@.service` | one target-qualified background service instance per named local or VM server |
| `/usr/share/ssf/SSF.example.md` | a starting point for your repository's SSF-agent operating contract in `SSF.md` ([Writing SSF.md](docs/ssf-md.md) explains it) |
| `/usr/share/ssf/config.example.toml` | every configuration key, with a comment |
| `/usr/share/ssf/vm/` | the scripts and units that build the microVM image |
| `/usr/share/doc/ssf/` | this file and `docs/`, [Setup](docs/setup.md) among them |

The package installs files without changing any user's configuration or
service. After `ssf setup`, the service belongs to `default.target`; linger
keeps the user manager running across logout and starts it during boot. It
comes back after a reboot (unless it
was switched off with the toggle or `ssf ui service disable`), waits for
the driver, and resumes the
agent sessions the reboot cut off. Building from a checkout, and running
the service from a dev build, is in [Development](docs/development.md).

Removing the shell plugin deliberately leaves the daemon running. To remove
the application, run `ssf uninstall` (add `--data` only if you also want to
remove configuration and state), then `sudo pacman -R ssf`. Project clones and
worktrees are preserved. The widget can be removed independently with
`omarchy plugin remove ssf.factory`.

## Set up

[Setup](docs/setup.md) is the document, top to bottom: prerequisites,
the bot account, the sign-in, who may drive the factory, where the
agents run, the harness login, the first repository and the harness and
model it runs on, the first issue, upgrading, stopping and
uninstalling. On Omarchy, start with the package install and `ssf setup` above;
the short form after that explicit setup is:

1. **The bot account.** The bot is a GitHub account of its own, created
   for the factory rather than yours (every agent post is made as it,
   and a post by the bot *without* a byline reads as a person's), with
   Write access on each repository it works and to its project boards.
2. **The VM** (the default; the agents never see your home
   directory): `ssf setup` creates the sole `ssf-server` target, then
   `ssf vm build` builds it. Then `ssf auth login --web` runs GitHub's device flow inside
   the guest: approve the printed code in a browser signed in as the bot.
   Its token and signing key stay on the guest data disk. Run
   `ssf vm login <harness>` to sign your coding agent in there too.
   The alternative is host mode in herdr, with bot and harness login on the host.
3. **A repository**: `ssf repo add owner/name --harness claude --model
   fable --effort medium` (the *harness* is the agent program: `claude`,
   `codex`, `gemini`, ...; `ssf agents` lists them, and the model and
   effort require an explicit choice when the harness supports them), and
   an `SSF.md` at its root telling SSF-spawned
   agents how to own and communicate work, manage the board, review and
   complete it, starting from [`SSF.example.md`](SSF.example.md) and
   [Writing SSF.md](docs/ssf-md.md).
   Machine-wide context can optionally go in `~/.ssf/SSF.md`, with
   harness-specific additions in `~/.ssf/SSF.<harness>.md`.

`ssf doctor` after each step says what is still missing; the document
shows what a healthy one looks like. Then assign an issue or pull request
to the bot on GitHub, or @mention it. Within a poll interval (10 s by default) a
workspace shows up in the driver and in `ssf status`, and the bar widget
shows the state of the factory: one row per agent session with the issue
or PR, its GitHub state, the agent's state, what it last said, the
branch and when it was last active.

A good first issue is small and self-contained, says what "done" looks
like (a test that passes, a file that changes, a command that works), and
names what to run before opening a pull request. Assign it to the bot and
watch the issue: within a couple of minutes the agent comments with what
it is about to do, and later with the pull request, after the gauntlet
its `SSF.md` asks for. Read it, answer or merge as you would for a
colleague, and close the issue when it is done; the agent then pushes
what is left, comments once more and gives its workspace back.

The same commands, with their variants:

```sh
ssf vm build                     # after conventional `ssf setup` (skip in host mode)
ssf vm start                     # or start the host service to supervise it
ssf auth login                    # guest device flow; in host mode, pick a gh account or sign in
ssf auth login --web              # straight to the browser flow; prints the URL and code,
                                  # so it also works over ssh (set BROWSER=true to stop gh opening one)
ssf auth login --user acme-bot    # guest device flow checks this login; host mode selects a gh account
printf '%s' "$TOKEN" | ssf auth login --token   # a pasted token instead of gh
ssf auth status
ssf agents                        # available harnesses and which are installed
ssf repo add acme/widgets --harness claude --model opus --effort high
ssf candidates                    # existing allocations waiting for explicit adoption; starts nothing
ssf adopt acme/widgets#12          # start one here with its complete GitHub history
ssf vm login claude               # sign the harness in inside the guest
ssf status
ssf peers                         # the agent sessions and what each is doing
ssf doctor                        # explicit model/effort, token and scopes, drivers, harness logins, gh and git wrappers, daemon socket, worktrees holding work with no agent on them
```

If the commits should carry your own name rather than the bot's, a
`[git]` table in the config says so while `gh` stays the bot (see
[Committing as a person](docs/identity-and-bylines.md#committing-as-a-person-while-gh-stays-the-bot)).
The SSF agent guidance is described in [The SSF agent guidance
file](docs/configuration.md#the-ssf-agent-guidance-file); this
repository's own [`SSF.md`](SSF.md) is what produced the comments quoted
above.

## Everyday commands

Everything except the credentials is scriptable, so an agent on the machine
can reconfigure the factory:

```sh
ssf repo list --json
ssf repo add acme/widgets --harness codex --model gpt-5.5 --effort high --instructions "Run make test before opening a PR."
ssf candidates [--repo acme/widgets] [--json]
ssf adopt acme/widgets#12 [acme/widgets#15 ...] [--json]
ssf repo set acme/widgets --model gpt-5.5 --effort high    # ids: ssf models <agent>; choosing: docs/setup.md
ssf repo set acme/widgets --harness pi --model openrouter/anthropic/claude-sonnet-4 --effort high
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
ssf handover --harness codex --model gpt-5.5 --summary "..."   # inside a session: hand the item to a new session on another harness, model or effort
ssf handover 12 --harness pi --no-summary          # or from your shell (owner/name#12, or --as)
ssf handover 12 --cancel                           # drop a handover the daemon has not carried out yet
ssf assign 12 --harness pi --model openrouter/anthropic/claude-sonnet-4 --effort high   # open the issue with gh, then start its first session on that stack
ssf release [12 | --as acme/widgets#12] [--force]   # remove a session's workspace once its work is on origin
ssf purge [--dry-run] [--older-than DAYS] [--force] # remove the clean workspaces of closed items; list the rest, a checkout whose workspace was closed by hand included
ssf guide                         # the reference for agents (the initial prompt points at it)
ssf ui service disable|enable|toggle|status
ssf uninstall [--yes] [--force] [--data]   # clean up the service/account safely; then remove its package
journalctl --user -fu ssf@ssf-server.service
```

Every command can target another machine that has SSF installed:

```sh
ssf --server factory.example status
SSF_SERVER=factory.example ssf peers
```

The destination is any SSH destination accepted by `ssh` (including a host
alias with its user and key in `~/.ssh/config`). The client invokes the same
`ssf-server` command endpoint that it executes locally, so local
and remote commands share parsing, validation, and the daemon's local Unix
socket path. The remote account therefore needs permission to operate that
factory; SSF exposes no network listener of its own.

The client can instead give factories stable names in
`~/.config/ssf/servers.toml`; see [Server catalog](docs/configuration.md#server-catalog).
With one entry it is selected automatically. With several, an unqualified
factory command refuses and lists the names, and `--server` / `SSF_SERVER`
select a name rather than a raw SSH destination. `ssf server list` and
`ssf server show NAME` inspect the client-owned catalog. Namespaced local
targets can coexist with the existing local or VM factory. `ssf server
migrate-vm` adopts an existing VM in place as `ssf-server`, moving only its
host-side settings into the catalog after verification. Multiple owned VMs can
then be operated with `ssf --server NAME vm ...` when their runtime names,
directories and ports are distinct. Background service controls are
target-qualified too: `ssf --server NAME ui service enable` supervises only
that local or VM target. Stop the legacy singleton before enabling the first
target service. Target-aware uninstall remains tracked in
[#261](https://github.com/mikekelly/simple-software-factory/issues/261).

Add advanced targets through the catalog CLI:

```sh
ssf server add local --local
ssf server add crucible --vm
ssf server add cloud --ssh ssf@factory.example.com
ssf server list
```

The first remains implicit. Adding the second deliberately makes unqualified
target commands refuse until `--server NAME` is supplied; there is no stored
default. Disable a target's service before `ssf server remove NAME`; removal
forgets only the route and retains local data and VM resources.

Factory config changes are picked up on the next poll; no restart needed.
In VM mode, repository, factory config and bot auth commands operate in the
guest; they fail if it is stopped or unreachable. Start it and retry.
Only `ssf vm ...` and `ssf config get|set vm.<key>` manage host VM settings,
whether they are still in legacy `[vm]` or owned by a migrated target.
Use `ssf vm status` for host VM health, and `ssf status` / `ssf doctor`
for guest factory health. There is no routine config sync. Every
key, with its default, is in [Configuration](docs/configuration.md).

Things to know when operating it:

- ssf never removes a workspace on its own. Closing an item tells the agent
  to push, comment and run `ssf release`; `ssf purge` is your sweep for what
  was left. Both refuse when anything is not on origin unless `--force`.
- A workspace closed by hand (a herdr tab) leaves its git
  checkout behind. `ssf doctor` warns, per repository, about every such
  checkout holding commits that are on no other branch and not on origin, or
  uncommitted changes, with no agent on it. Commenting on an active item
  starts its session again in that checkout; a retired item's branch is
  pushed by hand. Removing the directory loses the uncommitted changes and
  leaves the commits on a local branch nothing lists (see [Workspaces after
  close](docs/sessions.md#workspaces-after-close-release-and-purge)).
- Nothing reaches an agent off the record: every message it gets is activity
  on an item it works on or follows, or the daemon's own notice about that
  item. Decisions go on the item as comments.
- A daemon restart is invisible to agents; a reboot triggers the startup
  pass that relaunches interrupted sessions.
- ssf starts no second session on a pull request the bot opened: the
  agent that wrote it runs the gauntlet its `SSF.md` asks for (a fresh
  agent breaks the change, the author fixes and repeats), and its
  autonomy line says who merges (a person, as shipped). A `review` label
  does nothing; `ssf doctor` reports a
  repository without an `SSF.md`.

**Stopping it.** The toggle in the bar widget (Omarchy), or `ssf ui service
disable`, stops the service and keeps it from starting at the next login
(`enable` turns it back on). Add `--server NAME` when several targets exist.
Running agents
are left where they are: nothing reaches them while the daemon is down,
and it delivers what they missed when it comes back. With the factory in
a VM, stopping the service shuts the guest down cleanly.

One state directory has one engine owner. `ssf-server --once` refuses while its
daemon is active, including when the command is forwarded to an active VM;
in that case the guest's `ssf.service` owns the guest state, so let its next
poll do the work.

**Upgrading and uninstalling** are in [Setup](docs/setup.md#11-upgrading):
Upgrade ssf with its package manager. An active, already configured service is
restarted on package upgrade; installing the widget never upgrades it. For
complete removal run `ssf uninstall`, then remove the package with pacman or
apt. Remove the Omarchy widget separately if it is installed.

## The rest of the story

The reference, one file per area. Each starts with a line saying what it
covers and who needs it; they are installed under `/usr/share/doc/ssf/docs/`.

| Read | When you want to know |
|------|-----------------------|
| [Setup](docs/setup.md) | from a fresh machine to the first issue: prerequisites, the package, the bot account, the microVM or the host, the first repository and the harness and model it runs on, upgrading, uninstalling |
| [Configuration](docs/configuration.md) | every key in `config.toml`; the `SSF.md` agent-guidance file; models and effort levels; the permission-free command each agent is started with; who may drive the factory |
| [Workspaces and terminals](docs/drivers.md) | how SSF uses herdr for workspaces, terminals and agent state |
| [Inside a VM](docs/vm.md) | running the whole factory in a Firecracker microVM on Linux; also documents the work-in-progress lima backend for macOS |
| [What the agent is told](docs/prompts.md) | the first prompt, the messages an agent receives, project boards, and the boundary between `SSF.md` and `AGENTS.md` |
| [Identity and bylines](docs/identity-and-bylines.md) | how `gh` and `git` act as the bot inside a session, and how the byline and origin tag say which session posted |
| [Sessions](docs/sessions.md) | which session owns an item, second opinions, following and messaging other sessions, assigning a stack before an item's first session, handovers, release and purge |
| [Under the hood](docs/internals.md) | polling, delivery, resume and restarts; the `ssf status --json` fields; known limits |
| [Development](docs/development.md) | building, scratch runs, a dev build as the service, the source layout |

Agents get their own reference from `ssf guide`, printed by the running
binary so it cannot drift from the daemon.

## License

MIT; see [LICENSE](LICENSE).
