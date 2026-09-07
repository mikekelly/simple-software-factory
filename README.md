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

ssf is an Arch package for Omarchy. Until it is in Omarchy's package
repository, the package comes from the latest release: download
`ssf-<version>-1-x86_64.pkg.tar.zst` from the Releases page and install
it, then follow [Setup](docs/setup.md), yourself or with your coding
agent (the `ssf-setup` skill in this repository points an agent at that
document).

```sh
sudo pacman -U ssf-*.pkg.tar.zst
```

The package installs:

| Path | What |
|------|------|
| `/usr/bin/ssf` | the daemon and management CLI |
| `/usr/bin/ssf-ui` | the bar widget's and menu's helper: service toggle, log, status terminal, open a workspace |
| `/usr/lib/systemd/user/ssf.service` | background service, enabled for every user via `graphical-session.target.wants` |
| `/usr/share/ssf/omarchy-plugin/` | the bar widget, copied into `~/.config/omarchy/plugins/ssf.factory` on first start; it and the **Factory** menu show the state of the factory, and the service toggle is their one control |
| `/usr/share/ssf/SSF.example.md` | a starting point for your repository's `SSF.md` |
| `/usr/share/ssf/config.example.toml` | every configuration key, with a comment |
| `/usr/share/ssf/vm/` | the scripts and units that build the microVM image |
| `/usr/share/doc/ssf/` | this file and `docs/`, [Setup](docs/setup.md) among them |

`github-cli` and `herdr` are dependencies and come with it. The service
starts with the graphical session, and the package's install hook also
starts it in any session that is running at install time, so there is
nothing to enable; it stays in a restart loop until the bot is signed
in. It comes back with the next login after a reboot (unless it was
switched off with the toggle), waits for the driver, and resumes the
agent sessions the reboot cut off. Building from a checkout, and running
the service from a dev build, is in [Development](docs/development.md).

## Set up

[Setup](docs/setup.md) is the document, top to bottom: prerequisites,
the bot account, the sign-in, who may drive the factory, where the
agents run, the harness login, the first repository, the first issue,
upgrading, stopping and uninstalling. The short form of the default path:

1. **The bot account.** The bot is a GitHub account of its own, created
   for the factory rather than yours (every agent post is made as it,
   and a post by the bot *without* a byline reads as a person's), with
   Write access on each repository it works and to its project boards.
   `ssf auth login --web` signs it in through gh's device flow; ssf never
   stores the token, enrolls a dedicated key on the bot account for
   pushes and commit signing, and switches gh back to your own account
   afterwards.
2. **The microVM** (the default; the agents never see your home
   directory): `ssf vm build`, `ssf config set vm.enabled true`,
   `systemctl --user restart ssf.service`, then `ssf vm login <harness>`
   to sign your coding agent in inside the guest. The alternative is the
   agents on this machine, in herdr (the default driver) or in Orca
   (`ssf config set driver orca`), with the harness signed in here.
3. **A repository**: `ssf repo add owner/name --harness claude` (the
   *harness* is the agent program: `claude`, `codex`, `gemini`, ...;
   `ssf agents` lists them), and an `SSF.md` at its root telling agents
   how you want work done, starting from [`SSF.example.md`](SSF.example.md).

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
ssf auth login                    # pick an account gh knows, or sign in another in the browser
ssf auth login --web              # straight to the browser flow; prints the URL and code,
                                  # so it also works over ssh (set BROWSER=true to stop gh opening one)
ssf auth login --user acme-bot    # an account gh already knows, no questions
printf '%s' "$TOKEN" | ssf auth login --token   # a pasted token instead of gh
ssf auth status
ssf agents                        # which agents Omarchy knows and which are installed
ssf repo add acme/widgets --harness claude
ssf vm build && ssf config set vm.enabled true   # the factory in the microVM
ssf vm login claude               # sign the harness in inside the guest
ssf config set driver orca        # on the host: sessions in Orca instead of herdr (the default)
ssf status
ssf peers                         # the agent sessions and what each is doing
ssf doctor                        # token and scopes, drivers, harness logins, gh wrapper, daemon socket
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
- ssf starts no second session on a pull request the bot opened: the
  agent that wrote it runs the gauntlet its `SSF.md` asks for (a fresh
  agent breaks the change, the author fixes and repeats), and its
  autonomy line says who merges (a person, as shipped). A `review` label
  does nothing; `ssf doctor` reports a
  repository without an `SSF.md`.

**Stopping it.** The toggle in the bar widget, or `ssf ui service
disable`, stops the service and keeps it from starting at the next login
(`enable` turns it back on); `systemctl --user stop ssf.service` stops it
until the next login. Running agents are left where they are: nothing
reaches them while the daemon is down, and it delivers what they missed
when it comes back. With the factory in a microVM, stopping the service
shuts the guest down cleanly.

**Upgrading and uninstalling** are in [Setup](docs/setup.md#11-upgrading):
the package upgrade restarts the service (and, in the VM, the guest, whose
sessions are resumed), and uninstalling is a short ordered list ending in
`sudo pacman -R ssf`.

## The rest of the story

The reference, one file per area. Each starts with a line saying what it
covers and who needs it; they are installed under `/usr/share/doc/ssf/docs/`.

| Read | When you want to know |
|------|-----------------------|
| [Setup](docs/setup.md) | from a fresh machine to the first issue: prerequisites, the package, the bot account, the microVM or the host, the first repository, upgrading, uninstalling |
| [Configuration](docs/configuration.md) | every key in `config.toml`; the `SSF.md` prompt file; models and effort levels; the permission-free command each agent is started with; who may drive the factory |
| [Drivers](docs/drivers.md) | Orca versus herdr, and what each one does with workspaces and terminals |
| [Inside a microVM](docs/vm.md) | running the whole factory in a Firecracker VM: the image, what gets in, reaching it, what persists |
| [What the agent is told](docs/prompts.md) | the first prompt, the messages an agent receives, project boards, and what is left to `SSF.md` |
| [Identity and bylines](docs/identity-and-bylines.md) | how `gh` and `git` act as the bot inside a session, and how the byline and origin tag say which session posted |
| [Sessions](docs/sessions.md) | which session owns an item, second opinions, following and messaging other sessions, release and purge |
| [Under the hood](docs/internals.md) | polling, delivery, resume and restarts; the `ssf status --json` fields; known limits |
| [Development](docs/development.md) | building, scratch runs, a dev build as the service, the source layout |

Agents get their own reference from `ssf guide`, printed by the running
binary so it cannot drift from the daemon.

## License

MIT; see [LICENSE](LICENSE).
