use super::*;

#[test]
fn creating_and_assigning_the_bot_is_a_hand_off() {
    let delegate = format!("🤖#12 says: {}\n\nchild", o().delegate_tag());
    let plain = format!("{}\n\nchild", line());
    let o = o();
    let mut s = shim(&o);
    s.bot = Some("OverlayBot");
    for a in [
        args(&[
            "issue",
            "create",
            "-t",
            "t",
            "-b",
            "child",
            "--assignee",
            "OverlayBot",
        ]),
        args(&[
            "issue",
            "create",
            "-t",
            "t",
            "-b",
            "child",
            "-a",
            "overlaybot",
        ]),
        args(&[
            "issue",
            "create",
            "-t",
            "t",
            "-b",
            "child",
            "--assignee=alice,OverlayBot",
        ]),
        args(&["issue", "create", "-t", "t", "-b", "child", "-a@me"]),
        // Behind a cluster's value-less letters, attached and not.
        args(&[
            "issue",
            "create",
            "-t",
            "t",
            "-b",
            "child",
            "-wa",
            "OverlayBot",
        ]),
        args(&["issue", "create", "-t", "t", "-b", "child", "-wa@me"]),
        args(&["issue", "create", "-t", "t", "-b", "child", "-a=@me"]),
        args(&["pr", "create", "-t", "t", "-b", "child", "-da=OverlayBot"]),
        args(&[
            "pr",
            "create",
            "-t",
            "t",
            "-b",
            "child",
            "-da",
            "OverlayBot",
        ]),
        args(&[
            "pr",
            "create",
            "-t",
            "t",
            "-b",
            "child",
            "--assignee",
            "@me",
        ]),
    ] {
        let out = s.rewrite(a.clone());
        assert!(out.contains(&delegate), "{a:?} -> {out:?}");
    }
    // Assigning someone else, assigning on a comment, or not knowing the
    // bot login: an ordinary tag.
    for (a, bot) in [
        (
            args(&[
                "issue",
                "create",
                "-t",
                "t",
                "-b",
                "child",
                "--assignee",
                "alice",
            ]),
            Some("OverlayBot"),
        ),
        (
            args(&[
                "issue",
                "create",
                "-t",
                "t",
                "-b",
                "child",
                "--assignee",
                "OverlayBot",
            ]),
            None,
        ),
        (
            args(&[
                "issue",
                "create",
                "-t",
                "t",
                "-b",
                "child",
                "--",
                "--assignee",
                "OverlayBot",
            ]),
            Some("OverlayBot"),
        ),
        (
            args(&[
                "issue",
                "comment",
                "3",
                "-b",
                "child",
                "--assignee",
                "OverlayBot",
            ]),
            Some("OverlayBot"),
        ),
        // A login inside a body or a title is not an assignee. A
        // wrong yes here would give the new item a session of its
        // own instead of leaving it with whoever filed it.
        (
            args(&["issue", "create", "-t", "-wa @me", "-b", "child"]),
            Some("OverlayBot"),
        ),
        // A letter that takes a value of its own ends the walk, so
        // the `a` here is a label and not an assignee.
        (
            args(&[
                "issue",
                "create",
                "-t",
                "t",
                "-b",
                "child",
                "-la=OverlayBot",
            ]),
            Some("OverlayBot"),
        ),
        // And the value another letter takes is not an assignee
        // either, however much it reads like one.
        (
            args(&["issue", "create", "-t", "OverlayBot", "-b", "child"]),
            Some("OverlayBot"),
        ),
        (
            args(&[
                "issue",
                "create",
                "-t",
                "t",
                "-b",
                "child",
                "-l",
                "OverlayBot",
            ]),
            Some("OverlayBot"),
        ),
    ] {
        s.bot = bot;
        let out = s.rewrite(a.clone());
        assert!(out.contains(&plain), "{a:?} -> {out:?}");
    }
    // Nor is a login written in the body, which the tag would
    // otherwise be read out of. The rewrite loop reads the body flag
    // before it asks whether to step over a value, but `assigns_bot`
    // has no such branch: the long and short spellings are both in
    // the list only for its sake.
    s.bot = Some("OverlayBot");
    for a in [
        args(&["pr", "create", "-t", "t", "-b", "-wa @me"]),
        args(&["pr", "create", "-t", "t", "--body", "-wa @me"]),
    ] {
        let out = s.rewrite(a.clone());
        assert!(
            !out.iter().any(|o| o.contains("mode=delegate")),
            "{a:?} -> {out:?}"
        );
    }
    // @me is the bot even without SSF_BOT: gh runs with the bot's token.
    let out = rewrite(args(&[
        "issue", "create", "-t", "t", "-b", "child", "-a", "@me",
    ]));
    assert!(out.contains(&delegate));
    // A hand-off on another repository: long byline, delegate tag.
    s.bot = Some("OverlayBot");
    let out = s.rewrite(args(&[
        "issue",
        "create",
        "-R",
        "acme/other",
        "-t",
        "t",
        "-b",
        "child",
        "-a",
        "OverlayBot",
    ]));
    assert!(
        out.contains(&format!(
            "🤖acme/widgets#12 says: {}\n\nchild",
            o.delegate_tag()
        )),
        "{out:?}"
    );
}

#[test]
fn install_links_gh_and_ssf_to_the_binary() {
    let dir = std::env::temp_dir().join(format!("ssf-shim-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let exe = Path::new("/opt/ssf/bin/ssf");
    install_in(&dir, exe).unwrap();
    for name in LINKS {
        assert_eq!(std::fs::read_link(dir.join(name)).unwrap(), exe, "{name}");
    }
    // A second install with another target replaces both links.
    let other = Path::new("/opt/ssf/bin/ssf-2");
    install_in(&dir, other).unwrap();
    for name in LINKS {
        assert_eq!(std::fs::read_link(dir.join(name)).unwrap(), other, "{name}");
    }
    assert!(!dir.join(format!("gh.tmp.{}", std::process::id())).exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn path_gets_the_shim_first_once() {
    let dir = Path::new("/home/x/.config/ssf/bin");
    let p = prepend_to_path(
        dir,
        Some(std::ffi::OsStr::new(
            "/usr/bin:/home/x/.config/ssf/bin:/bin",
        )),
    );
    assert_eq!(p.unwrap(), "/home/x/.config/ssf/bin:/usr/bin:/bin");
    assert_eq!(
        prepend_to_path(dir, None).unwrap(),
        "/home/x/.config/ssf/bin"
    );
    assert!(prepend_to_path(Path::new("/odd:dir"), None).is_none());
}
