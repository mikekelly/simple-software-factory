use super::*;

/// The word after a flag that takes a value is that value even when
/// it is a `--`, for gh's command lookup as much as for its flags:
/// cobra breaks on a `--` only when it is the argument it is looking
/// at. `gh -b -- issue comment 1` is a comment whose body is `--`,
/// and it posts; reading that `--` as the end of the flags left the
/// pair unfound and the post untagged.
///
/// The second line here is the one shape this cannot reach. gh runs
/// it -- it approves the current branch's pull request -- but the
/// insert would land past a `--` that really is gh's, among the
/// positionals, and gh answers `accepts at most 1 arg(s)`. Ahead of
/// the `--` is worse: cobra pairs the `-a` with the `--body` and
/// reads the byline as the command. So it is left alone.
#[test]
fn a_swallowed_double_dash_does_not_hide_the_command() {
    let out = rewrite(args(&["-b", "--", "issue", "comment", "1"]));
    assert_eq!(
        out,
        args(&["-b", &format!("{}\n\n--", line()), "issue", "comment", "1"])
    );
    let a = args(&["pr", "-a", "--", "review"]);
    assert_eq!(rewrite(a.clone()), a);
}

/// A cluster before the command words does not take the next
/// argument as its value: cobra removes the pair from the list
/// before pflag parses it, so gh binds the value to the first word
/// after the pair. `gh -aF pr review notes.md 3` reads `notes.md`,
/// and `gh -db pr create hello --title t` is a create whose body is
/// `hello`; both were run against gh. Reading the command
/// word as the value instead would stamp it and hand gh `unknown
/// command "<byline>\n\npr"`, turning a line it accepts into an
/// error. Normalizing the command position lets every scan bind the
/// same value that gh does.
#[test]
fn a_value_past_the_command_words_is_stamped() {
    let origin = o();
    let read = |p: &str| Ok(format!("read {p}"));
    let mut shim = shim(&origin);
    shim.read = &read;
    for a in [
        args(&["-aF", "pr", "review", "notes.md", "999999"]),
        args(&["-ab", "pr", "review", "hello", "999999"]),
        args(&["-db", "pr", "create", "hello", "--title", "t"]),
        // The long spellings reach the same place whenever a
        // value-less flag ahead of them keeps the command words out
        // of reach: `gh -d --body pr create hello -t T` is a create
        // whose body is `hello`.
        args(&["-d", "--body", "pr", "create", "hello", "-t", "T"]),
        args(&["-d", "--body-file", "pr", "create", "hello", "-t", "T"]),
        args(&["pr", "-ab", "review", "hello", "999999"]),
        args(&["pr", "-dF", "create", "notes.md", "--title", "t"]),
    ] {
        let out = shim.rewrite(a.clone());
        assert!(out.join("\n").contains(&line()), "{a:?} -> {out:?}");
        assert!(matches!(out[0].as_str(), "pr"));
        assert!(matches!(out[1].as_str(), "create" | "review"));
        assert!(!out.join("\n").contains("read pr"));
    }
    // Attached, the value is where it looks, and this is stamped.
    let out = rewrite(args(&["-abhello", "pr", "review", "999999"]));
    assert_eq!(
        out,
        args(&[&format!("-ab{}\n\nhello", line()), "pr", "review", "999999"])
    );
}

/// A `--` is the end of the flags only where gh reads it as one. A
/// flag that takes a value takes this one: `gh pr review 3 -F --`
/// looks for a file called `--`, and both lines below post. Read as
/// a terminator, the first loses its approval and goes out untagged
/// and the second gets a second `--body` that gh prefers over the
/// agent's, which is the tag gone.
#[test]
fn a_double_dash_a_flag_swallows_is_not_the_end_of_the_flags() {
    assert_eq!(
        rewrite(args(&[
            "pr",
            "review",
            "7",
            "-R",
            "--",
            "-a",
            "--repo",
            "acme/widgets",
        ])),
        args(&[
            "pr",
            "review",
            "--body",
            &line(),
            "7",
            "-R",
            "--",
            "-a",
            "--repo",
            "acme/widgets",
        ])
    );
    assert_eq!(
        rewrite(args(&[
            "pr",
            "review",
            "7",
            "-aR",
            "--",
            "-b",
            "hi",
            "--repo",
            "acme/widgets",
        ])),
        args(&[
            "pr",
            "review",
            "7",
            "-aR",
            "--",
            "-b",
            &format!("{}\n\nhi", line()),
            "--repo",
            "acme/widgets",
        ])
    );
    // A `--` after the insert position is no reason to skip it:
    // `gh pr review --approve -- 7` is a line gh runs, and so is
    // the rewrite of it.
    assert_eq!(
        rewrite(args(&["pr", "review", "--approve", "--", "7"])),
        args(&["pr", "review", "--body", &line(), "--approve", "--", "7"])
    );
    // A `--` of its own still ends them: gh answers `--approve,
    // --request-changes, or --comment required` here, which is
    // exactly the reading being pinned -- past the `--` it sees a
    // selector and not an approval.
    let a = args(&["pr", "review", "--", "--approve"]);
    assert_eq!(rewrite(a.clone()), a);
}

