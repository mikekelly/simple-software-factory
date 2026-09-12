# Simple Software Factory (ssf)

Assign a GitHub issue to a bot account and a coding agent picks it up on your
machine: it gets a workspace of its own, works on the issue, opens a pull
request, answers the review, and reports back on the issue. Every issue gets
its own agent. The agents know about each other, about the project board, and
about the notes your repository keeps for them.

ssf is a small daemon for Linux, packaged for [Omarchy](https://omarchy.org/),
Arch, Debian/Ubuntu and Fedora. It runs the
agents in [herdr](https://herdr.dev/) by default, or in
[Orca](https://onorca.dev/), so you can watch them work, take over, or
nudge them at any time (see [Drivers](docs/drivers.md)). Nothing
runs in the cloud: the daemon polls GitHub and drives the multiplexer, and
the agents are the ones you already have installed (Claude Code, Codex,
...). If you would rather keep the agents off your machine altogether, the
whole factory can run [inside a VM](docs/vm.md).

## One issue, start to finish

This repository is built by ssf, so its own history shows what you get.
Here is issue [#18](https://github.com/mikekelly/simple-software-factory/issues/18)
on 2026-09-04, from assignment to merge, as it appears on GitHub. The bot
account is @OverlayBot; every comment below is from that account, and the
timestamps are UTC.

**20:24 — the issue is assigned to the bot** (for the second time; the
first assignment at 20:21 was undone and redone). Within a minute a workspace
named after the issue appears in the driver (Orca on that day; herdr by
default now) and an agent starts in it. Two minutes after the assignment
it posts:

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
  assigned to the bot, and is expected to put its own work through a
  gauntlet (a fresh agent it arranges itself) before calling it done: one
  session per item, the rule for the second pair of eyes in `SSF.md`.
- **Everything is on GitHub.** Agents talk to people, and to each other,
  through issue and pull request comments. Every post carries the byline of
  the session that made it. The one exception is `ssf tell`, a message
  typed straight into an agent's terminal, kept for nudges that would be
  noise on the item.
- **Nothing runs in the cloud.** The daemon polls GitHub, creates workspaces
  in Orca (or herdr), and starts the agents you have installed, with the
  bot's credentials, so what the agents do on GitHub is done as the bot.
  On your own machine that is a default rather than a wall (the agents run
  as you); the [VM](docs/vm.md) is the wall.

## How it works

The optional [herdr session dashboard](herdr-plugin/README.md) shows active
agents as a browser card grid, with linked originating and additional issues,
last activity, and the latest message or status summary. It also works on a
VM host through SSF's normal command forwarding.

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

Until ssf is published in Omarchy's package repository, download the current
Arch package and install it with pacman. This is a normal package-manager
install: pacman resolves `github-cli` and `herdr` from Omarchy's configured
repositories and records every installed file.

```sh
sudo pacman -U ./ssf-<version>-1-x86_64.pkg.tar.zst
ssf setup
```

`ssf setup` is the explicit per-user step. It creates the initial configuration,
enables and starts `ssf.service` for `default.target`, and asks visibly for
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

Arch, Debian/Ubuntu and Fedora (x86_64) remain available as packages, and
macOS through Homebrew; every release carries the Linux packages.

- **Omarchy**: use a release newer than `v0.1.0` and
  install `ssf-<version>-1-x86_64.pkg.tar.zst` with `sudo pacman -U
  ssf-*.pkg.tar.zst`; `github-cli` and `herdr` come from Omarchy's repositories.
- **Arch**: the same `.pkg.tar.zst`, after `github-cli` (`extra`) and
  `herdr` or `herdr-bin` (AUR), which it depends on. Without a Wayland
  session (X11, a server), see [Setup](docs/setup.md) step 2.
- **Debian 12+, Ubuntu 24.04+**: `ssf_<version>-1_amd64.deb`, `sudo apt
  install ./ssf_*_amd64.deb`; `gh`, `git` and `jq` come from the
  repositories (Debian 12 needs GitHub's apt repository for a new enough
  `gh`), and herdr is installed by hand ([Setup, step
  1](docs/setup.md#1-before-you-start)).
- **Fedora**: `ssf-<version>-1.x86_64.rpm`, `sudo dnf install
  ./ssf-*.x86_64.rpm`; herdr by hand, as above.
- **macOS**: `brew install mikekelly/tap/ssf`; the formula pulls in `gh`
  and `lima`. The factory runs inside a [lima](https://lima-vm.io) VM
  (`ssf vm build`, then `brew services start ssf`); the formula installs
  `ssf`, the VM scripts under `$(brew --prefix)/share/ssf/vm` and this
  documentation under `$(brew --prefix)/share/doc/ssf`.

The Linux packages install the same paths on every distribution:

| Path | What |
|------|------|
| `/usr/bin/ssf` | the daemon and management CLI |
| `/usr/bin/ssf-ui` | the bar widget's and menu's helper: service toggle, log, status terminal, open a workspace |
| `/usr/lib/systemd/user/ssf.service` | background user service, enabled for `default.target` by explicit `ssf setup` |
| `/usr/share/ssf/SSF.example.md` | a starting point for your repository's `SSF.md` |
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
   directory): `ssf vm build`, `ssf config set vm.enabled true`,
   `systemctl --user restart ssf.service` (macOS: `brew services start
   ssf`). Then `ssf auth login --web` runs GitHub's device flow inside
   the guest: approve the printed code in a browser signed in as the bot.
   Its token and signing key stay on the guest data disk. Run
   `ssf vm login <harness>` to sign your coding agent in there too.
   The alternative is host mode, in herdr or Orca
   (`ssf config set driver orca`), with bot and harness login on the host.
3. **A repository**: `ssf repo add owner/name --harness claude --model
   fable --effort medium` (the *harness* is the agent program: `claude`,
   `codex`, `gemini`, ...; `ssf agents` lists them, and the model and
   effort are worth choosing rather than leaving to the harness — not
   every harness has both), and an `SSF.md` at its root telling agents
   how you want work done, starting from
   [`SSF.example.md`](SSF.example.md).

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
ssf vm build && ssf config set vm.enabled true   # VM setup first (skip in host mode)
ssf vm start                     # or start the host service to supervise it
ssf auth login                    # guest device flow; in host mode, pick a gh account or sign in
ssf auth login --web              # straight to the browser flow; prints the URL and code,
                                  # so it also works over ssh (set BROWSER=true to stop gh opening one)
ssf auth login --user acme-bot    # guest device flow checks this login; host mode selects a gh account
printf '%s' "$TOKEN" | ssf auth login --token   # a pasted token instead of gh
ssf auth status
ssf agents                        # which agents Omarchy knows and which are installed
ssf repo add acme/widgets --harness claude
ssf vm login claude               # sign the harness in inside the guest
ssf config set driver orca        # host mode only: sessions in Orca instead of herdr
ssf status
ssf peers                         # the agent sessions and what each is doing
ssf doctor                        # token and scopes, drivers, harness logins, gh wrapper, daemon socket, worktrees holding work with no agent on them
```

If the commits should carry your own name rather than the bot's, a
`[git]` table in the config says so while `gh` stays the bot (see
[Committing as a person](docs/identity-and-bylines.md#committing-as-a-person-while-gh-stays-the-bot)).
The per-project notes are described in [The per-project prompt
file](docs/configuration.md#the-per-project-prompt-file); this
repository's own [`SSF.md`](SSF.md) is what produced the comments quoted
above.

## Everyday commands

Everything except the credentials is scriptable, so an agent on the machine
can reconfigure the factory:

```sh
ssf repo list --json
ssf repo add acme/widgets --harness codex --instructions "Run make test before opening a PR."
ssf repo set acme/widgets --model opus --effort high    # ids: ssf models <agent>; choosing: docs/setup.md
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
ssf handover --harness codex --model gpt-5.5 --summary "..."   # inside a session: hand the item to a new session on another harness, model or effort
ssf handover 12 --harness pi --no-summary          # or from your shell, like tell (owner/name#12, or --as)
ssf handover 12 --cancel                           # drop a handover the daemon has not carried out yet
ssf release [12 | --as acme/widgets#12] [--force]   # remove a session's workspace once its work is on origin
ssf purge [--dry-run] [--older-than DAYS] [--force] # remove the clean workspaces of closed items; list the rest, a checkout whose workspace was closed by hand included
ssf guide                         # the reference for agents (the initial prompt points at it)
ssf ui service disable|enable|toggle|status
ssf uninstall [--yes] [--force] [--data]   # clean up the service/account safely; then remove its package
journalctl --user -fu ssf.service   # macOS: tail -f $(brew --prefix)/var/log/ssf.log
```

Factory config changes are picked up on the next poll; no restart needed.
In VM mode, repository, factory config and bot auth commands operate in the
guest; they fail if it is stopped or unreachable. Start it and retry.
Only `ssf vm ...` and `ssf config get|set vm.<key>` manage host VM settings.
Use `ssf vm status` for host VM health, and `ssf status` / `ssf doctor`
for guest factory health. There is no routine config sync. Every
key, with its default, is in [Configuration](docs/configuration.md).

Things to know when operating it:

- ssf never removes a workspace on its own. Closing an item tells the agent
  to push, comment and run `ssf release`; `ssf purge` is your sweep for what
  was left. Both refuse when anything is not on origin unless `--force`.
- A workspace closed by hand (a herdr tab, an Orca worktree) leaves its git
  checkout behind. `ssf doctor` warns, per repository, about every such
  checkout holding commits that are on no other branch and not on origin, or
  uncommitted changes, with no agent on it. `ssf tell` to an active item
  brings the session back in that checkout; a retired item's branch is
  pushed by hand. Removing the directory loses the uncommitted changes and
  leaves the commits on a local branch nothing lists (see [Workspaces after
  close](docs/sessions.md#workspaces-after-close-release-and-purge)).
- `tell` is not mirrored to GitHub; decisions go on the item as comments,
  which the agent receives like any other activity.
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
(`enable` turns it back on); `systemctl --user stop ssf.service` (macOS:
`brew services stop ssf`) stops it until the next login. Running agents
are left where they are: nothing reaches them while the daemon is down,
and it delivers what they missed when it comes back. With the factory in
a VM, stopping the service shuts the guest down cleanly.

One state directory has one engine owner. `ssf run --once` refuses while its
daemon is active, including when the command is forwarded to an active VM;
in that case the guest's `ssf.service` owns the guest state, so let its next
poll do the work.

**Upgrading and uninstalling** are in [Setup](docs/setup.md#11-upgrading):
Upgrade ssf with its package manager. An active, already configured service is
restarted on package upgrade; installing the widget never upgrades it. For
complete removal run `ssf uninstall`, then remove the package with pacman, apt,
dnf or brew. Remove the Omarchy widget separately if it is installed.

## The rest of the story

The reference, one file per area. Each starts with a line saying what it
covers and who needs it; they are installed under `/usr/share/doc/ssf/docs/`
(`$(brew --prefix)/share/doc/ssf/docs/` on macOS).

| Read | When you want to know |
|------|-----------------------|
| [Setup](docs/setup.md) | from a fresh machine to the first issue: prerequisites, the package, the bot account, the microVM or the host, the first repository and the harness and model it runs on, upgrading, uninstalling |
| [Configuration](docs/configuration.md) | every key in `config.toml`; the `SSF.md` prompt file; models and effort levels; the permission-free command each agent is started with; who may drive the factory |
| [Drivers](docs/drivers.md) | Orca versus herdr, and what each one does with workspaces and terminals |
| [Inside a VM](docs/vm.md) | running the whole factory in a VM, Firecracker on Linux or lima on macOS: the backends, the image, what gets in, reaching it, what persists |
| [What the agent is told](docs/prompts.md) | the first prompt, the messages an agent receives, project boards, and what is left to `SSF.md` |
| [Identity and bylines](docs/identity-and-bylines.md) | how `gh` and `git` act as the bot inside a session, and how the byline and origin tag say which session posted |
| [Sessions](docs/sessions.md) | which session owns an item, second opinions, following and messaging other sessions, release and purge |
| [Under the hood](docs/internals.md) | polling, delivery, resume and restarts; the `ssf status --json` fields; known limits |
| [Development](docs/development.md) | building, scratch runs, a dev build as the service, the source layout |

Agents get their own reference from `ssf guide`, printed by the running
binary so it cannot drift from the daemon.

## License

MIT; see [LICENSE](LICENSE).
