//! The `gh` shim: a symlink named `gh` in an ssf-owned directory that `ssf
//! launch` puts first on the agent's PATH. It points at the ssf binary, which
//! notices it was invoked as `gh`, appends the session's origin tag to the
//! body of anything that posts to GitHub, and execs the real gh.
//!
//! Only `issue create|comment` and `pr create|comment|review` are touched;
//! every other invocation is passed on untouched. An `issue create` or `pr
//! create` that assigns the bot itself is a hand-off, and its tag says so
//! (`mode=delegate`) so the daemon gives the new item a session of its own. The shim reads nothing but
//! its environment (and a `--body-file`), writes nothing, and keeps stdin and
//! the terminal intact, so it works inside read-only sandboxes and leaves
//! gh's interactive flows alone.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::origin::{Origin, stamp_with};

/// Bodies above this are passed through untouched rather than moved from a
/// file onto the command line (GitHub rejects them anyway).
const MAX_INLINE_BODY: usize = 100_000;

/// Directory the shim lives in: `~/.config/ssf/bin`.
pub fn dir() -> PathBuf {
    crate::config::config_dir().join("bin")
}

pub fn path() -> PathBuf {
    dir().join("gh")
}

/// Was this process started under the name `gh`?
pub fn invoked_as_gh() -> bool {
    std::env::args_os()
        .next()
        .map(PathBuf::from)
        .and_then(|p| p.file_name().map(|f| f == "gh"))
        .unwrap_or(false)
}

