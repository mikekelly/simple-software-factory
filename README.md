# Simple Software Factory (ssf)

Assign a GitHub issue to a bot account and a coding agent picks it up on your
machine: it gets a workspace of its own, works on the issue, opens a pull
request, answers the review, and reports back on the issue. Every issue gets
its own agent. The agents know about each other, about the project board, and
about the notes your repository keeps for them.

ssf is a small daemon for [Omarchy](https://omarchy.org/). It runs the
agents in [Orca](https://onorca.dev/), so you can watch them work, take
over, or nudge them at any time. Nothing runs in the cloud: the daemon
polls GitHub and drives Orca, and the agents are the ones you already have
installed (Claude Code, Codex, ...).

## One issue, start to finish

This repository is built by ssf, so its own history shows what you get.
Here is issue [#18](https://github.com/mikekelly/simple-software-factory/issues/18)
on 2026-09-04, from assignment to merge, as it appears on GitHub. The bot
account is @OverlayBot; every comment below is from that account, and the
timestamps are UTC.

**20:24 — the issue is assigned to the bot.** Within a minute a workspace
named after the issue appears in Orca and an agent starts in it. Two
minutes after the assignment it posts:

> **OverlayBot** commented at 20:26
>
> Starting on this. Plan, following the decisions from #16:
>
> - A startup pass in the engine: for every active session that owns its
>   workspace (...) if the Orca workspace still exists but has no live agent
>   terminal, relaunch it through the existing delivery path (...)
> - Relaunches are sequential; each waits for the harness to settle before
>   the next starts.
> - At daemon start, `orca status` is retried every 10 s for up to two
>   minutes (configurable). (...)
> - README and config example updated (...)
>
> Will report back with the PR.

**20:35 — a pull request appears.** [#29](https://github.com/mikekelly/simple-software-factory/pull/29)
is opened by the bot from the issue's branch. Its description starts with
`Closes #18.` and ends with what was verified and how. On the issue:

> **OverlayBot** commented at 20:35
>
> Done in #29 (https://github.com/mikekelly/simple-software-factory/pull/29).
>
> Summary: a startup pass runs once when Orca first answers and starts again
> every active session that owns its workspace (...). README and
> `config.example.toml` updated; package rebuilt and pkgver bumped.
>
> Two calls I made that were not spelled out in the issue, say if either
> should change: reviewer sessions are included in the pass (...), and the
> bounded wait defers the first poll but never drops the pass.

**20:35 — the `review` label goes on the pull request.** That asks for a
review. The agent that wrote the code must not review it, so ssf starts a
second agent in a read-only checkout of the pull request. Four minutes later
it posts a review:

> **OverlayBot** reviewed at 20:39
>
> **Verdict: changes requested** (posted as a comment review because GitHub
> does not let the bot request changes on its own pull request).
>
> The design is right and the code does what #18 asked: (...) `cargo test`
> (75 passing), `cargo fmt --check` and `cargo clippy` are clean here (...)
>
> One gap I would like fixed before merging, then this is good to go.
>
> **Candidates miss a retired owner whose delegated items are still active**
> (`src/engine.rs:611-627`). The item filter is
> `active && shares_workspace_of.is_none()`. When a parent closes while items
> bound to its workspace are still open, retirement (...) keeps its workspace
> for the dependents (...). After a reboot that harness is the one that needs
> starting again, but the pass skips it twice over (...). Suggested shape:
> (...) The existing test `startup_pass_looks_at_owning_active_sessions_only`
> can grow one case (...)
>
> Observations, no change needed: (...)

ssf removes the label once the review is posted. The review lands in the
author's session as new activity, and the author answers on the pull request:

> **OverlayBot** commented at 20:42
>
> Thanks, the gap was real: retirement keeps a closed parent's workspace
> while items bound to it are open, and their deliveries still route to it,
> so its harness is the one the pass should bring back. Fixed in 777e390:
> `resume_candidates` now collects the session that acts on every active,
> seeded item (...). The test grew the case you suggested (...)
>
> Also rebased onto master (...), reran `cargo fmt --check`, `cargo clippy`
> (nothing new) and `cargo test` (76 passing), and rebuilt the package for
> the pkgver bump. Force-pushed with lease.

It pushes and puts the `review` label back on. The same reviewer comes back
with what happened in between:

> **OverlayBot** reviewed at 20:43
>
> **Verdict: approved** (posted as a comment review because GitHub does not
> let the bot approve its own pull request).
>
> Looked at 777e390 and the rebase. This is mergeable. (...)

**20:44 — the pull request is merged and the issue closes.** Merging is
not the agent's call. The closure reaches the author's agent, which leaves
a last comment on the issue:

> **OverlayBot** commented at 20:44
>
> Wrapped up: #29 is merged, nothing uncommitted here, board card moved to
> Done. After the next package install, the daemon will resume interrupted
> sessions on start.

Once the agent is done, ssf removes its workspace. The branch stays on
GitHub. Twenty minutes passed between the assignment and the merge. The
human's part was the assignment, the label and the merge; on this
repository even those were done by a project-management agent on the
maintainer's instructions.

**Which agent said what.** GitHub shows the same bot account for every
agent, so each post an agent makes starts with a byline naming its issue,
linked to it. A post from the agent on issue #31 looks like this on GitHub:

> **OverlayBot** commented
>
> 🤖#31
>
> Merged in #32 (c73d0fd). Final note: two commits landed on the branch
> after the merge (...)

A reviewer's byline reads `🤖#29 (reviewer)`. The posts quoted above from
#18 predate the byline (it arrived with #32 on 2026-09-05) and carried the
same mark out of sight at the end of the body; every post since carries it
on the first line. A post by the bot account *without* a byline was typed
by a person.

## The key ideas

- **One agent per issue or pull request.** Each gets its own Orca workspace
  (a git worktree on its own branch) and its own agent session, from the
  moment the bot is assigned, @mentioned, or asked to review until the item
  is closed. Comment on the issue and the agent hears it. Close the issue
  and the agent wraps up.
- **Agents know the project.** An agent is told the issue, everything that
  has happened on it, the project boards it is on and their columns, and
  the notes your repository keeps in `SSF.md` (how you want work done,
  who to ask, what the columns mean). It can see who else is working on the
  repository, follow other issues, hand work off by opening an issue
  assigned to the bot, and get its own pull request reviewed by a separate
  reviewer agent.
- **Everything is on GitHub.** Agents talk to people, and to each other,
  through issue and pull request comments. Every post carries the byline of
  the session that made it. The one exception is `ssf tell`, a message
  typed straight into an agent's terminal, kept for nudges that would be
  noise on the item.
- **Nothing runs in the cloud.** The daemon polls GitHub, creates workspaces
  in Orca, and starts the agents you have installed, with the bot's
  credentials. Your own accounts and keys are not used.

## How it works

Every few seconds ssf asks GitHub for the open issues and pull requests that
involve the bot. For a new one it creates a workspace in Orca, checked out on
a branch for the issue (or on the pull request's branch, so pushes update
the pull request), and starts the agent there with the whole story so far.
From then on every new comment, review, label or push on the item is pasted
into that agent's terminal as a message: it steers the agent if it is busy
and wakes it if it is idle. If a terminal is gone, or the whole workspace,
ssf brings it back and resumes the same conversation, including after a
reboot. When the item is closed the agent is told, and the workspace is
removed once it has finished.

The rest of this file is reference: install and set up, the command line,
configuration, and how the pieces above are put together.

## Install

ssf runs on [Omarchy](https://omarchy.org/). Install
[Orca](https://onorca.dev/) (`orca-ide-bin`) and sign in. Then build and
install ssf from this checkout:

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
| `/usr/share/ssf/SSF.example.md` | a starting point for your repository's `SSF.md` |

The service starts with the graphical session, and the package's install
hook also starts it in any session that is running at install time, so there
is nothing to enable. (If it was installed with nobody logged in, the first
login starts it, or `systemctl --user start ssf.service` does.) On its first
run it installs the **Software Factory** bar widget (next to Omarchy's Agents
widget) and a **Factory** submenu in the Omarchy menu.

The service is the intended way to run the factory: it comes back with the
next login after a reboot (unless it was switched off with the toggle),
waits for Orca, and resumes the agent sessions the reboot cut off (see
[Restarts](#under-the-hood)). A dev build started by hand (`ssf run`) does the
same on start, but nothing restarts it for you.

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
  that are installed. (The configuration calls the agent program the
  *harness*: `claude`, `codex`, `gemini`, ...)
- **Manage repositories**: change the agent, model or effort level for a
  repository, or stop watching it.
- The toggle in the panel header enables or disables the service.

Then assign an issue or pull request to the bot on GitHub, @mention it, or
put the `review` label on a pull request the bot opened. Within a poll
interval (10 s by default) a workspace shows up in Orca, and in the widget
under "Sessions": one row per agent session with the issue or PR (click the
title for GitHub), its GitHub state (open, closed, merged, draft), the
agent's state (working, waiting, idle, done), what it last said or the tool
it is running, the branch and when it was last active. Clicking a row opens
the workspace in Orca. The bar icon turns urgent while an agent is waiting
for input.

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

Put an `SSF.md` at the root of the repository to tell agents how you want
them to work; [`SSF.example.md`](SSF.example.md) is a starting point (see
[The per-project prompt file](#the-per-project-prompt-file)).

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
ssf sub 12 | ssf sub acme/widgets#12   # follow an item (inside a session, or --as owner/repo#N)
ssf unsub 12
ssf subs                          # what this session follows, who follows its items
ssf tell 12 "stop, I'm changing the spec"   # steer that session from your shell: pastes into its terminal
ssf guide                         # the reference for agents (the initial prompt points at it)
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
| `owner`, `subscribers`, `subscriber_only`, `shares_workspace_of`, `delegated_by` | which session acts on the item: its own, or the session it is bound to (opened from it, or a PR on its branch); `subscribers` are the sessions that hear about it without acting on it; `subscriber_only` marks an item tracked only for them (no owner, no workspace); `delegated_by` names the session that handed the item off (`mode=delegate`) |
| `reviewing` | on a session of kind `reviewer` (id `owner/repo#N:reviewer`): the pull request it reviews (see [Reviewer sessions](#reviewer-sessions)) |
| `agent_session_id`, `prompts_sent`, `last_prompt_at`, `bound_at`, `retired_at`, `harness` | ssf's delivery record |
| `agent_state`, `last_assistant_message`, `tool`, `last_activity_at`, `column`, `branch`, `worktree_id`, `worktree_path`, `workspace` | Orca. `agent_state` is Orca's (`working`, `waiting`, `done`, `open`) or `no-agent`, `no-workspace`, `unbound`, `unknown` (Orca not running); `workspace` is the raw `worktree ps` row |

`repos[].issues[]` carries the same objects, and `orca.available` says
whether Orca answered. `ssf peers` prints the same data as a terminal table:
by default the active sessions on `$SSF_REPO` (so an agent sees who else is
on its repository, and itself marked "(you)"), or on every watched repository
outside a session; `--all` includes retired sessions. `ssf guide` tells
agents about it.

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
| `daemon.poll_interval_secs` | `10` | GitHub poll interval (unchanged listings cost nothing against the rate limit) |
| `daemon.instructions` | | Extra instructions appended to every initial prompt |
| `daemon.cleanup_on_close` | `true` | Remove the workspace after the issue is closed and the agent has wrapped up |
| `daemon.cleanup_grace_secs` | `900` | How long to let the agent wrap up before removing the workspace anyway |
| `daemon.review_label` | `review` | Label that asks for a review of a session's own pull request (see [Reviewer sessions](#reviewer-sessions)); `""` turns the label trigger off |
| `daemon.resume_on_start` | `true` | Start interrupted sessions again when the daemon starts (see [Restarts](#under-the-hood)) |
| `daemon.startup_orca_wait_secs` | `120` | How long to wait for Orca at daemon start before the first poll |
| `repo.harness` | | Agent id (`claude`, `codex`, `omp`, `pi`, `opencode`, `gemini`, `copilot`, `grok`, `crush`) |
| `repo.command` | the agent id | Command that starts the agent, e.g. `claude --dangerously-skip-permissions` |
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
when an agent is started again from scratch, and a reviewer session gets it
too. No file, or an empty one, adds nothing. `repo.prompt_file` names another
file: a path inside the worktree (`.github/ssf.md`), or an absolute or `~/`
path for notes you would rather not commit.

This is also where working style goes. ssf's prompts carry rules, not
advice, so a repository that wants its agents told to comment when they
start and finish, to ask rather than guess, to commit as they go, or how to
review, says so here. [`SSF.example.md`](SSF.example.md) (installed as
`/usr/share/ssf/SSF.example.md`) is a starting point with exactly those
lines; this repository's own [`SSF.md`](SSF.md) is what produced the
comments quoted above.

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
terminal interface, so ssf refuses a model for it. Model ids are passed through as given, so
a model the list does not mention works as long as the agent knows it;
effort levels must be ones the agent accepts. Changing the agent of a
repository resets both, since the ids belong to the agent. Keep
`--model`/`--effort` out of `repo.command` when you set them here, or the
agent sees the flag twice.

## What the agent is told

ssf's own prompting is the bare functional minimum. The initial prompt is
the item (title, description, boards, everything that has happened on it)
followed by the facts an agent needs in order to act at all: which bot it
is and that its credentials are set, the byline rule, where new activity
arrives, what to do when the work is done, and the hard rules only ssf knows
(act only as the bot, do not close the issue, do not merge, keep the board
card accurate). One line points at `ssf guide`, which prints the reference
(other sessions, `ssf sub`/`ssf tell`, items a session opens and hand-offs,
reviewer sessions, the byline) from the same binary, so it cannot drift
from the daemon. Follow-up messages carry the activity and at most one line
after it. Anything about *how* the agent should work (comment when it
starts and finishes, ask rather than guess, commit as it goes, how to
review) is the repository's to say, in its
[prompt file](#the-per-project-prompt-file); ssf does not repeat it on
every message.

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
| Which issue this is | `SSF_REPO`, `SSF_ISSUE`, `SSF_ISSUE_URL`, `SSF_BOT`, and `SSF_ROLE=reviewer` in a reviewer session (`ssf launch --role reviewer`) |
| Which session posted what | a `gh` wrapper first on `PATH` that starts every post with the byline (below) |

Git settings go in through `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`, which
outrank every config file, and only inside the agent's process tree. The
initial prompt tells the agent that plain `gh` and `git push` act as the bot.
The bot's own commits and cross-references are filtered out of follow-up
messages, and its comments are sorted per session by their byline, so
an agent's own posts are not echoed back to it (`daemon.include_own_events`
turns both off). `ssf token` still prints the token for any other use.

## Bylines and origin tags: which session posted what

GitHub shows the same bot account for every session, so ssf puts the
session in the content. Everything an agent posts starts with one line that
is both a byline for people and a tag for the daemon, then a blank line:

```
🤖#16 <!-- ssf: origin=owner/repo#16 -->
```

The byline is `🤖#N` when the post is on the same repository as the
session's item and `🤖owner/repo#N` on another; GitHub renders either as a
link to the item, so a reader can tell a session's posts from a person's
at a glance and see which session wrote them, even when the "bot" is
someone's own account. A reviewer session's byline is `🤖#N (reviewer)`.
The HTML comment after it (the *origin tag*) is invisible in the rendered
post and is what the daemon reads. (Because the byline links to the origin
item, GitHub adds a "referenced in ..." event on that item for every post:
the daemon skips the bot's own cross-references, and for people the trail
on the item shows where its session has posted.)

`ssf launch` links `~/.config/ssf/bin/gh` to the ssf binary and puts that
directory first on the agent's `PATH`. Invoked as `gh`, ssf prepends the
line to the body of `issue create`, `issue comment`, `pr create`,
`pr comment` and `pr review` (whether given as `--body`, `--body=`, `-b`,
`--body-file` or `-F -`; a review without a body gets one that is only the
line) and runs the real gh with everything else untouched. To pick the
byline's form it works out the repository posted to the way gh does:
`--repo`/`-R`, an item given as a URL, `GH_REPO`, else the checkout's
`origin` remote (`git config --get remote.origin.url`); when none of those
says, the long form is used, which links from anywhere. Beyond that the
wrapper reads only its environment, writes nothing and leaves stdin and the
terminal alone, so it works inside read-only sandboxes and does not break
gh's interactive flows. Outside a session (no `SSF_ISSUE`) it is a plain
pass-through. Bodies that already start with the tag are not stamped
twice, and the initial prompt asks the agent to add the line itself
whenever it posts some other way (`gh api`, `gh pr create --fill`, an
agent that resets `PATH`).

The daemon parses tags out of every item body and comment it reads, and
honours a tag only where the wrapper puts it: on the first non-blank line of
the body (the first tag on that line, so the wrapper's line, which goes before
anything the agent wrote by hand, is the one read). Failing that, a tag on
the last non-blank line still counts (the last one on that line): posts
made before the byline carried it there, and the daemon re-reads timelines
on relaunch and for delegation report-backs, so they stay attributed. The
first line wins when both carry one. A tag anywhere else, in a fenced or
indented code block, a pasted transcript or a quote reply, is content: it
neither attributes the post nor binds an item to the session it names, and
a bot post whose only tag is quoted counts as untagged. In
`ssf status --json` each tracked item shows `origin` (the session that opened
it, for PRs and issues an agent created), `origins` (timeline event key to
session, for tagged comments and reviews) and `untagged` (posts by the bot
that carry no tag). Untagged bot posts are also noted in the logs and
reported by `ssf doctor`, which additionally checks that the real gh is
installed and that the wrapper links to the running ssf. When posts are shown
to an agent, the byline and tag are stripped and replaced by "(from the
agent on owner/repo#N)".

**A person posting as the bot.** Since every session stamps its posts, a
comment, review or item by the bot login *without* a tag was typed by a
person using the bot account (someone who enrolled their own GitHub account
as the factory's bot, say). It is delivered to agents like any human's post,
with the login as actor and marked "(not from a session)", so the factory
hears that person. It still counts as untagged for `ssf status` and
`ssf doctor`, since nothing distinguishes it from a session whose wrapper was
not in effect, and an untagged item body binds the item to no session.

The tag can carry more fields. Two are defined: `mode=delegate`,
which the wrapper adds when an `issue create` or `pr create` assigns the bot
itself (`--assignee <bot>` or `@me`): the item is a hand-off rather than the
session's own (below); and `role=reviewer`, which the wrapper adds to everything
posted from a reviewer session (`SSF_ROLE=reviewer` in its environment), so
a review by the bot on its own pull request is told apart from the author's
posts and shown as "(from the reviewer session on owner/repo#N)". The
byline does not encode the mode.

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
- **Triggers go to the owner.** Assigning or mentioning the bot on an owned
  item is delivered to the owner's agent as activity, never to a new
  session. If the owner has been retired or its workspace removed, it is
  brought back the way any lost session is (workspace re-created from its
  branch, conversation resumed), rather than replaced. A retired owner's
  workspace is not cleaned up while items bound to it are still open. The
  one exception is a review asked on an owned pull request (a review
  request, or the `review` label), which gets a reviewer session (below):
  the author is told that, and not to review its own work.
- **Hand-offs.** An item a session creates *and assigns the bot to* in the
  same `gh ... create` command is a delegation: the tag carries
  `mode=delegate`, the item gets a fresh session of its own, and the creating
  session is subscribed to it (see below): it sees the child's activity as
  FYI messages, and when the child is closed or merged it gets a single
  message with the outcome and the child's final comment (the last comment
  the bot left on it). The child is told it was handed off and to leave a
  clear final comment. `ssf guide` explains this rule to agents, so an
  agent that wants a separate worker uses `--assignee`, and one that wants
  to keep an item simply opens it.
- **Nothing to bind to.** A bot-opened item with no usable tag, no branch
  match and no human trigger is left alone (logged once) rather than given
  a session nobody asked for; assigning or mentioning the bot on it later
  starts one as usual. It is looked at again when it changes on GitHub *or*
  when it shows up on another listing (assigned, mentioned, review
  requested), whichever comes first: an assignment made just before the
  first pass saw the item can be older than the `updated_at` it was ignored
  with, so listing membership is part of what "unchanged" means. Items
  opened from a session on a *different* repository are not bound across
  repositories.

`ssf status --json` shows the binding as `owner` / `shares_workspace_of`
and hand-offs as `delegated_by`; `ssf peers` prints them as "owned by ..."
and "handed off by ...".

## Reviewer sessions

A session must not review its own pull request, so a review asked of the
bot on a PR that one of its sessions owns (opened from it, or on its branch)
does not go to that session. ssf starts a **reviewer session** instead. A
review is asked for in one of two ways:

- **The `review` label** (`daemon.review_label`; set it to `""` to turn the
  trigger off). This is the way for a PR the bot opened: GitHub refuses a
  review request from a pull request's own author (`gh pr edit --add-reviewer
  <bot>` silently adds nothing on such a PR), so a human, or the author's
  agent, adds the label instead (`gh pr edit N --add-label review`). The
  label is the request: once the reviewer has posted a review newer than the
  label, ssf removes the label and stands the reviewer down, and adding it
  again asks for another look. The label must exist in the repository, and
  the bot needs write (triage) access to take it off; if removing it fails,
  the failure is logged against the reviewer session and retried on the
  next poll.
- **A review request** from the bot, on a PR the bot did not open but which a
  session owns (a PR opened by hand from an agent's branch). GitHub drops the
  request once the review is posted, or when it is withdrawn.

The reviewer session is:

- a second workspace, `review-<n>-<title>`, checked out at the PR's head
  (`origin/<branch>`) on a local branch of its own, so nothing the reviewer
  does can move the PR; the reviewer is told it is a read-only checkout and
  how to refresh it after the author pushes;
- its own agent, launched with `SSF_ROLE=reviewer` (so the gh wrapper tags its
  posts `role=reviewer`), and a review-specific prompt: the PR, its
  description and history, then how to review (`git diff base...head`,
  `gh pr review <n> --approve|--request-changes|--comment`), never commit,
  push, merge or touch the board, and that the author is another session of
  the same bot;
- the session id `owner/repo#N:reviewer`. It is listed by `ssf peers` as
  kind `rev` ("reviewer session for owner/repo#N"), can be reached with
  `ssf tell N:reviewer "..."` (or `owner/repo#N:reviewer`), and can `ssf sub`
  other items as itself; it owns nothing and cannot be subscribed to (follow
  the PR instead).

The author session keeps the PR: the label (or review request) is delivered
to it as activity with a note that a reviewer session has it, and the review
itself arrives as activity marked "from the reviewer session on
owner/repo#N". The author answers on the PR and pushes fixes as it would for
a human reviewer; its replies reach the reviewer marked "from the agent on
owner/repo#A". To get another look it adds the `review` label again (`ssf guide`
says so).

The reviewer lives as long as the request: while the label is on the PR (or
the bot is a requested reviewer), new activity on the PR (pushes, replies) is
delivered to it as `[ssf] New activity on pull request ... which you are
reviewing`. Posting the review fulfils the request (ssf removes the label,
or GitHub drops the review request), and the reviewer is stood down (told to
stop, its record kept). Only a review counts, not a comment: a review by the
bot with the reviewer's origin tag, or without any tag; one tagged with
another session's origin is that session's doing. A repeated request brings
the same session back, with what happened in between, resuming its
conversation (and re-creating its workspace at the PR's current head if that
was removed). When the PR is closed or merged the reviewer is told, its
workspace is marked completed and cleaned up like any other. Reviewer state
lives next to the items in `state.json` under `reviewers`, keyed by PR
number; its `triggers` say what asked for the review (`review_label`,
`review_requested`).

Only same-repository PRs owned by a session get a reviewer. A PR the bot did
not write (a human's PR the bot is asked to review, or one assigned to it
without a session of its own on the branch) is handled as before: a session
of its own, on the PR's branch, which reviews when asked.

## Subscriptions and cross-session comments

Exactly one session acts on an item; any number can hear about it. Each
item carries a list of subscriber sessions next to its owner, and every
delivery about the item (new activity, closure, the bot being dropped from
it, or the item getting a session of its own) is fanned out to them with
FYI framing: `[ssf] FYI on issue owner/repo#N "title", owned by another
session (owner/repo#N): ...`, ending with the instruction not to act unless
asked and how to reach the agent on it. Subscriptions live in the state
file, so they survive relaunches and a session being brought back; a session
that retires (its item closed, or the bot dropped from it) is unsubscribed
everywhere.

The CLI takes the session identity from `SSF_REPO`/`SSF_ISSUE` (plus
`SSF_ROLE` for a reviewer) inside a session, or `--as owner/repo#N` (or
`owner/repo#N:reviewer`) from a human shell (an item bound to another
session counts as that session):

- `ssf sub <n|owner/repo#n>` / `ssf unsub ...` follow or drop an item.
  Subscribing to an item nothing tracks yet makes it tracked as
  *subscriber-only*: polled every pass for activity, no workspace, no owner;
  what happened before the subscription is not replayed. If the bot is later
  assigned to it (or it is otherwise bound), it gets a session as usual and
  the subscribers are told; when nobody follows it any more it is dropped.
- `ssf subs` lists what this session follows and who follows its items
  (`--json` for detail); `ssf peers` shows subscribers per session.
- `ssf tell <n> "message"` pastes a message into the terminal of the session
  acting on that item (`<n>:reviewer` for a PR's reviewer session), through
  the daemon's own delivery path (so the agent is relaunched or resumed first
  if its terminal is gone). It arrives as an
  `[ssf] Message from the agent session on owner/repo#A ("title") ...` prompt,
  or "from a human at the terminal" without `--as`. For an operator it is the
  steering tool ("stop, I'm changing the spec"). Between agents it is the
  exception: the default channel is a comment on the item (below), and
  `ssf guide` tells agents to keep `tell` for operational nudges that would
  be noise on the item ("master moved, rebase", "terminal is being replaced")
  and for reaching a session whose item is already closed. Tells are not
  mirrored to GitHub, so anything someone might need to find later
  (decisions, questions that change scope, status) goes on the item.
- Delegating parents are subscribed to their children automatically.

`sub`, `unsub` and `tell` talk to the running daemon over a Unix socket in
the state directory (`ssf.sock`), because the daemon owns the state and the
delivery path; `ssf doctor` reports whether it answers. `subs` and `peers`
read the state file and work without it.

**Cross-session comments.** Comments the bot posts carry the origin tag of
the session that made them (see above), and delivery is sorted out per
recipient rather than by author: a bot comment whose tag names a different
session is delivered like a human's, labelled "(from the agent on
owner/repo#M)", while a comment tagged with the recipient's own session
(or an item that session acts on) is the self-echo and stays filtered.
An untagged bot comment is a person's (above) and reaches every recipient,
marked "(not from a session)". So session A talks to session B by
commenting on B's issue with `gh`: B's agent receives it labelled as coming
from A, and A does not receive its own comment back, even when A is
subscribed to B's issue. This is the default channel between agents:
`ssf guide` says so, and a `tell` message repeats in one line that the
answer goes on the item.

## Under the hood

The details behind [How it works](#how-it-works).

- **Polling, not webhooks.** Every `poll_interval_secs` ssf makes four
  listings per repository (assigned to the bot, mentioning the bot, review
  requested from the bot, opened by the bot), as conditional requests, so a
  listing that has not changed costs nothing against the rate limit. Only
  items whose `updated_at` moved get their timeline fetched again.
- **Pull requests.** Review comments, reviews, force-pushes and merges are
  rendered like issue activity. A PR from a fork gets a workspace on the base
  branch and the agent is told it cannot push to the fork.
- **One workspace per issue.** The binding lives in
  `~/.local/state/ssf/state.json` and is also recoverable from Orca (the
  worktree is linked to the issue number), so a lost state file re-attaches
  instead of creating a second workspace.
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
- **First-run dialogs.** Claude Code asks whether to trust a new folder; ssf
  answers it so unattended launches do not stall.
- **Restarts.** A daemon restart is invisible to
  agents: the state is on disk, Orca keeps the terminals, and delivery finds
  them again. A machine restart takes the terminals with it, so the daemon
  runs a startup pass once Orca first answers: every active session that
  owns its workspace and has no live agent terminal is started again through
  the same path as any relaunch (`--resume` when a session id was captured,
  fresh with the item's story otherwise), with one message saying it was
  interrupted and telling it to check `git status`/`git log` and carry on,
  or say on the item what is left. Relaunches happen one at a time, each
  waiting for its agent to settle. Sessions that are still running are not
  touched, workspaces that are gone are brought back on their next event,
  and sessions whose workspace is waiting for cleanup are skipped. At start
  the daemon waits for Orca (`daemon.startup_orca_wait_secs`, checking every
  ten seconds) before its first poll; if Orca is still not up by then,
  polling starts anyway and the pass runs on the first poll that finds it.
  `daemon.resume_on_start = false` turns the pass off. `ssf run --once` runs
  it too.
- **Retirement and cleanup.** Closed or unassigned issues get one final
  message and are marked inactive. For closed issues, once the agent is idle
  (or after `daemon.cleanup_grace_secs`), the workspace is removed
  (`daemon.cleanup_on_close`, default on). Work that was pushed survives on
  the remote branch; re-assigning or reopening the issue re-creates the
  workspace and resumes the conversation.

## Notes and limitations (v1)

- The bot identity is a default, not a security boundary. Agents run as your
  Unix user inside your session, so a determined agent can still read your own
  gh token from the keyring or use your SSH agent. ssf tells agents to act
  only as the bot and to report missing permissions instead; real isolation
  would need a sandbox (bubblewrap without the session bus) or a dedicated
  Unix user for the factory.
- Session resume (and therefore memory across relaunches) is implemented for
  Claude Code and Codex; other agents are restarted with the full issue
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
(the polling loop), `src/sessions.rs` (agent session capture and resume),
`src/origin.rs` (bylines and origin tags), `src/shim.rs` (the `gh` wrapper), `src/ipc.rs`
(the CLI-to-daemon socket behind `sub`, `unsub` and `tell`),
`src/status.rs` (the joined item/session view behind `status`, `peers` and the
widget), `src/ui.rs` (Omarchy integration),
`omarchy-plugin/` (Quickshell bar widget), `bin/ssf-ui` (menu flows),
`packaging/` (PKGBUILD, systemd unit, pacman install script).
