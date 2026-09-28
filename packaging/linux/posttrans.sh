#!/bin/sh
# %posttrans of the ssf .rpm: restart running services on the new version
# (#638). On a fresh install no package-owned service is running yet, so
# this restarts nothing.
/usr/lib/ssf/package-post-upgrade
exit 0
