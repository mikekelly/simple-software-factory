# Repository instructions

- Work on the issue branch, read `git diff --cached` before committing, and
  link the PR from the issue. Use `Refs #N` for ongoing management or tracking
  issues; use `Closes #N` only when merging completes the entire issue.
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
