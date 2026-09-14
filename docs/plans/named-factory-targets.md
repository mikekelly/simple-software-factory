# Named factory targets and multiple managed VMs

Design specification for [#261](https://github.com/mikekelly/simple-software-factory/issues/261),
2026-09-13.

## Decision

SSF will replace the client's implicit host-versus-one-VM choice with a catalog
of named factory targets. A target is one independently configured factory and
uses one of three transports:

- `local`: a factory whose daemon, configuration and state live on this host;
- `vm`: a factory in an SSF-managed VM on this host; or
- `ssh`: a factory reached through an SSH destination, whether that destination
  is a cloud VM, another computer, or an SSH alias.

The conventional setup is one `vm` target named `ssf-server`. Its public target
name is independent of its shorter hypervisor instance and disk names. With only
that target configured, commands remain as short as they are today:

```sh
ssf status
ssf repo list
ssf vm status
```

The underlying model supports a host-local factory, multiple managed VMs and
multiple SSH factories at the same time. This permits a VM per repository or a
VM per group of repositories without making that complexity part of the normal
setup.

There is deliberately no persistent default-server setting. Implicit selection
is safe only when exactly one target is configured.

## Goals

1. Keep the recommended one-VM installation simple and name it `ssf-server`.
2. Let one client operate any mixture of local, managed-VM and SSH factories.
3. Let several managed VMs run concurrently, each owning an independent set of
   repositories, credentials, state, workspaces and agent sessions.
4. Use the same target selector for factory commands and VM lifecycle commands.
5. Make adding a second target turn an unqualified command into an error, never
   silently redirect it.
6. Preserve existing host and VM installations without moving, rebuilding or
   overwriting persistent data.
7. Keep factory configuration on the machine that owns the factory. The client
   catalog contains routing and host-side VM management data, not a copy of a
   remote or guest factory's configuration.

## Non-goals

- Distributing one factory engine or one repository across several machines.
- Automatically moving a repository, workspace or session between factories.
- Guessing a target from the current Git checkout, repository name, network or
  last-used target.
- Load balancing commands or GitHub items between targets.
- Giving one daemon process shared ownership of several state directories.
- Making remote SSH hosts into SSF-managed VM infrastructure. An SSH target is
  an already operated factory endpoint.
- Replacing explicit resource planning when several VMs run concurrently.

## Terminology

**Factory target** is the internal design term for an independently owned SSF
factory plus the route used to reach it. **Server** remains the user-facing term:
the option is `--server`, the environment variable is `SSF_SERVER`, and catalog
commands use `ssf server ...`.

A target's **public name** is the stable name people type, such as `local`,
`ssf-server`, `crucible` or `cloud`. For a VM target, its **runtime name** is the
backend-specific instance/disk identifier. These names are not required to be
the same. In particular, Lima currently permits only seven characters in
`vm.name` because `lima-ssf-<name>` must fit an ext4 label; this must not prevent
the conventional public name `ssf-server`.

## Selection contract

Selection happens in the client before a local process is executed or an SSH
connection is opened:

| Catalog state | No selector | `--server NAME` or `SSF_SERVER=NAME` |
|---|---|---|
| No catalog/targets | Use the legacy local endpoint | Preserve the legacy explicit SSH-destination behavior |
| Exactly one target | Select that target | Select the named target |
| More than one target | Refuse and list the names | Select the named target |

An explicit command-line option takes precedence over `SSF_SERVER`. Repeating
`--server` is accepted only by commands designed to aggregate factories, initially
`dashboard`. An explicit unknown name is an error before any command runs.

Once a catalog exists, `--server` and `SSF_SERVER` contain catalog names, not
unregistered SSH destinations. This prevents a typo from being treated as a new
host. A later interface may add an unmistakably explicit ad-hoc SSH URI; it is
not required for the first implementation. Removing the final target returns to
the documented no-target local fallback only after an explicit removal command.

Examples with one target:

```sh
ssf status                         # selects ssf-server
ssf --server ssf-server status     # same, explicitly
SSF_SERVER=ssf-server ssf status   # explicit shell/session intent
```

Examples after adding `local`:

```console
$ ssf status
error: multiple SSF servers are configured; select one with --server:
  local
  ssf-server
```

```sh
ssf --server local status
ssf --server ssf-server status
```

The ambiguity check applies to reads as well as writes so scripts do not develop
different selection behavior by command. It is especially important for `repo
remove`, `release`, `purge`, `vm reset`, `vm destroy` and `uninstall`.

### Dashboard

The dashboard keeps repeated explicit selection:

```sh
ssf --server local --server crucible --server cloud dashboard
```

With no `--server` or `SSF_SERVER` selection, `ssf dashboard` connects to all
configured targets (updated by #283). Explicit selectors narrow the dashboard
to the requested targets. With no catalog entries, it uses the local endpoint.

## Client catalog

Routing configuration must be separate from factory configuration because the
client needs it before it knows which factory can answer `ssf config`. The
normative shape is:

```toml
# ~/.config/ssf/servers.toml

[servers.local]
transport = "local"

[servers.ssf-server]
transport = "vm"
runtime_name = "default"
backend = "lima"

[servers.cloud]
transport = "ssh"
destination = "ssf@factory.example.com"
```

The implementation may factor verbose VM settings into a subordinate table, but
the ownership must remain the same. There is no `default`, `preferred`, priority
or last-used field.

Target names:

- are unique and case-sensitive;
- use a conservative portable character set suitable for CLI arguments, paths
  and service-instance escaping;
- cannot be empty, `.` or `..`, contain path separators, or use reserved names;
- are validated before any directory or service name is derived from them; and
- are display identities only: renaming one must not implicitly rename or move
  VM storage.

`ssh.destination` accepts the same SSH destinations as today's `--server`,
including `user@host`, IP addresses and `~/.ssh/config` aliases. SSH continues to
execute `ssf-server __client` and does not expose a new SSF network listener.

The catalog contains no GitHub token, bot signing key or harness credential. File
permissions should nevertheless be private because destinations, local paths and
operational topology may be sensitive. Catalog writes are validated and atomic.

### Catalog commands

Provide a small explicit management surface:

```sh
ssf server list [--json]
ssf server show NAME [--json]
ssf server add local --local
ssf server add ssf-server --vm
ssf server add cloud --ssh ssf@factory.example.com
ssf server remove NAME
```

`server remove` removes only the client registration. It never stops or destroys
a VM, deletes local factory data, removes a remote factory, or silently selects a
remaining target. It reports retained resources and the command that can inspect
or deliberately remove them. A target with an enabled local service or managed
VM should require that service to be disabled first so removal cannot orphan an
unmanaged running writer.

Adding a target validates conflicts with every existing target before writing:
runtime identity, data paths, SSH ports, web-dashboard ports, service identity
and any backend-specific global names.

## Configuration and data ownership

Each target is a separate factory. No mutable factory configuration or state is
shared between two targets.

### Local target

A local target owns distinct host-side paths for:

- factory `config.toml` and token/key material;
- daemon state and its engine lock;
- repository clones and worktrees; and
- daemon logs and service identity.

The selected target context must be passed explicitly to `ssf-server`; process-
global ambient defaults must not allow a daemon for one target to open another
target's paths. Existing `SSF_CONFIG_DIR` and `SSF_STATE_DIR` remain useful for
development and recovery, but are not the normal target-selection mechanism.

### VM target

A VM target has two strictly separated owners:

- The host-side catalog owns VM lifecycle settings and the administration
  material needed to build, start, stop and reach that VM.
- The guest data disk owns that factory's `config.toml`, bot credentials, git
  identity, daemon state, repositories, worktrees and harness logins.

This preserves the single-owner boundary established by
[#220](https://github.com/mikekelly/simple-software-factory/issues/220).
Ordinary factory commands execute against the guest. VM lifecycle commands
execute on the host against the selected target. A stopped or unreachable guest
never causes an edit to a host-side shadow factory configuration.

### SSH target

An SSH target stores only its destination and transport options locally. Factory
configuration, credentials and state remain on the remote endpoint. The local
catalog must not cache an editable copy of them.

## Command routing

The selected target determines both the route and the paths used at the endpoint:

| Target | Factory commands | VM lifecycle commands |
|---|---|---|
| `local` | Selected local daemon/config/state | Refused: target is not a managed VM |
| `vm` | Forward to selected guest | Operate on selected host-side VM |
| `ssh` | SSH to the configured endpoint | Refused initially; manage that host directly |

Thus the selector is consistent even though the execution plane differs:

```sh
ssf --server crucible repo list
ssf --server crucible vm restart
```

Commands that are genuinely client-wide, such as `ssf server list`, do not
require target selection. The implementation must classify every existing
command explicitly as client-wide, factory-plane or target-management-plane;
fall-through to whichever parser happens to run locally is not a routing rule.

`ssf config` continues to mean the selected factory's configuration. Client
catalog changes use `ssf server`, avoiding an ambiguous second meaning for
`config`. VM infrastructure settings are shown and changed through the selected
VM management surface; the implementation may retain compatibility aliases for
`config get|set vm.*` during migration, but new documentation must not teach
them as global settings.

`ssf ui service ...`, `ssf uninstall`, setup completion and the Omarchy widget
currently assume one service. Their target behavior must be made explicit before
multi-target setup is enabled. Destructive operations must name the resources
owned by the selected target and must not widen from one target to all targets.

## Concurrent VM and service requirements

Supporting several VM records in TOML is insufficient. Multiple targets must be
able to run concurrently without sharing mutable runtime resources.

Each managed VM needs a unique:

- backend instance and persistent data-disk identity;
- writable VM directory, PID files, sockets, console and generated seed;
- host SSH forwarding port and SSH control identity;
- host-known-hosts and VM administration key paths;
- daemon/state lock and guest factory data disk; and
- service instance, logs and health result.

Existing installations retain port 2222 and their current runtime names. New VM
targets receive a free, stable port recorded at creation; startup rechecks that
the port is not owned by another process or target and fails by name rather than
silently selecting another port. Public target names are not truncated to make
backend names. Backend-safe runtime identities are generated, persisted and
collision-checked.

Mutable build and boot paths must be per target or protected by an ownership lock.
Large immutable downloads or base images may be shared only when their version,
architecture, integrity and concurrent-build behavior make that safe. Firecracker
network devices and gvproxy processes, Lima instances/disks, dashboard listeners,
OAuth forwarding and Tailscale hostnames all require a multi-instance audit and
tests. A resource collision must stop before either existing VM is changed.

Each factory engine still has exactly one process-held state lock. The natural
runtime is one daemon/supervisor process per locally owned target, using
target-qualified service instances. Linux should use a package-owned templated
user unit or an equivalently inspectable per-target unit. macOS needs an
equivalent launchd/Homebrew-compatible design; merely starting the current one
Homebrew service several times is not acceptable. Package upgrades, service
enable/disable, logs and UI status must preserve and report each instance rather
than treating one process as global truth.

The conventional single `ssf-server` target may keep familiar presentation such
as `ssf.service`, provided that compatibility is an alias for the named instance
and does not create a second daemon. The final unit naming and macOS mechanism
must be decided and tested before enabling a second locally owned target.

## Setup experience

For a fresh supported installation, `ssf setup` establishes the conventional
shape:

1. Create one VM target with public name `ssf-server`.
2. Generate a backend-safe runtime name and allocate its host resources.
3. Keep VM sizing automatic unless the person supplies sizes.
4. Enable only that target's service.
5. Continue with guest-owned bot authentication, repository configuration and
   harness login as today.

Because there is one target, subsequent commands need no `--server`. The setup
output should explain its name without making the selector feel mandatory:

```text
Created the conventional VM server `ssf-server`.
It is selected automatically while it is the only configured server.
```

Advanced setup is additive and explicit. Examples include:

```sh
ssf server add local --local
ssf server add crucible --vm
ssf server add cloud --ssh ssf@factory.example.com
```

After the second target is committed, setup prints that unqualified commands
will now refuse and shows concrete selected commands. It does not create a
default. Per-directory `SSF_SERVER` through a person's shell tooling can provide
convenience, but SSF does not write shell startup files or infer repository
ownership.

`skills/ssf-setup/SKILL.md` and the installed setup documentation must describe:

- the recommended one-VM `ssf-server` path first;
- the zero/one/many selection rule and why no persistent default exists;
- adding and operating a host-local factory;
- adding more managed VMs and assigning different repository sets to them;
- adding SSH/cloud targets;
- explicit selection for lifecycle and destructive commands;
- CPU, memory, disk and port planning for concurrent VMs;
- service, dashboard and troubleshooting commands per target; and
- safe target removal, VM destruction, uninstall and legacy migration.

The setup skill must inspect `ssf server list`, then run `doctor` and `status`
against every relevant selected target before changing an established machine.
It must never infer that one target's clean status makes another target safe to
remove or migrate.

## Compatibility and migration

Migration is data-preserving and idempotent. It records completion only after the
catalog, service routing and selected target all validate. Interruption leaves
the old installation usable or stops with both representations retained.

### Existing host-mode installation

Create one `local` target pointing at the existing config and state paths. Do
not copy or move them in the first migration. Preserve its service state,
credentials, repositories and workspaces. It remains the sole implicit target.

### Existing VM-mode installation

Create one VM target named `ssf-server`. Preserve the existing internal
`[vm].name` (normally `default`), backend, directory, sizes, port, files,
administration key and known-hosts paths. Do not rename/recreate the instance,
reformat or move its data disk, or modify guest-owned factory data merely to fit
the new layout. The guest remains the sole factory-config and state owner.

Legacy host-side VM settings may initially be referenced in place, then copied
to the catalog only after an equality check. Conflicting old and new values stop
with a diagnostic and recovery instructions; recency is not precedence.

### Existing remote invocation

When no catalog exists, `ssf --server HOST ...` and `SSF_SERVER=HOST` retain
their current meaning as direct SSH destinations. Creating the first catalog
entry makes selectors names. Setup and migration output must show how to register
the old destination before that transition.

### Adding local beside an existing VM

This is a primary acceptance scenario. First migrate the existing VM to the
`ssf-server` target and verify its factory, repositories and state. Then create a
new empty `local` target with distinct config, state, project and service paths.
The operation must not start a host-local engine against legacy paths that belong
to the VM supervisor. On the motivating installation, the Crucible repository
and all of its existing state must remain visible under `ssf-server` before the
new local factory is enabled.

### Rename and removal

Renaming a public target changes catalog identity and service selection only
after all references are updated atomically. It does not rename backend resources
or data directories. Target removal and VM destruction remain separate actions.
Uninstall defaults to the selected target; removing the package or every target
requires a separate explicit whole-installation workflow and a report covering
all retained data.

## Diagnostics and machine-readable output

Errors name the unresolved selector and available targets without exposing SSH
credentials. `ssf server list --json` provides stable fields for name, transport,
availability and whether it is the sole implicit selection. It must distinguish
unknown/unreachable from stopped.

`ssf status --json` remains the selected factory's canonical status and adds its
public target name and transport. VM status continues to distinguish an answered
`running: false` from `running: null` plus a probe error. Dashboard groups use
public target names even when a remote server reports the same hostname as
another target.

`ssf doctor` checks the selected target. A client-wide diagnostic mode may iterate
all targets, but failure to reach one must not cause checks or mutations to fall
through to another. Logs and UI controls always label the target they operate.

## Security and safety invariants

1. Selection is resolved and validated before reading or writing target-owned
   factory state.
2. More than one configured target always makes an unqualified target-scoped
   command fail, including destructive commands and scripts using JSON output.
3. No target is selected from last use, current directory, repository name,
   daemon availability or iteration order.
4. A stopped, unreachable or invalid target never falls back to another target
   or to host operation.
5. A target owns disjoint mutable config/state/workspace and VM runtime paths.
6. One state directory has one engine owner and one process-held lock.
7. Creation validates derived paths, ports and backend names against all targets
   before making resources.
8. Removal of a catalog entry is not destruction of its data or VM.
9. Migration preserves both sides on conflict and never chooses by timestamp.
10. Uninstall, reset, destroy and forced cleanup report the exact public target
    and resolved resources before confirmation.
11. SSH host-key and account authorization protections are not weakened to make
    target enrollment convenient.
12. Tests and scratch instances cannot resolve to the user's real catalog,
    configuration, state, services or VM storage.

## Delivery sequence

Implement this in reviewable stages, while keeping released states internally
consistent:

1. **Catalog and resolver:** validated target types, zero/one/many selection,
   `ssf server list/show`, explicit errors and legacy no-catalog routing.
2. **Target context:** make local command execution, paths, status and JSON carry
   an explicit target; add isolated local targets and tests without enabling
   multiple background services by default.
3. **VM target extraction:** move singular host-owned `[vm]` settings behind a
   selected VM target, preserve the guest ownership boundary, and migrate an
   existing VM in place.
4. **Concurrent VMs:** isolate ports, names, mutable build/runtime paths and locks;
   qualify Firecracker and Lima with two simultaneously running factories.
5. **Per-target services and UI:** package and manage concurrent daemon instances
   on Linux and macOS; make service controls, logs, widget and uninstall target-
   aware.
6. **Conventional setup and documentation:** make fresh setup create the sole
   `ssf-server` VM target; update the setup skill and all user-facing examples.

If an intermediate release cannot safely supervise multiple local targets, its
catalog validation must refuse enabling that combination rather than accepting a
configuration it cannot operate.

## Acceptance criteria

1. A fresh standard setup creates one VM target named `ssf-server`; unqualified
   factory and VM commands select it.
2. A catalog with one target of each transport, tested separately, selects that
   target without `--server`.
3. A catalog with two or more targets makes every target-scoped unqualified
   command fail before transport, config, state, service or VM mutation and lists
   valid names.
4. `--server NAME` and `SSF_SERVER=NAME` select the same target; the option wins
   when both are present. Unknown names fail without being treated as SSH hosts.
5. Repeated selectors work for `dashboard`; one failed stream does not affect the
   others, and headings use catalog names.
6. A local target and a VM target run concurrently with different configuration,
   state locks, workspaces, services and logs.
7. Two Firecracker VM targets and two Lima VM targets can each be built, started,
   queried, stopped and restarted without port, path, PID, socket, instance, disk,
   seed, key or log collisions.
8. `ssf --server NAME vm ...` operates only on that managed VM. It refuses local
   and SSH targets without contacting or changing another VM.
9. Repositories configured on one target are absent from the others unless added
   there explicitly. The same GitHub repository may be configured on two targets
   only through an explicit action and produces a prominent ownership warning;
   no automatic deduplication or transfer is inferred.
10. Existing host-mode migration creates one `local` target and preserves all
    paths and behavior.
11. Existing VM-mode migration creates the public `ssf-server` target while
    preserving its runtime identity, service state, guest configuration,
    credentials, data disk, repositories, state, worktrees and harness logins.
12. Interrupted migration retries idempotently. Conflicting representations stop
    with both retained and actionable diagnostics.
13. Adding a new `local` target beside the migrated VM leaves the established VM
    factory unchanged; the Crucible installation is the real-machine validation
    case before general release.
14. Removing or renaming a target does not destroy or relocate its VM or factory
    data. Destruction remains an exact, separately confirmed action.
15. Package upgrades restart or preserve every enabled target service without
    accidentally starting disabled targets or reverting to a global daemon.
16. Setup, configuration, VM, dashboard, internals, uninstall and troubleshooting
    documentation, `config.example.toml`, README examples, and
    `skills/ssf-setup/SKILL.md` agree on the target model and selection rules.
17. Automated coverage includes parser/resolver tables, path and name validation,
    service isolation, dashboard routing, stopped/unreachable targets, migration
    conflicts, destructive-command ambiguity and test-environment isolation.
18. Final implementation changes pass `cargo test`, `cargo fmt --check`,
    `cargo clippy --all-targets`, and the packaging build required by repository
    policy when units, installation or packaging change.

## Open implementation decisions

These choices do not alter the user-visible contract, but must be resolved before
their delivery stage:

- the exact per-target directory layout while retaining in-place legacy paths;
- Linux template-unit naming and the macOS multi-instance service mechanism;
- whether conventional `ssf.service` is an alias, generated instance or retained
  compatibility wrapper;
- the exact subcommands for editing VM infrastructure fields after creation;
- stable automatic SSH-port allocation and reservation;
- the explicit syntax, if any, for ad-hoc SSH after a catalog exists; and
- whether duplicate repository enrollment across targets is warning-only or
  requires a dedicated acknowledgement flag.

None of these decisions may introduce a persistent default target or weaken the
zero/one/many selection invariant.
