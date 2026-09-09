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
with what a healthy `ssf doctor` looks like after each step and which
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
   harness, model and effort ids, and the daemon picks changes up on its
   next poll without a restart. Never write `github.token` into the
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
   the chart to read
   instead of answering from memory, and the three tiers. Propose a
   model and effort per repository and say why; the session's is `ssf
   repo add`/`ssf repo set --model --effort`, the tiers below it are the
   harness's own configuration and the repository's `SSF.md`. Say what
   it would cost and let the person decide. If you cannot verify the
   numbers, say so and ask for them, or leave `model` and `effort` unset
   with a note of what you would have looked up — an unverified
   recommendation is worse than the default.
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
   Debian, Ubuntu and Fedora the package does not bring herdr; install it
   as the document's step 1 says before expecting `ssf doctor`'s herdr
   line to pass. When reading `ssf vm status --json`, treat `running =
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
   item is still the bot's, and `--force` does not lift that (it covers
   the worktree checks only): the item has to stop being the bot's
   first. An item the bot was only ever mentioned on stays the bot's
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
    would not say whether the VM is running. Anything this configuration
    does not name -- what a changed `[vm] name` leaves: a lima instance
    or disk, or the old VM's directory under `[vm] dir` -- is listed
    under `keep:` with the command that removes it, and is never removed
    by ssf or reached by `--force`; leave that decision to the person.
    `ssf vm status` names them too, and `ssf doctor` does on a host not
    running the factory in a VM. All three name paths they could not
    inspect, including individual entries inside a readable directory.
    Only `NotFound` establishes absence; permission and I/O failures do
    not. An unread path prevents reassurance that its container is safe
    to remove. Fix access and inspect again before advising removal.
    An unread-path line is not a promise that the configured VM's own
    directory survives `vm destroy`, and reporting it alone adds no
    refusal. An unknown configured data-disk presence does engage the
    existing refusal for workspaces that cannot be checked; see setup §12
    and issue #176. Do not add `--force` on the person's
    behalf: show them the report and let them settle the work or decide;
    `--data` (config, the bot's key, state) is also theirs to ask for.
    The package removal that follows (`sudo pacman -R ssf`, `sudo apt
    remove ssf`, `sudo dnf remove ssf`, or on macOS `brew uninstall ssf`
    and then `brew untap mikekelly/ssf`; the command prints the one for
    the machine) is **you**.
11. **Calibrate the project's gauntlet by blast radius** when writing
    `SSF.md`: use the [boilerplate](../../SSF.example.md) and
    [gauntlet guidance](../../docs/sessions.md#second-opinions-the-gauntlet)
    for review classes and stopping rules. Keep required verification in
    every class, including changes that need only self-review. Keep the
    line the boilerplate carries beside those rules: check a claim by
    reading the source or trying it where trying it changes nothing,
    rather than reasoning your way to an answer; say where you have not,
    and apply the same to what you write about a change. Also keep the
    accompanying commit habit: read `git diff --cached` before writing the
    commit message, not after.

12. **Conflict notices concern committed branches.** Follow
    [Branch conflicts](../../docs/sessions.md#branch-conflicts) when an
    operator asks about them: `daemon.conflict_check_interval_secs`
    defaults to 300 seconds, a repository can override it, and `0`
    disables checks. `event_comments` does not control these terminal
    notices. Do not rebase merely because a branch is behind: a notice
    calls for rebasing and rerunning an already-started final round;
    before the round starts, no immediate action is needed.

13. **Name the base when reporting verification counts.** Follow the
    [development guidance](../../docs/development.md) for the JSON-based
    Clippy warning count and comparisons against a named base commit.
    Test totals need the base too; equal warning counts alone do not prove
    that a change adds no warnings.
