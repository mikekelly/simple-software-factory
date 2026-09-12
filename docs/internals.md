# Under the hood

How the daemon polls, delivers, resumes and restarts, what `ssf status --json` carries, and the known limits. For whoever debugs the factory or builds on its status output; agents need none of it.

The details behind the README's [How it works](../README.md#how-it-works).

`ssf` is a transport client. Locally it executes the adjacent `ssf-server`;
with `--server HOST` it asks SSH to execute `ssf-server` on that host. Both
arrive at the same server-side command parser. Commands that mutate live
engine state then use the daemon's Unix socket on the server machine; the
daemon itself exposes no network socket.

## Polling and delivery

- **Polling, not webhooks.** Every `poll_interval_secs` ssf makes four
  listings per repository (assigned to the bot, mentioning the bot, review
  requested from the bot, opened by the bot), as conditional requests, so a
  listing that has not changed costs nothing against the rate limit. Only
  items whose `updated_at` moved get their timeline fetched again, and an
  item ssf decided to leave alone is remembered with the reason (see
  [Ownership](sessions.md#ownership-one-session-per-item)).
- **Who is listened to.** Before an item gets a session, and before every
  delivery, the login behind the trigger or event is checked against the
  allow-list (see [Who may drive the factory](configuration.md#who-may-drive-the-factory)).
- **Pull requests.** Review comments, reviews, force-pushes and merges are
  rendered like issue activity. A PR from a fork gets a workspace on the base
  branch and the agent is told it cannot push to the fork.
- **One workspace per issue.** The binding lives in
  `~/.local/state/ssf/state.json` and is also recoverable from the driver
  (Orca links the worktree to the issue number; herdr names it after
  it), so a lost state file re-attaches instead of creating a second
  workspace.
- **One engine per state directory.** `state.lock` is an exclusive
  process-held lock beside `state.json`; both `ssf-server` and `ssf-server --once`
  take it before reading state. It is released when its owner exits. Do not
  unlink it to clear a refusal while an engine may still be running.
- **Delivery into the agent's terminal.** Messages are pasted with bracketed
  paste so multi-line text arrives as one message, then Enter. Claude Code
  queues it as a steering message while busy, or runs it when idle.
- **Bringing a session back.** After the first message ssf records the
  agent's conversation id (Claude Code and Codex keep transcripts on disk).
  If the agent's terminal is gone, ssf starts it again with `--resume <id>`
  (Codex: `codex resume <id>`) and sends only the new events; if resuming
  fails or the agent has no resume support, it starts fresh and resends the
  whole issue context (the session's own item, even when what triggered the
  relaunch was activity on an item it owns). If the workspace itself is
  gone, ssf re-creates it from the old branch (local or `origin/`) and does
  the same. Claude Code resumes a session from any directory, so this works
  even when the new worktree has a different path. A resume has failed
  only when no agent is running in its pane once the driver's wait is
  over (the screen then says whether the harness could not find the
  session): a resumed
  agent that is idle, at a question, or already at work on the messages
  queued in its conversation (a Claude Code with a backlog starts on it
  at once and reports `working`, never `idle`) has settled, and one still
  alive when the wait runs out without a state is kept as the resumed
  conversation all the same; the delivery goes to it, as a steering
  message when it is at work. A fresh harness is started only once
  nothing of the resume is running, so a workspace never holds two
  agents (#131, #133). The `resumed` event on the item says which it was:
  `conversation: resumed` or `fresh`.
- **First-run dialogs.** Claude Code and Codex ask whether to trust a new
  folder, Claude Code once per machine whether to accept its bypass
  permissions mode, and Gemini and Pi ask about trust when started without
  their flags; ssf answers all of them so unattended launches do not stall.
  Approval prompts never appear because of the [default
  commands](configuration.md#permissions).
- **Restarts.** A daemon restart is invisible to
  agents: the state is on disk, the driver keeps the terminals, and delivery
  finds them again. A machine restart takes the terminals with it, so the
  daemon runs a startup pass once the driver first answers: every active
  session that owns its workspace and has no live agent terminal is started
  again through the same path as any relaunch (`--resume` when a session id
  was captured, fresh with the item's story otherwise), with one message
  saying it was interrupted and telling it to check `git status`/`git log`
  and carry on, or say on the item what is left. Relaunches happen one at a
  time, each waiting for its agent to settle. Sessions that are still
  running are not touched, workspaces that are gone are brought back on
  their next event, and sessions whose workspace was released or is about
  to be are skipped. At start the daemon waits for the driver
  (`daemon.startup_driver_wait_secs`, checking every ten seconds) before its
  first poll; if it is still not up by then, polling starts anyway and
  the pass runs on the first poll that finds it. The pass runs per driver.
  `daemon.resume_on_start = false` turns it off. `ssf-server --once` runs it
  too.
- **Retirement.** Closed or unassigned issues get one final message (push,
  final comment, then `ssf release` if everything is on origin) and are
  marked inactive; the workspace is marked completed in the driver and left
  in place. An item missing from the listings is not enough on its own:
  before a session is told to stop, the item is read again and every
  trigger it was taken on is re-checked against it (the assignees, the
  author, a mention in the body or in any comment, a review request on the
  pull request). A listing that lags, or that comes back without an item it
  should carry, therefore changes nothing while the reason the session
  exists is still in the item. One consequence is that an item the bot was
  only ever mentioned on stays the bot's until it closes, since a mention
  cannot be withdrawn; `ssf release` says that rather than suggesting an
  unassignment that would do nothing. A retirement held this way is
  recorded on the item as `retirement_held_at`, shown by `ssf status
  --json`, and the item's timeline is not walked again for ten minutes:
  the listing that dropped it is wrong and stays wrong for a while, and
  the walk is the expensive part. Only that walk is paced. The item itself
  is still read every pass, so an item that closes is still noticed at
  once, and the hold is cleared as soon as a listing carries the item
  again. The mention re-check reads generously: it counts mentions inside
  code, in a pull request's review comments and logins like `@bot_2`,
  none of which GitHub's own listing indexes. That is the safe direction
  in both places it is used, since refusing a real mention would have the
  gate turn away a session somebody asked for and have retirement stop
  one that should have kept running. Its cost is that an item the bot is
  mentioned on only in one of those places holds its workspace until it
  closes, the same as one mentioned in plain sight. Removal is the
  agent's (`ssf release`) or a person's (`ssf purge`) to ask for, and is
  refused whenever the worktree holds anything
  that is not on origin (see [Workspaces after
  close](sessions.md#workspaces-after-close-release-and-purge)).
  Re-assigning or reopening the issue re-creates a released workspace and
  resumes the conversation.

## `ssf status --json`

`ssf status --json` joins what ssf knows about every tracked item with what
the driver reports about the workspace working on it (`orca worktree ps`,
or herdr's workspace and agent lists), so nothing else has to talk to the
driver. Its `sessions` array has one entry per item:

| Field | From |
|-------|------|
| `id`, `repo`, `number`, `kind` (`issue`/`pull_request`), `title`, `url` | ssf; `id` is the session identity `owner/repo#N` |
| `github_state` (`open`/`closed`/`merged`), `active`, `triggers`, `pr` | GitHub, as of the last poll |
| `owner`, `subscribers`, `subscriber_only`, `shares_workspace_of`, `delegated_by` | which session acts on the item: its own, or the session it is bound to (opened from it, or a PR on its branch); `subscribers` are the sessions that hear about it without acting on it; `subscriber_only` marks an item tracked only for them (no owner, no workspace); `delegated_by` names the session that handed the item off (`mode=delegate`) |
| `agent_session_id`, `prompts_sent`, `last_prompt_at`, `bound_at`, `retired_at`, `retirement_held_at` | ssf's delivery record; `retirement_held_at` is present while a listing has dropped an item that still carries one of its triggers, which is why `ssf release` refuses it |
| `harness`, `model`, `effort`, `overrides`, `handover`, `handover_note` | what the session runs: `harness`, `model` and `effort` are the effective ones (the repository's, with the item's overrides applied), and `overrides` is present only where [`ssf handover`](sessions.md#handover) put them there (`harness`, and `model` and `effort` where the handover named them; the same shape in `state.json` as `overrides` on the item). `handover_note` is what an earlier handover left for a session that has not read it yet (`from`, the harness it came from, and `summary_chars`), shown by `ssf status` and `ssf peers` as `handover note waiting:`. `handover` is a handover asked for and not yet carried out: `harness`, `harness_name` (for people), `model`, `effort`, `summary_chars`, `by` (the session that asked, absent for a person at a shell) and `requested_at`. `state.json` keeps the pending record itself as `handover` on the item, with the `summary` the outgoing session wrote in place of its length, the ids of the conversations a handover retired as `retired_session_ids` (never resumed or captured again), when the last handover happened as `handed_over_at`, and what the outgoing session left for the new one as `handover_note` (kept until a session has read it, so a harness that would not start does not take the summary with it) |
| `workspace_state`, `released_at` | on a retired item: `kept` (the workspace is still on disk), `released` (removed by `ssf release`/`ssf purge`, at `released_at`), `pending` (release accepted, removal on the next pass), `given-up` (kept after the daemon refused the agent's release three times) or `gone` (removed some other way) |
| `origin`, `posts_by_session`, `untagged_posts` | attribution (see [Identity and bylines](identity-and-bylines.md)): the session that opened the item, how many posts each session made on it, and how many bot posts carry no tag (the daemon's own `🤖 ssf` event posts count in neither) |
| `blocked` | set while the session cannot take prompts: its harness is at a login prompt (`reason` is `login`) or could not be started at all (`reason` is `start`, after a handover to a harness that will not run). `harness`, `harness_name` (for people), `detail` (what the screen or the driver said), `since`, `fix` (what a person does about it); `blocked_sessions` at the top lists the ids (see [A harness that is not signed in](sessions.md#a-harness-that-is-not-signed-in)) |
| `agent_state`, `last_assistant_message`, `tool`, `last_activity_at`, `column`, `branch`, `worktree_id`, `worktree_path`, `workspace` | the driver. `agent_state` is the driver's own (Orca: `working`, `waiting`, `done`, `open`; herdr: `idle`, `working`, `blocked`, `done`) or `no-agent`, `no-workspace`, `unbound`, `unknown` (driver not running); `workspace` is the raw workspace row |

`repos[].issues[]` carries the same objects, `repos[].allowed_users` says
who may drive each repository (`anyone_allowed` at the top is whether the
wildcard is on anywhere; the bar widget warns while it is), and the `orca`
key (`available`, `error`, `workspaces`, `down`) says whether the drivers
answered, whichever drivers are in use, for the bar widget's sake.
`ssf peers` prints the same data as a terminal table: by default the
active sessions on `$SSF_REPO` (so an agent sees who else is on its
repository, and itself marked "(you)"), or on every watched repository
outside a session; `--all` includes retired sessions. `ssf guide` tells
agents about it.

## Notes and limitations

- The bot identity is a default, not a security boundary. On bare metal
  the agents run as your Unix user inside your session, so a determined
  agent can still read your own gh token from the keyring or use your SSH
  agent. ssf tells agents to act only as the bot and to report missing
  permissions instead; the isolation is the [microVM](vm.md).
- Session resume (and therefore memory across relaunches) is implemented for
  Claude Code and Codex; other agents are restarted with the full issue
  context instead.
- One agent per issue; a second assignee is not coordinated with. A session
  owns what it opens only within its own repository.
- `ssf status` asks every driver in use for its workspace list on every call
  (a few hundred milliseconds); when a driver is not running the ssf side is
  still reported and its sessions' agent states show as unknown.
- The bar widget and menu entries are installed per user on first service
  start; `ssf ui uninstall` removes them, `ssf ui install` puts them back.
- Logs: `journalctl --user -fu ssf.service`, or on macOS, where the service
  is launchd's, `tail -f $(brew --prefix)/var/log/ssf.log` (inside the VM,
  `ssf vm logs`).
