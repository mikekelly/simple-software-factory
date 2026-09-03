# Notes for ssf agents

- Work on the issue's branch and open a PR that references the issue.
- Keep `cargo test` green and run `cargo fmt` and `cargo clippy` before pushing.
- Update `README.md` and `config.example.toml` for any user-visible behaviour.
- Rebuild the package with `cd packaging && makepkg -fd` before calling something done; commit the `pkgver` bump makepkg makes to `packaging/PKGBUILD`.
- The installed service runs the last package the maintainer installed, so verify daemon behaviour with unit tests and scratch `SSF_CONFIG_DIR`/`SSF_STATE_DIR` runs rather than expecting to see your change live.
