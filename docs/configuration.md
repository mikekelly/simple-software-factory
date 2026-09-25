# Configuration

Every key of a factory's `config.toml`, where it lives, how to change it, and who may drive the factory. For an agent tuning a factory that already runs; installing one is [install.md](install.md).

## Where configuration lives

| File | Owner | What is in it |
|------|-------|---------------|
| `~/.config/ssf/config.toml` | one factory | everything in [Every key](#every-key) |
| `~/.config/ssf/servers.toml` | this client | named routes to factories ([Server catalog](#server-catalog)) |
| `~/.config/ssf/token` (mode 0600) | one factory | a pasted GitHub token, never in `config.toml` |

`ssf config path` prints the config file. `ssf repo add`, `ssf repo set`
and `ssf config set` write most of it for you; the dashboards only show
the state of the factory and do not edit configuration.

```sh
ssf config path
ssf config show                       # the effective configuration, token redacted
ssf config get daemon.poll_interval_secs
ssf config set daemon.poll_interval_secs 60
```

A value that starts with `[` or `{` is read as TOML, so one command can
set a whole table:

```sh
ssf config set git '{ name = "Ann Person", email = "ann@example.com" }'
```

`ssf config set` refuses to touch `github.token`; use `ssf auth login`
for that. A pasted token (`ssf auth login --token`) goes to
`~/.config/ssf/token`. In host mode a browser login reads the token from
gh's keyring when needed; in VM mode browser login and credential storage
run inside the guest.

Changes are picked up on the next poll, with no restart, except
`[dashboard]` listener settings, which need the server restarted.

In VM mode the guest owns the factory: repositories, `[github]`, `[git]`,
`[daemon]` and driver settings live there, and repository commands,
`ssf config get|set` and `ssf auth` run in the guest over SSH, so paths in
their arguments are guest paths. The host keeps `vm.*` and `dashboard.*`.
Factory commands fail while the guest is stopped or unreachable; start it
with `ssf vm start` and retry. They never fall back to editing a host
copy. Guest config and credentials live in its persistent home on the data
disk, so a VM restart or a root rebuild preserves them and ordinary edits
need no `ssf vm sync`.

`ssf repo add <owner/repo> --harness <id>` takes `--driver`, `--path`,
`--clone-url`, `--base-branch`, `--command`, `--model`, `--effort`,
`--auto-compaction-tokens`, `--instructions`, `--prompt-file`,
`--allowed-users`, `--accept-anyone-risk` and `--event-comments`.
`ssf repo set` changes those, adds `--git-name`, `--git-email`,
`--git-signing-key` and `--git-credential`, and `--clear <field>` unsets
one (`git` for the whole `[repo.git]` table, `git.credential` for one
key).

### Environment overrides

| Variable | Effect |
|----------|--------|
| `SSF_GITHUB_TOKEN` | the GitHub token to use |
| `SSF_GITHUB_TOKEN_FILE` | a file holding the GitHub token. Precedence: `SSF_GITHUB_TOKEN`, then this file, then `[github].token`, then the token file `ssf auth login` writes, then the gh keyring; a missing or empty file is skipped. A daemon started with `SSF_GITHUB_TOKEN` hands its sessions the token this way (`launch-token`, mode 0600, in the state directory), so it never appears on a pane's command line; a daemon started without it removes that file at start. If the file cannot be written the daemon warns and the session falls back to `[github].token` or the keyring |
| `SSF_CONFIG_DIR`, `SSF_STATE_DIR` | where config and state live; a scratch factory uses its own (see [development.md](development.md)) |
| `SSF_SERVER` | the catalog target or SSH destination to act on |
| `HERDR_COMMAND` | the herdr CLI |
| `SSF_VM_DIR` | the VM guest scripts |
| `SSF_LOG`, `RUST_LOG` | log verbosity, what `--log` reads |

## Server catalog

`config.toml` belongs to one factory. Client-side routing to several
factories is a separate `~/.config/ssf/servers.toml`, written by
`ssf server add`:

```sh
ssf server add local --local
ssf server add ssf-server --vm
ssf server add cloud --ssh ssf@factory.example.com
```

```toml
[servers.local]
transport = "local"
config_dir = "/home/you/.config/ssf-factories/local"
state_dir = "/home/you/.local/state/ssf-factories/local"

[servers.cloud]
transport = "ssh"
destination = "ssf@factory.example.com"

[servers.ssf-server]
transport = "vm"
runtime_name = "factory"
[servers.ssf-server.config]
enabled = true
name = "factory"
dir = "/home/you/.local/share/ssf/vms/ssf-server"
backend = "firecracker"
ssh_port = 2222
```

A `vm` target's nested `config` table holds that VM's `[vm]` keys; read
and change them with `ssf --server NAME config get|set vm.<key>`.

`ssf server list [--json]` and `ssf server show NAME [--json]` inspect the
catalog. They are client-wide and ignore `SSF_SERVER`; passing `--server`
to them is an error. `ssf server remove NAME` refuses while that target's
service is enabled or active, removes only the catalog entry, and reports
the local paths or VM resources it retained. Destroying a VM is a separate
selected operation.

**Selecting a target.** There is no configurable default:

- One entry is selected automatically.
- More than one entry makes an unqualified factory command refuse and list
  the names; use `--server NAME` or `SSF_SERVER=NAME`. The exception is
  `ssf dashboard`, which connects to every entry when neither is set.
- A command-line selector overrides the environment. An unknown catalog
  name is an error and is never tried as an SSH host.
- With no catalog at all, an unqualified command uses the local endpoint
  and `--server HOST` or `SSF_SERVER=HOST` is a raw SSH destination.
- A command the factory runs itself answers for that factory: the daemon
  names its target in the environment of its service and of every session
  pane under it, so `ssf status`, `ssf ui service status` and `ssf doctor`
  there report that factory's own unit and state without `--server`.

**Coexisting targets.** Each VM target needs a distinct runtime name, an
absolute non-overlapping `config.dir` and distinct host ports; Firecracker
also reserves the adjacent port used while provisioning (normally
`ssh_port + 1`), so leave a gap between VM ports. An explicitly configured
Firecracker `rootfs` must be an absolute path disjoint from every other
target. Lima derives distinct instance and disk identities from the
runtime name. A namespaced local target must set both absolute paths, and
they cannot contain `..`, duplicate or overlap another target's paths.
`ssf server add` picks safe defaults and validates the whole resulting
catalog before writing it.

**Services.** Lifecycle and background service commands are target-aware:
`ssf --server NAME ui service enable` enables `ssf@NAME.service` on Linux
and a private `dev.ssf.server.NAME` launchd agent on macOS, each with its
own journal or `~/Library/Logs/ssf/NAME.log`. `ssf setup` with no selector
creates the conventional sole VM target `ssf-server`;
`ssf --server NAME setup` prepares a selected local or VM target. Package
upgrade restarts active instances, and package removal verifies, stops and
disables each one before removing the binaries, preserving factory data.

```toml
[daemon]
poll_interval_secs = 30

[[repo]]
name = "owner/repo"
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
| `github.auto_accept_invitations_from` | `[]` | GitHub logins whose pending repository invitations the server accepts automatically, matched case-insensitively (an optional leading `@` is ignored). Acceptance only grants account access: it does not add a `[[repo]]`, select a named server, or enable a service |
| `git.name`, `git.email` | the bot's login and email | Author and committer of the agents' commits, when a person rather than the bot (see [Committing as a person](identity-and-bylines.md#committing-as-a-person-while-gh-stays-the-bot)); both or neither. `gh` stays the bot |
| `git.signing_key` | the bot's key for the bot, unsigned for a person | SSH key to sign commits and tags with (a path), or `false` for unsigned |
| `git.credential` | `bot` | Who pushes over HTTPS: `bot` (the bot's token), `token:<login>` (the token gh holds for that account where the agents run), `file:<path>` (a token file), or a git credential helper string used as `credential.helper`. SSH remotes always use the bot's key |
| `driver` | `herdr` | What runs the agents. Herdr is the only supported value (see [Workspaces and terminals](drivers.md)); the key may be left unset |
| `auto_compaction_tokens` | `300000` | Context a session's harness may fill before it compacts its own history, in tokens, for every repository that does not set its own (`repo.auto_compaction_tokens`). Applied to the harnesses that take such a setting (`claude`, `codex`, `omp`, `grok` and `opencode`) and ignored by the rest, so one instance value can sit above a mixed set of repositories. `0` leaves each harness's own default alone (see [Context compaction](harnesses.md#context-compaction)) |
| `herdr.command` | `herdr` | The herdr CLI (a herdr session must be running) |
| `herdr.projects_dir` | `~/ssf/projects` | Where ssf clones repositories for the herdr driver; worktrees go in `<name>.worktrees/` next to the clone |
| `herdr.tui_idle_timeout_ms` | `90000` | How long a freshly started agent gets to show up in its pane |
| `dashboard.enabled` | `false` | Enable the optional server web UI; restart required |
| `dashboard.bind` | `127.0.0.1` | Loopback or Tailscale address (100.64.0.0/10, fd7a:115c:a1e0::/48) only; any other exposure requires an authenticated TLS reverse proxy |
| `dashboard.port` | `8787` | Server web UI port; restart required |
| `daemon.poll_interval_secs` | `10` | GitHub poll interval (unchanged listings cost nothing against the rate limit) |
| `daemon.include_own_events` | `false` | Deliver the bot's own commits and cross-references, and each session's posts back to it in live messages (normally noise; a session started again is always shown its own posts in the catch-up story; see [Identity and bylines](identity-and-bylines.md)) |
| `daemon.ignored_events` | `["mentioned", "subscribed", "unsubscribed"]` | Timeline event types that are never delivered |
| `daemon.max_body_chars` | `8000` | Longest comment body quoted in a prompt, in characters |
| `daemon.first_prompt_max_events` | `50` | Most timeline events a session's first message carries, newest first; `0` is no limit. What is left out is never delivered later, and the message says so and where to read it (see [What the agent is told](prompts.md#the-messages-an-agent-receives)) |
| `daemon.first_prompt_max_chars` | `32000` | Character budget for those events together, spent newest first so the newest is always included; `0` is no limit |
| `daemon.instructions` | | Extra instructions appended to every initial prompt |
| `daemon.resume_on_start` | `true` | Start interrupted sessions again when the daemon starts (see [Resume and restarts](internals.md#resume-and-restarts)) |
| `daemon.startup_driver_wait_secs` | `120` | How long to wait for herdr at daemon start before the first poll |
| `daemon.allowed_users` | the collaborators with push access | GitHub logins whose assignments, mentions, review requests, labels and comments the agents act on (see [Who may drive the factory](#who-may-drive-the-factory)); `["*"]` is anyone and needs `daemon.accepted_anyone_risk = true` |
| `daemon.accepted_anyone_risk` | `false` | Written next to a `["*"]` list by `ssf config set ... --accept-anyone-risk`; a wildcard without it is refused at load |
| `daemon.event_comments` | `true` | Post the daemon's essential events on the item as fenced `ssf` blocks: a session attached, resumed, blocked and unblocked, given up on, handed over, its workspace released (see [What ssf says on the item](sessions.md#what-ssf-says-on-the-item)); `false` posts nothing and changes nothing else |
| `daemon.item_pane_input` | `false` | Let a person type into an item session's agent pane from the web pane mirror (`api/pane/input`, the extension's terminal). Off, an item's pane is view-only and its agent is spoken to by commenting on the item; scratch sessions always take typing (see [the dashboard guide](dashboard.md#write-endpoints)) |
| `daemon.scratch_release_grace_hours` | `24` | How long a released (killed) scratch session is kept before the factory forgets it: its record leaves `ssf status` and the extension's list, and `ssf scratch resume` no longer knows it. Its branch and the harness's own transcript stay where they are. `0` keeps released scratch sessions for ever (see [Scratch sessions](sessions.md#scratch-sessions)) |
| `daemon.conflict_check_interval_secs` | `300` | Interval between base fetches and committed-branch conflict checks for active sessions; `0` disables. One fetch per repository, with merge simulations only for changed commit pairs (see [Branch conflicts](sessions.md#branch-conflicts)) |
| `vm.enabled` | `false` | Run the whole factory inside a VM (see [Inside a VM](vm.md)); `ssf-server` then starts and watches the VM, and daemon-facing client commands run in the guest |
| `vm.backend` | Firecracker on Linux, lima on macOS | `firecracker`, `lima` or `incus`: what runs the guest (see [Backends](vm.md#backends-and-host-prerequisites)); unset, `ssf vm build` writes the platform's default here. `incus` (Linux only, for hosts without KVM) runs the guest as an unprivileged system container that shares the host kernel: not a VM, and weaker isolation |
| `vm.name`, `vm.dir` | `default`, `~/.local/share/ssf/vm` | The VM's name and where the image, kernel, binaries and each VM's files live (`<dir>/<name>/`); under lima the instance is `ssf-<name>` and its data disk `ssf-<name>` in lima's home, and `name` is then at most 7 characters, since lima labels the disk's filesystem `lima-<disk>` and an ext4 label holds 16 |
| `vm.vcpus`, `vm.mem_mib` | chosen from the machine | The guest's size; unset, `ssf vm build` writes the host's CPUs minus one (at least 2) and half its RAM in MiB (at least 4096) here (see [Size](vm.md#size)) |
| `vm.data_gib` | chosen from the machine | The persistent data disk (state, clones, worktrees) in GiB, sparse; unset, `ssf vm build` writes half the free space of the filesystem that will hold the disk (at least 20) here, and prints the path it measured: `vm.dir` under Firecracker, lima's own disk directory (`$LIMA_HOME/_disks`, by default `~/.lima/_disks`) under lima, since that is where lima keeps its disks; `ssf vm grow` enlarges it later |
| `vm.root_gib` | `8` | The root image `ssf vm build` makes; under lima the instance's root disk, at least 20 whatever is set |
| `vm.ssh_port` | `2222` | Where the guest's sshd is published on `127.0.0.1` |
| `vm.files` | `[]` | Host files copied into the guest at every start (`src` or `src:dest`). Factory configuration, bot credentials, `.gitconfig` and data-disk destinations are protected and refused. Copies an existing harness login in (`~/.claude/.credentials.json`) as the same session as yours; `ssf vm login` makes the guest its own, see [Harness logins](vm.md#harness-logins) |
| `vm.firecracker`, `vm.gvproxy`, `vm.kernel`, `vm.rootfs` | under `vm.dir` | Firecracker only: use binaries or images of your own instead of the downloaded ones |
| `vm.limactl` | `limactl` on `PATH` | lima only: the `limactl` binary to drive the instance with; `ssf vm build` checks the version it needs and says why (see [Backends](vm.md#backends-and-host-prerequisites)) |
| `vm.image` | lima: Arch's cloud image on x86_64, Ubuntu LTS on aarch64; incus: `images:ubuntu/24.04` | lima: a cloud-init image (URL or path; Arch or Debian/Ubuntu) to boot instead of the default for the guest's architecture; incus: an Incus image (Debian or Ubuntu based) to launch instead |
| `vm.vm_type` | lima's default | lima only: `vz` or `qemu`, passed through to lima (`vz` is macOS only) |
| `vm.guest_binary` | this client on a Linux host, else the matching release asset fetched with `gh` | A Linux `ssf` client to seed into the guest; its matching `ssf-server` build must be beside it (`ssf-server`, or the corresponding versioned release-asset name) |
| `vm.herdr` | the host's own `herdr` on a Linux host, else herdr's latest Linux release downloaded by the guest while provisioning | lima only: a Linux herdr binary for the guest; installed when the guest is provisioned, so a change needs `ssf vm reset` |
| `repo.name` | | `owner/name` on GitHub (required) |
| `repo.enrolled_at` | set by `ssf repo add` | Enrollment generation used to quarantine allocations already present on the first successful poll; retained by `ssf repo set` and replaced after remove/add |
| `repo.github_id` | enrolled by ssf | GitHub's immutable repository database id; ssf uses it to discover renames and transfers |
| `repo.aliases` | `[]` | Previous `owner/name` values retained by ssf so historical session origin tags still route correctly |
| `repo.harness` | | Agent id (required): `claude`, `codex`, `omp`, `pi`, `opencode`, `gemini`, `copilot`, `grok`, `crush` (`ssf agents` lists them; see [Harnesses](harnesses.md)) |
| `repo.driver` | the top-level `driver` | Optional per-repository declaration; herdr is the only supported value |
| `repo.command` | the agent's permission-free command | Command that starts the agent; overrides the default (see [The launch command and permissions](harnesses.md#the-launch-command-and-permissions)), e.g. `claude --permission-mode acceptEdits` |
| `repo.model` | required by repo add/set when supported | Model id accepted by the agent; `provider/model` for `pi`, `omp` and `opencode`, and `auto` or a model name for `copilot` (`ssf models <agent>` lists known values; other ids pass through) |
| `repo.effort` | required by repo add/set when supported | Effort or thinking level (`ssf agents --json` lists what each agent accepts) |
| `repo.auto_compaction_tokens` | `auto_compaction_tokens` | Context this repository's sessions may fill before the harness compacts its own history, overriding the instance value. `0` leaves the harness's own default alone; `claude` accepts `100000`-`1000000` only, and a count outside that is refused by `ssf repo add`, `ssf repo set`, `ssf config set` and every config load, rather than written and left to keep a session from starting. Dropped when `ssf repo set --harness` switches harness, like the model and effort, since a threshold chosen for one harness need not be one the new harness takes |
| `repo.path` | | Register an existing checkout instead of cloning |
| `repo.clone_url` | `https://github.com/owner/name.git` | Use an SSH URL for private repositories (the bot's enrolled key is used) |
| `repo.base_branch` | the driver's default base for the repository | Base ref for issue worktrees, e.g. `origin/main` |
| `repo.instructions` | | Extra instructions appended to this repository's initial prompts, after `daemon.instructions`; a line or two, anything longer belongs in the prompt file |
| `repo.prompt_file` | `SSF.md` | The SSF agent guidance file (below), relative to the worktree unless absolute or `~/` |
| `repo.allowed_users` | `daemon.allowed_users` | Who may drive this repository, replacing the instance list; `[]` is nobody but the bot, `["*"]` needs `accepted_anyone_risk = true` on the repo |
| `repo.accepted_anyone_risk` | `false` | As `daemon.accepted_anyone_risk`, for a `["*"]` on this repository |
| `repo.event_comments` | `daemon.event_comments` | Whether the daemon posts its events on this repository's items (`ssf repo set <owner/name> --event-comments false`) |
| `repo.item_pane_input` | `daemon.item_pane_input` | Whether this repository's item panes take typing from the web pane mirror |
| `repo.conflict_check_interval_secs` | `daemon.conflict_check_interval_secs` | Conflict-check interval for this repository; `0` disables |
| `repo.first_prompt_max_events`, `repo.first_prompt_max_chars` | `daemon.first_prompt_max_events`, `daemon.first_prompt_max_chars` | How much of this repository's items' activity a session's first message carries, for repositories whose items differ from the instance's (a spec-heavy issue is not a bug thread); `0` is no limit. File-only, like `repo.conflict_check_interval_secs` |
| `repo.git.name`, `repo.git.email`, `repo.git.signing_key`, `repo.git.credential` | the `[git]` table | The same four keys for this repository, each overriding its `[git]` counterpart (a `[repo.git]` table under the `[[repo]]`) |

The daemon enrolls `repo.github_id` on its first successful pass. Every
five minutes it resolves that immutable id through GitHub. If GitHub
reports a new canonical `owner/name`, ssf repairs `repo.name`, retains the
former name in `repo.aliases`, migrates its session state, and updates
SSF-managed checkout remotes before polling the repository again. Rename
or transfer repositories through GitHub normally; no SSF-side rename
command is required.

## The SSF agent guidance file

`SSF.md` at the repository root is the operating contract for ssf-spawned
sessions: how the owner wants them to behave as unattended colleagues on an
item (planning, who decides, posting, review, merging, boards, delegation,
what delivered means). Repository-wide build, test, implementation,
architecture, domain and safety policy belongs in `AGENTS.md`, which
applies however an agent was started. ssf appends `SSF.md` to the
issue-owning main session's first prompt only, never to subagents the
harness creates, so advice meant for the orchestrating agent alone (keep
this context for deliberation, delegate execution) is safe there.
[Writing SSF.md](ssf-md.md) (`ssf skill ssf-md`) is the guide;
[`SSF.example.md`](../SSF.example.md) (installed as
`/usr/share/ssf/SSF.example.md`, and on macOS as
`$(brew --prefix)/share/ssf/SSF.example.md`) is the template.

When an agent is started for an item, ssf reads `SSF.md` from the item's
own checkout (so a PR branch that changes it is seen with its own version)
and appends it under an "SSF agent guidance" heading. The complete
instruction order is `daemon.instructions`, global shared and global
harness guidance, `repo.instructions`, repository shared guidance, then
repository harness guidance. The same text is included when an agent is
started again from scratch; it is not repeated on later messages. No file,
or an empty one, adds nothing, and `ssf doctor` reports a repository whose
SSF guidance is missing, looking for the file through the GitHub contents
API on `repo.base_branch` (else the default branch), so no clone is
needed; an absolute or `~/` `prompt_file` is looked for on the machine
instead. `repo.prompt_file` names another file: a path inside the worktree
(`.github/ssf.md`), or an absolute or `~/` path for SSF guidance you would
rather not commit.

For additional instructions specific to a harness, add `SSF.<harness>.md`
at the checkout root, for example `SSF.codex.md` or `SSF.claude.md`, using
the harness identifier from the configuration. ssf appends this file after
the shared SSF guidance, under its own "Harness guidance" heading. It uses
the harness the session is actually on: the one the driver reports for its
pane when there is one, and the one about to start otherwise, including
after a handover or restart. It includes no other harness's file. These
optional files are independent of `repo.prompt_file`. Missing, empty, or
HTML-comment-only files add nothing; HTML comments are filtered just as in
the shared guidance. `ssf doctor` checks the shared SSF guidance only, and
harness guidance too goes only to the issue-owning main session.

Optional machine-wide context belongs in `~/.ssf/SSF.md`: the operator's
preferences for every session on this factory, such as how the machine is
networked or which local services are available. `~/.ssf/SSF.<harness>.md`
adds machine-wide context for one harness. In VM mode these paths are in
the guest user's home; in host mode in the host user's home. The global
files follow the same missing, empty, HTML-comment filtering, restart and
handover rules; they are optional and `ssf doctor` does not inspect them.
Repository guidance comes later in the prompt so it can refine the broader
machine context.

`repo.model` selects the session's model, not its subagents' models;
subagent preferences belong in this guidance and depend on what the
harness supports (see [Models and effort](harnesses.md#models-and-effort)).

## Who may drive the factory

Everything that reaches the bot on GitHub comes from whoever can write on
the repository, and a comment is relayed straight into a running agent's
terminal. `allowed_users` says whose word counts:

```sh
ssf config set daemon.allowed_users '["alice", "bob"]'
ssf repo set owner/repo --allowed-users alice,bob    # for one repository, replacing the instance list
ssf repo set owner/repo --clear allowed_users
```

- **Unset** (a fresh install): the repository's collaborators with push
  access, which is GitHub's **Write** role or higher (Write, Maintain,
  Admin) in **Settings → Collaborators and teams**. The daemon fetches
  them once per pass (an unchanged answer is a free 304) and `ssf doctor`
  prints the list per repository. If they cannot be fetched and none were
  fetched before, the pass fails for that repository and nothing is acted
  on until the fetch works or a list is configured (see
  [troubleshooting.md](troubleshooting.md#github-access)).
- **A repository list replaces the instance list** rather than extending
  it, so one repository can be narrowed as well as widened; `[]` is nobody
  but the bot. Logins compare case-insensitively.
- **The bot itself always counts**, tagged posts and untagged ones alike
  (whoever types as the bot holds its token).
- **App accounts** such as `github-actions[bot]` are ordinary logins:
  listed explicitly or not at all, and never part of the collaborator
  default.

What the list does: an item only gets a session when an allowed login
asked for it, read from the item's timeline (who assigned the bot, who
mentioned it, who requested the review). One that nobody allowed asked for
is logged once at info level and not read again until it changes; an
allowed user assigning or mentioning the bot later brings it in. On a
running session, events by anyone else are dropped before delivery.
Commits are the one event without a login and pass, since pushing needs
write access to the branch; unassigning or closing still retires a
session, since stopping work is safe. One limit to know: the timeline says
who posted a body or comment, not who edited it, and anyone with write
access can edit anyone's text, so the list is a boundary against the
internet, not a hard one among people who can already push.

`"*"` means anyone on GitHub. It is never accepted silently: `ssf config
set daemon.allowed_users '["*"]'` and `ssf repo set <repo>
--allowed-users '*'` refuse it unless you type `yes` (nothing shorter) to
the risk at the terminal or pass `--accept-anyone-risk`, either of which
writes `accepted_anyone_risk = true` next to the list (setting a plain
list again removes it). A hand-edited file with `"*"` and no marker is
refused at load with the fix spelled out, `ssf status` prints a warning
while the wildcard is in effect, and the dashboards show one. Ask the
person before setting it; it is their repository and their spend. Choosing
the list during installation is step 7 of
[install.md](install.md#7-who-may-drive-the-factory).
