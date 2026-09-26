# Use an assistant as the SSF liaison

A liaison drives the factory on a person's behalf: the factory delivers the
coding work, and the liaison watches relevant GitHub activity, summarizes
changes, and helps the user decide what to ask the factory to do. Any assistant
with suitable repository access and a supported way to receive events or
schedule checks can fill this role.

## Where the liaison runs

A liaison is either on the same host as the factory or on the user's machine,
and that decides what it needs before it can do anything for them:

| | On the factory host | On the user's machine |
|---|---|---|
| Example | A bot-account assistant running on the same rented host as `ssf-server` | A desktop assistant watching a factory on another machine |
| Factory commands | `ssf` on this machine is the whole setup: no catalog entry, no SSH | `ssf` on the liaison machine drives the factory over SSH, so it needs the client, a key and `ssf-server` on the far side, see [Reach the factory over SSH](#reach-the-factory-over-ssh) |
| herdr | The herdr server the daemon drives is on this machine, so `herdr` and `ssf dashboard` inspect it directly | The factory's herdr server is saved in the liaison machine's herdr, see [Inspect the factory's herdr server](#inspect-the-factorys-herdr-server) |
| GitHub | The liaison's own integration or account, never the factory bot's credentials | The same, on the liaison machine, alongside the SSH access it needs |

Complete the factory first: [Install](install.md), or
[Rented hosts](platform-specifics.md#rented-hosts) when it runs on a machine
the person rents. Then set up the side the liaison runs on (the "Setup"
sections below), and configure its monitoring last.

## Keep factory and liaison access separate

A working factory does **not** establish the liaison's GitHub access or event
subscriptions. Configure each connection for its own purpose:

| Connection | Identity to use | Purpose |
|---|---|---|
| `gh` plus `ssf auth` on the factory host | The dedicated factory bot account | Lets SSF discover assigned work and lets its agent sessions comment, commit, and open pull requests as the bot |
| The assistant's GitHub integration | An account authorized by the user for the watched repositories | Lets the liaison inspect activity and, where supported, receive events |

The user's account gives the liaison the same repository view as the user; a
separate liaison account can be narrower. Choose deliberately. Never hand the
user's credentials to the factory bot, and do not assume a host `gh` sign-in
enrolls a hosted assistant integration. Some platforms configure event
delivery separately from the plugin the assistant uses to read GitHub during
a run; check both.

There is no required mapping between assistants and repositories: one
conversation per repository, one liaison for several related repositories, or
separate liaisons per responsibility all work. Choose boundaries that keep
history understandable and event volume manageable.

## Configure and test monitoring

Use the assistant's supported repository events or scheduled checks. A sample
instruction is:

> Watch the supported GitHub events on `OWNER/REPO` that need my attention.
> Inspect the current issue or pull request, summarize what changed with links,
> explain whether SSF is already handling it, and tell me the next decision
> needed. Do not comment, merge, close, assign, or change project fields without
> my approval.

Replace OWNER/REPO and set the liaison's action authority to match the user's
instructions. Narrow the event selection to activity that is useful; broad
listeners create noise. Test with a real matching GitHub event and check the
run history: a manual run alone does not prove a subscription works.

Check the platform's supported events. Do not assume issue comments,
assignments, labels, edits and Projects v2 board moves are all covered by a
repository listener; use scheduled checks for important activity that is not.
SSF's own repository polling continues independently, and a missed liaison
event does not transfer the factory bot's responsibilities to the liaison.

## Setup: a liaison on the factory host

Nothing is added for the factory side: the `ssf` client on the host is the
whole setup, with no catalog entry and no SSH, and `herdr` and `ssf
dashboard` inspect the factory's panes directly. What the liaison does need
is GitHub access of its own, configured through its platform, never the
bot's `ssf auth` credential. Every command it runs acts as the factory's
Unix user when it shares that account, so the same line applies as over
SSH: read-only commands (`ssf status`, `ssf doctor`, `ssf peers`, `ssf
dashboard`) are safe to run while watching; `ssf release`, `ssf purge`,
`ssf uninstall`, `ssf repo` and `ssf config` are the user's decisions.
Enrolment on a bot-account assistant platform is in
[Platform specifics](platform-specifics.md#liaison-on-a-bot-account-assistant).

## Setup: a liaison on the user's machine, factory elsewhere

The factory host needs nothing new: it already runs `ssf-server`. The liaison
machine is set up in two parts: reaching the factory for `ssf` commands, and
saving its herdr server so the agents' panes can be inspected. Without both, a
remote liaison can see GitHub but not the factory.

### Reach the factory over SSH

1. **A key for the factory account**, the Unix user that runs `herdr` and
   `ssf-server`. Put the liaison's public key in its `~/.ssh/authorized_keys`,
   or use the connection the assistant platform offers. No passphrase prompt
   may appear: every factory command is a non-interactive `ssh`, so load a
   passphrase-protected key first (`ssh-add`). Never copy the bot's token or
   SSH key, or the user's credentials, to the liaison machine.

2. **`ssf-server` on that account's non-interactive SSH PATH**, then check the
   connection before configuring anything else:

   ```sh
   ssh user@factory.example 'command -v ssf-server'
   ```

3. **The client on the liaison machine**, if it has none: see
   [Client only, driving a factory elsewhere](install.md#34-client-only-driving-a-factory-elsewhere).
   Driving a remote factory needs neither a local daemon nor `ssf setup`.

4. **A name for the factory**, so the destination is written once:

   ```sh
   ssf server add factory --ssh user@factory.example
   ssf --server factory status
   ssf --server factory doctor
   ```

   `ssf server add` writes the client-side catalog
   (`~/.config/ssf/servers.toml`); `ssf skill operate` covers a client with
   several targets. A raw destination selects a factory only on a client with
   no catalog file at all: there, `ssf --server user@factory.example <command>`
   and `SSF_SERVER=user@factory.example` reach the same factory.

Every remote command runs as the factory account, so the same line applies as
on the host: read-only commands are safe while watching, the rest are the
user's decisions.

### Inspect the factory's herdr server

Driving the factory over SSH says nothing about the panes its agents work in:
the herdr server, its sessions and its panes stay on the factory host. Install
herdr on the liaison machine (`curl -fsSL https://herdr.dev/install.sh | sh`),
then save that server once and it sits beside Local in the sidebar, switchable
like any other machine:

```sh
herdr machine add user@factory.example --label factory
herdr machine list
```

Run `machine add` in an interactive terminal. It checks the binary and the
running server at the other end, and can offer to install or update the remote
package, or to stop and replace a running server whose version it cannot work
with. **Answer No to replacing the factory's running server unless the user
asks for it**: those panes are live agent sessions, and a version difference
alone is not a reason to stop them. Once saved, the machine reconnects in the
background. The saved label is for the sidebar only: `herdr --remote` takes
the SSH target, so attaching is `herdr --remote user@factory.example`, which
is also the command to run when herdr reports a machine needs attention. A
host alias in `~/.ssh/config` shortens both.

Workspace, pane and agent ids are scoped to one server, so a herdr command run
locally does not reach the factory's panes: switch machines in the TUI, or run
the command on the factory host over SSH. `ssf dashboard` watches the
factory's items from anywhere, but its pane focus is scoped to the herdr
server it runs on.

A VM factory reached from its own host already works this way: `ssf vm
ssh-config` prints the `~/.ssh/config` entry that `herdr --remote ssf-default`
uses.

## Platform enrolment

Connecting the assistant's own GitHub access and creating its event routine
are platform steps, not ssf steps. For a bot-account assistant see
[Liaison on a bot-account assistant](platform-specifics.md#liaison-on-a-bot-account-assistant);
on any other platform use its equivalent controls, and verify the current UI
and supported event choices before configuring monitoring.
