#!/bin/bash
# Check or refresh the vendored JavaScript listed in vendor-js.lock.
#   (no argument)  download each file and write it if its SHA-256 matches
#   --check        verify the committed files against the lock, offline
#   --update       download each file and write it and its new SHA-256 into
#                  the lock (after changing a version/URL there); review the diff
set -euo pipefail
cd "$(dirname "$0")/.."
lock=vendor-js.lock
mode=${1:-fetch}
case "$mode" in fetch|--check|--update) ;; *) echo "usage: $0 [--check|--update]" >&2; exit 2 ;; esac
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
status=0
while read -r pkg version path url sum; do
    case "$pkg" in ''|'#'*) continue ;; esac
    if [ "$mode" = --check ]; then
        actual=$(sha256sum "$path" 2>/dev/null | cut -d' ' -f1 || true)
    else
        curl -fsSL -o "$tmp/file" "$url"
        actual=$(sha256sum "$tmp/file" | cut -d' ' -f1)
    fi
    if [ "$actual" = "$sum" ] || [ "$mode" = --update ]; then
        if [ "$mode" != --check ]; then
            mkdir -p "$(dirname "$path")"
            cp "$tmp/file" "$path"
            [ "$actual" = "$sum" ] || sed -i "s|^\($pkg $version $path $url\) $sum\$|\1 $actual|" "$lock"
        fi
        echo "ok      $pkg@$version $path"
    else
        echo "MISMATCH $pkg@$version $path: expected $sum, got ${actual:-nothing}" >&2
        status=1
    fi
done < "$lock"
exit $status
