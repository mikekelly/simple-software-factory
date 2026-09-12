use super::*;

#[test]
fn stamps_inline_bodies_in_every_spelling() {
    let expect = format!("{}\n\nhello", line());
    for a in [
        args(&["issue", "comment", "3", "--body", "hello"]),
        args(&["issue", "comment", "3", "-b", "hello"]),
        args(&["issue", "comment", "3", "--body=hello"]),
        args(&["issue", "comment", "3", "-bhello"]),
        args(&["pr", "create", "--title", "t", "--body", "hello", "--draft"]),
        args(&["pr", "comment", "--body", "hello", "--repo", "acme/widgets"]),
        args(&["pr", "review", "--approve", "--body", "hello"]),
        args(&["issue", "create", "-t", "t", "-b", "hello"]),
    ] {
        let out = rewrite(a.clone());
        assert_eq!(out.len(), a.len(), "{a:?}");
        let joined = out.join("\x00");
        assert!(joined.contains(&expect), "{out:?}");
    }
}

#[test]
fn a_flag_before_the_subcommand_still_leaves_a_tagged_command() {
    // gh takes the repository flag on either side of the subcommand.
    // In its separated spellings the value is a bare word, so reading
    // position alone made it the command: the pair was not tagged,
    // the line went through untouched, and the post carried no byline
    // and no origin tag at all -- read afterwards as a person's.
    //
    // The tag is what is at stake: without it the post reads as a
    // person's. The byline's form follows the repository posted to,
    // which for the `acme/other` rows is the long one.
    let tag = o().tag();
    for a in [
        args(&[
            "--repo",
            "acme/other",
            "issue",
            "comment",
            "3",
            "--body",
            "hello",
        ]),
        args(&[
            "-R",
            "acme/other",
            "issue",
            "comment",
            "3",
            "--body",
            "hello",
        ]),
        // The inline spellings are one argument starting with `-`, so
        // they were never affected; pinned so a fix for the two above
        // cannot regress them.
        args(&[
            "--repo=acme/other",
            "issue",
            "comment",
            "3",
            "--body",
            "hello",
        ]),
        args(&["-Racme/other", "issue", "comment", "3", "--body", "hello"]),
        // Every tagged subcommand, not just `issue comment`.
        args(&[
            "-R",
            "acme/other",
            "issue",
            "create",
            "-t",
            "t",
            "-b",
            "hello",
        ]),
        args(&[
            "-R",
            "acme/other",
            "pr",
            "create",
            "--title",
            "t",
            "--body",
            "hello",
        ]),
        args(&["-R", "acme/other", "pr", "comment", "3", "--body", "hello"]),
        args(&[
            "-R",
            "acme/other",
            "pr",
            "review",
            "--approve",
            "--body",
            "hello",
        ]),
    ] {
        let out = rewrite(a.clone());
        let joined = out.join("\x00");
        assert!(joined.contains(&tag), "no origin tag: {a:?} -> {out:?}");
        assert!(joined.contains("says: "), "no byline: {a:?} -> {out:?}");
        assert!(joined.contains("hello"), "body lost: {a:?} -> {out:?}");
    }
    // A repository flag floating before the subcommand still decides
    // the byline's form: it is not in `args[s + 1..]`, so both scans
    // read the whole line.
    let long = format!("🤖acme/widgets#12 says: {tag}");
    let short = format!("🤖#12 says: {tag}");
    let out = rewrite(args(&[
        "--repo",
        "acme/other",
        "issue",
        "comment",
        "3",
        "--body",
        "hello",
    ]));
    assert!(out.join("\x00").contains(&long), "floating --repo: {out:?}");
    let out = rewrite(args(&[
        "-R",
        "acme/widgets",
        "issue",
        "comment",
        "3",
        "--body",
        "hello",
    ]));
    assert!(
        out.join("\x00").contains(&short),
        "floating -R, own repo: {out:?}"
    );
    // A body flag floats like any other. Left unstamped it posts
    // untagged, and on `pr review` the approval branch below would
    // then add a *second* `--body`: gh takes the last, so the
    // agent's own text would be silently dropped, and a
    // `--body-file` alongside it is refused outright.
    for a in [
        args(&["--body", "hello", "issue", "comment", "3"]),
        args(&["-b", "hello", "issue", "comment", "3"]),
        args(&["--body=hello", "issue", "comment", "3"]),
        args(&["-bhello", "issue", "comment", "3"]),
        args(&["--body", "hello", "pr", "review", "--approve"]),
    ] {
        let out = rewrite(a.clone());
        let joined = out.join("\x00");
        assert!(joined.contains(&tag), "no origin tag: {a:?} -> {out:?}");
        assert!(joined.contains("hello"), "body lost: {a:?} -> {out:?}");
        // Exactly one body reaches gh: a second would win and drop
        // the agent's text, and `--body` beside `--body-file` is an
        // error.
        assert_eq!(
            joined.matches("says: ").count(),
            1,
            "one stamped body only: {a:?} -> {out:?}"
        );
    }
    // The subcommand need not follow the command word: flags float
    // between them too, in every spelling. The inline ones carry
    // their own value, so gh reads the word after them as the
    // subcommand; treating them as value-taking loses the tag.
    for between in [
        vec!["--repo", "acme/other"],
        vec!["--repo=acme/other"],
        vec!["-Racme/other"],
        vec!["-R=acme/other"],
    ] {
        let mut a = vec!["issue".to_string()];
        a.extend(between.iter().map(|s| s.to_string()));
        a.extend(args(&["comment", "3", "--body", "hello"]));
        let out = rewrite(a.clone());
        assert!(
            out.join("\x00").contains(&tag),
            "flag between the words: {a:?} -> {out:?}"
        );
    }
    // The *command* word can be a flag's value too, not just the
    // subcommand: cobra has `--label` swallow `pr` here, so this is
    // an `issue create` and must be tagged as one.
    let out = rewrite(args(&[
        "--label", "pr", "issue", "create", "--title", "t", "--body", "hello",
    ]));
    assert!(
        out.join("\x00").contains(&tag),
        "a command word in a flag's value: {out:?}"
    );
    // A flag-shaped value does not derail the scan either way.
    let out = rewrite(args(&["pr", "-b", "-a", "review", "--repo=acme/widgets"]));
    assert!(
        out.join("\x00").contains(&tag),
        "a flag-shaped value: {out:?}"
    );
    // An empty argument is not a word to cobra, and gh posts this:
    // `pr review` with no selector reviews the current branch.
    let out = rewrite(args(&["", "pr", "review", "--approve"]));
    assert!(
        out.join("\x00").contains(&tag),
        "an empty argument is not a command word: {out:?}"
    );
    // A floating body *file* is the one whose double-`--body`
    // collision gh refuses outright, so it gets its own shim.
    let origin = o();
    let mut s = shim(&origin);
    let from_file = |_: &str| -> std::io::Result<String> { Ok("hello\n".into()) };
    s.read = &from_file;
    let out = s.rewrite(args(&[
        "--body-file",
        "notes.md",
        "pr",
        "review",
        "--approve",
    ]));
    let joined = out.join("\x00");
    assert!(joined.contains(&tag), "floating --body-file: {out:?}");
    assert!(joined.contains("hello"), "file body lost: {out:?}");
    assert_eq!(
        joined.matches("says: ").count(),
        1,
        "one stamped body only: {out:?}"
    );
    // A boolean action flag floats too, wherever there is a spare
    // positional: `gh --approve 3 pr review` parses. Read only after
    // the subcommand it went unseen, and the approving review posted
    // with no byline and no tag at all.
    let out = rewrite(args(&["--approve", "3", "pr", "review"]));
    let joined = out.join("\x00");
    assert!(joined.contains(&tag), "floating --approve: {out:?}");
    // ... and the inserted body lands after the subcommand even when
    // flags float ahead of the command words.
    let out = rewrite(args(&[
        "-R",
        "acme/other",
        "pr",
        "review",
        "7",
        "--approve",
    ]));
    let at = out.iter().position(|a| a == "--body").expect("a body");
    assert_eq!(
        &out[at - 2..at],
        &["pr".to_string(), "review".to_string()],
        "the body goes after the subcommand: {out:?}"
    );
    // A floating assignee still makes a create a hand-off.
    let origin = o();
    let mut s = shim(&origin);
    s.bot = Some("acme-bot");
    let out = s.rewrite(args(&[
        "--assignee",
        "acme-bot",
        "issue",
        "create",
        "-t",
        "t",
        "-b",
        "hello",
    ]));
    assert!(
        out.join("\x00").contains("mode=delegate"),
        "floating --assignee is still a hand-off: {out:?}"
    );
}

