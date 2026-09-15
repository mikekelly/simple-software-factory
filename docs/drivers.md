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
delivered through the harness channel described below where one is available.

## Item-activity delivery

OMP and Pi sessions started with SSF's default command load the shipped
`ssf-delivery.ts` extension. The daemon puts each later item event in that
session's mailbox, and the extension sends it as a user-attributed context
message with `triggerTurn: true` and `deliverAs: "followUp"`. An idle agent
therefore starts a turn; a working agent receives the event after its current
turn. The extension acknowledges the event after handing it to the harness. It
never writes bytes to the pane, and a draft already in the OMP/Pi composer is
left intact.

The default command runs through a small exec wrapper that keeps the harness
transcript in that mailbox's session directory. It explicitly resumes only the
latest transcript in that directory, never a global "most recent" session.
Each injected message carries its stable mailbox ID in extension-only metadata.
If the harness exits between recording a message and acknowledging it, the
replacement resumes that transcript: its bridge acknowledges an ID already
present or injects the still-pending event, and Herdr does not also submit it
through the terminal.

The mailbox lives under the factory state directory at
`delivery/<owner>/<repo>/<issue>/`. Its ready marker belongs to the running
extension. After upgrading SSF, restart any already-running OMP/Pi session so
it is relaunched with the bridge; until then delivery is held rather than
risking a terminal paste. `ssf doctor` reports a live OMP/Pi session whose
marker is unavailable.

Claude Code's default command adds
`--settings '{"crossSessionInbound":"accept"}'` alongside bypass permissions.
Later events go to the exact pane's foreground Claude PID through its authenticated
peer inbox socket, discovered in `~/.claude/sessions/` (or `CLAUDE_CONFIG_DIR`).
Idle delivery starts a turn; busy delivery uses priority `next`, without touching
the composer. This unofficial protocol is live-verified with Claude Code 2.1.268.
SSF confirms receipt in that session's persistent transcript and keeps a journal
under the same delivery directory. Retries reconcile that journal rather than
sending another copy. If the socket is unavailable before sending, the legacy
terminal fallback remains; `ssf doctor` reports that limitation. Existing sessions
must be restarted with the new setting; a custom command must retain the inline
setting and bypass-permissions flag to use native delivery.

A write with no transcript confirmation is ambiguous: delivery is held, never
resent or pasted. Inspect the journal's target transcript before resolving it;
do not delete the journal merely to force a retry. An exited target is resumed
from its saved session before reconciliation, not given a duplicate first prompt.
Dialogs remain outside the channel's scope.

Codex has an **experimental, opt-in** native channel when its Herdr-managed TUI
is launched with an explicit `--remote unix:///absolute/item-specific/app.sock`
endpoint and SSF's bypass-approvals/sandbox and bypass-hook-trust flags. The
launcher/Herdr must provision that same server; SSF does not start a second
headless conversation or manage the server lifecycle. The socket must be private
(mode 0600), owned by the current user, and dedicated to one ordinary loaded
conversation. SSF binds its endpoint, directory, conversation ID and transcript
under `codex-binding.json` in the item's delivery directory. Switching to another
conversation or endpoint is held, never guessed from the newest session.

Later events use app-server `turn/start`: idle starts generation, while active
work admits the event into the current turn at a model boundary without terminal
input. This path is live-verified with Codex 0.154.0. SSF journals before sending,
and confirms the exact `clientUserMessageId` through the persistent rollout's
user-message echo. A delayed echo remains retryable; an ambiguous attempt is
never resent or pasted. Inspect the journal and target rollout before resolving
it; do not delete intent to force a retry. Ordinary standalone Codex sessions
retain terminal fallback. Explicit remote launches with an unavailable or invalid
channel are held, and `ssf doctor` reports the limitation. First prompts still
use Herdr's confirmed path. See [configuration](configuration.md#codex-native-delivery).

OpenCode, Gemini CLI, Copilot CLI, Grok CLI and Crush do not
yet have a proven channel wired into SSF's attached interactive session. Their
later activity still uses `herdr agent prompt`; if Herdr refuses because the
agent is at a question, the existing raw bracketed-paste fallback remains.
Fresh and resumed OMP/Pi panes also use the confirmed first-prompt path below;
there cannot be a person's draft in a pane SSF has just created.

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
command should retain the longer window. A custom OMP/Pi command must also use
`"$SSF_PI_LAUNCHER" pi|omp ... -e "$SSF_PI_BRIDGE"` to retain resumable native
item-activity delivery; the launch wrapper supplies both paths. Set a different
timeout value deliberately to
tune the tradeoff; `0` disables the watchdog and can leave a genuinely wedged
stream waiting forever.

Workspace ids stored by SSF combine herdr's workspace id with the checkout
path, such as `w7@/home/you/ssf/projects/widgets.worktrees/issue-42-fix`. The
path lets SSF verify ownership before delivering input or removing a workspace.
Closing a herdr tab does not remove its git worktree; `ssf doctor` reports
stranded work that may need recovery.

The `driver = "herdr"` setting remains available as an explicit declaration,
but herdr is currently the only supported driver.
