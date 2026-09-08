#!/bin/bash
# lima's `provision:` script (mode: system) execs this on every boot, as
# root, from the host's share mounted at /mnt/ssf. The first boot provisions
# the guest (provision.sh, backend lima) and marks it in /etc/ssf-image-built,
# then starts ssf-seed.service so the host can reach the guest as `ssf` in
# the same boot; later boots find the marker and leave the work to the
# enabled units. A failed provisioning leaves no marker: the host checks
# for it over ssh, and the next boot tries again.
#
# This script is itself in the share, so by the time it runs the share is
# mounted: waiting for /mnt/ssf is not its job and cannot be (an `exec` of a
# file that is not there leaves nothing behind to read). That wait, and the
# message when the share never appears, are in the template's provision hook
# instead (src/vm/lima.rs, boot_hook). What is left here is a check that the
# share is the whole share and not half of one.
#
# Everything an attempt says goes into $log as well as to lima's own output,
# from the first line on. The host (src/vm/lima.rs) watches that log: a
# non-empty log with none of the guest scripts running is how it sees a dead
# provisioning within seconds instead of waiting out its half-hour timeout.
# The log is emptied by the template's boot hook (src/vm/lima.rs,
# boot_hook), in its first line and so before the wait for the share: doing
# it here instead left the previous attempt's log on disk for the length of
# that wait, where the host read it as this attempt's.
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
marker=/etc/ssf-image-built
log=/var/log/ssf-provision.log
guest=/mnt/ssf/guest
if [ -f "$marker" ]; then
    exit 0
fi
# Say it once into both: lima's console/output and the log the host reads.
say() {
    printf '%s\n' "$*" | tee -a "$log"
}
if [ ! -f "$guest/provision.sh" ]; then
    say "ssf-provision: FAILED: $guest/provision.sh is not there, though $0 is; the share is incomplete"
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
