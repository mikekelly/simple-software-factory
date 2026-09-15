# Bot-managed host: use an assistant as the SSF liaison

A bot-managed host runs both SSF and an always-on assistant that acts as the
user's liaison. The factory delivers coding work; the liaison watches relevant
GitHub activity, summarizes changes, and helps the user decide what to ask the
factory to do. Any assistant with suitable repository access and a supported
way to receive events or schedule checks can fill this role. The two services
can also run on separate hosts.

First complete the [VPS / headless-host installation](headless-host.md), or
[Setup](setup.md) for a packaged factory. Then configure the liaison using its
own platform's instructions.

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

## Optional example: Grok Bot on Cursor

The following enrollment and routine controls are specific to Grok Bot on
Cursor. Use your assistant's equivalent controls on other platforms and verify
the current UI and supported event choices before configuring monitoring.

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
