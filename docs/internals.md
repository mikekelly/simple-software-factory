# Under the hood

How the daemon polls, delivers, resumes and restarts, what `ssf status --json` carries, and the known limits. For whoever debugs the factory or builds on its status output; agents need none of it.

The details behind the README's [How it works](../README.md#how-it-works).

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
  even when the new worktree has a different path.
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
  `daemon.resume_on_start = false` turns it off. `ssf run --once` runs it
  too.
- **Retirement.** Closed or unassigned issues get one final message (push,
  final comment, then `ssf release` if everything is on origin) and are
  marked inactive; the workspace is marked completed in the driver and left
  in place. Removal is the agent's (`ssf release`) or a person's (`ssf
  purge`) to ask for, and is refused whenever the worktree holds anything
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
| `agent_session_id`, `prompts_sent`, `last_prompt_at`, `bound_at`, `retired_at`, `harness` | ssf's delivery record |
| `overrides`, `handover_pending` | set by [`ssf handover`](sessions.md#handover): `overrides` is what the item runs on instead of its repository's settings (`harness`, and `model` and `effort` where the handover named them; kept in `state.json` as `overrides` on the item), `handover_pending` while one has been asked for and the daemon has not carried it out yet. `state.json` keeps the pending record itself as `handover`: the target `harness`, `model` and `effort`, the `summary` the outgoing session wrote, `by` (the session that asked, absent for a person at a shell) and `requested_at` |
| `workspace_state`, `released_at` | on a retired item: `kept` (the workspace is still on disk), `released` (removed by `ssf release`/`ssf purge`, at `released_at`), `pending` (release accepted, removal on the next pass), `given-up` (kept after the daemon refused the agent's release three times) or `gone` (removed some other way) |
| `origin`, `posts_by_session`, `untagged_posts` | attribution (see [Identity and bylines](identity-and-bylines.md)): the session that opened the item, how many posts each session made on it, and how many bot posts carry no tag (the daemon's own `🤖 ssf` event posts count in neither) |
| `blocked` | set while the session cannot take prompts because its harness is at a login prompt: `reason` (`login`), `harness`, `harness_name` (for people), `detail` (what the screen said), `since`, `fix` (the command to run); `blocked_sessions` at the top lists the ids (see [A harness that is not signed in](sessions.md#a-harness-that-is-not-signed-in)) |
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
- Logs: `journalctl --user -fu ssf.service` (inside the VM, `ssf vm logs`).
