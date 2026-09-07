//! The `gh` shim: a symlink named `gh` in an ssf-owned directory that `ssf
//! launch` puts first on the agent's PATH. It points at the ssf binary, which
//! notices it was invoked as `gh`, prepends the session's byline and origin
//! tag to the body of anything that posts to GitHub, and execs the real gh.
//! The same directory carries an `ssf` link to the same binary, so the `ssf`
//! commands the prompts name run the daemon's build.
//!
//! Only `issue create|comment` and `pr create|comment|review` are touched;
//! every other invocation is passed on untouched. An `issue create` or `pr
//! create` that assigns the bot itself is a hand-off, and its tag says so
//! (`mode=delegate`) so the daemon gives the new item a session of its own.
//! The byline links to the session's item, as `#N` on the item's own
//! repository and `owner/repo#N` elsewhere, so the shim works out which
//! repository the post goes to the way gh does: `--repo`, an item given as
//! a URL, `GH_REPO`, else the checkout's `origin` remote. The shim reads
//! nothing but its environment (a `--body-file`, and `git config` for that
//! remote), writes nothing, and keeps stdin and the terminal intact, so it
//! works inside read-only sandboxes and leaves gh's interactive flows alone.

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

/// Was this process started under the name `gh`?
pub fn invoked_as_gh() -> bool {
    std::env::args_os()
        .next()
        .map(PathBuf::from)
        .and_then(|p| p.file_name().map(|f| f == "gh"))
        .unwrap_or(false)
}

/// Names linked to the ssf binary in the shim directory: `gh` (the shim)
/// and `ssf` itself, so `ssf release`, `ssf sub`, `ssf tell` and the rest
/// run the daemon's own build rather than whatever `ssf` the agent's shell
/// happens to have (an older package, or nothing).
pub const LINKS: [&str; 2] = ["gh", "ssf"];

/// Make `<dir>/gh` and `<dir>/ssf` symlinks to `exe`, replacing whatever
/// is there.
pub fn install(exe: &Path) -> Result<PathBuf> {
    let dir = dir();
    install_in(&dir, exe)?;
    Ok(dir)
}

