#!/bin/sh
# Compatibility action for a graphical, same-machine herdr installation.
# Detach all streams so herdr can release the action slot immediately.
command -v ssf >/dev/null 2>&1 || {
    echo "Install the ssf client, then run ssf dashboard on your desktop." >&2
    exit 1
}
nohup ssf dashboard </dev/null >/dev/null 2>&1 &
