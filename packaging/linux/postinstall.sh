#!/bin/sh
set -u
# Inside an ssf VM guest (its boot scripts are in /usr/local/lib/ssf) the
# package provides ssf: refresh the boot script from it and point the paths
# the guest's units and git config use at the packaged binaries.
if [ -f /usr/local/lib/ssf/seed-common.sh ]; then
    install -m644 /usr/share/ssf/vm/guest/seed-common.sh /usr/local/lib/ssf/seed-common.sh
    ln -sfn /usr/bin/ssf /usr/local/bin/ssf
    ln -sfn /usr/bin/ssf-server /usr/local/bin/ssf-server
    exit 0
fi
# deb: `configure <old-version>` is an upgrade; restart running services on
# the new version (#638). The rpm does this in %posttrans (posttrans.sh).
if [ "${1:-}" = configure ] && [ -n "${2:-}" ]; then
    /usr/lib/ssf/package-post-upgrade
fi
echo "==> ssf installed or upgraded. Run 'ssf setup' as the user who will run the factory."
exit 0
