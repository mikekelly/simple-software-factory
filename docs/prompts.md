# What the agent is told

The prompts ssf writes: the first message an agent gets, the catch-up story and its budgets, the project boards section, and what is deliberately left to repository-owned guidance. For whoever wonders why an agent behaves as it does, or is writing an `SSF.md`.

## The first prompt

ssf's own prompting is the bare functional minimum: what ssf owns, plus every
affordance a session needs for coherent work, since reading the guide is not
guaranteed. The initial prompt opens with `[ssf]`, like later event deliveries,
and with "How to work on this":

```
[ssf] Simple Software Factory (ssf) spawned you as a coding agent for the GitHub account @bot, through the herdr multiplexer, into a worktree of this repository, because #16 was assigned to @bot.

## How to work on this

You are a remote colleague working this issue to delivery: clarify on it until the outcome is unambiguous, deliver (a pull request, a review, an answer), and let the people on it decide and review on GitHub. New activity on it arrives here as messages prefixed `[ssf]`; act on them. This terminal is unmanned: what a person, or another session, should see goes on the issue as a GitHub comment. Say there what you are about to do, and when you need a decision or have delivered.

- Posts are read on GitHub: write GitHub Flavored Markdown, link the exact lines you mean (pinned to a commit), and use tables, Mermaid diagrams, task lists, `<details>` for long output, and screenshots or wireframes where they make a decision easier. Collaborators are remote: a live demo needs an address they can reach.
- `gh` and `git push` already act as @bot; your posts are marked as this session's. Act only as @bot; never use another account, token or key you find on this machine.
- `--assignee bot` on a `gh` create gives the new item a session of its own; `ssf sub` follows another item; `ssf handover` passes this one to another harness; `ssf release` retires this workspace; `ssf doctor` checks the machine. `ssf guide` is the reference behind all of this.
```

After that block come ssf's own guidance additions, the operator's
instructions and the global, repository and harness guidance files (below),
and then the item, under its own `[ssf]` header: title with URL, project
boards, description, and what has most recently happened on it. The item's own
words, the description and every comment body, are relayed under an indented
`> ` marker: the marker says whose words these are and keeps a line of them (a
sign-in phrase someone quoted, say) from being read as ssf's own, both here and
on a harness screen. A session therefore reads what it is being asked to do
before the material that applies to it, and the `[ssf]` marker on the item's
header keeps the item's Markdown from colliding with harness composer
shortcuts.

## The lines that vary

The reason in the first sentence is whatever brought the item to ssf: assigned,
mentioned, a review request, opened by the bot, or handed off by another
session.

When `[git].credential` (or the repository's) names someone other than the bot,
the identity line reads "`gh` already acts as @bot and `git push` as @ann; your
posts are marked as this session's. Act only through those; never use another
account, token or key you find on this machine." A `file:` token or a helper
string is described rather than named; see [Committing as a
person](identity-and-bylines.md#committing-as-a-person-while-gh-stays-the-bot).

A pull request adds one line saying how the worktree relates to it (on its
branch, or unable to push to a fork's). A handed-off item adds one saying which
session follows it, which is also where the session's final comment goes. A
factory inside a [microVM](vm.md) adds one saying the agent has root there
through `sudo`.

An item that was [handed over](sessions.md#handover) starts its new session with
the same story, prefaced by what the outgoing session left: a line naming the
harness it came from, then the summary verbatim under a `## Summary from the
outgoing session` heading, or, after `--no-summary`, a line saying it left none
and to read the issue below. The story's reason then reads "because the agent
session on <harness> working on it handed #N over to you".

## What SSF.md must not repeat

Nothing in the prompt is about branches or worktrees: the agent decides for
itself whether to stay on the branch ssf created, switch, or add worktrees of
its own. The SSF-specific operating contract, issue communication and
ownership, board workflow, delegation, handoffs, review and completion
authority, belongs in the repository's [`SSF.md`](ssf-md.md), and
repository-wide build, test, implementation, architecture, domain and safety
policy belongs in `AGENTS.md`. What a capable model already knows (`gh`
mechanics, worktrees, how to write a comment) is left unsaid, and what each
affordance is for is `ssf guide`'s to explain, so an `SSF.md` that restates any
of this only spends context. ssf appends `SSF.md` to the issue-owning main
session alone; subagents created inside that harness receive only what their
parent or harness gives them. Optional `~/.ssf/SSF.md` and
`~/.ssf/SSF.<harness>.md` files prepend machine-wide and harness-specific
context to the configured repository instructions. None of these files is
repeated on every message.

## The messages an agent receives

Every later message starts with `[ssf]` too: item activity, lifecycle changes,
tracking and FYI notices, refusals and restart notices. `ssf guide` lists them
for the agent, and follow-up messages carry the activity and at most one line
after it. Every message names its item once: `#N "title"` with the URL on first
mention, `#N` alone in later messages about the session's own item;
cross-repository references are `owner/repo#N`, which GitHub links. Timestamps
are `2026-09-04 17:40Z`, or `17:40Z` when the date is today's.

The first message is not a delta but a catch-up: a session started fresh on the
item, the first session, a restart whose harness cannot resume its
conversation, a handover, a reassignment, is given the item's story before the
message that prompted it. That story is the one view in which a session's own
earlier posts are replayed, so it can read what it already said and promised.
Live follow-up messages leave them out.

A busy item holds more than a session should be handed before it has done any
work, and most of it does not bear on what brought the session up, so the story
spends its budget on recency. The title and description always arrive whole, and
the activity is the newest events, bounded by `daemon.first_prompt_max_events`
and `daemon.first_prompt_max_chars` (per-repository overrides of the same names;
`0` is no limit). Both are spent newest first, so the event that started the
session is always included, an update aggregated out of many comment bodies may
pass the character budget on its own. What was left out is said in ssf's own
words, above the events and outside the quoted item, with how many events and
how to read the rest (`gh issue view N --comments`, or the timeline API); those
events are never delivered later. A description passes no such cap: GitHub
refuses a body past 65,536 characters, so it cannot pass a session's window on
its own.

The same cap covers the first message a live session gets about an item newly
bound to it: it too is assembled against an empty `seen` map and would otherwise
carry the item's whole timeline. Live activity messages are deltas by
construction and are left alone.

## Project boards

If the issue or pull request is on any GitHub project (v2) boards, the initial
prompt lists them under a "Project boards" heading: each board's name and URL,
the card's current Status, the Status options the board offers, and the
`gh project item-edit` command (with the project, item, field and option ids
filled in) that changes it. The agent is told to keep the card's Status accurate
and that which column fits is its call. ssf itself never moves cards and
prescribes no mapping from events to columns; put any repository-specific
conventions about columns in `SSF.md`. The lookup is one GraphQL query per
onboarding and delivery, using the bot token's `project` scope; if it fails the
prompt simply carries no boards section and the daemon logs why. Closed boards
are left out.
