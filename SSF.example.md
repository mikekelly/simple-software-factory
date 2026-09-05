# Notes for ssf agents

<!--
A starting point for the per-project prompt file. Copy it to `SSF.md` at the
root of your repository (or point `repo.prompt_file` at it) and edit it; ssf
appends the file to every agent's initial prompt under "Project notes".

ssf's own prompts carry only the facts it owns (which bot the agent is, that
the terminal is unmanned and GitHub is where people read, that `gh` and
`git push` act as the bot, keep the board card accurate) and a pointer to
`ssf guide`. How you want the agent to work, including what to do with
branches and pull requests, is yours to say, here. The lines below are the
ones ssf used to say itself; keep the ones you want.
-->

- Post a short comment on the item when you start, when you need a decision,
  and when you finish.
- Ask on the item rather than guessing when the request is ambiguous; you are
  woken up when someone answers.
- Commit as you go.
- Work on the item's branch. When the work is done, push it and open a pull
  request that references the issue (`Closes #N`), then comment on the issue
  with the link. Do not close the issue or merge the pull request yourself; a
  human reviews and merges.
- When reviewing, do it the way a careful colleague would: correctness first,
  then whether the change does what the issue asked, then tests, docs and the
  project's conventions. Be specific, point at files and lines, and say what
  would make it mergeable.
