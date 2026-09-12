# Disposable Docker Sandboxes probe (#225)

Companion to the [feasibility report](../../plans/docker-sandboxes.md).
These are research fixtures, not SSF installation scripts. Read
[development isolation guidance](../../development.md) before substituting a real
SSF daemon. The heartbeat deliberately has no repositories, credentials, driver
connections or GitHub polling.

## Observed on 2026-09-12

| Check | Actual result |
|---|---|
| Environment | Existing SSF guest, Arch Linux image 2026.09.01, x86_64, kernel 6.1.182; `systemd-detect-virt` returned `kvm` |
| Nested virtualization | `/dev/kvm` absent; no changes made to host virtualization |
| Preinstalled tools | `sbx` and `docker` absent; `systemctl` available |
| Download | Official [v0.42.1 release](https://github.com/docker/sbx-releases/releases/tag/v0.42.1), published 2026-09-07; Linux amd64 tarball |
| SHA256 | `fe46facba420d1cb8b1dad57d5b182d6df9dadd46c324c2ca3ef574fb7eada6f` (observed download hash, not independently verified publisher attestation) |
| CLI version | `sbx version: v0.42.1 cc6e400a4a3ce3ce5e0b2b77b8ee352aac854c64` |
| CLI help | `create shell`, `exec`, `login`, `setup`, `secret set` help executed successfully |
| Kit validation | `sbx kit validate ./docs/experiments/docker-sandboxes/heartbeat` exited 0: VALID |
| Headless create | Dedicated temporary OS user `ssf-sbx-225`, stdin `/dev/null`, 30s timeout; exit 1, `ERROR: Not authenticated to Docker`, `Sign in with: sbx login` |
| Extra settings probe | `sbx settings list --no-trunc` unexpectedly tried to start sandboxd under the session user; exited with missing `io.containerd.transfer.v1` plugin. No `sbx`/`sandboxd` process remained. Archive had not been installed; this is not evidence of an Arch-specific defect. |
| Runtime/authentication | **Not demonstrated**: create never reached VM boot; Docker account unavailable to the isolated user and nested KVM absent |
| Extension/SSH/persistence | **Not executed**: no running sandbox, builder or authorized Docker account |

The temporary OS account was removed after confirming it had no processes. Extracted
archive and probe-home files remain under `/tmp/ssf-225-sbx-probe`; the failed extra
settings probe wrote Docker runtime diagnostics under the session user's XDG state.
No installer, `ssf vm` command, production daemon, reset, or credential import was
run. No credentials were supplied or exported. The production Firecracker VM/data
were not replaced or stopped. CLI flags and local validation do not prove guest
execution, service supervision, platform support, or authentication.

## Reproduce the completed checks

Run from the repository root. Download/extract to a new scratch directory; do not
run the bundled installer on the factory host. The version is intentionally pinned.

```bash
probe_dir=$(mktemp -d /tmp/ssf-225-cli.XXXXXX)
curl -fL https://github.com/docker/sbx-releases/releases/download/v0.42.1/DockerSandboxes-linux-amd64.tar.gz \
  -o "$probe_dir/release.tar.gz"
sha256sum "$probe_dir/release.tar.gz"
tar -xzf "$probe_dir/release.tar.gz" -C "$probe_dir"
"$probe_dir/docker-sbx/sbx" version
"$probe_dir/docker-sbx/sbx" create shell --help
"$probe_dir/docker-sbx/sbx" exec --help
"$probe_dir/docker-sbx/sbx" login --help
"$probe_dir/docker-sbx/sbx" kit validate ./docs/experiments/docker-sandboxes/heartbeat
uname -srmo
cat /etc/os-release
systemd-detect-virt
ls -l /dev/kvm
```

The exact failed creation command, run with `sudo -H -u ssf-sbx-225` under a newly
created account with home `/tmp/ssf-225-sbx-probe/home`, was:

```bash
timeout 30 /tmp/ssf-225-sbx-probe/docker-sbx/sbx create \
  --name ssf-225-probe --cpus 1 --memory 1g \
  --template docker.io/docker/sandbox-templates:shell shell </dev/null
```

Do not reproduce the extra settings probe under a production account: apparently
informational CLI commands can start services. Use a dedicated OS user for further
runtime inspection as well as mutations.

## Pending supported-host experiment

Owner/gate: [#226](https://github.com/mikekelly/simple-software-factory/issues/226).
Use a spare supported Ubuntu/KVM machine or dedicated test host/account, with no
production sandboxes or factory credentials. Install v0.42.1 according to
[Docker installation](https://docs.docker.com/ai/sandboxes/install/), record package
and kernel versions, and verify KVM access. macOS and Windows need their own runs;
these Bash commands are the Linux recipe. Do not use cloud mode as a substitute for
local VM results.

Perform dedicated Docker account enrollment first. Docker's
[automation guide](https://docs.docker.com/ai/sandboxes/workflows/automation/)
supports `sbx login --username ACCOUNT --password-stdin` from a protected PAT
source; never place the PAT in this document, command arguments or captured logs.
No browser or credentials were available for this step in the completed probe.

In the **dedicated runtime account only**, configure policy and disable ambient
SSH-agent access before creation:

```bash
export SBX_NO_TELEMETRY=1
sbx version
sbx daemon start --detach --policy deny-all
sbx settings set ssh.agentForwardingEnabled false
sbx daemon restart
sbx daemon status --json
```

No guest network is needed for the heartbeat. Runtime template pulls need host
registry connectivity. Check the installed CLI supports `--no-share-skills` at
creation; [shared skills documentation](https://docs.docker.com/ai/sandboxes/workflows/agent-skills/)
explains why this must be chosen at creation. Stop and record a version/API gap if
any documented flag is rejected, rather than silently sharing host state.

Build the extension on a qualified Docker builder, from this directory. These
steps were not executed. Record the resolved upstream digest; for repeat runs,
replace the Dockerfile's mutable FROM with that digest.

```bash
docker build -f Dockerfile.probe -t ssf-225-probe:v1 .
docker image inspect ssf-225-probe:v1 --format '{{.Id}} {{.Architecture}}'
docker image save ssf-225-probe:v1 -o /tmp/ssf-225-template.tar
sbx template load /tmp/ssf-225-template.tar
sbx kit validate ./heartbeat
probe_name="ssf-225-heartbeat-$(date -u +%Y%m%d%H%M%S)-$$"
sbx ls --json
# No workspace path, no attached session, no live factory files.
sbx create --name "$probe_name" --cpus 1 --memory 1g \
  --no-share-skills --template ssf-225-probe:v1 --kit ./heartbeat shell </dev/null
sbx exec "$probe_name" cat /etc/ssf-probe-template
sbx exec "$probe_name" jq --version
sbx exec "$probe_name" bash -c 'sleep 5; test -s /home/agent/ssf-probe/heartbeat.log'
sbx exec "$probe_name" bash -c 'printf "persistent-marker-v1\n" > /home/agent/ssf-probe/marker'
sbx exec "$probe_name" tail -n 3 /home/agent/ssf-probe/heartbeat.log
sbx stop "$probe_name"
sbx ls --json
# exec is documented to restart stopped sandboxes without an attached agent.
sbx exec "$probe_name" cat /home/agent/ssf-probe/marker
sbx exec "$probe_name" bash -c 'sleep 5; tail -n 3 /home/agent/ssf-probe/heartbeat.log'
sbx setup ssh
ssh -o BatchMode=yes "$probe_name.sbx" 'cat /etc/ssf-probe-template'
```

Require the same marker and new post-restart timestamps, a single lock-holding
heartbeat, successful client exit, and correct JSON identity/status. Record exit
codes and timings. Repeat after sandboxd restart and host reboot/logout; distinguish
disk persistence from automatic service replay. Inspect process counts in the guest
and confirm zero host workspace mounts. The kit uses `flock` to avoid duplicate
writers; it is a representative long-lived process, **not a production supervisor**.
Its schema is based on [kit reference](https://docs.docker.com/ai/sandboxes/customize/kit-reference/).

Copy only the harmless evidence out, then remove only the explicit disposable name
after recording results. If any test fails, preserve that sandbox for diagnosis;
never use a global reset/prune for cleanup.

```bash
sbx cp "$probe_name:/home/agent/ssf-probe/" /tmp/ssf-225-heartbeat-evidence/
sbx stop "$probe_name"
sbx rm "$probe_name"
```

## Pending Codex authentication test

Use the same dedicated runtime account. The tested CLI help says local OpenAI
OAuth enrollment is **global-only**; do not claim per-sandbox OAuth isolation.
[Docker's Codex documentation](https://docs.docker.com/ai/sandboxes/agents/codex/)
provides this host enrollment path:

```bash
sbx secret set openai --oauth
```

This needs an authorized operator/browser once. The API-key alternative is
`sbx secret set openai` using its hidden input prompt. Do not import ambient provider
keys. Neither path was executed during this investigation.

Then create a second mountless sandbox using the built-in Codex variant and the
required scoped network grants. Record the template digest and actual Codex version.
Keep auth output private; publish only success/failure and sanitized evidence.

```bash
auth_name="ssf-225-codex-$(date -u +%Y%m%d%H%M%S)-$$"
sbx create --name "$auth_name" --cpus 2 --memory 4g --no-share-skills \
  --template docker.io/docker/sandbox-templates:codex codex </dev/null
sbx policy allow network --sandbox "$auth_name" "api.openai.com:443,auth.openai.com:443,chatgpt.com:443"
sbx policy check network --sandbox "$auth_name" "api.openai.com:443"
sbx exec "$auth_name" codex --version
sbx exec "$auth_name" codex exec --skip-git-repo-check 'Reply with exactly AUTH_PROBE_OK. Do not use tools.'
sbx stop "$auth_name"
sbx exec "$auth_name" codex exec --skip-git-repo-check 'Reply with exactly AUTH_PROBE_OK. Do not use tools.'
```

Require successful provider requests, restart and token refresh without guest
credential copying. Inspect sentinel/credential handling with a redacting checker;
never print `auth.json`, environment dumps or secret-store content. Repeat through
herdr in the proposed SSF image; the built-in Codex test does not prove custom-kit
credential binding or SSF login probes. Withhold/expire a binding and require an
explicit authentication hold. Stop/remove only `auth_name` after success.

## Required result record

Record each as PASS, FAIL or NOT RUN with its exact error: headless create/start,
client disconnect, heartbeat restart/reboot, filesystem marker, extension marker,
exec exit-code/signal handling, SSH, Codex request/refresh, herdr/SSF readiness,
negative network tests, resource sizing, cold/warm startup, concurrency, export and
restore. Include OS/architecture, CLI hash/version, kit revision and all image
digests. No real factory migration is part of this experiment.
