---
name: working-with-ssf
description: "Work with Simple Software Factory (ssf) on a person's behalf: install a factory, setup a factory, configure ssf to watch a repository, operate and troubleshoot a running factory, write or audit a project's SSF.md, act as a liaison between the user and the factory. Use when asked to set up, configure, manage or fix ssf."
---

# Working with Simple Software Factory (ssf)

ssf turns GitHub issues and pull requests assigned to a bot account into
coding-agent sessions, one per item, each in its own git worktree and
terminal under herdr (scratch sessions, which work on no item, too). People collaborate with the agents in the item's
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
   local VM, a guest on a server (with laptop and resident-agent access),
   host mode and a rented host, then runs to the first issue. Once
   `ssf` is installed, continue from `ssf skill setup`, which is the same
   document at the installed version.
3. If you're an ssf agent session that ssf itself started on an issue (`SSF_SESSION` will be set)
   read `ssf guide` instead.

| The person wants | Topic |
| --- | --- |
| ssf installed, first repository watched | `ssf skill setup` |
| a repository added to a running factory | `ssf skill repo` |
| to inspect, change, upgrade or stop a factory | `ssf skill operate`, `ssf skill config`, `ssf skill harnesses` |
| a factory repaired | `ssf skill troubleshoot` |
| an `SSF.md` written or reviewed (a conversation about how their factory should work, not a template fill) | `ssf skill ssf-md`, `ssf skill audit` |
| an assistant that drives the factory for them | `ssf skill liaison` |
| their distro, macOS, a rented host, a harness's quirks, an old install | `ssf skill specifics` |

Before any change: `ssf server list`, then `ssf --server NAME doctor` and
`ssf --server NAME status` for the target you are about to touch. Ask the
person, one decision at a time at the step that needs it, before creating
accounts, spending money, choosing a model and effort, allowing anyone to
drive the factory, or passing `--force`. Hand `sudo` commands to the person
to run; run them yourself only where you already have non-interactive root.
The bot's credentials are the bot's, never the person's.
Upgrading ssf on the host does not upgrade a VM's guest: `ssf --server NAME
vm upgrade [VERSION]` (or `--deb PATH`) does, on the running guest, under
every backend. An Arch lima guest from an older ssf has no package and takes
the host's binary on `ssf vm restart`. `ssf vm build --force` and `ssf vm
reset` discard anything installed in the guest root; don't use them to
upgrade.
A host-mode factory runs its agents in its own herdr session, `ssf`, never
the person's own herdr session, with a herdr config ssf writes (`ssf doctor`
and `ssf status` print the session and its commands). Start it with
`HERDR_CONFIG_PATH=~/.config/ssf/herdr.toml herdr --session ssf server`;
attach with `herdr session attach ssf` on the host, or
`herdr --remote HOST --session ssf` from elsewhere (saved once with
`herdr machine add HOST --label factory --remote-session ssf`). A VM
factory's agents are in the guest's default herdr session (`ssf vm attach`).

## Reporting problems upstream

When ssf itself gets in the way (a bug, an error, a misleading doc or a step
that did not work as written), offer to report it on
[the upstream issue board](https://github.com/mikekelly/simple-software-factory/issues).
Ask the person first, show them the draft, and file it only with their
permission. Search the existing issues before opening a new one.

A good report gives:

- what went wrong, in one or two sentences;
- the steps to reproduce it, as the smallest sequence of `ssf` commands or
  actions that shows it;
- what you expected and what happened instead, with the exact error text;
- the ssf version (`ssf --version`), the OS and the harness involved.

Keep the person's environment out of it: no hostnames, IP addresses, user
or account names, file paths, repository or organization names, tokens,
keys, or configuration and log excerpts you have not reduced to the lines
that matter and scrubbed. Replace specifics with placeholders such as
`HOST` or `REPO`. When in doubt, leave it out and say it was omitted.

## Installing this skill

```sh
npx skills add mikekelly/simple-software-factory -g
```

Drop `-g` to install it for one project, or add `--skill working-with-ssf -y`
to skip prompts. The [skills CLI](https://github.com/vercel-labs/skills)
installs the skill for each detected harness.
