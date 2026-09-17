---
name: working-with-ssf
description: Work with Simple Software Factory (ssf) — operating a factory, working as an ssf-spawned agent on a GitHub issue, acting as a liaison for a person, writing SSF.md project guidance, auditing project guidance, and installing or upgrading a factory.
---

# Working with Simple Software Factory (ssf)

SSF turns GitHub issues and pull requests assigned to a bot account into coding
agent sessions in managed workspaces on herdr. People drive it by commenting on
GitHub; an `ssf` client controls a factory daemon locally, in a VM, or over SSH.

Install this skill globally so every project's agent can find it:

```sh
npx skills add mikekelly/simple-software-factory -g
```

Drop `-g` to install it for one project, or add `--skill working-with-ssf -y`
to skip prompts. The [skills CLI](https://github.com/vercel-labs/skills)
installs the skill for each detected harness.

## Read the running binary, not this file

This skill is a thin pointer on purpose, so it cannot drift from the version
you have. Once `ssf` is on `PATH`, `ssf skill` prints the overview bundled with
the executing binary, and `ssf skill <topic>` prints one topic: `setup`,
`agent`, `liaison`, `client-cli`, `server`, `config`, `vm`, `headless`,
`install-binaries`, `drivers`, `sessions`, `dashboard`, `uninstall`. They need
no configuration, daemon, VM, or network access, and `--server` or
`SSF_SERVER` does not redirect them. Inside a factory session, `ssf guide` is
the context-aware collaboration reference.

## Where to start

| Situation | Start with |
| --- | --- |
| Installing or upgrading a factory | [Install](https://github.com/mikekelly/simple-software-factory#install) in the repository, then `ssf skill setup`; `ssf skill headless` or `ssf skill install-binaries` for a VPS/container or a client-only host; `ssf doctor` after an upgrade |
| Operating a running factory | `ssf skill server`, `ssf skill client-cli`, `ssf skill config`, `ssf skill dashboard` |
| Working as an ssf-spawned agent on an assigned issue | `ssf guide`, then `ssf skill agent` |
| Acting as a liaison for a person, on the factory host or from their machine | `ssf skill liaison` |
| Writing `SSF.md` for a project or a factory | `ssf skill setup` (its `SSF.md` step) and [SSF.example.md](https://github.com/mikekelly/simple-software-factory/blob/master/SSF.example.md) |
| Auditing a project's existing agent guidance | [Bounded project guidance audit](https://github.com/mikekelly/simple-software-factory/blob/master/docs/audit.md) |

## Acting as a liaison

A liaison drives the factory on a person's behalf. Work out which side it runs
on first; that is what decides what it needs:

- **On the factory host** — a Grok Bot, Hermes or OpenClaw on the same VPS as
  `ssf-server`: the `ssf` client there is the whole setup, with no catalog entry
  and no SSH. Its GitHub access is still its own, not the bot's `ssf auth`.
- **On the person's machine, factory elsewhere**: it needs a key that reaches
  the factory account for `ssf` commands (`ssh user@factory 'command -v
  ssf-server'` is the check; every command is a non-interactive `ssh`), a
  client-only `ssf` install if the machine has none, `ssf server add factory
  --ssh user@factory.example` to name the destination, and `herdr machine add
  user@factory.example --label factory` so the factory's herdr server is managed
  from the local herdr. Answer No if that asks to replace the factory's running
  server: those panes are the factory's live agent sessions.

Either way, the liaison's GitHub access and event delivery are configured
separately from the factory bot's. `ssf skill liaison` has both setups in full
and links the [guide](https://github.com/mikekelly/simple-software-factory/blob/master/docs/liaison.md).

Release packages cover Arch-family and Debian-family Linux, including Omarchy
and Ubuntu on x86_64; macOS support is planned but not supported yet. Platform
requirements, setup choices and step-by-step installation live in the
repository and in `ssf skill setup`, not here.
