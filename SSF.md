# Notes for ssf agents

- You are in charge of the issue. Your job is to clarify the intended outcome, plan how to deliver it, and orchestrate a team of subagents that do the work, rather than doing it all yourself: keep your own context for managing the issue, not for implementation detail.
- Check the scope before you start. An issue represents one independently valuable outcome, titled and summarized in terms a manager can understand. Keep its delivery plan, implementation tasks, subagent work and linked PRs within that issue. Use subagents to divide implementation; do not create sibling or child issues merely to divide the work or a review. Record a separate issue only for an independently prioritized outcome outside the current scope, and leave it for separate scheduling. One you want worked by an agent of its own needs `--assignee OverlayBot` at creation, as `ssf guide` says. Keep the board focused on delivered outcomes, current work, blockers and next priorities; put technical detail in the plan and delivery evidence.
- Measure twice, cut once. Write the plan into the issue before execution begins: what you are going to do, in what order, and how you will know it worked. Put it in the body, appended under a heading of its own, and leave every word that is already there — the reporter's text, and the HTML comment carrying an `ssf: origin=` tag if the issue has one. ssf reads that tag to know which session the issue belongs to, and neither `gh issue edit` nor `gh pr edit` puts it back: overwrite it and the issue stops reaching the session that owns it. The plan is a living document, not a one-off: when execution teaches you something that changes the plan, update it there rather than leaving the correction in a comment.
- Keep as much of your activity visible as you can on the owning issue, through its plan, comments and linked pull requests. Put implementation decomposition in subagent tasks and the plan; do not create a separate issue for it.
- Post a short comment on the issue when you start, when you need a decision, and when you finish.
- Ask on the issue rather than guessing when the request is ambiguous; you are woken up when someone answers.
- Delegate on purpose: name the model you start each kind of subagent with, and its effort level where you can set one, rather than taking whatever the default is — one choice for the ones that plan, diagnose and run the gauntlet, another for the ones implementing work you have already planned, which is where most of the tokens go. Which belongs on each side is a decision to make from what the harness reaches and what a task costs there (`docs/setup.md`, "Choosing the harness and the model"), and where the harness gives a session no say in what a subagent runs on, or no effort level to set, say that on the issue instead of inventing a setting.
- Commit as you go.
  Read `git diff --cached` before writing the commit message, not after.
- Work on the issue's branch and open a PR that references the issue (`Closes #N`), then comment on the issue with the link.
- Keep `cargo test` green and run `cargo fmt` and `cargo clippy` before pushing.
- Update `README.md` (or the right file under `docs/`) and `config.example.toml` for any user-visible behaviour.
- A change to setup, configuration, commands or operating behaviour also updates `skills/ssf-setup/SKILL.md` in the same PR; the skill is what a coding agent follows to set ssf up, so a PR that leaves it stale is not done.
- Rebuild the package with `cd packaging && makepkg -fd` before calling something done; commit the `pkgver` bump makepkg makes to `packaging/PKGBUILD`.
- The installed service runs the last package the maintainer installed, so verify daemon behaviour with unit tests and scratch `SSF_CONFIG_DIR`/`SSF_STATE_DIR` runs rather than expecting to see your change live; those variables move ssf's own files and nothing else, so read `docs/development.md` before starting one.
- Check a claim rather than reasoning your way to one: read the source, or try it where trying it changes nothing. Where you have not checked, say so. This covers what you write about a change as much as the change itself — comments, commit messages, issue bodies, the sentence explaining why something is safe — because a wrong description outlives a wrong line, since the next person reads it instead of checking.
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
- Autonomy: no approval is needed along the way; use your judgment and gauntlet loops to address the issue. The one step that stays with a person for now is the merge: once the gauntlet has passed, say so on the issue and leave the merge to the maintainer or the project-management session; the issue closes with it, not by your hand.
