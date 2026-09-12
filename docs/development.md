# Development

Building ssf from source, running a scratch factory, running a dev build as the service, and where things are in the tree. For whoever works on ssf itself.

```sh
cargo build && cargo test
cargo fmt && cargo clippy
export SSF_CONFIG_DIR=/tmp/ssf-dev SSF_STATE_DIR=/tmp/ssf-dev SSF_GITHUB_TOKEN=$(gh auth token)
./target/debug/ssf config set driver herdr            # or orca; a fresh config names none
./target/debug/ssf config set herdr.projects_dir /tmp/ssf-dev/projects   # or orca.projects_dir
./target/debug/ssf repo add you/sandbox --harness claude   # a repository the real factory does not watch
./target/debug/ssf-server --once  # one pass; agents launched by this run read the same SSF_* locations
unset SSF_CONFIG_DIR SSF_STATE_DIR SSF_GITHUB_TOKEN   # in a guest shell, restore SSF_STATE_DIR=/var/lib/ssf/state
SSF_PLUGIN_DIR=$PWD/omarchy-plugin ./target/debug/ssf ui install   # on Omarchy: writes the REAL ~/.config/omarchy
omarchy plugin validate ./omarchy-plugin
cd packaging && makepkg -fd          # rebuild the package; commit the pkgver bump it makes to PKGBUILD
```

When reporting verification, a count must name what it is measured against:
record the branch and base commits, and use the same command and Rust toolchain
for both. Test totals need that context too; report the change from the base,
not just an absolute total. Recheck the comparison after a rebase.

To count unique Clippy warning locations across all targets (requires `jq`):

```sh
cargo clippy --all-targets --message-format=json 2>/dev/null \
  | jq -r 'select(.reason=="compiler-message") | .message
           | select(.level=="warning") | select(.code!=null)
           | "\(.spans[0].file_name):\(.spans[0].line_start):\(.spans[0].column_start)"' \
  | sort -u | wc -l
```

The lint-code filter excludes summaries, and `sort -u` collapses duplicate
locations reported by multiple targets. Counting `^warning:` lines also counts
per-target summaries; adding the summary totals double-counts shared warnings,
and using only the last summary can miss warnings exclusive to another target.
Run Clippy normally first and check it succeeds: this counting pipeline hides
stderr and is not a build-success check.

The count is a baseline, not a target: what matters is that the change adds no
warnings. For example, report “Clippy: 21 unique warning locations on branch
`<sha>` and on base `c8a4d05`, using the command above and Rust `<version>`;
no new warnings” only after comparing the diagnostics too. Equal counts alone
can hide one warning replacing another. If reporting attribution instead,
identify the base commit and verify that each warning comes from code already
on that base; a bare “Clippy: 21” does not establish that claim.

**`SSF_CONFIG_DIR` and `SSF_STATE_DIR` move ssf's own files and nothing
else.** Those are the config file, the state file, the socket its commands
talk to, the bot token, the SSH keys, the `bin` shim directory an agent
gets on its `PATH`, the `gh` configuration those agents use, and the marker
that keeps a disabled service off.

Everything else depends on your machine and your configuration rather than
on those variables: which driver and server the pass uses, where it clones
and whether that is a checkout something else is already working, which
account its token belongs to (run `gh auth status` before the recipe —
`SSF_GITHUB_TOKEN` wins over everything, and in your own shell it is you),
whether commands are forwarded to a microVM. Some of it no setting reaches
at all: `ssf ui install` writes the real `~/.config/omarchy`, undone by
`ssf ui uninstall && ssf ui install` from the packaged binary; `ssf ui
service` acts on the real unit; and agents use the harness's own sessions.

Assume a scratch factory shares all of it with the real one unless you have
arranged otherwise. A pass that finds an item for the bot — the startup
pass included — starts a real agent somewhere, and `ssf release` and
`ssf purge` will not be the way you clean it up.

`ssf-server --once` does a single pass, startup pass included, and exits. It
refuses while another engine owns its state directory, so wait for that
run to finish before starting a pass.

The installed service runs the last package installed, so a change is
verified with unit tests and scratch runs rather than by expecting to see
it live.

## Tests write nowhere but a temporary directory

A test must not write outside a temporary directory it made itself:
`cargo test` runs on machines with a live factory, and it used to
overwrite `~/.local/state/ssf/state.json` with a fixture. Nothing was
lost while the daemon kept running, but a restart in that window started
it from the fixture and every repository binding and every session's
workspace went with it. `makepkg`'s `check()` runs the suite too, so a
source build did it to whoever built the package.

So the test build reaches no real directory by accident: `config_dir()`
and `state_dir()` ignore `SSF_CONFIG_DIR`/`SSF_STATE_DIR` and the
platform's own answer under `cfg(test)`, and panic unless the calling
thread holds a guard saying which directories it means. A test that
writes takes the sandbox:

```rust
let _sandbox = crate::config::test_support::sandbox();
```

