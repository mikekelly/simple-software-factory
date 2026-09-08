#!/usr/bin/env bash
# Render packaging/homebrew/ssf.rb for a release: the url line gets the tag
# tarball of VERSION, the sha256 placeholder gets the tarball's SHA256, and
# the three lines that name the repository (url, homepage, head) get
# OWNER/REPO, which defaults to this one.
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

# X.Y.Z only, the tags release.yml builds packages for and homebrew.yml
# publishes a formula for; a pre-release (0.2.0-rc1) has no packages behind
# it, so a formula for one would point at an empty release.
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version '$version' is not X.Y.Z"
[[ $repo =~ ^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]] || die "repo '$repo' is not OWNER/REPO"
[[ $sha =~ ^[0-9a-f]{64}$ ]] || die "'$sha' is not a lower-case hex sha256"
[ "$sha" != "$placeholder" ] || die "the sha256 is the placeholder"
grep -q "^  sha256 \"$placeholder\"" "$src" || die "sha256 placeholder not found in $src"
grep -qE '^  url "https://github\.com/[^"]+/archive/refs/tags/v[^"]+\.tar\.gz"$' "$src" \
  || die "tag tarball url line not found in $src"

# url, homepage and head all name the repository, so all three follow
# OWNER/REPO: a formula rendered from a fork points at that fork throughout
# rather than sending `brew install --HEAD` and the homepage link upstream.
url="https://github.com/$repo/archive/refs/tags/v$version.tar.gz"
sed -E \
  -e "s|^  url \"https://github\.com/[^\"]+/archive/refs/tags/v[^\"]+\.tar\.gz\"$|  url \"$url\"|" \
  -e "s|^  homepage \"https://github\.com/[^\"]+\"$|  homepage \"https://github.com/$repo\"|" \
  -e "s|^(  head \"https://github\.com/)[^\"]+(\.git\".*)$|\1$repo\2|" \
  -e "/^  # Placeholder: filled in by /d" \
  -e "s|^  sha256 \"$placeholder\"|  sha256 \"$sha\"|" \
  "$src"
