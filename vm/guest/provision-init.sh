#!/bin/bash
# PID 1 of the one-off provisioning boot (`ssf vm build`): brings up the
# network through gvforwarder, runs provision.sh, and reboots, which makes
# Firecracker exit. Nothing here survives into the runtime image except
# what provision.sh installs.
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev 2>/dev/null || true
mkdir -p /dev/pts /dev/shm /run /tmp
mount -t devpts devpts /dev/pts
mount -t tmpfs tmpfs /dev/shm
mount -t tmpfs tmpfs /run
mount -t tmpfs tmpfs /tmp
mount -o remount,rw /
echo ssf-vm > /etc/hostname
ip link set lo up
finish() {
    echo "ssf-provision: $1"
    sync
    echo b > /proc/sysrq-trigger
    sleep 60
}
if ! /usr/local/lib/ssf/net-up.sh; then
    finish "FAILED: no network"
fi
/usr/local/bin/gvforwarder -url vsock://2:1024/connect -iface tap0 -mtu 1500 -preexisting >/run/gvforwarder.log 2>&1 &
sleep 1
if ! curl -fsS -o /dev/null --max-time 20 https://geo.mirror.pkgbuild.com/; then
    cat /run/gvforwarder.log
    finish "FAILED: no network"
fi
if /usr/local/lib/ssf/provision.sh; then
    date -u +%Y-%m-%dT%H:%M:%SZ > /etc/ssf-image-built
    finish "DONE"
else
    finish "FAILED: provision.sh exited $?"
fi
