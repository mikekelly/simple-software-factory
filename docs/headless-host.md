# VPS / headless-host installation

Use **standalone Linux binaries + host mode** on a dedicated Linux host,
VPS, or container where you want the factory to run directly, including hosts
without usable KVM or a systemd user session. This path runs herdr and the
factory as your Unix user; agents can access that user's files and credentials.
No VM, linger, `ssf setup`, or desktop integration is needed.

Choose steps from the host's actual OS, architecture, permissions, available
init/service manager, and KVM access. A hosting provider or assistant product
name does not establish these capabilities. Host mode does not require KVM,
Docker, or Podman. If you want VM isolation and the host supports it, use
[Setup](setup.md) instead.

This guide installs the factory only. If an always-on assistant on this host
should also watch SSF-tracked work and act as the user's liaison, follow the
separate [liaison guide](liaison.md) afterward; a liaison on the
user's own machine instead reaches this host over SSH, and that guide covers
that setup too. Configure the assistant's GitHub access and event delivery
separately from `ssf auth`.

These steps assume a **fresh installation**, run as the same non-root Unix
user throughout (except privileged prerequisite installation). If SSF already
exists, inspect `ssf server list`,
`ssf status` and the selected factory's config before changing it. Leave the
server catalog empty on this fresh path so `ssf` and foreground `ssf-server`
use the same default config and state. Do not set `SSF_SERVER` to another
factory or create a named local target halfway through these steps.

## 1. Install prerequisites and both SSF binaries

Install CA certificates, curl, Git, jq, GitHub CLI 2.40+, and an OpenSSH
client (`ssh-keygen` is needed for bot key enrollment) using the host's package
manager. Refresh its package indexes first on minimal images.

For example, on Debian/Ubuntu (**you** for sudo):

```sh
sudo apt update
sudo apt install ca-certificates curl git jq gh openssh-client
```

Use the equivalent packages on other distributions. On Debian 12, use
[GitHub's apt repository](https://github.com/cli/cli/blob/trunk/docs/install_linux.md)
for a recent enough gh. Installing a `systemd` package does not itself supply
a working user service manager; this standalone path does not require one.

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Follow [Download and install on Linux](install-binaries.md#download-and-install-on-linux),
then [Run a local factory without a package](install-binaries.md#run-a-local-factory-without-a-package)
to put **both `ssf` and `ssf-server` from the same release** into `~/.local/bin`.
Choose assets matching `uname -m`; these are static musl binaries. Keep the two
unversioned executables together, and persist the PATH setting for new shells.
Stop before the final daemon start there; complete authentication below first.

## 2. Install the headless driver and harness

```sh
curl -fsSL https://herdr.dev/install.sh | sh
herdr --version
ssf config set driver herdr
ssf agents
```

Choose a harness and model using
[Setup's selection guidance](setup.md#choosing-the-harness-and-the-model),
then install the chosen harness following its own instructions. For example,
OMP can be installed with `curl -fsSL https://omp.sh/install | sh`; verify it
with `omp --version`. The OMP example below is optional.
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
and doctor still reports a missing bot key. Install the host's OpenSSH client
package and rerun the handoff without `--no-keys` to complete enrollment. Keep `project` scope
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

Sign in to the selected harness and finish any one-time interactive setup as
the same Unix user and HOME that will run herdr. Use the harness's supported
credential storage or arrange persistent environment variables for both the
supervised processes and their agent panes. Verify a real request in a
herdr-launched pane; a daemon environment or passing login check alone does
not prove that a pane can authenticate.

### Example: OMP with OpenRouter

For OMP with OpenRouter, supply `OPENROUTER_API_KEY` on the host. For example,
create a private shell environment file (do not put secrets in repository config):

```sh
install -d -m700 "$HOME/.config/ssf"
(umask 077; touch "$HOME/.config/ssf/harness.env")
chmod 600 "$HOME/.config/ssf/harness.env"
```

**You:** edit that file locally to contain `export OPENROUTER_API_KEY='your-key'`.
Then load it and run one interactive OMP on the host, as the same Unix user
and HOME that will run herdr, before letting SSF spawn sessions:

```sh
. "$HOME/.config/ssf/harness.env"
omp
```

After OpenRouter authentication, finish the one-time wizard or press **Esc**
through the remaining “Setup step 1 of 5” screens to complete/skip it. A provider
row saying “OpenRouter ● logged in (api key)” means authentication is already
present; “Select provider to login” in that wizard does not call for another
`/login`. Completion persists `setupVersion` in `~/.omp/agent/config.yml`.
Then choose an OpenRouter model and verify a request.

OMP can also save provider credentials through its own login/settings UI in
`~/.omp/agent/agent.db`; missing `auth.json` alone does not mean signed out.
Do not edit the database directly. Preserve the credential file/database across
host restarts. SSF recognizes a nonempty `OPENROUTER_API_KEY` as signed in, but
that check does not prove the key is valid or has credit: the OMP request does.

A key in the host shell or `ssf-server` environment does **not** establish the
environment of herdr-launched panes. SSF does not forward that key to panes
and has no built-in loader for `omp.env` or `harness.env`; the shell commands
here load the example file explicitly. Save OpenRouter credentials through
OMP's UI in the shared `~/.omp` home, or ensure the actual pane environment
receives the key, and verify a request there. A passing `ssf doctor` or
non-interactive `omp models` does not prove interactive setup is complete.

## 6. Start both processes and watch a repository

In one persistent terminal, as the bot's Unix user:

```sh
export PATH="$HOME/.local/bin:$PATH"
# If using the environment-file example above:
# . "$HOME/.config/ssf/harness.env"
herdr server
```

This is the headless driver start; interactive `herdr` is optional. In a second
terminal, with the same user, HOME and config/state environment:

```sh
export PATH="$HOME/.local/bin:$PATH"
# If using the environment-file example above:
# . "$HOME/.config/ssf/harness.env"
ssf-server
```

Keep both processes running. Foreground logs appear in their terminals; Ctrl-C
stops them. For unattended operation use the host's process supervisor with
these commands, the same user/PATH, and any required credential environment
loaded **on every restart for both processes**. A variable exported only in your setup shell
will not reach a later daemon or herdr launch. Stop the SSF daemon before
replacing either binary; restart the driver and daemon with credentials loaded.

In a third terminal, load the same PATH and any required credential environment,
then configure the repository. Replace OWNER/NAME, HARNESS, MODEL and EFFORT
with the human operator’s choices:

```sh
ssf models HARNESS
ssf repo add OWNER/NAME --harness HARNESS --model MODEL --effort EFFORT
ssf auth status
ssf status
ssf doctor
```

Include `--effort` only if the selected harness supports it.

By default, collaborators with push access may drive the factory; see
[allowed users](setup.md#5-who-may-drive-the-factory) for an explicit allowlist.
Doctor should confirm the bot identity/key, herdr readiness, harness login,
repository access and SSF guidance. Before the first issue, a missing checkout
or agent command links can remain; those are created when an agent starts.
Check that the daemon is answering, not merely that doctor exits successfully.

Assign a small issue to the bot (or @mention it), then run `ssf status` and
`ssf doctor` again. Within a poll interval (10 seconds by default), expect a
workspace and an agent that comments on the issue. `ssf dashboard` provides a
terminal view. Resolve remaining doctor failures;
for 404s recheck acceptance, and for board moves recheck board Write and token
scope separately. This completes a watching host factory without systemd.