#[test]
fn new_is_gh_s_own_name_for_create() {
    let origin = o();
    let mut s = shim(&origin);
    s.bot = Some("acme-bot");
    for a in [
        args(&["issue", "new", "-t", "t", "-b", "hello", "-a", "acme-bot"]),
        args(&[
            "pr", "new", "--title", "t", "--body", "hello", "-a", "acme-bot",
        ]),
    ] {
        let out = s.rewrite(a.clone());
        let joined = out.join("\x00");
        // A hand-off's tag carries `mode=delegate`, so match the
        // origin rather than the plain tag.
        assert!(
            joined.contains("ssf: origin=acme/widgets#12"),
            "`new` is a create: {a:?} -> {out:?}"
        );
        assert!(
            joined.contains("mode=delegate"),
            "`new` hands off too: {a:?} -> {out:?}"
        );
    }
}

/// Lines that are not one of ours, or not a post, and must come back
/// exactly as they went in.
#[test]
fn a_command_that_is_not_ours_is_left_alone() {
    // A command that is not tagged still passes through untouched,
    // repository flag or not: matching gh's vocabulary must not tag
    // more than position did.
    for a in [
        args(&["issue", "list", "--body", "hello"]),
        args(&["-R", "acme/other", "issue", "list", "--body", "hello"]),
        args(&["-R", "acme/other", "issue", "edit", "3", "--body", "hello"]),
        args(&["repo", "view", "--body", "hello"]),
        // A subcommand word as a flag's *value* after a positional
        // is not the subcommand: `list` stops the search first.
        args(&["issue", "list", "--label", "comment", "--body", "hello"]),
        args(&[
            "issue",
            "edit",
            "3",
            "--add-label",
            "comment",
            "--body",
            "hello",
        ]),
        // The search ends for good at the first positional, so a
        // later `issue`/`pr` sitting in a flag's value cannot start
        // a fresh match. `gh issue edit --body` replaces an item's
        // body, and stamping it would write an origin tag into one,
        // where `find_owner` reads it.
        args(&[
            "issue",
            "edit",
            "3",
            "--add-label",
            "pr",
            "--add-label",
            "review",
            "--body",
            "hello",
        ]),
        args(&["secret", "set", "pr", "-b", "review"]),
        // A subcommand word that is a flag's value is not the
        // subcommand: this is a merge, and stamping it would put a
        // byline in the merge commit's message.
        args(&["pr", "--subject", "review", "merge", "3", "-r"]),
        // Nothing further down the line can promote itself to the
        // command: without that, `pr` and `review` here would be
        // read as the pair.
        args(&["secret", "set", "pr", "review", "-b", "hello"]),
        // An inline flag before the second word must not let the
        // search run past it. These are other `pr` subcommands whose
        // selector or branch happens to read as one of ours, and gh
        // accepts every one: stamping them would write an origin tag
        // into a pull request's body, into a merge commit's message,
        // or over the branch name `-b` means on a checkout.
        args(&["pr", "-Racme/other", "edit", "review", "--body", "hello"]),
        args(&["pr", "-R=acme/other", "merge", "review", "--body", "hello"]),
        args(&["pr", "-Racme/other", "merge", "review", "-r"]),
        args(&["pr", "-Racme/other", "checkout", "new", "-b", "mybranch"]),
        // A `--` of its own ends the flags for gh, so the words
        // beyond are positionals and not a command.
        args(&["pr", "--", "review", "--approve"]),
        // A tagged word in a flag's value, on a command of its
        // own: `pr edit` replaces a pull request's body.
        args(&[
            "pr",
            "edit",
            "3",
            "--add-label",
            "issue",
            "--add-label",
            "comment",
            "--body",
            "hello",
        ]),
        // A command word with no subcommand after it at all.
        args(&["issue", "--repo"]),
        args(&["pr"]),
    ] {
        assert_eq!(rewrite(a.clone()), a, "left alone: {a:?}");
    }
}

