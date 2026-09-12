#!/bin/bash
# Loaded by omarchy-iso's test/integration runner. Do not run directly.
source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/base-test.sh"
base_image_ready || { echo "No reusable base image" >&2; exit 1; }
[[ -d ${SSF_PLUGIN_REPO:-} && -f ${SSF_PACKAGE:-} && -x ${SSF_LEGACY_HELPER:-} ]] || { echo "SSF source/package/legacy fixture missing" >&2; exit 1; }

type_guest_password() {
  local i
  for ((i=0; i<${#GUEST_PASSWORD}; i++)); do press "${GUEST_PASSWORD:i:1}"; sleep .25; done
}
copy_source() {
  local source=$1 destination=$2
  while IFS= read -r -d '' path; do
    [[ -e "$source/$path" || -L "$source/$path" ]] && printf '%s\0' "$path"
  done < <(git -C "$source" ls-files --cached --others --exclude-standard -z) |
    tar -C "$source" --null --files-from=- -cf - |
    ssh -i "$SSH_KEY" -p "$SSH_PORT" -o BatchMode=yes -o IdentitiesOnly=yes \
      -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR \
      "$GUEST_USER@127.0.0.1" \
      "test ! -e '$destination' && mkdir -p '$destination' && tar -C '$destination' -xf - && git -C '$destination' init -q && git -C '$destination' add . && git -C '$destination' -c user.name=Test -c user.email=test@example.com commit -qm source"
}
copy_file() {
  scp -i "$SSH_KEY" -P "$SSH_PORT" -o BatchMode=yes -o IdentitiesOnly=yes \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR "$1" "$GUEST_USER@127.0.0.1:$2"
}

start_vm_from_base
wait_for_ssh "$BOOT_TIMEOUT"
type_guest_password; press ret
check "exact Quickshell process is ready" ssh_guest "timeout 120 bash -c 'until pgrep -x quickshell >/dev/null; do sleep 2; done'"
copy_source "$SSF_PLUGIN_REPO" /var/tmp/ssf-plugin-source
copy_file "$SSF_PACKAGE" /var/tmp/ssf.pkg.tar.zst
copy_file "$SSF_LEGACY_HELPER" /var/tmp/ssf-legacy-helper
check "guest received the immutable package artifact" ssh_guest "echo '$SSF_PACKAGE_SHA256  /var/tmp/ssf.pkg.tar.zst' | sha256sum -c -"
ssh_guest 'for x in cargo rustc mise; do printf "%s=%s\n" "$x" "$(command -v "$x" || true)"; done' >"$RUN_DIR/tools-before"

log "Widget first: actionable missing application, no runtime mutation"
ssh_guest "export OMARCHY_PATH=/usr/share/omarchy; omarchy plugin add file:///var/tmp/ssf-plugin-source --enable --yes" >"$RUN_DIR/widget-add.log" 2>&1
check "widget is valid, discovered and enabled" ssh_guest "export OMARCHY_PATH=/usr/share/omarchy; omarchy plugin validate ~/.config/omarchy/plugins/ssf.factory && omarchy plugin list --json | jq -e 'any(.[]; .id == \"ssf.factory\" and .enabled == true)'"
check "widget does not install or configure ssf" ssh_guest 'test ! -e /usr/bin/ssf && test ! -e ~/.config/ssf && test ! -e ~/.local/state/ssf && test ! -e ~/.config/systemd/user/default.target.wants/ssf.service'
check "missing-package state gives pacman command" ssh_guest "grep -Fq 'sudo pacman -U <package-file>' ~/.config/omarchy/plugins/ssf.factory/marketplace/FactoryPanel.qml"
check "widget has distinct actionable service-failure state" ssh_guest "grep -Fq 'The Software Factory service failed. Open Logs for details.' ~/.config/omarchy/plugins/ssf.factory/marketplace/FactoryPanel.qml && grep -Fq 'Could not read the Software Factory service state.' ~/.config/omarchy/plugins/ssf.factory/marketplace/FactoryPanel.qml"
ssh_guest 'for x in cargo rustc mise; do printf "%s=%s\n" "$x" "$(command -v "$x" || true)"; done' >"$RUN_DIR/tools-after"
check "widget did not install build tools" cmp "$RUN_DIR/tools-before" "$RUN_DIR/tools-after"
capture_console "success-widget-first"

log "Real pacman install with repository dependency resolution"
ssh_sudo "env OMARCHY_ALLOW_DIRECT_PACMAN=1 pacman -Syu --noconfirm" >"$RUN_DIR/pacman-sync.log" 2>&1
ssh_sudo "pacman -U --noconfirm /var/tmp/ssf.pkg.tar.zst" >"$RUN_DIR/pacman-install.log" 2>&1
check "pacman owns /usr/bin/ssf" ssh_guest "pacman -Qo /usr/bin/ssf | grep -q 'is owned by ssf'"
check "declared dependencies are all installed" ssh_guest 'test -z "$(pacman -T $(pacman -Qi ssf | sed -n '\''/^Depends On/{s/^Depends On *: //;p}'\''))"'
check "package install made no per-user state" ssh_guest 'test ! -e ~/.config/ssf && test ! -e ~/.local/state/ssf && ! systemctl --user is-enabled --quiet ssf.service'
check "widget distinguishes missing configuration" ssh_guest "grep -Fq 'Software Factory is installed but not configured.' ~/.config/omarchy/plugins/ssf.factory/marketplace/FactoryPanel.qml"

log "Migrate a positively-owned prior custom runtime during explicit setup"
ssh_guest 'mkdir -p ~/.local/share/ssf/marketplace/bin ~/.local/bin ~/.config/systemd/user; cp /usr/bin/ssf ~/.local/share/ssf/marketplace/bin/ssf; printf "owner=ssf-marketplace-v1\nversion=legacy\n" >~/.local/share/ssf/marketplace/install.env; cp /var/tmp/ssf-legacy-helper ~/.local/share/ssf/marketplace/ssf-marketplace; chmod +x ~/.local/share/ssf/marketplace/ssf-marketplace; ln -s ~/.local/share/ssf/marketplace/bin/ssf ~/.local/bin/ssf; printf "%s\n" "# Managed by ssf-marketplace; do not edit." "[Service]" "ExecStart=%h/.local/share/ssf/marketplace/bin/ssf run" "[Install]" "WantedBy=default.target" >~/.config/systemd/user/ssf.service'
ssh_guest 'pid=$(pgrep -xo quickshell); while IFS= read -r -d "" pair; do case "$pair" in WAYLAND_DISPLAY=*|HYPRLAND_INSTANCE_SIGNATURE=*|XDG_RUNTIME_DIR=*|DBUS_SESSION_BUS_ADDRESS=*) export "$pair";; esac; done <"/proc/$pid/environ"; omarchy-launch-floating-terminal-with-presentation /usr/bin/ssf setup >/dev/null 2>&1 &'
check "visible setup requests linger authorization" ssh_guest "timeout 120 bash -c 'until ps -C sudo -o args= | grep -Fx \"sudo loginctl enable-linger $GUEST_USER\"; do sleep 2; done'"
capture_console "success-visible-setup-auth"
type_guest_password; press ret
sleep 5
if ssh_guest "ps -C sudo -o args= | grep -Fx 'sudo loginctl enable-linger $GUEST_USER'" >/dev/null 2>&1; then
  press esc
  type_guest_password; press ret
fi
check "setup enables linger" ssh_guest "timeout 90 bash -c 'until loginctl show-user \"$GUEST_USER\" -p Linger --value | grep -qx yes; do sleep 2; done'"
check "setup enables packaged default.target unit" ssh_guest 'systemctl --user is-enabled --quiet ssf.service && test -L ~/.config/systemd/user/default.target.wants/ssf.service'
check "setup records explicit completion" ssh_guest "grep -qx ssf-setup-v1 ~/.local/state/ssf/setup-complete"
check "service status reports this user configured" ssh_guest "/usr/bin/ssf ui service status --json | jq -e '.configured == true and .enabled == true'"
check "legacy runtime and PATH shadow are removed" ssh_guest 'test ! -e ~/.local/share/ssf/marketplace && test ! -e ~/.local/bin/ssf && test "$(command -v ssf)" = /usr/bin/ssf'
check "authentication remains separately required" ssh_guest "/usr/bin/ssf doctor 2>&1 | grep -qiE 'bot|auth|sign.?in|GitHub'"
ssh_guest 'mkdir -p ~/.config/ssf ~/.local/state/ssf ~/ssf/projects/keep-me; printf config >~/.config/ssf/preserved; printf state >~/.local/state/ssf/preserved; printf work >~/ssf/projects/keep-me/work'

log "Boot and pre-login persistence"
ssh_sudo "systemctl reboot" || true
waited=0; while ssh_guest true 2>/dev/null && ((waited<60)); do sleep 1; ((waited+=1)); done
((waited<60)) || { echo "Guest SSH never went down" >&2; exit 1; }
wait_for_ssh "$BOOT_TIMEOUT"
check "linger and service enablement survive reboot" ssh_guest "loginctl show-user '$GUEST_USER' -p Linger --value | grep -qx yes && systemctl --user is-enabled --quiet ssf.service"
check "service attempted before graphical login" ssh_guest "timeout 90 bash -c 'until journalctl --user -b -u ssf.service --no-pager | grep -qiE \"bot|auth|sign.?in|GitHub\"; do sleep 2; done'"
sleep 8; type_guest_password; press ret
check "Quickshell returns after reboot" ssh_guest "timeout 120 bash -c 'until pgrep -x quickshell >/dev/null; do sleep 2; done'"

log "Package update preserves user and widget state"
ssh_sudo "pacman -U --noconfirm /var/tmp/ssf.pkg.tar.zst" >"$RUN_DIR/pacman-upgrade.log" 2>&1
check "update preserves service, widget and data" ssh_guest 'systemctl --user is-enabled --quiet ssf.service && test -d ~/.config/omarchy/plugins/ssf.factory && grep -qx config ~/.config/ssf/preserved && grep -qx state ~/.local/state/ssf/preserved && grep -qx work ~/ssf/projects/keep-me/work'

log "SSF cleanup and package removal retain widget missing state"
ssh_guest "/usr/bin/ssf uninstall --yes" >"$RUN_DIR/ssf-cleanup.log" 2>&1
check "SSF cleanup leaves widget enabled" ssh_guest "export OMARCHY_PATH=/usr/share/omarchy; omarchy plugin list --json | jq -e 'any(.[]; .id == \"ssf.factory\" and .enabled == true)'"
# Reactivate the opted-in unit so pacman's pre-transaction hook itself has to
# stop and disable a running service before removing its executable.
ssh_guest "/usr/bin/ssf setup" >"$RUN_DIR/setup-before-remove.log" 2>&1
ssh_sudo "pacman -R --noconfirm ssf" >"$RUN_DIR/pacman-remove.log" 2>&1
check "package, SSF process and unit enablement are gone" ssh_guest 'test ! -e /usr/bin/ssf && test ! -e /usr/bin/ssf-server && ! pgrep -x ssf-server >/dev/null && ! systemctl --user is-enabled --quiet ssf.service'
check "package removal stopped the owned running service" ssh_guest '! systemctl --user is-active --quiet ssf.service'
check "cleanup preserves requested data" ssh_guest 'grep -qx config ~/.config/ssf/preserved && grep -qx state ~/.local/state/ssf/preserved && grep -qx work ~/ssf/projects/keep-me/work'
check "widget remains enabled in missing-package state" ssh_guest "export OMARCHY_PATH=/usr/share/omarchy; test -d ~/.config/omarchy/plugins/ssf.factory && omarchy plugin list --json | jq -e 'any(.[]; .id == \"ssf.factory\" and .enabled == true)' && grep -Fq 'sudo pacman -U <package-file>' ~/.config/omarchy/plugins/ssf.factory/marketplace/FactoryPanel.qml"

log "Removing only widget leaves reinstalled package and service"
ssh_sudo "pacman -U --noconfirm /var/tmp/ssf.pkg.tar.zst" >"$RUN_DIR/pacman-reinstall.log" 2>&1
ssh_guest "/usr/bin/ssf setup" >"$RUN_DIR/setup-reinstall.log" 2>&1
ssh_guest "export OMARCHY_PATH=/usr/share/omarchy; omarchy plugin remove ssf.factory --yes" >"$RUN_DIR/widget-remove.log" 2>&1
check "widget removal leaves service enabled" ssh_guest 'test ! -e ~/.config/omarchy/plugins/ssf.factory && test -x /usr/bin/ssf && systemctl --user is-enabled --quiet ssf.service'
before=$(ssh_guest 'stat -c "%i:%Y" /usr/bin/ssf; systemctl --user is-enabled ssf.service')
ssh_guest "export OMARCHY_PATH=/usr/share/omarchy; omarchy plugin add file:///var/tmp/ssf-plugin-source --enable --yes" >"$RUN_DIR/widget-reinstall.log" 2>&1
after=$(ssh_guest 'stat -c "%i:%Y" /usr/bin/ssf; systemctl --user is-enabled ssf.service')
check "widget reinstall does not mutate application" test "$before" = "$after"
check "full reinstall retains data" ssh_guest 'grep -qx config ~/.config/ssf/preserved && grep -qx state ~/.local/state/ssf/preserved && grep -qx work ~/ssf/projects/keep-me/work'
capture_console "success-package-widget-reinstalled"
finish
