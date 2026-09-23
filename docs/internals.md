# Under the hood

How the daemon polls, delivers, resumes and restarts, what `ssf status --json` carries, and the known limits. For whoever debugs the factory or builds on its status output; agents need none of it. `ssf` is a transport client: locally it executes the adjacent `ssf-server`, and with `--server HOST` it asks SSH to execute `ssf-server` on that host. Both arrive at the same server-side command parser, and commands that mutate live engine state then use the daemon's Unix socket on the server machine; the daemon exposes no network socket by default. A target-qualified daemon starts as `ssf-server --target NAME`: it loads and validates the client catalog first, refuses SSH targets, then pins the local config/state context or owned VM context for the life of the process (see [targets](operate.md#targets-which-factory-a-command-reaches) and [the server catalog](configuration.md)). This is the detail behind the README's [How it works](../README.md#how-it-works).

## `ssf status --json`

`ssf status --json` joins what ssf knows about every tracked item with what
herdr reports about the workspace working on it, so nothing else has to talk
to the driver. The top level carries `server`, `bot_login`,
`token_configured`, `service_enabled`, `service_active`, `daemon_reachable`,
`last_poll_at`, `last_error`, `poll_interval_secs`, `config_path`,
`anyone_allowed` (the
wildcard allow-list is in effect somewhere; the dashboards warn while it is),
`blocked_sessions` (the ids of sessions whose harness is not signed in),
`driver`, `sessions`, `repos` and the derived `dashboard` presentation.

`daemon_reachable` is whether something answers on the factory's Unix socket
(`ssf.sock`), and is the field that says whether the factory is running;
`service_active` and `service_enabled` are the service manager's own view of
the unit it may or may not run the daemon under. They differ wherever a
daemon was started without that unit — a container, another supervisor, a
foreground `ssf-server` — and the dashboards warn on the daemon rather than
on the unit (#463).

`repos[]` carries `name`, `harness`, `model`, `effort`, `path`,
`allowed_users` (who may drive that repository), `anyone_allowed` and
`issues[]`, which holds the same session objects as `sessions`. `driver` is
`available`, `error`, `workspaces` and `down`, and says whether herdr
answered.

The `sessions` array has one entry per tracked item.

| Field | Meaning |
|-------|---------|
| `id` | the session identity, `owner/repo#N` |
| `repo`, `number`, `kind`, `title`, `url` | the item; `kind` is `issue` or `pull_request` |
| `github_state` | `open`, `closed` or `merged`, as of the last poll |
| `active` | whether ssf still works the item |
| `triggers` | why ssf took it (assigned, mentioned, review requested, opened by the bot) |
| `pr` | pull request detail, when the item is one |
| `owner` | which session acts on the item: its own, or the session a PR is bound to by its origin tag or branch |
| `subscribers` | sessions that hear about the item without acting on it |
| `subscriber_events` | what each of them hears, for the ones that asked for more than the default (`state`/`all`, `ssf sub --events`); a session not named hears the item's own state changes |
| `subscriber_only` | the item is tracked only for subscribers: no owner, no workspace |
| `shares_workspace_of` | the session whose workspace this item is worked in |
| `delegated_by` | the session that handed the item off (`mode=delegate`) |
| `harness`, `model`, `effort` | what the session is on: the harness the driver reports for its pane, and the record's stack when nothing live reports one |
| `next_launch` | the stack (`harness`, `model`, `effort`) the next launch, resume, relaunch or re-creation would use. Present only when the pane is on another harness than the record names, as after a config edit under a live session; the model and effort are then left off the session itself, since ssf does not have that session's stack |
| `overrides` | `harness`, plus `model` and `effort` where the command named them, as put there by [`ssf handover`](sessions.md#handover) or [`ssf assign`](sessions.md#assigning-a-stack-before-there-is-a-session). Absent otherwise; the same shape as `overrides` on the item in `state.json` |
| `assigned_stack` | which of the two wrote the overrides. `ssf peers` prints an assigned stack as `harness pi` against a handover's `handed over to pi`; `ssf status` prints `assigned: harness=pi` and `handed over: harness=pi` |
| `handover` | a handover asked for and not yet carried out: `harness`, `harness_name` (for people), `model`, `effort`, `summary_chars`, `by` (the session that asked, absent for a person at a shell) and `requested_at` |
| `handover_note` | what an earlier handover left for a session that has not read it yet: `from` (the harness it came from) and `summary_chars`. `ssf status` and `ssf peers` show it as `handover note waiting:` |
| `agent_session_id`, `prompts_sent`, `last_prompt_at`, `bound_at` | ssf's delivery record |
| `retired_at` | when the session was marked inactive |
| `retirement_held_at` | present while a listing has dropped an item that still carries one of its triggers, which is why `ssf release` refuses it |
| `workspace_state` | on a retired item: `kept` (still on disk), `released` (removed by `ssf release`/`ssf purge`), `pending` (release accepted, removal on the next pass), `given-up` (kept after the daemon refused the agent's release three times) or `gone` (removed some other way) |
| `released_at` | when a released workspace was removed |
| `origin`, `posts_by_session`, `untagged_posts` | attribution (see [Identity and bylines](identity-and-bylines.md)): the session that opened the item, how many posts each session made on it, and how many bot posts carry no tag. The daemon's own `🤖 ssf` event posts count in neither |
| `blocked` | set while the session cannot take prompts: `reason` is `login` (its harness is at a sign-in prompt) or `start` (it could not be started at all, as after a handover to a harness that will not run), with `harness`, `harness_name`, `detail` (what the screen or the driver said), `since` and `fix`. See [A harness that is not signed in](sessions.md#a-harness-that-is-not-signed-in) |
| `agent_state` | herdr's `idle`, `working`, `blocked`, `done`, or SSF's `no-agent`, `no-workspace`, `unbound`, `unknown` (herdr not running) |
| `agent_live` | whether a live agent answers for the session |
| `last_assistant_message`, `tool`, `last_activity_at`, `column` | herdr's view of the pane; `last_activity_at` is the last write to the harness's local transcript, and when it is null the dashboard model's `activity_note` says why (see [dashboard.md](dashboard.md)) |
| `branch`, `worktree_id`, `worktree_path`, `workspace` | the workspace; `workspace` is the normalized workspace row |

`ssf peers` prints the same data as a terminal table: by default the active
sessions on `$SSF_REPO` (so an agent sees who else is on its repository, and
itself marked "(you)"), or every watched repository outside a session;
`--all` includes retired sessions. `ssf guide` tells agents about it.

The `dashboard` key is a presentation built from this model, not a second
source of truth. `dashboard.cards` comes only from driver-reported live agents
(origin, additional assigned issues, agent session id, state, activity, latest
message); active monitored items with no agent appear under
`dashboard.monitored_items`, each saying whether ssf has a workspace recorded
for it (`has_workspace`, with `branch` when one is known), so a client offering
`ssf assign` can tell an item that takes a session from one the command
refuses. Both dashboards consume this; neither reconstructs factory ownership.
See [Session dashboard](dashboard.md).

## Polling and delivery

- **Repository identity.** The mutable configured `owner/name` is paired with
  GitHub's immutable repository database id, resolved before issue polling at
  startup and every five minutes. A rename or transfer made through GitHub
  repairs the configuration, state keys, historical-name aliases and
  SSF-managed checkout remotes before work resumes.
- **Polling, not webhooks.** Every `poll_interval_secs` ssf makes four
  listings per repository (assigned to the bot, mentioning the bot, review
  requested from the bot, opened by the bot), as conditional requests, so a
  listing that has not changed costs nothing against the rate limit. Only
  items whose `updated_at` moved get their timeline fetched again, and an
  item ssf decided to leave alone is remembered with the reason (see
  [Ownership](sessions.md#ownership-one-session-per-item)).
- **Who is listened to.** Before an item gets a session, and before every
  delivery, the login behind the trigger or event is checked against the
  allow-list (see [Who may drive the
  factory](configuration.md#who-may-drive-the-factory)).
- **Pull requests.** Review comments, reviews, force-pushes and merges are
  rendered like issue activity. A PR from a fork gets a workspace on the base
  branch and the agent is told it cannot push to the fork.
- **One workspace per issue.** The binding lives in
  `~/.local/state/ssf/state.json` and is also recoverable from the driver
  (herdr names the workspace after the repository and issue number), so a lost
  state file re-attaches instead of creating a second workspace.
- **One engine per state directory.** `state.lock` is an exclusive
  process-held lock beside `state.json`; both `ssf-server` and
  `ssf-server --once` take it before reading state, and it is released when
  its owner exits. Do not unlink it to clear a refusal while an engine may
  still be running.
- **The capability secret.** An enabled [server web
  dashboard](dashboard.md#optional-server-web-dashboard) keeps its URL secret in
  `dashboard-token` beside `state.json` (0600), generated on the first start
  that serves the listener and read on every later one, so a restart does not
  invalidate the URL a client was configured with.
- **Retirement.** Closed or unassigned issues get one final message (push,
  final comment, then `ssf release` if everything is on origin) and are marked
  inactive; the workspace is marked completed in the driver and left in place.
  An item missing from the listings is not enough on its own: before a session
  is told to stop, the item is read again and every trigger it was taken on is
  re-checked against it (the assignees, the author, a mention in the body or in
  any comment, a review request on the pull request). A listing that lags, or
  that comes back without an item it should carry, therefore changes nothing
  while the reason the session exists is still in the item.
- **Held retirement.** A retirement held that way is recorded on the item as
  `retirement_held_at`, and only the item's timeline walk is paced: it is not
  repeated for ten minutes, because the listing that dropped the item stays
  wrong for a while and the walk is the expensive part. The item itself is
  still read every pass, so a close is noticed at once and the hold clears as
  soon as a listing carries the item again.
- **Mentions cannot be withdrawn.** An item the bot was only ever mentioned on
  stays the bot's until it closes, and `ssf release` says that rather than
  suggesting an unassignment that would do nothing. The mention re-check reads
  generously: it counts mentions inside code, in a pull request's review
  comments, and logins like `@bot_2`, none of which GitHub's own listing
  indexes. That is the safe direction in both places it is used, since refusing
  a real mention would turn away a session somebody asked for, or stop one that
  should have kept running. Its cost is that an item mentioning the bot only in
  one of those places holds its workspace as long as one mentioned in plain
  sight.
- **Removal.** Removal is the agent's (`ssf release`) or a person's
  (`ssf purge`) to ask for, and is refused whenever the worktree holds anything
  that is not on origin (see [Workspaces after
  close](sessions.md#workspaces-after-close-release-and-purge)). Re-assigning
  or reopening the issue re-creates a released workspace, resumes the
  conversation, and starts the new session on the item's own overrides.

## Per-harness delivery

A session's first prompt always goes through the driver's terminal path; later
item activity goes through the harness's own channel where one exists, and
through the terminal where it does not. The operator's view is [Item activity
delivery](harnesses.md#item-activity-delivery); what follows is the mechanism.

### First-prompt confirmation

ssf answers known first-run trust dialogs from the pane screen: Claude Code
and Codex ask whether to trust a new folder, Claude Code asks once per machine
whether to accept its bypass permissions mode, and Gemini and Pi ask about
trust when started without their flags. Approval prompts never appear, because
of the [default commands](harnesses.md#the-launch-command-and-permissions). A
sign-in prompt is the one dialog ssf cannot answer: that session is marked
blocked until a person signs the harness in.

A first prompt counts as delivered only once herdr sees the harness start
working on it, not because its bytes reached the terminal. A long paste first
opens OMP's attachment choice; herdr's Enter accepts that choice but can leave
the resulting attachment in the composer, so on a stalled delivery ssf
identifies that original prompt and sends only the missing Enter rather than
pasting a second copy. A transition to `working` or `blocked` confirms the
delivery; a harness herdr cannot narrate is accepted after a successful Enter,
so later passes never submit the assignment again. An unseeded live harness
found on a later pass submits an identifiable stranded prompt, and resends only
after a positively identified first-run dialog; an ambiguous screen is accepted
rather than risking an already-consumed prompt being sent again as a steering
message. If initial delivery reports an error but the launched harness is
alive, ssf uses that recovery path at once, and the item stays active while
recovery is pending rather than appearing retired.

### Held deliveries

A channel that is not ready is a hold, not a failure: nothing is published, the
item keeps its events and its session, no delivery failure is counted against
it, and a session restart takes what is waiting. An ambiguous write, one whose
receipt the harness never confirmed, is held the same way and never resent or
pasted, since a duplicated prompt is the worse outcome. Inspect the journal and
the target transcript before resolving such a hold; do not delete a journal to
force a retry. `ssf doctor` names the sessions in these states.

### OMP and Pi: the delivery bridge

OMP and Pi sessions started with SSF's default command load the shipped
`ssf-delivery.ts` extension. The daemon puts each later item event in that
session's mailbox under the factory state directory at
`delivery/<owner>/<repo>/<issue>/`, and the extension sends it as a
user-attributed context message with `triggerTurn: true`. An idle agent
therefore starts a turn, and a working agent receives the event at its next
step boundary rather than at the end of the turn: OMP is given
`deliverAs: "aside"`, and Pi, whose extension API has no `aside` but whose
`steer` means that same step boundary, is given `deliverAs: "steer"`. Both
hand the message over once the tool calls in flight have finished and before
the next model call, without cutting those calls short. Neither waits for the
turn to end, which an agent inside one long tool loop may not reach for hours.
A harness the launcher does not name gets `steer` too, which on OMP preempts
the step it arrives in and finishes it in the background; immediate delivery is
preferred to a queue that may never drain. The launcher names the harness in
`SSF_HARNESS` for that choice.

The extension never writes bytes to the pane, and a draft already in the
composer is left intact. It injects each pending event once per process and
renames the file to an acknowledgement only once the session's transcript
records the injected message: the rename is the daemon's receipt that the agent
has the event, not that the harness accepted a call it could still drop, and
the acknowledgement remains the session's idempotency record. Until the record
exists the file stays pending, so a bounded wait that ends first reports the
delivery as published rather than recorded, and the bridge keeps trying. A
later event waiting behind an unrecorded one is held rather than published
beside it, so a session never receives the same events twice. Each mailbox
event is written through a temporary file and an atomic rename.

The stable mailbox key is the session's next prompt count plus a content
fingerprint, so a retry observes the same pending or acknowledged event rather
than publishing another copy. The daemon never reports a delivery it did not
make: a caller that took one would move its watermark past events the mailbox
never received.

`ssf launch` gives the extension the session's mailbox and the bridge this build
ships: the installed `share/ssf/harness` copy when it is byte-for-byte this
daemon's own, else a copy served from `harness/` under the factory state
directory. `SSF_PI_BRIDGE` and `SSF_PI_LAUNCHER` name the two files, and `ssf
doctor` says which installed copy it refused. The pairing is not cosmetic: the
bridge repairs the mailbox's ready marker, so a session running one from another
build takes no events at all and its agent never hears that its item closed.
Restarting the session loads the bridge this daemon serves; after upgrading SSF,
restart any already-running OMP or Pi session for that reason.

The poller writes `ready.json` as the running extension's own attestation that
events left in the mailbox will be taken, and rewrites it on any poll that does
not find it naming its process, so a marker removed under a live session is
repaired within a poll instead of the channel being refused for the rest of
that session's life. A session that changes under the process, OMP's
`session_switch`, `session_branch`, `session_tree`, as `session_start` does,
takes the marker and the poller over while keeping what an earlier one had
handed over, since those events replace the transcript and not the queue an
unrecorded injection may sit in. `session_shutdown` removes only its own marker
and stops its poller.

The default command runs through a small exec wrapper that keeps the harness
transcript in that mailbox's session directory. It explicitly resumes only the
latest transcript in that directory, never a global "most recent" session, and
each injected message carries its stable mailbox id in extension-only metadata.
If the harness exits between injecting a message and recording it, the file is
still pending, and the replacement resumes that transcript: its bridge
acknowledges an id already present or injects the still-pending event, and
herdr does not also submit it through the terminal. `ssf doctor` checks the
bridge and launcher a session would be started with, plus the ready marker of
each live OMP or Pi session.

The same state directory holds the [context
compaction](harnesses.md#context-compaction) overlay an OMP session is started
with, one file per threshold (`omp-compaction-<tokens>.yml`), because OMP takes
that setting only through its own configuration: `ssf launch` writes the file
and names it in `PI_CONFIG_FILES`, which a session reads at startup where
`omp --config <file>` is not. A write that fails leaves the session on OMP's
own threshold and says so on stderr rather than keeping it from starting. The
other harnesses that have the setting take it on the command line.

### Claude Code: the peer inbox

Claude Code's default command adds
`--settings '{"crossSessionInbound":"accept"}'` alongside bypass permissions. A
custom command must keep both to use this channel. Later events go to the exact
pane's foreground Claude PID through its authenticated NDJSON peer inbox
socket, discovered in `~/.claude/sessions/` (or under `CLAUDE_CONFIG_DIR`).
Numeric Linux `/proc` start ticks, and older `ps` timestamps elsewhere, guard
against PID reuse. Idle delivery starts a turn, busy delivery uses priority
`next`, and the composer is untouched.

The protocol is unofficial and has no ordinary receipt, so SSF journals the
intent before writing and then confirms the enqueue and user entries in that
session's persistent transcript, keeping the journal under the item's delivery
directory. A retry reconciles that journal rather than sending another copy. An
exited target is resumed from its saved session before reconciliation, never
given a duplicate first prompt. A write with no transcript confirmation is
ambiguous and is held. If the socket is unavailable before anything is sent,
the terminal path remains and `ssf doctor` reports the limitation. Existing
sessions must be restarted with the setting before they can use the channel.
Dialogs stay outside the channel's scope.

### Codex: the app-server channel

Codex has an experimental, opt-in native channel when its herdr-managed TUI is
launched with an explicit `--remote unix:///absolute/item-specific/app.sock`
endpoint together with SSF's bypass-approvals/sandbox and bypass-hook-trust
flags. The launcher or herdr must provision that same server: SSF does not
start a second headless conversation and does not manage the server lifecycle.
The socket must be private (mode 0600), owned by the current user, and
dedicated to one ordinary loaded conversation. SSF binds its endpoint,
directory, conversation id and transcript under `codex-binding.json` in the
item's delivery directory; switching to another conversation or endpoint is
held, never guessed from the newest session.

Later events use the app-server's `turn/start`: an idle conversation starts
generating, and active work admits the event into the current turn at a model
boundary, with no terminal input. Admission can coalesce into an existing turn,
so a turn id is not a message receipt: SSF journals before sending and confirms
the exact `clientUserMessageId` through the persistent rollout's user-message
echo, matching thread and content. Subsequent passes reconcile the same intent
without resubmitting the RPC, including after a crash between receipt and
confirmation. A delayed echo stays retryable; an ambiguous attempt is never
resent or pasted. A changed binding, or an explicit channel that is unavailable
or invalid, is held and reported by `ssf doctor`; it never silently pastes.

A handover or fresh onboarding archives the active binding without discarding
old receipts, and a release retains it so the saved conversation can be
resumed. A remote resume retains server permissions and omits the
permission-bypass flag. Standalone Codex sessions, launched without an explicit
endpoint, keep the terminal path.

### The terminal path

OpenCode, Gemini CLI, Copilot CLI, Grok CLI and Crush have no proven channel
wired into SSF's attached interactive session, so their later activity uses
`herdr agent prompt`; if herdr refuses because the agent is at a question, the
raw bracketed-paste fallback is used. First prompts in every harness, and a
newly created or resumed pane of any harness, use the confirmed first-prompt
path above, there cannot be a person's draft in a pane SSF has just created.

## Resume and restarts

After the first message ssf records the agent's conversation id (Claude Code
and Codex keep transcripts on disk). If the agent's terminal is gone, ssf
starts it again with `--resume <id>` (Codex: `codex resume <id>`) and sends
only the new events. If resuming fails, or the harness has no resume support,
it starts fresh and resends the whole issue context, the session's own item,
even when what triggered the relaunch was activity on an item it owns. If the
workspace itself is gone, ssf re-creates it from the old branch (local or
`origin/`) and does the same. Claude Code resumes a session from any directory,
so this works even when the new worktree has a different path.

A resume has failed only when no agent is running in its pane once the driver's
wait is over; the screen then says whether the harness could not find the
session. A resumed agent that is idle, at a question, or already at work on the
messages queued in its conversation has settled, a Claude Code with a backlog
starts on it at once and reports `working`, never `idle`, and one still alive
when the wait runs out without a state is kept as the resumed conversation all
the same, with the delivery going to it as a steering message when it is at
work. A fresh harness is started only once nothing of the resume is running, so
a workspace never holds two agents. The `resumed` event on the item says which
it was: `conversation: resumed` or `fresh`.

Herdr keeps no issue link of its own, so SSF recovers a workspace by the
worktree name (`issue-N-...`) and asks herdr which worktree a workspace is
bound to before prompting or removing it.

A daemon restart is invisible to agents: the state is on disk, the driver keeps
the terminals, and delivery finds them again. The service comes back from any
exit, a clean one included (`Restart=always`, see [the background
service](operate.md#the-background-service)), because systemd counts a SIGTERM
as a clean exit and `on-failure` would leave the factory inactive after one. An
explicit `systemctl stop`, and anything else that stops the unit, is not undone.

A machine restart takes the terminals with it, so the daemon runs a startup
pass once the driver first answers. Every active session that owns its
workspace and has no live agent terminal is started again through the same path
as any relaunch (`--resume` when a session id was captured, fresh with the
item's story otherwise), with one message saying it was interrupted and telling
it to check `git status` and `git log` and carry on, or say on the item what is
left. Relaunches happen one at a time, each waiting for its agent to settle.
Sessions that are still running are not touched, workspaces that are gone are
brought back on their next event, and sessions whose workspace was released or
is about to be are skipped. At start the daemon waits for the driver
(`daemon.startup_driver_wait_secs`, checking every ten seconds) before its first
poll; if the driver is still not up by then, polling starts anyway and the pass
runs on the first poll that finds it. The pass runs per driver.
`daemon.resume_on_start = false` turns it off. `ssf-server --once` runs it too.

## The gh and git shims

`ssf launch` links `~/.config/ssf/bin/gh` to the ssf binary and puts that
directory first on the agent's `PATH`. Next to it, `git` wraps git the same
way, and `ssf` links to the same binary so the `ssf` commands the prompts name
run the daemon's own build rather than an older package on the shell's `PATH`
(`ssf doctor` says when the two differ). The contract the shims implement, the
byline and the origin tag, is in [Identity and
bylines](identity-and-bylines.md).

**Where the byline goes.** Invoked as `gh`, ssf prepends the byline line to the
body of `issue create`, `issue comment`, `pr create`, `pr comment` and
`pr review` (and `issue new` and `pr new`, gh's own names for the same two
creates), in every spelling gh takes the body in after the command words:
`--body`, `--body=`, `-b`, `-b=`, `-bX`, `--body-file`, `--body-file=`, `-F`,
`-F -`, `-F=` and `-FX`, and any of those behind the value-less letters of a
cluster, so a review's `-ab hi` and a create's `-eF notes.md` are stamped as
much as `-b hi` is. Which letters are value-less depends on the command, since
`-a` approves on a review and names an assignee on a create. Flags before the
command words are handled too: after locating those words, the shim binds
separated values the way gh does, so `gh -ab pr review hi` stamps `hi`.

**Bodies.** The last repeated `--body-file` wins, as in gh; earlier files and
stdin are not read, and mixing `--body` and `--body-file` is passed through for
gh to reject. Invalid UTF-8 produces an explicit error rather than posting
altered or untagged text; an unreadable file is left for gh to report, while a
stdin read error stops the shim because stdin may already have been consumed. A
body gh builds for itself (`--fill`, `--fill-first`, `--fill-verbose`,
`--editor`, `--web`, the interactive prompt) carries no byline unless the agent
supplies one. Bodies that already start with the tag are not stamped twice, and
outside a session (no `SSF_ISSUE`) the shim is a plain pass-through. A large
stamped body travels through an inherited anonymous file instead of argv,
avoiding argument limits: Linux uses a memory file, other Unix systems a
private temporary file that is immediately unlinked. Failure to prepare it
stops the command without posting; GitHub still enforces its own body-size
limit.

**Reviews.** An approving review needs no body of its own, so one that is only
the byline is added, in every spelling of the approval (`--approve`,
`--approve=true`, `-a`, `-a=true` and the `-a` inside a cluster; not
`--approve=false`, which gh does not read as an approval either). Explicit
empty or whitespace-only bodies on comment and request-changes reviews are
rejected before posting, and missing bodies are left for gh to reject: a review
carrying nothing but a byline says nothing, and a request for changes carrying
nothing but a byline blocks the pull request.

**Which repository.** To pick the byline's short or long form the shim works out
the repository posted to: `--repo` or `-R` in any spelling, an item given as a
URL, `GH_REPO`, else the checkout's `origin` remote
(`git config --get remote.origin.url`); when none of those says, the long form
is used, which links from anywhere. That is gh's own list, read slightly
differently: the first `--repo` wins over a later one and over a URL, and a
`--repo` that will not parse ends the search. Each difference needs a line
naming the repository twice over, or unparseably, which is not a line an agent
writes; the cost is the short form on a post landing elsewhere, never a lost
tag. The URL has to be the item's own: a URL that is the value of a flag
taking one is not read as the item, which matters most for `--parent`,
`--blocked-by` and `--blocking`, since that answer outranks `GH_REPO` and a
post landing elsewhere with the short `#N` would link to that repository's
issue N. An item after `--` still counts, as it does for gh. Everything else is
passed to the real gh untouched; the shim leaves the terminal alone and does
not break gh's interactive flows.

**A tool that drops its environment.** A harness may run a tool of its own with
a reduced environment: OMP's Python tool keeps `HOME`, `PATH` and a handful
more, and the session's variables are gone. `PATH` survives, so the shim still
runs, but with no `SSF_REPO` no post is stamped, with no `GH_CONFIG_DIR` and
`GH_TOKEN` the real gh reads the operator's `~/.config/gh` and posts as them,
and without `GIT_SSH_COMMAND`, the `GIT_CONFIG_*` entries and the credential
helper, a push uses the operator's key. So both shims look up the process tree
for the nearest ancestor carrying `SSF_REPO`, the pane's own environment, and
hand the program they exec its `SSF_*`, `GH_*`, `GITHUB_*` and `GIT_*`
variables (whole families, so a variable `ssf launch` starts exporting needs no
change here), each only where the process has no value of its own. The `git`
shim touches nothing else: it passes the command line through and runs the real
git, so `ssf git-credential` resolves the session's repository and `[repo.git]`
identity as it does from the agent's shell. Nothing is recovered outside a
session, and where the process tree cannot be read the shims run without the
recovery. ssf's own use of gh stays outside this: `gh auth token --user <login>`
clears `GH_CONFIG_DIR` and `GH_TOKEN` to read another account from gh's own
store, and on a host whose gh keeps tokens in its config file rather than the
keyring, putting them back would look in ssf's account-less directory instead,
so ssf resolves the real gh directly rather than through the shim.

`ssf doctor` checks that the real gh and git are installed and that the shim
links to the running ssf, and reports untagged bot posts.

## Known limits

- The bot identity is a default, not a security boundary. On bare metal the
  agents run as your Unix user inside your session, so a determined agent can
  still read your own gh token from the keyring or use your SSH agent. ssf
  tells agents to act only as the bot and to report missing permissions
  instead; the isolation is the [microVM](vm.md).
- Session resume, and therefore memory across relaunches, is implemented for
  Claude Code and Codex; other harnesses are restarted with the full issue
  context instead.
- One agent per issue; a second assignee is not coordinated with. A session
  owns what it opens only within its own repository.
- `ssf status` asks every driver in use for its workspace list on every call (a
  few hundred milliseconds); when a driver is not running the ssf side is still
  reported and its sessions' agent states show as unknown.
- Logs: `journalctl --user -fu ssf.service`, or on macOS, where the service is
  launchd's, `tail -f $(brew --prefix)/var/log/ssf.log` (inside the VM,
  `ssf vm logs`).
