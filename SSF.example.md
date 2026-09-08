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
want. The gauntlet rules are there because ssf runs
one session per item and starts no reviewer: the session decides the review
depth from the blast radius and arranges any second pair of eyes itself. The delegation line names the
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
  issue edit <N> --parent <M>`), and link them in the text otherwise; one
  you want worked by an agent of its own needs `--assignee` in the same
  create command, as `ssf guide` says. People and other agents follow the
  work at a high level from that structure, so keep it accurate as it
  changes.
- Measure twice, cut once. Write the plan into the item before execution
  begins: what you are going to do, in what order, and how you will know it
  worked. Put it in the body, appended under a heading of its own, and leave
  every word that is already there — the reporter's text, and the HTML
  comment carrying an `ssf: origin=` tag if the item has one. ssf reads that
  tag to know which session the item belongs to, and neither `gh issue edit`
  nor `gh pr edit` puts it back: overwrite it and the item stops reaching
  the session that owns it. The plan is a living document, not a one-off:
  when execution teaches you something that changes it, update it there
  rather than leaving the correction in a comment.
- Keep as much of your activity visible as you can, through issue comments,
  sub-issues and pull requests, so people and other agents can follow what
  you are doing and collaborate with you.
- Ask on the item rather than guessing when the request is ambiguous; you are
  woken up when someone answers.
- Delegate on purpose: name the model you start each kind of subagent with,
  and its effort level where you can set one, rather than taking whatever
  the default is. One choice for the ones that plan, diagnose and run the
  gauntlet, another for the ones implementing work you have already planned,
  which is where most of the tokens go. If this line does not name them,
  choose deliberately and say on the item what you chose and why; where your
  harness gives a session no say in what a subagent runs on, or no effort
  level to set, say that instead of inventing a setting.
  <!-- Name the ids and effort levels in the line above once you have picked
  them; which belongs on each side is this repository's decision, not a rule
  of thumb. ssf's setup document, "Choosing the harness and the model", is
  how to pick them: docs/setup.md in ssf's own repository, and
  /usr/share/doc/ssf/docs/setup.md (or $(brew --prefix)/share/doc/ssf/... on
  macOS) where ssf is installed. The line is here at all because ssf sets
  the model of this session (`repo.model`, `repo.effort`) and nothing below
  it, and on some harnesses a subagent inherits the session's model unless
  it is told otherwise. -->
- Commit as you go.
- Work on the item's branch. When the work is done, push it and open a pull
  request that references the issue (`Closes #N`), then comment on the issue
  with the link.
- The gauntlet is a final adversarial review that tries to break your change.
  Choose its depth by blast radius, not diff size. State the class and why on
  the issue: data loss, a factory unable to start, a broken package
  or a destroyed workspace require deep review until two consecutive rounds
  find no must-fix; ordinary daemon behaviour visible to sessions or people
  gets one round, and a second only if the first found a must-fix. Changes
  confined to documentation, comments, configuration examples or tests get
  careful self-review, no fresh agent. For mixed changes use the highest class.
- For each required round, give a fresh agent the diff, issue and claimed
  outcome; ask it to break correctness, requirements, tests, docs and project
  conventions. The author decides: accept feedback, decline it with a sentence
  explaining why, or debate it with that reviewer. Reviewers propose; they do
  not instruct. A must-fix is a confirmed substantive issue requiring a diff
  change; fix these before continuing. Wording, comment and naming tidies do
  not count. A round with only declined suggestions is clean. Stop on a clean
  round (two consecutive for deep review).
  Never run a round just to review tidies: take or leave them and finish;
  deep review's second clean round may review the same substantive diff.
  Use an in-harness subagent by default; for deep review prefer a strong
  reviewer on a different model through herdr in round one (`ssf guide`).
  Report the class, findings and fixes on the issue. Every class still runs
  the required tests, formatter, linter and package build before delivery.
- Autonomy: a person approves everything. Once the gauntlet has passed, say
  so on the item and stop: do not merge the pull request or close the issue
  yourself; a person reviews and merges.
  <!-- That is the cautious end of the spectrum. The other end reads: "No
  approval is needed: use your judgment and gauntlet loops to address the
  item and close it out." Anything between the two (approval for merges
  only, say) is one line here as well. -->
