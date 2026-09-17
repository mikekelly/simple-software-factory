# Client CLI

Start with `ssf server list`. The catalog lives on the client in
`~/.config/ssf/servers.toml` (or under `SSF_CONFIG_DIR`). One entry is selected
implicitly. With several entries, choose `ssf --server NAME <command>` or set
`SSF_SERVER=NAME`; there is no persistent default. With no catalog, commands
use the local factory, or an SSH destination supplied by `--server HOST`.

`ssf dashboard` defaults to all catalog entries; repeat `--server` to select
several. Other factory commands operate on one target. `ssf server` manages
the client catalog and rejects an explicit `--server`. `ssf skill` always
prints local bundled guidance, independently of server selection.

## Inspect and configure

```sh
ssf server list
ssf --server NAME doctor
ssf --server NAME status
ssf --server NAME agents
ssf --server NAME models codex
ssf --server NAME repo list
ssf --server NAME config
```

Use `ssf repo add --help`, `ssf repo set --help`, `ssf config set --help`, and
`ssf auth login --help` before mutations. Prefer these validating commands to
hand-editing configuration. Choose the repository's harness, model and effort
with the person; see `ssf skill setup` and `ssf skill config`.

In VM mode normal factory commands go to the guest and fail if it is down.
Use `ssf --server NAME vm status` to inspect host infrastructure; never fall
back to host configuration edits when guest routing fails. SSH targets invoke
the remote `ssf-server` command endpoint; the remote account needs permission
to operate that factory.

## Observe and coordinate

`ssf status --json` provides a snapshot; add `--watch` for a stream.
`ssf dashboard` is the live terminal view. Inside an issue session, `ssf guide`
explains `peers`, `tell`, `sub`, `unsub`, `subs`, `handover`, `assign` and
`release` with that session's context. `ssf skill sessions` provides the full
lifecycle reference. Release and purge protect worktrees holding unpushed
work; resolve that work before removal and do not bypass checks on someone's
behalf.
