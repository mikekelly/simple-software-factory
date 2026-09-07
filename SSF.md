# Notes for ssf agents

- Post a short comment on the issue when you start, when you need a decision, and when you finish.
- Ask on the issue rather than guessing when the request is ambiguous; you are woken up when someone answers.
- Commit as you go.
- Work on the issue's branch and open a PR that references the issue (`Closes #N`), then comment on the issue with the link. Do not close the issue or merge the PR yourself: once the gauntlet has passed, say so on the issue and leave the merge to the maintainer or the project-management session.
- Keep `cargo test` green and run `cargo fmt` and `cargo clippy` before pushing.
- Update `README.md` (or the right file under `docs/`) and `config.example.toml` for any user-visible behaviour.
- A change to setup, configuration, commands or operating behaviour also updates `skills/ssf-setup/SKILL.md` in the same PR; the skill is what a coding agent follows to set ssf up, so a PR that leaves it stale is not done.
- Rebuild the package with `cd packaging && makepkg -fd` before calling something done; commit the `pkgver` bump makepkg makes to `packaging/PKGBUILD`.
- The installed service runs the last package the maintainer installed, so verify daemon behaviour with unit tests and scratch `SSF_CONFIG_DIR`/`SSF_STATE_DIR` runs rather than expecting to see your change live.
- Before you call work done, put it through a gauntlet: hand the diff, the issue and your claim of what the change does to a fresh agent that has not seen your reasoning, and ask it to break it (correctness first, then whether it does what the issue asked, then tests, docs, `config.example.toml` and `skills/ssf-setup/SKILL.md` for anything user-visible, then the project's conventions). Fix what it finds and run the gauntlet again until it finds nothing that matters. A subagent of your own harness is the default; for work that is complex, risky or important, use herdr to have a different agent and model look (`ssf guide` has the invocation). Say on the issue what the gauntlet found and what you changed; nobody re-reviews after you.
