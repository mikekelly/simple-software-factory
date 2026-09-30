# Install a factory

Read this when someone asks you to set up Simple Software Factory for them, from nothing to a factory that watches one repository and has worked its first issue. Offline copy: `ssf skill setup`. This document chooses where the factory runs and sends you to one route document; the steps every route shares are in [install-common.md](install-common.md) (`ssf skill setup-common`).

**Check the guidance is current.** If `ssf` is already installed where you are working, compare `ssf --version` with the latest release (`gh release view --repo mikekelly/simple-software-factory --json tagName`). If it is older, its `ssf skill setup` is stale: upgrade it first, or read this file from `master`. When installing onto another machine, follow the guidance of the version being installed there, not of the client you happen to have.

## 0. Who this is for, and the outcome

You are an agent doing this on behalf of a person, on a machine you have not seen before. The person owns every decision that costs money, creates an account, needs root on their machine, or widens who can drive the factory. You own the probing, the reading, the unprivileged commands, and the diagnosis.

At the end:

- `ssf` and `ssf-server` are installed somewhere the factory can run.
- A separate GitHub bot account is signed in, with Write access on one repository.
- A harness (the coding agent program) is signed in where sessions run.
- One repository is watched, with a harness, model and effort the person chose.
- One small issue assigned to the bot has produced an agent comment on GitHub.
- `ssf doctor` passes.
- When the agents run on a VM or a rented server: that machine is reachable over SSH, the agents can become root on it without asking anyone (so they administer their own environment), and it is saved in the person's local herdr, so the person and any agents on their machine can oversee the sessions there.
- The person was offered remote access (Tailscale), the web dashboard with the Chrome extension, and terminal access from them, in one question ([12.1](install-common.md#121-offer-remote-access-the-dashboard-and-terminal-access-together)), and what they accepted is set up.

The install is not done until that last offer has been made, even when everything else passes.

Work through the sections in order, here and then in the route document. Every step says what a good result looks like and what is safe to re-run.

**Run it as a guided install, not a checklist.** Before probing, tell the person in a few lines that you will guide them through setup, name the stages (where it runs → install → bot account → harness sign-in → first repository → first issue → oversight → remote access and dashboard), and say you will ask only what is needed, as you go. Before each step, say in one or two sentences what is about to happen and why, and whether it needs anything from them ("Next I'll build the VM image; this takes a few minutes and needs nothing from you"). After it, give a one-line result.

## 1. How to ask, and what needs consent

**Ask one decision at a time, at the step that needs it; never present the full list of questions up front.** Before the first install command, probe ([2](#2-choose-where-the-factory-runs)) and ask only where the factory should run. Every later question lives in the section that needs it:

| Question | Asked in |
|---|---|
| Where the factory runs, after you present the options | [2](#2-choose-where-the-factory-runs) |
| Whether renting a host is acceptable, and at what cost | [2](#2-choose-where-the-factory-runs), only if a rented host is proposed |
| Whether a bot GitHub account exists, or may be created | [8](install-common.md#8-the-bot-account) (`ssf skill setup-common`) |
| Which repository to watch, who owns it, and whether they can grant the bot Write | [8, Write access](install-common.md#8-the-bot-account) (`ssf skill setup-common`) |
| Which harness they already pay for, and any metered API spend | [10](install-common.md#10-sign-in-the-harness) (`ssf skill setup-common`) |
| The model and effort | [11](install-common.md#11-watch-the-first-repository) (`ssf skill setup-common`) |
| Who may drive the factory, if not the default | [9](install-common.md#9-who-may-drive-the-factory) (`ssf skill setup-common`) |

**Hand privileged commands to the person.** Do not run `sudo` yourself: most harnesses have no terminal for a password, and root on their machine is theirs to use. Prepare everything the command needs first (download the package, print its exact path), give the person the exact command, ask them to run it in their own terminal (in Claude Code, typing `! <command>` runs it in the session), and continue once they confirm and you have checked the result. Run it yourself only when you already have non-interactive root there (`sudo -n true` succeeds), such as inside the factory's own VM.

Consent you must obtain explicitly, in words, at the step it applies to:

| Needs consent | Why |
|---|---|
| Creating a GitHub account | it is their identity and their email |
| Renting a host, or any metered API spending | it costs them money |
| The harness, model and effort for the repository | it costs them money and sets quality |
| `--allowed-users '*'` / `--accept-anyone-risk` | it lets anyone on GitHub drive their factory |
| `ssf uninstall --force`, `ssf purge --force` | these can destroy unpushed work |

Decide these yourself, no need to ask: which probe commands to run, which install path fits the measurements, VM sizes (let `ssf vm build` choose), when to re-run a failed idempotent step, how to read `ssf doctor`.

## 2. Choose where the factory runs

Run the probes, then propose. The person picks; this is the only question before installing.

```sh
uname -s -m
nproc 2>/dev/null || sysctl -n hw.ncpu
free -g 2>/dev/null || sysctl -n hw.memsize
df -h "$HOME"
test -r /dev/kvm && test -w /dev/kvm && echo kvm-ok
systemctl --user is-system-running
command -v gh herdr
```

`free`, `/dev/kvm` and `systemctl --user` are Linux only; on macOS `sysctl -n hw.memsize` reports bytes and the VM runs through lima instead of KVM.

| What the probes say | Path |
|---|---|
| Linux or macOS, and the factory should run on this machine | **On this machine**: the first rung of [the runtime ladder](install-common.md#3-choose-the-runtime) that holds (a VM, else an Incus guest, else host mode in a Docker container, else host mode on the machine itself). [install-local.md](install-local.md) (`ssf skill setup-local`). |
| This machine is a server or VPS dedicated to the factory, running nothing else | **Host mode on a dedicated server**. [install-server.md](install-server.md) (`ssf skill setup-server`). |
| This machine is a server or VPS that also runs something else (you are its resident agent, or other services) | **A guest on the server** (Firecracker, else Incus). [install-server.md](install-server.md) (`ssf skill setup-server`). |
| Too few resources here, or the person does not want agents on this machine | **A server** runs the factory; this machine only drives it. [install-server.md](install-server.md) (`ssf skill setup-server`); [install-client.md](install-client.md) (`ssf skill setup-client`) here. |
| A factory already runs somewhere else | **Client only**. [install-client.md](install-client.md) (`ssf skill setup-client`). |

### The setups at a glance

Four setups install a factory. Each route document says which setups it covers, and each command block where it runs: the **laptop** (the person's own machine), the **server**, or the **guest**. Where the daemon runs decides where the dashboard and Tailscale go.

| | Local VM | Guest on a server | Host mode, dedicated server | Host mode, locally |
|---|---|---|---|---|
| Machines | laptop, guest | laptop, server, guest | laptop, server | laptop |
| Installed on the laptop | ssf package or Homebrew | ssf client only ([4.4](install-common.md#44-client-only-driving-a-factory-elsewhere)) | ssf client only ([4.4](install-common.md#44-client-only-driving-a-factory-elsewhere)) | ssf package or Homebrew, herdr, harness |
| Installed on the server | — | ssf package (brings gh, git, jq), Incus if no KVM; nothing else by hand | ssf, gh, git, jq, herdr, harness ([Host mode on a dedicated server](install-server.md#host-mode-on-a-dedicated-server)) | — |
| Installed in the guest | by `ssf vm build` | by `ssf vm build` | — | — |
| Agents run as | guest's `ssf` user | guest's `ssf` user | the server's factory account | the person's own user |
| Passwordless sudo | guest's `ssf` user (already) | guest's `ssf` user (already); **not** the server's account | the factory account (granted by the person) | nobody new |
| Daemon, dashboard, Tailscale | guest (`ssf vm tailscale`) | guest (`ssf vm tailscale`) | server | laptop |

### Is a VM reasonable here

`ssf vm build` sizes the guest from the host and prints what it chose:

- vCPUs: host CPUs minus one, at least 2.
- Memory: half the RAM, at least 4096 MiB, but never more than the host has.
- Data disk: half the free space where the disk lands, at least 20 GiB, sparse so it reserves nothing up front.

Rule of thumb for judging "reasonable": each parallel agent session wants about one vCPU and 2 GiB of RAM, and the person's own desktop needs to keep about 4 GB. A 4-core, 8 GB machine gives a guest of 3 vCPUs and 4 GiB, which is one or two sessions at a time and leaves the machine usable. An 8-core, 8 GB machine gets the same 4 GiB but 7 vCPUs by the rule, which is more CPU than that memory can use: pass `--vcpus 2` or `--vcpus 3` to `ssf vm build` on a small machine rather than accept the rule. With 8 GB or less in total, present all three options and recommend host mode or a rented host over a VM; below 8 GB, the VM is not reasonable. An Incus guest is a container: its vCPUs and memory are limits shared with the host, not reserved, so the 8 GB rule does not apply to it, and a small VPS with 4 GB can run one session at a time. Allow roughly 30 GB of disk headroom for images and data, plus room for the repositories and their builds.

Say to the person, in one line each, what the options cost them: the VM keeps agents away from their files but takes half the machine; host mode takes only what the sessions use but the agents run as their user with permission prompts bypassed; a rented host costs money and puts the factory on a machine they administer over SSH.

### Supported platforms

Tier 1, tested by hand before each release: **Arch/Omarchy with Firecracker**, **macOS with Lima**, and the **Claude** and **Codex** harnesses. Everything else (`.deb` and `.rpm` distributions, standalone binaries, Incus, host mode, SSH targets, other harnesses) is best effort: each package format is installed in a container and its `ssf --version` and `ssf --help` checked before a release is published, but nothing is run there beyond that. This list documents what is tested; it enables or disables nothing.

### Open the route document

Read the route document for the path the person chose, and follow it to the end; it links each shared step in [install-common.md](install-common.md) (`ssf skill setup-common`) in order.

| Route | Document | Offline |
|---|---|---|
| On this machine: local VM, local Incus guest, Docker container or host mode | [install-local.md](install-local.md) | `ssf skill setup-local` |
| A server, dedicated or shared, and access to it | [install-server.md](install-server.md) | `ssf skill setup-server` |
| Client only, driving a factory elsewhere | [install-client.md](install-client.md) | `ssf skill setup-client` |
| Steps every route shares, from the runtime choice and install to the checklist | [install-common.md](install-common.md) | `ssf skill setup-common` |

Reading this file from GitHub, the others sit beside it at the same base URL: `https://raw.githubusercontent.com/mikekelly/simple-software-factory/master/docs/install-local.md`, and likewise `install-server.md`, `install-client.md` and `install-common.md`.
