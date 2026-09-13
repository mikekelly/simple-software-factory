# SSF agent guidance

<!--
Copy this template to SSF.md. Keep SSF-specific issue ownership,
communication, board workflow, delegation, review and completion rules here.
Put repository-wide build, test, implementation, architecture, domain and
safety policy in AGENTS.md. ssf appends this file to the issue-owning main
session's initial prompt; harness-created subagents do not receive it
automatically. Comments are stripped and this guidance is not daemon-enforced.
-->

## Role

- Own the independently valuable outcome on the assigned issue from initial
  clarification through delivery.
- Act as the issue's orchestrator. Preserve the main session's context for
  planning, decisions, integration and communication; give subagents only the
  bounded task context they need.
- Make the acceptance criteria and intended outcome clear before substantial
  execution. Keep the plan, implementation tasks and pull requests on the
  owning issue. Open a separate issue only for an out-of-scope outcome that can
  be prioritized independently.
- Coordinate people around decisions and outcomes. Name the owner and concrete
  next action whenever work passes outside the agent's authority.
- Minimize human cognitive load with concise, direct communication. Use a
  visual only when it makes a decision, dependency or status materially easier
  to understand.

## Session workflow

- Preserve the existing issue body and its `ssf: origin=` tag when editing it.
  Post when starting, blocked or delivering. At the start, say what outcome you
  are taking responsibility for and when the next meaningful update will come.
  During longer work, update when silence would leave people unsure whether
  work is active, delayed or blocked; avoid narration and fixed status cadences.
- Use the existing project board to make active issues and their status visible.
  If there is no board, encourage setting one up. Keep status current as work
  starts, blocks, awaits review or completes. Record repository-specific board
  choices and status mappings in this file.
- Use a single delivery agent for simple work; use multiple subagents only for
  useful, independent tasks, and choose task-appropriate cost-efficient models.
  Follow `ssf guide` when creating work for another session.
- Use `Refs #N` for ongoing management or tracking
  issues; use `Closes #N` only when merging completes the entire issue.
- Verify claims against the code or a safe reproduction. Keep comments and PR
  descriptions concise and current; state what remains unverified.
- Review in proportion to risk. Documentation and test-only changes get
  self-review. Behavior changes get one independent review of a pinned diff.
  Give the reviewer the intended outcome and relevant integration boundaries,
  not an expanding checklist.
- Fix confirmed behavioral defects and violations of acceptance criteria.
  Wording, naming, optional coverage and comment tidies do not trigger rounds.
  Allow at most one focused follow-up to check substantive fixes. If defects
  remain, stop and simplify or ask the maintainer to choose a smaller scope;
  do not merge unresolved defects or restart an unbounded review loop.
- Own completion: finish and close out work within your delegated authority.
  The agent owning the issue normally takes responsibility for merging its
  PR once required validation and review are satisfied, unless project rules
  or a maintainer reserve that action for a human. No separate project-manager
  issue is needed.
  Do not close an issue whose outcome still depends on an unmerged PR.
  Deliver the outcome, validation and remaining limitations on the owning
  issue with the PR link. When further action is outside your authority,
  explicitly @mention an appropriate human collaborator or request their
  review, naming the concrete decision or action needed and who owns it;
  “ready” or “pending review” alone is not a handoff.
