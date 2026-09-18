# Simple Software Factory (ssf)

Run a team of coding agents from your GitHub issues.

Write down what you want as an issue, assign it to your bot, and get on
with something else. An agent picks it up, asks you what it needs to know
on the issue, plans the work, breaks it up, and drives it through to merged
pull requests. Several agents can work at once, on the same project, talking
to each other on GitHub as colleagues would. You are pulled in only for the
things that need a person: what to build, the calls that matter, the merge.

ssf runs your factory on hardware you control: in a microVM on your own
machine, or in the cloud (for example on your Grok Bot's computer). It uses
the coding agents you already have, as a bot account you own, and leaves the
whole record, the discussion, the plan, the decisions and the pull requests,
on GitHub where your work already lives. There is no dashboard to log in to
and no transcript in a vendor's product; the issue tracker you have is the
interface. Linux today; macOS is next.

## Why

Coding agents are good at working an issue. Managing them is the problem:
one chat per agent, each waiting on you, with the outcome copied by hand
into the place the work is actually tracked. Tools that fix that put the
agents in their cloud and the conversation in their product.

ssf starts from the other end. GitHub is already where work is described,
discussed, reviewed and merged, so that is where the agents live: the issue
is the unit of work, the assignment is the trigger, the comment thread is
the conversation, the pull request is the deliverable and the board is the
status. The factory is small enough to read: a daemon that polls GitHub, a
terminal per item, and one file in your repository saying how its sessions
should behave.

## How a feature gets built

You open an issue with an idea and assign it to the bot. A session takes it
on and works out, with you on the issue, what the objective is and what
"done" means; it writes up a plan and asks for sign-off.

With the plan agreed it breaks the work into subtasks on the project board
and opens a coordinating issue that kicks them off. Each subtask gets a
session of its own; sessions that depend on each other talk by commenting
on each other's issues. When a decision is a person's to make, a session
@mentions you and waits.

Each session opens a pull request, puts it through the review its `SSF.md`
asks for and reports on its issue with the link. The cards make their way
across the board, the pull requests merge and the feature is delivered.
GitHub holds the whole story: the discussion, the plan, the linked issues,
the pull requests and every decision. This repository is built that way; its
issues and pull requests are the worked example.

## Install

Choose the installation for the machine:

- **Omarchy, Arch, Debian or Ubuntu with KVM and a systemd user session:**
  the package below, then [Setup](docs/setup.md).
- **A VPS, container or Linux without KVM or a user session:** standalone
  binaries in host mode, from [VPS / headless host](docs/headless-host.md).
- **Only controlling an existing factory over SSH:** the
  [standalone client](docs/install-binaries.md#client-only-operate-an-existing-factory-over-ssh).

Download the package from
[GitHub Releases](https://github.com/mikekelly/simple-software-factory/releases)
and install it with the package manager, which resolves `gh` and, on
Omarchy, `herdr` (on Arch install `herdr` from the AUR first; on Debian and
Ubuntu install it by hand for host mode, and Debian 12 needs GitHub's apt
repository for a recent `gh`):

```sh
sudo pacman -U ./ssf-<version>-1-x86_64.pkg.tar.zst    # Omarchy, Arch
sudo apt install ./ssf_<version>-1_amd64.deb            # Debian 12+, Ubuntu 24.04+
ssf setup
```

`ssf setup` is the per-user step: it creates the managed VM target
`ssf-server` and enables its user service, asking before it turns on
systemd linger so the service survives logout and starts at boot. On
Omarchy the optional bar widget shows the factory's state and toggles the
service; it never installs or upgrades ssf:

```sh
omarchy plugin add https://github.com/mikekelly/simple-software-factory.git --enable
```

## First run

[Setup](docs/setup.md) is the document, top to bottom. The short form:

1. **The bot account.** A GitHub account of its own, created for the
   factory rather than yours, with Write access on each repository it works
   and on their project boards. Every agent post is made as it.
2. **The VM.** `ssf vm build` builds the guest, then `ssf auth login --web`
   runs GitHub's device flow inside it: approve the printed code in a
   browser signed in as the bot. Then `ssf vm login <harness>` signs your
   coding agent in there too. Token, key and harness login stay on the
   guest's data disk. The alternative is host mode, with both logins on the
   host.
3. **A repository.** `ssf repo add owner/name --harness claude --model
   fable --effort medium` (`ssf agents` lists the harnesses; model and
   effort are an explicit choice), and an `SSF.md` at its root, starting
   from [`SSF.example.md`](SSF.example.md).

`ssf doctor` after each step says what is still missing. Then assign an
issue to the bot. A good first issue is small, says what "done" looks like
and names what to run before opening a pull request. Within a poll interval
a workspace shows up in `ssf status`; within a couple of minutes the agent
comments with what it is about to do, and later with the pull request. Read
it, answer or merge as you would for a colleague, and close the issue; the
agent pushes what is left, comments once more and gives its workspace back.

## How it works

Every few seconds ssf asks GitHub for the open issues and pull requests that
involve the bot. For a new one it creates a workspace in herdr, checked out
on a branch for the issue (or on the pull request's branch, so pushes update
the pull request), and starts the agent there with the whole story so far.
From then on every comment, review, label or push on the item is delivered
into that agent's terminal: it steers the agent if it is busy and wakes it if
it is idle. If a terminal is gone, or the whole workspace, ssf brings it back
and resumes the same conversation, including after a reboot. When the item is
closed the agent is told to push what is worth keeping and, only then, to
release its workspace ([Under the hood](docs/internals.md)).

ssf itself is a small daemon. It needs a GitHub account for the bot, `gh`,
[herdr](https://herdr.dev/) to run the workspaces and terminals, and a
coding agent you already have installed (Claude Code, Codex, ...). Packages
cover the Arch family (including [Omarchy](https://omarchy.org/)) and the
Debian family (including Ubuntu).

- **One agent per issue or pull request.** Each gets its own workspace (a
  git worktree on its own branch, in herdr) and its own session, from the
  moment the bot is assigned, @mentioned or asked to review until the item
  is closed. An agent is told the issue, everything that has happened on it,
  the boards it is on and the operating guidance in your repository's
  `SSF.md`: how it owns and communicates work, who to ask, what the columns
  mean, who merges ([Writing SSF.md](docs/ssf-md.md)). Build and test
  policy stays in `AGENTS.md`, as for any agent.
- **Everything is on GitHub.** Agents talk to people, and to each other,
  through issue and pull request comments. Every post carries the byline of
  the session that made it, and the item is the only channel between
  sessions: nothing reaches an agent off the record. Watching, nudging or
  rescuing a session is done at its terminal through herdr.
- **Nothing runs in the cloud.** The daemon polls GitHub, creates workspaces
  and starts the agents you have installed, with the bot's credentials, so
  what the agents do on GitHub is done as the bot. Only people you allow
  can drive it: by default the repository's collaborators with push access
  ([Who may drive the factory](docs/configuration.md#who-may-drive-the-factory)).

## Everyday commands

The shape of the client; `ssf --help` has the rest, and every command takes
`--server NAME` for another factory ([Server
catalog](docs/configuration.md#server-catalog)):

```sh
ssf status | ssf dashboard | ssf peers          # what is running and what each agent is doing
ssf repo add owner/name ... | ssf config set ... # configure; picked up on the next poll
ssf sub 12 | ssf assign 12 ... | ssf handover ... # follow an item, start one on a chosen stack, pass one on
ssf release | ssf purge                          # give back a workspace; sweep those of closed items
ssf doctor                                       # what is missing, and which checkouts still hold work
```

Everything except the credentials is scriptable, so an agent on the machine
can reconfigure the factory. Things to know when operating it:

- **ssf never removes a workspace on its own.** Closing an item tells the
  agent to push, comment and `ssf release`; `ssf purge` is your sweep for
  what was left. Both refuse when anything is not on origin unless
  `--force`. A tab closed by hand in herdr leaves its checkout behind, and
  `ssf doctor` names every one holding work with no agent on it
  ([Workspaces after close](docs/sessions.md#workspaces-after-close-release-and-purge)).
- **Nothing reaches an agent off the record.** Every message it gets is
  activity on an item it works on or follows. Decisions go on the item.
- **Restarts are invisible to agents.** A daemon restart delivers what was
  missed when it comes back; a reboot relaunches the interrupted sessions.
- **The bot reviews its own pull requests.** ssf starts no second session
  on a pull request the bot opened: the agent that wrote it runs the review
  its `SSF.md` asks for, and `SSF.md` says who merges (a person, as
  shipped). A `review` label does nothing.
- **Stopping.** The widget toggle or `ssf ui service disable` stops the
  service and keeps it from starting at login; running agents are left where
  they are. Upgrade with the package manager; remove with `ssf uninstall`,
  then the package ([Stopping and
  uninstalling](docs/setup.md#12-stopping-and-uninstalling)).

## The rest of the story

One file per area, installed under `/usr/share/doc/ssf/docs/`. Read them in
this order the first time.

| Read | When you want to know |
|------|-----------------------|
| [Setup](docs/setup.md) | from a fresh machine to the first issue: prerequisites, the package, the bot, the VM or the host, the first repository, upgrading, uninstalling |
| [VPS / headless host](docs/headless-host.md) | standalone binaries and host mode on a server or container |
| [Standalone binaries](docs/install-binaries.md) | the release binaries, ARM64, and the client-only install |
| [Inside a VM](docs/vm.md) | the Firecracker microVM on Linux, and the work-in-progress lima backend for macOS |
| [Configuration](docs/configuration.md) | every key in `config.toml`; models and effort; who may drive the factory; the server catalog |
| [Writing SSF.md](docs/ssf-md.md) | the operating guidance your repository gives its sessions, and what belongs in `AGENTS.md` instead |
| [What the agent is told](docs/prompts.md) | the first prompt, the messages an agent receives, project boards |
| [Identity and bylines](docs/identity-and-bylines.md) | how `gh` and `git` act as the bot inside a session, and which session posted what |
| [Sessions](docs/sessions.md) | who owns an item, second opinions, following and messaging other sessions, handovers, release and purge |
| [Session dashboard](docs/dashboard.md) | `ssf dashboard` across one or several factories, and the optional browser dashboard |
| [Workspaces and terminals](docs/drivers.md) | how ssf uses herdr for workspaces, terminals and agent state |
| [Use an assistant as the liaison](docs/liaison.md) | an always-on assistant that watches the factory and drives it on your behalf |
| [Agent operating guidance](docs/agent-guidance.md) | for an agent installing, operating or upgrading a factory for a person |
| [Guidance audit](docs/audit.md) | a bounded review of a project's `SSF.md` and `AGENTS.md` |
| [Under the hood](docs/internals.md) | polling, delivery, resume and restarts; `ssf status --json`; known limits |
| [Uninstall reference](docs/uninstall.md) | work-preservation checks, recovery cases and retained data |
| [Development](docs/development.md) | building, scratch runs, a dev build as the service, the source layout |

For agents: `ssf guide` is a session's collaboration reference and `ssf
skill` the operating topics, both printed by the running binary so they
cannot drift from it. `npx skills add mikekelly/simple-software-factory -g`
installs the `working-with-ssf` skill that points agents at them.

## License

MIT; see [LICENSE](LICENSE).
