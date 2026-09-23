# Operating a factory

For an agent looking after a factory that already runs for someone: how to
select which factory you are talking to, what to inspect before changing
anything, the everyday commands, the background service, upgrades, and how
to stop. Read `ssf skill repo` (docs/repositories.md) for adding
repositories and `ssf skill troubleshoot` (docs/troubleshooting.md) when
something is wrong.

## Targets: which factory a command reaches

A factory is a server. The client computer keeps a catalog of the servers it
can reach in `~/.config/ssf/servers.toml` (under `SSF_CONFIG_DIR` when that
is set). The catalog belongs to the client and is separate from any
factory's own configuration.

```sh
ssf server list
ssf server show NAME
```

`ssf server list` prints each configured name and its transport, or says
that none are configured. A target is one of three transports: `local` (a
factory on this computer), `vm` (a managed VM on this computer), or `ssh`
(a factory already running on another machine).

```sh
ssf server add NAME --local          # a factory on this computer
ssf server add NAME --vm             # a managed VM on this computer
ssf server add NAME --ssh DESTINATION # a factory reached over SSH
ssf server remove NAME               # forget it; its data and VM stay
```

`--local` takes `--config-dir` and `--state-dir` together, to give that
factory its own isolated trees; `--vm` takes `--runtime-name`, `--vm-dir`
and `--ssh-port`. `ssf server remove` only drops the catalog entry: it never
deletes factory data or destroys a VM, and refuses while that target's
service is enabled or active.

Selection rules:

| Catalog | Command | Goes to |
|---|---|---|
| no catalog | `ssf status` | the local factory |
| no catalog | `ssf --server HOST status` | `HOST` as a raw SSH destination |
| one entry | `ssf status` | that entry, implicitly |
| several entries | `ssf status` | refused; it lists the names |
| several entries | `ssf --server NAME status` | `NAME` |

`SSF_SERVER=NAME` does the same as `--server NAME`. There is no persistent
default: with several entries, every command names one.

`ssf dashboard` is the exception. With no `--server` it opens every catalog
entry at once; name one to watch just that factory. Every other command
takes a single target, and refuses more than one.

Two commands ignore server selection. `ssf server ...` manages the client's
own catalog and errors with `--server does not apply to 'ssf server'`.
`ssf skill TOPIC` always prints the bundled local guidance and contacts no
server. `ssf vm ...` refuses a target whose transport is not a managed VM,
and `ssf uninstall` refuses a named VM or namespaced local target: it is
still installation-wide.

## Inspect before you change anything

Establish what exists before touching it. One healthy target says nothing
about another, so check each name you intend to work on.

```sh
ssf server list
ssf --server NAME doctor
ssf --server NAME status
```

`ssf doctor` checks GitHub credentials, the drivers in use and the
configured harnesses, and names a remedy for each failure; rerun it after
the change. `ssf status` lists tracked issues and their workspaces joined
with what the driver reports for each agent session; `--json` gives one
snapshot and `--watch` a stream of them.

```sh
ssf --server NAME repo list
ssf --server NAME config
ssf --server NAME config get daemon.poll_interval_secs
ssf --server NAME vm status     # VM targets only
```

`ssf vm status` says whether the VM runs, whether its daemon answers, its
size and how full the data disk is. On a VM target the ordinary factory
commands run in the guest and fail when it is down: start it with
`ssf vm start` and retry, rather than editing host configuration as a
fallback.

Prefer the validating commands to hand-editing files: `ssf repo set` and
`ssf config set` check what you give them, and `ssf repo add --help`,
`ssf repo set --help` and `ssf config set --help` list the keys each
accepts. Every key is in `ssf skill config` (docs/configuration.md).

Repository and daemon settings are picked up on the daemon's next poll; no
restart is needed. Service settings, VM settings and a new `ssf` binary
need a restart (`ssf ui service ...`, or `ssf vm restart` for a VM).

## Everyday commands

```sh
ssf status | ssf dashboard | ssf peers   # what is running, and what each agent is doing
ssf repo add owner/repo ... | ssf config set ...   # configure; picked up on the next poll
ssf assign 12 --harness codex | ssf handover 12 --harness claude  # start an item; pass one on
ssf sub 12 | ssf unsub 12 | ssf subs      # follow an item's activity from this session
ssf release | ssf purge                   # give a workspace back; sweep those of closed items
ssf doctor                                # what is missing, and which checkouts still hold work
```

One line each:

- `ssf status` tracked items and their workspaces; `--json`, `--watch`.
- `ssf dashboard` the live terminal view, across one or every target.
- `ssf peers` the sessions on a repository: item, GitHub state, agent
  state, branch, last message.
- `ssf candidates` items allocated before this factory enrolled the
  repository; they stay idle until adopted.
- `ssf adopt owner/repo#N` starts those, replaying the item's GitHub
  history into a fresh session.
- `ssf token` prints the bot's GitHub token, for
  `GH_TOKEN="$(ssf token)" gh ...`.