/// Make `<dir>/gh` a symlink to `exe`, replacing whatever is there.
pub fn install(exe: &Path) -> Result<PathBuf> {
    let dir = dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let link = dir.join("gh");
    if std::fs::read_link(&link).ok().as_deref() == Some(exe) {
        return Ok(dir);
    }
    let tmp = dir.join(format!("gh.tmp.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(exe, &tmp)
        .with_context(|| format!("linking {} -> {}", tmp.display(), exe.display()))?;
    std::fs::rename(&tmp, &link).with_context(|| format!("installing {}", link.display()))?;
    Ok(dir)
}

/// Where the shim currently points, if it is installed.
pub fn target() -> Option<PathBuf> {
    std::fs::read_link(path()).ok()
}

/// PATH with the shim directory first (and nowhere else). `None` when the
/// directory cannot go on a PATH (it contains `:`).
pub fn prepend_to_path(dir: &Path, path: Option<&std::ffi::OsStr>) -> Option<std::ffi::OsString> {
    let mut parts = vec![dir.to_path_buf()];
    if let Some(p) = path {
        parts.extend(std::env::split_paths(p).filter(|d| d != dir));
    }
    std::env::join_paths(parts).ok()
}

/// The real GitHub CLI: the first `gh` on PATH that is neither this binary
/// nor anything in the shim directory (so a missing /proc, which makes
/// `current_exe` fail, cannot turn the shim into an exec loop).
pub fn real_gh() -> Option<PathBuf> {
    let me = std::env::current_exe().and_then(std::fs::canonicalize).ok();
    let shim_dir = dir();
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|d| {
            *d != shim_dir && std::fs::canonicalize(d).ok() != std::fs::canonicalize(&shim_dir).ok()
        })
        .map(|d| d.join("gh"))
        .filter(|p| p.is_file())
        .find(|p| {
            let canonical = std::fs::canonicalize(p).ok();
            let is_me = canonical.is_some() && canonical == me;
            // Without /proc we cannot know our own path; treat any link to an
            // `ssf` binary as another shim.
            let looks_like_ssf = me.is_none()
                && canonical
                    .as_deref()
                    .and_then(Path::file_name)
                    .is_some_and(|n| n == "ssf");
            !is_me && !looks_like_ssf
        })
}

/// Entry point when invoked as `gh`. Never returns.
pub fn run() -> ! {
    use std::os::unix::process::CommandExt;
    let raw: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let Some(real) = real_gh() else {
        eprintln!("gh: the GitHub CLI is not installed (ssf's gh shim found no other gh on PATH)");
        std::process::exit(127);
    };
    let mut cmd = std::process::Command::new(&real);
    // Only well-formed UTF-8 argument lists are inspected; anything else is
    // handed to gh exactly as received.
    let utf8: Option<Vec<String>> = raw.iter().map(|a| a.to_str().map(str::to_string)).collect();
    let bot = std::env::var("SSF_BOT").ok();
    match (utf8, Origin::from_env()) {
        (Some(args), Some(origin)) => {
            cmd.args(rewrite(args, &origin, bot.as_deref(), &read_body_file));
        }
        _ => {
            cmd.args(&raw);
        }
    }
    let err = cmd.exec();
    eprintln!("gh: could not run {}: {err}", real.display());
    std::process::exit(126);
}

fn read_body_file(path: &str) -> std::io::Result<String> {
    use std::io::Read;
    let mut s = String::new();
    if path == "-" {
        std::io::stdin().read_to_string(&mut s)?;
    } else {
        std::fs::File::open(path)?.read_to_string(&mut s)?;
    }
    Ok(s)
}

/// Position of the command and subcommand words in a gh command line.
fn command_words(args: &[String]) -> Option<(usize, usize)> {
    let mut words = args
        .iter()
        .enumerate()
        .filter(|(_, a)| !a.starts_with('-'))
        .map(|(i, _)| i);
    Some((words.next()?, words.next()?))
}

fn is_tagged(cmd: &str, sub: &str) -> bool {
    matches!(
        (cmd, sub),
        ("issue", "create")
            | ("issue", "comment")
            | ("pr", "create")
            | ("pr", "comment")
            | ("pr", "review")
    )
}

/// Does an `issue create` / `pr create` command line assign the bot (`bot`)
/// itself? `@me` is the bot too, since gh runs with its token.
fn assigns_bot(args: &[String], bot: Option<&str>) -> bool {
    let mut i = 0;
    let mut names: Vec<&str> = Vec::new();
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            break;
        }
        if (a == "--assignee" || a == "-a") && i + 1 < args.len() {
            names.push(args[i + 1].as_str());
            i += 2;
            continue;
        }
        if let Some(v) = a.strip_prefix("--assignee=") {
            names.push(v);
        } else if let Some(v) = a.strip_prefix("-a").filter(|v| !v.is_empty() && !a.starts_with("--")) {
            names.push(v);
        }
        i += 1;
    }
    names
        .iter()
        .flat_map(|n| n.split(','))
        .map(|n| n.trim().trim_start_matches('@'))
        .any(|n| n.eq_ignore_ascii_case("me") || bot.is_some_and(|b| n.eq_ignore_ascii_case(b)))
}

