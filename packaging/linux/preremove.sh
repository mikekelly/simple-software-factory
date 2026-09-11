#!/bin/sh
set -u
case "${1:-}" in upgrade|1) exit 0 ;; esac
failed=0
listing=$(loginctl list-users --no-legend 2>/dev/null) || {
  echo "ssf: refusing package removal: could not list user managers" >&2
  exit 1
}
users=$(printf '%s\n' "$listing" | awk '{print $2}') || exit 1
for user in $users; do
  ctl="systemctl --machine=${user}@.host --user"
  manager=$($ctl is-system-running 2>/dev/null); manager_status=$?
  case "$manager" in
    offline) continue ;;
    running|degraded|starting|stopping) ;;
    *) echo "ssf: refusing package removal: cannot inspect the user manager for $user (status $manager_status: ${manager:-no state})" >&2; failed=1; continue ;;
  esac
  fragment=$($ctl show -p FragmentPath --value ssf.service 2>/dev/null) || {
    echo "ssf: refusing package removal: cannot determine ssf.service ownership for $user" >&2; failed=1; continue;
  }
  [ -n "$fragment" ] || continue
  [ "$fragment" = /usr/lib/systemd/user/ssf.service ] || continue
  exec_start=$($ctl show -p ExecStart --value ssf.service 2>/dev/null) || {
    echo "ssf: refusing package removal: cannot inspect package-owned ssf.service for $user" >&2; failed=1; continue;
  }
  path_count=$(printf '%s' "$exec_start" | grep -o 'path=' | wc -l)
  if [ "$path_count" -ne 1 ] ||
     ! printf '%s' "$exec_start" | grep -F 'path=/usr/bin/ssf ;' >/dev/null ||
     ! printf '%s' "$exec_start" | grep -F 'argv[]=/usr/bin/ssf run ;' >/dev/null; then
    echo "ssf: refusing package removal: package-owned ssf.service has an unrelated effective ExecStart for $user" >&2
    failed=1
    continue
  fi
  if ! $ctl stop ssf.service || $ctl is-active --quiet ssf.service; then
    echo "ssf: refusing package removal: could not stop package-owned ssf.service for $user" >&2
    failed=1
    continue
  fi
  $ctl disable ssf.service || { echo "ssf: could not disable ssf.service for $user" >&2; failed=1; }
  $ctl daemon-reload || { echo "ssf: could not reload the user manager for $user" >&2; failed=1; }
done
exit "$failed"
