---
name: ssf-setup
description: Setup runbook for Simple Software Factory (ssf), the Omarchy daemon that turns GitHub issues assigned to a bot account into coding-agent sessions in herdr or Orca. Use when a person says "install ssf" or "set up ssf" (the package, the service), when creating or signing in the bot GitHub account (a fresh account or an organisation's machine user, its access, token scopes and keys, committing as the bot or as the person), when running the factory inside the Firecracker microVM (ssf vm build, vm.enabled, ssf vm login) or on the host with herdr or Orca, when writing or changing ~/.config/ssf/config.toml ([github], [git], [[repo]], [daemon], [vm]), writing a repository's SSF.md, setting up the review label or project board conventions, upgrading or uninstalling ssf, or operating a running factory (ssf status, doctor, tell, sub, release, purge), including a session blocked on an expired harness login.
license: MIT
metadata:
  source: https://github.com/mikekelly/simple-software-factory
---

# Setting up Simple Software Factory (ssf)

The setup document is `docs/setup.md`: `/usr/share/doc/ssf/docs/setup.md`
once the package is installed, or `docs/setup.md` in a checkout of the
repository. Read it and follow it, top to bottom for a first install
(its numbered steps, then its checklist), or the one step that matches
what the person asked for on an installed factory. It has the
prerequisites, the package, the bot account, the sign-in, who may drive
the factory, the microVM (the default) and the host alternatives, the
harness login, the first repository, `SSF.md`, the first issue,
upgrading, stopping and uninstalling, with what a healthy `ssf doctor`
looks like after each step. There is no second copy of the steps here.
The README next to it (`/usr/share/doc/ssf/README.md`) has the everyday
commands, and the rest of `docs/` is the reference the document links to.

## Rules for an agent following it

1. **Stop where only the person can act.** The document marks them
   **you**: type a sudo password (`pacman`), create a GitHub account,
   sign in in a browser, approve a token or scopes, sign a harness in.
   Give the exact command or URL, say what they will see, and wait;
   carry on when they say it is done. Everything else is for the agent
   to run. In Claude Code, a command the person must type themselves can
   be run as `! <command>` from the prompt.
2. **If ssf is already installed, run `ssf doctor` and `ssf status` first**
   and read them before changing anything; most setup problems show up
   there, and the document says which lines are expected to fail at each
   step.
3. **Prefer the CLI** (`ssf repo add`, `ssf repo set`, `ssf config set`,
   `ssf auth login`) over editing `config.toml` by hand: it validates
   harness, model and effort ids, and the daemon picks changes up on its
   next poll without a restart. Never write `github.token` into the
   file; `ssf config set` refuses it on purpose.
4. **Never sign in as the person** or use their token, key or account for
   the bot. The bot is an account of its own; `ssf auth login --user
   <bot> -y` is the form an agent may run, once the bot is in gh's
   keyring.
5. **Never pass `--accept-anyone-risk`** on the person's behalf, and do
   not set `allowed_users` to `"*"` for them; say what it means and let
   them decide.
6. **Take the default path** (the factory inside the microVM, herdr
   inside it) unless the person asks for an alternative or the machine
   cannot run the VM (no `/dev/kvm`, not x86_64); the document says
   where the alternatives branch off.
