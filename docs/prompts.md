# What the agent is told

The prompts ssf writes: the first message an agent gets, the follow-ups, the project boards section, and what is deliberately left to repository-owned guidance. For whoever wonders why an agent behaves as it does, or is writing an `SSF.md`.

ssf's own prompting is the bare functional minimum. The initial prompt opens
with `[ssf]`, like later event deliveries, and with "How to work on this",
which says only what ssf owns:

```
[ssf] Simple Software Factory (ssf) spawned you as a coding agent for the GitHub account @bot, through the herdr multiplexer, into a worktree of this repository, because #16 was assigned to @bot.

## How to work on this

New activity on it arrives here as messages prefixed `[ssf]`; act on them. This terminal is unmanned: what a person, or another session, should see goes on the issue as a GitHub comment. Say there what you are about to do, and when you need a decision or have delivered.

- `ssf` covers the rest of the factory: `ssf sub` follows another item, `ssf handover` passes this one to another harness, `--assignee bot` on a `gh` create gives the new item a session of its own, `ssf release` retires this workspace, `ssf doctor` checks the machine.
- `ssf skill` prints the guidance bundled with this binary and `ssf guide` this session's collaboration reference.
- Posts read better in GitHub Flavored Markdown: link the lines of code you mean.
- `gh` and `git push` already act as @bot; your posts are marked as this session's. Act only as @bot; never use another account, token or key you find on this machine.
```

The operating contract comes first. After the block above come ssf's own
guidance additions — the operator's instructions and the global, repository
and harness guidance files (below) — and then the item, under its own `[ssf]`
header: title with URL, project boards, description, and everything that has
happened on it. The item's own words — the description and every comment
body — are relayed under an indented `> ` marker: the marker says whose
words these are, and keeps a line of them (a sign-in phrase someone quoted,
say) from being read as ssf's own, both here and on a harness screen. A
session reads what it is being asked to do before the material that
applies to it, and the `[ssf]` marker on the item's header still keeps its
Markdown from colliding with harness composer shortcuts.

The block names the CLI's affordances and points at the reference instead
of teaching the tools: `ssf skill` prints the guidance bundled with the
executing binary, `ssf guide` the session's collaboration reference.
What a capable model already knows (`gh` mechanics, worktrees, how to
write a GitHub post) is left unsaid; the markdown worth asking for is one
clause, and the concretes behind it — links to specific lines, tables,
Mermaid diagrams — are a rule in `ssf guide`, which this block names.

