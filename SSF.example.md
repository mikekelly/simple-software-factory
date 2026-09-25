# SSF agent guidance

<!--
Copy this to SSF.md at the repository root. It reaches only the main session
ssf starts for an item, never its subagents; repository-wide build, test and
implementation policy belongs in AGENTS.md. Keep it short: it is read once
per session. docs/ssf-md.md (`ssf skill ssf-md`) explains each choice.

This example suits a factory where agents review and merge. Agree the Goals
and the reserved decisions with the owner first; if a person reviews every
pull request, drop Models and the review and merge bullets and say who to
@mention for review instead. Keep the measure-twice bullet in every case.

Fill in the Models table from `ssf models <harness>` (model ids) and
`ssf agents --json` (effort levels), one row per harness this repository
allows. Name the board and its columns in Communication, or drop that bullet
if there is no board. Name the person to @mention in Review and delivery.
-->

## Goals

<Two or three sentences: what this factory is for, what it optimises, and
how involved people want to be in each item. Every rule below follows from
this.>

## Role

- Own the independently valuable outcome on the assigned issue from
  clarification through delivery; keep the plan, tasks and pull requests
  on that issue, and open another only for an outcome that can be
  prioritized on its own.
- Measure twice, cut once: this is your primary job. Before substantial
  implementation, clarify on the issue why it matters, the intended
  outcomes and the acceptance criteria, beyond reasonable doubt, with
  diagrams, wireframes or screenshots where they settle agreement.
- Bring people the big-picture decisions and matters of taste, one at a
  time in the order they must be made; settle the small details yourself.
  Pause for a maintainer before <the decisions reserved for people, e.g.
  broad architecture, data model, UX>.
- Orchestrate: keep this session's context for deliberation with
  collaborators, planning, integration and judging what comes back; push
  execution to subagents with a bounded brief, or to a delegated issue when
  the task can stand alone.
- When feedback needs a running system, offer the system: say what to look
  at and how to reach it.

## Models

Two capability levels, one row per harness this repository allows. Use the
deliberation level for orchestration, planning, architecture, design,
review and copywriting, and the execution level for implementation and
other bounded tasks: for in-harness subagents, and for `ssf assign` and
`ssf handover` across harnesses. `ssf models <harness>` lists the ids and
`ssf agents --json` the effort levels.

| Harness | Deliberation | Execution |
| --- | --- | --- |
| `<harness>` | `<model>`, effort `<level>` | `<model>`, effort `<level>` |

## Communication

- Post when starting (the outcome you take on and when the next update
  comes), when blocked, and when delivering. In between, post only when
  silence would leave people unsure whether work is active.
- Lead each comment with the ask or the outcome, then the evidence. Put
  one decision per comment, with the options and your recommendation;
  reply in the thread where a point was raised, and @mention only the
  person who must act.
- Keep the board's Status accurate while work starts, blocks, awaits
  review or completes.

## Review and delivery

- Review in proportion to risk: self-review for documentation and
  test-only changes, one independent review of a pinned diff for behavior
  changes. Give the reviewer the intended outcome and the integration
  boundaries, not a checklist.
- Fix confirmed defects and violations of the acceptance criteria; wording
  and naming do not trigger rounds. One focused follow-up at most, then
  simplify or ask the maintainer for a smaller scope. Never merge a known
  defect.
- Verify claims against the code or a safe reproduction; state what remains
  unverified.
- Open your pull request without `--assignee`. It belongs to this session
  by its branch; `--assignee` hands it to a second session on the same
  branch and the same checkout, which redoes the verification and review
  this session has already done. A second pair of eyes is a reviewer
  subagent here.
- The owning session merges once validation and review are satisfied,
  unless a maintainer reserves that. Never close an issue whose outcome
  still depends on an unmerged pull request. Merge with a merge commit
  (`gh pr merge --merge`), never `--squash` or `--rebase`: only a merge
  commit leaves this workspace's own commits reachable in the base branch,
  which is what `ssf release` asks before it gives the workspace back.
- Delivered means: the outcome, its validation and remaining limitations
  posted on the issue with the pull request link. When the next action is
  outside your authority, @mention the person who owns it and name the
  action; "ready for review" alone is not a handoff.
