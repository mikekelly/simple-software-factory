#!/bin/sh
set -u
failed=0
listing=$(loginctl list-users --no-legend 2>/dev/null) || {
  echo "ssf: refusing package removal: could not list user managers" >&2
  exit 1
}
users=$(printf '%s\n' "$listing" | awk '{print $2}') || exit 1
for user in $users; do
  ctl="systemctl --machine=${user}@.host --user"
  stop_vms=
  manager=$($ctl is-system-running 2>/dev/null); manager_status=$?
  case "$manager" in
    offline) continue ;;
    running|degraded|starting|stopping) ;;
    *) echo "ssf: refusing package removal: cannot inspect the user manager for $user (status $manager_status: ${manager:-no state})" >&2; failed=1; continue ;;
  esac
  target_files=$($ctl list-unit-files --no-legend --plain 'ssf@*.service' 2>/dev/null) || {
    echo "ssf: refusing package removal: cannot list target services for $user" >&2; failed=1; continue;
  }
  target_units=$($ctl list-units --all --no-legend --plain 'ssf@*.service' 2>/dev/null) || {
    echo "ssf: refusing package removal: cannot list running target services for $user" >&2; failed=1; continue;
  }
  targets=$(printf '%s\n%s\n' "$target_files" "$target_units" | awk '$1 ~ /^ssf@[^[:space:]]+\.service$/ {print $1}' | sort -u)
  for unit in ssf.service $targets; do
    fragment=$($ctl show -p FragmentPath --value "$unit" 2>/dev/null) || {
      echo "ssf: refusing package removal: cannot determine $unit ownership for $user" >&2; failed=1; continue;
    }
    [ -n "$fragment" ] || continue
    case "$unit:$fragment" in
      ssf.service:/usr/lib/systemd/user/ssf.service|ssf@*.service:/usr/lib/systemd/user/ssf@.service) ;;
      *) continue ;;
    esac
    exec_start=$($ctl show -p ExecStart --value "$unit" 2>/dev/null) || {
      echo "ssf: refusing package removal: cannot inspect package-owned $unit for $user" >&2; failed=1; continue;
    }
    path_count=$(printf '%s' "$exec_start" | grep -o 'path=' | wc -l)
    target=${unit#ssf@}; target=${target%.service}
    if [ "$unit" = ssf.service ]; then expected='argv[]=/usr/bin/ssf-server ;'
    else expected="argv[]=/usr/bin/ssf-server --target $target ;"
    fi
    if [ "$path_count" -ne 1 ] ||
       ! printf '%s' "$exec_start" | grep -F 'path=/usr/bin/ssf-server ;' >/dev/null ||
       ! printf '%s' "$exec_start" | grep -F "$expected" >/dev/null; then
      echo "ssf: refusing package removal: package-owned $unit has an unrelated effective ExecStart for $user" >&2
      failed=1
      continue
    fi
    if ! $ctl stop "$unit" || $ctl is-active --quiet "$unit"; then
      echo "ssf: refusing package removal: could not stop package-owned $unit for $user" >&2
      failed=1
      continue
    fi
    stop_vms=1
    $ctl disable "$unit" || { echo "ssf: could not disable $unit for $user" >&2; failed=1; }
  done
  # Stopping a service leaves its VM running (the VM has its own lifetime,
  # so an upgrade does not reboot it). Removal stops every VM ssf manages
  # for this user, cleanly and with ssf's own code, in the user manager the
  # services ran in: after a stopped service, or while a VM scope runs
  # (a service disabled by hand, or `ssf vm start` without one).
  scopes=$($ctl list-units --no-legend --plain 'ssf-vm-*.scope' 2>/dev/null) || scopes=
  if [ -n "$stop_vms" ] || [ -n "$scopes" ]; then
    if ! systemd-run --machine="${user}@.host" --user --wait --quiet --collect /usr/bin/ssf-server --stop-vm; then
      echo "ssf: refusing package removal: a VM of $user did not stop (the user journal has why); stop it with \`ssf --server NAME vm stop\`, then remove the package again" >&2
      failed=1
    fi
  fi
  $ctl daemon-reload || { echo "ssf: could not reload the user manager for $user" >&2; failed=1; }
done
exit "$failed"
