//! A harness tool that starts with a scrubbed environment must still reach
//! the factory as the session rather than as the operator (#367).
//!
//! The chain reproduced here is the one observed live on this machine: the
//! pane's environment (`ssf launch`), a child of it that kept only `HOME` and
//! `PATH` (OMP's Python tool runner), and the `gh` and `git` shims run from
//! that child. The shims exec the real programs, so the environment they were
//! handed is what a post or a push would act with: `gh` posts as whoever the
//! operator is signed in as without `GH_CONFIG_DIR`/`GH_TOKEN`, and stamps no
//! byline without `SSF_REPO`; `git push` uses the operator's key and
//! credential helper without `GIT_SSH_COMMAND` and the `GIT_CONFIG_*` entries.

#[cfg(target_os = "linux")]
mod linux {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// What the pane was launched with.
    const SESSION: &[(&str, &str)] = &[
        ("SSF_REPO", "acme/widgets"),
        ("SSF_ISSUE", "12"),
        ("SSF_ISSUE_URL", "https://github.com/acme/widgets/issues/12"),
        ("SSF_BOT", "widgets-bot"),
        ("SSF_CONFIG_DIR", "/factory/ssf"),
        ("SSF_STATE_DIR", "/factory/state"),
        ("GH_CONFIG_DIR", "/factory/ssf/gh"),
        ("GH_TOKEN", "session-token"),
        ("GITHUB_TOKEN", "session-token"),
        (
            "GIT_SSH_COMMAND",
            "ssh -i /factory/keys/widgets-bot_ed25519 -o IdentitiesOnly=yes",
        ),
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "user.name"),
        ("GIT_CONFIG_VALUE_0", "widgets-bot"),
    ];

    fn client() -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_ssf"))
    }

    /// A directory of `gh` and `git` stand-ins that print the environment they
    /// were exec'd with, and a directory linking the shims to the client.
    fn fixtures(root: &Path) -> (PathBuf, PathBuf) {
        let (shim, tools) = (root.join("shim"), root.join("tools"));
        for dir in [&shim, &tools] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let watched = SESSION
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(" ");
        for name in ["gh", "git"] {
            std::os::unix::fs::symlink(client(), shim.join(name)).unwrap();
            let tool = tools.join(name);
            std::fs::write(
                &tool,
                format!("#!/bin/sh\nfor v in {watched}; do\n  eval \"echo $v=\\$$v\"\ndone\n"),
            )
            .unwrap();
            std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
        }
        (shim, tools)
    }

    /// Run `tool args` the way the harness does: the pane's shell carries the
    /// session, the tool's runner is a *child* of it that kept only `HOME`,
    /// `PATH` and `own` (OMP's Python tool), and `PATH` still leads to the
    /// shim directory. The trailing `:` keeps the pane shell alive as that
    /// child's parent instead of being replaced by it.
    fn scrubbed(shim: &Path, tools: &Path, tool: &str, args: &str, own: &[(&str, &str)]) -> String {
        let path = format!("{}:{}:/usr/bin", shim.display(), tools.display());
        let mut pane = Command::new("sh");
        // Nothing ambient: the test's own environment may be a session's
        // already, and the point is what the shim recovers on its own.
        pane.env_clear()
            .env("PATH", &path)
            .env("HOME", "/home/pane");
        for (name, value) in SESSION {
            pane.env(name, value);
        }
        let own = own
            .iter()
            .map(|(name, value)| format!(" {name}={value}"))
            .collect::<String>();
        let out = pane
            .arg("-c")
            .arg(format!(
                "env -i HOME=/home/runner PATH={path}{own} {tool} {args}; :"
            ))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "{tool}: {stdout}");
        stdout
    }

    #[test]
    fn the_gh_and_git_shims_hand_the_real_programs_the_session_environment() {
        let root = std::env::temp_dir().join(format!("ssf-shim-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (shim, tools) = fixtures(&root);
        // `git push` is not a tagged command, so its arguments pass through
        // untouched; `gh issue comment` is a post, and is stamped as well (the
        // byline's own tests cover the body).
        for (tool, args) in [
            ("gh", "issue comment 12 --body hi"),
            ("git", "push origin HEAD"),
        ] {
            let stdout = scrubbed(&shim, &tools, tool, args, &[]);
            for (name, value) in SESSION {
                assert!(
                    stdout.contains(&format!("{name}={value}\n")),
                    "{tool} ran without {name}={value}: {stdout}"
                );
            }
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A value the tool was given itself is not overwritten: the shim puts
    /// back what a scrubber dropped, it does not impose its session's on a
    /// program that was handed its own.
    #[test]
    fn a_value_the_tool_was_given_itself_wins() {
        let root = std::env::temp_dir().join(format!("ssf-shim-own-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (shim, tools) = fixtures(&root);
        let stdout = scrubbed(
            &shim,
            &tools,
            "gh",
            "--version",
            &[
                ("GH_TOKEN", "tool-token"),
                ("GIT_CONFIG_VALUE_0", "someone-else"),
            ],
        );
        assert!(
            stdout.contains("GH_TOKEN=tool-token\n")
                && stdout.contains("GIT_CONFIG_VALUE_0=someone-else\n"),
            "the tool's own values were replaced: {stdout}"
        );
        assert!(
            stdout.contains("SSF_REPO=acme/widgets\n") && stdout.contains("GIT_CONFIG_COUNT=1\n"),
            "the rest of the session was not put back: {stdout}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// ssf's own gh calls are not routed through the wrapper, so a variable
    /// they clear on purpose stays cleared. `ssf git-credential` reads a token
    /// for another account with `gh auth token --user` after removing
    /// `GH_CONFIG_DIR` and `GH_TOKEN`, which is what makes gh read that
    /// account from the operator's own store; the wrapper's recovery would put
    /// the session's values back and look in ssf's account-less directory
    /// instead, failing the push.
    #[test]
    fn ssf_itself_reaches_gh_without_the_shim() {
        let root = std::env::temp_dir().join(format!("ssf-ghcli-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (shim, tools) = fixtures(&root);
        let config = root.join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.toml"),
            "[git]\nname = \"Widgets Bot\"\nemail = \"widgets-bot@example.com\"\n\
             credential = \"token:ann\"\n",
        )
        .unwrap();
        // The real gh records the environment it was run with, and answers the
        // one call ssf makes here.
        let log = root.join("gh-env");
        std::fs::write(
            tools.join("gh"),
            format!(
                "#!/bin/sh\nenv >> {}\nif [ \"$1 $2\" = \"auth token\" ]; then echo anna-token; fi\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            tools.join("gh"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let path = format!("{}:{}:/usr/bin", shim.display(), tools.display());
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "printf 'protocol=https\\nhost=github.com\\n\\n' | '{}' git-credential get",
                client().display()
            ))
            .env_clear()
            .env("PATH", &path)
            .env("HOME", "/home/pane")
            .env("SSF_REPO", "acme/widgets")
            .env("SSF_ISSUE", "12")
            .env("SSF_BOT", "widgets-bot")
            .env("SSF_CONFIG_DIR", &config)
            .env("GH_CONFIG_DIR", config.join("gh"))
            .env("GH_TOKEN", "session-token")
            .env("SSF_TEST_LOG", &log)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{stdout}");
        assert!(
            stdout.contains("password=anna-token"),
            "the token for the other account was not read: {stdout}"
        );
        let seen = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(!seen.is_empty(), "the real gh never ran: {stdout}");
        for cleared in ["GH_CONFIG_DIR=", "GH_TOKEN=", "GITHUB_TOKEN="] {
            assert!(
                !seen.contains(cleared),
                "the wrapper put {cleared} back into a call that cleared it: {seen}"
            );
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}
