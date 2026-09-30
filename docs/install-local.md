# Install on the person's own machine

The route for a factory on the machine you are working on, chosen in [install.md](install.md#2-choose-where-the-factory-runs) (`ssf skill setup`). The steps every route shares are one-line links into [install-common.md](install-common.md) (`ssf skill setup-common`).

Start with [3. Choose the runtime](install-common.md#3-choose-the-runtime) (`ssf skill setup-common`): run the probes on this machine and take the first rung that holds. On this route the daemon runs on this machine, and in host mode the agents run as the person's own user, so no rung adds conditions here. Then follow the steps for the rung taken.

## A guest on this machine

For the VM and Incus guest rungs. The daemon runs in the guest, on this machine; HOST is `ssf-default`.

1. For the Incus guest rung, its setup and backend step ([Incus guest rung](install-common.md#incus-guest-rung)). Then install: [4.1 Linux package](install-common.md#41-linux-package), or on macOS [4.2 macOS, Homebrew](install-common.md#42-macos-homebrew) (`ssf skill setup-common`).
2. [5. `ssf setup` and the service](install-common.md#5-ssf-setup-and-the-service) (`ssf skill setup-common`), on this machine.
3. [6.1 The VM](install-common.md#61-the-vm) (`ssf skill setup-common`), on this machine.
4. [7. Oversee the agents from the person's machine](install-common.md#7-oversee-the-agents-from-the-persons-machine) (`ssf skill setup-common`), with HOST `ssf-default`.
5. [Install the working-with-ssf skill](install-common.md#install-the-working-with-ssf-skill) (`ssf skill setup-common`), on this machine.
6. [Then, on every path](#then-on-every-path).

## Host mode in a Docker container

For the Docker container rung. The daemon runs in the container, on this machine, as its `factory` user; HOST is `ssf-docker`, the SSH entry [platform-specifics.md#docker-container](platform-specifics.md#docker-container) (`ssf skill specifics`) writes. Run the steps below in the container (`ssh ssf-docker`), not on this machine.

1. Build and start the container with [platform-specifics.md#docker-container](platform-specifics.md#docker-container) (`ssf skill specifics`); it installs the package, herdr and linger.
2. [5. `ssf setup` and the service](install-common.md#5-ssf-setup-and-the-service) (`ssf skill setup-common`), for host mode, in the container.
3. [6.2 Host mode](install-common.md#62-host-mode) (`ssf skill setup-common`), in the container; install the harness CLI there.
4. [7. Oversee the agents from the person's machine](install-common.md#7-oversee-the-agents-from-the-persons-machine) (`ssf skill setup-common`), with HOST `ssf-docker`.
5. [Install the working-with-ssf skill](install-common.md#install-the-working-with-ssf-skill) (`ssf skill setup-common`), in the container.
6. [Then, on every path](#then-on-every-path), reading "where the factory runs" as the container.

## Host mode on this machine

For the host mode rung. The daemon runs on this machine, as the person's own user; there is no HOST.

1. Install: [4.1 Linux package](install-common.md#41-linux-package), [4.2 macOS, Homebrew](install-common.md#42-macos-homebrew), or [4.3 Standalone binaries](install-common.md#43-standalone-binaries) when no package fits (`ssf skill setup-common`).
2. [5. `ssf setup` and the service](install-common.md#5-ssf-setup-and-the-service) (`ssf skill setup-common`), for host mode, on this machine (not on the standalone-binaries path).
3. [6.2 Host mode](install-common.md#62-host-mode) (`ssf skill setup-common`).
4. [7. Oversee the agents from the person's machine](install-common.md#7-oversee-the-agents-from-the-persons-machine) (`ssf skill setup-common`), with no HOST.
5. [Install the working-with-ssf skill](install-common.md#install-the-working-with-ssf-skill) (`ssf skill setup-common`), on this machine.
6. [Then, on every path](#then-on-every-path).

## Then, on every path

Where these steps say "where the factory runs" or "where you ran `ssf setup`", read this machine.

1. [8. The bot account](install-common.md#8-the-bot-account) (`ssf skill setup-common`).
2. [9. Who may drive the factory](install-common.md#9-who-may-drive-the-factory) (`ssf skill setup-common`).
3. [10. Sign in the harness](install-common.md#10-sign-in-the-harness) (`ssf skill setup-common`).
4. [11. Watch the first repository](install-common.md#11-watch-the-first-repository) (`ssf skill setup-common`).
5. [12. Other devices, then verify](install-common.md#12-other-devices-then-verify) (`ssf skill setup-common`), including the offer in [12.1](install-common.md#121-offer-remote-access-the-dashboard-and-terminal-access-together).
6. [14. Checklist](install-common.md#14-checklist) (`ssf skill setup-common`); [13. Upgrading, stopping, uninstalling](install-common.md#13-upgrading-stopping-uninstalling) and [what is safe to re-run](install-common.md#what-is-safe-to-re-run) (`ssf skill setup-common`) for later.
