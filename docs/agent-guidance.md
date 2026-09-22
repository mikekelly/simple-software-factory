# Agent operating guidance

This is for an agent installing, operating, upgrading or removing a factory
on a person's behalf. A session ssf started to work an issue has its own
reference, `ssf guide`; this document is not for it. `ssf skill` lists the
other bundled topics.

The setup procedure is `docs/setup.md` (`ssf skill setup`;
`/usr/share/doc/ssf/docs/setup.md` once the Linux package is installed).
Read it and follow it for a first install, or the one step that matches
what the person asked for on an installed factory. It has the
prerequisites, the package, the bot account, the sign-in, who may drive the
factory, the VM and the host alternatives, the harness login, the first
repository and its harness and model, `SSF.md`, the first issue, upgrading,
stopping and uninstalling, with `ssf doctor` checkpoints after each step.
There is no second copy of the steps here. Release packages cover
Arch-family Linux (including Omarchy) and Debian-family Linux (including
Ubuntu) on x86_64; macOS is planned and not yet supported.

## Rules

1. **Stop where only the person can act.** The setup document marks them
   **you**: a sudo password, creating a GitHub account, signing in in a
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
   another. The document says which lines are expected to fail at each
   step. A state directory has one engine owner: `ssf-server --once`
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
   Follow [Choosing the harness and the
   model](setup.md#choosing-the-harness-and-the-model): the machine says
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
7. **Take the default path** (the factory inside the VM, herdr inside it)
   unless the person asks for an alternative or the machine cannot run the
   VM; the document says where the alternatives branch off. Let `ssf vm
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
   prints at the end (`sudo pacman -R ssf` or `sudo apt remove ssf`) is
   **you**. [Uninstall](uninstall.md) has the cases.
10. **Writing `SSF.md`**: follow [Writing SSF.md](ssf-md.md) (`ssf skill
    ssf-md`) and start from [`SSF.example.md`](../SSF.example.md). Use the
    bounded [project guidance audit](audit.md) when asked to assess a
    repository's existing guidance.
11. **Conflict notices concern committed branches.** Follow [Branch
    conflicts](sessions.md#branch-conflicts): `daemon.conflict_check_interval_secs`
    (default 300, `0` disables, a repository can override). Resolve before
    delivery, preferably against stable dependency heads; a notice alone
    does not restart review.
12. **Name the base when reporting verification counts.** Follow the
    [development guidance](development.md): a Clippy warning count or a
    test total means nothing without the base commit it is compared to.
13. **Let GitHub own repository renames and transfers.** Do not remove and
    re-add a watched repository because its `owner/name` changed: ssf
    records GitHub's immutable repository id and repairs the name, state and
    checkout remotes itself. `ssf doctor` verifies the reconciliation.
14. **On any conflict between two states, stop**, preserve both versions
    and have the person choose; never guess or discard state.

## Setup-specific notes

Each note applies to one kind of factory. Read the one that matches.

### Factory in the VM (Firecracker or Lima)

Build, enable and start the VM first, then authenticate the bot and
configure repositories inside it through the normal CLI: factory commands
route to the guest and fail if it is unavailable. Never fall back to host
edits or recommend routine `ssf vm sync`. Only `[vm]` settings and the
guest administration SSH key belong to the host; bot credentials, git keys
and factory config live on the guest data disk. Use guest-native `ssf auth
login --web`: the person approves the device code as the bot and the
credential stays in the guest.

A Linux machine without a usable `/dev/kvm`, or not x86_64, cannot run the
Firecracker backend but can still run the VM with `[vm] backend = "lima"`
(qemu, slower; needs `qemu-system-<arch>` and lima 2.0.1 or newer, which
`ssf vm build` checks by name). On Debian and Ubuntu the package does not
bring host herdr; install it as setup step 2 says for host sessions. In
`ssf vm status --json`, `running = null` is an unanswered lima probe, not a
stopped VM; `probe_error` says why. `ssf vm status` is host
infrastructure, `ssf status` and `ssf doctor` are guest health. When
`ssf doctor` says the data disk is full, `ssf vm grow` (VM stopped)
enlarges it without losing anything ([Size](vm.md#size)).

Before upgrading an old copied-config VM, read the migration and recovery
section of [the VM document](vm.md). Legacy Firecracker roots need the
matching package and guest scripts, then `ssf vm build --force`, `ssf vm
reset` and `ssf vm start`; legacy Lima roots need reset and start. Both keep
the data disk; never patch a legacy seed script in place. Tailscale is
absent from the base image: when asked to enrol the guest, run `ssf vm
tailscale` and relay its browser URL; a root reset discards the enrolment.
Do not enable Tailscale SSH, routes, an exit node or key-expiry changes
unless asked.

### Headless VPS or stripped container

Without KVM or a systemd user session, use [the headless
document](headless-host.md) (`ssf skill headless`) first: standalone
binaries, host mode, `herdr server` and a foreground `ssf-server`. Keep the
fresh server catalog empty so client and daemon share paths; skip package
setup and linger. Refresh the package manager's indexes and install an
OpenSSH client before key enrolment. Follow its older-gh device flow and
token handoff, and load harness credentials into both processes at
restart. Setup steps 3b to 3e separate owner invitation, bot acceptance,
project board access and `SSF.md` on the default branch: do not skip
acceptance or infer board access from repository Write. Standalone binaries
and client-only SSH installs are in [Install
binaries](install-binaries.md): bare binaries ship no service units or VM
scripts, and `ssf setup` needs the packaged installation. Package
installation leaves the service disabled; explicit setup enables it and
asks about linger.

### Several factories on one client

An optional client-owned `~/.config/ssf/servers.toml` names local, VM and
SSH factories (`ssf server list`, `ssf server show NAME`). With one entry
commands select it implicitly; with several, every factory command needs
`--server NAME` or `SSF_SERVER=NAME`, except `ssf dashboard`, which defaults
to all. There is no persistent default. A fresh `ssf setup` creates the sole
VM target `ssf-server`. `ssf server add local --local`, `ssf server add
NAME --vm` and `ssf server add cloud --ssh ssf@factory.example.com` are the
advanced layouts; VM creation picks a Lima-safe runtime name, a
non-overlapping directory and a free port pair, so plan CPU, memory and
disk per VM before building several. For an established VM, verify `ssf vm
status` and `ssf status`, run `ssf server migrate-vm`, then verify both
again as `ssf-server` before adding another target. Services are
`ssf@NAME.service` on Linux (`dev.ssf.server.NAME` on macOS, logging to
`~/Library/Logs/ssf/NAME.log`); before enabling the first target service,
stop the legacy singleton (`systemctl --user disable --now ssf.service`,
or `brew services stop ssf`), since ssf refuses two supervisors on one
factory. Disable a target's service before `ssf server remove NAME`;
catalog removal never destroys its VM or data. To operate a factory over
SSH, `ssf --server HOST COMMAND` or `SSF_SERVER=HOST` runs the same
`ssf-server` command endpoint; the SSH account must be able to operate it.

### OMP (Oh My Pi) sign-in

For VM enrolment, have the person run `ssf vm login omp` on the computer
with their browser, select `/login` and the provider, then open the short
loopback `/launch` URL once ssf has its SSH forwarding up; keep that
terminal open through authorisation and exit OMP to check the result. If a
loopback port is occupied, free it and retry. OMP stores credentials in
`~/.omp/agent/agent.db`; a missing `auth.json` alone is not a sign-out. On
a headless or herdr host, after OpenRouter auth run one interactive `omp`
as the factory's Unix user and finish or Esc through the first-run wizard
before spawning sessions; an already-logged-in provider under setup needs
completion or skipping, not another `/login` (completion persists
`setupVersion` in `~/.omp/agent/config.yml`). A key in the daemon
environment is not forwarded to herdr panes and ssf has no `omp.env`
loader: use OMP's saved credentials in the shared home, or verify the pane
environment and a request there. Doctor success alone does not verify it.

### Dashboard and web UI

`ssf dashboard` (in the Linux client) watches one or several factories in
a terminal: `--server HOST` per factory, or all catalog entries by default;
one `status --json --watch` stream per server. Only driver-reported agents
become cards; monitored items without an agent are listed separately.
Keyboard and mouse selection focus a matched agent when running inside
herdr, scoped to that herdr server. The web UI is off by default;
`[dashboard] enabled = true` enables a listener on port 8787 after
restarting ssf-server; the bind must be a loopback or Tailscale address,
anything else is refused, remote access beyond a tailnet needs an
authenticated TLS reverse proxy, and the logged capability URL is a
secret. In VM mode `dashboard.*` settings belong to the host. See [the
dashboard document](dashboard.md).

### Omarchy

The **Factory** menu entries come from `ssf ui install` and can be removed
with `ssf ui uninstall`; neither touches the running factory. The bar widget
this package used to ship is gone (#413): `ssf setup` and `ssf ui install`
disable and remove one left by an earlier version, and `ssf doctor` says what
is left until one of them runs.
Preserved configuration, state, clones and worktrees are reused unless the
person explicitly chose `--data` at uninstall.

### GitHub body handling (the `gh` shim)

The session's `gh` shim stamps explicit bodies, reading only the last
repeated body-file value; large bodies go through an anonymous file. An
invalid UTF-8 body or a stdin read error fails before posting, as does an
explicit blank comment or request-changes review body. Generated `--fill`
bodies still need the byline supplied by the agent; do not replace
requested commit text with a byline-only body. The same directory links a
`git` wrapper; both run the real program with the session's own
`GH_TOKEN`, `GIT_SSH_COMMAND` and git configuration, taken from an ancestor
process when the tool that started them dropped them, so a harness tool
that scrubs its environment still posts and pushes as the bot. See
[Identity and bylines](identity-and-bylines.md).
