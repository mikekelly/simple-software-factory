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

Two PKGBUILDs share one `package()`:

- `packaging/PKGBUILD` is the development build: it tars the working tree,
  local changes included, and its `pkgver` is
  `<Cargo version>.r<commits>.g<short sha>`, so every build sorts after
  the one before. `cd packaging && makepkg -fd` is the local workflow, and
  the `pkgver` bump makepkg writes is committed with the change.
- `packaging/release/PKGBUILD` is the release build: `pkgver=X.Y.Z`,
  `source` is the GitHub tag tarball
  (`.../archive/refs/tags/vX.Y.Z.tar.gz`) with its sha256, and
  `cargo build --frozen --release` from that tarball. The development
  PKGBUILD sources it and overrides the version, the source and the
  `prepare()`/`build()`/`check()` that work on the copied tree, so
  `depends`, `optdepends`, `options` and `package()` live in the release
  file alone.

`packaging/release/` is laid out the way Omarchy's package repository
([omacom/omarchy-pkgs](https://github.com/omacom/omarchy-pkgs)) wants a
package directory: `PKGBUILD`, `ssf.install` (a symlink to
`../ssf.install`; the copy in step 3 below dereferences it) and `.omarchy/package.json`, which
tells its `sync-upstream` to follow this repository's `vX.Y.Z` tags and
puts ssf on the fast release ring, so a new tag reaches the stable channel
without waiting for an Omarchy release. Everything Omarchy's builder needs
is in that directory plus the tag tarball, which it downloads
unauthenticated: the repository has to be public for the build to work.

Cutting a release:

1. Bump `version` in `Cargo.toml`, `cargo build` (updates `Cargo.lock`),
   commit, tag `vX.Y.Z` and push the tag; make the GitHub release from it.
2. In `packaging/release/`: `pkgver=X.Y.Z`, `pkgrel=1`, `updpkgsums`
   (downloads the tag tarball and writes its sha256; it needs the
   repository to be public, or the tarball fetched with a token into
   that directory first), `makepkg -fd` to check it builds from the
   tarball, commit. Attach the `ssf-X.Y.Z-1-x86_64.pkg.tar.zst` it made
   to the GitHub release: a development build (`0.1.0.r271.g06491ae`)
   sorts *above* the release version (`0.1.0`) for pacman, so a machine
   installed from one would not be upgraded by the package from Omarchy's
   repository until the next tag.
3. Once ssf is in Omarchy's repository, Omarchy's `sync-upstream` does step
   2 on its side and opens the PR there; a change to `depends`,
   `package()` or `ssf.install` still needs a PR to omarchy-pkgs with the
   `packaging/release/` files:

   ```sh
   cp -L packaging/release/PKGBUILD packaging/release/ssf.install <omarchy-pkgs>/pkgbuilds/ssf/
   cp -r packaging/release/.omarchy <omarchy-pkgs>/pkgbuilds/ssf/
   ```

The first submission to omarchy-pkgs is a PR adding `pkgbuilds/ssf/` from
`packaging/release/` (issue #123 has the prepared branch and the command).
When it lands, `README.md` "Install" and `docs/setup.md` steps 2 and 11
and the checklist's first item switch from "download the package from the latest release" to
`sudo pacman -S ssf`, and this document's development-build note stays as
it is. Until then the release carries the package file
(`ssf-X.Y.Z-1-x86_64.pkg.tar.zst`, built with `makepkg -fd` in
`packaging/release/`) and [Setup](setup.md) says "from the latest release".

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
| `src/ipc.rs` | the CLI-to-daemon socket behind `sub`, `unsub`, `tell`, `handover`, `release` and `purge` |
| `src/status.rs` | the joined item/session view behind `status`, `peers` and the widget |
| `src/agents.rs`, `src/models.rs` | Omarchy's agent catalogue; model, effort and permission-free commands per harness |
| `src/keys.rs`, `src/ghcli.rs` | SSH key enrollment; the GitHub CLI's keyring |
| `src/ui.rs`, `omarchy-plugin/`, `bin/ssf-ui` | Omarchy integration: the Quickshell bar widget (a dashboard of the factory's state), the menu entries, and the helper behind both (service toggle, log, status terminal, open a workspace) |
| `packaging/` | the development PKGBUILD, systemd unit, pacman install script, `dev-install.sh` (the service on a dev build); `release/` is the release PKGBUILD and Omarchy metadata, the directory that goes into omarchy-pkgs |
| `skills/ssf-setup/` | the `ssf-setup` agent skill: a pointer at `docs/setup.md` plus the rules for an agent following it |
| `docs/` | `setup.md` (the setup document) and the reference behind the README, installed under `/usr/share/doc/ssf/` |

This repository is built by ssf itself: [`SSF.md`](../SSF.md) is what its
agents are told.
