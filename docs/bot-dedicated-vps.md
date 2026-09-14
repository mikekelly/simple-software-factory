# Bot-dedicated-VPS: use Grok Bot as the SSF liaison

A **Bot-dedicated-VPS** is a Grok Bot computer used as the user's always-on
assistant host as well as the machine that runs SSF. This is a distinct setup
from merely installing SSF on stripped or headless Linux: the factory handles
work, while a human-facing Grok Bot can watch the relevant GitHub repositories,
summarize activity, and help the user decide what to ask the factory to do.

First complete the [Grok Bot / headless-host installation](headless-host.md) so
the factory can act as its bot GitHub account. Then configure the liaison below.

## Keep the two GitHub enrollments separate

The working factory does **not** mean the liaison can wake on GitHub activity.
These are two independent connections:

| Connection | Identity to use | Purpose |
|---|---|---|
| `gh` plus `ssf auth` on the host | The dedicated factory bot account | Lets SSF discover assigned work and lets its agent sessions comment, commit, and open pull requests as the bot |
| The Cursor account's GitHub connection | Prefer the **user's GitHub account** | Supplies GitHub events that can wake a Grok Bot routine and gives the liaison the same repository view as the user |

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

## Choose the liaison granularity

There is no SSF-required mapping between Bots and repositories. Choose the
boundary that makes the conversation useful:

- one liaison Bot and conversation per watched repository;
- one liaison covering several related repositories; or
- finer Bots for distinct streams or responsibilities within a repository.

A repository-per-liaison arrangement usually gives the clearest history. A
coarser liaison reduces the number of conversations, while a finer arrangement
can separate noisy or operationally different event streams.

## Create a GitHub-event routine

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

## Current listener limits

Treat the GitHub listener as a selective wake-up path, not as an audit stream
of everything visible on GitHub:

- GitHub Projects v2 board changes do not currently wake a Grok Bot routine.
  A status-field move or other board-only update can therefore pass unnoticed.
- The available issue-event choices do not cover every kind of issue activity.
  Do not interpret an issue listener as “all comments, labels, assignments, and
  edits.” Review the event choices shown when the routine is created.
- Only activity matching the selected, supported repository events wakes the
  routine. Verify important workflows with a real event and check run history.

Keep board monitoring and any event types absent from the routine picker as a
separate manual or scheduled check. SSF's own repository polling and project
management continue independently; a missed liaison wake does not transfer the
factory bot's identity or responsibilities to the user-connected liaison.
