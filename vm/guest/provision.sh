#!/bin/bash
# What goes into the image, run once as root inside the provisioning boot
# with the network up. Edit the package lists here and run `ssf vm build
# --force` to make a new image.
#
# SSF_VM_BACKEND: `firecracker` (default) provisions the Arch image
# `ssf vm build` made (the guest files are already in it); `lima` provisions
# a stock cloud image (Arch or Debian/Ubuntu) on its first boot, from the
# host's share mounted at /mnt/ssf, and installs the guest files itself.
set -euxo pipefail
export HOME=/root
export TERM=dumb
backend=${SSF_VM_BACKEND:-firecracker}
share=/mnt/ssf
case "$backend" in
    firecracker) pkg=pacman ;;
    lima)
        if command -v pacman >/dev/null 2>&1; then
            pkg=pacman
        elif command -v apt-get >/dev/null 2>&1; then
            pkg=apt
        else
            echo "provision: no pacman or apt-get in this image; use an Arch or Debian/Ubuntu cloud image ([vm] image)" >&2
            exit 1
        fi
        ;;
    *) echo "provision: SSF_VM_BACKEND=$backend is not firecracker or lima" >&2; exit 1 ;;
esac
machine=$(uname -m)
case "$pkg" in
    pacman)
        sshd_unit=sshd.service
        echo 'Server = https://geo.mirror.pkgbuild.com/$repo/os/$arch' > /etc/pacman.d/mirrorlist
        pacman-key --init
        pacman-key --populate archlinux
        pacman -Sy --noconfirm archlinux-keyring
        pacman -Su --noconfirm --needed base openssh sudo git github-cli nodejs npm tmux \
            less which vim bash-completion man-db ripgrep jq unzip
        ;;
    apt)
        sshd_unit=ssh.service
        export DEBIAN_FRONTEND=noninteractive
        apt-get update
        apt-get install -y openssh-server sudo git gh nodejs npm tmux less vim bash-completion \
            man-db ripgrep jq unzip curl locales
        ;;
esac
# Locale and time.
sed -i 's/^#\s*en_US.UTF-8/en_US.UTF-8/' /etc/locale.gen
locale-gen
echo 'LANG=en_US.UTF-8' > /etc/locale.conf
if command -v update-locale >/dev/null 2>&1; then update-locale LANG=en_US.UTF-8; fi
ln -sf /usr/share/zoneinfo/UTC /etc/localtime
# The user everything runs as: not root, because Claude Code refuses its
# permission-free mode as root, but root through sudo (see sudoers).
id ssf >/dev/null 2>&1 || useradd -m -U -s /bin/bash ssf
install -d -m 700 -o ssf -g ssf /home/ssf/.ssh /home/ssf/.config
# Claude Code's onboarding (theme, login method) is marked done so a
# session goes straight to work; a `[vm] files` entry for ~/.claude.json
# replaces this. Its bypass-permissions acceptance is answered by the driver.
printf '{"hasCompletedOnboarding": true}\n' > /home/ssf/.claude.json
chown ssf:ssf /home/ssf/.claude.json
# Environment for ssh sessions and the units: ssf's state lives on the data
# disk, and SSF_VM_GUEST tells ssf (the daemon's prompts, `ssf guide`, the
# forwarded commands) that it is inside the guest.
if [ "$backend" = firecracker ]; then
    cat > /etc/environment <<'ENV'
SSF_STATE_DIR=/var/lib/ssf/state
HERDR_COMMAND=/usr/local/bin/herdr
SSF_VM_GUEST=1
ENV
else
    # A cloud image's /etc/environment has lines of its own (Ubuntu's PATH); keep them.
    { grep -vE '^(SSF_STATE_DIR|HERDR_COMMAND|SSF_VM_GUEST)=' /etc/environment 2>/dev/null || true
      printf 'SSF_STATE_DIR=/var/lib/ssf/state\nHERDR_COMMAND=/usr/local/bin/herdr\nSSF_VM_GUEST=1\n'
    } > /etc/environment.ssf
    mv /etc/environment.ssf /etc/environment
