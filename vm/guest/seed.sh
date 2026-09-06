#!/bin/bash
# At every boot: mount the data disk (`/dev/vdb`) and the seed disk the host
# made (`/dev/vdc`), keep the guest user's home on the data disk, and put
# the host's ssf binary, config, token, ssh key and listed files where the
# guest expects them. The root disk then holds nothing but packages, so
# `ssf vm reset` loses no state.
set -euo pipefail
mkdir -p /seed /var/lib/ssf
mount -o ro /dev/vdc /seed
if ! mount /dev/vdb /var/lib/ssf 2>/dev/null; then
    mkfs.ext4 -q -L ssf-data /dev/vdb
    mount /dev/vdb /var/lib/ssf
fi
# The home directory: on the data disk, seeded from the image's on first use.
if [ ! -d /var/lib/ssf/home ]; then
    cp -a /home/ssf /var/lib/ssf/home
fi
mount --bind /var/lib/ssf/home /home/ssf
install -d -o ssf -g ssf /var/lib/ssf/state /var/lib/ssf/projects /home/ssf/.config /home/ssf/.ssh
chmod 700 /home/ssf/.ssh
install -m755 /seed/ssf /usr/local/bin/ssf
rm -rf /home/ssf/.config/ssf
cp -r /seed/config /home/ssf/.config/ssf
chmod 700 /home/ssf/.config/ssf
install -m600 -o ssf -g ssf /seed/authorized_keys /home/ssf/.ssh/authorized_keys
# Files the host chose to share ([vm] files): <seed>/files/<n> -> the path in files.list.
if [ -f /seed/files.list ]; then
    n=0
    while IFS= read -r dest; do
        n=$((n + 1))
        [ -n "$dest" ] || continue
        case "$dest" in
            /*) ;;
            *) dest=/home/ssf/$dest ;;
        esac
        install -D -m600 "/seed/files/$n" "$dest"
    done < /seed/files.list
fi
# git in the guest (the daemon's own clones, and a shell) pushes and pulls
# as the bot: the token through ssf's credential helper, no other helper.
cat > /home/ssf/.gitconfig <<'GIT'
[credential]
	helper =
	helper = !/usr/local/bin/ssf git-credential
GIT
chown -R ssf:ssf /home/ssf
