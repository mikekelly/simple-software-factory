#!/bin/bash
# Turn the Arch bootstrap tarball into an ext4 root image that `ssf vm build`
# boots once to provision. Needs no root: fakeroot keeps the tarball's
# ownership and mkfs.ext4 -d populates the image from the tree.
#
#   make-base.sh <bootstrap.tar.zst> <out.ext4> <size-gib> <guest-dir> [herdr] [gvforwarder]
#
# <guest-dir> is this directory's guest/ (units and scripts copied into the
# image), <herdr> the herdr binary to ship (the host's /usr/bin/herdr) and
# <gvforwarder> the guest side of gvisor-tap-vsock.
set -euo pipefail
tarball=$1 out=$2 size=$3 guest=$4 herdr=${5:-} gvforwarder=${6:-}
work=$(mktemp -d "${TMPDIR:-/tmp}/ssf-vm-base.XXXXXX")
trap 'rm -rf "$work"' EXIT
export work tarball out size guest herdr gvforwarder
fakeroot -- bash -euo pipefail <<'INNER'
mkdir -p "$work/root"
bsdtar -xf "$tarball" -C "$work/root" --strip-components=1
root=$work/root
install -Dm755 "$guest/provision-init.sh" "$root/usr/local/lib/ssf/provision-init.sh"
install -Dm755 "$guest/provision.sh" "$root/usr/local/lib/ssf/provision.sh"
install -Dm755 "$guest/net-up.sh" "$root/usr/local/lib/ssf/net-up.sh"
install -Dm755 "$guest/seed.sh" "$root/usr/local/lib/ssf/seed.sh"
install -Dm644 "$guest/seed-common.sh" "$root/usr/local/lib/ssf/seed-common.sh"
install -d "$root/etc/systemd/system"
install -m644 "$guest"/units/*.service "$root/etc/systemd/system/"
# The Firecracker drop-ins travel in the image (there is no share here);
# provision.sh installs them under /etc/systemd/system/<unit>.d/ inside it.
install -d "$root/usr/local/lib/ssf/units/firecracker"
install -m644 "$guest"/units/firecracker/*.conf "$root/usr/local/lib/ssf/units/firecracker/"
install -Dm440 "$guest/sudoers" "$root/etc/sudoers.d/ssf"
if [ -n "$gvforwarder" ]; then install -Dm755 "$gvforwarder" "$root/usr/local/bin/gvforwarder"; fi
if [ -n "$herdr" ]; then install -Dm755 "$herdr" "$root/usr/local/bin/herdr"; fi
rm -f "$out"
truncate -s "${size}G" "$out"
mkfs.ext4 -q -L ssf-root -d "$root" "$out"
INNER
echo "made $out from $tarball"
