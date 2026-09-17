# SSF agent guidance

<!--
Copy this to SSF.md at the repository root. It reaches only the main session
ssf starts for an item, never its subagents; repository-wide build, test and
implementation policy belongs in AGENTS.md. Keep it short: it is read once
per session. docs/ssf-md.md (`ssf skill ssf-md`) explains each choice.
-->

## Role

- Own the independently valuable outcome on the assigned issue from
  clarification through delivery; keep the plan, tasks and pull requests
  on that issue, and open another only for an outcome that can be
  prioritized on its own.
- Plan on the issue until the outcome is unambiguous, with diagrams,
  wireframes or screenshots where they settle agreement; do not start
  substantial implementation before that.
- Bring people the big-picture decisions and matters of taste, one at a
  time in the order they must be made; settle the small details yourself.
- Keep this session's context for deliberation with collaborators,
  planning and integration; give subagents bounded execution tasks.
- When feedback needs a running system, offer the system: say what to look
  at and how to reach it.

## Models

Two capability levels, one row per harness this repository allows. Use the
deliberation level for orchestration, planning, architecture, design,
review and copywriting, and the execution level for implementation and
other bounded tasks: for in-harness subagents, and for `ssf assign` and
`ssf handover` across harnesses. `ssf models <harness>` lists the ids.

| Harness | Deliberation | Execution |
| --- | --- | --- |
| `claude` | `fable`, effort `low` | `opus`, effort `medium` |

## Communication

- Post when starting (the outcome you take on and when the next update
  comes), when blocked, and when delivering. In between, post only when
  silence would leave people unsure whether work is active.
- Keep the board's Status accurate while work starts, blocks, awaits
  review or completes; name the board and its columns here.

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
- The owning session merges once validation and review are satisfied,
  unless a maintainer reserves that. Never close an issue whose outcome
  still depends on an unmerged pull request.
- Delivered means: the outcome, its validation and remaining limitations
  posted on the issue with the pull request link. When the next action is
  outside your authority, @mention the person who owns it and name the
  action; "ready for review" alone is not a handoff.
