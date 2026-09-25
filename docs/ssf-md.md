# Writing SSF.md

`SSF.md` at a repository's root is how its owner says how ssf sessions should
behave *as ssf sessions*: an unattended colleague working an item over GitHub.
ssf appends it to the first prompt of the session that owns the item, after
its own "How to work on this" and the operator's machine-wide files, and
never repeats it. `ssf doctor` reports a watched repository without one.
[SSF.example.md](../SSF.example.md) is a starting point, not a default;
[What the agent is told](prompts.md) is what ssf says on its own.

## Your job: a conversation, then a file

If you are helping someone write or audit an `SSF.md`, do not fill in the
template for them. Find out how they want this factory to work, then write
down what they chose. Ask one question at a time:

1. **Operational goals.** What is this factory for, and what should it
   optimise: throughput, safety, the owner's attention, learning,
   something else? How much does the owner want to be involved in each
   item? The answer becomes a **thesis** of two or three sentences at the
   top of `SSF.md`. It explains the "why" behind every rule below it, so a
   session can act sensibly where the rules are silent.
2. **Latitude.** Where may agents use their own judgement, and where must
   they stop and defer to a person before proceeding? Common candidates
   are broad architecture, fundamental entity or data design, UX and
   visual choices, public interfaces, anything that costs money or is hard
   to reverse. Name who decides each. Much of this is settled while
   clarifying the item (below); list only what the owner wants called out.
3. **Review and merging.** Who reviews a pull request (a person every
   time, an AI reviewer, both, a reviewer on another harness or model) and
   who merges. Only if agents review or merge does `SSF.md` need a review
   policy, a merge method and a Models table; a factory where a person
   reviews every pull request needs none of them.
4. **The rest, as they apply:** what a start, a blocked and a delivered
   post contains and how often to post in between; what a good comment
   looks like (lead with the ask or outcome, one decision per comment with
   a recommendation, reply in the thread, @mention only whoever must act);
   the project board's columns; how demos are given; what "delivered"
   means.

Propose the resulting file on the item or in the conversation, and let the
owner correct it. When auditing, run the same conversation against the
existing file: does it state a thesis, and do its rules follow from it?

## Always included: measure twice, cut once

Whatever the owner chooses, every `SSF.md` makes the session primarily
responsible for clarifying its item before building: the **why**, the
**intended outcomes** and the **acceptance criteria**, agreed on the item
beyond reasonable doubt before substantial implementation. Do not trim this.

## What goes where

1. **Would this rule also apply to a coding session someone runs at their own
   keyboard?** Build, test, formatting, architecture, domain and safety
   policy: yes, so it belongs in `AGENTS.md`, which every harness reads
   however the session was started. `CLAUDE.md` should be one line,
   `@AGENTS.md`, so there is one copy.
2. **Would this rule be wrong, or wasteful, if a subagent followed it?**
   Most harnesses hand `AGENTS.md` to every subagent they spawn; ssf gives
   `SSF.md` only to the main session. Orchestration belongs here: keep the
   main session's context for deliberation with collaborators, planning
   and integration, and push execution to subagents. A subagent reading
   that would try to delegate its own task.

A rule that fails the first question and needs no protection from the
second still belongs here, because it only makes sense for a session that
is an unattended colleague on an item. The other files ssf layers around
this one are listed in [What the agent is told](prompts.md#how-to-work-on-this).

## Mechanics worth getting right

- **Merging.** Where the session merges, say how: a merge commit
  (`gh pr merge --merge`), not `--squash` or `--rebase`, because only a
  merge commit leaves the session's own commits reachable from a
  remote-tracking ref, which is the check `ssf release` makes before it
  gives the workspace back ([sessions.md](sessions.md#workspaces-after-close-release-and-purge)).
- **Models.** Where agents delegate or review, a table of the harnesses
  the repository allows, each with a model and effort for *deliberation*
  (orchestration, planning, architecture, design, review, copywriting) and
  *execution* (implementation and other bounded tasks). The session reads
  it for subagents, `ssf assign` and `ssf handover`; without it, subagents
  inherit whatever the session runs on. Fill it from `ssf models <harness>`
  and `ssf agents --json`; `repo.model` and `repo.effort` in the factory's
  config still choose what the session itself starts on.

## Keep it short

- As short as possible, but no shorter. Every line is read by every
  session on every start; keep a rule only if the session would act
  differently without it.
- Give a rule its reason in a clause, not a paragraph: enough that the
  session can apply it where the rule is silent.
- Do not restate the prelude or `ssf guide` (formatting of posts, the
  byline, `--assignee`, `ssf release`, what the `[ssf]` messages mean).
- Write rules, not narration. One outcome per bullet.

## An example

A solo maintainer who reviews and merges everything, with no board:

```markdown
# SSF agent guidance

This factory turns my issues into pull requests I can review quickly.
I care more about getting the right thing than about speed, and I want
to spend my attention on decisions, not on reading code twice.

- Before building, pin down on the issue why it matters, the intended
  outcome and the acceptance criteria, until neither of us could
  reasonably disagree about them.
- Ask me before changing scope, the data model, the UI, or anything that
  costs money; settle the rest and say what you chose.
- Post when you start, when blocked and when delivering. Lead each comment
  with what you need from me; one decision per comment, with options and
  your recommendation.
- Open the PR with `Closes #N` and @mention me to review; I merge.
- Keep this session for planning and talking to me; delegate execution to
  subagents.
```

[SSF.example.md](../SSF.example.md) is a fuller example for a factory
where agents also review and merge.
