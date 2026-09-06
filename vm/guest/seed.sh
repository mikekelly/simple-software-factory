#!/bin/bash
# At every boot: mount the seed disk the host made (`/dev/vdc`) and the data
# disk (`/dev/vdb`), and put the host's ssf binary, config, token, ssh key
# and listed files where the guest expects them.
set -euo pipefail
mkdir -p /seed /var/lib/ssf
mount -o ro /dev/vdc /seed
if ! mount /dev/vdb /var/lib/ssf 2>/dev/null; then
    mkfs.ext4 -q -L ssf-data /dev/vdb
    mount /dev/vdb /var/lib/ssf
fi
install -d -o ssf -g ssf /var/lib/ssf/state /var/lib/ssf/projects
install -m755 /seed/ssf /usr/local/bin/ssf
rm -rf /home/ssf/.config/ssf
cp -r /seed/config /home/ssf/.config/ssf
chmod 700 /home/ssf/.config/ssf
install -d -m700 -o ssf -g ssf /home/ssf/.ssh
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
        install -D -m600 -o ssf -g ssf "/seed/files/$n" "$dest"
    done < /seed/files.list
fi
# git in the guest (the daemon's own clones, and a shell) pushes and pulls
# as the bot: the token through ssf's credential helper, no other helper.
cat > /home/ssf/.gitconfig <<'GIT'
[credential]
	helper =
	helper = !/usr/local/bin/ssf git-credential
GIT
chown ssf:ssf /home/ssf/.gitconfig
chown -R ssf:ssf /home/ssf/.config