/// A body inside a shorthand cluster is still a body. Reading the
/// action flag without reading this would put a second `--body` on
/// the line, and gh takes the last: the agent's text would be
/// dropped.
#[test]
fn bodies_inside_a_cluster_are_stamped() {
    let body = format!("{}\n\nhello", line());
    // The cluster is re-emitted whole with the body attached to it,
    // however the body arrived, so that nothing on the line becomes
    // a bare word cobra could read as the command.
    for (a, want) in [
        (
            args(&["pr", "review", "7", "-ab", "hello"]),
            args(&["pr", "review", "7", &format!("-ab{body}")]),
        ),
        (
            args(&["pr", "review", "7", "-abhello"]),
            args(&["pr", "review", "7", &format!("-ab{body}")]),
        ),
        (
            args(&["pr", "review", "7", "-ab=hello"]),
            args(&["pr", "review", "7", &format!("-ab{body}")]),
        ),
        (
            args(&["pr", "review", "7", "-cb", "hello"]),
            args(&["pr", "review", "7", &format!("-cb{body}")]),
        ),
        (
            args(&["pr", "review", "7", "-rb", "hello"]),
            args(&["pr", "review", "7", &format!("-rb{body}")]),
        ),
        // Every value-less letter a body can legally sit behind.
        (
            args(&["pr", "create", "-t", "t", "-db", "hello"]),
            args(&["pr", "create", "-t", "t", &format!("-db{body}")]),
        ),
        (
            args(&["pr", "create", "-t", "t", "-fb", "hello"]),
            args(&["pr", "create", "-t", "t", &format!("-fb{body}")]),
        ),
        (
            args(&["pr", "create", "-t", "t", "-wb", "hello"]),
            args(&["pr", "create", "-t", "t", &format!("-wb{body}")]),
        ),
        // `-e` is dead next to a body on the two comment commands,
        // where gh refuses `--editor` alongside `--body`, but it is
        // live on the two creates, which prompt in a terminal.
        (
            args(&["issue", "create", "-t", "t", "-eb", "hello"]),
            args(&["issue", "create", "-t", "t", &format!("-eb{body}")]),
        ),
        // The attached form on its own, which used to come out as a
        // body of `=hello`.
        (
            args(&["issue", "comment", "7", "-b=hello"]),
            args(&["issue", "comment", "7", &format!("-b{body}")]),
        ),
        // A letter followed by nothing but `=` is not that form:
        // pflag reads the `=` as the value, and so does this.
        (
            args(&["issue", "comment", "7", "-b="]),
            args(&["issue", "comment", "7", &format!("-b{}\n\n=", line())]),
        ),
    ] {
        assert_eq!(rewrite(a.clone()), want, "{a:?}");
    }
    // The body flag comes first, so the letter after it is its
    // value: this is a comment of `a` on item 7, which is what gh
    // makes of it too. The same shape on a review is a line gh
    // refuses, since `7` and the word after it are then two
    // positionals.
    assert_eq!(
        rewrite(args(&["issue", "comment", "-ba", "7"])),
        args(&["issue", "comment", &format!("-b{}\n\na", line()), "7"])
    );
}

