# Configuration

Every key in `~/.config/ssf/config.toml`, the per-project prompt file, model and effort settings, the permission-free commands each agent is started with, and who may drive the factory. For whoever sets up or tunes a factory; agents need none of it.

`~/.config/ssf/config.toml` is mostly written for you by `ssf repo add`,
`ssf config set` and the bar widget;
[`config.example.toml`](../config.example.toml) (installed as
`/usr/share/ssf/config.example.toml`) shows every key with a comment. The
token is never in this file: a pasted one (`ssf auth login --token`) lives
in `~/.config/ssf/token` (mode 0600), otherwise it is read from gh's
keyring when needed. `ssf config set` refuses to touch `github.token`; use
`ssf auth login` for that. Changes are picked up on the next poll; no
restart needed.

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

## Every key

| Key | Default | Meaning |
|-----|---------|---------|
| `github.api_url` | `https://api.github.com` | GitHub Enterprise: `https://ghe.example.com/api/v3` |
| `github.login`, `github.email`, `github.ssh_key_path`, `github.ssh_key_id`, `github.signing_key_id` | set by `ssf auth login` | The bot's login, commit email, enrolled key and the ids of its two entries on GitHub (so `ssf auth logout` can revoke them); edit `email` if the bot has a public address |
| `driver` | `orca` | What runs the agents: `orca` or `herdr` (see [Drivers](drivers.md)) |
| `orca.command` | `/usr/lib/orca-ide/bin/orca-ide` | Orca CLI binary (`/usr/bin/orca-ide` launches the app, not the CLI) |
| `orca.host` | `local` | The Orca host projects and worktrees are created on |
| `orca.projects_dir` | `~/orca/projects` | Where repositories are cloned when Orca has no project for them |
| `orca.setup_timeout_secs` | `900` | How long to wait for a project clone to be ready |
| `orca.tui_idle_timeout_ms` | `90000` | How long a freshly started agent gets to show up and go idle in its terminal |
| `herdr.command` | `herdr` | The herdr CLI (a herdr session must be running) |
| `herdr.projects_dir` | `~/ssf/projects` | Where ssf clones repositories for the herdr driver; worktrees go in `<name>.worktrees/` next to the clone |
| `herdr.tui_idle_timeout_ms` | `90000` | How long a freshly started agent gets to show up in its pane |
| `daemon.poll_interval_secs` | `10` | GitHub poll interval (unchanged listings cost nothing against the rate limit) |
| `daemon.include_own_events` | `false` | Deliver the bot's own commits and cross-references, and each session's posts back to it (normally noise; see [Identity and bylines](identity-and-bylines.md)) |
| `daemon.ignored_events` | `["mentioned", "subscribed", "unsubscribed"]` | Timeline event types that are never delivered |
| `daemon.max_body_chars` | `8000` | Longest comment body quoted in a prompt, in characters |
| `daemon.instructions` | | Extra instructions appended to every initial prompt |
| `daemon.cleanup_on_close` | | No longer used: item workspaces are never removed on close (see [Workspaces after close](sessions.md#workspaces-after-close-release-and-purge)); still accepted so old files load |
| `daemon.cleanup_grace_secs` | `900` | How long a reviewer session gets to finish before its read-only workspace is removed anyway; item workspaces are not affected |
| `daemon.review_label` | `review` | Label that asks for a review of a session's own pull request (see [Reviewer sessions](sessions.md#reviewer-sessions)); `""` turns the label trigger off |
| `daemon.resume_on_start` | `true` | Start interrupted sessions again when the daemon starts (see [Restarts](internals.md#polling-and-delivery)) |
| `daemon.startup_orca_wait_secs` | `120` | How long to wait for Orca at daemon start before the first poll |
| `daemon.allowed_users` | the collaborators with push access | GitHub logins whose assignments, mentions, review requests, labels and comments the agents act on (see [Who may drive the factory](#who-may-drive-the-factory)); `["*"]` is anyone and needs `daemon.accepted_anyone_risk = true` |
| `daemon.accepted_anyone_risk` | `false` | Written next to a `["*"]` list by `ssf config set ... --accept-anyone-risk`; a wildcard without it is refused at load |
| `vm.enabled` | `false` | Run the whole factory inside a Firecracker microVM (see [Inside a microVM](vm.md)); `ssf run` then starts and watches the VM, and the daemon-facing commands run in the guest |
| `vm.name`, `vm.dir` | `default`, `~/.local/share/ssf/vm` | The VM's name and where the image, kernel, binaries and each VM's disks live (`<dir>/<name>/`) |
| `vm.vcpus`, `vm.mem_mib` | `2`, `4096` | The guest's size |
| `vm.data_gib`, `vm.root_gib` | `20`, `8` | The persistent data disk (state, clones, worktrees; sparse) and the root image `ssf vm build` makes |
| `vm.ssh_port` | `2222` | Where the guest's sshd is published on `127.0.0.1` |
| `vm.files` | `[]` | Host files copied into the guest at every start (`src` or `src:dest`). Copies an existing harness login in (`~/.claude/.credentials.json`) as the same session as yours; `ssf vm login` makes the guest its own, see [Harness logins](vm.md#harness-logins) |
| `vm.firecracker`, `vm.gvproxy`, `vm.kernel`, `vm.rootfs` | under `vm.dir` | Use binaries or images of your own instead of the downloaded ones |
| `repo.name` | | `owner/name` on GitHub (required) |
| `repo.harness` | | Agent id (required): `claude`, `codex`, `omp`, `pi`, `opencode`, `gemini`, `copilot`, `grok`, `crush` (`ssf agents` lists them) |
| `repo.driver` | the top-level `driver` | This repository's driver, so one daemon can run some repositories in Orca and others in herdr |
| `repo.command` | the agent's permission-free command | Command that starts the agent; overrides the default from [Permissions](#permissions), e.g. `claude --permission-mode acceptEdits` |
| `repo.model` | the agent's default | Model: an Orca model id, or the agent's own `provider/model` (`ssf models <agent>` lists them; other ids pass through) |
| `repo.effort` | the agent's default | Effort or thinking level (`ssf agents --json` lists what each agent accepts) |
| `repo.path` | | Register an existing checkout instead of cloning |
| `repo.clone_url` | `https://github.com/owner/name.git` | Use an SSH URL for private repositories (the bot's enrolled key is used) |
| `repo.base_branch` | the driver's default base for the repository | Base ref for issue worktrees, e.g. `origin/main` |
| `repo.instructions` | | Extra instructions appended to this repository's initial prompts, after `daemon.instructions`; a line or two, anything longer belongs in the prompt file |
| `repo.prompt_file` | `SSF.md` | The per-project prompt file (below), relative to the worktree unless absolute or `~/` |
| `repo.allowed_users` | `daemon.allowed_users` | Who may drive this repository, replacing the instance list; `[]` is nobody but the bot, `["*"]` needs `accepted_anyone_risk = true` on the repo |
| `repo.accepted_anyone_risk` | `false` | As `daemon.accepted_anyone_risk`, for a `["*"]` on this repository |

The CLI writes all of it: `ssf repo add <owner/name> --harness <id>` with
`--driver`, `--path`, `--clone-url`, `--base-branch`, `--command`,
`--model`, `--effort`, `--instructions`, `--prompt-file`,
`--allowed-users` and `--accept-anyone-risk`; `ssf repo set` changes some
of those and `--clear <field>` unsets one; `ssf config get|set
<dotted.key> [value]` for everything else.

Environment overrides: `SSF_GITHUB_TOKEN` (the token), `SSF_CONFIG_DIR`
and `SSF_STATE_DIR` (where config and state live; a scratch factory uses
its own), `ORCA_CLI_COMMAND` and `HERDR_COMMAND` (the driver binaries),
`SSF_VM_DIR` (the VM image scripts), `SSF_PLUGIN_DIR` (the bar widget's
source, for development), `SSF_LOG` or `RUST_LOG` (log verbosity, what
`--log` reads).

## The per-project prompt file

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
advice (see [What the agent is told](prompts.md)), so a repository that
wants its agents told to comment when they start and finish, to ask rather
than guess, to commit as they go, or how to review, says so here.
[`SSF.example.md`](../SSF.example.md) (installed as
`/usr/share/ssf/SSF.example.md`) is a starting point with exactly those
lines; this repository's own [`SSF.md`](../SSF.md) is what produced the
comments quoted in the README's walkthrough.

## Models and effort levels

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
| `codex` | `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-5.5`, `gpt-5.2-codex`, ... | `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra` | `-m <id> -c model_reasoning_effort=<level>` |
| `gemini` | `gemini-3-pro-preview`, `gemini-3-flash-preview`, `gemini-2.5-pro`, `gemini-2.5-flash`, ... | none | `-m <id>` |
| `grok` | `grok-4.6`, `grok-4.5` | `low`, `medium`, `high`, `xhigh` | `-m <id> --reasoning-effort <level>` |
| `pi` | `provider/model` as in `pi --list-models`, e.g. `openrouter/anthropic/claude-sonnet-4` | `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` | `--model <id> --thinking <level>` |
| `omp` | `provider/model` as in `omp models`, e.g. `openai-codex/gpt-5.4` | as `pi`, plus `auto` | `--model <id> --thinking <level>` |
| `opencode` | `provider/model` as in `opencode models` | none | `-m <id>` |
| `copilot` | `auto` or a model name | `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` | `--model <id> --effort <level>` |

`ssf models <agent>` prints the ids to choose from, asking the installed
agent for its list where it has one (`pi`, `omp`, `opencode`); the menu's
*Change model* picker uses the same list. Crush has no model flag for its
terminal interface, so ssf refuses a model for it. Model ids are passed
through as given, so a model the list does not mention works as long as the
agent knows it; effort levels must be ones the agent accepts (a wrong one
is refused when the config loads). Changing the agent of a repository
resets both, since the ids belong to the agent. Keep `--model`/`--effort`
out of `repo.command` when you set them here, or the agent sees the flag
twice.

## Permissions

Nobody sits at an ssf terminal, so an agent that stops to ask whether it may
run a command waits forever. Unless `repo.command` says otherwise, ssf
therefore starts every agent with the flags that let it run unattended:

| Agent | Default command | What still shows up at start |
|-------|-----------------|------------------------------|
| `claude` | `claude --dangerously-skip-permissions --disallowedTools AskUserQuestion` | the folder-trust question, and once per machine the "Bypass Permissions mode" acceptance (ssf answers both) |
| `codex` | `codex --dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust` | the directory-trust question (ssf answers it) |
| `gemini` | `gemini --yolo --skip-trust` | nothing (without `--skip-trust`, a trust dialog ssf answers) |
| `grok` | `grok --always-approve` | nothing |
| `pi` | `pi --approve` (Pi has no tool approvals; the flag trusts the repository's `.pi/` files) | nothing (without `--approve`, a trust dialog ssf answers) |
| `omp` | `omp --auto-approve` | nothing |
| `opencode` | `opencode --auto` | nothing |
| `copilot` | `copilot --allow-all` | nothing (the flag trusts the folder too) |
| `crush` | `crush --yolo` | an offer to create `AGENTS.md`, which the first prompt dismisses |

`ssf agents --json` shows the default as `launch_command`. Claude Code's
`AskUserQuestion` tool is switched off because it, too, waits for a person
at the terminal; the agent is told to ask on the issue instead. Login and
first-run onboarding are not covered: sign each agent in once, by hand, on
the machine that runs the daemon.

Set `repo.command` to run an agent differently, for instance with a
permission mode of your own or a tool deny list in the agent's own syntax
(`--disallowedTools` for Claude Code, `--deny` for Grok, `--deny-tool` for
Copilot, `--exclude-tools` for Pi):

```sh
ssf repo set acme/widgets --command "claude --dangerously-skip-permissions --disallowedTools 'Bash(git push:*)'"
ssf repo set acme/widgets --clear command      # back to the default
```

Behavioural limits (do not merge, do not
close issues) belong in the [per-project prompt
file](#the-per-project-prompt-file), not in the command. ssf has no
tool allow/deny list of its own; who may *drive* the agents is the next
section.

## Who may drive the factory

Everything that reaches the bot on GitHub comes from whoever can write on
the repository, and a comment is relayed straight into a running agent's
terminal. `allowed_users` says whose word counts:

```toml
[daemon]
allowed_users = ["mikekelly", "alice"]      # for every repository below

[[repo]]
name = "acme/widgets"
harness = "claude"
allowed_users = ["mikekelly"]               # replaces the instance list here
```

```sh
ssf config set daemon.allowed_users '["mikekelly", "alice"]'
ssf repo set acme/widgets --allowed-users alice,bob    # for one repository, replacing the instance list
ssf repo set acme/widgets --clear allowed_users
```

- **Unset** (a fresh install): the repository's collaborators with push
  access. The daemon fetches them once per pass (an unchanged answer is a
  free 304) and `ssf doctor` prints the list per repository. If they cannot
  be fetched, an organisation repository where the token lacks `read:org`
  say, and none were fetched before, the pass fails for that repository and
  nothing is acted on until either the fetch works or a list is configured;
  `ssf status` shows the error and `ssf doctor` says how to fix it.
- **A repository list replaces the instance list** rather than extending
  it, so one repository can be narrowed as well as widened; `[]` is nobody
  but the bot. Logins compare case-insensitively.
- **The bot itself always counts**, tagged posts and untagged ones alike
  (whoever types as the bot holds its token).
- **App accounts** such as `github-actions[bot]` or
  `github-project-automation[bot]` are ordinary logins: listed explicitly
  or not at all, and never part of the collaborator default.

What the list does: an item only gets a session when an allowed login
asked for it, read from the item's timeline: who assigned the bot (latest
assignment), who mentioned it (body, comment or review), who requested the
review or added the `review` label. One that nobody allowed asked for is
logged once at info level with the login and trigger, and not read again
until it changes; an allowed user assigning or mentioning the bot later
brings it in. On a running session, events by anyone else are dropped
before delivery, so a non-listed user's comment on an owned item reaches
neither the owner nor its subscribers or reviewer. Commits are the one
event without a login and pass (pushing needs write access to the branch);
unassigning or closing still retires a session, since stopping work is
safe. One limit to know: the timeline says who posted a body or comment,
not who edited it, and anyone with write access can edit anyone's text, so
the list is a boundary against the internet, not a hard one among people
who can already push. Prompts are unchanged: this is all daemon-side.

`"*"` means anyone on GitHub. It is never accepted silently: `ssf config set
daemon.allowed_users '["*"]'` and `ssf repo set <repo> --allowed-users '*'`
refuse it unless you type `yes` (nothing shorter) to the risk at the terminal or pass
`--accept-anyone-risk`, either of which writes `accepted_anyone_risk = true`
next to the list (setting a plain list again removes it). A hand-edited
file with `"*"` and no marker is refused at load with the fix spelled out,
`ssf status` prints a warning while the wildcard is in effect, and the bar
widget shows one and turns its icon urgent.
