#!/usr/bin/env bash
# Install Simple Software Factory (ssf) on Omarchy.
#
#   bash <(curl -fsSL https://raw.githubusercontent.com/mikekelly/simple-software-factory/master/install.sh)
#   ./install.sh                    # from a checkout: builds and installs this tree
#
# What it does, in order, skipping what is already done:
#   1. refuses on anything that is not Arch-based (pacman + makepkg);
#   2. clones the repository, or fast-forwards an existing clone, unless run
#      from inside a checkout, which is then what gets built;
#   3. builds and installs the package with `makepkg -si` (the one step that
#      asks for sudo: pacman installs the build dependencies and the package),
#      or with --dev builds ./target/release/ssf and points ssf.service at it
#      through a systemd drop-in;
#   4. makes sure ssf.service is running in this session;
#   5. installs the ssf-setup skill for your coding agent (`npx skills add`),
#      so the agent can drive the rest of the setup;
#   6. runs `ssf doctor` and prints the next step.
#
# Re-running it upgrades: pull, rebuild, reinstall, restart. Nothing here
# touches ~/.config/ssf or the running factory's state.
set -euo pipefail

REPO_URL="${SSF_REPO_URL:-https://github.com/mikekelly/simple-software-factory}"
SRC="${SSF_SRC:-}"
REF="${SSF_REF:-}"
MODE=package
SKILL=1
AGENTS="${SSF_SKILL_AGENTS:-}"
DEPS=0
DRY_RUN=0
NOCHECK=0

usage() {
  cat <<'USAGE'
usage: install.sh [options]

  --dev            build ./target/release/ssf and run the service from it
                   (a drop-in under ~/.config/systemd/user/ssf.service.d/)
                   instead of installing the package build; the package is
                   still installed once for the unit and the bar widget
  --src DIR        where to clone or update the repository
                   (default ~/.local/src/simple-software-factory, or the
                   checkout this script is run from; SSF_SRC)
  --ref REF        branch or tag to check out after cloning (SSF_REF)
  --deps           also install what the default setup needs beyond the
                   package's own dependencies (herdr, github-cli, and the
                   tools `ssf vm build` uses) with `sudo pacman -S --needed`;
                   without it they are checked and named
  --nocheck        skip the package's test run (makepkg --nocheck)
  --no-skill       do not install the ssf-setup skill
  --agent LIST     which agents `npx skills add` installs the skill for:
                   comma-separated here (claude-code,codex), passed to the
                   skills CLI as one -a per agent (SSF_SKILL_AGENTS; default:
                   let the skills CLI detect them)
  --dry-run        print what would run and change nothing
  -h, --help       this
USAGE
}

while [ $# -gt 0 ]; do
  case "$1" in
    --dev) MODE=dev ;;
    --src) SRC="$2"; shift ;;
    --src=*) SRC="${1#*=}" ;;
    --ref) REF="$2"; shift ;;
    --ref=*) REF="${1#*=}" ;;
    --deps) DEPS=1 ;;
    --nocheck) NOCHECK=1 ;;
    --no-skill) SKILL=0 ;;
    --agent) AGENTS="$2"; shift ;;
    --agent=*) AGENTS="${1#*=}" ;;
    --dry-run|-n) DRY_RUN=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "install.sh: unknown option $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

say()  { printf '\033[1m==> %s\033[0m\n' "$*"; }
note() { printf '    %s\n' "$*"; }
die()  { printf 'install.sh: %s\n' "$*" >&2; exit 1; }
# Print a command, then run it unless --dry-run.
run() {
  printf '    $ %s\n' "$*"
  [ "$DRY_RUN" = 1 ] || "$@"
}
# The same, in a directory that may not exist yet on a dry run.
run_in() {
  local dir="$1"; shift
  printf '    $ (cd %s && %s)\n' "$dir" "$*"
  [ "$DRY_RUN" = 1 ] || (cd "$dir" && "$@")
}

# 1. Arch only: the package is a PKGBUILD and the service is a systemd user unit.
if [ -r /etc/os-release ]; then
  # shellcheck disable=SC1091
  . /etc/os-release
