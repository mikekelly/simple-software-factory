#!/bin/bash
# lima's `provision:` script (mode: system) execs this on every boot, as
# root, from the host's share mounted at /mnt/ssf. The first boot provisions
# the guest (provision.sh, backend lima) and marks it in /etc/ssf-image-built,
# then starts ssf-seed.service so the host can reach the guest as `ssf` in
# the same boot; later boots find the marker and leave the work to the
# enabled units. A failed provisioning leaves no marker: the host checks
# for it over ssh, and the next boot tries again.
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
marker=/etc/ssf-image-built
log=/var/log/ssf-provision.log
guest=/mnt/ssf/guest
if [ -f "$marker" ]; then
    exit 0
fi
# The share is mounted by lima before its provisioning scripts run (9p and
# virtiofs); give a slow mount a chance rather than fail on it.
for _ in $(seq 120); do
    [ -f "$guest/provision.sh" ] && break
    sleep 1
done
if [ ! -f "$guest/provision.sh" ]; then
    echo "ssf-provision: FAILED: $guest/provision.sh not visible after 120s (is /mnt/ssf mounted?)"
    exit 1
fi
echo "ssf-provision: provisioning the guest, log in $log"
if SSF_VM_BACKEND=lima bash "$guest/provision.sh" >"$log" 2>&1; then
    date -u +%Y-%m-%dT%H:%M:%SZ > "$marker"
else
    rc=$?
    echo "ssf-provision: FAILED: provision.sh exited $rc (see $log)"
    tail -n 20 "$log"
    exit 1
fi
systemctl daemon-reload
if systemctl start ssf-seed.service; then
    echo "ssf-provision: DONE"
else
    echo "ssf-provision: FAILED: ssf-seed.service did not start (see $log)"
    journalctl -u ssf-seed.service --no-pager >>"$log" 2>&1
    exit 1
fi
