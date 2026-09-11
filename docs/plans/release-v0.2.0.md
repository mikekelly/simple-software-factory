# Release v0.2.0

## Source and tag

The release uses two commits to keep the PKGBUILD in the tag verifiable without
a checksum that depends on itself:

1. Commit A freezes the tested implementation, documentation, version metadata
   and release workflow.
2. Download GitHub's immutable tarball for commit A and record its SHA-256.
3. Commit B changes only release recipe metadata. The release PKGBUILD uses
   `pkgver=0.2.0`, pins the commit A archive by its full object ID, names its
   commit-based extracted directory, and carries the tarball's actual SHA-256.
4. Tag commit B as `v0.2.0`. The tag therefore contains a complete, valid
   recipe whose compiled application source is exactly commit A.

The development PKGBUILD's generated version is refreshed after the source
commit. No release recipe uses `SKIP`, a guessed checksum, or a moving branch.

## Build and validation

Build the final Arch artifact with ordinary `makepkg` from commit B's release
recipe and the checksum-verified commit A archive. Verify that the result is
named `ssf-0.2.0-1-x86_64.pkg.tar.zst`, its `.PKGINFO` retains every declared
dependency, its binary reports `ssf 0.2.0`, and its payload contains the
`default.target` user unit, package removal hook, and Omarchy widget resources.

The final source runs the full test suite, `cargo fmt --check`, and Clippy. The
release notes publish the artifact SHA-256 and a fail-fast install example that
downloads to a fresh directory under `/tmp`, verifies the checksum, installs
with `pacman -U`, and then calls the explicit `ssf setup` flow. They state that
bot authentication is separate and that SSF does not configure automatic
updates.

The package/widget lifecycle was exercised in an Omarchy 4.0.3 VM against the
same implementation before the release-only version bump. The versioned final
package is separately built, tested, and inspected; the release does not claim
a second full VM lifecycle run unless one is performed.
