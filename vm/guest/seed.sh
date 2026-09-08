#!/bin/bash
# At every boot (Firecracker): mount the data disk (`/dev/vdb`) and the seed
# disk the host made (`/dev/vdc`), then seed the guest from it
# (seed-common.sh: home on the data disk, ssf binary, config, keys, files).
set -euo pipefail
. /usr/local/lib/ssf/seed-common.sh
mkdir -p /seed /var/lib/ssf
mount -o ro /dev/vdc /seed
# The host formats the data disk before the first boot. It is checked and
# mounted here, never formatted: a mount that fails stops this unit, and
# with it sshd, herdr and the daemon, rather than lose what is on it.
fsck.ext4 -p /dev/vdb >/dev/null || true
mount /dev/vdb /var/lib/ssf
seed_from /seed
