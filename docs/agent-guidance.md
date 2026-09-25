# Agent operating guidance

Rules for an agent installing, operating, upgrading or removing a factory
on a person's behalf, on any platform. A session ssf started to work an
issue has its own reference, `ssf guide`; this document is not for it.

The procedures live elsewhere and are not repeated here: [Install](install.md)
(`ssf skill setup`) from a fresh machine to the first issue,
[Repositories](repositories.md) (`ssf skill repo`) for a factory that already
runs, [Operate](operate.md) (`ssf skill operate`) for inspection, the service
and upgrades, [Troubleshooting](troubleshooting.md) (`ssf skill troubleshoot`)
when something is wrong, and [Platform specifics](platform-specifics.md)
(`ssf skill specifics`) for anything that depends on the distro, macOS, a
rented host, a harness's quirks or an older installation. Each has `ssf
doctor` checkpoints; run them.

## Rules

1. **Stop where only the person can act.** The install document lists
   them: a sudo password, renting a host, creating a GitHub account, signing in in a
   browser, approving a token or scopes, signing a harness in. Give the
   exact command or URL, say what they will see, and wait; carry on when
   they say it is done. Everything else is yours to run. Never ask for
   callback URLs, codes or tokens in chat, or replay them from another
   shell. In Claude Code, a command the person must type themselves can be
   run as `! <command>` from the prompt.
2. **Inspect before changing.** On an installed factory read `ssf server
   list`, then `ssf --server NAME doctor` and `ssf --server NAME status`
   for every named target (unqualified `ssf doctor` and `ssf status` when
   there is no catalog). One target's clean result says nothing about
   another. The install document says which lines are expected to fail
   before the first issue. A state directory has one engine owner: `ssf-server --once`
   refuses while the daemon runs; with a VM, the guest daemon and state are
   the ones that matter, so let its next poll run.
3. **Prefer the CLI** (`ssf repo add`, `ssf repo set`, `ssf config set`,
   `ssf auth login`) over editing `config.toml`: it validates harness ids,
   model support and effort levels; unknown model ids pass through to the
   harness. Repository settings are picked up on the next poll. Never write
   `github.token` into the file; `ssf config set` refuses it on purpose.
   `ssf auth login` and `ssf auth logout` change credentials and config
   only, not the daemon's live `state.json`.
4. **Raise the harness and the model; never take the defaults silently.**
   Follow [Choose harness, model and effort with the
   person](repositories.md#2-choose-harness-model-and-effort-with-the-person):
   the machine says
   which harnesses are installed and signed in; only the person can say
   which subscriptions or keys are behind them, what metered spend is
   acceptable and what must not be exhausted. Propose a model and effort
   per repository with the tradeoff; if numbers cannot be verified, say so.
   Before adding a watched repository or changing its harness, model or
   effort, obtain the person's explicit choice for each supported setting.
   A choice made earlier in this conversation counts; examples,
   recommendations, silence, installer defaults and existing values do not.
   Without one, pause that step and ask. ssf sets the main session's model;
   subagents follow the harness and the repository's guidance.
5. **Never sign in as the person** or use their token, key or account for
   the bot. The bot is an account of its own; `ssf auth login --user <bot>
   -y` is the form an agent may run once the bot is in gh's credential
   store on the machine running the factory.
6. **Never pass `--accept-anyone-risk`** on the person's behalf, and do not
   set `allowed_users` to `"*"` for them; say what it means and let them
   decide.
7. **Check the machine before proposing where the factory runs.** The
   install document opens with the probes: with the resources for a VM,
   propose the VM (herdr inside it); without them, propose host mode or a
   rented host and let the person choose. Let `ssf vm
   build` size the VM and tell the person what it picked; pass `--vcpus`,
   `--mem-mib` or `--data-gib` only when they ask.
8. **Never delete a worktree directory or `ssf purge --force`** on the
   person's behalf. `ssf doctor` prints a `WARN` line per checkout holding
   commits on no other branch, or uncommitted changes, with no agent on it;
   show the person the line and let them decide. For an active item the fix
   is a comment on it, which starts its session again in that checkout;
   a retired item's branch is pushed by hand. [Workspaces after
   close](sessions.md#workspaces-after-close-release-and-purge) has the
   release and purge rules, including `ssf release --as owner/repo#N
   --force` for a retired session pinned by open follow-ups.
9. **Uninstall with `ssf uninstall`**, never by hand: it reports and asks
   once, and refuses while a workspace holds unpushed work or the VM's
   clones cannot be checked. Its refusal names its own remedy; do not add
   `--force` or `--data` on the person's behalf. The package removal it
   prints at the end (the package manager's remove command for this
   machine) is the person's to run. [Uninstall](uninstall.md) has the cases.
10. **Writing `SSF.md`**: follow [Writing SSF.md](ssf-md.md) (`ssf skill
    ssf-md`): agree the factory's goals and the agents' latitude with the
    owner first, then adapt [`SSF.example.md`](../SSF.example.md). Use the
    bounded [project guidance audit](audit.md) when asked to assess a
    repository's existing guidance.
11. **Conflict notices concern committed branches.** Follow [Branch
    conflicts](sessions.md#branch-conflicts): `daemon.conflict_check_interval_secs`
    (default 300, `0` disables, a repository can override). Resolve before
    delivery, preferably against stable dependency heads; a notice alone
    does not restart review.
12. **Name the base when reporting verification counts.** A warning count
    or a test total means nothing without the commit it is compared to.
13. **Let GitHub own repository renames and transfers.** Do not remove and
    re-add a watched repository because its `owner/name` changed: ssf
    records GitHub's immutable repository id and repairs the name, state and
    checkout remotes itself. `ssf doctor` verifies the reconciliation.
14. **On any conflict between two states, stop**, preserve both versions
    and have the person choose; never guess or discard state.
