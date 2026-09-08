#!/bin/bash
# Build the Debian/Ubuntu (.deb) and Fedora (.rpm) packages of ssf.
#
#   packaging/linux/build.sh [VERSION]
#
# From the repository root (any working directory works): builds the static
# x86_64 musl binary (`rustup target add x86_64-unknown-linux-musl` if it is
# missing; when there is no musl-gcc the C parts, ring's, are compiled with
# plain gcc), then runs nfpm on packaging/linux/nfpm.yaml for both formats.
# Output, in packaging/linux/dist/: ssf_VERSION-1_amd64.deb,
# ssf-VERSION-1.x86_64.rpm and the bare static binary ssf-VERSION-linux-x86_64
# (a release asset too: the macOS lima guest downloads it).
#
# VERSION is the argument, else $VERSION, else the version in Cargo.toml.
# nfpm is $NFPM, else `nfpm` on PATH (https://nfpm.goreleaser.com, 2.47.0 is
# what .github/workflows/release.yml pins).
# To look inside afterwards: `ar p ssf_*.deb data.tar.gz | tar -tzv` (nfpm gzips)
# and `bsdtar -tvf ssf-*.rpm` (or dpkg-deb -c / rpm -qlp where installed).
set -euo pipefail

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  sed -n '2,/^set -euo/{/^set -euo/!s/^# \{0,1\}//p}' "$0"
  exit 0
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

target=x86_64-unknown-linux-musl
nfpm="${NFPM:-nfpm}"
command -v "$nfpm" >/dev/null || { echo "build.sh: nfpm not found (set NFPM or put nfpm on PATH)" >&2; exit 1; }

cargo_version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
VERSION="${1:-${VERSION:-$cargo_version}}"
if [[ "$VERSION" != "$cargo_version" ]]; then
  echo "build.sh: VERSION $VERSION does not match Cargo.toml's $cargo_version; bump Cargo.toml (and Cargo.lock) before tagging" >&2
  exit 1
fi
export VERSION

if ! rustup target list --installed | grep -qx "$target"; then
  echo "==> rustup target add $target"
  rustup target add "$target"
fi
if ! command -v musl-gcc >/dev/null; then
  echo "==> no musl-gcc: compiling the C parts for $target with gcc (CC_x86_64_unknown_linux_musl=gcc)"
  export CC_x86_64_unknown_linux_musl=gcc
fi

echo "==> cargo build --release --target $target (version $VERSION)"
# Stripped at link time (the Arch package is stripped by makepkg).
CARGO_PROFILE_RELEASE_STRIP=true cargo build --locked --release --target "$target"

outdir=packaging/linux/dist
mkdir -p "$outdir"
built=()
for fmt in deb rpm; do
  echo "==> nfpm package -p $fmt"
  # nfpm prints "created package: PATH" for the file it wrote; that path,
  # not a name built here, is what gets checked at the end.
  out="$("$nfpm" package -f packaging/linux/nfpm.yaml -p "$fmt" -t "$outdir")"
  echo "$out"
  pkg="$(sed -n 's/^created package: *//p' <<<"$out" | tail -1)"
  [[ -n "$pkg" ]] || { echo "build.sh: nfpm did not report a created package for $fmt" >&2; exit 1; }
  built+=("$pkg")
done
install -m755 "target/$target/release/ssf" "$outdir/ssf-$VERSION-linux-x86_64"
built+=("$outdir/ssf-$VERSION-linux-x86_64")
echo "==> packages in $outdir:"
ls -1 "${built[@]}"
