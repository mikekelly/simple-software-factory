use super::*;

#[test]
fn large_bodies_survive_exec_transport() {
    for text in [
        "x".repeat(120_000),
        "界".repeat(60_000),
        "x".repeat(2_000_000),
    ] {
        let read = |_: &str| Ok(text.clone());
        let origin = o();
        let mut shim = shim(&origin);
        shim.read = &read;
        let rewritten = shim.rewrite(args(&["pr", "create", "-F", "-", "-t", "title"]));
        let mut files = Vec::new();
        let transported = transport_bodies(rewritten, &mut files).unwrap();
        assert_eq!(transported[2], "--body-file");
        // A real child process must be able to open the inherited fd,
        // and argv must carry only its path, not the oversized text.
        let output = std::process::Command::new("cat")
            .arg(&transported[3])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{}\n\n{text}", line())
        );
    }
}

#[test]
fn only_the_last_file_is_read_and_mixed_sources_stay_invalid() {
    let reads = std::cell::RefCell::new(Vec::new());
    let read = |path: &str| {
        reads.borrow_mut().push(path.to_string());
        if path == "missing" {
            Err(std::io::Error::other("missing"))
        } else {
            Ok("final body".to_string())
        }
    };
    let origin = o();
    let mut shim = shim(&origin);
    shim.read = &read;
    for first in ["missing", "-"] {
        reads.borrow_mut().clear();
        let out = shim.rewrite(args(&["issue", "comment", "3", "-F", first, "-F", "final"]));
        assert_eq!(*reads.borrow(), vec!["final"]);
        assert!(out.last().unwrap().ends_with("final body"));
    }
    reads.borrow_mut().clear();
    let a = args(&["issue", "comment", "3", "-F", "-", "-F", "missing"]);
    assert_eq!(shim.rewrite(a.clone()), a);
    assert_eq!(*reads.borrow(), vec!["missing"]);
    reads.borrow_mut().clear();
    let a = args(&["pr", "comment", "3", "-F", "-", "--body", "hello"]);
    assert_eq!(shim.rewrite(a.clone()), a);
    assert!(reads.borrow().is_empty());
}

