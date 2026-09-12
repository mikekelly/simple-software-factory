# Docker Sandboxes feasibility for SSF

Investigation for [#225](https://github.com/mikekelly/simple-software-factory/issues/225),
2026-09-12. SSF source audited: `f388b1a455fbece3dfe430a37731f45652154b52`.
Docker CLI examined: **v0.42.1**, commit
`cc6e400a4a3ce3ce5e0b2b77b8ee352aac854c64`.

## Decision

**No-go for replacing Firecracker or Lima, or enabling production migration now.
Go for a gated, opt-in experimental backend, beginning with a supported-host
lifecycle/authentication experiment.** Prefer one mountless sandbox per factory.
Docker removes substantial image/runtime plumbing, and ordinary Dockerfile
extensions are attractive. It does not yet demonstrate SSF's data-preserving
reset, unattended credentials, or service recovery contract.

This is a design proposal, not an implemented backend. Docker documentation is
evidence of an API, not evidence that SSF works on it. Executed Docker
checks were CLI inspection, kit validation, a failed isolated create attempt, and
a settings query that unexpectedly attempted (and failed) daemon startup.
[Reproduction and pending runtime tests](../experiments/docker-sandboxes/README.md)
record that distinction. No existing VM or factory state was used; no credentials were supplied or exported.

## Capability matrix

The current-backend columns describe **SSF's implementations**, not everything
Firecracker or Lima could theoretically do. Sources:
[VM guide](../vm.md), [dispatch/Firecracker](../../src/vm.rs),
[Lima](../../src/vm/lima.rs), [guest services](../../vm/guest/units/),
[configuration](../../src/config.rs).

| Requirement | Firecracker in SSF | Lima in SSF | Docker Sandboxes candidate |
|---|---|---|---|
| Hosts | Linux x86_64, KVM | macOS Intel/Apple silicon; Linux QEMU | Documented Ubuntu 24.04+ x86_64/arm64, macOS 14+ Apple silicon, Windows 11 x64; Arch unsupported |
| Headless lifetime | Host supervisor + guest systemd | Host supervisor + guest systemd | CLI background creation/exec; kit startup hooks; SSF supervision not demonstrated |
| Isolation | One root-capable factory guest; no host home mount | Same; SSF-owned read-only seed share | MicroVM; use mountless creation and disable shared skills/SSH-agent access |
| Create/start/stop | Implemented | Implemented | `create`, auto-starting `exec`, `stop` documented |
| Inspection/health | VM, SSH, daemon health; unknown distinct from stopped | Same | `ls --json`, `daemon status --json`; SSF heartbeat/IPC checks still required |
| Command/terminal | SSH, attach, forwarding | SSH, attach, forwarding | `exec`, SSH through local daemon; different user/environment/auth contract |
| Logs | Guest journal + VM logs | Guest journal + Lima logs | Policy/diagnostics tooling; retain SSF/herdr logs explicitly |
| CPU/RAM/disk | Explicit resources, separate data disk | Explicit resources, separate data disk | CPU/RAM create flags, filesystem size settings; replacement/resize requires qualification |
| Stop/start persistence | Root + independent data | Root + independent data | Sandbox filesystem retained; no claim that arbitrary processes resume |
| Replace OS/reset | Replace root, retain data | Recreate instance, retain data disk | No equivalent proven; `sbx reset` destroys global state |
| Credentials | Guest-owned bot/harness secrets, signing key | Same | Host proxy possible; SSF token resolution and probes need adaptation |
| Harness choice | Nine harnesses, herdr integration | Same | Built-in agent kit is one runtime; custom SSF kit or explicit shell bootstrap needed |
| Template distribution | SSF-built Ubuntu 24.04 LTS image | Provisioned cloud image | OCI template + Dockerfile extension; architecture/build pipeline untested |
| Operational dependency | Open-source VM/network binaries | limactl + host hypervisor | Docker account, proprietary runtime, evolving Early Access surface |

Docker sources for platform, isolation, and operation:
[installation](https://docs.docker.com/ai/sandboxes/install/),
[architecture](https://docs.docker.com/ai/sandboxes/architecture/),
[CLI](https://docs.docker.com/reference/cli/sbx/).

## Lifecycle and unattended operation

The primitives are sufficient for a prototype, not yet a stable SSF contract:

| Operation | Documented primitive | Required SSF interpretation / remaining test |
|---|---|---|
| Create | [`sbx create`](https://docs.docker.com/reference/cli/sbx/create/) | Explicit unique name, no workspace argument, bounded CPU/RAM; reject an unexpected existing identity |
| Start/execute | [`sbx exec`](https://docs.docker.com/reference/cli/sbx/exec/) | Automatically starts a stopped sandbox; `-d` runs a background process; check exit status and readiness independently |
| Stop | [`sbx stop`](https://docs.docker.com/reference/cli/sbx/stop/) | Quiesce SSF/herdr first; test signal delivery, timeout and repeated stop |
| Inspect | [`sbx ls --json`](https://docs.docker.com/reference/cli/sbx/ls/) | Validate schema and select exact name; there is no documented local `inspect` verb in the CLI index |
| Runtime service | [`daemon start`](https://docs.docker.com/reference/cli/sbx/daemon/start/), [`daemon status`](https://docs.docker.com/reference/cli/sbx/daemon/status/) | Test foreground supervision and daemon/socket readiness; no undocumented private API dependency |
| SSH | [Managed SSH integration](https://docs.docker.com/ai/sandboxes/integrations/) | `sbx setup ssh`, then `ssh NAME.sbx`; requires Docker login, starts stopped guest, terminates at host daemon; no guest sshd needed |
| Recreate | [`sbx rm`](https://docs.docker.com/reference/cli/sbx/rm/) + create | Export and validate full factory data before removal; retain old instance until new one is accepted |
| Reset | [`sbx reset`](https://docs.docker.com/reference/cli/sbx/reset/) | Never map `ssf vm reset` to this global destructive operation |
| Health/logs | [`sbx diagnose`](https://docs.docker.com/reference/cli/sbx/diagnose/), `exec` | VM running is not factory healthy; check supervisor, herdr endpoint, SSF IPC and fresh heartbeat; keep rotating guest logs |

SSH ignores client environment forwarding, and selects the image's default user.
SSF currently expects `ssf`, explicit guest variables and its own SSH admin key.
Use an argv-based `exec` transport initially; validate terminal and signal handling
before preserving `attach` and `ssh-config` behavior. Do not disable SSH host-key
checking to accommodate the new transport.

[Headless automation](https://docs.docker.com/ai/sandboxes/workflows/automation/)
documents Docker PAT login via stdin and background command execution. Initial
credential enrollment is distinct from an unattended reboot. A persistent Linux
service needs a dedicated OS user, stable runtime/state paths, an available user
manager after logout (including linger where appropriate), and an explicit hold
on expired Docker login. macOS needs equivalent launchd/session testing; Windows
needs service/task and login testing **plus an SSF platform port**. SSF's current
Unix process/signal and platform code does not become Windows-compatible merely
because `sbx` supports Windows.

Size explicitly: `--cpus` and `--memory` avoid defaults consuming all host CPUs
and up to half its RAM. Docker documents `DOCKER_SANDBOXES_ROOT_SIZE` for the
default 20 GB root and `DOCKER_SANDBOXES_DOCKER_SIZE` for the separate default
10 GB Docker data disk in [troubleshooting](https://docs.docker.com/ai/sandboxes/troubleshooting/).
These are not proven online resize or independently recoverable SSF data volumes.
Measure actual host pressure under concurrent builds.

Proposed lifecycle: host `ssf run` supervises the selected sandbox; a versioned
guest bootstrap starts herdr and SSF exactly once, preserves the guest ownership
marker, and exposes readiness. On unknown runtime state, retry with bounds and
report unavailable; never fall back to running the factory on the host. On stop,
quiesce sessions and state before stopping the sandbox. Reboot must re-enter this
same path without `sbx run` attachment.

[Kit startup hooks](https://docs.docker.com/ai/sandboxes/customize/kit-reference/)
run non-interactively on each start, but do not gate the agent entrypoint or
promise crash supervision. Use a real supervisor and a readiness gate inside SSF's
bootstrap. Installing the current systemd units in a container image is not enough.
The heartbeat kit tests startup replay; it does not establish herdr or daemon
recovery. Kill each service, restart sandboxd, reboot/log out, and expire login in
the follow-up. Setup output is not retained by `sbx`; explicitly capture SSF/herdr
logs rather than assume a Docker-style `logs` command exists.

## Granularity

These resource and recovery comparisons are architectural expectations, not
measurements. Record cold/warm startup time, idle RSS/disk, and concurrent workload
latency on the qualification host; this guest cannot supply those numbers.

| Unit | Isolation and concurrency | Cost/startup | Recovery and fit |
|---|---|---|---|
| Whole factory (preferred) | Same shared trust boundary as today; repositories and agents share guest privileges | One VM, shared caches, warm session launch | One failure affects factory; closest to current daemon/herdr/state ownership |
| Repository | Limits cross-repository filesystem damage; credentials/policies must also be separated | VM and tool caches per repo, simultaneous CPU/RAM pressure | Repo failures contained; needs partitioned state and routing, or multiple coordinated factories |
| Agent session | Strongest session filesystem separation if no shared writable data or credential scope | VM creation per session, duplicated caches, burst resource demand | Disposable only after exporting work and resume state; requires remote worker/driver design |

Docker describes per-sandbox filesystem/daemon storage costs in its
[architecture](https://docs.docker.com/ai/sandboxes/architecture/). Template caching
is not the same as sharing mutable build caches. Splitting sessions is a separate
execution architecture, not the smallest `ssf vm` backend.

## Base template and user extension

[Templates](https://docs.docker.com/ai/sandboxes/customize/templates/) extend an
existing variant; adding executables does not change its selected agent runtime.
For a Codex-only spike, extend `codex`. For the factory, prefer a `shell`-derived
SSF image plus an SSF sandbox kit: SSF/herdr chooses each harness. Keep a plain
`shell` + explicit bootstrap as the simpler lifecycle experiment. Separate images
per harness would fragment factory compatibility and fail mixed-harness operation.

The existing set is `claude`, `codex`, `gemini`, `copilot`, `opencode`, `pi`, `omp`,
`grok`, `crush` ([catalogue](../../src/agents.rs),
[provisioning](../../vm/guest/provision.sh)). Presence of a CLI does not prove its
authentication or herdr hooks work under a custom kit. Qualify Codex first; publish
an explicit supported-harness manifest and fail when a requested harness is absent.

Proposed release build, **not a working release artifact**:

```dockerfile
# Supply a reviewed shell image reference pinned by digest.
ARG SANDBOX_BASE
FROM ${SANDBOX_BASE}
ARG TARGETARCH
USER root
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates git openssh-client curl jq util-linux supervisor \
    && rm -rf /var/lib/apt/lists/*
# CI supplies checksum-verified, architecture-matched artifacts and dependencies.
COPY artifacts/${TARGETARCH}/bin/ /usr/local/bin/
# Must contain ssf, herdr, gh and supported harness launchers; not host credentials.
COPY runtime/ /opt/ssf/
RUN install -d -o agent -g agent /var/lib/ssf /var/lib/ssf/state \
      /var/lib/ssf/projects /var/lib/ssf/home \
    && /opt/ssf/check-image
USER agent
WORKDIR /var/lib/ssf/projects
# Inherit sandbox image conventions; kit/explicit bootstrap owns service startup.
```

`runtime/` and `artifacts/` above are proposed build inputs, not checked-in or
published files. `check-image` must verify architecture, binary versions, required
harnesses/hooks, proxy CA behavior and guest bootstrap compatibility. Preserve UID
1000/default `agent` requirements; SSF must explicitly adapt its current `/home/ssf`
assumptions rather than introduce a second unconfigured default user. Keep durable
home/config at canonical paths and validate bind/symlink and ownership behavior
before importing existing factories.

Normal user extension (illustrative unpublished reference):

```dockerfile
FROM ghcr.io/mikekelly/ssf-sandbox:VERSION-shell
USER root
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*
USER agent
```

Build/push on a qualified builder; configure SSF with the resulting full registry
reference and resolved digest. Docker documents Desktop for custom-template
building, registry pulls, and `docker image save` followed by `sbx template load`
for local transfer. A host Docker image store is not automatically visible to
`sbx`. Publishing a multi-architecture manifest is the proposed CI design: native
amd64 and arm64 builds, checksums/SBOM/provenance, per-platform smoke tests, then one
version tag. Current SSF arm64 release artifacts are best effort, so this needs a
release gate. Upstream variant manifests and user-layer architecture compatibility
remain unverified; do not advertise multi-arch based on an OCI tag alone.

Proposed config, **rejected by current SSF's strict config parser**:

```toml
[vm]
enabled = true
backend = "docker-sandbox"
name = "ssf-factory"
vcpus = 4
mem_mib = 8192

[vm.sandbox]
template = "ghcr.io/example/project-factory@sha256:REPLACE_WITH_RESOLVED_DIGEST"
kit = "ghcr.io/mikekelly/ssf-kit:VERSION"
root_gib = 80
network_profile = "ssf-restricted"
```

The exact key names are subject to implementation. Store template digest, kit
revision, host CLI version, guest protocol, data schema and harness manifest in an
SSF-owned identity record. Pin kit dependencies too: kit signatures do not freeze
mutable image tags. A user extension may add tools, but must retain the bootstrap,
paths, identity checks and supported protocol. No credentials in build arguments,
layers, example configuration or images saved from a working factory.

## State, upgrades and recovery

SSF currently owns `/var/lib/ssf/{state,projects,home}`; `/home/ssf` is backed by
that durable home. It includes config, bot keys, harness auth/transcripts, herdr
sessions, caches, clones and worktrees. Worktree/session IDs and absolute paths
matter ([state model](../../src/state.rs), [VM persistence](../vm.md)).

| Data/event | Sandbox stop/start | Template change / recreation | Global reset |
|---|---|---|---|
| Mountless config, projects, home, sessions | Retained on disk | No automatic migration; export/import required | Deleted |
| Package/build caches in guest | Retained | Rebuild or explicitly restore compatible caches | Deleted |
| Host bind-mounted paths | Host-owned, writes visible immediately | Host files survive, guest environment replaced | Host workspace files remain; do not treat as isolated factory data |
| Template cache | Cached locally | Different template does not upgrade existing factory by itself | Cleared |
| Host secrets/policies/login | Outside guest; validate expiry separately | Rebind/authenticate as needed | Removed by default; preserve-secrets option is not data recovery |

Sources: [usage/persistence](https://docs.docker.com/ai/sandboxes/usage/),
[reset scope](https://docs.docker.com/reference/cli/sbx/reset/).
Kit block volumes can place data at explicit paths, but documented local APIs do
not establish detachable, independently retained factory disks.
[`sbx volume`](https://docs.docker.com/reference/cli/sbx/volume/) is cloud-only;
do not confuse it with local kit volumes. Test removal, attachment and backup
semantics before promising SSF's existing reset guarantee.

Proposed safe upgrade is replacement with a cold export: stop factory writers,
export full data with ownership/links/modes and a manifest, verify checksum and
restore on a **new named sandbox**, check paths and schema offline, then switch the
host pointer and enable one writer. Retain old sandbox/disks and pre-migration
backup. Test uncommitted/ignored files, linked worktrees, large repos, interrupted
export, disk full and interrupted cutover. `sbx cp` is a transport, not a guarantee
of an atomic or metadata-complete backup; use a quiesced archive and test extraction.

A backup stored only inside the sandbox does not protect against `rm`, `prune` or
`reset`. Export to a private host backup directory, never continuously mount it
writable into the agent guest. Encrypt secret-bearing backups and make retention
explicit. Do not use `template save` as a publishable factory backup: it captures
secrets and mutable state. Restoring old SSF against newly migrated state is unsafe;
rollback means restoring the old schema snapshot, accepting work since cutover may
need separate recovery.

There are two upgrade axes: SSF/template and the host `sbx` runtime. Current SSF
injects its binary on boot; the proposed template model pins it instead and requires
host/guest compatibility checks. Home hooks need versioned migrations, not repeated
seeding. Docker documents database incompatibility on runtime downgrade and a
recovery involving destructive reset in
[troubleshooting](https://docs.docker.com/ai/sandboxes/troubleshooting/).
Consequently a host package downgrade is not a reliable rollback procedure.

## Credentials and trust


| Path | Evidence and decision |
|---|---|
| Docker login | Dedicated operator-owned Docker account/PAT, distinct from GitHub bot and harness accounts. Login expiry must hold startup, not choose another account. |
| GitHub daemon/API | [Authentication workflow](https://docs.docker.com/ai/sandboxes/workflows/authentication/) supports host `gh` credential resolvers and proxy injection. SSF currently resolves real tokens and sends Bearer headers through reqwest; add a proxy credential provider, test CA/proxy handling and REST/GraphQL, and verify `gh api user` is the configured bot. |
| Guest `gh` and HTTPS Git | Exercise the SSF wrapper, `GH_CONFIG_DIR`, `GH_TOKEN` sentinel and `ssf git-credential` together. Verify clone/push and byline behavior; built-in guest `gh` success alone is insufficient. Do not register an ambient human `gh auth token` resolver. |
| SSH push and commit/tag signing | [Git workflow](https://docs.docker.com/ai/sandboxes/workflows/git/) supports forwarded host SSH agents and inline public-key signing configuration. Use only a dedicated bot agent/socket or retain a guest bot key as an explicitly weaker alternative. HTTP injection cannot sign commits or authenticate SSH. Test after logout/reboot and verify signatures on GitHub. |
| Codex ChatGPT login | [Docker's Codex guide](https://docs.docker.com/ai/sandboxes/agents/codex/) documents `sbx secret set openai --oauth` on the host, or `sbx secret set openai` for an API key. Test built-in Codex first, then herdr-launched Codex with refresh after restart. Host Codex config is not automatically imported. |
| Other harnesses | Audit every login probe and kit credential binding. SSF currently inspects credential files; a proxy sentinel must not be mistaken for either a real usable token or a logged-out session. |

Sources for current assumptions: [token configuration](../../src/config.rs),
[GitHub client](../../src/github.rs), [Git helpers/launch](../../src/main.rs),
[SSH keys](../../src/keys.rs), [login probes](../../src/login.rs).

[Credential storage](https://docs.docker.com/ai/sandboxes/configuration/credentials/)
is host-managed, but global secret defaults are not per-factory authorization.
Use explicit scope where supported and third-party-kit mechanism/domain bindings;
v0.42.1 local OpenAI OAuth is global-only, requiring a dedicated runtime account; disallow OAuth
passthrough. Missing required bindings can still let a kit start with credentials
withheld: SSF readiness must fail closed on bot/provider validation. Host proxy
resolvers must work in the service environment, and cache/refresh behavior must be
tested. Hidden token bytes do not prevent an agent from exercising the account's
API rights or exfiltrating data through an allowed service.

[Security guidance](https://docs.docker.com/ai/sandboxes/security/) identifies
writable shared skills and host MCP as additional boundaries. Disable shared
skills and generic SSH-agent forwarding in a dedicated factory runtime; install
versioned skills in its template instead. Allow only deliberately configured MCP
servers. A compromised template has guest sudo and can consume delegated account
rights. Host mounts can also expose host Git hooks, editor configuration and other
executables; preserve SSF's guest ownership model with mountless creation.

## Network policy

Use **deny-default with exact host:port grants**, scoped to the factory. Bootstrap
policy before any factory process starts. Docker's `balanced` preset permits a
broader set of common services; it is not SSF's minimum allow-list.
[Policy documentation](https://docs.docker.com/ai/sandboxes/security/policy/)
provides `sbx policy init deny-all`, scoped `allow`, `check` and connection logs.
Initialize global policy only in a dedicated runtime account; do not change the
user's unrelated sandboxes. Under organization governance, the administrator owns
effective grants; local or kit allowances cannot override organization restrictions.

This is a **candidate qualification list**, not a universal or measured allow-list:

| Purpose | Initial candidates (TCP) | Validation needed |
|---|---|---|
| GitHub API/Git | `api.github.com:443`, `github.com:443` | REST/GraphQL, bot identity, clone, push |
| Downloads/LFS/releases | `codeload.github.com:443`, `raw.githubusercontent.com:443`, `objects.githubusercontent.com:443`, `release-assets.githubusercontent.com:443`, actual LFS host | Redirects and repository-specific artifact storage |
| Codex | `api.openai.com:443`, `auth.openai.com:443`, `chatgpt.com:443` | Selected API/OAuth flow, refresh, model request; use installed kit's effective endpoints |
| Other model providers | Selected harness kit's exact API/auth hosts | No blanket wildcard covering every provider |
| npm/Python/Rust | `registry.npmjs.org:443`, `pypi.org:443`, `files.pythonhosted.org:443`, `index.crates.io:443`, `static.crates.io:443`, `static.rust-lang.org:443` | Lockfile/git dependencies and download redirects |
| OS/user tools | Actual configured apt mirror hosts/ports and tool registries | Read image sources and project requirements; prefer baked tools; approve additional hosts explicitly |
| Optional Git SSH | `github.com:22` or `ssh.github.com:443` | Dedicated bot agent and both authentication/signing tests |

Check denied private-network/metadata destinations and unexpected redirects as well
as allowed requests. Do not add broad grants simply to make a failed install pass.
The kit reference marks some CIDR/wildcard/range enforcement as pending; use tested
exact hosts/ports. Outbound non-HTTP TCP may be routed but does not receive HTTP
credential injection; UDP/ICMP limitations can break user tools. Preserve managed
proxy variables and trust bundles in daemon/herdr children; test applications that
ignore proxy settings. Network denial should produce actionable diagnostics, not
an automatic permissive retry.

Host runtime traffic is separate from guest allow-lists. Docker's
[FAQ](https://docs.docker.com/ai/sandboxes/faq/) lists login/registry/control-plane
endpoints; firewall qualification must include these and the chosen template
registry. Record effective policy and blocked destinations without recording
headers, tokens or prompt content.

## Platform, dependencies and Early Access risks

`sbx` itself does not need host Docker Engine/Desktop. Docker's supported Linux
target is Ubuntu 24.04+ with KVM; even derived distributions are not promised.
macOS support excludes Intel; Windows requires Windows Hypervisor Platform.
The archive running on this Arch guest is **not** an Arch/Omarchy support result.
The physical host was not inspected: this session is inside its factory guest,
with no nested `/dev/kvm`. Host Arch packaging, hypervisor permissions, service
startup, system updates and resource contention remain separate tests.

The v0.42.1 archive installer requires `mkfs.ext4`, bundles a VM kernel/root image,
shim, libraries and `mkfs.erofs`, and can install an AppArmor profile. We inspected
it but did not run it. A static download is not dependency-free. Keep SSF installation
independent until supported-host qualification; do not add an Arch package or
replace installed virtualization components as part of this investigation.

Docker says the CLI is free for commercial use with a required free account;
organization governance is separately paid. The downloaded `LICENSE` identifies
proprietary software under Docker's Subscription Service Agreement. Free use does
not establish redistribution rights for SSF's base dependencies. Inventory licenses
for every image/CLI/harness and have the publisher resolve distribution terms
before publishing supported artifacts. This report makes no legal interpretation.

The FAQ documents telemetry opt-out via `SBX_NO_TELEMETRY=1` and command/outcome,
duration and Docker-username collection. On headless Linux without Secret Service,
credentials fall back to permission-protected files, not encrypted storage. Account,
host-disk protection and service access therefore remain part of SSF's threat model.
Do not automatically upload diagnostics; logs can contain user content despite
redaction. See [diagnostics guidance](https://docs.docker.com/ai/sandboxes/troubleshooting/).

Templates and kits are Early Access, kit commands/schema are experimental, and
some parser-accepted features lack runtime enforcement. SSH is separately marked
GA; that does not stabilize the entire backend. Pin **tested** CLI/template/kit
combinations, retain fixture-based CLI parsing checks, and reject unknown versions
with useful diagnostics. Never auto-reset state to repair a version mismatch.

## Interfaces and migration plan

Preserve `ssf vm start/stop/restart/status`, command forwarding, `attach`, `login`
and `logs` where behavior can match. Map `build` to template validation/import or
an explicit build workflow; explain changed semantics. `ssh-config` must use the
managed sbx endpoint. Gate `grow`, `console`, image paths, `guest_binary`, `vm.files`
and reset until equivalent behavior is implemented. Retain generic sizing keys
where possible but reject unsupported resize operations. Never quietly reinterpret
`data_gib` as disposable root capacity. `sync` must not become repeated host config
seeding. Keep factory configuration and credentials guest-owned.

Incremental delivery gates (follow-up issue links below):

1. **Qualification only:** dedicated supported Linux user/host; pinned release;
   run the disposable recipe, authenticated Codex, reboot/logout and resource
   measurements. Repeat on physical Arch separately, then supported macOS/Windows.
   No SSF backend default changes. Stop if persistent operation needs an attached
   terminal or credentials cannot be constrained to the bot.
2. **Template/runtime contract:** native two-architecture build, Codex + herdr
   hooks, idempotent supervised bootstrap, readiness and proxy-provider support.
   Verify bot REST/GraphQL, wrapped `gh`, Git/signing and auth expiry. Extend the
   harness matrix deliberately. No production credentials in images.
3. **Opt-in adapter and recovery:** implement transport/capability dispatch and
   staged export/import; test crash/data-loss paths before allowing `reset` or
   migration. Preserve both current backends and default selection. Update VM/setup,
   configuration examples and setup skill when actual behavior changes.

Migration begins only after these gates. Stop the old host supervisor and guest
writers; export full factory data, retaining original disks. Import offline into
a new sandbox; verify bot identity, paths, ownership, schema, repository/worktree
links, session resume and herdr hooks. Bind explicit new host proxy credentials
rather than automatically copying all host or guest authentication. Activate exactly
one factory; two pollers can duplicate sessions and GitHub actions. Keep a rollback
snapshot and use it if new state cannot be read by the old backend. Do not destroy
Firecracker/Lima data until the owner accepts the migrated factory and backup.

## Follow-up work

- [#226: Qualify unattended operation](https://github.com/mikekelly/simple-software-factory/issues/226): supported-host experiments; no implementation prerequisite. Maintainer supplies a dedicated host/account when prioritized.
- [#227: Template, supervision and bot authentication](https://github.com/mikekelly/simple-software-factory/issues/227): depends on #226 go decision; owns image/architecture/auth compatibility.
- [#228: Opt-in backend and safe recovery](https://github.com/mikekelly/simple-software-factory/issues/228): depends on #226 and #227; owns data-loss tests, migration and user documentation.

Dependency order: **#226 → #227 → #228**. These are proposed implementation
outcomes, not prerequisites to accepting this investigation's no-go for replacement.
A missing live result is an explicit gate, not a claimed pass.
