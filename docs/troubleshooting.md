# Troubleshooting

For the agent asked why the factory is not doing what it should. Symptom, check, remedy. Offline: `ssf skill troubleshoot`.

## Triage

Run these in order before reading any table. Most causes are named by the
first two.

```sh
ssf server list                     # which factories this client knows, and their transports
ssf --server NAME doctor            # NAME from the listing; omit --server with no catalog
ssf status                          # tracked items, their sessions, and each repository's last error
ssf vm status                       # only when the factory runs in a VM: running, size, disk use
```

**It worked before and stopped:** run the four commands above, then read [Nothing happens on assignment](#nothing-happens-on-assignment). The usual causes are a service that did not come back after a reboot or upgrade (`ssf ui service status`), a harness sign-in that lapsed (a `BLOCKED:` session in `ssf status`, remedy in [Sessions and delivery](#sessions-and-delivery)), a bot token that was revoked or lost a scope (the first `ssf doctor` lines), or GitHub rate limiting, which the journal reports and which clears on its own.

A good `ssf doctor` ends with `all good`. Every failing line starts `FAIL` and
carries its own remedy; `note` lines are informational and do not fail the run.
`ssf doctor --json` prints the same report as one object,
`{"problems": N, "checks": [{"level": "ok"|"fail"|"warn"|"note", "message": "..."}]}`,
with the same exit status. The daemon also runs it about 30 seconds after it
starts and every 15 minutes after, and `ssf status --json` carries the latest
result's failures and warnings as `doctor` (see [internals](internals.md)).

Logs, on the machine that runs the daemon:

| Where | Command |
| --- | --- |
| Linux service | `journalctl --user -u ssf@NAME.service` (add `-f` to follow) |
| macOS service | `~/Library/Logs/ssf/NAME.log` |
| The guest daemon in a VM | `ssf vm logs -f` |
| The guest's kernel and systemd messages | `ssf vm console` |

Before anything else, confirm you are looking at the right target. A command
without `--server` acts on this machine's own factory, which on a client-only
install is not the factory at all.

## Doctor fails

One row per check `ssf doctor` makes, in the order it makes them.

| Failing line | Check | Remedy |
| --- | --- | --- |
| `config: ...` | the config file parses | fix the named error in `ssf config path`; `ssf config show` prints the effective config with the token redacted |
| `dashboard bind ... is neither loopback nor Tailscale` | an enabled web UI binds somewhere safe | `ssf config set dashboard.bind 127.0.0.1`, or a Tailscale address; see [dashboard bind refused](#dashboard-bind-refused-web-ui-unreachable) |
| `GitHub token rejected` / no token | the bot's token works | `ssf auth login`; see [GitHub access](#github-access) |
| `config expects @X` | the token belongs to the configured bot | `ssf auth login --user X`, or you signed in as the wrong account |
| `bot identity not recorded` | the bot login is in the config | `ssf auth login` |
| `bot SSH key ...` | the signing key exists | without it commits are unsigned and pushes are HTTPS only; re-run `ssf auth login` to recreate it |
| `<driver> driver: CLI ... not found` | the driver binary is on PATH | install it with the command the line prints; a driver in `~/.local/bin` is found, but check the service's PATH if not |
| `<driver>: <error>` | the driver answers | start it; see [herdr not running](#herdr-not-running-or-wrong-version) |
| `<harness> not signed in` | each harness in use is signed in where the daemon runs | run the command the line prints; in a VM, `ssf vm login <harness>` |
| `data disk ... full` | guest disk under 85 % used | see [VM out of disk or memory](#vm-out-of-disk-or-memory) |
| `guest memory: ... short` | the guest has headroom | see [VM out of disk or memory](#vm-out-of-disk-or-memory) |
| `0 repositories configured` | at least one repository is watched | `ssf repo add owner/repo --harness ...`; see [repositories.md](repositories.md) |
| `harness \`X\` installed` | a harness an item is pinned to exists | install it, or hand the item to an installed one with `ssf handover` |
| `<repo>: supported model/effort settings explicit` | the repository names a model and effort its harness supports | `ssf repo set owner/repo --model ... --effort ...`, ids from `ssf models <harness>` and `ssf agents --json` |
| `<repo>: GitHub repository identity ...` | the recorded repository id still matches the name | the repository was renamed or replaced; check on GitHub, and `ssf repo set` the new name |
| `<repo>: allowed users ...` | who may drive is known | the collaborator list could not be read (token scope or access), or set `--allowed-users` explicitly |
| `<repo>: harness \`X\` installed` | the repository's harness exists on the daemon's machine | install it there, or `ssf repo set --harness` to one that is |
| `<repo>: SSF agent guidance ... missing` | `SSF.md` is on the base branch | see [no SSF.md](#repository-and-items) |
| `<repo>: checkout at ...` | the configured checkout exists | expected before the first session, which clones it; otherwise fix `--path`, or `ssf repo set --clear path` and let the next session clone it |
| `<repo>: ... worktrees ... not on origin` | no work is stranded | see [stranded worktrees](#stranded-worktrees) |
| `gh and git and ssf links ... do not all point at this ssf` | the shim directory is intact | expected before the first agent has started: the links are written then. Afterwards, `ssf launch` relinks them when an agent next starts; if it persists, another ssf wrote them |
| `post(s) by the bot arrived without an origin tag` | every bot post came from a session | a person posted as the bot, or the `gh` shim was bypassed; nothing to fix if intentional |
| `factory stopped` | the factory is running | `ssf --server NAME ui service enable`, or start the daemon directly where no service manager is available; see [banner says the service is inactive](#banner-says-the-service-is-inactive-but-the-daemon-is-running) |

### Client and server version skew

`ssf doctor` compares the client's version with the server's. Same major and
minor is compatible. A differing patch prints `WARN` and still passes; a
differing major or minor prints `FAIL` and the client and server must be
brought to the same release.

Remedy: upgrade the one that is behind, then restart the server (`ssf vm
restart` when the factory runs in a VM). Which package command applies is
per platform: see [platform-specifics.md](platform-specifics.md).

## GitHub access

| Symptom | Check | Remedy |
| --- | --- | --- |
| `GitHub token rejected`, or API calls 401 | `ssf doctor` first line about the token | `ssf auth login` and sign in as the bot |
| Doctor says the token belongs to another account | the login in the line | sign out of the wrong account in the browser, then `ssf auth login --user bot-login` |
| The allowed-users check cannot read collaborators; organization repositories invisible | token scopes | the token needs `read:org` for organization membership and `project` for board lookups; re-run `ssf auth login` and grant them when asked |
| Bot comments but cannot push, or cannot open a pull request | the bot's role on GitHub | the person grants `@bot-login` **Write** on `owner/repo` |
| The prompt carries no `Project boards` section | the bot's access to the board | the person grants the bot access to the project (v2) board; the token needs the `project` scope; the daemon logs why the lookup failed |
| Nothing happens and the bot never appears as a collaborator | a pending invitation | invitations are accepted automatically only from logins in `github.auto_accept_invitations_from`; otherwise someone accepts it in the bot's GitHub account. `ssf config get github.auto_accept_invitations_from` |

## Repository and items

| Symptom | Check | Remedy |
| --- | --- | --- |
| Doctor: `SSF agent guidance ... missing` | the file named by `prompt_file` on the base branch | commit an `SSF.md` at the repository root (a minimal one is in [repositories.md](repositories.md#4-ssfmd); `SSF.example.md` is a fuller start) |
| Doctor: guidance missing `on branch X (does the branch exist?)` | `--base-branch` | the base branch does not exist or the file is not on it; fix one of the two |
| Items assigned before enrollment sit idle | `ssf candidates` | that is intended; `ssf adopt owner/repo#N` starts the ones that should run |
| `ssf assign` refused | the item already has a session | `ssf handover` instead |

### Nothing happens on assignment

The daemon runs, `ssf status` shows no session for the item. Check in this
order.

1. **Is the repository watched, on this target?** `ssf repo list`.
2. **Is the bot actually the assignee?** A mention alone in a comment body is
   not an assignment unless it triggers one; check the item's assignee field.
3. **Did the assignment come from an allowed user?** An item assigned by an
   account without push access, or outside `--allowed-users` /
   `daemon.allowed_users`, is logged once and ignored. The journal names it.
4. **Has a poll happened?** `daemon.poll_interval_secs` is 10 s by default.
   `ssf config get daemon.poll_interval_secs`.
5. **Does the item pre-date enrollment?** `ssf candidates`, then `ssf adopt`.
6. **Is the repository erroring?** `ssf status` carries each repository's last
   error.
7. **Is the harness signed in where the daemon runs?** `ssf doctor`.

## Sessions and delivery

| Symptom | Check | Remedy |
| --- | --- | --- |
| A session shows `BLOCKED:` | `ssf status`, or `blocked_sessions` in `ssf status --json` | the line names the harness and the fix. Most often the harness's sign-in lapsed: sign in again (`ssf vm login <harness>` in a VM, the harness's own login on the host) and ssf resumes the session and delivers what it held. See [sessions.md](sessions.md) |
| Blocked with `reason: setup incomplete` | the harness's first-run setup | finish it in the harness itself, then the session resumes |
| Blocked with `reason: could not be started` | the model or effort the item is pinned to | start the harness by hand in the workspace, or `ssf handover` with a model and effort it accepts |
| An item has a `blocked` event saying the session is stuck at a harness question | the pane it names (`ssf vm attach` in a VM, herdr on the host) | the harness is at a screen ssf does not know, and a prompt typed into it would be lost, so the session is held with no time limit (`ssf status` shows it `BLOCKED:`); other sessions carry on. Answer the question in the pane; on the next pass ssf gives the session its first message and delivers what it held. |
| Deliveries queue but never arrive | the delivery record | a write whose receipt the harness never confirmed is held, never resent and never pasted, to avoid duplicating a first prompt. Inspect the journal and the target transcript, then decide. See [drivers.md](drivers.md) and [internals.md](internals.md) |
| Doctor notes Codex native delivery unavailable | the channel | deliveries on an explicit native channel are held; the terminal fallback covers the standalone TUI. See [drivers.md](drivers.md) |
| An item closed but the session lingers | `retirement_held_at` in `ssf status --json` | a trigger (assignment, review request) is still on the item; remove it, or the retirement stays held |

### Release refused

`ssf release` refuses while the tree is dirty, a commit at HEAD is reachable
from no remote-tracking ref, a stash was made on the branch, or an active owner
or pending handover still holds the workspace. The refusal says which.

Remedy: push or commit the work, drop the stash, or cancel the handover
(`ssf handover --cancel`). `--force` releases anyway and the work in the
workspace is lost, so ask the person before using it.

### Stranded worktrees

Closing a herdr tab by hand leaves the git worktree behind with whatever it
holds. `ssf doctor` prints a `WARN` line per repository naming every worktree
with uncommitted changes, or commits on no other branch and not on origin, and
no agent in it.

- For an **active** item: comment on it, and the session starts again in that
  same checkout.
- For a **retired** item, or none: push the branch by hand
  (`git -C <checkout> push -u origin <branch>`), then `ssf purge`.

Do not delete the directory or run `ssf purge --force` first: that loses the
uncommitted changes and leaves commits on a local branch nothing lists.
`ssf purge --dry-run` lists what would go. See
[sessions.md](sessions.md#workspaces-after-close-release-and-purge).

## VM and host

| Symptom | Check | Remedy |
| --- | --- | --- |
| Guest unreachable, commands time out | `ssf vm status` | if not running, `ssf vm start`; if running but the daemon does not answer, `ssf vm logs` then `ssf vm restart` |
| `ssf vm status` says not running and the service is on | the service owns the VM | start the service rather than the VM by hand |
| Data disk over 85 % used (doctor fails) | `ssf vm status` | see [VM out of disk or memory](#vm-out-of-disk-or-memory) |
| Guest memory short (doctor fails) | doctor's `guest memory` line | same section |
| `ssf vm build` fails pointing at ssh, minutes in | a blank data disk | see [lima disk unproven](#lima-disk-unproven-and-the-format-flag) |
| Boot refused naming a `format` flag | lima's copy of the template | same section |
| `ssf uninstall` refuses naming `data.ext4` | the VM backend was switched | see [VM backend switch](#vm-backend-switch) |

### VM out of disk or memory

`ssf doctor` fails the data disk over 85 % used, and the guest memory when it is
short. The guest has no swap, so short memory means the kernel kills processes
rather than running slowly.

Disk: stop the VM (stop the service, or `ssf vm stop` when it was started by
hand), `ssf vm grow`, start it again. Growing keeps what is on the disk and
requires the VM to be stopped. Free space first if growing is not possible:
`ssf purge` removes the workspaces of closed items whose agent is gone.

Memory: `ssf config set vm.mem_mib N` (acts on the host side of a VM target) and `ssf vm restart`.
Budget about one vCPU and 2 GiB per parallel session.

### Lima disk unproven and the format flag

Only the build that first creates a lima data disk may hand lima `format:
true`. If a build dies before the guest has put a filesystem on that new disk,
the disk is left blank and no later build, start or reset will format it: the
build waits for a mount that never comes and fails minutes later pointing at
ssh. ssf marks such a disk (`<vm.dir>/<name>/disk-unproven`) and names it in the
error.

Remedy: `ssf vm build --force`, which deletes and re-creates that one disk. A
disk a finished build has used carries no marker and is never deleted this way.

If a boot is refused because the `format` flag is still set somewhere, stop the
instance (`ssf vm stop`) and run the command again; `ssf vm build` and `ssf vm
start` repair the flag in ssf's template and in lima's copy while the instance
is stopped. A running instance cannot be repaired in place, because `limactl
edit` refuses one. The error names the copy that still says it. See
[vm.md](vm.md).

### VM backend switch

`[vm] dir` is shared by both backends and `data.ext4` is the Firecracker disk.
Switching `[vm] backend` from `firecracker` to `lima` while keeping `[vm] name`
leaves the Firecracker clones and worktrees in `<[vm] dir>/<name>`, where lima
knows nothing of them. A healthy lima guest then reports a clean machine, so
the check happens on the host: `ssf uninstall` refuses, names the file, and
says what to do.

Remedy: put `[vm] backend` back to `firecracker`, start that VM and recover the
work, then switch. `--force` destroys it. See [uninstall.md](uninstall.md).

## Service, daemon and dashboard

### Service will not enable

A state directory has exactly one engine owner and one process-held lock. A
second service instance, or an `ssf-server --once` run against a state
directory a service already owns, is refused.

Remedy: find what holds it (`systemctl --user list-units 'ssf@*'` on Linux),
stop the one that should not be there, then enable the one that should. Do not
run `ssf-server --once` against a state directory a running service owns.

### Banner says the service is inactive but the daemon is running

Read which fact the banner is about. `SSF service is inactive; showing latest
saved state` means nothing answered on the factory's socket
(`ssf status --json` → `daemon_reachable`), so the page is showing the last
state the daemon saved. Check the daemon first:

```sh
ssf status --json | jq '{daemon_reachable, service_active, service_enabled}'
```

- `daemon_reachable: true` — the factory is running and there is no problem;
  the banner would not be drawn. A page still showing one was loaded before
  the daemon came back, or is reading another target.
- `daemon_reachable: false` and the unit stopped — start the daemon (`ssf
  --server NAME ui service enable`, or `ssf-server` where no service manager
  is available).
- `daemon_reachable: false`, `service_active: true` — the unit is up and its
  process is not answering; read its log (see [operate.md](operate.md#the-background-service))
  and restart it.

`service_active`/`service_enabled` alone never mean the factory is down: a
daemon in a container, under another supervisor, or started by hand is a
running factory. See [dashboard.md](dashboard.md#running-without-systemd).

### Herdr not running or wrong version

`ssf doctor` reports the driver's CLI and whether it is reachable. A missing
CLI is installed with the command doctor prints. A CLI that is present but does
not answer means herdr is not running where the daemon looks: start it there,
which in VM mode is inside the guest (`ssf vm attach` shows the guest's herdr).
A herdr too old for this ssf shows as a driver error naming the capability it
lacks; upgrade it on the machine that runs the sessions.

### Dashboard bind refused, web UI unreachable

An enabled dashboard listener accepts only a loopback address or a Tailscale
address (`100.64.0.0/10`, or IPv6 in `fd7a:115c:a1e0::/48`). A LAN address, a
public address, `0.0.0.0` or `::` is refused when the config is read, and an
enabled listener that cannot bind is an explicit startup error in the journal.

```sh
ssf config set dashboard.bind 127.0.0.1
```

Unreachable but bound: the `Host` header must match the configured bind address
and port, so reach it by that address rather than through another name. See
[dashboard.md](dashboard.md).

## Where remedies are platform-specific

Package names, service managers, and the install commands for herdr and the
harnesses differ per platform. One line each, with the commands, is in
[platform-specifics.md](platform-specifics.md).