which points both directories at a fresh temporary directory for that
thread and deletes it when the guard is dropped. Tests run one per
thread, so two running in parallel cannot see each other's `state.json`.
`sandbox.config_dir()`, `sandbox.state_dir()`, `sandbox.home()` and
`sandbox.root()` are the paths, for a test that wants to lay a fixture
down first. `ui::home()` is guarded the same way and answers
`sandbox.home()`, since the Omarchy widget's install and uninstall write
and delete under `~/.config/omarchy`, which is nobody's temporary
directory either.

`cfg(test)` is what makes any of this hold, and that in turn rests on
`ssf` having no `[lib]` target: the tests are all inline, so they are the
only thing that compiles `config.rs`. `tests/packaging.rs` cannot reach
`crate::config` at all for the same reason. Give the crate a library and
anything under `tests/` links it built *without* `cfg(test)`, with the
real directories back — so a `[lib]` target comes with moving the guard
somewhere it does not depend on how the file was compiled.

The guard is the calling thread's, and nothing carries it: resolve a
directory on a thread the test handed work to (`spawn_blocking`, a
multi-threaded runtime) and it panics there instead. That is not always
loud — `Engine::probe_harness` turns a panicked `spawn_blocking` into an
`Unknown` login probe and carries on — so keep the resolution on the
test's own thread rather than relying on the panic to find it for you.

Everything else a test writes goes under `std::env::temp_dir()`, in a
directory named after its module and the process (`ssf-state-<pid>`,
`ssf-engine-events-<pid>`); most remove it at the end, some do not.

The `#[ignore]`d live tests are the exception to all of this and are
meant to be: they are run by hand, against this machine. `vm_live` boots
a VM under `~/.local/share/ssf/vm` and may migrate legacy factory settings
and credentials from the real config directory on first adoption, so it
holds `test_support::the_machine_itself()` instead of a
sandbox: the same stack, pointing the three directories at the machine's
own, creating and deleting nothing. `herdr_live` starts a real harness
and leaves session files under `~/.claude`, and
`herdr_live_first_prompt` a `trust_level` entry in `~/.codex/config.toml`.
That guard is the only way to a real directory from the test build, and
nothing `cargo test` runs on its own may hold one.

## Firecracker ownership upgrade regression

`firecracker_ownership_boot_persistence` is an ignored, isolated test for a
Linux host with KVM. It boots a new VM on a temporary data disk, changes guest
configuration, verifies the boot script and state after restart, refuses a
legacy root, then verifies state again after resetting to the rebuilt root.
It uses no host factory credentials. Supply absolute paths to a current image
built with this checkout's `vm/` scripts, an unmodified legacy image, the kernel,
and executables:

```sh
cargo build
SSF_VM_TEST_ROOTFS=/path/to/current/rootfs.ext4 \
SSF_VM_TEST_LEGACY_ROOTFS=/path/to/v0.2/rootfs.ext4 \
SSF_VM_TEST_KERNEL=/path/to/vmlinux \
SSF_VM_TEST_FIRECRACKER=/path/to/firecracker \
SSF_VM_TEST_GVPROXY=/path/to/gvproxy \
SSF_VM_TEST_BINARY="$PWD/target/debug/ssf" \
env -u SSF_VM_GUEST cargo test firecracker_ownership_boot_persistence -- --ignored --nocapture
```

The test copies source images; it does not alter them. It stops its VM before
removing temporary disks, and retains those disks if stopping fails. The normal
suite also reproduces a committed ext4 journal transaction that restores legacy
script content despite a successful pre-recovery readback; that regression needs
`mkfs.ext4`, `debugfs` and `e2fsck`, but no KVM or mount privileges.

## A dev build as the service