/// The gh arguments with the origin tag appended to the body, where there is
/// one. `read` resolves `--body-file` (a path, or `-` for stdin). `bot` is
/// the bot login, to notice a `create --assignee <bot>` hand-off.
pub fn rewrite(
    args: Vec<String>,
    origin: &Origin,
    bot: Option<&str>,
    read: &dyn Fn(&str) -> std::io::Result<String>,
) -> Vec<String> {
    let Some((c, s)) = command_words(&args) else {
        return args;
    };
    if !is_tagged(&args[c], &args[s]) {
        return args;
    }
    let delegate = args[s] == "create" && assigns_bot(&args[s + 1..], bot);
    let stamp = |body: &str| stamp_with(body, origin, delegate);
    let mut out: Vec<String> = args[..=s].to_vec();
    let mut stamped = false;
    let mut i = s + 1;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            out.extend_from_slice(&args[i..]);
            break;
        }
        let short = |flag: &str| a.strip_prefix(flag).filter(|_| !a.starts_with("--"));
        // Inline body: --body X, -b X, --body=X, -bX.
        if (a == "--body" || a == "-b") && i + 1 < args.len() {
            out.push(a.to_string());
            out.push(stamp(&args[i + 1]));
            stamped = true;
            i += 2;
            continue;
        }
        if let Some(v) = a.strip_prefix("--body=") {
            out.push(format!("--body={}", stamp(v)));
            stamped = true;
            i += 1;
            continue;
        }
        if let Some(v) = short("-b").filter(|v| !v.is_empty()) {
            out.push(format!("-b{}", stamp(v)));
            stamped = true;
            i += 1;
            continue;
        }
        // Body from a file (or stdin): moved onto the command line so no
        // temporary file is needed.
        let file = if (a == "--body-file" || a == "-F") && i + 1 < args.len() {
            Some((args[i + 1].as_str(), 2))
        } else if let Some(v) = a.strip_prefix("--body-file=") {
            Some((v, 1))
        } else {
            short("-F").map(|v| (v, 1))
        };
        if let Some((path, used)) = file {
            let Ok(text) = read(path) else {
                return args; // let gh report the unreadable file
            };
            let body = stamp(&text);
            if body.len() > MAX_INLINE_BODY {
                return args;
            }
            out.push("--body".to_string());
            out.push(body);
            stamped = true;
            i += used;
            continue;
        }
        out.push(a.to_string());
        i += 1;
    }
    // An approval needs no body, but should still say where it came from.
    // Without an action flag gh would prompt (or reject --body), so those
    // are left alone.
    let has_action = args[s + 1..].iter().any(|a| {
        matches!(
            a.as_str(),
            "--approve" | "-a" | "--request-changes" | "-r" | "--comment" | "-c"
        )
    });
    if !stamped && args[c] == "pr" && args[s] == "review" && has_action {
        out.insert(s + 1, origin.tag());
        out.insert(s + 1, "--body".to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o() -> Origin {
        Origin::new("acme/widgets", 12).unwrap()
    }

    fn tag() -> String {
        o().tag()
    }

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    fn no_files(_: &str) -> std::io::Result<String> {
        Err(std::io::Error::other("no files in tests"))
    }

    #[test]
    fn stamps_inline_bodies_in_every_spelling() {
        let expect = format!("hello\n\n{}", tag());
        for a in [
            args(&["issue", "comment", "3", "--body", "hello"]),
            args(&["issue", "comment", "3", "-b", "hello"]),
            args(&["issue", "comment", "3", "--body=hello"]),
            args(&["issue", "comment", "3", "-bhello"]),
            args(&["pr", "create", "--title", "t", "--body", "hello", "--draft"]),
            args(&["pr", "comment", "--body", "hello", "--repo", "a/b"]),
            args(&["pr", "review", "--approve", "--body", "hello"]),
            args(&["issue", "create", "-t", "t", "-b", "hello"]),
        ] {
            let out = rewrite(a.clone(), &o(), None, &no_files);
            assert_eq!(out.len(), a.len(), "{a:?}");
            let joined = out.join("\x00");
            assert!(joined.contains(&expect), "{out:?}");
        }
    }

    #[test]
    fn body_files_move_inline() {
        let read = |p: &str| -> std::io::Result<String> {
            assert!(p == "notes.md" || p == "-");
            Ok("from file\n".into())
        };
        let expect = format!("from file\n\n{}", tag());
        let out = rewrite(
            args(&["pr", "create", "-t", "t", "--body-file", "notes.md"]),
            &o(),
            None,
            &read,
        );
        assert_eq!(out, args(&["pr", "create", "-t", "t", "--body", &expect]));
        let out = rewrite(args(&["pr", "create", "-t", "t", "-F", "-"]), &o(), None, &read);
        assert_eq!(out, args(&["pr", "create", "-t", "t", "--body", &expect]));
        let out = rewrite(
            args(&["issue", "comment", "1", "--body-file=notes.md"]),
            &o(),
            None,
            &read,
        );
        assert_eq!(out, args(&["issue", "comment", "1", "--body", &expect]));
        let out = rewrite(
            args(&["issue", "comment", "1", "-Fnotes.md", "-R", "a/b"]),
            &o(),
            None,
            &read,
        );
        assert_eq!(
            out,
            args(&["issue", "comment", "1", "--body", &expect, "-R", "a/b"])
        );
        // Unreadable file: untouched, gh reports it.
        let a = args(&["issue", "comment", "1", "--body-file", "missing"]);
        assert_eq!(rewrite(a.clone(), &o(), None, &no_files), a);
    }

    #[test]
    fn reviews_without_a_body_get_one() {
        let out = rewrite(args(&["pr", "review", "7", "--approve"]), &o(), None, &no_files);
        assert_eq!(
            out,
            args(&["pr", "review", "--body", &tag(), "7", "--approve"])
        );
    }

    #[test]
    fn everything_else_passes_through() {
        for a in [
            args(&["pr", "list"]),
            args(&["issue", "view", "3"]),
            args(&["issue", "edit", "3", "--body", "x"]),
            args(&["api", "repos/a/b/issues", "-f", "body=x"]),
            args(&["pr", "create", "--fill"]),
            args(&["issue", "comment", "3", "--editor"]),
            args(&["issue", "comment", "3", "--", "--body", "x"]),
            args(&["--version"]),
            args(&[]),
        ] {
            assert_eq!(rewrite(a.clone(), &o(), None, &no_files), a, "{a:?}");
        }
    }

    #[test]
    fn malformed_flags_pass_through() {
        for a in [
            args(&["issue", "comment", "3", "-b"]),
            args(&["issue", "comment", "3", "--body"]),
            args(&["issue", "comment", "3", "--body-file"]),
        ] {
            assert_eq!(rewrite(a.clone(), &o(), None, &no_files), a, "{a:?}");
        }
    }

    #[test]
    fn already_tagged_bodies_are_left_alone() {
        let body = format!("done\n\n{}", tag());
        let a = args(&["issue", "comment", "3", "--body", &body]);
        assert_eq!(rewrite(a.clone(), &o(), None, &no_files), a);
    }

    #[test]
    fn creating_and_assigning_the_bot_is_a_hand_off() {
        let delegate = format!("child\n\n{}", o().delegate_tag());
        let plain = format!("child\n\n{}", tag());
        for a in [
            args(&["issue", "create", "-t", "t", "-b", "child", "--assignee", "OverlayBot"]),
            args(&["issue", "create", "-t", "t", "-b", "child", "-a", "overlaybot"]),
            args(&["issue", "create", "-t", "t", "-b", "child", "--assignee=alice,OverlayBot"]),
            args(&["issue", "create", "-t", "t", "-b", "child", "-a@me"]),
            args(&["pr", "create", "-t", "t", "-b", "child", "--assignee", "@me"]),
        ] {
            let out = rewrite(a.clone(), &o(), Some("OverlayBot"), &no_files);
            assert!(out.contains(&delegate), "{a:?} -> {out:?}");
        }
        // Assigning someone else, assigning on a comment, or not knowing the
        // bot login: an ordinary tag.
        for (a, bot) in [
            (args(&["issue", "create", "-t", "t", "-b", "child", "--assignee", "alice"]), Some("OverlayBot")),
            (args(&["issue", "create", "-t", "t", "-b", "child", "--assignee", "OverlayBot"]), None),
            (args(&["issue", "create", "-t", "t", "-b", "child", "--", "--assignee", "OverlayBot"]), Some("OverlayBot")),
            (args(&["issue", "comment", "3", "-b", "child", "--assignee", "OverlayBot"]), Some("OverlayBot")),
        ] {
            let out = rewrite(a.clone(), &o(), bot, &no_files);
            assert!(out.contains(&plain), "{a:?} -> {out:?}");
        }
        // @me is the bot even without SSF_BOT: gh runs with the bot's token.
        let out = rewrite(
            args(&["issue", "create", "-t", "t", "-b", "child", "-a", "@me"]),
            &o(),
            None,
            &no_files,
        );
        assert!(out.contains(&delegate));
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
}
