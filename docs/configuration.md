# Configuration

Every key in `~/.config/ssf/config.toml`, the per-project prompt file, model and effort settings, the permission-free commands each agent is started with, and who may drive the factory. For whoever sets up or tunes a factory; agents need none of it.

`~/.config/ssf/config.toml` is mostly written for you by `ssf repo add` and
`ssf config set` (the bar widget only shows the state of the factory);
[`config.example.toml`](../config.example.toml) (installed as
`/usr/share/ssf/config.example.toml`, and on macOS as
`$(brew --prefix)/share/ssf/config.example.toml`) shows every key with a
comment. The token is never in this file: a pasted one
(`ssf auth login --token`) lives in `~/.config/ssf/token` (mode 0600),
otherwise it is read from gh's keyring when needed.
`ssf config set` refuses to touch `github.token`; use
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
| `git.name`, `git.email` | the bot's login and email | Author and committer of the agents' commits, when a person rather than the bot (see [Committing as a person](identity-and-bylines.md#committing-as-a-person-while-gh-stays-the-bot)); both or neither. `gh` stays the bot |
| `git.signing_key` | the bot's key for the bot, unsigned for a person | SSH key to sign commits and tags with (a path), or `false` for unsigned |
| `git.credential` | `bot` | Who pushes over HTTPS: `bot` (the bot's token), `token:<login>` (the token gh holds for that account where the agents run), `file:<path>` (a token file), or a git credential helper string used as `credential.helper`. SSH remotes always use the bot's key |
| `driver` | `herdr` | What runs the agents: `herdr` or `orca` (see [Drivers](drivers.md)). Unset, `ssf config show` and `ssf doctor` say which is in effect; the default was `orca` until 2026-09-06, so an older file that never set it now runs in herdr unless it says `driver = "orca"` |
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
| `daemon.cleanup_grace_secs` | | No longer used: it timed the reviewer sessions out, which went with #115 (see [Second opinions](sessions.md#second-opinions-the-gauntlet)); still accepted so old files load, and `ssf doctor` says so while it stays |
| `daemon.review_label` | | No longer used: the label started a reviewer session until #115; ssf reacts to no label now. Still accepted so old files load; `ssf doctor` says so while it stays |
| `daemon.resume_on_start` | `true` | Start interrupted sessions again when the daemon starts (see [Restarts](internals.md#polling-and-delivery)) |
| `daemon.startup_driver_wait_secs` | `120` | How long to wait for the driver (herdr or Orca) at daemon start before the first poll; the old name `startup_orca_wait_secs` still loads |
| `daemon.allowed_users` | the collaborators with push access | GitHub logins whose assignments, mentions, review requests, labels and comments the agents act on (see [Who may drive the factory](#who-may-drive-the-factory)); `["*"]` is anyone and needs `daemon.accepted_anyone_risk = true` |
| `daemon.accepted_anyone_risk` | `false` | Written next to a `["*"]` list by `ssf config set ... --accept-anyone-risk`; a wildcard without it is refused at load |
| `daemon.event_comments` | `true` | Post the daemon's essential events on the item as fenced `ssf` blocks: a session attached, resumed, blocked and unblocked, given up on, handed over, its workspace released (see [What ssf says on the item](sessions.md#what-ssf-says-on-the-item)); `false` posts nothing and changes nothing else |
| `vm.enabled` | `false` | Run the whole factory inside a VM (see [Inside a VM](vm.md)); `ssf run` then starts and watches the VM, and the daemon-facing commands run in the guest |
| `vm.backend` | Firecracker on Linux, lima on macOS | `firecracker` or `lima`: what runs the guest (see [Backends](vm.md#backends)); unset, `ssf vm build` writes the platform's default here |
| `vm.name`, `vm.dir` | `default`, `~/.local/share/ssf/vm` | The VM's name and where the image, kernel, binaries and each VM's files live (`<dir>/<name>/`); under lima the instance is `ssf-<name>` and its data disk `ssf-<name>` in lima's home, and `name` is then at most 7 characters, since lima labels the disk's filesystem `lima-<disk>` and an ext4 label holds 16 |
| `vm.vcpus`, `vm.mem_mib` | chosen from the machine | The guest's size; unset, `ssf vm build` writes the host's CPUs minus one (at least 2) and half its RAM in MiB (at least 4096) here (see [Size](vm.md#size)) |
| `vm.data_gib` | chosen from the machine | The persistent data disk (state, clones, worktrees) in GiB, sparse; unset, `ssf vm build` writes half the free space of the filesystem that will hold the disk (at least 20) here, and prints the path it measured: `vm.dir` under Firecracker, lima's own disk directory (`$LIMA_HOME/_disks`, by default `~/.lima/_disks`) under lima, since that is where lima keeps its disks; `ssf vm grow` enlarges it later |
| `vm.root_gib` | `8` | The root image `ssf vm build` makes; under lima the instance's root disk, at least 20 whatever is set |
| `vm.ssh_port` | `2222` | Where the guest's sshd is published on `127.0.0.1` |
| `vm.files` | `[]` | Host files copied into the guest at every start (`src` or `src:dest`). Copies an existing harness login in (`~/.claude/.credentials.json`) as the same session as yours; `ssf vm login` makes the guest its own, see [Harness logins](vm.md#harness-logins) |
| `vm.firecracker`, `vm.gvproxy`, `vm.kernel`, `vm.rootfs` | under `vm.dir` | Firecracker only: use binaries or images of your own instead of the downloaded ones |
| `vm.limactl` | `limactl` on `PATH` | lima only: the `limactl` binary to drive the instance with; lima 2.0.1 or newer, which `ssf vm build` checks and says why (see [Backends](vm.md#backends)) |
| `vm.image` | Arch's cloud image on x86_64, Ubuntu LTS on aarch64 | lima only: a cloud-init image (URL or path; Arch or Debian/Ubuntu) to boot instead of the default for the guest's architecture |
| `vm.vm_type` | lima's default | lima only: `vz` or `qemu`, passed through to lima (`vz` is macOS only) |
| `vm.guest_binary` | this binary on a Linux host, else the release asset `ssf-<version>-linux-<arch>` fetched with `gh` | A Linux `ssf` binary to seed into the guest, for a dev build or a version without a release asset |
| `vm.herdr` | the host's own `herdr` on a Linux host, else herdr's latest Linux release downloaded by the guest while provisioning | lima only: a Linux herdr binary for the guest; installed when the guest is provisioned, so a change needs `ssf vm reset` |
| `repo.name` | | `owner/name` on GitHub (required) |
| `repo.harness` | | Agent id (required): `claude`, `codex`, `omp`, `pi`, `opencode`, `gemini`, `copilot`, `grok`, `crush` (`ssf agents` lists them) |
| `repo.driver` | the top-level `driver` | This repository's driver, so one daemon can run some repositories in Orca and others in herdr |
| `repo.command` | the agent's permission-free command | Command that starts the agent; overrides the default from [Permissions](#permissions), e.g. `claude --permission-mode acceptEdits` |
| `repo.model` | the agent's default | Model: an Orca model id for `claude`, `codex`, `gemini` and `grok`, the agent's own `provider/model` for `pi`, `omp`, `opencode` and `copilot` (`ssf models <agent>` lists them; other ids pass through) |
| `repo.effort` | the agent's default | Effort or thinking level (`ssf agents --json` lists what each agent accepts) |
| `repo.path` | | Register an existing checkout instead of cloning |
| `repo.clone_url` | `https://github.com/owner/name.git` | Use an SSH URL for private repositories (the bot's enrolled key is used) |
| `repo.base_branch` | the driver's default base for the repository | Base ref for issue worktrees, e.g. `origin/main` |
| `repo.instructions` | | Extra instructions appended to this repository's initial prompts, after `daemon.instructions`; a line or two, anything longer belongs in the prompt file |
| `repo.prompt_file` | `SSF.md` | The per-project prompt file (below), relative to the worktree unless absolute or `~/` |
| `repo.allowed_users` | `daemon.allowed_users` | Who may drive this repository, replacing the instance list; `[]` is nobody but the bot, `["*"]` needs `accepted_anyone_risk = true` on the repo |
| `repo.accepted_anyone_risk` | `false` | As `daemon.accepted_anyone_risk`, for a `["*"]` on this repository |
| `repo.event_comments` | `daemon.event_comments` | Whether the daemon posts its events on this repository's items (`ssf repo set <owner/name> --event-comments false`) |
| `repo.git.name`, `repo.git.email`, `repo.git.signing_key`, `repo.git.credential` | the `[git]` table | The same four keys for this repository, each overriding its `[git]` counterpart (a `[repo.git]` table under the `[[repo]]`) |

The CLI writes all of it: `ssf repo add <owner/name> --harness <id>` with
`--driver`, `--path`, `--clone-url`, `--base-branch`, `--command`,
`--model`, `--effort`, `--instructions`, `--prompt-file`,
`--allowed-users` and `--accept-anyone-risk`; `ssf repo set` changes some
of those, sets `--git-name`, `--git-email`, `--git-signing-key` and
`--git-credential`, and `--clear <field>` unsets one (`git` for the whole
`[repo.git]` table, `git.credential` for one key); `ssf config get|set
<dotted.key> [value]` for everything else. A value that starts with `[`
or `{` is read as TOML, so `ssf config set git '{ name = "Ann Person",
email = "ann@example.com" }'` sets both halves of an identity at once.

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
when an agent is started again from scratch. No file, or an empty one, adds
nothing, and `ssf doctor` reports a repository whose notes are missing
(`FAIL no SSF.md in owner/name; start from /usr/share/ssf/SSF.example.md`;
on a Mac the message names the Homebrew copy instead, under
`$(brew --prefix)/share/ssf/`, since ssf looks beside its own binary
first there), looking for the file through the GitHub
contents API on `repo.base_branch`
(else the default branch), so no clone is needed; an absolute or `~/`
`prompt_file` is looked for on the machine instead. `repo.prompt_file` names another
file: a path inside the worktree (`.github/ssf.md`), or an absolute or `~/`
path for notes you would rather not commit.

This is also where working style goes. ssf's prompts carry rules, not
advice (see [What the agent is told](prompts.md)), so a repository that
wants its agents told to comment when they start and finish, to ask rather
than guess, or to commit as they go, says so here. It is also the only
natural place to steer the models a session spawns its subagents on
(`daemon.instructions` and `repo.instructions` reach the prompt too),
since `repo.model` reaches the session alone: a line naming the model
for planning, diagnosis and the gauntlet and the model for the bulk of
the implementation work is a rule like any other, and like any other it
is advice in a prompt rather than configuration, so it holds only where
the harness lets a session choose a model as it spawns one (see
[Choosing the harness and the
model](setup.md#choosing-the-harness-and-the-model)).
[`SSF.example.md`](../SSF.example.md) (installed as
`/usr/share/ssf/SSF.example.md`, and on macOS as
`$(brew --prefix)/share/ssf/SSF.example.md`) is a starting point with
those lines, who is in charge of the item, how visible to stay, an
autonomy line (how much a person approves), a delegation line (choose
the model and effort each subagent runs on rather than taking the
default), a scope line (an item is one cohesive piece of work; split it
into sub-issues and sibling issues when it is not, so the shape of the
work can be read off the issue tree), a plan line (write the plan into
the body of the item before execution and keep it current there), a line on
checking a claim (check how something behaves rather than reasoning your way
to it, and say where you have not) and the
**gauntlet** rule: ssf runs one session per item and starts no reviewer,
so the boilerplate tells the author to choose self-review, one or two
fresh-agent rounds, or deep review by blast radius, with explicit stopping
rules and the same required verification in every class (see [Second
opinions](sessions.md#second-opinions-the-gauntlet)).
This repository's own [`SSF.md`](../SSF.md) is what produced the comments
quoted in the README's walkthrough.

## Models and effort levels

`repo.model` and `repo.effort` are the *session's* model: the agent ssf
starts for an item, which plans and delegates. They are not the model of
the subagents it spawns underneath itself, which is the harness's own
business — on some harnesses a subagent inherits the session's model
unless the session or an agent definition names another, so a session
left on an expensive model is an expensive subagent too. What ssf can do
about the tiers below the session is the line the repository's `SSF.md`
puts in the prompt. Which model to put where, and how to work it out
from what the person can run and what a task costs, is [Choosing the
harness and the model](setup.md#choosing-the-harness-and-the-model) in
the setup document; the rest of this section is the mechanics.

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

### Per-item overrides

`ssf handover` (see [Handover](sessions.md#handover)) moves one item to
another harness, model or effort level without touching `config.toml`.
What it sets is a per-item override, kept on the item in `state.json`
next to the rest of its record, and it wins over the `[[repo]]` the item
belongs to:

- **The same harness the repository uses**: the repository's `command`
  still starts the agent, and the override's model and effort replace the
  repository's; either one left out keeps the repository's. As
  everywhere else, the model and effort are appended to `command` as
  flags rather than rewritten into it, so a `command` that hard-codes a
  model or effort itself (`command = "claude --model opus"`) leaves the
  agent with the flag twice and the handover's model changes nothing it
  can rely on: keep the model and effort in their own keys on a
  repository whose items are handed over.
- **Another harness**: the item runs on that harness with the
  permission-free command from [Permissions](#permissions) (the
  repository's `command` belongs to its own harness and is not reused),
  and with the override's model and effort, or that harness's own
  defaults where the handover named none.

The override applies to every later launch of the item: a delivery that
has to start the agent again, a resumed conversation, a workspace
re-created from the branch, the startup pass after a reboot. It is in the
state file, so it survives daemon and machine restarts, and an item bound
to another session's workspace follows that session's override. `ssf
status` and `ssf peers` show the overridden harness, model and effort on
the item's line (and `overrides` in `--json`), together with a handover
that has not been carried out yet. Releasing the workspace or purging the
item clears the override, and the item comes back on the repository's own
settings.

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
allowed_users = ["alice", "bob"]            # for every repository below

[[repo]]
name = "acme/widgets"
harness = "claude"
allowed_users = ["alice"]                   # replaces the instance list here
```

```sh
ssf config set daemon.allowed_users '["alice", "bob"]'
ssf repo set acme/widgets --allowed-users alice,bob    # for one repository, replacing the instance list
ssf repo set acme/widgets --clear allowed_users
```

- **Unset** (a fresh install): the repository's collaborators with push
  access, which is GitHub's **Write** role or higher (Write, Maintain,
  Admin) in **Settings → Collaborators and teams**. The daemon fetches them once per pass (an unchanged answer is a
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
review. One that nobody allowed asked for is
logged once at info level with the login and trigger, and not read again
until it changes; an allowed user assigning or mentioning the bot later
brings it in. On a running session, events by anyone else are dropped
before delivery, so a non-listed user's comment on an owned item reaches
neither the owner nor its subscribers. Commits are the one
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