#[test]
fn byline_follows_the_repository_posted_to() {
    let short = format!("🤖#12 says: {}\n\nhi", o().tag());
    let long = format!("🤖acme/widgets#12 says: {}\n\nhi", o().tag());
    // --repo in every spelling, compared case-insensitively.
    for a in [
        args(&[
            "issue",
            "comment",
            "3",
            "--body",
            "hi",
            "--repo",
            "ACME/widgets",
        ]),
        args(&[
            "issue",
            "comment",
            "3",
            "--body",
            "hi",
            "-R",
            "acme/widgets",
        ]),
        args(&[
            "issue",
            "comment",
            "3",
            "--body",
            "hi",
            "--repo=acme/widgets",
        ]),
        args(&["issue", "comment", "3", "--body", "hi", "-Racme/widgets"]),
        args(&["issue", "comment", "3", "--body", "hi", "-R=acme/widgets"]),
        args(&[
            "issue",
            "comment",
            "3",
            "--body",
            "hi",
            "-R",
            "github.com/acme/widgets",
        ]),
        args(&[
            "issue",
            "comment",
            "3",
            "--body",
            "hi",
            "-R",
            "https://github.com/acme/widgets",
        ]),
        args(&[
            "issue",
            "comment",
            "https://github.com/acme/widgets/issues/3",
            "--body",
            "hi",
        ]),
        args(&[
            "pr",
            "comment",
            "--body",
            "hi",
            "https://github.com/acme/widgets/pull/3",
        ]),
    ] {
        let out = rewrite(a.clone());
        assert!(out.contains(&short), "{a:?} -> {out:?}");
    }
    for a in [
        args(&[
            "issue",
            "comment",
            "3",
            "--body",
            "hi",
            "--repo",
            "acme/other",
        ]),
        args(&[
            "issue",
            "comment",
            "3",
            "--body",
            "hi",
            "-R",
            "other/widgets",
        ]),
        args(&[
            "issue",
            "comment",
            "https://github.com/acme/other/issues/3",
            "--body",
            "hi",
        ]),
        // gh takes a plain `http://` item too, so the byline has to
        // follow it there.
        args(&[
            "issue",
            "comment",
            "http://github.com/acme/other/issues/3",
            "--body",
            "hi",
        ]),
        args(&["issue", "create", "-t", "t", "-b", "hi", "-R", "acme/other"]),
        args(&["issue", "comment", "3", "--body", "hi", "--repo=acme/other"]),
        // Behind a cluster's value-less letters, where `-R` is as
        // much the repository as it is on its own: read only at the
        // start of an argument, the answer would fall through to the
        // checkout and put this session's short `#N` on a post
        // landing somewhere else.
        args(&["pr", "create", "-t", "t", "-b", "hi", "-dR", "acme/other"]),
        args(&["pr", "create", "-t", "t", "-b", "hi", "-dRacme/other"]),
        args(&["pr", "create", "-t", "t", "-b", "hi", "-dR=acme/other"]),
        // `-a` approves here rather than naming an assignee, so the
        // cluster has to be walked with the review's letters.
        args(&["pr", "review", "3", "-b", "hi", "-aR", "acme/other"]),
    ] {
        let out = rewrite(a.clone());
        assert!(out.contains(&long), "{a:?} -> {out:?}");
    }
    // A URL in the body is not the item, in the shorthand spelling
    // as much as the long one: `-b` takes it, so this review is on
    // the session's own repository and carries the short form. This
    // is what `b` is doing in the review half of `valued_shorthand`.
    let out = rewrite(args(&[
        "pr",
        "review",
        "7",
        "-b",
        "https://github.com/acme/other/pull/1",
        "--approve",
    ]));
    assert!(out[4].starts_with("🤖#12 says: "), "{out:?}");
    // A URL in the body is not the item.
    let out = rewrite(args(&[
        "issue",
        "comment",
        "3",
        "--body",
        "https://github.com/acme/other/pull/1",
    ]));
    assert!(out[4].starts_with("🤖#12 says: "), "{out:?}");
    let out = rewrite(args(&[
        "pr",
        "create",
        "-t",
        "https://github.com/acme/other",
        "-b",
        "hi",
    ]));
    assert!(out.contains(&short), "{out:?}");
    // Without --repo the checkout decides; GH_REPO outranks it, as in gh.
    let o = o();
    let other = || Some("acme/other".to_string());
    let mut s = shim(&o);
    s.checkout = &other;
    let out = s.rewrite(args(&["issue", "comment", "3", "--body", "hi"]));
    assert!(out.contains(&long), "{out:?}");
    let out = s.rewrite(args(&[
        "issue",
        "comment",
        "3",
        "--body",
        "hi",
        "-R",
        "acme/widgets",
    ]));
    assert!(
        out.contains(&short),
        "--repo outranks the checkout: {out:?}"
    );
    s.gh_repo = Some("acme/widgets");
    let out = s.rewrite(args(&["issue", "comment", "3", "--body", "hi"]));
    assert!(out.contains(&short), "{out:?}");
    s.gh_repo = Some("ACME/OTHER");
    let out = s.rewrite(args(&["issue", "comment", "3", "--body", "hi"]));
    assert!(out.contains(&long), "{out:?}");
    // `-R=o/r` is a spelling pflag takes, and the `=` is not part of
    // the repository: read as one it would give the long form here.
    let out = s.rewrite(args(&[
        "issue",
        "comment",
        "3",
        "--body",
        "hi",
        "-R=acme/widgets",
    ]));
    assert!(out.contains(&short), "-R= is a repository: {out:?}");
    // A URL that is one of NOT_THE_ITEM's values is not the item.
    // gh documents the URL form for all three of `issue create`'s
    // number-or-URL flags; reading one as the item beats GH_REPO and
    // stamps the short `#N` on a post landing elsewhere, where it
    // links to that repository's own issue N. A fresh shim, so the
    // checkout is this session's repository and GH_REPO is the only
    // thing that can give the long form.
    for flag in ["--parent", "--blocked-by", "--blocking"] {
        let mut s = shim(&o);
        s.gh_repo = Some("acme/other");
        let out = s.rewrite(args(&[
            "issue",
            "create",
            flag,
            "https://github.com/acme/widgets/issues/5",
            "--title",
            "t",
            "--body",
            "hi",
        ]));
        assert!(
            out.contains(&long),
            "{flag}'s value is not the item: {out:?}"
        );
    }
    // The item's own URL still is one, flags before it or not: a
    // valueless flag must not hide it, or the fallback (the
    // checkout, this session's own repository) puts the short form
    // on a post landing elsewhere -- the same defect mirrored.
    let mut s = shim(&o);
    s.gh_repo = Some("ACME/OTHER");
    let out = s.rewrite(args(&[
        "issue",
        "comment",
        "https://github.com/acme/widgets/issues/3",
        "--body",
        "hi",
    ]));
    assert!(out.contains(&short), "the item's URL still counts: {out:?}");
    let s = shim(&o);
    let out = s.rewrite(args(&[
        "pr",
        "review",
        "--approve",
        "https://github.com/acme/other/pull/7",
        "--body",
        "hi",
    ]));
    assert!(
        out.contains(&long),
        "a valueless flag does not hide the item: {out:?}"
    );
    // `--` ends the flags for gh too, and the item can be after it.
    let s = shim(&o);
    let out = s.rewrite(args(&[
        "issue",
        "comment",
        "--body",
        "hi",
        "--",
        "https://github.com/acme/other/issues/3",
    ]));
    assert!(
        out.contains(&long),
        "the item after `--` still counts: {out:?}"
    );
    // Nothing says: the long form, which links from anywhere.
    let mut s = shim(&o);
    s.checkout = &unknown;
    let out = s.rewrite(args(&["issue", "comment", "3", "--body", "hi"]));
    assert!(out.contains(&long), "{out:?}");
    // The checkout is not consulted when it is not needed.
    let boom = || -> Option<String> { panic!("checkout looked at") };
    let mut s = shim(&o);
    s.checkout = &boom;
    s.rewrite(args(&[
        "issue", "comment", "3", "--body", "hi", "-R", "a/b",
    ]));
    s.rewrite(args(&["pr", "list"]));
}

#[test]
fn repositories_are_read_out_of_any_spelling() {
    for (given, want) in [
        ("acme/widgets", Some("acme/widgets")),
        (" acme/widgets ", Some("acme/widgets")),
        ("github.com/acme/widgets", Some("acme/widgets")),
        ("https://github.com/acme/widgets", Some("acme/widgets")),
        ("https://github.com/acme/widgets/", Some("acme/widgets")),
        ("https://github.com/acme/widgets.git", Some("acme/widgets")),
        (
            "https://github.com/acme/widgets/pull/12",
            Some("acme/widgets"),
        ),
        (
            "https://github.com/acme/widgets/issues/12#issuecomment-1",
            Some("acme/widgets"),
        ),
        ("git@github.com:acme/widgets.git", Some("acme/widgets")),
        (
            "ssh://git@github.com/acme/widgets.git",
            Some("acme/widgets"),
        ),
        ("ssh://git@github.com:22/acme/widgets", Some("acme/widgets")),
        ("acme", None),
        ("", None),
        ("https://github.com/", None),
        ("https://github.com/acme", None),
    ] {
        assert_eq!(repo_of(given).as_deref(), want, "{given:?}");
    }
}
