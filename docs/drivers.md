# Drivers: Orca and herdr

What a driver is, how the Orca and herdr drivers differ, and what to expect from each. For whoever chooses where the agents run; agents need none of it.

The daemon does not care what holds the agents' terminals. Everything it
asks for (a checkout of the repository, a workspace per item on the item's
branch, starting the agent there, pasting a message in, whether the agent
is still there or busy, removing the workspace) goes through a *driver*,
and there are two. The `driver` key picks the default for the whole
instance; a `[[repo]]` can set its own (`ssf repo add ... --driver herdr`),
so one daemon can run some repositories in Orca and others in herdr.
`ssf doctor` checks every driver in use. To keep the agents (and the
daemon) away from your home directory altogether, the whole factory can
run inside a microVM instead (see [Inside a microVM](vm.md)).

A pass skips the repositories of a driver that is not answering while the
others carry on; the outage shows as the error in `ssf status`, and that
driver's sessions show an unknown agent state until it answers (the other
driver's are reported as usual). The startup pass runs per driver, on the
first pass that finds that driver ready. Cloud-hosted agent sessions are
not a driver yet; they need a different shape (no local terminal, no local
`ssf`) and are tracked in
[#49](https://github.com/mikekelly/simple-software-factory/issues/49).

Either way the agent is started through `ssf launch`, which supplies the
bot identity, the `gh` wrapper that adds the byline, and the `ssf`
commands (see [Identity and bylines](identity-and-bylines.md)), so nothing
changes for the agent. The first-run dialogs an agent shows are answered by
ssf under both drivers: Claude Code's folder-trust question and its
one-off "Bypass Permissions mode" acceptance, Codex's directory-trust
question, and Gemini's and Pi's trust dialogs when they are started without
their `--skip-trust`/`--approve` flags (see
[Permissions](configuration.md#permissions)). A login prompt is the one
dialog ssf cannot answer: a session showing one is marked blocked and
brought back once a person has signed the harness in (see [A harness that
is not signed in](sessions.md#a-harness-that-is-not-signed-in)).

## `orca` (the default)

The [Orca](https://onorca.dev/) desktop app and its CLI. Orca keeps the
projects (repositories it has no project for are cloned under
`orca.projects_dir`), worktrees are linked to the issue number, terminals
belong to the workspace, and the bar widget's "open workspace" goes there.
Orca has to be installed (`orca-ide-bin`), signed in and running; the
daemon waits for it at start (`daemon.startup_orca_wait_secs`).
`orca.command` must be the CLI entry point,
`/usr/lib/orca-ide/bin/orca-ide` (`/usr/bin/orca-ide` launches the app).
Pick Orca to watch agents work in a GUI and take over a terminal.

## `herdr`

The [herdr](https://herdr.dev/) terminal workspace manager (a herdr
session has to be running: start `herdr` in a terminal and leave it). ssf
clones the repository itself under `herdr.projects_dir` (or uses
`repo.path`), makes a git worktree per item in `<name>.worktrees/` next to
the clone, opens it as a herdr workspace and runs the agent in the
workspace's root pane through the same `ssf launch` wrapper as with Orca.
herdr recognises the agent in the pane and reports its state (`idle`,
`working`, `blocked`, `done`); messages go in with `herdr agent prompt`,
which pastes and submits them.

herdr keeps no link between a workspace and an issue, so ssf finds a
workspace it lost track of by the worktree's name (`issue-N-...`), and
remembers a workspace as herdr's id plus the checkout it was opened on
(`w7@/path`). Before prompting or removing, ssf asks herdr which worktree
that workspace is bound to; a workspace id herdr has since given to
something else is treated as gone rather than touched, and a shell in the
workspace that has `cd`'d elsewhere changes nothing. herdr also opens one
workspace for the clone itself the first time it opens a worktree of it;
that one is left alone. Clicking a session in the bar widget focuses its
herdr workspace.

herdr can only run the agents it recognises in a pane (`herdr agent start
--help` lists them; `crush` from `ssf agents` is not among them in herdr
0.8.2); `ssf repo add --driver herdr` warns when the harness is not on that
list, and a start with one that is not gives up after
`herdr.tui_idle_timeout_ms`. Pick herdr for a terminal-only machine, over
ssh, or as the driver inside the [microVM](vm.md), where it is the only one.