fn install_in(dir: &Path, exe: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for name in LINKS {
        let link = dir.join(name);
        if std::fs::read_link(&link).ok().as_deref() == Some(exe) {
            continue;
        }
        let tmp = dir.join(format!("{name}.tmp.{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(exe, &tmp)
            .with_context(|| format!("linking {} -> {}", tmp.display(), exe.display()))?;
        std::fs::rename(&tmp, &link).with_context(|| format!("installing {}", link.display()))?;
    }
    Ok(())
}

/// The first `ssf` on PATH outside the shim directory, if any: what an
/// agent's shell would run without the shim directory in front.
pub fn ssf_on_path() -> Option<PathBuf> {
    let shim_dir = dir();
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|d| {
            *d != shim_dir && std::fs::canonicalize(d).ok() != std::fs::canonicalize(&shim_dir).ok()
        })
        .map(|d| d.join("ssf"))
        .find(|p| p.is_file())
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
    let gh_repo = std::env::var("GH_REPO").ok();
    match (utf8, Origin::from_env()) {
        (Some(args), Some(origin)) => {
            let shim = Shim {
                origin: &origin,
                bot: bot.as_deref(),
                gh_repo: gh_repo.as_deref(),
                read: &read_body_file,
                checkout: &checkout_repo,
            };
            cmd.args(shim.rewrite(args));
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

/// The repository the current directory's checkout pushes to (`origin`).
fn checkout_repo() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    repo_of(String::from_utf8(out.stdout).ok()?.trim())
}

/// `owner/repo` out of any way of naming a GitHub repository: `OWNER/REPO`,
/// `HOST/OWNER/REPO`, an `https://` URL of the repository or of an item in
/// it, or a remote URL (`https://host/o/r.git`, `git@host:o/r.git`,
/// `ssh://git@host/o/r`).
pub fn repo_of(s: &str) -> Option<String> {
    let s = s.trim().trim_end_matches('/');
    let path = if let Some((_, rest)) = s.split_once("://") {
        // host/owner/repo[/...]
        rest.split_once('/').map(|(_, p)| p)?
    } else if let Some((_, rest)) = s.split_once(':').filter(|(host, _)| !host.contains('/')) {
        // scp-like: [user@]host:owner/repo
        rest
    } else if s.matches('/').count() >= 2 {
        // HOST/OWNER/REPO
        s.split_once('/').map(|(_, p)| p)?
    } else {
        s
    };
    let mut segs = path.split('/').filter(|p| !p.is_empty());
    let owner = segs.next()?;
    let repo = segs.next()?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let name = format!("{owner}/{repo}");
    crate::config::split_repo_name(&name).ok()?;
    Some(name)
}

/// The repository a gh command posts to, as far as its arguments say:
/// `--repo`/`-R` (in any spelling), else an item named by its URL. Values
/// of the body and title flags are not looked at.
fn repo_in_args(args: &[String]) -> Option<String> {
    let mut i = 0;
    let mut url = None;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            break;
        }
        if (a == "--repo" || a == "-R") && i + 1 < args.len() {
            return repo_of(&args[i + 1]);
        }
        if let Some(v) = a.strip_prefix("--repo=") {
            return repo_of(v);
        }
        if let Some(v) = a.strip_prefix("-R").filter(|v| !v.is_empty()) {
            return repo_of(v);
        }
        if matches!(a, "--body" | "-b" | "--body-file" | "-F" | "--title" | "-t") {
            i += 2;
            continue;
        }
        if url.is_none()
            && !a.starts_with('-')
            && (a.starts_with("https://") || a.starts_with("http://"))
        {
            url = repo_of(a);
        }
        i += 1;
    }
    url
}

/// What the shim knows about the session it runs in, and how it reads the
/// world: `read` resolves `--body-file` (a path, or `-` for stdin),
/// `checkout` names the current checkout's repository. `bot` is the bot
/// login, to notice a `create --assignee <bot>` hand-off; `gh_repo` is the
/// `GH_REPO` gh honours over the checkout.
pub struct Shim<'a> {
    pub origin: &'a Origin,
    pub bot: Option<&'a str>,
    pub gh_repo: Option<&'a str>,
    pub read: &'a dyn Fn(&str) -> std::io::Result<String>,
    pub checkout: &'a dyn Fn() -> Option<String>,
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
        } else if let Some(v) = a
            .strip_prefix("-a")
            .filter(|v| !v.is_empty() && !a.starts_with("--"))
        {
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

impl Shim<'_> {
    /// The gh arguments with the byline and origin tag prepended to the
    /// body, where there is one.
    pub fn rewrite(&self, args: Vec<String>) -> Vec<String> {
        let Some((c, s)) = command_words(&args) else {
            return args;
        };
        if !is_tagged(&args[c], &args[s]) {
            return args;
        }
        let origin = self.origin;
        let delegate = args[s] == "create" && assigns_bot(&args[s + 1..], self.bot);
        let on_repo = repo_in_args(&args[s + 1..])
            .or_else(|| self.gh_repo.and_then(repo_of))
            .or_else(|| (self.checkout)());
        let stamp = |body: &str| stamp_with(body, origin, on_repo.as_deref(), delegate);
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
                let Ok(text) = (self.read)(path) else {
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
            out.insert(s + 1, stamp(""));
            out.insert(s + 1, "--body".to_string());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o() -> Origin {
        Origin::new("acme/widgets", 12).unwrap()
    }

    /// The first line of a post on the session's own repository.
    fn line() -> String {
        o().first_line(Some("acme/widgets"), false)
    }

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    fn no_files(_: &str) -> std::io::Result<String> {
        Err(std::io::Error::other("no files in tests"))
    }

    fn same_repo() -> Option<String> {
        Some("acme/widgets".into())
    }

    fn unknown() -> Option<String> {
        None
    }

    /// A shim in a checkout of the session's own repository, no `GH_REPO`.
    fn shim(origin: &Origin) -> Shim<'_> {
        Shim {
            origin,
            bot: None,
            gh_repo: None,
            read: &no_files,
            checkout: &same_repo,
        }
    }

    fn rewrite(a: Vec<String>) -> Vec<String> {
        let o = o();
        shim(&o).rewrite(a)
    }

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
            args(&["issue", "create", "-t", "t", "-b", "hi", "-R", "acme/other"]),
        ] {
            let out = rewrite(a.clone());
            assert!(out.contains(&long), "{a:?} -> {out:?}");
        }
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
        assert_eq!(out, args(&["issue", "comment", "1", "--body", &expect]));
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
                "--body",
                &expect,
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
        ] {
            s.bot = bot;
            let out = s.rewrite(a.clone());
            assert!(out.contains(&plain), "{a:?} -> {out:?}");
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
}
