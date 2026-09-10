# Uninstall reference

For the normal removal sequence, see [Stopping and uninstalling](setup.md#12-stopping-and-uninstalling).
This page details work-preservation checks, recovery cases and retained data.

**Uninstalling** is one command and one step for you:

1. `ssf uninstall`: reports what it will stop, remove and revoke, lists
   the items and the state of their workspaces, and asks once. Then, in
   the order the pieces depend on each other: `purge` of the clean and
   pushed workspaces of closed items (needs the running daemon; skipped
   when it is down), `ui service disable` (with `vm.enabled` that shuts
   the guest down; on macOS this is `brew services stop ssf`), `ui
   uninstall` (the bar widget and menu, Omarchy only), `auth logout`
   (revokes the bot's keys on GitHub and forgets it), `vm destroy`
   (under the lima backend the lima instance `ssf-default` and its disk
   `ssf-default` too, on whichever OS you run it; whether there is a VM
   at all is a question for the backend rather than for `[vm] dir`, so
   an instance whose directory has been removed by hand, or whose `[vm]
   dir` has since been changed, is still found and destroyed, and a data
   disk that outlived its instance is too). Where lima itself will not
   answer -- `limactl` moved by an upgrade, off the PATH the service
   runs under, a stale `[vm] limactl` -- what settles it is whether
   `[vm] dir` or lima's home still holds the instance or the disk, so a
   machine with nothing of either on it gets a plain `no VM` and one
   that still has a disk of workspaces is never told it has none. Each
   step tolerates the thing being gone already, so a second run, or a
   run on a half-uninstalled machine, is fine. With the factory in the
   VM the report and the purge come from the guest, before it goes. The
   one step that can end the run early is `ui service disable`:
   everything after it destroys something, and none of it may happen
   while the daemon might still be working, so a service that would not
   stop leaves the machine as it was and tells you to stop it by hand
   (`systemctl --user stop ssf.service`, or `brew services stop ssf`)
   and run `ssf uninstall` again.
2. `sudo pacman -R ssf`, `sudo apt remove ssf` or `sudo dnf remove ssf`
   (**you**: sudo; nothing in ssf runs it); on macOS `brew uninstall
   ssf`, then `brew untap mikekelly/ssf` (`gh` and `lima` stay unless
   you `brew uninstall` them). The command prints the one for this
   machine last.

What stops it: a workspace with uncommitted or unpushed work (an open
item's too), one that cannot be checked (no origin, a git error), or a
VM with a data disk whose clones cannot be checked -- it is stopped, it
gives no report, the disk outlived the instance that mounted it, `[vm]
enabled = false` means ssf never asks its guest, or lima would not say
whether it is running or whether that disk is there. Only a data disk
stops it: what `[vm] dir` holds without one is ssf's own template, ssh
key and share, and goes without a word. Push or discard the work
(`ssf vm start` to check a stopped VM; the message says what to do in
each of the other cases, and it is never `ssf vm start`), or pass
`--force` to go ahead: on the host the work stays where it is; in the VM
the clones live on its data disk and are destroyed with it, checked or
not. `--yes` skips the question for scripted use.

`[vm] dir` is shared by the two backends, and `data.ext4` is
Firecracker's name for its disk. If you switch `[vm] backend` from
`firecracker` to `lima` and keep `[vm] name`, the Firecracker VM's
clones and worktrees stay in `<[vm] dir>/<name>` -- where lima knows
nothing of them and the destroy would remove the directory with them in
it. A healthy lima guest reports a clean machine, because that disk is
on nothing lima mounted, so this is checked on the host: `ssf uninstall`
refuses, names the file, and says to put `[vm] backend` back to
`firecracker` and run it again to reach the work on it. A directory ssf
could not read counts the same way, since it cannot rule the disk out.
`--force` goes ahead.

Only "not found" counts as "there is no data disk". A directory ssf
could not read at all -- a lima home an earlier `sudo` left root-owned,
which is itself one of the reasons `limactl` fails, or a volume that
has gone away -- is a question that was never answered, and the command
refuses over it rather than destroying workspaces nobody checked.
`--force` still goes ahead. For the same reason a destroy that cannot
look for the data disk now fails the step instead of reporting that
lima's home holds no such disk.

`[vm] name` has to name a directory under `[vm] dir`: it is joined onto
that path and `ssf vm destroy` and `ssf uninstall` remove the result
whole, so an empty name (which would be `[vm] dir` itself), an absolute
one, or one containing `..` is refused when the config is read. Nested
names like `a/b` are fine.

What it keeps, and lists at the end: the clones and worktrees under
`~/ssf/projects` (or Orca's projects; may hold unpushed work), the `[vm]
dir` (the image and downloads; retained -- ssf does not look inside it,
so inspect it before removing it), and, unless you pass
`--data`, `~/.config/ssf` (config and the bot's key) and
`~/.local/state/ssf` (state, and the marker that keeps a disabled service
off, so a reinstall stays stopped until `ssf ui service enable`; with
`--data` gone, a reinstall starts the service). Under lima the instance
and the data disk go out of lima's own home with `vm destroy`, but
`~/.lima` itself stays, holding lima's cache of downloaded images; the
report does not name it, so remove it by hand once nothing else of yours
uses lima. The bot GitHub account itself is not touched, nor its gh
sign-in. `ssf status` afterwards says not signed in and stopped; the
watched repositories and the records of past items still show until
`--data` (or a reinstall from scratch) clears them. With the VM gone the
config's `vm.enabled` is cleared, so `status` does not go looking for it.
