# Development

Building ssf from source, running a scratch factory, running a dev build as the service, and where things are in the tree. For whoever works on ssf itself.

```sh
cargo build && cargo test
cargo fmt && cargo clippy
SSF_CONFIG_DIR=/tmp/ssf-dev SSF_STATE_DIR=/tmp/ssf-dev SSF_GITHUB_TOKEN=$(gh auth token) \
  ./target/debug/ssf repo add you/sandbox --harness claude
SSF_CONFIG_DIR=/tmp/ssf-dev SSF_STATE_DIR=/tmp/ssf-dev SSF_GITHUB_TOKEN=$(gh auth token) \
  ./target/debug/ssf run --once     # one pass; agents launched by this run read the same SSF_* locations
SSF_PLUGIN_DIR=$PWD/omarchy-plugin ./target/debug/ssf ui install   # live-test the widget
omarchy plugin validate ./omarchy-plugin
cd packaging && makepkg -fd          # rebuild the package; commit the pkgver bump it makes to PKGBUILD
```

A scratch factory with its own `SSF_CONFIG_DIR`/`SSF_STATE_DIR` touches
nothing of the real one; `ssf run --once` does a single pass, startup
pass included, and exits. The installed service runs the last package
installed, so a change is verified with unit tests and scratch runs rather
than by expecting to see it live.

## A dev build as the service

`packaging/dev-install.sh` builds `target/release/ssf`, writes the
drop-in below pointing the unit at the build, then `systemctl --user
daemon-reload && systemctl --user restart ssf.service`, and runs the
build's `doctor`. The package has to be installed once for the unit and
the widget (`cd packaging && makepkg -si`); the script stops and says so
otherwise. Keep the build outside any worktree an agent might release.
The drop-in survives package upgrades, so the service keeps running the
dev build until `packaging/dev-install.sh --undo` removes it and restarts
the service on the package:

```ini
# ~/.config/systemd/user/ssf.service.d/dev-build.conf
[Service]
ExecStartPre=
ExecStartPre=-/home/you/src/simple-software-factory/target/release/ssf ui install --quiet
ExecStart=
ExecStart=/home/you/src/simple-software-factory/target/release/ssf run
```

Run `./target/release/ssf doctor` rather than the packaged `ssf doctor`:
`doctor` says whether the `ssf` on PATH is the binary running it, so only
the dev build's own `doctor` shows the two differ (agents run the daemon's
binary either way, through the links `ssf launch` makes). A dev build
started by hand (`ssf run`) resumes interrupted sessions on start like the
service does, but nothing restarts it for you.

## Releasing

The package a newcomer installs comes from a GitHub release: `cd
packaging && makepkg -f` builds `ssf-<pkgver>-1-x86_64.pkg.tar.zst` from
the working tree, and that file is what the release carries (the `pkgver`
is `<Cargo version>.r<commits>.g<short sha>`, so it sorts after any
earlier build). Landing the package in Omarchy's repository is a later
step and needs a tagged release with the PKGBUILD's `source` pointing at
it; until then [Setup](setup.md) says "from the latest release".

## Layout

| Path | What |
|------|------|
| `src/main.rs` | the CLI: every subcommand, `doctor`, VM forwarding |
| `src/engine.rs` | the polling loop: listings, onboarding, delivery, the startup pass, retirement |
| `src/github.rs` | REST and GraphQL client (listings, timelines, boards, collaborators) |
| `src/prompt.rs` | timeline rendering, prompt templates and `ssf guide` |
| `src/config.rs`, `src/state.rs` | `config.toml` and `state.json` |
| `src/driver.rs`, `src/orca.rs`, `src/herdr.rs` | the driver interface and the two drivers |
| `src/vm.rs`, `vm/` | `ssf vm` and the guest image scripts and units |
| `src/sessions.rs` | agent session capture and resume |
| `src/origin.rs`, `src/shim.rs` | bylines and origin tags; the `gh` wrapper |
| `src/allow.rs` | the allow-list of GitHub users |
| `src/release.rs` | the release and purge checks |
| `src/ipc.rs` | the CLI-to-daemon socket behind `sub`, `unsub`, `tell`, `release` and `purge` |
| `src/status.rs` | the joined item/session view behind `status`, `peers` and the widget |
| `src/agents.rs`, `src/models.rs` | Omarchy's agent catalogue; model, effort and permission-free commands per harness |
| `src/keys.rs`, `src/ghcli.rs` | SSH key enrollment; the GitHub CLI's keyring |
| `src/ui.rs`, `omarchy-plugin/`, `bin/ssf-ui` | Omarchy integration: the Quickshell bar widget (a dashboard of the factory's state), the menu entries, and the helper behind both (service toggle, log, status terminal, open a workspace) |
| `packaging/` | PKGBUILD, systemd unit, pacman install script, `dev-install.sh` (the service on a dev build) |
| `skills/ssf-setup/` | the `ssf-setup` agent skill: a pointer at `docs/setup.md` plus the rules for an agent following it |
| `docs/` | `setup.md` (the setup document) and the reference behind the README, installed under `/usr/share/doc/ssf/` |

This repository is built by ssf itself: [`SSF.md`](../SSF.md) is what its
agents are told.
