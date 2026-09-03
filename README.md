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
   so pushes update the PR; if the bot's own issue workspace already holds
   that branch, the PR joins that agent instead,
3. sends the agent the issue, its description and everything that has happened
   on it so far, plus instructions on how to report back,
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
- **Manage repositories**: change the agent for a repository or stop watching it.
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
| `owner`, `subscribers`, `shares_workspace_of` | which session acts on the item (a PR that joined its issue's workspace is owned by that issue's session); `subscribers` is reserved for #3 |
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

## How it works

- **Polling, not webhooks.** Every `poll_interval_secs` ssf makes three
  listings per repository (assigned to the bot, mentioning the bot, review
  requested from the bot), using ETags so unchanged listings cost no rate
  limit. Only items whose `updated_at` moved get their timeline re-fetched.
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
  context. If the workspace itself is gone, ssf re-creates it from the old
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
| `repo.path` | | Register an existing checkout instead of cloning |
| `repo.clone_url` | `https://github.com/owner/name.git` | Use an SSH URL for private repositories |

Environment overrides: `SSF_GITHUB_TOKEN`, `SSF_CONFIG_DIR`, `SSF_STATE_DIR`,
`ORCA_CLI_COMMAND`, `RUST_LOG`.

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
- One agent per issue; a second assignee is not coordinated with.
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
