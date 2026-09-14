# Workspaces and terminals

SSF is built on top of [herdr](https://herdr.dev/). Herdr provides the terminal
workspaces that hold coding agents, while SSF connects those workspaces to
GitHub issues, prompts, persistent session state, and lifecycle events.

The daemon uses the herdr CLI to open one workspace per issue or pull request,
run the configured harness in its root pane, deliver later GitHub activity, and
read the agent's state. `herdr.command` selects the CLI and
`herdr.projects_dir` selects where SSF keeps repository clones. Worktrees are
created beside each clone in `<clone>.worktrees/`.

Herdr must be installed and its server must be running wherever the factory
runs. A VM factory installs and starts herdr in the guest. In host mode, install
herdr on the host and start a herdr session before starting SSF. `ssf doctor`
checks both the CLI and the running server.

New and reopened workspaces are labelled `<repo>-<issue-number>` (for example,
`simple-software-factory-282`), using the GitHub repository name even when
`repo.path` has a different directory name. Already open workspaces keep their
labels until reopened. Git worktree paths and branch names still include the
issue number and title for recovery. Herdr recognises the agent in the root pane
and reports its state (`idle`, `working`, `blocked`, `done`); later messages are
delivered with `herdr agent prompt`.

SSF answers known first-run trust dialogs from the pane screen. A login prompt
is the one dialog it cannot answer: that session is marked blocked until a
person signs the harness in. The first prompt after a launch is confirmed by
waiting until herdr sees the harness working on it. Ambiguous stalls are handled
without pasting a second copy, so a long prompt already sitting in the composer
is not duplicated.

Herdr keeps no issue link of its own, so SSF recovers a workspace by the
worktree name (`issue-N-...`). Before prompting or removing it, SSF asks herdr
which worktree the workspace is bound to. A workspace id since reused for
something else is treated as gone rather than touched, and changing a shell's
working directory does not change ownership. The clone's own herdr workspace is
left alone.

Herdr can only launch agents it recognises (`herdr agent start --help` lists
them). `ssf repo add` warns about an unsupported harness, and a start that never
produces an agent gives up after `herdr.tui_idle_timeout_ms`.

OMP aborts a provider stream after five minutes without an event. It can retry
before output is visible, but a stall after partial output ends the turn rather
than risk replaying side effects; Herdr correctly reports that terminal as
`done`, indistinguishable from an ordinary completed turn. SSF's default OMP
command sets `PI_STREAM_IDLE_TIMEOUT_MS=900000` (15 minutes), the upstream
recommendation for long agentic workloads. A repository `command` replaces the
whole default, so include that environment setting there too if a custom OMP
command should retain the longer window. Set a different value deliberately to
tune the tradeoff; `0` disables the watchdog and can leave a genuinely wedged
stream waiting forever.

Workspace ids stored by SSF combine herdr's workspace id with the checkout
path, such as `w7@/home/you/ssf/projects/widgets.worktrees/issue-42-fix`. The
path lets SSF verify ownership before delivering input or removing a workspace.
Closing a herdr tab does not remove its git worktree; `ssf doctor` reports
stranded work that may need recovery.

The `driver = "herdr"` setting remains available as an explicit declaration,
but herdr is currently the only supported driver.
