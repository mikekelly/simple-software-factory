#!/bin/bash
# At every boot (lima; installed as /usr/local/lib/ssf/seed.sh): wait for
# the host's share (`/mnt/ssf`, the seed tree under it) and for the data
# disk lima formats and mounts at `/mnt/lima-<disk>`, grow the data disk's
# filesystem if `ssf vm grow` lengthened it, bind it on /var/lib/ssf, then
# seed the guest from the share (seed-common.sh: home on the data disk, ssf
# binary, config, keys, files). Both waits are here, so the unit needs no
# ordering on lima's own boot scripts.
set -euo pipefail
. /usr/local/lib/ssf/seed-common.sh
share=/mnt/ssf
wait_for 120 "the host share at $share/seed (is $share mounted?)" test -f "$share/seed/ssf"
# lima.env: SSF_VM_DATA_DISK (the lima disk name) and SSF_VM_NAME, written by the host.
. "$share/seed/lima.env"
: "${SSF_VM_DATA_DISK:?seed: $share/seed/lima.env does not set SSF_VM_DATA_DISK}"
data=/mnt/lima-$SSF_VM_DATA_DISK
wait_for 120 "the data disk $SSF_VM_DATA_DISK at $data (lima mounts it; is the disk attached?)" findmnt -n "$data"
# `ssf vm grow` resizes the disk with the VM stopped; the filesystem follows here.
dev=$(findmnt -no SOURCE "$data")
resize2fs "$dev" >/dev/null 2>&1 || echo "seed: resize2fs $dev failed; the data disk keeps its old size" >&2
mkdir -p /var/lib/ssf
findmnt -n /var/lib/ssf >/dev/null || mount --bind "$data" /var/lib/ssf
seed_from "$share/seed"
