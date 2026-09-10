# Notes for ssf agents

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
- Run `cargo test`, `cargo fmt --check` and `cargo clippy --all-targets`
  on the final code change. Use focused tests while fixing defects; rerun
  broader checks only when changes warrant it. Documentation-only changes
  need a content/link check, not a package build.
- Build with `cd packaging && makepkg -fd` when changing packaging or
  installation. Commit its generated `pkgver` only after a successful build;
  do not rebuild packages and bump versions for each review round.
- Update relevant user documentation and configuration examples for behavior
  changes, and `skills/ssf-setup/SKILL.md` when its setup or operating guidance
  changes. Test daemon behavior in isolation; see `docs/development.md`
  before running scratch instances.
- Own completion: finish and close out work within your delegated authority.
  Leave merging to the maintainer or project-management session.
  Do not close an issue whose outcome still depends on an unmerged PR.
  Deliver the outcome, validation and remaining limitations on the owning
  issue with the PR link. When further action is outside your authority,
  explicitly @mention an appropriate human collaborator or request their
  review, naming the concrete decision or action needed and who owns it;
  “ready” or “pending review” alone is not a handoff.
