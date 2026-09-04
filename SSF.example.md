# Notes for ssf agents

<!--
A starting point for the per-project prompt file. Copy it to `SSF.md` at the
root of your repository (or point `repo.prompt_file` at it) and edit it; ssf
appends the file to every agent's initial prompt under "Project notes".

ssf's own prompts carry only the rules it needs (act as the bot, tag posts,
do not close the issue or merge the PR, keep the board card accurate) and a
pointer to `ssf guide`. How you want the agent to work is yours to say, here.
The lines below are the ones ssf used to say itself; keep the ones you want.
-->

- Post a short comment on the item when you start, when you need a decision,
  and when you finish.
- Ask on the item rather than guessing when the request is ambiguous; you are
  woken up when someone answers.
- Commit as you go.
- When reviewing, do it the way a careful colleague would: correctness first,
  then whether the change does what the issue asked, then tests, docs and the
  project's conventions. Be specific, point at files and lines, and say what
  would make it mergeable.
