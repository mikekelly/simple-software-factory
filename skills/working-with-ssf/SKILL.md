---
name: working-with-ssf
description: Work with Simple Software Factory (ssf) on a person's behalf: install a factory and watch its first repository, add a repository, operate and troubleshoot a running factory, write or audit a project's SSF.md, act as a liaison. Use when asked to set up, configure, manage or fix ssf.
---

# Working with Simple Software Factory (ssf)

ssf turns GitHub issues and pull requests assigned to a bot account into
coding-agent sessions, one per item, each in its own git worktree and
terminal under herdr. People collaborate with the agents in the item's
comments; the `ssf` client configures and inspects the factory daemon
locally, in a VM, or over SSH. Linux and macOS are supported.

## Start here

1. Is `ssf` on `PATH`? If yes, run `ssf skill`. It prints a router keyed on
   what the person asked, from the version actually installed, and
   `ssf skill <topic>` prints each topic. Read those, not this file.
2. If not, the person wants ssf installed. Read
   [docs/install.md](https://github.com/mikekelly/simple-software-factory/blob/master/docs/install.md)
   (raw:
   `https://raw.githubusercontent.com/mikekelly/simple-software-factory/master/docs/install.md`).
   It opens with a resource check that decides, with the person, between a
   local VM, host mode and a rented host, then runs to the first issue. Once
   `ssf` is installed, continue from `ssf skill setup`, which is the same
   document at the installed version.
3. Inside a session that ssf itself started on an issue, read `ssf guide`
   instead; the first prompt already carries what that session needs.

| The person wants | Topic |
| --- | --- |
| ssf installed, first repository watched | `ssf skill setup` |
| a repository added to a running factory | `ssf skill repo` |
| to inspect, change, upgrade or stop a factory | `ssf skill operate`, `ssf skill config`, `ssf skill harnesses` |
| a factory repaired | `ssf skill troubleshoot` |
| an `SSF.md` written or reviewed | `ssf skill ssf-md`, `ssf skill audit` |
| an assistant that drives the factory for them | `ssf skill liaison` |
| their distro, macOS, a rented host, a harness's quirks, an old install | `ssf skill specifics` |

Before any change: `ssf server list`, then `ssf --server NAME doctor` and
`ssf --server NAME status` for the target you are about to touch. Ask the
person before creating accounts, spending money, using `sudo`, choosing a
model and effort, allowing anyone to drive the factory, or passing `--force`.
The bot's credentials are the bot's, never the person's.

## Installing this skill

```sh
npx skills add mikekelly/simple-software-factory -g
```

Drop `-g` to install it for one project, or add `--skill working-with-ssf -y`
to skip prompts. The [skills CLI](https://github.com/vercel-labs/skills)
installs the skill for each detected harness.
