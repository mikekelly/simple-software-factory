# Simple Software Factory (ssf): where to start

For an agent helping a person install, configure, operate or repair a factory.
ssf turns GitHub issues and pull requests assigned to a bot account into
coding-agent sessions, one per item, each in its own git worktree and terminal
under herdr. People collaborate with the agents in the item's comments; the
`ssf` client configures and inspects the factory daemon (`ssf-server`)
locally, in a VM, or over SSH.

## Route by what the person asked

| The person wants | Do this |
| --- | --- |
| ssf installed and their first repository watched | `ssf skill setup`. It opens with a resource check that decides between a local VM, host mode and a rented host, then runs to the first issue. Before `ssf` is installed, the same document is `docs/install.md` in the repository. |
| a repository added to a factory that already runs | `ssf skill repo`: access for the bot, choosing harness, model and effort with the person, `ssf repo add`, a minimal `SSF.md`, `ssf candidates` and `ssf adopt`, the first issue. |
| to know what is running, change a setting, upgrade, stop | `ssf skill operate`: targets, `doctor` and `status` before any change, the service, versions. `ssf skill config` for every key; `ssf skill harnesses` for models, effort and what a session runs. |
| something is not working | `ssf skill troubleshoot`: triage sequence, then symptom, check and remedy. |
| an `SSF.md` written or reviewed for a project | `ssf skill ssf-md` (agree the factory's goals and what agents may decide alone with the person first), then `SSF.example.md`; `ssf skill audit` for a bounded review of existing guidance. |
| an always-on assistant that drives the factory for them | `ssf skill liaison`. |
| details for their distro, macOS, a rented host, a harness's quirks, or an old installation | `ssf skill specifics`. Nothing there is needed on the generic path. |

Every topic prints from the executing binary, so it matches the installed
version. They need no configuration, daemon, VM or network; `--server` and
`SSF_SERVER` do not redirect them. When client and server versions differ, run
`ssf skill` on the server machine for its version.

A session that ssf itself started on an issue reads `ssf guide`, not these
topics: the first prompt already carries what that session needs.

## Rules that apply on every route

Read `ssf skill agent` once; the short form:

- Inspect before changing: `ssf server list`, then `ssf --server NAME doctor`
  and `ssf --server NAME status` for the target you are about to touch. One
  healthy target says nothing about another.
- The bot's credentials belong to the bot account, never to the person's
  account. Only `ssf auth` handles them.
- Ask the person before creating accounts, spending money, using `sudo`,
  choosing a model and effort, allowing anyone to drive the factory, or
  passing `--force` to anything. Decide the rest yourself.
- Preserve work: ssf never removes a workspace on its own, and `release` and
  `purge` refuse anything not on origin. Do not bypass that on someone's
  behalf.
- Prefer the validating commands (`ssf repo add|set`, `ssf config set`,
  `ssf auth login`) to editing configuration by hand; `ssf <command> --help`
  before a mutation.

## All topics

| `ssf skill ...` | Document | Covers |
| --- | --- | --- |
| `setup` | `docs/install.md` | fresh machine to first issue, all install paths |
| `repo` | `docs/repositories.md` | adding and configuring a repository, `SSF.md`, first issue |
| `operate` | `docs/operate.md` | targets, inspection, service, upgrade, stopping |
| `troubleshoot` | `docs/troubleshooting.md` | symptom, check, remedy |
| `specifics` | `docs/platform-specifics.md` | distro, macOS, rented hosts, Tailscale, harness notes, older installs |
| `agent` | `docs/agent-guidance.md` | rules for an agent acting for a person |
| `ssf-md` | `docs/ssf-md.md` | writing a repository's `SSF.md` |
| `liaison` | `docs/liaison.md` | an assistant that acts for a person |
| `audit` | `docs/audit.md` | a bounded review of a project's guidance |
| `config` | `docs/configuration.md` | every key, the server catalog, who may drive |
| `harnesses` | `docs/harnesses.md` | models and effort, launch commands, compaction, delivery, sign-in |
| `vm` | `docs/vm.md` | VM lifecycle, sizing, host versus guest |
| `drivers` | `docs/drivers.md` | herdr workspaces and how activity reaches a harness |
| `sessions` | `docs/sessions.md` | ownership, following, handover, release and purge |
| `dashboard` | `docs/dashboard.md` | terminal dashboard and optional web UI |
| `uninstall` | `docs/uninstall.md` | safe removal and retained data |

Not topics, in the repository only: `docs/prompts.md` (what a session is
told), `docs/identity-and-bylines.md` (how posts are attributed),
`docs/internals.md` (polling, delivery, `ssf status --json`),
`docs/development.md` (working on ssf itself).
