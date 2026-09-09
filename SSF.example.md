# Notes for ssf agents

<!--
Copy this to SSF.md and adapt validation and merge authority to your project.
ssf appends it to the session prompt; comments are stripped. These are project
preferences, not daemon-enforced policy.
-->

- Own one independently valuable outcome. Keep its plan, implementation tasks
  and PRs on the owning issue. File separate issues only for out-of-scope
  outcomes that can be prioritized independently; keep board titles readable
  without implementation knowledge.
- Write a short plan before substantial work. Preserve the existing issue body
  and its `ssf: origin=` tag when editing it. Post when starting, blocked,
  or delivering; avoid narrating every check or duplicating updates.
- Prefer the smallest change that solves the problem. Delegate only useful,
  independent tasks; simple work does not need a team. Follow `ssf guide`
  when creating work for another session.
- Work on the issue branch, read `git diff --cached` before committing, and
  link the PR from the issue. Use `Refs #N` for ongoing management or tracking
  issues; use `Closes #N` only when merging completes the entire issue.
- Verify claims against the code or a safe reproduction. Keep comments and PR
  descriptions concise and current; state what remains unverified.
- Review in proportion to risk. Documentation and test-only changes get
  self-review. Behavior changes get one independent review of a pinned diff;
  focus high-risk changes on data loss, startup, installation and workspace
  safety. Give the reviewer the intended outcome and relevant integration
  boundaries, not an expanding checklist.
- Fix confirmed behavioral defects and violations of acceptance criteria.
  Wording, naming, optional coverage and comment tidies do not trigger rounds.
  Allow at most one focused follow-up to check substantive fixes. If defects
  remain, stop and simplify or ask the maintainer to choose a smaller scope;
  do not merge unresolved defects or restart an unbounded review loop.
- Run the project's relevant tests and checks before delivery. Build packages
  when packaging or installation changes; do not generate version bumps for
  review iterations. Document user-visible changes.
- Deliver the PR with its outcome, validation and remaining limitations.
  A person reviews and merges; do not merge or close the issue yourself.
