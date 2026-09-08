---
name: ssf-setup
description: Setup runbook for Simple Software Factory (ssf), the Linux daemon (Omarchy, Arch, Debian/Ubuntu, Fedora) that turns GitHub issues assigned to a bot account into coding-agent sessions in herdr or Orca. Use when a person says "install ssf" or "set up ssf" (the package, the service), when creating or signing in the bot GitHub account (a fresh account or an organisation's machine user, its access, token scopes and keys, committing as the bot or as the person), when running the factory inside the Firecracker microVM (ssf vm build, vm.enabled, ssf vm login) or on the host with herdr or Orca, when writing or changing ~/.config/ssf/config.toml ([github], [git], [[repo]], [daemon], [vm]), writing a repository's SSF.md (including its gauntlet rule, the second pair of eyes ssf leaves to the agent), project board conventions, upgrading or uninstalling ssf (ssf uninstall, then the package), or operating a running factory (ssf status, doctor, tell, sub, handover, release, purge), including a session blocked on an expired harness login and the fenced `ssf` blocks the daemon posts on an issue (daemon.event_comments).
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
   **you**: type a sudo password (`pacman`, `apt`, `dnf`), create a GitHub account,
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
   where the alternatives branch off. On Debian, Ubuntu and Fedora the
   package does not bring herdr; install it as the document's step 1
   says before expecting `ssf doctor`'s herdr line to pass.
7. **Let `ssf vm build` size the VM** from the machine (vCPUs, memory,
   data disk; it prints what it chose and writes it to `[vm]`) and tell
   the person what it picked; pass `--vcpus`, `--mem-mib` or
   `--data-gib` only when they ask for a size. When `ssf doctor` says the
   data disk is full, `ssf vm grow` (VM stopped) enlarges it without
   losing anything; see [Size](../../docs/vm.md#size).
8. **Uninstall with `ssf uninstall`**, never by hand: it reports and
   asks once, and it refuses while a workspace holds unpushed work or
   the VM is stopped so its clones cannot be checked. Do not add
   `--force` on the person's behalf: show them the report and let them
   settle the work or decide; `--data` (config, the bot's key, state)
   is also theirs to ask for. The package removal that follows (`sudo
   pacman -R ssf`, `sudo apt remove ssf` or `sudo dnf remove ssf`; the
   command prints the one for the machine) is **you**.