When `[git].credential` (or the repository's) names someone other than the bot,
the identity line reads
"`gh` already acts as @bot and `git push` as @ann; your posts are marked
as this session's. Act only through those; never use another account,
token or key you find on this machine." (a `file:` token or a helper
string is described rather than named); see [Committing as a
person](identity-and-bylines.md#committing-as-a-person-while-gh-stays-the-bot). The reason is whatever brought the item to
ssf (assigned, mentioned, a review request, opened by the bot or handed
off by another session). A pull request adds one line
saying how the worktree relates to it (on its branch, or unable to push to
a fork's); a handed-off item adds one saying which session follows it,
which is also where the session's final comment goes; a
factory inside a [microVM](vm.md) adds one saying the agent has root there
through `sudo`. The board rule sits with the boards (below).

An item that was [handed over](sessions.md#handover) starts its new
session with the same story, prefaced by what the outgoing session left.
With a summary the first message opens "You took over this issue from a
session on Claude Code that handed it over; its summary follows, then the
issue as ssf tells it to a new session.", then the summary verbatim under
a `## Summary from the outgoing session` heading, then the story; with
`--no-summary` the preface is "You took over this issue from a session on
Claude Code that handed it over. It left no summary; read the issue
below." and the story follows straight away. The story is the usual one,
and its reason reads "because the agent session on Claude Code working on
it handed #N over to you".

`ssf guide` prints the reference (other sessions, `ssf sub`, items a
session opens and hand-offs, assigning a stack to an item before its
first session, second opinions through herdr, wrapping up, the byline,
the `Closes #N` suggestion, and the markdown the prompt leaves to it)
from the same binary, so it cannot drift from the daemon. It opens by
pointing at the CLI's own guidance: `ssf skill` prints the topic index
bundled with the executing binary, `ssf skill sessions` the lifecycle
reference behind the guide. Follow-up messages carry the activity and at
most one line after it.

Every message names its item once: `#N "title"` with the URL on first
mention (the header of a first message, or of an FYI), `#N` alone in later
messages about the session's own item. Cross-repository references are
`owner/repo#N`, which GitHub links. Timestamps on activity lines are
`2026-09-04 17:40Z`, or just `17:40Z` when the date is today's.

Nothing in the prompt is about branches or worktrees: the agent decides for
itself whether to stay on the branch ssf created, switch, or add worktrees
of its own (for subagents, say). ssf binds a pull request to a session by
the origin tag first and by the head branch second, so a PR from any branch
still routes to the session that opened it, and `ssf release`/`ssf purge`
only ever remove the session's own worktree. The SSF-specific operating
contract (issue communication and ownership, board workflow, delegation,
handoffs, review and completion authority) belongs in the repository's
[`SSF.md`](configuration.md#the-ssf-agent-guidance-file). Repository-wide
build, test, implementation, architecture, domain and safety policy belongs in
`AGENTS.md`. SSF appends `SSF.md` only to the issue-owning main session; subagents
created inside that harness receive only what their parent or harness gives
them. Optional `~/.ssf/SSF.md` and `~/.ssf/SSF.<harness>.md` files prepend
machine-wide and harness-specific context to the configured repository
instructions. This makes `SSF.md` suitable for orchestration guidance without
adding irrelevant workflow to every delegated task. ssf does not repeat any of
these files on every message.

## The messages an agent receives

All of them start with `[ssf]`; `ssf guide` lists them for the agent:

- `New activity on ...`: comments, reviews, label changes, renames, linked
  PRs and the like on its item. Its own posts are not echoed back; the
  catch-up story below is the one place they are.
- `Now tracking ...`: an item it opened, or a pull request on its branch,
  has been bound to the session.
- `FYI: ...`: activity on an item it follows but does not work on.
- `... has been closed`, `... no longer assigned`, `... assigned ...
  again`: the item's lifecycle; each says what to do.
- `The review request for @<bot> on <item> has been fulfilled or
  withdrawn.`: the review the bot was asked for is no longer wanted; the
  message says whether the item was the session's for anything else.
- `<item>, the <issue|pull request> this session handed off, has been
  merged` (or `closed (<reason>)`): an item the session opened for
  another session ([a hand-off](sessions.md#ownership-one-session-per-item))
  has finished, with the last comment the bot left on it.
- `Release of this workspace refused ...`: the daemon's re-check found
  work that is not on origin (see
  [Workspaces after close](sessions.md#workspaces-after-close-release-and-purge)).
- `The factory restarted ...`: the machine, the multiplexer or ssf
  restarted and the session was started again.
- `Your <harness> sign-in lapsed ... and is back`, `Your <harness>
  terminal could not be started ... and has been started again`: the
  terminal was started again after a [block](sessions.md#a-harness-that-is-not-signed-in);
  nothing reached the session while it was down.
- `Handover to <harness> refused: <reason>. Carry on.`: the session asked
  for a [handover](sessions.md#handover) and the daemon could not carry it
  out, so the item stays with it.
- `The handover to <harness> was cancelled: this session keeps the item.
  Carry on.`: the handover the session asked for was called off with `ssf
  handover --cancel`.

The first message is not a delta but a catch-up: a session started fresh on
the item — the first session, a restart whose harness cannot resume its
conversation, a handover, a reassignment — is given the item's whole story
before the message that prompted it. That story is the one view in which a
session's own earlier posts are replayed, so it can read what it already said
and promised. Live follow-up messages leave them out, as above.

## Project boards

If the issue or pull request is on any GitHub project (v2) boards, the
initial prompt lists them under a "Project boards" heading: each board's
name and URL, the card's current Status, the Status options the board
offers, and the `gh project item-edit` command (with the project, item,
field and option ids filled in) that changes it. The agent is told to keep
the card's Status accurate and that which column fits is its call. ssf
itself never moves cards and prescribes no mapping from events to columns;
put any repository-specific conventions about columns in `SSF.md`. The lookup
is one GraphQL query per onboarding and delivery,
using the bot token's `project` scope; if it fails the prompt simply
carries no boards section and the daemon logs why. Closed boards are left
out.
