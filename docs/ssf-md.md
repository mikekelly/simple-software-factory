# Writing SSF.md

`SSF.md` at a repository's root is how its owner says how ssf sessions should
behave *as ssf sessions*: an unattended colleague working an item over GitHub.
ssf appends it to the first prompt of the session that owns the item, after
its own "How to work on this" and the operator's machine-wide files, and
never repeats it. `ssf doctor` reports a watched repository without one.
[SSF.example.md](../SSF.example.md) is the template this guide explains;
[What the agent is told](prompts.md) is what ssf says on its own.

## Two questions decide what goes where

1. **Would this rule also apply to a coding session someone runs at their own
   keyboard?** Build, test, formatting, architecture, domain and safety
   policy: yes, so it belongs in `AGENTS.md`, which every harness reads
   however the session was started. `CLAUDE.md` should be one line,
   `@AGENTS.md`, so there is one copy.
2. **Would this rule be wrong, or wasteful, if a subagent followed it?**
   Most harnesses hand `AGENTS.md` to every subagent they spawn; ssf gives
   `SSF.md` only to the main session. Advice for the main agent alone goes
   here. The clearest case is orchestration: keep the main session's context
   for deliberation with collaborators, planning and integration, and push
   execution to subagents. A subagent reading that would try to delegate
   its own task.

A rule that fails the first question and needs no protection from the
second still belongs here, because it only makes sense for a session that
is an unattended colleague on an item: when to post, who decides, what
"delivered" means.

Per-harness session behaviour goes in `SSF.<harness>.md` beside it
(`SSF.codex.md`, `SSF.claude.md`), which ssf adds only when that harness
starts the session. The operator's machine-wide preferences go in
`~/.ssf/SSF.md` on the factory (which models to spend, hours when nobody
answers), not in the repository, so the repository behaves the same on
another factory.

## The questions the owner answers

Each becomes a line or two; this is the stylistic part, the owner's taste
about how their sessions work.

1. **Planning.** How much clarification on the item before code, and what
   counts as agreed: a comment from a named person, a ticked task list, a
   wireframe accepted.
2. **Decisions.** Which decisions need a person and who that person is.
   Everything else the session settles and reports.
3. **Posting.** What a start, a blocked and a delivered post must contain,
   and how often to post in between (usually: only when silence would
   mislead).
4. **Review.** Self-review, one independent review, or a human, and for
   which classes of change; how many rounds before simplifying instead.
5. **Merging and closing.** Who merges, and whether the session closes its
   own issue.
6. **Boards.** The board and what each column means, if there is one.
7. **Delegation.** Appetite for subagents, delegated issues (`--assignee`),
   and handovers to another stack.
8. **Demos.** How feedback on a running system is given: screenshots, a URL
   the collaborators can reach, a recording.
9. **Delivered.** What "done" means here: merged, deployed, documented,
   workspace released.

## Keep it short

- Aim for well under 300 words. Every line is read by every session on
  every start, and a session that has read it once does not read it again.
- Do not restate the prelude or `ssf guide` (the byline, `--assignee`,
  `ssf release`, what the `[ssf]` messages mean). The file is for
  preferences, not mechanics.
- Write rules, not narration. One outcome per bullet.

## Two examples

A solo maintainer:

```markdown
# SSF agent guidance

- Post when you start (what you will deliver, when the next update is),
  when you are blocked, and when you deliver.
- Ask me before changing scope or anything that costs money; settle the
  rest yourself and say what you chose.
- Documentation and test-only changes: self-review. Behaviour changes: one
  review by a fresh subagent of the pinned diff, one follow-up at most.
- Open the PR with `Closes #N` and @mention me to merge; I merge.
- Keep this session for planning and talking to me; delegate execution to
  subagents.
```

A team with a board:

```markdown
# SSF agent guidance

- Plan on the issue until @lead has agreed the acceptance criteria as a
  task list; then implement.
- On the `Product` board, `In Progress` while working, `Review` when the PR
  is up, `Done` only after merge.
- Behaviour changes get one independent review of the pinned diff and a
  human approval from a code owner; fix confirmed defects, do not loop.
- Delegate independent tasks as issues assigned to the bot; keep this
  session for integration and communication.
- Delivered means merged, the deploy checked, and `ssf release` run.
```
