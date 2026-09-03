# Simple Software Factory (ssf)

Assign a GitHub issue or pull request to your bot account, @mention it, or
request its review, and an agent starts working on it in
[Orca](https://onorca.dev/). Comment and the agent hears about it. Close it
and the agent wraps up.

`ssf` is a small Rust daemon packaged for [Omarchy](https://omarchy.org/). It
watches the repositories you tell it about, and for every open issue or pull
request that involves the bot (assigned to it, @mentioning it, or requesting
its review) it:

1. makes sure Orca has a project for the repository (cloning it if needed),
2. creates an Orca workspace (git worktree) linked to the issue or PR and
   launches the agent you configured for that repository (Claude Code,
   Codex, ...). A pull request's workspace is checked out on the PR's branch,
   so pushes update the PR. An item a session opened itself (or a PR on a
   session's branch) gets no session of its own: it is routed to the agent
   that opened it (see [Ownership](#ownership-one-session-per-item)),
3. sends the agent the issue, its description, the project boards it is on
   and everything that has happened on it so far, plus instructions on how
   to report back and whatever the repository's own `SSF.md` says,
4. keeps polling the issue timeline and pastes new activity (comments, label
   changes, renames, linked PRs, ...) into the same agent session, which
   steers it if it is busy and wakes it up if it is idle,
5. brings the agent back if its terminal or even the whole workspace is gone,
   resuming the same conversation,
6. tells the agent to stop when the issue is closed or unassigned, marks the
   workspace completed, and (by default) removes it once the agent has
   wrapped up. If the issue comes back to life the workspace is re-created
   and the conversation resumed.

Everything the agent does lands in a normal Orca workspace, so you can watch,
take over, or nudge it from Orca at any time.

## Install

Orca is not in the Omarchy package repository yet, so install it first
(`orca-ide-bin`) and sign in. Then build and install ssf from this checkout:

```sh
cd packaging && makepkg -si
```

The package installs:

| Path | What |
|------|------|
| `/usr/bin/ssf` | the daemon and management CLI |
| `/usr/bin/ssf-ui` | Omarchy menu flows (sign in, add repo, ...) |
| `/usr/lib/systemd/user/ssf.service` | background service, enabled for every user via `graphical-session.target.wants` |
| `/usr/share/ssf/omarchy-plugin/` | the bar widget, copied into `~/.config/omarchy/plugins/ssf.factory` on first start |

The service starts with the graphical session, and the package's install
hook also starts it in any session that is running at install time, so there
is nothing to enable. (If it was installed with nobody logged in, the first
login starts it, or `systemctl --user start ssf.service` does.) On its first
run it installs the **Software Factory** bar widget (next to Omarchy's Agents
widget) and a **Factory** submenu in the Omarchy menu.

## Set up

Click the factory icon in the bar, or open the Omarchy menu and pick
**Factory**. From there:

- **Sign in bot account**: the bot is a GitHub account that the GitHub CLI
  knows. The flow lists the accounts `gh` already holds and offers "sign in
  another account in the browser", which runs gh's device flow (use a private
  window so GitHub does not reuse your own session); whoever signs in becomes
  the bot. ssf never stores the token: it reads it from gh's keyring when it
  needs it, and switches gh back to your own account afterwards. It then
  records the bot's commit identity (`login <id+login@users.noreply.github.com>`),
  generates a dedicated ed25519 key under `~/.config/ssf/keys/` and enrolls it
  on the bot account as both an SSH key and a commit signing key. If the gh
  token lacks the scopes for that (`repo`, `project`,
  `admin:public_key`, `admin:ssh_signing_key`), ssf asks gh to add them. `ssf auth logout`
  revokes the keys and forgets the bot; the gh sign-in itself stays.
- **Watch a repository**: type `owner/name` and pick the agent that works it.
  The agent list comes from Omarchy's agent catalogue and only shows agents
  that are installed.
- **Manage repositories**: change the agent, model or effort level for a
  repository, or stop watching it.
- The toggle in the panel header enables or disables the service.

Then assign an issue or PR to the bot on GitHub, @mention it, or request its
review. Within a poll interval (10 s by
default) a workspace shows up in Orca, and in the widget under "Sessions": one
row per agent session with the issue or PR (click the title for GitHub), its
GitHub state (open, closed, merged, draft), the agent's state (working, waiting,
idle, done), what it last said or the tool it is running, the branch and when
it was last active. Clicking a row opens the workspace in Orca. The bar icon
turns urgent while an agent is waiting for input.

The same can be done from a terminal:

```sh
ssf-ui login                      # menu-driven; or in a terminal:
ssf auth login                    # pick an account gh knows, or sign in another in the browser
ssf auth login --web              # straight to the browser flow; prints the URL and code,
                                  # so it also works over ssh (set BROWSER=true to stop gh opening one)
ssf auth login --user overlay-bot # an account gh already knows, no questions
printf '%s' "$TOKEN" | ssf auth login --token   # a pasted token instead of gh
ssf auth status
ssf agents                        # which agents Omarchy knows and which are installed
ssf repo add acme/widgets --harness claude
ssf status
ssf peers                         # the agent sessions and what each is doing
ssf doctor
```

## Management CLI (for humans and for agents)

Everything except the credentials is scriptable, so an agent on the machine
can reconfigure the factory:

```sh
ssf repo list --json
ssf repo add acme/widgets --harness codex --instructions "Run make test before opening a PR."
ssf repo set acme/widgets --harness claude --command "claude --dangerously-skip-permissions"
ssf repo set acme/widgets --model opus --effort high
ssf repo set acme/widgets --harness pi --model openrouter/anthropic/claude-sonnet-4 --effort high
ssf models pi                     # ids the installed agent takes
ssf repo set acme/widgets --clear model --clear effort
ssf repo set acme/widgets --prompt-file .github/ssf.md   # instead of SSF.md
ssf repo remove acme/widgets
ssf config get daemon.poll_interval_secs
ssf config set daemon.poll_interval_secs 60
ssf config set daemon.instructions "Always open PRs as drafts."
ssf status --json
ssf peers [--repo owner/name] [--all] [--json]
ssf ui service disable|enable|toggle|status
```

Config changes are picked up on the next poll; no restart needed. `ssf config
set` refuses to touch `github.token`; use `ssf auth login` (interactive) for
that.

`ssf status --json` joins what ssf knows about every tracked item with what
Orca reports about the workspace working on it (`orca worktree ps`), so
nothing else has to talk to Orca. Its `sessions` array has one entry per item:

| Field | From |
|-------|------|
| `id`, `repo`, `number`, `kind` (`issue`/`pull_request`), `title`, `url` | ssf; `id` is the session identity `owner/repo#N` |
| `github_state` (`open`/`closed`/`merged`), `active`, `triggers`, `pr` | GitHub, as of the last poll |
| `owner`, `subscribers`, `shares_workspace_of`, `delegated_by` | which session acts on the item: its own, or the session it is bound to (opened from it, or a PR on its branch); `delegated_by` names the session that handed the item off (`mode=delegate`); `subscribers` is reserved for #3 |
| `agent_session_id`, `prompts_sent`, `last_prompt_at`, `bound_at`, `retired_at`, `harness` | ssf's delivery record |
| `agent_state`, `last_assistant_message`, `tool`, `last_activity_at`, `column`, `branch`, `worktree_id`, `worktree_path`, `workspace` | Orca. `agent_state` is Orca's (`working`, `waiting`, `done`, `open`) or `no-agent`, `no-workspace`, `unbound`, `unknown` (Orca not running); `workspace` is the raw `worktree ps` row |

`repos[].issues[]` carries the same objects, and `orca.available` says
whether Orca answered. `ssf peers` prints the same data as a terminal table:
by default the active sessions on `$SSF_REPO` (so an agent sees who else is
on its repository, and itself marked "(you)"), or on every watched repository
outside a session; `--all` includes retired sessions. The initial prompt tells
agents about it.

## How the agent gets the bot's identity

Agents are started through `ssf launch`, which builds an environment in which
everything git and GitHub related is the bot, whatever the human's own
`~/.gitconfig`, `gh auth` or SSH agent say:

| What | How |
|------|-----|
| `gh` and the GitHub API | `GH_TOKEN`, `GITHUB_TOKEN` (read from gh's keyring for the bot account, or from a pasted token / `SSF_GITHUB_TOKEN`) |
| HTTPS pushes | a git credential helper (`ssf git-credential`) that answers with the token, placed ahead of any configured helper |
| SSH pushes | `GIT_SSH_COMMAND` pinned to the enrolled bot key with `IdentitiesOnly=yes` |
| Commit author and committer | `GIT_AUTHOR_*`, `GIT_COMMITTER_*` and `user.name`/`user.email` |
| Commit signing | `gpg.format=ssh`, `user.signingkey=<bot key>`, `commit.gpgsign=true` (or `commit.gpgsign=false` when no key is enrolled, so nothing is signed with the human's key) |
| Which issue this is | `SSF_REPO`, `SSF_ISSUE`, `SSF_ISSUE_URL`, `SSF_BOT` |
| Which session posted what | a `gh` shim first on `PATH` that stamps posts with an origin tag (below) |

Git settings go in through `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`, which
outrank every config file, and only inside the agent's process tree. The
initial prompt tells the agent that plain `gh` and `git push` act as the bot.
Because activity by the bot login is filtered out of follow-up messages, the
agent's own comments are not echoed back to it (`daemon.include_own_events`
turns that off). `ssf token` still prints the token for any other use.

## Project boards

If the issue or pull request is on any GitHub project (v2) boards, the initial
prompt lists them under a "Project boards" heading: each board's name and URL,
the card's current Status, the Status options the board offers, and the
`gh project item-edit` command (with the project, item, field and option ids
filled in) that changes it. The agent is told that keeping its card accurate
is part of the job and that which column fits is its own judgement from what
is actually happening. ssf itself never moves cards and prescribes no mapping
from events to columns; put any repository-specific conventions about columns
in the per-repository instructions. The lookup is one GraphQL query per
onboarding and delivery, using the bot token's `project` scope; if it fails
the prompt simply carries no boards section and the daemon logs why. Closed
boards are left out.

## Origin tags: which session posted what

GitHub shows the same bot account for every session, so ssf encodes the
session in the content. Everything an agent posts ends with an invisible
marker naming the item its workspace belongs to:

```
<!-- ssf: origin=owner/repo#N -->
```

`ssf launch` links `~/.config/ssf/bin/gh` to the ssf binary and puts that
directory first on the agent's `PATH`. Invoked as `gh`, ssf appends the tag
to the body of `issue create`, `issue comment`, `pr create`, `pr comment`
and `pr review` (whether given as `--body`, `--body=`, `-b`, `--body-file`
or `-F -`; a review without a body gets one that is only the tag) and execs
the real gh with everything else untouched. The shim reads only its
environment, writes nothing and leaves stdin and the terminal alone, so it
works inside read-only sandboxes and does not break gh's interactive flows.
Outside a session (no `SSF_ISSUE`) it is a plain pass-through. Bodies that
already carry the tag are not stamped twice, and the initial prompt asks the
agent to add the tag itself whenever it posts some other way (`gh api`,
`gh pr create --fill`, a harness that resets `PATH`).

The daemon parses tags out of every item body and comment it reads. In
`ssf status --json` each tracked item shows `origin` (the session that opened
it, for PRs and issues an agent created), `origins` (timeline event key to
session, for tagged comments and reviews) and `untagged` (posts by the bot
that carry no tag, meaning the shim was not in effect where they were made).
Untagged bot posts are also warned about in the logs and reported by
`ssf doctor`, which additionally checks that the real gh is installed and
that the shim links to the running ssf. When comments are shown to an
agent, the tag is stripped and replaced by "(from session owner/repo#N)".

The tag can carry more fields. The one defined so far is `mode=delegate`,
which the shim adds when an `issue create` or `pr create` assigns the bot
itself (`--assignee <bot>` or `@me`): the item is a hand-off rather than the
session's own (below).

## Ownership: one session per item

GitHub has one bot identity, so without help a human's @mention or review
request on a pull request the bot opened would start a second agent next
to the one that wrote it. ssf instead binds every item to at most one
owning session, decided once when the item is first discovered (first
binding wins):

- **Opened by a session.** An issue or PR whose body carries a session's
  origin tag belongs to that session. The bot's own open items are polled
  (a fourth listing, `creator=<bot>`), so a session hears about the PR it
  opened without anyone assigning or mentioning the bot: it gets one
  message saying the item is now tracked for it, then every later comment,
  review, review request, assignment and the closure, all into the same
  agent, with `SSF_ISSUE` unchanged. The agent's own posts on it are
  filtered out as usual.
- **A PR on a session's branch.** A same-repo pull request whose head branch
  is the branch of a tracked workspace belongs to that workspace's session,
  tag or no tag (this is what a PR opened by hand from an agent's branch, or
  with `gh pr create --fill`, falls back to).
- **Triggers go to the owner.** Assigning, mentioning or requesting a review
  from the bot on an owned item is delivered to the owner's agent as
  activity, never to a new session. If the owner has been retired or its
  workspace removed, it is brought back the way any lost session is
  (workspace re-created from its branch, conversation resumed), rather than
  replaced. A retired owner's workspace is not cleaned up while items bound
  to it are still open.
- **Hand-offs.** An item a session creates *and assigns the bot to* in the
  same `gh ... create` command is a delegation: the tag carries
  `mode=delegate`, the item gets a fresh session of its own, and the creating
  session hears nothing more about it until it is closed or merged, when it
  gets a single message with the outcome and the child's final comment (the
  last comment the bot left on it). The child is told it was handed off and
  to leave a clear final comment. The initial prompt explains this rule to
  agents, so an agent that wants a separate worker uses `--assignee`, and
  one that wants to keep an item simply opens it.
- **Nothing to bind to.** A bot-opened item with no usable tag, no branch
  match and no human trigger is left alone (logged once) rather than given
  a session nobody asked for; assigning or mentioning the bot on it later
  starts one as usual. Items opened from a session on a *different*
  repository are not bound across repositories.

`ssf status --json` shows the binding as `owner` / `shares_workspace_of`
and hand-offs as `delegated_by`; `ssf peers` prints them as "owned by ..."
and "handed off by ...".

## How it works

- **Polling, not webhooks.** Every `poll_interval_secs` ssf makes four
  listings per repository (assigned to the bot, mentioning the bot, review
  requested from the bot, opened by the bot), using ETags so unchanged
  listings cost no rate limit. Only items whose `updated_at` moved get their
  timeline re-fetched.
- **Pull requests.** Review comments, reviews, force-pushes and merges are
  rendered like issue activity. A PR from a fork gets a workspace on the base
  branch and the agent is told it cannot push to the fork.
- **One workspace per issue.** The binding lives in
  `~/.local/state/ssf/state.json` and is also recoverable from Orca (the
  worktree is linked to the issue number), so a lost state file re-attaches
  instead of creating a second workspace.
- **Delivery into a live TUI.** Messages are pasted with bracketed paste so
  multi-line text arrives as one message, then Enter. Claude Code queues it as
  a steering message while busy, or runs it when idle.
- **Rehydration.** After the first message ssf records the harness's
  conversation id (Claude Code and Codex keep transcripts on disk). If the
  agent's terminal is gone, ssf starts it again with `--resume <id>` (Codex:
  `codex resume <id>`) and sends only the new events; if resuming fails or the
  harness has no resume support, it starts fresh and resends the whole issue
  context (the session's own item, even when what triggered the relaunch was
  activity on an item it owns). If the workspace itself is gone, ssf re-creates it from the old
  branch (local or `origin/`) and does the same. Claude Code resumes a session
  from any directory, so this works even when the new worktree has a
  different path.
- **First-run dialogs.** Claude Code asks whether to trust a new folder; ssf
  answers it so unattended launches do not stall.
- **Retirement and cleanup.** Closed or unassigned issues get one final
  message and are marked inactive. For closed issues, once the agent is idle
  (or after `daemon.cleanup_grace_secs`), the workspace is removed
  (`daemon.cleanup_on_close`, default on). Work that was pushed survives on
  the remote branch; re-assigning or reopening the issue re-creates the
  workspace and resumes the conversation.

## Configuration

`~/.config/ssf/config.toml` (see `config.example.toml` for every key):

```toml
[daemon]
poll_interval_secs = 30

[[repo]]
name = "acme/widgets"
harness = "claude"
model = "opus"
effort = "high"
instructions = "Run `make test` before opening a PR."
```

| Key | Default | Meaning |
|-----|---------|---------|
| `github.api_url` | `https://api.github.com` | GitHub Enterprise: `https://ghe.example.com/api/v3` |
| `github.login`, `github.email`, `github.ssh_key_path` | set by `ssf auth login` | The bot's identity and key; edit `email` if the bot has a public address |
| `orca.command` | `/usr/lib/orca-ide/bin/orca-ide` | Orca CLI binary (`/usr/bin/orca-ide` launches the app, not the CLI) |
| `orca.projects_dir` | `~/orca/projects` | Where repositories are cloned when Orca has no project for them |
| `daemon.poll_interval_secs` | `10` | GitHub poll interval (unchanged listings are free conditional requests) |
| `daemon.instructions` | | Extra instructions appended to every initial prompt |
| `daemon.cleanup_on_close` | `true` | Remove the workspace after the issue is closed and the agent has wrapped up |
| `daemon.cleanup_grace_secs` | `900` | How long to let the agent wrap up before removing the workspace anyway |
| `repo.harness` | | Agent id (`claude`, `codex`, `omp`, `pi`, `opencode`, `gemini`, `copilot`, `grok`, `crush`) |
| `repo.command` | the harness id | Command that starts the agent, e.g. `claude --dangerously-skip-permissions` |
| `repo.model` | the agent's default | Model: an Orca model id, or the agent's own `provider/model` (`ssf models <agent>` lists them; other ids pass through) |
| `repo.effort` | the agent's default | Effort or thinking level (`ssf agents --json` lists what each agent accepts) |
| `repo.path` | | Register an existing checkout instead of cloning |
| `repo.prompt_file` | `SSF.md` | The per-project prompt file (below), relative to the worktree unless absolute or `~/` |
| `repo.clone_url` | `https://github.com/owner/name.git` | Use an SSH URL for private repositories |

Environment overrides: `SSF_GITHUB_TOKEN`, `SSF_CONFIG_DIR`, `SSF_STATE_DIR`,
`ORCA_CLI_COMMAND`, `RUST_LOG`.

### The per-project prompt file

Notes that only matter to ssf agents, and so do not belong in `CLAUDE.md` or
`AGENTS.md` (which conventions the project boards use, who to ask about what,
how the humans want PRs written up, ...), go in an `SSF.md` at the root of
the repository. When an agent is started for an item, ssf reads the file from
the item's own checkout (so a PR branch that changes it is seen with its own
version) and appends it to the initial prompt under a "Project notes" heading,
after `daemon.instructions` and `repo.instructions`. The same text is included
when a harness is started again from scratch. No file, or an empty one, adds
nothing. `repo.prompt_file` names another file: a path inside the worktree
(`.github/ssf.md`), or an absolute or `~/` path for notes you would rather
not commit.

### Models and effort levels

For the agents Orca has a model catalogue for, `repo.model` and `repo.effort`
use the same identifiers as Orca's own `--model`/`--effort` options (`orca
orchestration worker-start`). Pi, Oh My Pi, OpenCode and Copilot are not in
Orca's catalogue; they take their own `provider/model` ids (Pi and Oh My Pi
reach many providers, OpenRouter among them) and their own thinking or
reasoning levels. Either way ssf turns the setting into the agent's
command-line flags when it starts the agent, including when it resumes a
session:

| Agent | Model ids | Effort levels | What is appended to the command |
|-------|-----------|---------------|---------------------------------|
| `claude` | `fable`, `opus`, `sonnet`, `haiku`, or a full model name | `low`, `medium`, `high`, `xhigh`, `max` | `--model <id> --effort <level>` |
| `codex` | `gpt-5.5`, `gpt-5.2-codex`, ... | `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra` | `-m <id> -c model_reasoning_effort=<level>` |
| `gemini` | `gemini-3-pro-preview`, `gemini-2.5-pro`, ... | none | `-m <id>` |
| `grok` | `grok-4.6`, `grok-4.5` | `low`, `medium`, `high`, `xhigh` | `-m <id> --reasoning-effort <level>` |
| `pi` | `provider/model` as in `pi --list-models`, e.g. `openrouter/anthropic/claude-sonnet-4` | `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` | `--model <id> --thinking <level>` |
| `omp` | `provider/model` as in `omp models`, e.g. `openai-codex/gpt-5.4` | as `pi`, plus `auto` | `--model <id> --thinking <level>` |
| `opencode` | `provider/model` as in `opencode models` | none | `-m <id>` |
| `copilot` | `auto` or a model name | `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` | `--model <id> --effort <level>` |

`ssf models <agent>` prints the ids to choose from, asking the installed
agent for its list where it has one (`pi`, `omp`, `opencode`); the menu's
*Change model* picker uses the same list. Crush has no model flag for its
TUI, so ssf refuses a model for it. Model ids are passed through as given, so
a model the list does not mention works as long as the agent knows it;
effort levels must be ones the agent accepts. Changing the agent of a
repository resets both, since the ids belong to the agent. Keep
`--model`/`--effort` out of `repo.command` when you set them here, or the
agent sees the flag twice.

## Notes and limitations (v1)

- The bot identity is a default, not a security boundary. Agents run as your
  Unix user inside your session, so a determined agent can still read your own
  gh token from the keyring or use your SSH agent. ssf tells agents to act
  only as the bot and to report missing permissions instead; real isolation
  would need a sandbox (bubblewrap without the session bus) or a dedicated
  Unix user for the factory.

- Session resume (and therefore memory across relaunches) is implemented for
  Claude Code and Codex; other harnesses are restarted with the full issue
  context instead.
- One agent per issue; a second assignee is not coordinated with. A session
  owns what it opens only within its own repository.
- `ssf status` asks Orca for the workspace list on every call (a few hundred
  milliseconds); when Orca is not running the ssf side is still reported and
  agent states show as unknown.
- The bar widget and menu entries are installed per user on first service
  start; `ssf ui uninstall` removes them, `ssf ui install` puts them back.
- Logs: `journalctl --user -fu ssf.service`.

## Development

```sh
cargo build && cargo test
SSF_CONFIG_DIR=/tmp/ssf-dev SSF_STATE_DIR=/tmp/ssf-dev SSF_GITHUB_TOKEN=$(gh auth token) \
  ./target/debug/ssf repo add you/sandbox --harness claude
SSF_CONFIG_DIR=/tmp/ssf-dev SSF_STATE_DIR=/tmp/ssf-dev SSF_GITHUB_TOKEN=$(gh auth token) \
  ./target/debug/ssf run --once     # agents launched by this run read the same SSF_* locations
SSF_PLUGIN_DIR=$PWD/omarchy-plugin ./target/debug/ssf ui install   # live-test the widget
omarchy plugin validate ./omarchy-plugin
```

Layout: `src/github.rs` (REST client), `src/orca.rs` (Orca CLI wrapper),
`src/prompt.rs` (timeline rendering and prompt templates), `src/engine.rs`
(reconciliation loop), `src/sessions.rs` (harness session capture and resume),
`src/origin.rs` (origin tags), `src/shim.rs` (the `gh` shim),
`src/status.rs` (the joined item/session view behind `status`, `peers` and the
widget), `src/ui.rs` (Omarchy integration),
`omarchy-plugin/` (Quickshell bar widget), `bin/ssf-ui` (menu flows),
`packaging/` (PKGBUILD, systemd unit, pacman install script).
