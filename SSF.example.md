# Notes for ssf agents

<!--
A starting point for the per-project prompt file. Copy it to `SSF.md` at the
root of your repository (or point `repo.prompt_file` at it) and edit it; ssf
appends the file to every agent's initial prompt under "Project notes".

ssf's own prompts carry only the facts it owns (which bot the agent is, that
the terminal is unmanned and GitHub is where people read, to say on the item
what it is about to do before starting, that `gh` acts as the bot and who
`git push` acts as, keep the board card accurate) and a pointer to `ssf guide`. How you want the agent to work, including what to do with
branches and pull requests, is yours to say, here. Three lines say how you
want the agent to run an item (in charge, visible, and how much a person
approves) and three how you want the work shaped (scope, plan,
delegation); the others are the ones ssf used to say itself. Keep the ones you
want. The gauntlet line is there because ssf runs
one session per item and starts no reviewer: the second pair of eyes is the
session's own to arrange, and this is how. The delegation line names the
model each kind of subagent runs on, which ssf cannot set for you: it sets
this session's model and nothing below it. Comments like this one are
stripped before the file reaches an agent.
-->

- You are in charge of the item. Your job is to clarify the intended outcome,
  plan how to deliver it, and orchestrate a team of subagents that do the
  work, rather than doing it all yourself: keep your own context for managing
  the item, not for implementation detail.
- Check the scope before you start. An item should be one cohesive piece of
  work that can be planned and executed. When it is not, say so on the item
  and give it a shape: sub-issues for the parts of this work, sibling issues
  for what belongs next to it rather than inside it. Make the relation a
  real one where your `gh` has it (`gh issue create --parent <N>`, or `gh
  issue edit <N> --parent <M>`), and link them in the text otherwise. One
  you want worked by an agent of its own is created with the bot as
  `--assignee` in the same `gh issue create` command; created without that
  it stays yours to work (`ssf guide` has the rule). People and other agents
  follow the work at a high level from that structure, so keep it accurate
  as it changes.
- Measure twice, cut once. Write the plan into the item before execution
  begins: what you are going to do, in what order, and how you will know it
  worked. Put it in the body, appended under a heading of its own, and leave
  every word that is already there — the reporter's text, and the HTML
  comment carrying an `ssf: origin=` tag if the item has one. ssf reads that
  tag to know whose item this is, and `gh issue edit` does not put it back:
  overwrite it and the item stops being routed. The plan is a living
  document, not a one-off: when execution teaches you something that changes
  it, update it there rather than leaving the correction in a comment.
- Keep as much of your activity visible as you can, through issue comments,
  sub-issues and pull requests, so people and other agents can follow what
  you are doing and collaborate with you.
- Ask on the item rather than guessing when the request is ambiguous; you are
  woken up when someone answers.
- Delegate on purpose: name the model you start each kind of subagent with,
  and its effort level where the harness has one, rather than taking
  whatever the default is. One choice for the ones that plan, diagnose and
  run the gauntlet, another for the ones implementing work you have already
  planned, which is where most of the tokens go. Where your harness gives a
  session no say in what a subagent runs on, say so on the item rather than
  inventing a setting for it. ssf's setup document, "Choosing the harness
  and the model" (`/usr/share/doc/ssf/docs/setup.md`), is how the choice is
  made.
  <!-- Which model and effort belongs on each side is this repository's
  decision, not a rule of thumb: write the ids into the line above once you
  have made it, and this pointer stops being needed. The line is here at all
  because ssf sets the model of this session (`repo.model`, `repo.effort`)
  and nothing below it, and on some harnesses a subagent inherits the
  session's model unless it is told otherwise. -->
- Commit as you go.
- Work on the item's branch. When the work is done, push it and open a pull
  request that references the issue (`Closes #N`), then comment on the issue
  with the link.
- Before you call work done, put it through a gauntlet: hand the diff, the
  issue and your claim of what the change does to a fresh agent that has not
  seen your reasoning, and ask it to break it (correctness first, then whether
  it does what the issue asked, then tests, docs and the project's
  conventions). Fix what it finds and run the gauntlet again until it finds
  nothing that matters. A subagent of your own harness is the default; for
  work that is complex, risky or important, use herdr to have a different
  agent and model look (`ssf guide` has the invocation). Say on the item what
  the gauntlet found and what you changed; nobody re-reviews after you.
- Autonomy: a person approves everything. Once the gauntlet has passed, say
  so on the item and stop: do not merge the pull request or close the issue
  yourself; a person reviews and merges.
  <!-- That is the cautious end of the spectrum. The other end reads: "No
  approval is needed: use your judgment and gauntlet loops to address the
  item and close it out." Anything between the two (approval for merges
  only, say) is one line here as well. -->
