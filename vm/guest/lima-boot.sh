#!/bin/bash
# lima's `provision:` script (mode: system) execs this on every boot, as
# root, from the host's share mounted at /mnt/ssf. The first boot provisions
# the guest (provision.sh, backend lima) and marks it in /etc/ssf-image-built,
# then starts ssf-seed.service so the host can reach the guest as `ssf` in
# the same boot; later boots find the marker and leave the work to the
# enabled units. A failed provisioning leaves no marker: the host checks
# for it over ssh, and the next boot tries again.
#
# Everything an attempt says goes into $log as well as to lima's own output,
# from the first line on. The host (src/vm/lima.rs) watches that log: a
# non-empty log with none of the guest scripts running is how it sees a dead
# provisioning within seconds instead of waiting out its half-hour timeout.
# So a failure this script reports itself -- the share never appearing --
# has to be in the log too, not only on stdout.
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
marker=/etc/ssf-image-built
log=/var/log/ssf-provision.log
guest=/mnt/ssf/guest
if [ -f "$marker" ]; then
    exit 0
fi
# This boot is a provisioning attempt, so the log starts empty here rather
# than each attempt appending to the last one's. The alternative was to tag
# every attempt and let the host tell the runs apart; truncating is chosen
# because it keeps the host's probe a one-line shell test, and because the
# log a failed attempt left has already been printed by the host that saw it
# fail. Without this, the log of a failed provisioning would still be there
# when the next boot begins, and the host would read it as "this attempt
# died" in its first seconds.
: > "$log"
# Say it once into both: lima's console/output and the log the host reads.
say() {
    printf '%s\n' "$*" | tee -a "$log"
}
# The share is mounted by lima before its provisioning scripts run (9p and
# virtiofs); give a slow mount a chance rather than fail on it.
say "ssf-provision: waiting for $guest/provision.sh"
for _ in $(seq 120); do
    [ -f "$guest/provision.sh" ] && break
    sleep 1
done
if [ ! -f "$guest/provision.sh" ]; then
    say "ssf-provision: FAILED: $guest/provision.sh not visible after 120s (is /mnt/ssf mounted?)"
    exit 1
fi
say "ssf-provision: provisioning the guest, log in $log"
if SSF_VM_BACKEND=lima bash "$guest/provision.sh" >>"$log" 2>&1; then
    date -u +%Y-%m-%dT%H:%M:%SZ > "$marker"
else
    rc=$?
    say "ssf-provision: FAILED: provision.sh exited $rc (see $log)"
    tail -n 20 "$log"
    exit 1
fi
# The units were enabled after multi-user.target was processed, so this
# boot has to start them itself; queued rather than waited for, since they
# are ordered after cloud-init's final stage, which is what runs this
# script. The seed runs the moment provisioning ends and the host waits for
# ssh as `ssf`; herdr and the daemon follow it (a build stops the VM again,
# a reset's start goes on to use them).
systemctl daemon-reload
systemctl start --no-block ssf-seed.service herdr-server.service ssf.service
say "ssf-provision: DONE"
