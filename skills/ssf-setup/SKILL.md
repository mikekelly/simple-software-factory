---
name: ssf-setup
description: Setup runbook for Simple Software Factory (ssf), the daemon for Linux (Omarchy, Arch, Debian/Ubuntu, Fedora) and macOS that turns GitHub issues assigned to a bot account into coding-agent sessions in herdr or Orca. Use when a person says "install ssf" or "set up ssf", when creating or signing in the bot GitHub account (a fresh account or an organisation's machine user, its access, token scopes and keys, and who commits), when running the factory in the VM (Firecracker or lima; ssf vm build, vm.enabled, ssf vm login) or on the host, when writing ~/.config/ssf/config.toml or a repository's SSF.md (its gauntlet, scope, plan and delegation rules), when choosing a repository's harness, model and effort level, project board conventions, upgrading or uninstalling ssf (ssf uninstall, then the package), or operating a factory (ssf status, doctor, tell, sub, handover, release, purge), including a session blocked on an expired harness login and the `ssf` blocks the daemon posts on an issue (daemon.event_comments).
license: MIT
metadata:
  source: https://github.com/mikekelly/simple-software-factory
---

# Setting up Simple Software Factory (ssf)

The setup document is `docs/setup.md`: `/usr/share/doc/ssf/docs/setup.md`
once the package is installed on Omarchy,
`$(brew --prefix)/share/doc/ssf/docs/setup.md` on macOS, or
`docs/setup.md` in a checkout of the repository. Read it and follow it,
top to bottom for a first install (its numbered steps, then its
checklist), or the one step that matches what the person asked for on an
installed factory. It has the prerequisites, the package, the bot
account, the sign-in, who may drive the factory, the VM (the default:
Firecracker on Linux, lima on macOS) and the host alternatives, the
harness login, the first repository and the harness and model it runs
on, `SSF.md`, the first issue, upgrading, stopping and uninstalling,
with `ssf doctor` checkpoints after each step and which
commands differ on a Mac. There is
no second copy of the steps here. The README next to it
(`/usr/share/doc/ssf/README.md`, or `$(brew --prefix)/share/doc/ssf/README.md`)
has the everyday commands, and the rest of `docs/` is the reference the
document links to.

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
   A state directory has one engine owner: `ssf run --once` refuses while
   the daemon is running. With a VM it is the guest daemon and state that
   matter, so let its next poll run.
3. **Prefer the CLI** (`ssf repo add`, `ssf repo set`, `ssf config set`,
   `ssf auth login`) over editing `config.toml` by hand: it validates
   harness IDs, model support and effort levels. Unknown model IDs pass
   through to the harness. Repository settings are picked up on the next poll;
   VM or service changes may require a restart. Never write `github.token` into the
   file; `ssf config set` refuses it on purpose. `ssf auth login` and
   `ssf auth logout` change credentials and config only; they do not edit
   the daemon's live `state.json` for bot identity.
4. **Raise the harness and the model; do not silently take the
   defaults.** Follow [Choosing the harness and the
   model](../../docs/setup.md#choosing-the-harness-and-the-model): it
   has the commands that say what the machine can run, what to ask the
   person (the machine says which harnesses are installed and signed
   in; only they can say which subscriptions or keys are behind them,
   what metered spend is acceptable and what must not be exhausted),
   and current provider documentation for availability, pricing and limits.
   Capability/cost comparisons can supplement this; API prices do not measure
   subscription allowance. Propose a model and effort per repository and
   explain the tradeoff. If numbers cannot be verified, say so and let the
   person choose, or explicitly leave model and effort unset. ssf sets the
   main session's model; optional subagents follow harness configuration and
   project instructions. Do not impose a delegation hierarchy on simple work.
5. **Never sign in as the person** or use their token, key or account for
   the bot. The bot is an account of its own; `ssf auth login --user
   <bot> -y` is the form an agent may run, once the bot is in gh's
   keyring.
6. **Never pass `--accept-anyone-risk`** on the person's behalf, and do
   not set `allowed_users` to `"*"` for them; say what it means and let
   them decide.
7. **Take the default path** (the factory inside the VM, herdr inside
   it) unless the person asks for an alternative or the machine cannot
   run the VM at all (on macOS the VM is lima and needs macOS 13.5 or
   later). A Linux machine without a usable `/dev/kvm`, or one that is
   not x86_64, cannot run the Firecracker backend, but it can still run
   the VM: set `[vm] backend = "lima"`, which uses qemu there (slower,
   and it needs `qemu-system-<arch>` installed). The lima backend needs
   lima 2.0.1 or newer on either OS; `ssf vm build` reads
   `limactl --version` and refuses an older one by name rather than
   letting it fail at the first boot, so a distribution shipping an old
   lima means lima's release tarball or `[vm] limactl` pointing at a
   newer one. The document says where the alternatives branch off. On
   Debian, Ubuntu and Fedora the package does not bring host herdr; install it
   as the document's step 2 says for host sessions. The VM supplies its own.
   When reading `ssf vm status --json`, treat `running =
   null` as an unanswered lima probe, not a stopped VM; `probe_error`
   names why the host could not ask.
8. **Let `ssf vm build` size the VM** from the machine (vCPUs, memory,
   data disk; it prints what it chose and writes it to `[vm]`) and tell
   the person what it picked; pass `--vcpus`, `--mem-mib` or
   `--data-gib` only when they ask for a size. When `ssf doctor` says the
   data disk is full, `ssf vm grow` (VM stopped) enlarges it without
   losing anything; see [Size](../../docs/vm.md#size).
9. **Never delete a worktree directory or `ssf purge --force` on the
   person's behalf.** A workspace closed by hand leaves its git checkout
   under `<checkout>.worktrees/`; `ssf doctor` prints a `WARN` line per
   repository naming each such checkout holding commits on no other
   branch and not on origin (or uncommitted changes) with no agent on
   it. For an active item the fix is `ssf tell <item> "..."`, which
   brings the session back in that checkout; a retired item refuses a
   tell, and its branch is pushed by hand. `ssf purge` says `(workspace
   gone, checkout still on disk)` for one whose item is closed and
   removes it only when clean and pushed. Show the person the line and
   let them decide about anything else. `ssf release` refuses while an
   item is still the bot's, and `--force` does not lift that: the item
   has to stop being the bot's first. For a retired session pinned by
   open follow-ups, a person can use `ssf release --as owner/repo#N
   --force`; this bypasses both the follow-up guard and worktree checks.
   Follow-ups retain ownership and provenance, and later activity can
   recreate the workspace. A pending handover still blocks release. An
   item the bot was only ever mentioned on stays the bot's
   until it closes, since nobody can withdraw a mention, so its
   workspace is not releasable while the item is open. `ssf status
   --json` marks such an item with `retirement_held_at`.
10. **Uninstall with `ssf uninstall`**, never by hand: it reports and
    asks once, and it refuses while a workspace holds unpushed work or
    the VM's clones cannot be checked. Under the lima backend it asks
    lima what it holds rather than reading `[vm] dir`, so the instance
    and its data disk are found and destroyed even where that directory
    has gone or `[vm] dir` has changed. The refusal names its own
    remedy: `ssf vm start` only where there is an instance to start, and
    never for a data disk that outlived its instance or for a lima that
    would not say whether the VM is running. A directory ssf could not
    read at all is a question that was never answered, not an empty
    one, and refuses too. So does a `data.ext4` a switch of
    `[vm] backend` left in the VM's own directory: it is checked on the
    host, because a healthy lima guest cannot see it, and the remedy is
    to put `[vm] backend` back rather than any lima command. Do not add `--force` on the
    person's behalf: show them the report and let them settle the work
    or decide; `--data` (config, the bot's key, state) is also theirs to
    ask for. The package removal that follows (`sudo pacman -R ssf`,
    `sudo apt remove ssf`, `sudo dnf remove ssf`, or on macOS `brew
    uninstall ssf` and then `brew untap mikekelly/ssf`; the command
    prints the one for the machine) is **you**.
11. **Keep project review bounded** when writing
    `SSF.md`. Keep shared guidance there (or in `repo.prompt_file`). Optional
    `SSF.<harness>.md` files at the checkout root add instructions only for the
    selected harness, including handovers; for example, put Codex-specific
    subagent guidance in `SSF.codex.md`. They do not replace the shared notes.
    Use the [boilerplate](../../SSF.example.md) and
    [gauntlet guidance](../../docs/sessions.md#second-opinions-the-gauntlet)
    for one behavior review and at most one focused follow-up for substantive
    fixes. Unresolved defects require simplification or a maintainer decision;
    wording changes do not restart review. Match validation to the change,
    with package builds for packaging/installation changes. Keep one outcome
    per issue and implementation tasks within it. Use `Refs #N` for ongoing
    management/tracking work; reserve `Closes #N` for complete delivery.
    State completion and merge authority explicitly: the owning issue agent
    normally takes responsibility for merging after required checks/review,
    unless reserved for a human. Complete work within delegated authority;
    otherwise tag an appropriate human with the concrete next action. Use the
    bounded [project guidance audit](../../docs/audit.md) when asked to assess
    these policies.

12. **Conflict notices concern committed branches.** Follow
    [Branch conflicts](../../docs/sessions.md#branch-conflicts) when an
    operator asks about them: `daemon.conflict_check_interval_secs`
    defaults to 300 seconds, a repository can override it, and `0`
    disables checks. `event_comments` does not control these terminal
    notices. Resolve conflicts before delivery, preferably against stable
    dependency heads. Test the integration and review behavior changes;
    a notice alone does not require restarting review.

13. **Name the base when reporting verification counts.** Follow the
    [development guidance](../../docs/development.md) for the JSON-based
    Clippy warning count and comparisons against a named base commit.
    Test totals need the base too; equal warning counts alone do not prove
    that a change adds no warnings.

## GitHub body handling

The session’s gh shim stamps explicit bodies, reading only the last repeated
body-file value. Large bodies use an inherited anonymous file (memory on
Linux, an immediately unlinked temporary file on other Unix systems). An
invalid UTF-8 body or stdin read error fails before posting; fix the input
and retry. Explicit blank comment/request-changes review bodies are rejected before
posting. Generated `--fill` bodies still need attribution supplied
by the agent; do not replace requested commit text with a byline-only body.
See `docs/identity-and-bylines.md`.
