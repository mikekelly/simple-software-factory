# Uninstall

For the agent asked to remove a factory from a machine. Read this before
running anything: uninstalling destroys the VM and its workspaces, and the
person must agree to that.

## The sequence

1. `ssf uninstall`. It reports what it will stop, remove and revoke, lists the
   items and the state of their workspaces, and asks once.
2. Remove the package. The command prints the one for this machine as the last
   line of its report: `sudo pacman -R ssf`, `sudo apt remove ssf`,
   `sudo dnf remove ssf` or `brew uninstall ssf`. This is the person's to run;
   nothing in ssf runs `sudo`.

```sh
ssf uninstall
```

A good result is a report, one question, then each step in order. If it refuses,
read [What it refuses](#what-it-refuses) rather than reaching for `--force`.

Flags: `--yes` skips the question, for scripted use. `--force` goes ahead over a
refusal. `--data` also removes the config and state directories. `--force` and
`--data` both destroy data the person may want, so ask before using either.

### What the command does

In the order the pieces depend on each other:

| Step | Effect |
| --- | --- |
| purge | removes the clean, pushed workspaces of closed items; needs the running daemon, and is skipped when it is down |
| service disable | stops and disables the background service, then shuts the VM down cleanly (stopping the service alone leaves a VM running) |
| auth logout | revokes the bot's keys on GitHub and forgets it |
| vm destroy | destroys the VM and, under the lima backend, its instance and data disk |

Each step tolerates the thing being gone already, so a second run, or a run on a
half-uninstalled machine, is fine. With the factory in a VM the report and the
purge come from the guest, before it goes.

Service disable is the one step that can end the run early. Everything after it
destroys something, and none of it may happen while the daemon might still be
working, so a service that will not stop leaves the machine as it was and tells
you to stop it by hand and run `ssf uninstall` again. Stopping it by hand is the
service command for this machine's init system; see
[operate.md](operate.md#the-background-service).

Under the lima backend, whether there is a VM at all is a question for the
backend rather than for `[vm] dir`, so an instance whose directory was removed
by hand, or whose `[vm] dir` has since changed, is still found and destroyed,
and a data disk that outlived its instance is too. Where lima itself will not
answer (a moved `limactl`, one off the PATH the service runs under, a stale
`[vm] limactl`), what settles it is whether `[vm] dir` or lima's home still
holds the instance or the disk. A machine with neither gets a plain `no VM`; one
that still has a disk of workspaces is never told it has none.

Desktop menu entries a desktop environment installed are left in place;
`ssf ui uninstall` removes them.

## What it refuses

Without `--force`, the command stops before destroying anything when:

| Refusal | Why | What to do |
| --- | --- | --- |
| a workspace holds uncommitted or unpushed work, including an open item's | the work is only on that machine | push or discard it |
| a workspace cannot be checked (no origin, a git error) | ssf cannot rule out unpushed work | fix the repository or look at it yourself |
| the state file or config cannot be read, a clone's worktrees cannot be listed, or a `<name>.worktrees` directory has lost its clone (`unchecked:` lines) | the inventory has a hole in it; workspaces are found by looking at every clone in the projects directory, not only at the state file's records | repair or remove what the line names |
| the VM is stopped, so its workspaces cannot be checked | in VM mode the clones live on the data disk | `ssf vm start`, then run uninstall again |
| a data disk's clones cannot be checked: the disk outlived its instance, `[vm] enabled = false` so ssf never asks the guest, or lima will not say whether it or the disk is running | an unanswered question, not a clean machine | follow the remedy the message names; it is not always `ssf vm start` |
| a data disk a backend switch left behind | see [Recovery cases](#recovery-cases) and [troubleshooting](troubleshooting.md#vm-backend-switch) | put `[vm] backend` back and run it again |

Only a data disk stops the command. What `[vm] dir` holds without one is ssf's
own template, ssh key and share, and goes without a word. Only "not found"
counts as "there is no data disk": a directory ssf could not read at all (a lima
home an earlier `sudo` left root-owned, which is itself one of the reasons
`limactl` fails, or a volume that has gone away) is a question that was never
answered, and the command refuses over it rather than destroying workspaces
nobody checked. A destroy that cannot look for the data disk fails that step
instead of reporting no such disk.

`--force` goes ahead in every one of these cases. On the host the work stays
where it is; in the VM the clones are on its data disk and are destroyed with
it, checked or not. That is the person's decision, not the agent's.

## What is retained

The report lists these at the end:

- `~/ssf/projects`, the clones and worktrees. May hold unpushed work.
- `[vm] dir`, the VM image and downloads. ssf does not look inside it, so
  inspect it before removing it.
- Without `--data`: `~/.config/ssf` (host configuration and any host-mode bot
  keys) and `~/.local/state/ssf` (state, and the marker that keeps a disabled
  service off, so a reinstall stays stopped until `ssf ui service enable`). With
  `--data`, a reinstall starts the service.
- The bot's GitHub account itself, and its `gh` sign-in.
- Under lima, `~/.lima`, holding lima's cache of downloaded images. The report
  does not name it; remove it by hand once nothing else uses lima.

In VM mode the guest data disk owns factory configuration, bot credentials,
signing keys and harness logins as well as state and worktrees. Destroying that
disk removes them, and a retained host `[vm]` config cannot restore the factory.
Export anything needed from the guest before confirming.

After a successful run `ssf status` says not signed in and stopped. The watched
repositories and the records of past items still show until `--data`, or a
reinstall from scratch, clears them. With the VM gone, `vm.enabled` is cleared
so `status` does not go looking for it.

## Recovery cases

- **Removing one VM server from a multi-server installation.**
  `ssf --server NAME uninstall` removes that server only, with the same report,
  prompt and refusal over unpushed work: it purges, signs the bot out inside
  the guest (revoking the keys it enrolled) while the guest answers, stops and
  disables that server's service, destroys its VM, and removes its setup marker,
  its macOS service log and its catalog entry. The other servers, the shared
  config and state directories and the package stay. `--data` is refused while
  other servers remain; on the last one it removes the shared directories as a
  plain uninstall does. A namespaced local target is still refused. To stop
  operating a target without deleting it, run
  `ssf --server NAME ui service disable`, then `ssf server remove NAME`.
- **Backend switch.** `[vm] dir` is shared by the two backends. Switching
  `[vm] backend` from `firecracker` to `lima` while keeping `[vm] name` leaves
  the Firecracker VM's clones and worktrees in `<[vm] dir>/<name>`, where lima
  knows nothing of them and the destroy would remove the directory with them in
  it. A healthy lima guest reports a clean machine, because that disk is on
  nothing lima mounted, so this is checked on the host: `ssf uninstall` refuses,
  names the file and says to put `[vm] backend` back to `firecracker` and run it
  again to reach the work. See
  [troubleshooting.md](troubleshooting.md#vm-backend-switch).
- **Invalid `[vm] name`.** The name is joined onto `[vm] dir`, and `ssf vm
  destroy` and `ssf uninstall` remove the result whole, so an empty name (which
  would be `[vm] dir` itself), an absolute one, or one containing `..` is
  refused when the config is read. Nested names like `a/b` are fine.
- **A machine that ran an older ssf.** Unit and path names from earlier layouts
  are in
  [platform specifics](platform-specifics.md#upgrading-from-an-older-ssf).

Per-platform package removal details are in
[platform specifics](platform-specifics.md).