- `ssf sub 12` / `ssf unsub 12` / `ssf subs` follow an item without working
  on it, stop following, list what is followed. A follow hears the item's own
  state changes by default and not what is said on it; `ssf sub 12 --events
  all` adds comments, reviews and commits, and `ssf subs` shows the level.
  See [Subscriptions](sessions.md#subscriptions-and-cross-session-comments).
- `ssf assign 12 --harness ID` assigns the bot and starts the item's first
  session on that stack; it refuses an item that already has one.
- `ssf handover 12 --harness ID` moves an item to a new session on another
  harness, model or effort in the same workspace.
- `ssf release` gives a workspace back once everything is on origin.
- `ssf purge` removes the workspaces of closed items whose agent is gone;
  `--dry-run` lists only.

Safety rules that hold everywhere:

- ssf never removes a workspace on its own. Closing an item tells the agent
  to push, comment and `ssf release`; `ssf purge` is the sweep for what was
  left behind. A herdr tab closed by hand leaves its checkout, and
  `ssf doctor` names every one that still holds work.
- `ssf release` and `ssf purge` refuse work that is not on origin: the tree
  must be clean, every commit at HEAD reachable from a remote-tracking ref,
  and no stash on the branch. `--force` removes it anyway and the work is
  lost. Never pass `--force` on the person's behalf without asking them
  first; resolve the unpushed work instead.
- Ask before spending, before `sudo`, and before choosing a model or effort
  level for them.

`ssf guide`, run inside a session, explains the same commands with that
session's context; `ssf skill sessions` (docs/sessions.md) is the full
lifecycle reference.

## The background service

Each target runs its own service, controlled through the selected target:

```sh
ssf --server NAME ui service status
ssf --server NAME ui service enable    # run at login, and start now
ssf --server NAME ui service disable   # stop, and keep it from starting at login
ssf --server NAME ui service toggle
```

`ssf ui service is-enabled` exits 0 when it is enabled, for scripts and menu
conditions.

These commands, and this table, are about the service manager's unit. The
factory is the daemon: `ssf status`, `ssf doctor` and the dashboards read
whether one answers on `ssf.sock`, so a daemon started outside the unit — in a
container, under another supervisor, or in the foreground — is a running
factory with no warning, and the unit's own state is reported as the detail
beside it (#463). `ssf ui service status` keeps reporting the unit alone.

| Platform | Unit | Logs |
|---|---|---|
| Linux | `ssf@NAME.service` (systemd user) | `journalctl --user -fu ssf@NAME.service` |
| macOS | launchd agent `dev.ssf.server.NAME` | `~/Library/Logs/ssf/NAME.log` |

One state directory has exactly one engine owner. Do not run
`ssf-server --once` against the state of a running factory: it refuses a
second owner, and in VM mode the guest refuses it while its own service
holds the state. Let the next poll do the work instead. Disable a target's
service before removing its catalog entry.

Restarts are invisible to agents: a daemon restart delivers what was missed
when it comes back, and a reboot relaunches the interrupted sessions. Agents
keep running when the service stops; nothing tears their workspaces down.

In VM mode the two sides own different things:

| Host | Guest |
|---|---|
| VM administration settings (`vm.*`), the dashboard listener, the SSH key that reaches the guest | factory state, bot credentials, repositories, `[github]`, `[git]`, `[daemon]` and driver settings |

`ssf config get|set vm.<key>` and `dashboard.<key>` act on the host;
everything else runs in the guest over SSH, and paths in those arguments
are guest paths.

For a scratch daemon during development, follow docs/development.md: config
and state overrides alone do not isolate credentials, drivers or workspaces.

## Versions and upgrading

The client and the server it talks to should be the same release.
`ssf doctor` prints both and judges them: identical passes; a difference in
the patch component alone is a warning; a different major or minor version
fails, because the command surface may have changed between them.

Upgrade both sides to the same release:

1. Upgrade the package on the machine that runs the client (the package
   manager on Linux, `brew upgrade ssf` on macOS, or replacing the
   standalone binaries). Ask before running `sudo`. Package upgrades
   restart active target services.
2. For an SSH target, upgrade the remote machine the same way.
3. For a VM target, `ssf --server NAME vm restart` picks up the new `ssf`
   binary in the guest. `ssf skill vm` (docs/vm.md) covers the rare cases
   that need a rebuilt guest root.
4. Run `ssf --server NAME doctor` again; it should look as it did before.

Configuration, state, keys and the VM's disks survive an upgrade. If the
person is upgrading an installation that predates named servers, see
[Upgrading from an older ssf](platform-specifics.md#upgrading-from-an-older-ssf).

## Stopping

```sh
ssf --server NAME ui service disable
```

That stops the service and keeps it from starting at login. Running agents
are left where they are; their workspaces and everything on GitHub are
untouched, and enabling the service again resumes delivery.

To take the machine back to just the package, or off it entirely, read
`ssf skill uninstall` (docs/uninstall.md).
