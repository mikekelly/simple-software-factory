#!/bin/sh
# Restarts each logged-in user's running, package-owned ssf services after a
# package upgrade, so the new version takes effect (#638). Run by Arch's
# post_upgrade, the .deb's postinst and the .rpm's %posttrans.
#
# A restart must never stop a VM or an agent: a VM runs in ssf-vm-*.scope
# and host mode's herdr in ssf-herdr.scope, outside the service (#600,
# #602), and the next daemon reattaches to them. A service started by an
# older ssf may still hold its VM or herdr inside its own cgroup; that one,
# or one whose cgroup cannot be read, is left running with a message.
# Nothing here may fail the package operation; the script exits 0.
set -u
# An ssf VM guest upgrades its own daemon (`ssf vm upgrade`).
[ -f "${SSF_GUEST_SEED:-/usr/local/lib/ssf/seed-common.sh}" ] && exit 0
cgroup_root=${SSF_CGROUP_ROOT:-/sys/fs/cgroup}
proc_root=${SSF_PROC_ROOT:-/proc}
users=$(loginctl list-users --no-legend 2>/dev/null | awk '{print $2}') || users=
for user in $users; do
  ctl="systemctl --machine=${user}@.host --user"
  case "$($ctl is-system-running 2>/dev/null)" in
    running|degraded|starting) ;;
    offline) continue ;;
    *) echo "==> could not reach $user's user manager; if ssf runs there, restart it: systemctl --user restart ssf.service (or ssf@NAME.service)" >&2; continue ;;
  esac
  target_files=$($ctl list-unit-files --no-legend --plain 'ssf@*.service' 2>/dev/null) || target_files=
  target_units=$($ctl list-units --all --no-legend --plain 'ssf@*.service' 2>/dev/null) || target_units=
  targets=$(printf '%s\n%s\n' "$target_files" "$target_units" | awk '$1 ~ /^ssf@[^[:space:]]+\.service$/ {print $1}' | sort -u)
  $ctl daemon-reload || { echo "==> warning: could not reload the user manager for $user; restart ssf's services yourself" >&2; continue; }
  for unit in ssf.service $targets; do
    $ctl is-active --quiet "$unit" >/dev/null 2>&1 || continue
    fragment=$($ctl show -p FragmentPath --value "$unit" 2>/dev/null) || continue
    case "$unit:$fragment" in
      ssf.service:/usr/lib/systemd/user/ssf.service|ssf@*.service:/usr/lib/systemd/user/ssf@.service) ;;
      *) continue ;;
    esac
    exec_start=$($ctl show -p ExecStart --value "$unit" 2>/dev/null) || continue
    target=${unit#ssf@}; target=${target%.service}
    if [ "$unit" = ssf.service ]; then expected='argv[]=/usr/bin/ssf-server ;'
    else expected="argv[]=/usr/bin/ssf-server --target $target ;"
    fi
    [ "$(printf '%s' "$exec_start" | grep -o 'path=' | wc -l)" -eq 1 ] &&
      printf '%s' "$exec_start" | grep -F 'path=/usr/bin/ssf-server ;' >/dev/null &&
      printf '%s' "$exec_start" | grep -F "$expected" >/dev/null || continue
    cgroup=$($ctl show -p ControlGroup --value "$unit" 2>/dev/null) || cgroup=
    procs=
    if [ -n "$cgroup" ] && [ -d "$cgroup_root$cgroup" ]; then
      procs=$(find "$cgroup_root$cgroup" -name cgroup.procs -exec cat {} + 2>/dev/null) || cgroup=
    else
      cgroup=
    fi
    if [ -z "$cgroup" ]; then
      echo "==> $unit for $user was not restarted: its processes could not be inspected, and a restart might stop its VM or agents. Restart it when that suits: systemctl --user restart $unit" >&2
      continue
    fi
    inside=
    for pid in $procs; do
      argv=$(tr '\0' '\n' <"$proc_root/$pid/cmdline" 2>/dev/null | head -n 2 | sed '1s|.*/||' | tr '\n' ' ')
      case "$argv" in
        "firecracker "*|"gvproxy "*|"limactl hostagent "*|qemu-system-*) inside="its VM" ;;
        "herdr "*) inside="its herdr session and agents" ;;
      esac
      [ -n "$inside" ] && break
    done
    if [ -n "$inside" ]; then
      echo "==> $unit for $user was not restarted: it was started by an older ssf and runs $inside inside the service, which a restart would stop. Restart it when that suits: systemctl --user restart $unit" >&2
      continue
    fi
    if $ctl try-restart "$unit"; then
      echo "==> restarted $unit for $user"
    else
      echo "==> warning: could not restart $unit for $user" >&2
    fi
  done
done
exit 0
