# Homebrew packaging

`ssf.rb` is the Homebrew formula for ssf and the only copy that is edited.
It is written for the version in `Cargo.toml` with a placeholder sha256;
when a `vX.Y.Z` GitHub release is published, `.github/workflows/homebrew.yml` runs `render.sh`
to put the tag tarball's url and sha256 in (the source of truth keeps the
placeholder) and pushes the result to the tap repository
[mikekelly/homebrew-tap](https://github.com/mikekelly/homebrew-tap) as
`Formula/ssf.rb`, which is what `brew install` reads.

The formula is for macOS, and says so to Homebrew with `depends_on
:macos`, so Linuxbrew refuses it rather than installing something that
does not work. On Linux `brew services` would write a
`homebrew.ssf.service` unit of its own while ssf drives its own
`ssf.service` through `ssf ui service enable|disable`, so the two would
not line up; a Linux host installs the `.deb`, `.rpm` or Arch package
from `packaging/linux` and `packaging/release` instead.

## One version filter, in three places

Three checks decide which versions get a formula, and they have to agree:
the published release's tag is checked inside `homebrew.yml`, its manual
dispatch checks the tag the maintainer enters, and `render.sh` checks the
version it is given. All accept only `v[0-9]+.[0-9]+.[0-9]+`, matching
`.github/workflows/release.yml`'s tag filter. A looser check could publish a
formula for a tag release.yml never built (`v0.2.0-rc1`, say, which it skips
because nfpm would write `0.2.0~rc1` and makepkg refuses a pkgver with a
hyphen), so `brew install` would build from a tarball whose release
carries no packages. Change one and change the others in the same commit.

`render.sh` takes the repository as its third argument (the workflow
passes the one it is running in) and writes it into all three lines of
the formula that name a repository: `url`, `homepage` and `head`. A
formula rendered from a fork is therefore about that fork throughout,
including what `brew install --HEAD` builds.

One asset of each release matters to the Mac path: the Linux aarch64
binary `ssf-X.Y.Z-linux-aarch64`, which `ssf vm build` fetches with `gh`
as the guest's `ssf` on Apple silicon, since a macOS binary cannot run in
the Linux guest. It comes from release.yml's `linux-aarch64-binary` job,
which is `continue-on-error: true` so a cross-compilation failure never
blocks the release: a release can exist, and the formula installs cleanly,
with that asset missing, and `ssf vm build` then fails on the download.
Build it as that job does with a real aarch64-musl compiler (never
`aarch64-linux-gnu-gcc`, which emits glibc references), and attach it by hand
when that happens:

```sh
rustup target add aarch64-unknown-linux-musl
musl_cc=/path/to/aarch64-linux-musl-gcc
CC_aarch64_unknown_linux_musl="$musl_cc" \
  CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$musl_cc" \
  cargo build --locked --release --target aarch64-unknown-linux-musl
install -m755 target/aarch64-unknown-linux-musl/release/ssf ssf-0.2.0-linux-aarch64
gh release upload v0.2.0 ssf-0.2.0-linux-aarch64 \
  --repo mikekelly/simple-software-factory
```

(`[vm] guest_binary` pointing at a Linux build is the way out on one
machine; the asset is what every other Mac needs.)

## Installing

```sh
brew install mikekelly/tap/ssf
```

(`mikekelly/tap/ssf` is Homebrew's short name for `Formula/ssf.rb` in the
`mikekelly/homebrew-tap` repository; `brew tap mikekelly/tap` first is
equivalent.) The formula's caveats say what comes next: setup document,
`ssf vm build`, `brew services start ssf`.

## The tap, once

The maintainer creates the tap repository and gives this repository's
workflow a token that can push to it:

1. Use the public `mikekelly/homebrew-tap` repository, then commit
   `Formula/ssf.rb` to it, for
   example the workflow artifact of the current release, or a render made
   by hand:

   ```sh
   v=0.1.0
   curl -fsSLo ssf.tar.gz "https://github.com/mikekelly/simple-software-factory/archive/refs/tags/v$v.tar.gz"
   packaging/homebrew/render.sh "$v" "$(sha256sum ssf.tar.gz | cut -d' ' -f1)" > <tap>/Formula/ssf.rb
   ```

2. Make a fine-grained personal access token on GitHub with *Contents:
   read and write* on `mikekelly/homebrew-tap` only, and save it as the
   `HOMEBREW_TAP_TOKEN` Actions secret of this repository
   (`gh secret set HOMEBREW_TAP_TOKEN`). Without the secret the workflow
   uploads the rendered formula as an artifact and then fails, so a release
   cannot look published when the tap was not updated.

The tap repository's name is the `HOMEBREW_TAP` environment variable at the
top of the workflow; nothing else needs to know it besides the header
comment of the formula and the install command above.

The tag tarball has to be downloadable without a login, or Homebrew cannot
fetch the source: this repository must be public for the formula to work
(the workflow's download of the tarball fails with a 404 while it is
private).

## Testing a formula change

On a Mac with Homebrew, from the repository root:

```sh
brew install --build-from-source ./packaging/homebrew/ssf.rb
brew audit --strict --new ssf
brew test ssf
brew services start ssf   # the launchd agent; `brew services stop ssf` to stop it
```

`brew install` from a local file checks the `url` download against the
`sha256` in it, so the placeholder fails; `--HEAD` builds the `master`
branch from git without a sha256, and to test the stable url render the
formula first (the command block above) and install the rendered file.
`brew audit --strict` flags the placeholder sha256 too; the rendered copy
in the tap is what should pass it clean.