fi
case " ${ID:-} ${ID_LIKE:-} " in
  *" arch "*|*" omarchy "*) ;;
  *) die "this installs an Arch package with makepkg; this host is '${PRETTY_NAME:-${ID:-unknown}}', not Arch-based. ssf runs on Omarchy (Arch)." ;;
esac
command -v pacman >/dev/null || die "pacman is not on PATH; ssf is installed as an Arch package"
command -v makepkg >/dev/null || die "makepkg is not on PATH; install base-devel (sudo pacman -S --needed base-devel)"
[ "$(id -u)" != 0 ] || die "run this as your own user, not root: makepkg refuses root and the service is a per-user unit"
[ "${ID:-}" = omarchy ] || note "not Omarchy (${PRETTY_NAME:-$ID}): the daemon and CLI work, the bar widget and menu entries need Omarchy"

# 2. The source tree: --src, else the checkout this script is in, else a
# clone under ~/.local/src.
here=""
if [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ]; then
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  [ -f "$here/packaging/PKGBUILD" ] && [ -f "$here/Cargo.toml" ] || here=""
fi
if [ -z "$SRC" ] && [ -n "$here" ]; then
  SRC="$here"
  say "Building the checkout this script is in: $SRC"
  if [ -n "$(git -C "$SRC" status --porcelain 2>/dev/null)" ]; then
    note "the working tree has local changes; they are what gets built"
  fi
  [ -z "$REF" ] || note "--ref $REF ignored: this checkout is built as it is"
elif [ -d "${SRC:=$HOME/.local/src/simple-software-factory}/.git" ]; then
  say "Updating $SRC"
  if [ -n "$(git -C "$SRC" status --porcelain)" ]; then
    note "local changes in $SRC; not pulling, building the tree as it is"
  elif [ -n "$REF" ]; then
    run git -C "$SRC" fetch --quiet origin
    run git -C "$SRC" checkout --quiet "$REF"
    run git -C "$SRC" merge --ff-only --quiet "origin/$REF" 2>/dev/null || true
  else
    run git -C "$SRC" pull --ff-only --quiet
  fi
else
  say "Cloning $REPO_URL into $SRC"
  run mkdir -p "$(dirname "$SRC")"
  if [ -n "$REF" ]; then
    run git clone --quiet --branch "$REF" "$REPO_URL" "$SRC"
  else
    run git clone --quiet "$REPO_URL" "$SRC"
  fi
fi
[ "$DRY_RUN" = 1 ] || [ -f "$SRC/packaging/PKGBUILD" ] || die "$SRC has no packaging/PKGBUILD; not an ssf checkout"

# 3. What the default setup (the microVM with herdr) needs beyond the package.
# `makepkg -s` installs depends and makedepends (cargo, jq, ...) itself.
# herdr comes from the Omarchy repository, so elsewhere it is named, not installed.
missing=()
for p in herdr github-cli fakeroot libarchive e2fsprogs openssh curl; do
  pacman -Qq "$p" >/dev/null 2>&1 || missing+=("$p")