fi
cat > /etc/profile.d/ssf.sh <<'PROF'
export SSF_STATE_DIR=/var/lib/ssf/state
export HERDR_COMMAND=/usr/local/bin/herdr
export SSF_VM_GUEST=1
PROF
# sshd: keys only. Under Firecracker the ssf user only; under lima, lima's
# own user keeps ssh (limactl shell), so no AllowUsers.
cat > /etc/ssh/sshd_config.d/ssf.conf <<'SSHD'
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
AcceptEnv LANG LC_* SSF_*
SSHD
if [ "$backend" = firecracker ]; then
    sed -i '/^PermitRootLogin no$/a AllowUsers ssf' /etc/ssh/sshd_config.d/ssf.conf
fi
# Under lima there is no image-build step: the guest files come from the
# share (what make-base.sh puts into the Firecracker image), and herdr from
# the share when the host could supply a Linux binary, else from its release.
if [ "$backend" = lima ]; then
    install -Dm755 "$share/guest/seed-lima.sh" /usr/local/lib/ssf/seed.sh
    install -Dm644 "$share/guest/seed-common.sh" /usr/local/lib/ssf/seed-common.sh
    install -Dm440 "$share/guest/sudoers" /etc/sudoers.d/ssf
    install -d /etc/systemd/system
    for u in ssf-seed.service herdr-server.service ssf.service; do
        install -m644 "$share/guest/units/$u" /etc/systemd/system/$u
    done
    install -Dm644 "$share/guest/units/ssf-seed-lima.conf" /etc/systemd/system/ssf-seed.service.d/lima.conf
    if [ -f "$share/herdr" ]; then
        install -m755 "$share/herdr" /usr/local/bin/herdr
    else
        case "$machine" in
            x86_64|aarch64) ;;
            *) echo "provision: no herdr release for $machine; set [vm] herdr to a Linux binary for it" >&2; exit 1 ;;
        esac
        # Release assets are raw binaries named herdr-linux-<x86_64|aarch64>.
        curl -fsSL -o /tmp/herdr "https://github.com/herdrdev/herdr/releases/latest/download/herdr-linux-$machine"
        install -m755 /tmp/herdr /usr/local/bin/herdr
        rm -f /tmp/herdr
    fi
    echo "provision: herdr $(/usr/local/bin/herdr --version 2>&1 | head -1)"
fi
# Harness CLIs (best effort: a failed one is reported, not fatal).
npm_pkgs=(
    @anthropic-ai/claude-code
    @openai/codex
    @google/gemini-cli
    @github/copilot
    opencode-ai
    @earendil-works/pi-coding-agent
    @xai-official/grok
)
for p in "${npm_pkgs[@]}"; do
    npm install -g "$p" || echo "provision: npm install $p failed" >&2
done
# npm runs no install scripts as root, and Claude Code's and OpenCode's are
# what fetch their native binaries. Arch's npm puts global packages under
# /usr/lib, Debian's and Ubuntu's under /usr/local/lib.
npm_roots=(/usr/lib/node_modules /usr/local/lib/node_modules)
npm_root=$(npm root -g 2>/dev/null || true)
if [ -n "$npm_root" ] && [[ " ${npm_roots[*]} " != *" $npm_root "* ]]; then
    npm_roots+=("$npm_root")
fi
for root in "${npm_roots[@]}"; do
    for d in "$root"/*/ "$root"/@*/*/; do
        if [ -f "$d/package.json" ] && jq -e '.scripts.postinstall' "$d/package.json" >/dev/null 2>&1; then
            (cd "$d" && npm run postinstall) || echo "provision: postinstall of $d failed" >&2
        fi
    done
done
# Release downloads pick the asset for this machine: crush names them
# _Linux_x86_64 / _Linux_arm64, omp linux-x64 / linux-arm64.
case "$machine" in
    x86_64) crush_arch=x86_64 omp_arch='x64|x86_64|amd64' ;;
    aarch64|arm64) crush_arch=arm64 omp_arch='arm64|aarch64' ;;
    *) crush_arch= omp_arch= ;;
esac
# Crush ships release tarballs.
if [ -z "$crush_arch" ]; then
    echo "provision: no crush release for $machine; skipped" >&2
    crush_url=