#[test]
fn body_files_inside_a_cluster_move_inline() {
    let read = |p: &str| -> std::io::Result<String> { Ok(format!("read {p}")) };
    let o = o();
    let mut shim = shim(&o);
    shim.read = &read;
    let body = format!("--body={}\n\nread notes.md", line());
    // The letters ahead of the `-F` survive the move to `--body`,
    // and the body attaches to that flag with an `=` rather than
    // standing as a word of its own: those letters are a word, and
    // cobra pairs a two-character one with the word after it while
    // it looks for the command. Three words and `gh -aF notes.md pr
    // review 7` comes back `unknown command`, which was checked
    // against the real gh.
    assert_eq!(
        shim.rewrite(args(&["pr", "review", "7", "-aF", "notes.md"])),
        args(&["pr", "review", "7", "-a", &body])
    );
    assert_eq!(
        shim.rewrite(args(&["issue", "create", "-t", "t", "-eF", "notes.md"])),
        args(&["issue", "create", "-t", "t", "-e", &body])
    );
    // The shape that made it matter: gh takes a clustered body file
    // ahead of the command words, so the rewrite has to leave the
    // command findable. The value has to be attached for gh to take
    // it there -- as its own word it is the word cobra reads as the
    // command -- so this is the spelling that reaches the API, and
    // `gh -aF/dev/null pr review 999999` was run to confirm it.
    assert_eq!(
        shim.rewrite(args(&["-aFnotes.md", "pr", "review", "7"])),
        args(&["-a", &body, "pr", "review", "7"])
    );
    // `-e` on a create, which gh runs in a terminal.
    assert_eq!(
        shim.rewrite(args(&["pr", "create", "-t", "t", "-eF", "notes.md"])),
        args(&["pr", "create", "-t", "t", "-e", &body])
    );
    // One word in, one word out. Two would leave the body standing
    // as a bare word, and the `-d` ahead of it swallows the
    // `--body`, so cobra would read the byline as the command.
    assert_eq!(
        shim.rewrite(args(&[
            "-d",
            "--body-file=notes.md",
            "pr",
            "create",
            "-t",
            "t"
        ])),
        args(&[
            "-d",
            &format!("--body={}\n\nread notes.md", line()),
            "pr",
            "create",
            "-t",
            "t"
        ])
    );
    // A separated file value binds after command removal, as in gh.
    let a = args(&["-dF", "pr", "create", "notes.md", "-t", "t"]);
    assert_eq!(
        shim.rewrite(a),
        args(&["pr", "create", "-d", &body, "-t", "t"])
    );
    // The attached form, which used to make the shim read a file
    // called `=notes.md`, fail, and hand the line to gh unstamped.
    assert_eq!(
        shim.rewrite(args(&["pr", "create", "-t", "t", "-F=notes.md"])),
        args(&[
            "pr",
            "create",
            "-t",
            "t",
            &format!("--body={}\n\nread notes.md", line())
        ])
    );
    // With no letters to keep, the two-word form gh has always had
    // is left as it is: cobra pairs `--body` with the word after it,
    // so the command is still found.
    assert_eq!(
        shim.rewrite(args(&["-F", "notes.md", "pr", "review", "7", "-a"])),
        args(&[
            "--body",
            &format!("{}\n\nread notes.md", line()),
            "pr",
            "review",
            "7",
            "-a"
        ])
    );
}

/// The argument after a flag that takes a value is that value. Read
/// as a flag of its own it could be stamped, which would rewrite
/// somebody's title, and the body further along the line would be
/// the second `--body` on it.
#[test]
fn a_flags_value_is_not_read_as_a_flag() {
    let expect = format!("{}\n\nhello", line());
    for (a, want) in [
        (
            args(&["issue", "create", "--title", "-dbx", "--body", "hello"]),
            args(&["issue", "create", "--title", "-dbx", "--body", &expect]),
        ),
        (
            args(&["issue", "create", "-t", "-dbx", "-b", "hello"]),
            args(&["issue", "create", "-t", "-dbx", "-b", &expect]),
        ),
        (
            args(&["pr", "create", "--label", "-bx", "-t", "t", "-b", "hello"]),
            args(&["pr", "create", "--label", "-bx", "-t", "t", "-b", &expect]),
        ),
        // A clustered flag takes the next argument the same way.
        (
            args(&["pr", "create", "-dt", "-bx", "-b", "hello"]),
            args(&["pr", "create", "-dt", "-bx", "-b", &expect]),
        ),
        // The value belongs to the flag even where it reads as an
        // approval: `-b` takes the `-ac`, so the approval on this
        // line is the `--approve` and the body is the text, and no
        // second body is added.
        (
            args(&["pr", "review", "7", "-b", "-ac", "--approve"]),
            args(&[
                "pr",
                "review",
                "7",
                "-b",
                &format!("{}\n\n-ac", line()),
                "--approve",
            ]),
        ),
    ] {
        assert_eq!(rewrite(a.clone()), want, "{a:?}");
    }
}

