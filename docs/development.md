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

`./install.sh --dev` does this: it installs the package once if it is
not (for the unit and the widget), builds `target/release/ssf`, writes
the drop-in below pointing the unit at the build, then `systemctl --user
daemon-reload && systemctl --user restart ssf.service`. Keep the build
outside any worktree an agent might release, and remove the drop-in when
the package is reinstalled from master (a plain `./install.sh` says when
one is there):

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
| `src/ui.rs`, `omarchy-plugin/`, `bin/ssf-ui` | Omarchy integration: the Quickshell bar widget and the menu flows |
| `install.sh` | the install script: clone or update, `makepkg -si` (or `--dev`), the service, the skill, `ssf doctor` |
| `packaging/` | PKGBUILD, systemd unit, pacman install script |
| `skills/ssf-setup/` | the `ssf-setup` agent skill, installed with `npx skills add` |
| `docs/` | the reference behind the README, installed under `/usr/share/doc/ssf/` |

This repository is built by ssf itself: [`SSF.md`](../SSF.md) is what its
agents are told.