else
    crush_url=$(curl -fsSL https://api.github.com/repos/charmbracelet/crush/releases/latest \
        | jq -r --arg re "_Linux_${crush_arch}.tar.gz\$" '.assets[] | select(.name | test($re)) | .browser_download_url' | head -1 || true)
    if [ -z "$crush_url" ]; then
        echo "provision: crush release not found (GitHub API unreachable or rate-limited); skipped" >&2
    fi
fi
if [ -n "$crush_url" ]; then
    curl -fsSL "$crush_url" | tar -xz -C /tmp && install -m755 /tmp/crush*/crush /usr/local/bin/crush \
        || echo "provision: crush install failed" >&2
fi
# Oh My Pi ships release binaries: the glibc one (`omp-linux-x64`), not
# the musl one next to it, whose loader the guest does not have.
if [ -z "$omp_arch" ]; then
    echo "provision: no omp release for $machine; skipped" >&2
    omp_url=
else
    omp_url=$(curl -fsSL https://api.github.com/repos/can1357/oh-my-pi/releases/latest \
        | jq -r --arg re "^omp-linux-(${omp_arch})\$" '.assets[] | select(.name | test($re)) | .browser_download_url' | head -1 || true)
    if [ -z "$omp_url" ]; then
        echo "provision: omp release not found (GitHub API unreachable or rate-limited); skipped" >&2
    fi
fi
if [ -n "$omp_url" ]; then
    (cd /tmp && curl -fsSL -o omp.dl "$omp_url" && case "$omp_url" in
        *.tar.gz|*.tgz) tar -xzf omp.dl && install -m755 "$(find . -maxdepth 2 -type f -name omp | head -1)" /usr/local/bin/omp ;;
        *.zip) unzip -o omp.dl >/dev/null && install -m755 "$(find . -maxdepth 2 -type f -name omp | head -1)" /usr/local/bin/omp ;;
        *) install -m755 omp.dl /usr/local/bin/omp ;;
    esac) || echo "provision: omp install failed" >&2
fi
# Every harness CLI answers --version, or the build log says which did not.
for c in claude codex gemini copilot opencode pi grok crush omp; do
    if ! command -v "$c" >/dev/null 2>&1; then
        echo "provision: $c not installed" >&2
    elif v=$("$c" --version 2>&1); then
        echo "provision: $c ${v%%$'\n'*}"
    else
        echo "provision: $c does not run: $v" >&2
    fi
done
# herdr's agent integrations (hooks that report the agent's state to herdr,
# and for Claude Code the setting that skips its bypass-permissions warning),
# for the ssf user, for every agent that is installed. herdr wants the
# agent's config directory to exist first.
install -d -o ssf -g ssf /home/ssf/.claude /home/ssf/.codex /home/ssf/.copilot \
    /home/ssf/.pi/agent /home/ssf/.omp/agent /home/ssf/.config/opencode /home/ssf/.grok
for a in claude codex copilot pi omp opencode grok; do
    if command -v "$a" >/dev/null 2>&1; then
        su ssf -c "/usr/local/bin/herdr integration install $a" || echo "provision: herdr integration $a failed" >&2
    fi
done
# Services: seed, sshd, herdr, ssf; under Firecracker also the network
# (gvforwarder over vsock, instead of the systemd network stack), under
# lima the image's own network stays.
if [ "$backend" = firecracker ]; then
    systemctl enable gvforwarder.service ssf-net.service ssf-seed.service sshd.service herdr-server.service ssf.service
    systemctl disable systemd-networkd.service systemd-resolved.service 2>/dev/null || true
else
    systemctl enable ssf-seed.service "$sshd_unit" herdr-server.service ssf.service
fi
# The serial console gets a login prompt for debugging from `ssf vm console`.
systemctl enable serial-getty@ttyS0.service
mkdir -p /var/lib/ssf /seed
chown ssf:ssf /var/lib/ssf
rm -f /usr/local/lib/ssf/provision-init.sh
case "$pkg" in
    pacman) pacman -Scc --noconfirm >/dev/null ;;
    apt) apt-get clean ;;
esac
