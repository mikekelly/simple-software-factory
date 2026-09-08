#!/bin/sh
# Post-remove script of the ssf .deb and .rpm (nfpm embeds it as postrm /
# %postun). A port of post_remove in ../ssf.install.
#   deb: `remove`, `purge` => removal; `upgrade`, `failed-upgrade`, `abort-*`,
#        `disappear` => not a removal
#   rpm: `0` removal, `1` upgrade
# Nothing here may fail the package operation; the script exits 0.
set -u

case "${1:-}" in
  remove|purge|0)
    cat <<'MSG'
==> ssf removed. Per-user files are left in place:
    ~/.config/ssf (config, bot key), ~/.local/state/ssf (state),
    ~/ssf/projects (clones and worktrees), ~/.local/share/ssf/vm (the microVM's disks).
MSG
    ;;
esac
exit 0
