#!/usr/bin/env bash
# Render packaging/homebrew/ssf.rb for a release: the url line gets the tag
# tarball of VERSION and the sha256 placeholder gets the tarball's SHA256.
# .github/workflows/homebrew.yml runs this on every tag; it is a script so a
# maintainer can run the same thing by hand.
#
#   packaging/homebrew/render.sh VERSION SHA256 [OWNER/REPO] > Formula/ssf.rb
set -euo pipefail

usage() { echo "usage: $0 VERSION SHA256 [OWNER/REPO]" >&2; exit 2; }
die() { echo "render.sh: $*" >&2; exit 1; }

[ $# -ge 2 ] && [ $# -le 3 ] || usage
version=$1
sha=$2
repo=${3:-mikekelly/simple-software-factory}
src="$(cd "$(dirname "$0")" && pwd)/ssf.rb"
placeholder=0000000000000000000000000000000000000000000000000000000000000000

[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+.][0-9A-Za-z.]+)?$ ]] || die "version '$version' is not X.Y.Z"
[[ $sha =~ ^[0-9a-f]{64}$ ]] || die "'$sha' is not a lower-case hex sha256"
[ "$sha" != "$placeholder" ] || die "the sha256 is the placeholder"
grep -q "^  sha256 \"$placeholder\"" "$src" || die "sha256 placeholder not found in $src"
grep -qE '^  url "https://github\.com/[^"]+/archive/refs/tags/v[^"]+\.tar\.gz"$' "$src" \
  || die "tag tarball url line not found in $src"

url="https://github.com/$repo/archive/refs/tags/v$version.tar.gz"
sed -E \
  -e "s|^  url \"https://github\.com/[^\"]+/archive/refs/tags/v[^\"]+\.tar\.gz\"$|  url \"$url\"|" \
  -e "/^  # Placeholder: filled in by /d" \
  -e "s|^  sha256 \"$placeholder\"|  sha256 \"$sha\"|" \
  "$src"
