# End-to-end smoke testing

How to exercise a real factory stack (host service, VM or container, guest
daemon, sessions) on scratch infrastructure before trusting it on a live one.
Unit tests and `ssf-server --once` scratch runs ([development.md](development.md))
cover the code; these checks cover what only shows up on a real host: service
restarts, package upgrades, reboots and the backend underneath.

Record each run on the issue it verifies: the host, the versions before and
after, and the result of every step. This page is the recipe, not the record.

## Rules

- **Never use the live factory's VM, catalog or service.** A scratch VM gets
  its own `[vm] name`, `ssh_port` and disks, and is driven with an isolated
  `SSF_CONFIG_DIR`/`SSF_STATE_DIR`.
- **Check host disk and memory first.** A scratch VM plus incus images needs
  10–15 GB of disk. On btrfs, reflink copies of VM disks still fill the
  filesystem as they diverge. Delete scratch disks when you finish.
- **Sessions are real.** Signing a bot in and binding a repository starts
  real agents on GitHub. Use a sandbox repository the live factory does not
  watch, and agree it with the person first.

## A scratch Firecracker VM

On a KVM host with ssf's Firecracker assets already downloaded (`~/.local/share/ssf/vm`),
a second VM reuses the kernel and root image:

```sh
S=/path/to/scratch; mkdir -p $S/cfg $S/state
cat > $S/cfg/config.toml <<'EOF'
[vm]
enabled = true
backend = "firecracker"
name = "scratch"          # not the live VM's name
dir = "~/.local/share/ssf/vm"
vcpus = 2
mem_mib = 5120
data_gib = 30
ssh_port = 2322           # not the live VM's port (2222)
EOF
SSF_CONFIG_DIR=$S/cfg SSF_STATE_DIR=$S/state ssf vm start
SSF_CONFIG_DIR=$S/cfg SSF_STATE_DIR=$S/state ssf vm ssh -- uptime
```

It runs as `ssf-vm-scratch-*.scope` units beside the live VM's. The root disk
is the image's size (about 8 GB, 3 GB free), so put anything large on the data
disk at `/var/lib/ssf`.

Firecracker exposes no nested virtualisation, so this guest has no `/dev/kvm`:
a faithful stand-in for a KVM-less VPS.

## Incus backend on a host without KVM

The scratch VM becomes the "VPS". The ssf guest image is not a clean host, so
prepare it first:

1. **Stop being a guest.** `sudo systemctl disable --now ssf`, then remove the
   `SSF_*` lines from `/etc/environment` and move `/etc/profile.d/ssf.sh`
   aside. Otherwise every login has `SSF_VM_GUEST=1` and the guest's
   `SSF_STATE_DIR`.
2. **Use a fresh host user.** The `ssf` user's `~/.config/ssf/config.toml` has
   a legacy `[vm]` table that conflicts with a catalog. Create a user
   (`vps` below) in `sudo` and `incus-admin`, give it your key, add it to
   sshd's `AllowUsers` (the image allows only `ssf`) and
   `loginctl enable-linger` it so its user services run.
3. **Add `127.0.1.1 ssf-vm` to `/etc/hosts`**, or every `sudo` warns.
4. **Install incus with its state on the data disk:** bind-mount a directory
   under `/var/lib/ssf` on `/var/lib/incus` (in `/etc/fstab`), then
   `apt install incus iptables`.
5. **Network:** ssf's guest kernel lacks nftables `inet` tables and
   masquerading, so `incus admin init --minimal` fails half-done. Follow
   [platform-specifics.md, Linux without KVM](platform-specifics.md#linux-without-kvm)
   to create `incusbr0` without Incus's firewall, add the NAT rule (use
   `iptables-legacy`), set `net.ipv4.ip_forward=1`, and add the default
   profile's `root` and `eth0` devices.
6. **Log `gh` in for the host user.** The host downloads the guest package
   with `gh`; without it the first start leaves the guest on a seeded binary
   with its daemon disabled until `ssf vm upgrade` is run.

Then, as `vps`, write `~/.config/ssf/servers.toml` with a `vm` server using
`backend = "incus"` (small sizes: 2 vCPUs, 3072 MiB, 10 GiB data), run
`ssf --server vps vm build`, and `systemctl --user enable --now ssf@vps`.

To install an older release for an upgrade test, `dpkg -i` its `.deb` from
`gh release download vX.Y.Z --pattern 'ssf_X.Y.Z-1_amd64.deb'`.

### Probe

Run after every step and compare with the baseline. A marker unit in the guest
stands in for an agent when no session is live:

```sh
ssf --server vps vm ssh -- sudo systemd-run --unit=ssf-marker sleep infinity   # once

echo "host: $(ssf --version) service pid: $(systemctl --user show -p MainPID --value ssf@vps)"
echo "container: $(incus list ssf-default -f csv -c ns) init pid: $(incus info ssf-default | awk '/^PID:/{print $2}')"
ssf --server vps vm ssh -- 'echo "uptime -s: $(uptime -s) guest: $(dpkg-query -W -f="\${Version}" ssf) marker: $(systemctl show -p MainPID --value ssf-marker)"'
```

### Checks

| Check | Pass |
|---|---|
| `ssf vm build` with no `/dev/kvm` | an Incus system container, provisioned |
| `systemctl --user restart ssf@vps`, twice | uptime, container init pid and marker pid unchanged; the log says "leaving the VM running" then "already running; supervising it" |
| host upgrade: `sudo dpkg -i` the newer `.deb` | the container is untouched and `ssf status` shows the version difference. The package does not restart user services: `systemctl --user daemon-reload && systemctl --user restart ssf@vps`, then the new server supervises the running container |
| `ssf vm upgrade` | `dpkg -l ssf` in the guest shows the new version; uptime, container pid and marker (or agent) pids unchanged |
| guest binary | `/usr/local/bin/ssf` is absent or links to `/usr/bin/ssf` |
| `sudo systemctl restart incus` | container, guest and supervisor unaffected |
| Docker in the container, as `ssf` | `docker run --rm hello-world` and a small `docker build` work; `incus config show ssf-default` has `security.nesting` and the `mknod`/`setxattr` intercepts, set by ssf |
| `ssf vm restart` with live sessions | every session resumes through `ssf launch` with its original `--resume` id and `GH_TOKEN`, `GIT_AUTHOR_NAME`, `SSF_DELIVERY_MAILBOX`; no "terminal fallback" in `ssf doctor` |

The last row, and agent pids in the upgrade row, need a bot signed in inside
the guest and a sandbox repository bound (see Rules).

## Teardown

```sh
SSF_CONFIG_DIR=$S/cfg SSF_STATE_DIR=$S/state ssf vm destroy
rm -rf $S
df -h /home
```

`gh` was logged in for the scratch host user with a real token; destroying the
VM removes it.