done
if [ ${#missing[@]} -gt 0 ]; then
  if [ "$DEPS" = 1 ]; then
    if [ "${ID:-}" != omarchy ] && [ "${missing[0]}" = herdr ]; then
      note "herdr is in the Omarchy package repository (pkgs.omarchy.org); install it by hand on plain Arch"
      missing=("${missing[@]:1}")
    fi
    if [ ${#missing[@]} -gt 0 ]; then
      say "Installing what the default setup needs: ${missing[*]}"
      run sudo pacman -S --needed --noconfirm "${missing[@]}"
    fi
  else
    say "Not installed (needed for the default setup; --deps installs them): ${missing[*]}"
    note "sudo pacman -S --needed ${missing[*]}"
  fi
fi
if [ ! -w /dev/kvm ]; then
  note "/dev/kvm is not writable by you: the microVM (ssf vm build) needs it; the factory runs on the host without it"
fi

# 4. Build and install.
if [ "$MODE" = package ] || ! pacman -Qq ssf >/dev/null 2>&1; then
  say "Building and installing the package (makepkg -si; pacman asks for your sudo password)"
  args=(-si --noconfirm)
  [ "$NOCHECK" = 1 ] && args+=(--nocheck)
  # PKGBUILD builds the working tree it lives in; -f rebuilds when the
  # version did not change (a dirty tree, a re-run at the same commit), and
  # pacman -U without --needed reinstalls it, so a re-run installs what is
  # checked out now.
  run_in "$SRC/packaging" makepkg -f "${args[@]}"
fi

dropin="$HOME/.config/systemd/user/ssf.service.d/dev-build.conf"
if [ "$MODE" = dev ]; then
  say "Building the dev binary"
  run_in "$SRC" cargo build --release
  bin="$SRC/target/release/ssf"
  say "Pointing ssf.service at $bin"
  note "drop-in: $dropin"
  if [ "$DRY_RUN" != 1 ]; then
    mkdir -p "$(dirname "$dropin")"
    cat >"$dropin" <<EOF
# Written by install.sh --dev: run the service from a dev build rather than
# the package's /usr/bin/ssf. Remove this file (and daemon-reload) to go
# back to the package.
[Service]
ExecStartPre=
ExecStartPre=-$bin ui install --quiet
ExecStart=
ExecStart=$bin run
EOF
  fi
  run systemctl --user daemon-reload
  run systemctl --user restart ssf.service
  SSF="$bin"
else
  if [ -f "$dropin" ]; then
    note "a dev-build drop-in exists at $dropin; the service runs that build, not the package (remove it and daemon-reload to switch)"
  fi
  SSF=/usr/bin/ssf
fi

# 5. The service (the package's install hook starts it in a running
# session; this covers a session it could not reach, and --dev restarts).
say "Service"
if [ "$DRY_RUN" = 1 ]; then
  note "systemctl --user daemon-reload; systemctl --user start ssf.service (if not active)"
elif ! systemctl --user is-active --quiet ssf.service; then
  systemctl --user daemon-reload
  if [ -e "$HOME/.local/state/ssf/disabled" ]; then
    note "ssf.service is switched off (~/.local/state/ssf/disabled); \`ssf ui service enable\` turns it on"
  elif ! systemctl --user start ssf.service 2>/dev/null; then
    note "could not start ssf.service now (no graphical session?); it starts with the next login"
  fi
fi
[ "$DRY_RUN" = 1 ] || systemctl --user --no-pager status ssf.service 2>/dev/null | sed -n '1,4p' | sed 's/^/    /' || true

# 6. The setup skill: the runbook a coding agent follows from here.
if [ "$SKILL" = 1 ]; then
  if command -v npx >/dev/null; then
    say "Installing the ssf-setup skill for your coding agent"
    skill_args=(add "$SRC" --skill ssf-setup -g -y)
    # The skills CLI takes one -a per agent.
    IFS=, read -r -a agent_list <<<"$AGENTS"
    for a in "${agent_list[@]}"; do
      [ -n "$a" ] && skill_args+=(-a "$a")
    done
    if ! run npx -y skills "${skill_args[@]}"; then
      note "skill install failed; run it yourself: npx skills add $SRC --skill ssf-setup -g"
    fi
  else
    note "npx is not on PATH (pacman -S nodejs npm), so the skill was not installed; later: npx skills add $REPO_URL --skill ssf-setup -g"
  fi
fi

# 7. Where things stand, and the next step.
say "ssf doctor"
if [ "$DRY_RUN" = 1 ]; then
  note "$SSF doctor"
else
  "$SSF" doctor 2>&1 | sed 's/^/    /' || true
fi

cat <<EOF

==> Installed. Next:
    1. Sign in the bot account (a GitHub account of its own, not yours):
         ssf auth login --web        # in a private browser window, as the bot
    2. Decide where the agents run. The default is a microVM with herdr:
         ssf vm build && ssf config set vm.enabled true && systemctl --user restart ssf.service
         ssf vm login claude         # sign your harness in inside the guest
       or stay on this machine (herdr running, or Orca with \`ssf config set driver orca\`)
       and sign the harness in here.
    3. Watch a repository the bot has push access to:
         ssf repo add owner/name --harness claude
    4. Put an SSF.md at the repository root and assign an issue to the bot.
    A coding agent with the ssf-setup skill walks you through all of it
    (\`/ssf-setup\` in Claude Code); docs: /usr/share/doc/ssf/README.md
EOF