`packaging/dev-install.sh` builds `target/release/ssf` and `ssf-server`, writes the
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
ExecStart=/home/you/src/simple-software-factory/target/release/ssf-server
```

Run `./target/release/ssf doctor` rather than the packaged `ssf doctor`:
`doctor` says whether the `ssf` on PATH is the binary running it, so only
the dev build's own `doctor` shows the two differ (agents run the daemon's
binary either way, through the links `ssf launch` makes). A dev build
started by hand (`ssf-server`) resumes interrupted sessions on start like the
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
   commit, tag `vX.Y.Z` and push the tag. The tag must be plain `vX.Y.Z`
   (no `-rc1` or the like: the workflow only runs on those, nfpm would
   write `0.2.0~rc1` and makepkg refuses a hyphen in `pkgver`) and its
   X.Y.Z must equal `Cargo.toml`'s `version`, or `build.sh` stops. The tag runs
   `.github/workflows/release.yml`, which makes the GitHub release if
   there is none and attaches `ssf_X.Y.Z-1_amd64.deb`,
   `ssf-X.Y.Z-1.x86_64.rpm` and the bare static binary
   `ssf-X.Y.Z-linux-x86_64` (a musl build via `packaging/linux/build.sh`
   and nfpm), `ssf-X.Y.Z-1-x86_64.pkg.tar.zst` (from
   `packaging/release/PKGBUILD` in an Arch container, the PKGBUILD
   Omarchy's repository builds) and, best effort, `ssf-X.Y.Z-linux-aarch64`.
   x86_64 only, like the microVM image. A run started by hand
   (`workflow_dispatch`) builds the same from the working tree and leaves
   workflow artifacts, no release. Locally, `packaging/linux/build.sh`
   builds the .deb, .rpm and the bare binary into `packaging/linux/dist/`
   (it needs `nfpm` and the musl target, and says so).
   The same tag runs `.github/workflows/homebrew.yml`, which renders
   `packaging/homebrew/ssf.rb` (the formula's source of truth; the
   `url` and `sha256` of the tag tarball go in, see
   `packaging/homebrew/render.sh`) and pushes it to the tap
   `mikekelly/homebrew-tap` as `Formula/ssf.rb` when the
   `HOMEBREW_TAP_TOKEN` secret is set. Without it, the workflow uploads the
   rendered formula as the run's `ssf.rb` artifact and then fails
   (`packaging/homebrew/README.md` has the tap setup and the formula
   test). The lima backend downloads the release's bare binaries,
   `ssf-X.Y.Z-linux-x86_64` and `ssf-X.Y.Z-linux-aarch64`, as the guest
   binary on a Mac, so they must be attached with exactly those names:
   `release.yml` does that, and when its best-effort aarch64 job failed,
   `gh release upload vX.Y.Z ssf-X.Y.Z-linux-aarch64` adds the missing
   one by hand.
2. In `packaging/release/`: `pkgver=X.Y.Z`, `pkgrel=1`, `updpkgsums`
   (downloads the tag tarball and writes its sha256; it needs the
   repository to be public, or the tarball fetched with a token into
   that directory first), `makepkg -fd` to check it builds from the
   tarball, commit. The workflow attaches the same package to the
   release, so nothing is uploaded by hand. A development build
   (`0.1.0.r271.g06491ae`) sorts *above* the release version (`0.1.0`)
   for pacman, so a machine installed from one would not be upgraded by
   the package from Omarchy's repository until the next tag.
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
(`ssf-X.Y.Z-1-x86_64.pkg.tar.zst`, built by the workflow) and
[Setup](setup.md) says "from the latest release"; the .deb and .rpm come
from the release either way.

## Layout

| Path | What |
|------|------|
| `src/main.rs` | the CLI: every subcommand, `doctor`, VM forwarding |
| `src/engine.rs` | the polling loop: listings, onboarding, delivery, the startup pass, retirement |
| `src/engine/requests.rs` | daemon-side handling of CLI requests over the IPC socket |
| `src/github.rs` | REST and GraphQL client (listings, timelines, boards, collaborators) |
| `src/prompt.rs` | timeline rendering, prompt templates and `ssf guide` |
| `src/config.rs`, `src/state.rs` | `config.toml` and `state.json` |
| `src/driver.rs`, `src/orca.rs`, `src/herdr.rs` | the driver interface and the two drivers |
| `src/vm.rs`, `src/vm/lima.rs`, `vm/` | `ssf vm`: the Firecracker backend and what both backends share, the lima backend, and the guest scripts and units |
| `src/platform.rs` | what differs per host OS: the systemd user unit on Linux, the Homebrew launchd service on macOS |
| `src/sessions.rs` | agent session capture and resume |
| `src/origin.rs`, `src/shim.rs` | bylines and origin tags; the `gh` wrapper |
| `src/allow.rs` | the allow-list of GitHub users |
| `src/release.rs` | the release and purge checks |
| `src/ipc.rs` | the CLI-to-daemon socket behind `sub`, `unsub`, `tell`, `handover`, `release` and `purge` |
| `src/status.rs` | the joined item/session view behind `status`, `peers` and the widget |
| `src/agents.rs`, `src/models.rs` | Omarchy's agent catalogue; model, effort and permission-free commands per harness |
| `src/keys.rs`, `src/ghcli.rs` | SSH key enrollment; the GitHub CLI's keyring |
| `src/ui.rs`, `omarchy-plugin/`, `bin/ssf-ui` | Omarchy integration: the Quickshell bar widget (a dashboard of the factory's state), the menu entries, and the helper behind both (service toggle, log, status terminal, open a workspace) |
| `packaging/` | the development PKGBUILD, the Omarchy systemd unit, pacman install script, `dev-install.sh` (the service on a dev build); `release/` is the release PKGBUILD and Omarchy metadata, the directory that goes into omarchy-pkgs; `linux/` is the .deb and .rpm: `nfpm.yaml`, `build.sh`, the `default.target` unit and the post-install and post-remove hooks; `homebrew/` is the macOS formula, its render script and the tap notes |
| `.github/workflows/release.yml` | the release workflow: on a `vX.Y.Z` tag, builds the .deb, .rpm, .pkg.tar.zst and bare binaries and attaches them to the GitHub release |
| `.github/workflows/homebrew.yml` | the tap workflow: when the release is published, renders the Homebrew formula and pushes it to `mikekelly/homebrew-tap` |
| `skills/ssf-setup/` | the `ssf-setup` agent skill: a pointer at `docs/setup.md` plus the rules for an agent following it |
| `docs/` | `setup.md` (the setup document) and the reference behind the README, installed under `/usr/share/doc/ssf/` |

This repository is built by ssf itself: [`SSF.md`](../SSF.md) is what its
agents are told.
