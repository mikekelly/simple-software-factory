//! Reading a session's variables back out of an ancestor process.
//!
//! The chain from a pane to a scrubbed tool of its harness, and the shim run
//! from there, is exercised end to end in `tests/shim_environment.rs`.

use super::*;

fn entries(environ: &str) -> Vec<(String, String)> {
    parse(environ.as_bytes())
        .into_iter()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

fn names(environ: &str) -> Vec<String> {
    entries(environ).into_iter().map(|(name, _)| name).collect()
}

#[test]
fn the_identity_variables_come_out_of_the_ancestor_environment() {
    let environ = "HOME=/home/a\0SSF_REPO=o/r\0SSF_ISSUE=7\0SSF_CONFIG_DIR=/c\0\
         SSF_DELIVERY_MAILBOX=/m\0GH_CONFIG_DIR=/c/gh\0GH_TOKEN=t\0GITHUB_TOKEN=t\0\
         GIT_SSH_COMMAND=ssh -i /k\0GIT_CONFIG_COUNT=2\0GIT_CONFIG_KEY_0=user.name\0\
         GIT_CONFIG_VALUE_0=bot\0GIT_AUTHOR_NAME=bot\0GH_FORCE_TTY=1\0PATH=/usr/bin\0LANG=C\0";
    // Whole families, not a list of names, so a variable `ssf launch` starts
    // exporting is carried too; everything else in the pane is left where it
    // is. The values are handed to a program that would otherwise read the
    // operator's gh configuration and gitconfig.
    assert_eq!(
        names(environ),
        vec![
            "SSF_REPO",
            "SSF_ISSUE",
            "SSF_CONFIG_DIR",
            "SSF_DELIVERY_MAILBOX",
            "GH_CONFIG_DIR",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GIT_SSH_COMMAND",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
            "GIT_AUTHOR_NAME",
            "GH_FORCE_TTY",
        ]
    );
    // Values keep their bytes, spaces and all.
    let ssh = entries(environ)
        .into_iter()
        .find(|(name, _)| name == "GIT_SSH_COMMAND")
        .map(|(_, value)| value);
    assert_eq!(ssh.as_deref(), Some("ssh -i /k"));
}

#[test]
fn a_variable_without_a_value_and_stray_bytes_are_skipped() {
    // `ssf launch` removes SSF_ROLE and SSF_SERVER from the pane; an old one
    // in an outer shell is not this session's either. A malformed entry is
    // not an environment variable at all.
    assert_eq!(
        entries("SSF_REPO=o/r\0no_equals\0=empty_name\0SSF_CONFIG_DIR=\0"),
        vec![
            ("SSF_REPO".to_string(), "o/r".to_string()),
            ("SSF_CONFIG_DIR".to_string(), String::new()),
        ]
    );
}

#[test]
fn only_a_marker_with_its_own_name_marks_a_session() {
    // A person's shell has none of the session's variables; neither has a
    // process whose only similar-looking name is a different one.
    assert!(!carries_marker(b"HOME=/home/a\0PATH=/usr/bin\0"));
    assert!(!carries_marker(b"SSF_REPO_SUFFIX=o/r\0"));
    assert!(carries_marker(b"PATH=/usr/bin\0SSF_REPO=o/r\0"));
    // An empty value still marks the process as the session's.
    assert!(carries_marker(b"SSF_REPO=\0"));
}

#[test]
fn a_variable_the_process_already_has_is_left_alone() {
    let recovered = parse(b"SSF_REPO=o/r\0GH_TOKEN=from-the-ancestor\0");
    let kept = missing(recovered, |name| name == OsStr::new("GH_TOKEN"));
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].0, OsStr::new("SSF_REPO"));
    assert_eq!(kept[0].1, OsStr::new("o/r"));
}

#[test]
fn the_parent_comes_after_the_command_name_in_the_parens() {
    // Field 4 of `/proc/<pid>/stat` is the parent, and the command name in
    // parentheses before it may itself contain spaces and parentheses, so the
    // fields are read from after the last `)`.
    let stat = "42 (omp tool (x)) S 7 42 42 0 -1 4194304 1 0 0 0 0 0 0 0 20 0 1 0 0 0";
    assert_eq!(
        stat.rsplit_once(") ").unwrap().1.split_whitespace().nth(1),
        Some("7")
    );
    // And a real one names a process that exists.
    assert!(parent_of(std::process::id()).is_some());
}