#[test]
fn invalid_utf8_and_stdin_read_errors_fail_closed() {
    let origin = o();
    let mut shim = shim(&origin);
    let invalid = |_: &str| {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid UTF-8",
        ))
    };
    shim.read = &invalid;
    for path in ["-", "invalid.md"] {
        assert!(
            shim.try_rewrite(args(&["issue", "comment", "3", "-F", path]))
                .is_err()
        );
    }
    shim.read = &no_files;
    assert!(
        shim.try_rewrite(args(&["issue", "comment", "3", "-F", "-"]))
            .is_err()
    );
    use std::os::fd::AsRawFd;
    let file = body_file("test").unwrap();
    use std::io::Write;
    (&file).write_all(&[0xff]).unwrap();
    assert_eq!(
        read_body_file(&format!("/dev/fd/{}", file.as_raw_fd()))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn disabled_review_actions_do_not_suppress_approval_attribution() {
    for flags in [
        vec!["-r", "--request-changes=false"],
        vec!["-c", "--comment=false"],
        vec!["-rr=false"],
    ] {
        let mut a = args(&["pr", "review", "3", "--approve", "--body", ""]);
        a.extend(args(&flags));
        assert!(rewrite(a).join("\n").contains(&line()));
    }
}

#[test]
fn explicit_blank_reviews_fail_before_posting() {
    for action in [
        "--comment",
        "--request-changes",
        "-c",
        "-r",
        "--request-changes=true",
        "-r=true",
    ] {
        for body in ["", " ", "\n\t"] {
            let a = args(&["pr", "review", "3", action, "--body", body]);
            let origin = o();
            assert!(shim(&origin).try_rewrite(a).is_err());
            let read = |_: &str| Ok(body.to_string());
            let mut shim = shim(&origin);
            shim.read = &read;
            assert!(
                shim.try_rewrite(args(&["pr", "review", "3", action, "-F", "-"]))
                    .is_err()
            );
        }
    }
}

#[test]
fn body_files_move_inline() {
    let read = |p: &str| -> std::io::Result<String> {
        assert!(p == "notes.md" || p == "-");
        Ok("from file\n".into())
    };
    let o = o();
    let mut s = shim(&o);
    s.read = &read;
    let expect = format!("{}\n\nfrom file", line());
    let out = s.rewrite(args(&[
        "pr",
        "create",
        "-t",
        "t",
        "--body-file",
        "notes.md",
    ]));
    assert_eq!(out, args(&["pr", "create", "-t", "t", "--body", &expect]));
    let out = s.rewrite(args(&["pr", "create", "-t", "t", "-F", "-"]));
    assert_eq!(out, args(&["pr", "create", "-t", "t", "--body", &expect]));
    let out = s.rewrite(args(&["issue", "comment", "1", "--body-file=notes.md"]));
    assert_eq!(
        out,
        args(&["issue", "comment", "1", &format!("--body={expect}")])
    );
    let out = s.rewrite(args(&[
        "issue",
        "comment",
        "1",
        "-Fnotes.md",
        "-R",
        "acme/widgets",
    ]));
    assert_eq!(
        out,
        args(&[
            "issue",
            "comment",
            "1",
            &format!("--body={expect}"),
            "-R",
            "acme/widgets"
        ])
    );
    // Unreadable file: untouched, gh reports it.
    let a = args(&["issue", "comment", "1", "--body-file", "missing"]);
    assert_eq!(rewrite(a.clone()), a);
}

#[test]
fn reviews_without_a_body_get_one() {
    let out = rewrite(args(&["pr", "review", "7", "--approve"]));
    assert_eq!(
        out,
        args(&["pr", "review", "--body", &line(), "7", "--approve"])
    );
    let out = rewrite(args(&[
        "pr",
        "review",
        "7",
        "--approve",
        "-R",
        "acme/other",
    ]));
    assert_eq!(
        out,
        args(&[
            "pr",
            "review",
            "--body",
            &o().first_line(None, false),
            "7",
            "--approve",
            "-R",
            "acme/other"
        ])
    );
}

/// Every spelling gh reads as an approval, and every spelling it
/// does not. Each was run against the real gh binary -- not the one
/// on a session's PATH, which is this shim -- on a pull request
/// number that does not exist. What tells the two groups apart is
/// gh's own "--approve, --request-changes, or --comment required":
/// the approvals get past that line and fail on something later, the
/// number or a blank body, and the rest stop there or, for a value
/// it cannot parse, before it.
///
/// Only an approval gets a body it did not ask for. `--comment` and
/// `--request-changes` are actions too, and are read as such
/// everywhere else, but gh refuses them without a body of their own
/// and that refusal is the right answer: a review carrying nothing
/// but a byline says nothing, and a request for changes carrying
/// nothing but a byline blocks the pull request.
#[test]
fn approvals_are_seen_in_every_spelling() {
    for a in [
        // Bare, long and short.
        args(&["pr", "review", "7", "--approve"]),
        args(&["pr", "review", "7", "-a"]),
        // Attached: gh parses the value, so a true one approves.
        args(&["pr", "review", "7", "--approve=true"]),
        args(&["pr", "review", "7", "--approve=1"]),
        args(&["pr", "review", "7", "--approve=True"]),
        args(&["pr", "review", "7", "--approve=TRUE"]),
        args(&["pr", "review", "7", "-a=true"]),
        args(&["pr", "review", "7", "-a=t"]),
        args(&["pr", "review", "7", "-a=T"]),
        // Clustered. `-ac` and `-ra` name two actions, so gh will
        // not run them either way, but the approval is there and
        // this reading finds it. `-aR o/r` is the one cluster here
        // that gh runs.
        args(&["pr", "review", "7", "-ac"]),
        args(&["pr", "review", "7", "-ra"]),
    ] {
        let mut want = a.clone();
        want.splice(2..2, [args(&["--body"])[0].clone(), line()]);
        assert_eq!(rewrite(a.clone()), want, "{a:?}");
    }
    // The approval is clustered ahead of a flag that takes a value,
    // and that value is not a flag of its own.
    assert_eq!(
        rewrite(args(&["pr", "review", "7", "-aR", "acme/widgets"])),
        args(&[
            "pr",
            "review",
            "--body",
            &line(),
            "7",
            "-aR",
            "acme/widgets"
        ])
    );
    for a in [
        // An action, but one gh will not post without a body of its
        // own: left alone, so gh asks for the body rather than this
        // inventing one.
        args(&["pr", "review", "7", "--comment"]),
        args(&["pr", "review", "7", "-c"]),
        args(&["pr", "review", "7", "--comment=true"]),
        args(&["pr", "review", "7", "-c=true"]),
        args(&["pr", "review", "7", "--request-changes"]),
        args(&["pr", "review", "7", "-r"]),
        args(&["pr", "review", "7", "--request-changes=TRUE"]),
        args(&["pr", "review", "7", "-r=true"]),
        args(&["pr", "review", "7", "-cR", "acme/widgets"]),
        // The flag is there, the approval is not: gh refuses the
        // line for want of an action, so a body would be a body on a
        // review that never posts.
        args(&["pr", "review", "7", "--approve=false"]),
        args(&["pr", "review", "7", "--approve=F"]),
        args(&["pr", "review", "7", "-a=false"]),
        args(&["pr", "review", "7", "-a=0"]),
        // gh refuses a value it cannot parse outright.
        args(&["pr", "review", "7", "--approve=yes"]),
        // A `--` ends the flags, so what follows is a positional.
        args(&["pr", "review", "--", "--approve"]),
        // No action at all.
        args(&["pr", "review", "7"]),
        args(&["pr", "review", "7", "-R", "acme/widgets"]),
    ] {
        assert_eq!(rewrite(a.clone()), a, "{a:?}");
    }
}
