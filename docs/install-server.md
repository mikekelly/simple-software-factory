# Install on a server

The route for a factory on a server or VPS, chosen in [install.md](install.md#2-choose-where-the-factory-runs) (`ssf skill setup`). The person's own machine only drives it: [install-client.md](install-client.md) (`ssf skill setup-client`) there. The steps every route shares are one-line links into [install-common.md](install-common.md) (`ssf skill setup-common`).

## A server

Suitable servers include the dedicated server that comes with a Grok Bot account, or a VPS from Hetzner, Linode, OVH or a similar provider. Requirements: Linux, a non-root user, outbound HTTPS, and enough RAM for the sessions by the rule in [install.md](install.md#is-a-vm-reasonable-here) (`ssf skill setup`). Renting a machine costs money and usually means creating an account: both are the person's decision, not yours. Get explicit consent before proposing a specific product, and do not create the account for them. Check the host's real capabilities with [the probes](install.md#2-choose-where-the-factory-runs), run on the server, rather than the product description: a vendor name settles none of them.

Ask the person one question: **is this server dedicated to the factory, or does it also run something else** (a resident agent such as Hermes, OpenClaw, Grok Bot or Meta Muse, or other services)?

**Dedicated: host mode.** A guest keeps the agents away from a resident agent and its files; on a server that exists only for the factory there is nothing to keep them from, and the server itself is the isolation boundary. A guest there only costs memory, setup (Incus, a second Tailscale enrolment) and failure modes of its own. Follow [Host mode on a dedicated server](#host-mode-on-a-dedicated-server).

```
laptop  ──ssh──▶  server (factory account, host mode)
```

**Shared: a guest.** Put the agents in a guest, so they are kept away from the resident agent and its files:

```
laptop  ──ssh──▶  server (resident agent)  ──▶  ssf guest (Firecracker, or Incus without KVM)
```

Take [3. Choose the runtime](install-common.md#3-choose-the-runtime) (`ssf skill setup-common`) with the probes run on the server. On this route, root for Incus and the package is the server's: give the person the commands to run over SSH there. A VM or Incus guest rung leads to [A guest on the server](#a-guest-on-the-server). The host mode rung here means a factory account of its own, kept apart from the resident agent's, following [Host mode on a dedicated server](#host-mode-on-a-dedicated-server) from its step 2.

**Who installs** a guest. Two entry points reach the same end state; follow the one that is you:

- **A. An agent on the person's laptop installs**, driving the server over SSH: run every server-side command below as `ssh user@server '<command>'` (or in an SSH session), and the laptop-side steps locally. At the end, give the resident agent (if there is one) its own access, [7.1](#71-a-guest-on-a-server-access-for-the-resident-agent-and-the-laptop) step 1, run over SSH as the resident agent's user.
- **B. The resident agent on the server installs**: run the server-side commands locally. Before anything else after [6.1 The VM](install-common.md#61-the-vm) is built, give yourself working access ([7.1](#71-a-guest-on-a-server-access-for-the-resident-agent-and-the-laptop) step 1) and confirm it; then **offer** the person direct access from their own device (7.1 step 2), producing the exact steps for them or for their laptop's agent.

Either way the end state is: the resident agent and the person's laptop each have `ssf` CLI, SSH and herdr access to the guest.

## Host mode on a dedicated server

The daemon runs on the server in host mode, as a factory account of its own; the person operates it from their own machine as a client over SSH. Nothing here needs KVM, Docker or a desktop session. In host mode agents can reach their user's files and credentials, so the factory gets its own account, and that account can become root without a password so the agents manage their own environment: the server is the isolation boundary, so this is safe there. HOST is `user@host`, that account on the server, and the agents live in the herdr session `ssf`. The same steps are the fallback on a shared server that can run no guest.

1. Probe the server ([A server](#a-server)).
2. Create the factory account and give it passwordless sudo, for example `echo 'ssf ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/ssf` as root, then `visudo -cf /etc/sudoers.d/ssf`. Put the person's SSH public key in that account's `authorized_keys` so they, and `herdr machine add`, can reach it directly.
3. Install herdr, and the harness CLIs the repositories will use, as that account.
4. Install ssf as that account: [4.1 Linux package](install-common.md#41-linux-package) where one fits (Debian/Ubuntu or Fedora/RHEL on x86_64 or aarch64, Arch on x86_64; it brings `gh` and the `ssf@.service` unit), with its prerequisites (on minimal images, refresh the package indexes first and add CA certificates and curl); otherwise [4.3 Standalone binaries](install-common.md#43-standalone-binaries) (`ssf skill setup-common`) into `~/.local/bin`, run under whatever keeps processes alive on that host.
5. On the package path, [5. `ssf setup` and the service](install-common.md#5-ssf-setup-and-the-service) (`ssf skill setup-common`), for host mode, as that account; its linger step is `sudo loginctl enable-linger ssf`, and the service then survives logout and starts at boot.
6. [6.2 Host mode](install-common.md#62-host-mode) (`ssf skill setup-common`), on the server.
7. [4.4 Client only](install-common.md#44-client-only-driving-a-factory-elsewhere) (`ssf skill setup-common`) on the person's own machine, with the destination `user@host`: [install-client.md](install-client.md) (`ssf skill setup-client`).
8. [7. Oversee the agents from the person's machine](install-common.md#7-oversee-the-agents-from-the-persons-machine) (`ssf skill setup-common`), with HOST `user@host`, in the herdr session `ssf`.
9. [Install the working-with-ssf skill](install-common.md#install-the-working-with-ssf-skill) (`ssf skill setup-common`), here and on HOST.
10. [The shared steps](#the-shared-steps), as the factory account on the server.

## A guest on the server

The daemon runs in the guest, on the server; the account on the server gets no passwordless sudo.

1. For the Incus guest rung, its setup and backend step ([Incus guest rung](install-common.md#incus-guest-rung)), on the server.
2. [4.1 Linux package](install-common.md#41-linux-package) (`ssf skill setup-common`), on the server, and nothing else there.
3. [5. `ssf setup` and the service](install-common.md#5-ssf-setup-and-the-service) (`ssf skill setup-common`), on the server.
4. [6.1 The VM](install-common.md#61-the-vm) (`ssf skill setup-common`), on the server.
5. [7. Oversee the agents from the person's machine](install-common.md#7-oversee-the-agents-from-the-persons-machine) (`ssf skill setup-common`) for its root paragraph, then [7.1](#71-a-guest-on-a-server-access-for-the-resident-agent-and-the-laptop) below in place of its SSH and herdr steps: HOST is `ssf-default` on the server and `ssf-factory` on the laptop.
6. [Install the working-with-ssf skill](install-common.md#install-the-working-with-ssf-skill) (`ssf skill setup-common`), on the machine you are running on.
7. [The shared steps](#the-shared-steps), on the server; their commands reach the guest.

## The shared steps

1. [8. The bot account](install-common.md#8-the-bot-account) (`ssf skill setup-common`).
2. [9. Who may drive the factory](install-common.md#9-who-may-drive-the-factory) (`ssf skill setup-common`).
3. [10. Sign in the harness](install-common.md#10-sign-in-the-harness) (`ssf skill setup-common`).
4. [11. Watch the first repository](install-common.md#11-watch-the-first-repository) (`ssf skill setup-common`).
5. [12. Other devices, then verify](install-common.md#12-other-devices-then-verify) (`ssf skill setup-common`), with [the checks for a guest on a server](#verify-a-guest-on-a-server), including the offer in [12.1](install-common.md#121-offer-remote-access-the-dashboard-and-terminal-access-together).
6. [14. Checklist](install-common.md#14-checklist) (`ssf skill setup-common`), with [the items for a server](#checklist-for-a-server); [13. Upgrading, stopping, uninstalling](install-common.md#13-upgrading-stopping-uninstalling) and [what is safe to re-run](install-common.md#what-is-safe-to-re-run) (`ssf skill setup-common`) for later.

## 7.1 A guest on a server: access for the resident agent and the laptop

Only when the guest runs on a shared server ([A server](#a-server)). The guest's SSH listens on the server's loopback (`127.0.0.1:<vm.ssh_port>`, 2222 by default), so nothing is published on the internet: the laptop reaches it with `ProxyJump` through the server, and the server's own user reaches it directly.

**Step 1. The server's user (the resident agent).** On the server, as the user that ran `ssf setup`:

```sh
ssf status                                    # the ssf CLI reaches the guest daemon
grep -q '^Host ssf-default$' ~/.ssh/config 2>/dev/null || {
  mkdir -p ~/.ssh && chmod 700 ~/.ssh
  ssf vm ssh-config >> ~/.ssh/config && chmod 600 ~/.ssh/config
}
ssh ssf-default true
herdr machine add ssf-default --label factory </dev/null
herdr machine list
```

Good: `ssf status` names the account and repositories (or none yet), `ssh ssf-default true` returns silently, `herdr machine list` shows `ssf-default`. If `herdr machine add` stops at a prompt, handle it as in [7. Oversee the agents](install-common.md#7-oversee-the-agents-from-the-persons-machine) (`ssf skill setup-common`).

**Step 2. The person's laptop.** In entry point B, offer this to the person first ("Shall I set up access from your laptop?"); in A, do it.

1. **The ssf client** on the laptop: package, Homebrew or bare binary, as in [4.4](install-common.md#44-client-only-driving-a-factory-elsewhere) (`ssf skill setup-common`). No `ssf setup`, no daemon.
2. **A key of the laptop's own.** On the laptop, `ssh-keygen -t ed25519 -f ~/.ssh/ssf-factory -N ''` (skip if it exists), and get the contents of `~/.ssh/ssf-factory.pub`. Never copy the server's private key to the laptop.
3. **Authorise it in the guest.** On the server, append that public key line to the guest `ssf` user's `authorized_keys`, idempotently:

   ```sh
   KEY='ssh-ed25519 AAAA... laptop'            # the laptop's .pub line
   ssf vm ssh -- "mkdir -p ~/.ssh && chmod 700 ~/.ssh && touch ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys && { grep -qxF '$KEY' ~/.ssh/authorized_keys || echo '$KEY' >> ~/.ssh/authorized_keys; }"
   ```

   The laptop must also reach the server itself over SSH as `user@server` (the person's usual login).
4. **The SSH entry** on the laptop, appended to `~/.ssh/config` (skip if `grep -q '^Host ssf-factory$' ~/.ssh/config` finds it); use the port `ssf config get vm.ssh_port` prints on the server:

   ```
   Host ssf-factory
     HostName 127.0.0.1
     Port 2222
     HostKeyAlias ssf-factory
     User ssf
     ProxyJump user@server
     IdentityFile ~/.ssh/ssf-factory
     IdentitiesOnly yes
   ```

   Then `ssh ssf-factory true`. The first connection asks to trust the guest's host key; that is expected. `HostKeyAlias` records it under `ssf-factory` rather than `[127.0.0.1]:2222`, which every guest forwarded to that port would share.
5. **herdr** on the laptop, so the person opens the agents' terminals in their own herdr and agents on the laptop reach running sessions:

   ```sh
   herdr machine add ssf-factory --label factory </dev/null
   herdr machine list
   ```

6. **The server catalog** on the laptop, so `ssf status` and `ssf doctor` reach the factory without `--server`:

   ```sh
   ssh ssf-factory 'command -v ssf-server'    # /usr/local/bin/ssf-server: on the non-interactive PATH
   ssf server add factory --ssh ssf-factory
   ssf status
   ```

   The destination is the SSH alias, so the jump and key from step 4 apply.
7. **The skill, globally on the laptop**, so every harness there knows ssf. In entry point B the laptop is not the installing machine, so no earlier step covers it:

   ```sh
   npx -y skills add mikekelly/simple-software-factory -g -y
   ```

8. **Optionally Tailscale** instead of the jump, for a person already on a tailnet: `ssf vm tailscale` on the server enrolls the guest ([12.1](install-common.md#121-offer-remote-access-the-dashboard-and-terminal-access-together) (`ssf skill setup-common`)); the laptop's SSH entry then uses the guest's tailnet name as `HostName`, port 22, and no `ProxyJump`.

**Skills: the resident agent's harness must load `working-with-ssf`, not only have it on disk.** The laptop has the skill from step 2, item 7; the guest gets it from `ssf vm build` ([Install the working-with-ssf skill](install-common.md#install-the-working-with-ssf-skill), `ssf skill setup-common`). On the server, as the resident agent's user:

```sh
npx -y skills add mikekelly/simple-software-factory -g -y
```

That links the skill into the global skills directories of the harnesses it detects, and prints which. If the resident harness is not in that list, name it with `-a`:

| Resident harness | Global skills directory | Install for it |
|---|---|---|
| Hermes Agent | `~/.hermes/skills` | `-a hermes-agent` |
| OpenClaw | `~/.openclaw/skills` (`~/.clawdbot/skills` or `~/.moltbot/skills` on older installs) | `-a openclaw` |
| Grok Build | `~/.grok/skills` | `-a grok` |
| Claude Code | `~/.claude/skills` | `-a claude-code` |
| Pi | `~/.pi/agent/skills` | `-a pi` |

For example `npx -y skills add mikekelly/simple-software-factory -g -y -a openclaw`. `npx skills add --help` and the CLI's agent list cover the other harnesses it knows; a harness that reads a directory of its own can take a copy with `--copy`.

Then **confirm it is loaded**: start a new session of the resident harness (skills are usually read at startup) and check that it lists `working-with-ssf` in its skills, or ask it to quote that skill's description. A file on disk that the harness does not read does not count.

**No skill mechanism** (for example Meta Muse, or any harness not above that loads no skills directory): put a line in the resident agent's standing instructions or memory instead, such as "For anything about the ssf factory on this server, run `ssf skill` and follow it; `ssf skill setup` is the install guide." Confirm the same way: a new session answers where the ssf guidance comes from.

## Verify a guest on a server

At [12. Other devices, then verify](install-common.md#12-other-devices-then-verify) (`ssf skill setup-common`), for a guest on a server, run from the laptop: `working-with-ssf` is installed globally (`npx -y skills ls -g` lists it), `ssh ssf-factory true`, `herdr machine list` shows `ssf-factory`, and `ssf status` answers; and from the resident agent on the server: its own harness has `working-with-ssf` loaded (a new session lists it or quotes its description; or, without a skill mechanism, its standing instructions point at `ssf skill`), `ssh ssf-default true`, `herdr machine list` shows `ssf-default`, and `ssf status` answers.

## Checklist for a server

Add these to [the checklist](install-common.md#14-checklist) (`ssf skill setup-common`):

- [ ] Server: dedicated, in host mode under a factory account with passwordless sudo; or shared, with the agents in a guest by [the runtime ladder](install-common.md#3-choose-the-runtime), or in host mode as its fallback with the reason said to the person.
- [ ] Guest on a server: the resident agent (if any) and the person's laptop each pass `ssh <entry> true`, show the factory in `herdr machine list`, and get an answer from `ssf status`; the laptop uses its own key through `ProxyJump`; `working-with-ssf` is installed globally on the laptop, and the resident agent has it loaded in its own harness (listed by a new session), or `ssf skill` in its standing instructions where the harness has no skills.
