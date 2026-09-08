#!/bin/bash
# Sourced by the seed scripts (seed.sh for Firecracker, seed-lima.sh for
# lima): what both do once their disks are mounted. Installed as
# /usr/local/lib/ssf/seed-common.sh.

# wait_for <seconds> <what> <command...>: poll the command once a second
# until it succeeds; say what was waited for and fail after the deadline.
wait_for() {
    local secs=$1 what=$2 i
    shift 2
    for ((i = 0; i < secs; i++)); do
        if "$@" >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    echo "seed: gave up waiting ${secs}s for $what" >&2
    return 1
}

# seed_from <seed-dir>: with the data disk on /var/lib/ssf, keep the guest
# user's home there, and put the host's ssf binary, config, token, ssh key
# and listed files where the guest expects them. The root disk then holds
# nothing but packages, so `ssf vm reset` loses no state.
seed_from() {
    local seed=$1 n dest
    # The home directory: on the data disk, seeded from the image's on first use.
    if [ ! -d /var/lib/ssf/home ]; then
        cp -a /home/ssf /var/lib/ssf/home
    fi
    findmnt -n /home/ssf >/dev/null || mount --bind /var/lib/ssf/home /home/ssf
    install -d -o ssf -g ssf /var/lib/ssf/state /var/lib/ssf/projects /home/ssf/.config /home/ssf/.ssh
    chmod 700 /home/ssf/.ssh
    install -m755 "$seed/ssf" /usr/local/bin/ssf
    rm -rf /home/ssf/.config/ssf
    cp -r "$seed/config" /home/ssf/.config/ssf
    chmod 700 /home/ssf/.config/ssf
    install -m600 -o ssf -g ssf "$seed/authorized_keys" /home/ssf/.ssh/authorized_keys
    # Files the host chose to share ([vm] files): <seed>/files/<n> -> the path in files.list.
    if [ -f "$seed/files.list" ]; then
        n=0
        while IFS= read -r dest; do
            n=$((n + 1))
            [ -n "$dest" ] || continue
            case "$dest" in
                /*) ;;
                *) dest=/home/ssf/$dest ;;
            esac
            install -D -m600 "$seed/files/$n" "$dest"
        done < "$seed/files.list"
    fi
    # git in the guest (the daemon's own clones, and a shell) pushes and pulls
    # as the bot: the token through ssf's credential helper, no other helper.
    cat > /home/ssf/.gitconfig <<'GIT'
[credential]
	helper =
	helper = !/usr/local/bin/ssf git-credential
GIT
    chown -R ssf:ssf /home/ssf
}
