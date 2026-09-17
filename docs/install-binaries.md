# Install standalone binaries

For a VPS or Linux container without KVM / a systemd user session, follow
[VPS / headless host](headless-host.md): standalone binaries
and host mode run a complete factory without package services. Otherwise use
the packages for Arch-family Linux (including Omarchy) or Debian-family Linux
(including Ubuntu); see [Setup](setup.md#2-install-the-package). macOS support
is planned next but is not supported yet. The Linux package
named `ssf` includes both the `ssf` client and `ssf-server` daemon. There are no
separate client and server packages. Neither package installation nor copying a
binary enables a background service.

[GitHub Releases](https://github.com/mikekelly/simple-software-factory/releases)
also provide these standalone **Linux** executables:

| Architecture (`uname -m`) | Client | Server |
|---|---|---|
| `x86_64` | `ssf-VERSION-linux-x86_64` | `ssf-server-VERSION-linux-x86_64` |
| `aarch64` / `arm64` | `ssf-VERSION-linux-aarch64` | `ssf-server-VERSION-linux-aarch64` |

These are static musl builds. ARM64 builds are best effort; check that the
selected release has both assets before installing a local factory. Packages
are x86_64 only. Linux binaries do not run on macOS or other Unix kernels.
Other operating systems and architectures have no
prebuilt binaries in this release workflow.

## Download and install on Linux

Select a release and matching architecture explicitly. For example, these
commands install the published v0.10.0 client for Linux x86_64 without sudo:

```sh
version=0.10.0
arch=x86_64                 # aarch64 for Linux ARM64
base="https://github.com/mikekelly/simple-software-factory/releases/download/v$version"
download_dir=$(mktemp -d)
curl -fL "$base/ssf-$version-linux-$arch" -o "$download_dir/ssf"
install -Dm755 "$download_dir/ssf" "$HOME/.local/bin/ssf"
export PATH="$HOME/.local/bin:$PATH"
ssf --version
```

Add `~/.local/bin` to your shell's PATH for future terminals. Release asset
names include a version; install them under the unversioned names shown here.
For updates, repeat with the new version, updating both binaries together on a
local factory after stopping its daemon.

## Client only: operate an existing factory over SSH

Install an OpenSSH client using your distribution's package manager, then use:

```sh
ssf --server user@factory.example status
```

The remote account must have SSH access and `ssf-server` on its noninteractive
SSH PATH. The factory runs on that remote machine; the client computer needs
neither a local daemon nor `ssf setup`. For named destinations, see the
[server catalog](configuration.md#server-catalog).

## Run a local factory without a package

Download the server from the **same release**, using the variables above:

```sh
curl -fL "$base/ssf-server-$version-linux-$arch" -o "$download_dir/ssf-server"
install -Dm755 "$download_dir/ssf-server" "$HOME/.local/bin/ssf-server"
ssf-server --version
ssf config show
```

Keep `ssf` and `ssf-server` together in the same directory: local client
commands invoke the server-side endpoint. Supply GitHub CLI 2.40+, Git,
OpenSSH, jq, and your selected driver and coding harness separately. Follow
[Setup's host alternative](setup.md#alternative-on-the-host-in-herdr)
for driver configuration, bot authentication and repository setup. Skip its
`ssf server add local` and `ssf setup` commands for this standalone path: on a
fresh installation, leave the server catalog empty so the client and foreground
daemon use the same default configuration and state directories. Then run
`ssf-server` in the foreground. It remains running until stopped; arrange your
own service supervision if required.

The bare binaries do not include the package's service units, VM build scripts,
configuration examples or Omarchy helpers. `ssf setup` expects a package-owned
installation and is not a standalone-binary installer. For the documented
managed VM and boot service setup, use the full package. Installing Linux ARM64
binaries alone does not establish support for the packaged Firecracker image.
