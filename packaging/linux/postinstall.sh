#!/bin/sh
# Post-install script of the ssf .deb and .rpm (nfpm embeds it as postinst /
# %post). A port of post_install/post_upgrade in ../ssf.install.
#   deb: `configure [old-version]`   old-version present  => upgrade;
#        `abort-upgrade`, `abort-remove`, `abort-deconfigure` (dpkg rolling
#        back a failed operation; the old version stays) => upgrade
#   rpm: `1` install, `2` upgrade
# Nothing here may fail the package operation: every systemctl/loginctl call
# is tolerated and the script exits 0.
set -u

case "${1:-}" in
  configure) if [ -n "${2:-}" ]; then op=upgrade; else op=install; fi ;;
  abort-*) op=upgrade ;;
  2) op=upgrade ;;
  *) op=install ;;
esac

# Start (or restart) ssf.service in every user manager that is running right
# now. The package manager runs as root outside any session, so the unit's
# default.target.wants symlink only kicks in at the next login; this covers
# the session that is installing the package.
start_for_logged_in_users() {
  action="$1"
  started=0
  for user in $(loginctl list-users --no-legend 2>/dev/null | awk '{print $2}'); do
    if systemctl --machine="${user}@.host" --user is-system-running >/dev/null 2>&1 \
      || systemctl --machine="${user}@.host" --user is-system-running 2>/dev/null | grep -qE 'running|degraded'; then
      systemctl --machine="${user}@.host" --user daemon-reload >/dev/null 2>&1 || true
      if systemctl --machine="${user}@.host" --user "$action" ssf.service >/dev/null 2>&1; then
        echo "==> ssf.service ${action}ed for $user"
        started=1
      fi
    fi
  done
  [ "$started" -eq 1 ]
}

case "$op" in
  install)
    cat <<'MSG'
==> Simple Software Factory (ssf) is installed.
    It runs as a per-user service (ssf.service) that starts with your login session.
    On a machine you do not log in to, `loginctl enable-linger $USER` keeps it running.
    The agents run in herdr, which this package does NOT install (see
    /usr/share/doc/ssf/docs/setup.md, step 1; by default inside a microVM with `ssf vm`),
    or in Orca (installed by hand; `ssf config set driver orca`).
MSG
    if ! start_for_logged_in_users start; then
      echo "    No running user session found; it starts at next login, or now with:"
      echo "      systemctl --user daemon-reload && systemctl --user start ssf.service"
    fi
    cat <<'MSG'
    Next: follow /usr/share/doc/ssf/docs/setup.md, yourself or with your coding agent:
    the bot account (`ssf auth login --web`), the microVM (`ssf vm build && ssf config set
    vm.enabled true`) or this machine, then a repository (`ssf repo add owner/name --harness claude`).
MSG
    ;;
  upgrade)
    if ! start_for_logged_in_users try-restart; then
      echo "==> ssf upgraded; restart it with: systemctl --user restart ssf.service"
    fi
    ;;
esac
exit 0
