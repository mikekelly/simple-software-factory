# Notes for ssf agents

- Post a short comment on the issue when you start, when you need a decision, and when you finish.
- Ask on the issue rather than guessing when the request is ambiguous; you are woken up when someone answers.
- Commit as you go.
- Work on the issue's branch and open a PR that references the issue (`Closes #N`), then comment on the issue with the link. Do not close the issue or merge the PR yourself; a human reviews and merges.
- Keep `cargo test` green and run `cargo fmt` and `cargo clippy` before pushing.
- Update `README.md` and `config.example.toml` for any user-visible behaviour.
- Rebuild the package with `cd packaging && makepkg -fd` before calling something done; commit the `pkgver` bump makepkg makes to `packaging/PKGBUILD`.
- The installed service runs the last package the maintainer installed, so verify daemon behaviour with unit tests and scratch `SSF_CONFIG_DIR`/`SSF_STATE_DIR` runs rather than expecting to see your change live.
- When reviewing, do it the way a careful colleague would: correctness first, then whether the change does what the issue asked, then tests, docs and the project's conventions. Be specific, point at files and lines, and say what would make it mergeable.
