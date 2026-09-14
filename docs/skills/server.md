# Server operation

Supported release packages are for Arch-family Linux (including Omarchy) and
Debian-family Linux (including Ubuntu). macOS support is planned next;
macOS-specific service details below document work in progress.

`ssf-server` runs the factory engine. The `ssf` client invokes its command
endpoint locally or over SSH; managed VMs run the engine in the guest.
Use `ssf skill setup` for packaged setup, `ssf skill headless` for foreground
host operation, and `ssf skill vm` for the VM lifecycle.

## Inspect before changing

Run `ssf server list`, then `ssf --server NAME doctor` and
`ssf --server NAME status` for each target. Without a catalog use unqualified
commands. One healthy target says nothing about another. `ssf doctor` checks
configuration, authentication, drivers and harness readiness; follow the named
remedy and rerun it after the relevant change.

Doctor also reports the invoking client and selected server versions. Exact
versions pass. A patch-only difference within the same major and minor release
is a warning; a major or minor difference fails the check. Update either side
to the same release and restart the server (or `ssf vm restart` for a managed
VM), then run doctor again. With multiple catalog targets, select and check
each name independently.

The client catalog is separate from factory configuration. Local defaults use
isolated config/state trees; VM targets own their VM resources. Repository
settings are picked up on the next poll. Service and VM changes can require a
restart. See `ssf skill config` for settings and `ssf skill agent` for agent
operating rules.

## Service ownership

For a selected local or VM target, use:

```sh
ssf --server NAME ui service status
ssf --server NAME ui service enable
ssf --server NAME ui service disable
```

Linux uses `ssf@NAME.service`; macOS uses `dev.ssf.server.NAME` and logs to
`~/Library/Logs/ssf/NAME.log`. Before enabling the first target service, inspect
and stop the legacy singleton (`systemctl --user disable --now ssf.service`
or `brew services stop ssf`). SSF refuses target enablement while that
singleton is active or enabled. Disable a target service before removing its
catalog entry. Catalog removal does not destroy its VM or local data.

A state directory has one engine owner. Do not run `ssf-server --once` against
a running factory's state; it refuses a second owner. In VM mode the guest
owns the factory state, bot credentials and repositories; only VM administration
settings and its SSH key belong on the host. For development, follow
`docs/development.md` in the repository before running a scratch daemon:
config/state overrides alone do not isolate credentials, drivers or workspaces.

The optional web UI is off by default. See `ssf skill dashboard` for its
loopback listener and authenticated remote access requirements. Prefer
`ssf dashboard` for a terminal view.
