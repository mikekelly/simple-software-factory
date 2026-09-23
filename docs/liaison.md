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
| Example | Grok Bot, or Hermes/OpenClaw, running on the same VPS as `ssf-server` | A desktop assistant watching a factory that runs on a VPS |
| Factory commands | `ssf` on this machine is the whole setup: no catalog entry, no SSH | `ssf` on the liaison machine drives the factory over SSH, so it needs the client, a key and `ssf-server` on the far side — see [Reach the factory over SSH](#reach-the-factory-over-ssh) |
| herdr | The herdr server the daemon drives is on this machine, so `herdr` and `ssf dashboard` inspect it directly | The factory's herdr server is saved in the liaison machine's herdr — see [Inspect the factory's herdr server](#inspect-the-factorys-herdr-server) |
| GitHub | The liaison's own integration or account, never the factory bot's credentials | The same, on the liaison machine, alongside the SSH access it needs |

Complete the factory first: [Install](install.md) (the rented-host path is
in [Platform specifics](platform-specifics.md)). Then set up the side the liaison
runs on (the "Setup" sections below), and configure its monitoring last.

## Keep factory and liaison access separate

A working factory does **not** establish the liaison's GitHub access or event
subscriptions. Configure each connection for its own purpose:

| Connection | Identity to use | Purpose |
|---|---|---|
| `gh` plus `ssf auth` on the factory host | The dedicated factory bot account | Lets SSF discover assigned work and lets its agent sessions comment, commit, and open pull requests as the bot |
| The assistant's GitHub integration | An account authorized by the user for the watched repositories | Lets the liaison inspect activity and, where supported, receive events |

The user's account can give the liaison the same repository view as the user;
a separate liaison account can provide narrower access. Choose the identity
and permissions deliberately. Never hand the user's credentials to the factory
bot or assume that a host `gh` sign-in enrolls a hosted assistant integration.

Some assistants configure event delivery separately from plugins used to read
or change GitHub during a run. Check both connections if applicable.

## Choose the liaison granularity

There is no SSF-required mapping between assistants and repositories. Choose
one conversation per repository, one liaison for several related repositories,
or separate liaisons for distinct responsibilities within a repository.
Use boundaries that keep history understandable and event volume manageable.

## Configure and test monitoring

Use the assistant's supported repository events or scheduled checks. A sample
instruction is:

> Watch the supported GitHub events on `OWNER/REPO` that need my attention.
> Inspect the current issue or pull request, summarize what changed with links,
> explain whether SSF is already handling it, and tell me the next decision
> needed. Do not comment, merge, close, assign, or change project fields without
> my approval.

Replace OWNER/REPO and set the liaison's action authority to match the user's
instructions. Select useful events, enable the listener or schedule, and test
with a safe matching GitHub event. Check run history to confirm delivery; a
manual run alone does not prove that an event subscription works.

Check the platform's current supported events and permissions. Do not assume
that issue comments, assignments, labels, edits, or Projects v2 board moves are
all covered by a repository listener. Use manual or scheduled checks for
important activity that is not supported. SSF's repository polling continues
independently; a missed liaison event does not transfer the factory bot's
identity or responsibilities to the liaison.

## Setup: a liaison on the factory host (Grok Bot, Hermes, OpenClaw)

Nothing is added for the factory side: the `ssf` client on the host is the
whole setup, with no catalog entry and no SSH, and `herdr` and `ssf
dashboard` inspect the factory's panes directly. What the liaison does need
is GitHub access of its own, configured through its platform, never the
bot's `ssf auth` credential. Every command it runs acts as the factory's
Unix user when it shares that account, so the same line applies as over
SSH: read-only commands (`ssf status`, `ssf doctor`, `ssf peers`, `ssf
dashboard`) are safe to run while watching; `ssf release`, `ssf purge`,
`ssf uninstall`, `ssf repo` and `ssf config` are the user's decisions.
Platform-specific enrolment is in its own section below.

## Setup: a liaison on the user's machine, factory elsewhere

The factory host needs nothing new: it already runs `ssf-server` for its
own agent sessions. The liaison machine is the one set up, in two parts:
reaching the factory for `ssf` commands, and saving its herdr server so the
agents' panes can be inspected. A remote liaison cannot work from GitHub
alone; without these it cannot see the factory's sessions or ask it to do
anything.

### Reach the factory over SSH

1. **A key for the factory account.** That account is the Unix user that runs
   `herdr` and `ssf-server`, because an SSH target runs `ssf-server` at the
   other end. Put the liaison's public key in its `~/.ssh/authorized_keys` (or
   use the connection the assistant platform offers), and make sure no
   passphrase prompt can appear: every factory command is a non-interactive
   `ssh`, so a passphrase-protected key has to be loaded first (`ssh-add`).
   Never copy the bot's token, the bot's SSH key or the user's credentials to
   the liaison machine.

2. **`ssf-server` on that account's non-interactive SSH PATH**, then check the
   connection before configuring anything else:

   ```sh
   ssh user@factory.example 'command -v ssf-server'
   ```

3. **The client on the liaison machine**, if it has none: the Linux package, or
   the [client-only install](install.md).
   Driving a remote factory needs neither a local daemon nor `ssf setup`. Where
   no client build runs on that machine — macOS is not supported yet — run
   `ssf` on the factory host over SSH instead.

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

Every remote command runs as the factory account, so it can change or remove
that factory's workspaces: `ssf release`, `ssf purge`, `ssf uninstall`, `ssf
repo` and `ssf config` are the user's decisions, not the liaison's. `ssf
status`, `ssf doctor`, `ssf peers`, `ssf dashboard` and the other read-only
commands are safe to run while watching.

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

Run `machine add` in an interactive terminal. It checks the installed binary
and the running server at the other end, and can ask to install or update the
remote package, or to stop and replace a running server whose version it cannot
work with. **Answer No to replacing the factory's running server unless the
user asks for it**: on a factory host those panes are the live agent sessions.
A version difference between the liaison's client and the factory's server is
not a reason to stop it. ssf starts a session again after its terminal
disappears, but the interruption is still the user's call. Once saved, the
machine reconnects in the background. The saved label is for the sidebar:
`herdr --remote` takes the SSH target, not the label, so attaching is
`herdr --remote user@factory.example`, and that is also the command to run when
herdr reports that a machine needs attention. A host alias in `~/.ssh/config`
gives a shorter target that works in both commands.

Herdr commands act on the session their own pane inherited, and workspace,
pane and agent ids are scoped to one server, so a command run locally does not
reach the factory's panes: switch machines in the TUI to look at them, or run
the command on the factory host over SSH. `ssf dashboard` watches the
factory's items from anywhere, but its pane focus is scoped to the herdr server
it runs on, so run the dashboard on the factory host when that focus is wanted.

A VM factory reached from its own host already works this way: `ssf vm
ssh-config` prints the `~/.ssh/config` entry that `herdr --remote ssf-default`
uses.

## Setup: Grok Bot on Cursor

The following enrollment and routine controls are specific to Grok Bot on
Cursor, and assume a liaison on the factory host. Use your assistant's
equivalent controls on other platforms and verify the current UI and supported
event choices before configuring monitoring.

Open the Cursor [Integrations dashboard](https://cursor.com/dashboard/integrations)
and connect GitHub to the Cursor account that owns the Grok Bot even if `gh api
user` and `ssf auth status` already succeed on the host. Prefer the user's
account for this second enrollment, with access to each repository the liaison
will watch. That preserves the useful separation: the liaison sees what the
human sees, while the factory remains the actor that delivers work as its bot
account.

The GitHub event connection used by a routine is also separate from a GitHub
plugin the Bot may use to read or change GitHub during a run. If the liaison
also needs that plugin, follow Cursor's
[plugin connection guidance](https://cursor.com/help/grok-bot/connect-plugins)
and treat each requested permission according to the access the liaison needs.

### Create a GitHub-event routine

Once the Cursor GitHub connection is active, ask the Grok Bot that will own the
liaison routine to create it. For example:

> Create an active routine for the supported GitHub events on `OWNER/REPO`
> that need my attention. When it runs, inspect the current issue or pull
> request, summarize what changed with links, explain whether SSF is already
> handling it, and tell me the next decision needed. Do not comment, merge,
> close, assign, or change project fields without my approval.

Name the repository and narrow the supported event selection to the activity
that is useful; broad listeners create noise and consume Grok Bot usage. Open
the Bot's **View conversation details → Routines** to review its instruction
and **When to run** trigger, turn it active, and inspect its run history. Test
with a safe matching GitHub event after saving rather than assuming that a
manual test proves the listener is subscribed. Cursor's
[skills and routines guide](https://cursor.com/docs/grok-bot/work#skills-and-routines)
describes creating, testing, and managing routines.
