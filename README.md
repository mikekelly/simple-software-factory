# Simple Software Factory

[![CI](https://github.com/mikekelly/simple-software-factory/actions/workflows/ci.yml/badge.svg)](https://github.com/mikekelly/simple-software-factory/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/mikekelly/simple-software-factory)](https://github.com/mikekelly/simple-software-factory/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Linux and macOS](https://img.shields.io/badge/platform-Linux%20%7C%20macOS-informational)](docs/install.md)

**Run a team of coding agents from your GitHub issues.** Assign an issue to
your bot and a dedicated agent picks it up in its own terminal and its own
worktree, plans the work with you on the issue, delivers it as pull
requests, and coordinates with the other agents by commenting on their
issues. GitHub is the interface; your terminal is the back door.

- **Multiplayer by default.** Your whole team, and every agent, in the same
  issue threads, with rich Markdown, @mentions and notifications.
- **Watchable and steerable.** Every agent is a real terminal session you
  can shell into, read, and take over like any coding session of your own.
- **Self-hosted.** A microVM on your machine or a server you rent; the
  agents you already pay for; nothing in anyone else's cloud.

## Why

Coding agents are good at working an issue. Managing several is the
problem: one chat window per agent, each waiting on you, and the outcome
copied by hand into the place the work is actually tracked. The tools that
promise to fix that put the agents in their cloud and the conversation in
their product, where nobody else on your team can see it.

Your team already has a place where work is described, discussed, reviewed
and merged, and it is already multiplayer: GitHub. ssf puts the agents
there. The issue is the unit of work, the assignment is the trigger, the
comment thread is the conversation, the pull request is the deliverable and
the board is the status. A person is pulled in only where a person is
needed: to say what to build, to make the calls the agents cannot, and to
merge. Everything else, plan, decisions, review and result, is on the
record where you would have looked anyway.

And because every agent is an ordinary terminal session in a git worktree,
the escape hatch is always open. Shell into the factory, open the agent's
terminal, read what it is doing, type to it, or finish the job yourself.

## Key concepts

| Concept | What it means in ssf |
|---|---|
| **GitHub is the GUI** | People and agents collaborate in issue and pull request comments: rich content, user tagging, notifications, reviews. No second interface to learn. |
| **The issue is the unit of work** | Every task is an issue. Assign it to the bot and it is being worked; close it and the work is wrapped up. Boards say where it stands. |
| **One long-lived agent per issue** | Each issue gets its own agent session in a terminal, from assignment until close, and every comment, review, label and push on the issue is delivered into it. |
| **Agents talk on GitHub** | Sessions that depend on each other comment on each other's issues. That is the only channel between them, so the record is complete. |
| **One worktree per agent** | Each session works in its own git worktree on its own branch, so parallel sessions on one repository never collide. |
| **Terminals you can enter** | Sessions run in [herdr](https://herdr.dev/), a terminal multiplexer. Attach to see what an agent is doing, steer it, or take over. |
| **Your repository sets the rules** | An `SSF.md` at the root tells sessions how to own work, communicate, review and hand off. Build and test policy stays in `AGENTS.md`. |

ssf runs on Linux and macOS, in a microVM on your own computer, on the
host itself, or on a server you rent, with `gh`, a bot GitHub account, herdr
and any of the coding agents you already have: Claude Code, Codex, Gemini,
Copilot, Grok, OpenCode, Pi, Oh My Pi or Crush.

## How a feature gets built

You open an issue with an idea and assign it to the bot. A session takes it
on and works out, with you on the issue, what the objective is and what
"done" means; it writes up a plan and asks for sign-off.

With the plan agreed, the work is split into subtasks on the project board
under a coordinating issue, and the session on that issue starts them, each
with a session of its own on the stack it chooses; sessions that depend on each other talk by commenting
on each other's issues. When a decision is a person's to make, a session
@mentions you and waits.

Each session opens a pull request, puts it through the review its `SSF.md`
asks for and reports on its issue with the link. The cards make their way
across the board, the pull requests merge and the feature is delivered.
GitHub holds the whole story: the discussion, the plan, the linked issues,
the pull requests and every decision. This repository is built that way:
[#361](https://github.com/mikekelly/simple-software-factory/issues/361) is
a board issue fanning out to parallel sessions with a decision escalated to
the owner, and
[#348](https://github.com/mikekelly/simple-software-factory/issues/348) is
one issue from plan through review rounds to merge and release.

## Install

ssf runs on Linux and macOS: on your own machine, with the agents in a
microVM (Firecracker on Linux, lima on macOS) or directly on the host, or on
a server you rent, operated from your machine over SSH. Packages for the
Arch, Debian and Fedora families and a Homebrew formula are on
[GitHub Releases](https://github.com/mikekelly/simple-software-factory/releases).

The install document is written for the coding agent you already have.
Point it at this repository and say "help me set up ssf":

- with `ssf` not yet installed, it reads [docs/install.md](docs/install.md),
  which opens with a check of your machine's resources and settles, with
  you, where the factory should run, then goes to the first issue;
- with `ssf` installed, `ssf skill` prints the same guidance from the
  installed version, routed by what you ask for, and
  `npx skills add mikekelly/simple-software-factory -g` installs the
  `working-with-ssf` skill that points agents at it.

You need a GitHub account of the bot's own, with Write access on each
repository it works, and a coding agent signed in where the agents run:
Claude Code, Codex, Gemini, Copilot, Grok, OpenCode, Pi, Oh My Pi or Crush.
Then assign an issue to the bot. A good first issue is small, says what
"done" looks like and names what to run before opening a pull request.
Within a couple of minutes the agent comments with what it is about to do,
and later with the pull request; read it, answer or merge as you would for
a colleague, and close the issue.

## How it works

Every ten seconds (by default) ssf asks GitHub for the open issues and pull
requests that involve the bot. For a new one it creates a workspace in herdr,
checked out on a branch for the issue (or on the pull request's branch, so
pushes update the pull request), and starts the agent there with the whole
story so far. From then on every comment, review, label or push on the item
is delivered into that agent's terminal: it steers the agent if it is busy
and wakes it if it is idle. If a terminal is gone, or the whole workspace,
ssf brings it back and resumes the same conversation, including after a
reboot. When the item is closed the agent is told to push what is worth
keeping and, only then, to release its workspace
([Under the hood](docs/internals.md)).

Only people you allow can drive it: by default the repository's
collaborators with push access, or a list you set
([Who may drive the factory](docs/configuration.md#who-may-drive-the-factory)).
Every post an agent makes is made as the bot, with a byline naming its
session, harness, model and effort
([Identity and bylines](docs/identity-and-bylines.md)).

Things to know when operating it:

- **ssf never removes a workspace on its own.** Closing an item tells the
  agent to push, comment and `ssf release`; `ssf purge` is your sweep for
  what was left. Both refuse when anything is not on origin.
- **Nothing reaches an agent off the record.** Every message it gets is
  activity on an item it works on or follows. Decisions go on the item.
- **A comment is never a command.** Everyone collaborates in the item's
  comments, agent and person alike. Directing the factory is a terminal
  command: `ssf handover` changes an item's stack, `ssf assign` starts the
  first session of an item that has none.
- **Restarts are invisible to agents.** A daemon restart delivers what was
  missed when it comes back; a reboot relaunches the interrupted sessions.
- **The bot reviews its own pull requests.** ssf starts no second session
  on a pull request the bot opened: the agent that wrote it runs the review
  its `SSF.md` asks for, and `SSF.md` says who merges.

## The rest of the story

For agents, `ssf skill` prints these from the installed binary; in the
repository they are one file per area under `docs/`, installed under
`/usr/share/doc/ssf/docs/` (or Homebrew's `share/doc/ssf/docs/`).

| Read | When you want to know |
|------|-----------------------|
| [Install](docs/install.md) | from a fresh machine to the first issue, on every supported path |
| [Repositories](docs/repositories.md) | adding a repository to a running factory, `SSF.md`, the first issue |
| [Operate](docs/operate.md) | targets, inspecting before changing, the service, upgrading, stopping |
| [Troubleshooting](docs/troubleshooting.md) | symptom, check and remedy |
| [Platform specifics](docs/platform-specifics.md) | your distro, macOS, rented hosts, Tailscale, harness notes, upgrading from an older ssf |
| [Configuration](docs/configuration.md) | every key in `config.toml`; who may drive the factory; the server catalog |
| [Harnesses](docs/harnesses.md) | models and effort, launch commands and permissions, compaction, delivery channels, sign-in |
| [Writing SSF.md](docs/ssf-md.md) | the operating guidance your repository gives its sessions, and what belongs in `AGENTS.md` instead |
| [Guidance audit](docs/audit.md) | a bounded review of a project's `SSF.md` and `AGENTS.md` |
| [Agent operating guidance](docs/agent-guidance.md) | rules for an agent installing, operating or upgrading a factory for a person |
| [Use an assistant as the liaison](docs/liaison.md) | an always-on assistant that watches the factory and drives it on your behalf |
| [Inside a VM](docs/vm.md) | the Firecracker microVM on Linux and the lima instance on macOS |
| [Sessions](docs/sessions.md) | who owns an item, second opinions, following and messaging other sessions, handovers, release and purge |
| [Session dashboard](docs/dashboard.md) | `ssf dashboard` across one or several factories, and the optional browser dashboard |
| [Workspaces and terminals](docs/drivers.md) | how ssf uses herdr for workspaces, terminals and agent state |
| [Uninstall](docs/uninstall.md) | work-preservation checks and retained data |
| [What the agent is told](docs/prompts.md) | the first prompt, the messages an agent receives, project boards |
| [Identity and bylines](docs/identity-and-bylines.md) | how `gh` and `git` act as the bot inside a session, and which session posted what |
| [Under the hood](docs/internals.md) | polling, delivery, resume and restarts; `ssf status --json`; known limits |
| [Development](docs/development.md) | building, scratch runs, a dev build as the service, the source layout |

Inside a session ssf started, `ssf guide` is the collaboration reference.

## License

MIT; see [LICENSE](LICENSE).
