# Grok Bot / headless-host installation

Use **standalone Linux binaries + host mode** on a Grok Bot computer or a
stripped Debian container without usable KVM or a systemd user session. This
path runs herdr and the factory directly as your Unix user; agents can access
that user's files and credentials. No VM, linger, `ssf setup`, or Omarchy
widget is needed.

The [September 2026 install report](https://github.com/mikekelly/simple-software-factory/issues/287)
observed Debian 13, `/.dockerenv`, PID 1 `tini`, and no usable KVM for `box`.
Use what the computer actually exposes when choosing installation steps;
public descriptions of Firecracker infrastructure do not imply nested KVM or
systemd inside it. A plan-tier change does not change this computer shape.
Do not expect a nested Docker Engine or `/var/run/docker.sock`. Rootless
Podman can work after installation if the host permits it; it is a separate
runtime, not Docker-in-Docker, and is not required for SSF host mode.

These steps assume a **fresh installation**, run as the same non-root Unix
user throughout (except apt). If SSF already exists, inspect `ssf server list`,
`ssf status` and the selected factory's config before changing it. Leave the
server catalog empty on this fresh path so `ssf` and foreground `ssf-server`
use the same default config and state. Do not set `SSF_SERVER` to another
factory or create a named local target halfway through these steps.

## 1. Install prerequisites and both SSF binaries

On Debian/Ubuntu (**you** for sudo; a root provisioning shell can omit sudo):

```sh
sudo apt update
sudo apt install ca-certificates curl git jq gh openssh-client
export PATH="$HOME/.local/bin:$PATH"
```

Refresh apt lists first: minimal images may have stale or absent indexes.
`openssh-client` supplies `ssh-keygen` for bot key enrollment. Use gh 2.40+
(on Debian 12 use [GitHub's apt repository](https://github.com/cli/cli/blob/trunk/docs/install_linux.md)).
The SSF `.deb` has a hard `Depends: systemd`; resolving it with apt does not
make systemd the container's init or supply a working user service manager.

Follow [Download and install on Linux](install-binaries.md#download-and-install-on-linux),
then [Run a local factory without a package](install-binaries.md#run-a-local-factory-without-a-package)
to put **both `ssf` and `ssf-server` from the same release** into `~/.local/bin`.
Choose assets matching `uname -m`; these are static musl binaries. Keep the two
unversioned executables together, and persist the PATH setting for new shells.
Stop before the final daemon start there; complete authentication below first.

## 2. Install the headless driver and harness

```sh
curl -fsSL https://herdr.dev/install.sh | sh
curl -fsSL https://omp.sh/install | sh
herdr --version
omp --version
ssf config set driver herdr
ssf agents
```

OMP is one example; choose a harness and model using
[Setup's selection guidance](setup.md#choosing-the-harness-and-the-model).
Install your repository's build tools separately on this host.

## 3. Sign in as the bot

**You:** create or choose a separate bot GitHub account as described in
[Setup step 3](setup.md#3-create-the-bot-account). Approve the device code in a
browser signed in as that bot. On this dedicated bot host, use gh's device
flow directly, then hand its token to SSF without printing it:

```sh
BROWSER=true gh auth login --hostname github.com --web --git-protocol https \
  --scopes repo,project,admin:public_key,admin:ssh_signing_key
gh api user --jq .login            # must be BOT
# Replace BOT with that login; SSF verifies the token belongs to it.
gh auth token --hostname github.com --user BOT | ssf auth login --token --user BOT
ssf auth status
```

Check that `GH_TOKEN` / `GITHUB_TOKEN` are not overriding the intended bot
account. The [gh login manual](https://cli.github.com/manual/gh_auth_login)
describes device login and credential storage. SSF stores the handed-off token
in `~/.config/ssf/token` (0600) and enrolls its own SSH and signing key.

**Older-gh footgun:** `ssf auth login --web` passes gh's `--skip-ssh-key`, which
was unsupported in the reported gh 2.46 install. Account pickup also failed
there after browser login. The direct flow above avoids both. SSF's `--no-keys`
is a different option: add it to the **token handoff** only as a temporary
workaround if OpenSSH is unavailable. It leaves commits unsigned / HTTPS-only,
and doctor still reports a missing bot key. Install `openssh-client` and rerun
the handoff without `--no-keys` to complete enrollment. Keep `project` scope
for board operations even if temporarily skipping keys.

## 4. Complete repository and board access

Do all four before configuring the watch:

1. **Repository owner, on their own computer:** invite BOT with **Write** on
   every watched repository (`gh api repos/OWNER/NAME/collaborators/BOT -X PUT
   -f permission=push`). The bot should never borrow the owner's credentials.
2. **Bot, on this host:** list and accept the intended invitation:

   ```sh
   gh api user/repository_invitations --jq '.[] | {id, repository: .repository.full_name}'
   gh api user/repository_invitations/ID -X PATCH
   gh api repos/OWNER/NAME --jq '{repository: .full_name, push: .permissions.push}'
   ```

   Replace ID with the matching invitation ID; expect `push: true`. A pending
   invitation can cause a private repo to return 404, looking like “repo missing”.
3. **Project owner:** for Projects (v2), grant BOT **Write** under the board's
   **Settings → Manage access**. Repository Write alone is insufficient.
4. **Repository owner:** adapt [SSF.example.md](../SSF.example.md) and commit
   it as root-level `SSF.md` on the **default branch**. Doctor reads it through
   GitHub; a local file or unmerged feature branch does not satisfy the check.

See [Setup's access steps](setup.md#3-create-the-bot-account) and
[guidance and boards](setup.md#9-ssf-agent-guidance-and-boards) for details.

## 5. Give the harness persistent credentials

For OMP with OpenRouter, supply `OPENROUTER_API_KEY` on the host. For example,
create a private shell environment file (do not put secrets in repository config):

```sh
install -d -m700 "$HOME/.config/ssf"
(umask 077; touch "$HOME/.config/ssf/harness.env")
chmod 600 "$HOME/.config/ssf/harness.env"
```

**You:** edit that file locally to contain `export OPENROUTER_API_KEY='your-key'`.
Then load it and run OMP once to choose an OpenRouter model and verify a request:

```sh
. "$HOME/.config/ssf/harness.env"
omp
```

OMP can also save provider credentials through its own login/settings UI in
`~/.omp/agent/agent.db`; missing `auth.json` alone does not mean signed out.
Do not edit the database directly. Preserve the credential file/database across
host restarts. SSF recognizes a nonempty `OPENROUTER_API_KEY` as signed in, but
that check does not prove the key is valid or has credit: the OMP request does.

## 6. Start both processes and watch a repository

In one persistent terminal, as the bot's Unix user:

```sh
export PATH="$HOME/.local/bin:$PATH"
. "$HOME/.config/ssf/harness.env"
herdr server
```

This is the headless driver start; interactive `herdr` is optional. In a second
terminal, with the same user, HOME and config/state environment:

```sh
export PATH="$HOME/.local/bin:$PATH"
. "$HOME/.config/ssf/harness.env"
ssf-server
```

Keep both processes running. Foreground logs appear in their terminals; Ctrl-C
stops them. For unattended operation use the host's process supervisor with
these commands, the same user/PATH, and the environment file loaded **on every
restart for both processes**. A variable exported only in your setup shell
will not reach a later daemon or herdr launch. Stop the SSF daemon before
replacing either binary; restart the driver and daemon with credentials loaded.

In a third terminal, load the same PATH and environment file, then configure
the repository (replace OWNER/NAME and MODEL with your selected values):

```sh
ssf models omp
ssf repo add OWNER/NAME --harness omp --model MODEL
ssf auth status
ssf status
ssf doctor
```

By default, collaborators with push access may drive the factory; see
[allowed users](setup.md#5-who-may-drive-the-factory) for an explicit allowlist.
Doctor should confirm the bot identity/key, herdr readiness, harness login,
repository access and SSF guidance. Before the first issue, a missing checkout
or agent command links can remain; those are created when an agent starts.
Check that the daemon is answering, not merely that doctor exits successfully.

Assign a small issue to the bot (or @mention it), then run `ssf status` and
`ssf doctor` again. Within a poll interval (10 seconds by default), expect a
workspace and an agent that comments on the issue. `ssf dashboard` provides a
terminal view without a desktop widget. Resolve remaining doctor failures;
for 404s recheck acceptance, and for board moves recheck board Write and token
scope separately. This completes a watching host factory without systemd.
