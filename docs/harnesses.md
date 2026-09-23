# Harnesses

The coding agents ssf can run, and the settings that belong to them. For an agent choosing or tuning the stack a repository's sessions run on; the keys themselves are in [configuration.md#every-key](configuration.md#every-key).

A harness is the agent CLI ssf starts in a pane for an item: `claude`,
`codex`, `omp`, `pi`, `opencode`, `gemini`, `copilot`, `grok`, `crush`.
A repository names one in `repo.harness`, and one item can be moved to
another without changing the repository (see [per-item
overrides](#per-item-overrides)).

## What is available on this factory

```sh
ssf agents
ssf agents --installed
ssf agents --json
```

`ssf agents` lists every harness ssf knows, its display name, and whether
it is installed on the machine that runs the sessions (in [VM](vm.md) mode
that is the guest). `--json` adds `launch_command` (the permission-free
command below), `takes_model`, and `effort_levels` (the levels that
harness accepts). Only an installed harness can run a session; a
repository configured for one that is missing gives a session that never
starts, and `ssf doctor` says so.

Herdr must also recognise the harness to launch it (`herdr agent start
--help` lists what it can start). `ssf repo add` warns about a harness
herdr does not know.

## Models and effort

```sh
ssf models <harness>
ssf models <harness> --json
```

`repo.model` and `repo.effort` are the *session's* model: the agent ssf
starts for an item, which plans and delegates. They are not the model of
the subagents it spawns underneath itself, which is the harness's own
business: on some harnesses a subagent inherits the session's model unless
the session or an agent definition names another, so a session left on an
expensive model is an expensive subagent too. What ssf can do about the
tiers below the session is the model register the repository's `SSF.md`
puts in the prompt, a model and effort per harness for deliberation and
for execution (see [Writing SSF.md](ssf-md.md)). Which model to put where,
and how to work it out from what the person can run and what a task costs,
is [Choosing harness, model and
effort](repositories.md#2-choose-harness-model-and-effort-with-the-person).

Ask the person before selecting a model or an effort level. It is their
spend. ssf cannot tell a value a person chose from one a file already
carried, so a setup agent must ask rather than infer.

**Model ids.** `ssf models <harness>` prints the ids to choose from and
says on stderr which source answered. Ids are passed through to the
harness as given, so an id the list does not mention works as long as the
harness knows it.

| Source | Which harnesses | Where it is read |
|--------|-----------------|------------------|
| `catalogue` | `claude`, `codex` | the harness's own on-disk model catalogue under its config home (`$CLAUDE_CONFIG_DIR`, default `~/.claude`; `$CODEX_HOME`, default `~/.codex`) |
| `command` | `pi`, `omp`, `opencode` | the harness itself, asked for its model list |
| `table` | the rest, or when the machine has neither | ssf's built-in table |

`--json` prints `harness`, `models` and `source` (its `kind` is
`catalogue`, `command` or `table`, and `detail` is the file or command
line that answered). The dashboard menu's *Change model* picker uses the
same list. Pi, Oh My Pi and OpenCode take `provider/model` ids; Copilot
takes `auto` or a model name; Crush has no model flag for its terminal
interface, so ssf refuses a model for it.

**Effort levels** are ssf's own set per harness, and `effort_levels` in
`ssf agents --json` is the list each one accepts. They are deliberately
not read from a catalogue: a catalogue scopes levels to one *model*, so a
list read from there would refuse levels the flag itself takes, and a
level that only a catalogue carried would make the config unloadable the
moment that catalogue changed, taking the factory with it. A level the
harness does not list is refused when the config loads. Harnesses with an
empty `effort_levels` take no effort setting.

**How they reach the harness.** ssf appends them to the launch command as
flags when it starts the agent, resumes included:

| Harness | Appended |
|---------|----------|
| `claude` | `--model <id> --effort <level>` |
| `codex` | `-m <id> -c model_reasoning_effort=<level>` |
| `grok` | `-m <id> --reasoning-effort <level>` |
| `pi`, `omp` | `--model <id> --thinking <level>` |
| `copilot` | `--model <id> --effort <level>` |
| `gemini`, `opencode` | `-m <id>` |

Because they are appended, a `repo.command` that hard-codes a model or an
effort itself leaves the harness with the flag twice. Keep the model and
the effort in their own keys and out of `repo.command`.

Both are required by `ssf repo add` and `ssf repo set` for a harness that
supports them. Changing a repository's harness drops them, since the ids
belong to the harness, and a fresh selection is required; `--clear model`
or `--clear effort` alone is refused for the same reason. `ssf doctor`
fails and prints a repair command for a repository whose supported
settings are missing.

### Per-item overrides

`ssf handover` (see [Handover](sessions.md#handover)) moves one item to
another harness, model or effort level without touching `config.toml`, and
`ssf assign` (see [Assigning a stack before there is a
session](sessions.md#assigning-a-stack-before-there-is-a-session)) starts
an item's *first* session on one. What they set is a per-item override,
kept on the item in `state.json` next to the rest of its record, and it
wins over the `[[repo]]` the item belongs to:

- **The same harness the repository uses**: the repository's `command`
  still starts the agent, and the override's model and effort replace the
  repository's; either one left out keeps the repository's.
- **Another harness**: the item runs on that harness with its
  permission-free command (the repository's `command` belongs to its own
  harness and is not reused), and with the override's model and effort, or
  that harness's own defaults where none was named.

The override applies to every later launch of the item: a delivery that
has to start the agent again, a resumed conversation, a workspace
re-created from the branch, the startup pass after a reboot. It is in the
state file, so it survives daemon and machine restarts, and an item bound
to another session's workspace follows that session's override. `ssf
status` and `ssf peers` show the overridden harness, model and effort on
the item's line (`overrides` in `--json`), worded by which command wrote
them (`handed over to pi` against `harness pi`). Releasing the workspace
or purging the item leaves the override alone; only a later `ssf handover`
or `ssf assign` replaces it. `ssf assign` writes nothing when the stack it
is given is the one the item would run anyway, so an override nobody needs
cannot pin the item out of a later `ssf repo set`.

### What a session runs, against what launches next

An override and the repository's config are both about the *next* launch.
What a session that is already running is on is the driver's answer: the
harness reported for its pane governs what ssf reports, which harness's
sign-in prompt it looks for on screen, and which `SSF.<harness>.md`
guidance the session is given.

Change a repository's harness, model or effort under a live session and
that session stays where it is. ssf does not restart a running agent under
an operator, and `ssf repo set` prints a line saying so when the
repository has running sessions. The change waits for that session's next
launch, resume, relaunch or re-creation, and until then `ssf status` and
`ssf peers` show both sides (`harness codex → omp next launch`, and
`next_launch` beside `harness` in `--json`; the dashboard cards show the
same arrow). The running session's own model and effort are left out while
the two differ: only the harness is the driver's to report. To move a
running item now, use `ssf handover <item> --harness <the configured one>`.

## The launch command and permissions

Nobody sits at an ssf terminal, so an agent that stops to ask whether it
may run a command waits forever. ssf therefore starts every harness with
the flags that let it run unattended. `ssf agents --json` prints each
one's default as `launch_command`; `ssf launch` is what runs it.

A few first-run dialogs still appear and ssf answers them from the pane
screen: folder or directory trust, Claude Code's once-per-machine bypass
permissions acceptance, Crush's offer to create `AGENTS.md`. A **login**
prompt is the one dialog ssf cannot answer: that session is marked blocked
until a person signs the harness in.

Claude Code's `AskUserQuestion` tool is switched off in the default
command because it too waits for a person at the terminal; the agent is
told to ask on the issue instead.

`repo.command` replaces the whole default for a repository, for instance
to run with a permission mode of your own or a tool deny list in the
harness's own syntax (`--disallowedTools` for Claude Code, `--deny` for
Grok, `--deny-tool` for Copilot, `--exclude-tools` for Pi):

```sh
ssf repo set owner/repo --command "claude --dangerously-skip-permissions --disallowedTools 'Bash(git push:*)'"
ssf repo set owner/repo --clear command      # back to the default
```

A custom command must keep what the default carries, or the session loses
a capability ssf assumes:

| Harness | What a custom command must keep | Why |
|---------|---------------------------------|-----|
| `claude` | the bypass-permissions flag and the inline `--settings '{"crossSessionInbound":"accept"}'` | without them the session cannot take later item activity natively and falls back to the terminal |
| `pi`, `omp` | `"$SSF_PI_LAUNCHER" pi\|omp ... -e "$SSF_PI_BRIDGE"` | the launcher isolates and resumes the session, the extension is the item-activity channel; without it `ssf doctor` asks for the session to be restarted after the command is fixed |
| `omp` | `PI_STREAM_IDLE_TIMEOUT_MS=900000` | the longer stream-idle window suits long agentic turns; the default sets it and a custom command replaces the whole default |
| any | no `--model` or effort flag of its own | ssf appends those from `repo.model` and `repo.effort` |

`ssf launch` sets `SSF_PI_BRIDGE` to the packaged extension and
`SSF_PI_LAUNCHER` to an exec wrapper, not a process that stays beside the
harness.

Limits on what a session may *do* (do not merge, do not close issues)
belong in the [SSF agent guidance
file](configuration.md#the-ssf-agent-guidance-file), not in the command.
ssf has no tool allow/deny list of its own. Who may *drive* the agents is
[Who may drive the
factory](configuration.md#who-may-drive-the-factory).

## Context compaction

An unattended session that runs long enough fills its context window. Each
harness that grows one summarises its own history when the context crosses
a threshold, and ssf sets that threshold conservatively
(`auto_compaction_tokens`, `300000` by default) because the alternative is
not free: every turn re-sends the whole conversation, so an agent left to
grow until the model's own limit pays for its entire history on each
request, and that cost grows with the square of the turns. A smaller
window is a deliberate trade of detail for cost: the summary keeps the
gist, not the file paths and the reasoning on them. `0` leaves the
harness's own default alone.

| Harness | How ssf sets it | Notes |
|---------|-----------------|-------|
| `claude` | `--autocompact <tokens>` on the launch command, resumes included | Claude Code accepts `100000`-`1000000` and refuses to start outside it, so a count it cannot take is refused while the config loads and again when `ssf config set` or `ssf repo set` would write one |
| `codex` | `-c model_auto_compact_token_limit=<tokens>`, the same route the effort level takes | codex type-checks the key and reports a bad value at startup |
| `omp` | `PI_CONFIG_FILES` pointed at a one-key overlay under the state directory, written by `ssf launch` | omp has no flag or environment variable for the value. A command that already sets `PI_CONFIG_FILES` for itself keeps its own overlays, layered after ssf's |
| everything else | nothing | `pi`, `opencode`, `gemini`, `grok`, `copilot` and `crush` have no such setting; a value configured above them is unused rather than an error, so one instance value can sit above a mixed set of repositories |

Set it per repository as well as per instance: a large-context model can
afford more room than a 200K one, and a repository whose items carry long
histories is where a small window costs the most.

```sh
ssf config set auto_compaction_tokens 300000
ssf repo set owner/repo --auto-compaction-tokens 500000
```

A repository's own value belongs to its harness: `ssf repo set --harness`
drops it, exactly as it drops the model and the effort, and an item handed
to another harness falls back to the instance value or the default rather
than carrying a count the new harness may refuse. A custom `repo.command`
does not replace ssf's value: the flag or the overlay is appended to
whatever command you configured.

## Item activity delivery

An item's first prompt always goes through the driver's confirmed path.
Later activity (a new comment, a review, a push) is delivered to a session
that is already running, and how well that works depends on the harness:

| Harness | Later activity |
|---------|----------------|
| `claude` | a native channel into the running agent, with the default command's settings kept |
| `codex` | a native channel, experimental and opt-in, and only when an operator provides the launcher and the server it attaches to (below) |
| everything else | the driver prompts the pane; if the agent is sitting at a question, a raw paste into the terminal |

Where a native channel is unavailable or an explicit native launch fails,
the event is **held**, not pasted and not resent: ssf journals before
sending and confirms receipt in the session's own transcript, so an
ambiguous attempt is reconciled rather than duplicated. `ssf doctor`
reports a session whose channel is degraded. The mechanics, including what
to inspect before resolving a held journal, are in [Workspaces and
terminals](drivers.md#item-activity-delivery).

**Codex native delivery** is the one channel that needs operator work. The
default `codex` command is standalone and uses the terminal fallback. To
opt in, set `repo.command` to a launcher that attaches the
herdr-managed TUI to a per-item app-server:

```sh
codex --remote unix:///absolute/item-specific/app.sock \
  --dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust
```

You own that server's startup, restart and cleanup; ssf provisions none
and exposes no TCP listener. The launcher receives `SSF_REPO`, `SSF_ISSUE`
and `SSF_DELIVERY_MAILBOX`; derive a unique private endpoint per item
rather than share one socket across items. The socket must be mode 0600,
owned by the same user, and dedicated to one ordinary loaded conversation,
the same one the TUI displays. Keep model and effort in their own keys.
A remote *resume* must omit the bypass-approvals flag, since Codex rejects
permission overrides when reconnecting to a persisted task; ssf removes it
for a direct `codex --remote` command, and a custom launcher must handle
that itself.

## Signing the harnesses in

ssf never signs a harness in. Each one is signed in once, by hand, on the
machine that runs the sessions, and the person must do it: it is their
account and may cost money.

- A new factory: the sign-in step of [install.md](install.md#8-sign-in-the-harness).
- In VM mode the sessions run in the guest, so the sign-in belongs there
  (`ssf vm login`, see [Harness logins](vm.md#harness-logins)). Copying a
  host credential file in with `vm.files` shares one session between host
  and guest, so a logout in either place signs out both.
- Anything a particular harness needs on a particular platform or vendor
  is in [platform-specifics.md](platform-specifics.md).

A session whose harness is not signed in is marked blocked, not failed:
sign in, and the next launch picks up where it stopped. See
[troubleshooting.md](troubleshooting.md).