/// Every flag these commands take a value for, named again here so
/// that dropping one from the lists in the source fails a test
/// rather than narrowing them quietly. The value used reads as a
/// flag: stepped over it is left as it is, read as a flag it comes
/// back with a byline in it and somebody's title or label rewritten.
#[test]
fn the_value_of_a_value_taking_flag_is_left_alone() {
    let body = format!("{}\n\nhello", line());
    // Everything `pr create` takes a value for, from its own help,
    // less the two body flags, which are read before this question
    // is asked: a `--body-file` naming no readable file hands the
    // whole line to gh rather than reaching the step-over at all.
    for f in [
        "--assignee",
        "--base",
        "--head",
        "--label",
        "--milestone",
        "--project",
        "--recover",
        "--reviewer",
        "--title",
        "-a",
        "-B",
        "-H",
        "-l",
        "-m",
        "-p",
        "-r",
        "-t",
    ] {
        assert_eq!(
            rewrite(args(&["pr", "create", f, "-dbx", "-t", "t", "-b", "hello"])),
            args(&["pr", "create", f, "-dbx", "-t", "t", "-b", &body]),
            "{f}"
        );
    }
    // Four of the entries cannot be shown this way, because gh
    // refuses any value shaped like a flag for them: a repository
    // has to look like one, and `--template` is refused beside a
    // body at all. Their rows pin the reading rather than a line gh
    // would run.
    for f in ["--repo", "-R", "--template", "-T"] {
        assert_eq!(
            rewrite(args(&["pr", "create", f, "-dbx", "-t", "t", "-b", "hello"])),
            args(&["pr", "create", f, "-dbx", "-t", "t", "-b", &body]),
            "{f}"
        );
    }
    // And the four more that only `issue create` takes.
    for f in ["--blocked-by", "--blocking", "--parent", "--type"] {
        assert_eq!(
            rewrite(args(&[
                "issue", "create", f, "-dbx", "-t", "t", "-b", "hello"
            ])),
            args(&["issue", "create", f, "-dbx", "-t", "t", "-b", &body]),
            "{f}"
        );
    }
}

#[test]
fn everything_else_passes_through() {
    for a in [
        args(&["pr", "list"]),
        args(&["issue", "view", "3"]),
        args(&["issue", "edit", "3", "--body", "x"]),
        args(&["api", "repos/a/b/issues", "-f", "body=x"]),
        args(&["pr", "create", "--fill"]),
        args(&["pr", "create", "-f"]),
        args(&["pr", "create", "--fill-first"]),
        args(&["pr", "create", "--fill-verbose"]),
        args(&["issue", "comment", "3", "--editor"]),
        args(&["issue", "comment", "3", "--", "--body", "x"]),
        args(&["--version"]),
        args(&[]),
    ] {
        assert_eq!(rewrite(a.clone()), a, "{a:?}");
    }
}

#[test]
fn malformed_flags_pass_through() {
    for a in [
        args(&["issue", "comment", "3", "-b"]),
        args(&["issue", "comment", "3", "--body"]),
        args(&["issue", "comment", "3", "--body-file"]),
    ] {
        assert_eq!(rewrite(a.clone()), a, "{a:?}");
    }
}

#[test]
fn already_tagged_bodies_are_left_alone() {
    for body in [
        format!("{}\n\ndone", line()),
        format!("{}\n\ndone", o().tag()),
        format!("{}\n\ndone", o().first_line(None, false)),
    ] {
        let a = args(&["issue", "comment", "3", "--body", &body]);
        assert_eq!(rewrite(a.clone()), a);
    }
    // A tag at the end, where posts used to carry it, is not the post's
    // own any more: the line goes on top.
    let old = format!("done\n\n{}", o().tag());
    let out = rewrite(args(&["issue", "comment", "3", "--body", &old]));
    assert_eq!(out[4], format!("{}\n\n{old}", line()));
}
