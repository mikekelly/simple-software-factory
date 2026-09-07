#!/usr/bin/env bash
# Run ssf.service from a dev build instead of the package's /usr/bin/ssf.
# For whoever works on ssf itself; a user installs the package (README).
#
#   packaging/dev-install.sh          # build target/release/ssf, point the unit at it, restart
#   packaging/dev-install.sh --undo   # back to the package's binary
#
# The package has to be installed once for the unit and the bar widget
# (`cd packaging && makepkg -si`); this script says so if it is not. The
# drop-in it writes survives package upgrades, so the service keeps running
# the dev build until --undo removes it. Nothing here touches ~/.config/ssf
# or the running factory's state.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dropin="$HOME/.config/systemd/user/ssf.service.d/dev-build.conf"

case "${1:-}" in
  --undo)
    rm -f "$dropin"
    systemctl --user daemon-reload
    systemctl --user try-restart ssf.service
    echo "ssf.service runs /usr/bin/ssf again"
    exit 0
    ;;
  -h|--help)
    sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") ;;
  *) echo "dev-install.sh: unknown option $1 (--undo, --help)" >&2; exit 2 ;;
esac

pacman -Qq ssf >/dev/null 2>&1 || {
  echo "dev-install.sh: the ssf package is not installed; install it once for the unit and the widget:" >&2
  echo "  cd $repo/packaging && makepkg -si" >&2
  exit 1
}

echo "==> cargo build --release in $repo"
(cd "$repo" && cargo build --release)
bin="$repo/target/release/ssf"

echo "==> pointing ssf.service at $bin ($dropin)"
mkdir -p "$(dirname "$dropin")"
cat >"$dropin" <<UNIT
# Written by packaging/dev-install.sh: run the service from a dev build
# rather than the package's /usr/bin/ssf. \`packaging/dev-install.sh --undo\`
# removes this file and restarts the service on the package again.
[Service]
ExecStartPre=
ExecStartPre=-$bin ui install --quiet
ExecStart=
ExecStart=$bin run
UNIT
systemctl --user daemon-reload
systemctl --user restart ssf.service
systemctl --user --no-pager status ssf.service 2>/dev/null | sed -n '1,4p' || true

echo "==> $bin doctor"
"$bin" doctor || true
