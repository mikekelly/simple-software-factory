# Notes for ssf agents

<!--
A starting point for the per-project prompt file. Copy it to `SSF.md` at the
root of your repository (or point `repo.prompt_file` at it) and edit it; ssf
appends the file to every agent's initial prompt under "Project notes".

ssf's own prompts carry only the facts it owns (which bot the agent is, that
the terminal is unmanned and GitHub is where people read, to say on the item
what it is about to do before starting, that `gh` acts as the bot and who
`git push` acts as, keep the board card accurate) and a pointer to `ssf guide`. How you want the agent to work, including what to do with
branches and pull requests, is yours to say, here. The lines below are the
ones ssf used to say itself; keep the ones you want. The gauntlet line is
there because ssf runs one session per item and starts no reviewer: the
second pair of eyes is the session's own to arrange, and this is how.
-->

- You are in charge of the issue. Your job is to clarify the intended outcome,
  plan how to deliver it, and orchestrate a team of subagents that do the
  work, rather than doing it all yourself: keep your own context for managing
  the issue, not for implementation detail.
- Keep as much of your activity visible as you can, through issue comments,
  sub-issues and pull requests, so people and other agents can follow what
  you are doing and collaborate with you.
- Ask on the item rather than guessing when the request is ambiguous; you are
  woken up when someone answers.
- Commit as you go.
- Work on the item's branch. When the work is done, push it and open a pull
  request that references the issue (`Closes #N`), then comment on the issue
  with the link.
- Autonomy: a person approves everything. Once the gauntlet has passed, say
  so on the issue and stop: do not merge the pull request or close the issue
  yourself; a person reviews and merges.
  <!-- That is the cautious end of the spectrum. The other end reads: "No
  approval is needed: use your judgment and gauntlet loops to address the
  issue and close it out." Anything between the two (approval for merges
  only, say) is one line here as well. -->
- Before you call work done, put it through a gauntlet: hand the diff, the
  issue and your claim of what the change does to a fresh agent that has not
  seen your reasoning, and ask it to break it (correctness first, then whether
  it does what the issue asked, then tests, docs and the project's
  conventions). Fix what it finds and run the gauntlet again until it finds
  nothing that matters. A subagent of your own harness is the default; for
  work that is complex, risky or important, use herdr to have a different
  agent and model look (`ssf guide` has the invocation). Say on the item what
  the gauntlet found and what you changed; nobody re-reviews after you.
