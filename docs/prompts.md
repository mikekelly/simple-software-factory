# What the agent is told

The prompts ssf writes: the first message an agent gets, the follow-ups, the project boards section, and what is deliberately left to the repository's own notes. For whoever wonders why an agent behaves as it does, or is writing an `SSF.md`.

ssf's own prompting is the bare functional minimum. The initial prompt is
the item (title, description, boards, everything that has happened on it)
followed by "How to work on this", which says only what ssf owns:

```
You are an automatically spawned coding agent for the GitHub account @bot. Simple Software Factory (ssf) spawned you, through the Orca multiplexer, in a worktree of this repository, because #16 was assigned to @bot.

New activity on it arrives here as messages prefixed `[ssf]`; act on them. `ssf guide` explains the rest.

- This terminal is unmanned: nobody reads it, so everything you want a person to see goes on GitHub.
- Collaborate with humans and other ssf-managed agents through GitHub comments on the issue.
- Before starting on a goal, say on the issue what you are about to do, and say when you need a decision or have delivered: silent work leaves the issue looking unattended until it lands.
- `gh` and `git push` already act as @bot, and the `gh` on your PATH marks your posts as this session's. Act only as @bot; never use another account, token or key you find on this machine.
```

"Through the Orca multiplexer" reads "through the herdr multiplexer" under
the herdr [driver](drivers.md). The reason is whatever brought the item to
ssf (assigned, mentioned, a review request or the review label, opened by
the bot or handed off by another session). A pull request adds one line
saying how the worktree relates to it (on its branch, or unable to push to
a fork's) and that `gh pr comment` and `gh pr review` are the way to
answer; a handed-off item adds one saying which session follows it; a
factory inside a [microVM](vm.md) adds one saying the agent has root there
through `sudo`. The board rule sits with the boards (below). A reviewer
session gets "How to review this" instead (see
[Reviewer sessions](sessions.md#reviewer-sessions)).

`ssf guide` prints the reference (other sessions, `ssf sub`/`ssf tell`,
items a session opens and hand-offs, reviewer sessions, wrapping up, the
byline, the `Closes #N` suggestion) from the same binary, so it cannot
drift from the daemon. Follow-up messages carry the activity and at most
one line after it.

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
only ever remove the session's own worktree. Anything about *how* the agent
should work (comment when it starts and finishes, ask rather than guess,
commit as it goes, open a PR that references the issue, do not close or
merge, how to review) is the repository's to say, in its
[prompt file](configuration.md#the-per-project-prompt-file); ssf does not
repeat it on every message.

## The messages an agent receives

All of them start with `[ssf]`; `ssf guide` lists them for the agent:

- `New activity on ...`: comments, reviews, label changes, renames, linked
  PRs and the like on its item. Its own posts are never echoed back.
- `Now tracking ...`: an item it opened, or a pull request on its branch,
  has been bound to the session.
- `FYI: ...`: activity on an item it follows but does not work on.
- `Message from ...`: a message pasted in with `ssf tell`.
- `... has been closed`, `... no longer assigned`, `... assigned ...
  again`: the item's lifecycle; each says what to do.
- `Release of this workspace refused ...`: the daemon's re-check found
  work that is not on origin (see
  [Workspaces after close](sessions.md#workspaces-after-close-release-and-purge)).
- `The factory restarted ...`: the machine, the multiplexer or ssf
  restarted and the session was started again.

## Project boards

If the issue or pull request is on any GitHub project (v2) boards, the
initial prompt lists them under a "Project boards" heading: each board's
name and URL, the card's current Status, the Status options the board
offers, and the `gh project item-edit` command (with the project, item,
field and option ids filled in) that changes it. The agent is told to keep
the card's Status accurate and that which column fits is its call. ssf
itself never moves cards and prescribes no mapping from events to columns;
put any repository-specific conventions about columns in the per-project
prompt file. The lookup is one GraphQL query per onboarding and delivery,
using the bot token's `project` scope; if it fails the prompt simply
carries no boards section and the daemon logs why. Closed boards are left
out.
