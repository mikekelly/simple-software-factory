#!/bin/bash
# What goes into the image, run once as root inside the provisioning boot
# with the network up. Edit the package lists here and run `ssf vm build
# --force` to make a new image.
set -euxo pipefail
export HOME=/root
export TERM=dumb
echo 'Server = https://geo.mirror.pkgbuild.com/$repo/os/$arch' > /etc/pacman.d/mirrorlist
pacman-key --init
pacman-key --populate archlinux
pacman -Sy --noconfirm archlinux-keyring
pacman -Su --noconfirm --needed base openssh sudo git github-cli nodejs npm tmux \
    less which vim bash-completion man-db ripgrep jq unzip
# Locale and time.
sed -i 's/^#en_US.UTF-8/en_US.UTF-8/' /etc/locale.gen
locale-gen
echo 'LANG=en_US.UTF-8' > /etc/locale.conf
ln -sf /usr/share/zoneinfo/UTC /etc/localtime
# The unprivileged user everything runs as (Claude Code refuses its
# permission-free mode as root).
id ssf >/dev/null 2>&1 || useradd -m -U -s /bin/bash ssf
install -d -m 700 -o ssf -g ssf /home/ssf/.ssh /home/ssf/.config
# Claude Code's first-run questions are answered up front: the VM is the
# sandbox its bypass-permissions warning asks for. A `[vm] files` entry for
# ~/.claude.json replaces this.
printf '{"hasCompletedOnboarding": true, "bypassPermissionsModeAccepted": true}\n' > /home/ssf/.claude.json
chown ssf:ssf /home/ssf/.claude.json
# Environment for ssh sessions and the units: ssf's state lives on the data disk.
cat > /etc/environment <<'ENV'
SSF_STATE_DIR=/var/lib/ssf/state
HERDR_COMMAND=/usr/local/bin/herdr
ENV
cat > /etc/profile.d/ssf.sh <<'PROF'
export SSF_STATE_DIR=/var/lib/ssf/state
export HERDR_COMMAND=/usr/local/bin/herdr
PROF
# sshd: keys only, the ssf user only.
cat > /etc/ssh/sshd_config.d/ssf.conf <<'SSHD'
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
AllowUsers ssf
AcceptEnv LANG LC_* SSF_*
SSHD
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
# what fetch their native binaries.
for d in /usr/lib/node_modules/*/ /usr/lib/node_modules/@*/*/; do
    if [ -f "$d/package.json" ] && jq -e '.scripts.postinstall' "$d/package.json" >/dev/null 2>&1; then
        (cd "$d" && npm run postinstall) || echo "provision: postinstall of $d failed" >&2
    fi
done
for c in claude codex gemini copilot opencode pi grok; do
    if command -v "$c" >/dev/null 2>&1; then
        echo "provision: $c $("$c" --version 2>&1 | head -1)"
    else
        echo "provision: $c not installed" >&2
    fi
done
# Crush ships release tarballs.
crush_url=$(curl -fsSL https://api.github.com/repos/charmbracelet/crush/releases/latest \
    | jq -r '.assets[] | select(.name | test("_Linux_x86_64.tar.gz$")) | .browser_download_url' | head -1)
if [ -n "$crush_url" ]; then
    curl -fsSL "$crush_url" | tar -xz -C /tmp && install -m755 /tmp/crush*/crush /usr/local/bin/crush \
        || echo "provision: crush install failed" >&2
fi
# Oh My Pi ships release binaries.
omp_url=$(curl -fsSL https://api.github.com/repos/can1357/oh-my-pi/releases/latest \
    | jq -r '.assets[] | select(.name | test("linux.*(x64|x86_64|amd64)")) | .browser_download_url' | head -1)
if [ -n "$omp_url" ]; then
    (cd /tmp && curl -fsSL -o omp.dl "$omp_url" && case "$omp_url" in
        *.tar.gz|*.tgz) tar -xzf omp.dl && install -m755 "$(find . -maxdepth 2 -type f -name omp | head -1)" /usr/local/bin/omp ;;
        *.zip) unzip -o omp.dl >/dev/null && install -m755 "$(find . -maxdepth 2 -type f -name omp | head -1)" /usr/local/bin/omp ;;
        *) install -m755 omp.dl /usr/local/bin/omp ;;
    esac) || echo "provision: omp install failed" >&2
fi
# Services: network, seed, sshd, herdr, ssf.
systemctl enable gvforwarder.service ssf-net.service ssf-seed.service sshd.service herdr-server.service ssf.service
systemctl disable systemd-networkd.service systemd-resolved.service 2>/dev/null || true
# The serial console gets a login prompt for debugging from `ssf vm console`.
systemctl enable serial-getty@ttyS0.service
mkdir -p /var/lib/ssf /seed
chown ssf:ssf /var/lib/ssf
rm -f /usr/local/lib/ssf/provision-init.sh
pacman -Scc --noconfirm >/dev/null
