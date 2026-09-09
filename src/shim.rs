//! The `gh` shim: a symlink named `gh` in an ssf-owned directory that `ssf
//! launch` puts first on the agent's PATH. It points at the ssf binary, which
//! notices it was invoked as `gh`, prepends the session's byline and origin
//! tag to the body of anything that posts to GitHub, and execs the real gh.
//! The same directory carries an `ssf` link to the same binary, so the `ssf`
//! commands the prompts name run the daemon's build.
//!
//! Only `issue create|comment` and `pr create|comment|review` are touched
//! (with `new`, gh's own alias for `create` on both);
//! every other invocation is passed on untouched. An `issue create` or `pr
//! create` that assigns the bot itself is a hand-off, and its tag says so
//! (`mode=delegate`) so the daemon gives the new item a session of its own.
//! The byline links to the session's item, as `#N` on the item's own
//! repository and `owner/repo#N` elsewhere, so the shim works out which
//! repository the post goes to the way gh does: `--repo`, an item given as
//! a URL (but not one that is a value-taking flag's value),
//! `GH_REPO`, else the checkout's `origin` remote. The shim reads
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
/// `--repo`/`-R` (in any spelling), else an item named by its URL.
/// [`valued_long`] and [`valued_shorthand`] say which flags' values are
/// not that item.
///
/// This answer outranks `GH_REPO` in [`Shim::rewrite`] and decides the
/// byline's form, so reading a flag's URL as the item is not cosmetic: a
/// post that lands on another repository carrying the short `#N` links
/// to *that* repository's issue N.
fn repo_in_args(args: &[String], review: bool) -> Option<String> {
    let mut i = 0;
    let mut url = None;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            // Everything after `--` is positional for gh too, so the
            // item can be there (`gh issue comment -- <url>` names the
            // same item as without it). Stop reading flags, keep looking
            // for it: giving up here would leave the answer to the
            // checkout, which is this session's own repository, and put
            // the short `#N` on a post landing elsewhere.
            return url.or_else(|| args[i + 1..].iter().find_map(|a| item_url(a)));
        }
        if (a == "--repo" || a == "-R") && i + 1 < args.len() {
            return repo_of(&args[i + 1]);
        }
        if let Some(v) = a.strip_prefix("--repo=") {
            return repo_of(v);
        }
        // `-R` in every shorthand spelling gh takes, including behind
        // the value-less letters of a cluster: `gh pr create -dR
        // acme/other` opens a draft there, and reading only a `-R` that
        // starts the argument would leave the answer to the checkout and
        // put this session's own short `#N` on a post landing elsewhere.
        if let Some(('R', attached)) = cluster(a, review).and_then(|cl| cl.valued) {
            return match attached {
                Some(v) => repo_of(v),
                None => args.get(i + 1).and_then(|v| repo_of(v)),
            };
        }
        // The value of a flag that takes one is not the item, whatever
        // it looks like, and a `--` there is that value rather than the
        // end of the flags. Same narrow rule as the rewrite loop: only
        // flags positively known to take a value, so a URL after a
        // value-less one stays visible.
        let skips_value = match cluster(a, review).and_then(|cl| cl.valued) {
            Some((c, None)) => valued_shorthand(c, review),
            _ => valued_long(a),
        };
        if skips_value {
            i += 2;
            continue;
        }
        if url.is_none() {
            url = item_url(a);
        }
        i += 1;
    }
    url
}

/// The repository of an argument that names an item by its URL, if it
/// does: a bare `http(s)://…`, not a flag and not a flag's value (the
/// caller decides that).
fn item_url(a: &str) -> Option<String> {
    (!a.starts_with('-') && (a.starts_with("https://") || a.starts_with("http://")))
        .then(|| repo_of(a))
        .flatten()
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

/// The commands whose posts are tagged, and the subcommands of each.
const TAGGED: [(&str, &[&str]); 2] = [
    // `new` is gh's own alias for `create` on both, so it posts the same
    // way and has to be tagged the same way.
    ("issue", &["create", "new", "comment"]),
    ("pr", &["create", "new", "comment", "review"]),
];

/// Is this subcommand word one that opens an item, under either of the
/// names gh takes for it?
fn creates(sub: &str) -> bool {
    matches!(sub, "create" | "new")
}

/// Could this argument swallow the next word, the way cobra does when it
/// looks for the command? Only a `--long` or a two-character `-x`, and
/// only without an `=`: `--repo=o/r`, `-Ro/r` and `-R=o/r` carry their
/// own value, so the word after them is the next argument proper. Cobra
/// makes an exception for a flag it already knows to be boolean, which
/// at this level is only `--help` and `--version`; neither posts, so the
/// difference cannot reach a tagged command. `--` needs no case here:
/// the caller stops at one before asking.
fn may_take_a_value(a: &str) -> bool {
    !a.contains('=') && a.starts_with('-') && (a.starts_with("--") || a.len() == 2)
}

/// Position of the command and subcommand words in a gh command line:
/// the first two arguments left once the flags are taken out, the way
/// cobra does it when it looks for the command.
///
/// Position alone cannot tell a command from a flag's value. gh lets a
/// subcommand's flags come *before* the subcommand — `gh -R o/r issue
/// comment 3 ...`, `gh --limit 1 issue list`, both accepted — and in the
/// separated spellings the value is a bare word, so taking the first two
/// bare words made `("o/r", "issue")` the command: not a tagged pair, so
/// the line went through untouched and the post carried no byline and no
/// origin tag, read afterwards as a person's rather than the session's.
///
/// Naming the flags that take a value does not work either: gh has no
/// global ones to enumerate, every subcommand's own flags may float
/// forward, and a list that misses one leaves the defect open for it.
/// Skipping flags the way cobra does and then insisting the two words
/// that remain are a tagged pair is what holds: nothing further down the
/// line can promote itself to the command, so a `pr` or an `issue`
/// sitting in a value — or a `pr merge` whose branch is called `review`
/// — is never mistaken for one of ours.
///
/// Clustered shorthands need no case here either. This scan steps over
/// a flag without asking what it is, and cobra's own command lookup
/// steps over a cluster in one piece as well, since only a
/// two-character `-x` can swallow the word after it. What is inside one
/// matters further down, where the body and the action flags are read:
/// see `cluster`.
fn command_words(args: &[String]) -> Option<(usize, usize)> {
    let mut words = Vec::with_capacity(2);
    let mut i = 0;
    while i < args.len() && words.len() < 2 {
        let a = args[i].as_str();
        if a == "--" {
            break;
        }
        if a.starts_with('-') {
            // A `--` here is swallowed like any other word. cobra
            // breaks on one only when it is the argument it is looking
            // at, so `gh -b -- issue comment 1` is a comment with a body
            // of `--`, and `gh -R -- pr review 3 -a` is an approval:
            // both post, and stopping at that `--` left the pair unfound
            // and the post untagged. Both were run against gh.
            i += if may_take_a_value(a) { 2 } else { 1 };
            continue;
        }
        // An empty argument is not a word to cobra either, and
        // `gh "" pr review --approve` posts.
        if a.is_empty() {
            i += 1;
            continue;
        }
        words.push(i);
        i += 1;
    }
    let (c, s) = (*words.first()?, *words.get(1)?);
    let subs = TAGGED.iter().find(|(t, _)| *t == args[c])?.1;
    subs.contains(&args[s].as_str()).then_some((c, s))
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
        if a == "--assignee" && i + 1 < args.len() {
            names.push(args[i + 1].as_str());
            i += 2;
            continue;
        }
        if let Some(v) = a.strip_prefix("--assignee=") {
            names.push(v);
            i += 1;
            continue;
        }
        // `-a` in every shorthand spelling gh takes, the value-less
        // letters of a cluster included: `gh issue create -wa me`
        // assigns as much as `-a me` does, and missing it would cost the
        // new item its `mode=delegate` and its creator ownership. This
        // is only ever asked about a create, where `-a` names an
        // assignee, so the walk uses that command's letters.
        if let Some(('a', attached)) = cluster(a, false).and_then(|cl| cl.valued) {
            match attached {
                Some(v) => {
                    names.push(v);
                    i += 1;
                }
                None => {
                    if let Some(v) = args.get(i + 1) {
                        names.push(v.as_str());
                    }
                    i += 2;
                }
            }
            continue;
        }
        // The argument after a flag that takes a value is that value, so
        // a login written in a body or a title is not an assignee. This
        // scan decides whether a new item starts a session of its own,
        // so a wrong yes here is a session nobody asked for.
        let skips_value = match cluster(a, false).and_then(|cl| cl.valued) {
            Some((c, None)) => valued_shorthand(c, false),
            _ => valued_long(a),
        };
        i += if skips_value { 2 } else { 1 };
    }
    names
        .iter()
        .flat_map(|n| n.split(','))
        .map(|n| n.trim().trim_start_matches('@'))
        .any(|n| n.eq_ignore_ascii_case("me") || bot.is_some_and(|b| n.eq_ignore_ascii_case(b)))
}

/// The letters `pr review` gives its three action flags. The long names
/// are spelled out in `action_flag`, which is the only place needing
/// them; this is the letter half, shared by the two readings that walk a
/// cluster.
const ACTIONS: [char; 3] = ['a', 'c', 'r'];

/// Shorthand letters that carry no value, so a cluster continues past
/// them, read out of `gh <command> --help`. The two sets are disjoint
/// and have to be: on a review `-a` approves and `-r` requests changes,
/// while on a create `-a` names an assignee and `-r` a reviewer and both
/// take a value. The second set is the union over the four commands
/// rather than any one of them -- `-d` and `-f` are only on `pr create`,
/// `-e` and `-w` are on all four -- and being over-broad there is safe
/// because gh refuses the lines where it is wrong: `gh issue create -db
/// hi` is `unknown shorthand flag: 'd' in -db`, stamped or not. A letter
/// in neither set ends the walk, which is the other safe way to be
/// wrong: it stops the scan rather than reading a value as more flags.
/// `-h` is the one value-less shorthand these sets leave out, and
/// deliberately: a line carrying `--help` prints help and posts nothing,
/// so no reading of it can be observed. Read from gh 2.98.0. The one
/// direction that would matter is a new
/// value-less shorthand on a create or a comment: a body behind it would
/// stop the walk and go out unstamped, so this is worth re-reading when
/// gh grows a flag.
fn boolean_shorthand(c: char, review: bool) -> bool {
    if review {
        ACTIONS.contains(&c)
    } else {
        matches!(c, 'd' | 'e' | 'f' | 'w')
    }
}

/// Shorthand letters that take a value on the commands this shim tags,
/// listed from `gh <command> --help` rather than assumed. A letter
/// missing from here is read the way it was before, which is the safe
/// direction to be wrong; a value-less letter wrongly listed is the
/// unsafe one, since the argument after it would be stepped over and a
/// body sitting there would go out unstamped.
fn valued_shorthand(c: char, review: bool) -> bool {
    if review {
        matches!(c, 'b' | 'F' | 'R')
    } else {
        matches!(
            c,
            'a' | 'B' | 'b' | 'F' | 'H' | 'l' | 'm' | 'p' | 'R' | 'r' | 'T' | 't'
        )
    }
}

/// The same for the long flags, and for the same reason.
///
/// This is also the list that keeps a URL in a flag's value from being
/// read as the item, which is why `--body`, `--body-file` and `--title`
/// are in it although the rewrite loop reads them earlier: `assigns_bot`
/// and `repo_in_args` have no such branch, and for them this is the only
/// thing that stops a login or a URL in a body being taken for one.
/// `--parent`, `--blocked-by` and `--blocking` take "numbers or URLs" in
/// gh's own help, so a URL there is a relation and not the item either.
///
/// A blanket positional rule ("the argument after an option is its
/// value") would need no list at all, but it would hide the item's own
/// URL behind a value-less flag -- `gh pr review --approve <url>` is
/// gh's own example -- and the answer would fall through to the
/// checkout, giving the short `#N` on a post landing elsewhere. Naming
/// the flags is the safer shape for the same reason it is safer above: a
/// flag missing from here costs only the reading it had before, while a
/// value-less flag wrongly in it would step over something that
/// mattered.
fn valued_long(a: &str) -> bool {
    matches!(
        a,
        "--assignee"
            | "--base"
            | "--blocked-by"
            | "--blocking"
            | "--body"
            | "--body-file"
            | "--head"
            | "--label"
            | "--milestone"
            | "--parent"
            | "--project"
            | "--recover"
            | "--repo"
            | "--reviewer"
            | "--template"
            | "--title"
            | "--type"
    )
}

/// A single-dash argument read the way pflag reads it: every letter is a
/// flag of its own, and the first one that takes a value swallows the
/// rest of the cluster, or the next argument when the cluster ends
/// there. So `-ab hi`, `-abhi` and `-ab=hi` all approve with a body of
/// `hi`, while `-ba hi` is a body of `a` and no approval at all.
struct Cluster<'a> {
    /// The value-less letters ahead of the one that takes a value.
    bools: &'a str,
    /// That letter, and its value where the cluster carries one; `None`
    /// for the value means the next argument carries it instead.
    valued: Option<(char, Option<&'a str>)>,
}

fn cluster(a: &str, review: bool) -> Option<Cluster<'_>> {
    let rest = a
        .strip_prefix('-')
        .filter(|r| !r.is_empty() && !r.starts_with('-'))?;
    for (n, c) in rest.char_indices() {
        let after = &rest[n + c.len_utf8()..];
        // pflag asks about an attached `=` before it asks whether the
        // flag takes a value at all, which is why `-a=false` really does
        // set `--approve` to false. A letter followed by nothing but `=`
        // is not that shape: there the `=` is the value.
        let attached = if after.len() > 1 && after.starts_with('=') {
            Some(&after[1..])
        } else if boolean_shorthand(c, review) {
            continue;
        } else if after.is_empty() {
            None
        } else {
            Some(after)
        };
        return Some(Cluster {
            bools: &rest[..n],
            valued: Some((c, attached)),
        });
    }
    Some(Cluster {
        bools: rest,
        valued: None,
    })
}

/// The values gh reads as true, which is Go's `strconv.ParseBool`. The
/// false ones (`0`, `f`, `F`, `FALSE`, `false`, `False`) parse perfectly
/// well and simply leave the flag unset; only a value that is neither,
/// such as `--approve=yes`, makes gh refuse the line outright. Either
/// way the flag is not an action.
fn flag_true(v: &str) -> bool {
    matches!(v, "1" | "t" | "T" | "TRUE" | "true" | "True")
}

/// Does this argument carry one of `pr review`'s action flags, set? The
/// action is what decides whether gh posts at all, and it has more
/// spellings than a bare word: `--approve`, `--approve=true`, `-a`,
/// `-a=true`, and inside a cluster as the `-a` of `-aR o/r` or of
/// `-ab hi`.
/// `--approve=false` carries the flag but not the action, exactly as gh
/// reads it, and a value gh would refuse is no action either. One thing
/// it does not do is prefer the last of a repeated flag the way gh
/// does, so `-aa=false` reads as an action here and not to gh. Nothing
/// posts either way -- gh refuses that line for having no action, and
/// refuses it again for carrying a body without one -- so the answer
/// only changes which of the two complaints comes back.
fn action_flag(a: &str) -> bool {
    for name in ["--approve", "--comment", "--request-changes"] {
        if a == name {
            return true;
        }
        if let Some(v) = a.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
            return flag_true(v);
        }
    }
    // Only an action letter can reach `bools` on a review, so the first
    // test could be a bare emptiness check; it names the letters anyway,
    // so that a change to the boolean set cannot quietly turn some other
    // flag into an action here.
    cluster(a, true).is_some_and(|cl| {
        cl.bools.chars().any(|c| ACTIONS.contains(&c))
            || cl
                .valued
                .is_some_and(|(c, v)| ACTIONS.contains(&c) && v.is_some_and(flag_true))
    })
}

impl Shim<'_> {
    /// The gh arguments with the byline and origin tag prepended to the
    /// body, where there is one.
    pub fn rewrite(&self, args: Vec<String>) -> Vec<String> {
        // `command_words` only ever names a pair from `TAGGED`, so
        // finding one is the whole of the test.
        let Some((c, s)) = command_words(&args) else {
            return args;
        };
        let origin = self.origin;
        // Both scans read the whole line, not just what follows the
        // subcommand: gh lets a subcommand's flags float before it, so a
        // `--repo` or an `--assignee` can sit ahead of the command words
        // and must still count. The command words themselves are bare
        // vocabulary, never a repository or a login, so including them
        // changes no answer.
        let delegate = creates(&args[s]) && assigns_bot(&args, self.bot);
        // Which letters take a value depends on the command: `-a` is an
        // assignee on a create and an approval on a review.
        let review = args[c] == "pr" && args[s] == "review";
        let on_repo = repo_in_args(&args, review)
            .or_else(|| self.gh_repo.and_then(repo_of))
            .or_else(|| (self.checkout)());
        let stamp = |body: &str| stamp_with(body, origin, on_repo.as_deref(), delegate);
        // The body flag floats like any other, so the whole line is
        // walked rather than what follows the subcommand. Leaving one
        // ahead of the command words unstamped would post it untagged,
        // and worse, `pr review` below would then add a second `--body`:
        // gh takes the last, so the agent's text would be dropped, and a
        // `--body-file` alongside it is refused outright.
        let mut out: Vec<String> = Vec::with_capacity(args.len());
        let mut stamped = false;
        let mut has_action = false;
        // Where a `--` of gh's own ended the flags, if one did.
        let mut ends_flags = None;
        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            if a == "--" {
                ends_flags = Some(i);
                out.extend_from_slice(&args[i..]);
                break;
            }
            // Whether the line already carries an action is read here
            // rather than in a scan of its own. This walk is the one
            // that knows a flag's value from a flag, and a `--` is a
            // value like any other when a flag takes it: `gh pr review 3
            // -R -- --repo o/r -a` posts, and a scan that stopped at
            // that `--` would never see the approval and would let it
            // out untagged.
            has_action = has_action || (review && action_flag(a));
            // The shorthand spellings of a body, cluster included, all
            // come from the one reading of the argument.
            let short = cluster(a, review).and_then(|cl| cl.valued.map(|(c, v)| (cl.bools, c, v)));
            // Inline body: --body X, --body=X, -b X, -bX, -b=X, and the
            // same three behind leading boolean letters (-ab hi).
            if a == "--body" && i + 1 < args.len() {
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
            if let Some((bools, 'b', attached)) = short {
                // Re-emitted the way it arrived: attached to the cluster
                // where the value was attached, and as the next argument
                // where it was the next argument.
                match attached {
                    Some(v) => {
                        out.push(format!("-{bools}b{}", stamp(v)));
                        stamped = true;
                        i += 1;
                        continue;
                    }
                    None if i + 1 < args.len() => {
                        out.push(format!("-{bools}b"));
                        out.push(stamp(&args[i + 1]));
                        stamped = true;
                        i += 2;
                        continue;
                    }
                    // A `-b` with nothing after it is gh's to complain
                    // about.
                    None => {}
                }
            }
            // Body from a file (or stdin): moved onto the command line so no
            // temporary file is needed. Any boolean letters clustered
            // ahead of the `-F` are kept, since the `--body` replacing it
            // cannot carry them -- and then the body has to be attached
            // to it with an `=`, because those letters are a word of
            // their own and cobra pairs `-a` with the word after it
            // when it goes looking for the command. `gh -aF/dev/null pr
            // review 3` reaches the API; split into three words it comes
            // back `unknown command`, the byline having been read as the
            // command. The value has to be attached for gh to take a
            // cluster there at all, so `-aF notes.md` ahead of the words
            // is already `unknown command` before the shim sees it.
            let file = if a == "--body-file" && i + 1 < args.len() {
                Some(("", args[i + 1].as_str(), 2))
            } else if let Some(v) = a.strip_prefix("--body-file=") {
                Some(("", v, 1))
            } else {
                match short {
                    Some((bools, 'F', Some(v))) => Some((bools, v, 1)),
                    Some((bools, 'F', None)) => args.get(i + 1).map(|v| (bools, v.as_str(), 2)),
                    _ => None,
                }
            };
            if let Some((bools, path, used)) = file {
                let Ok(text) = (self.read)(path) else {
                    return args; // let gh report the unreadable file
                };
                let body = stamp(&text);
                if body.len() > MAX_INLINE_BODY {
                    return args;
                }
                if bools.is_empty() {
                    out.push("--body".to_string());
                    out.push(body);
                } else {
                    out.push(format!("-{bools}"));
                    out.push(format!("--body={body}"));
                }
                stamped = true;
                i += used;
                continue;
            }
            // The argument after a flag that takes a value is that
            // value and not a flag of its own. Reading it as one is how
            // a `--title -dbhello` came back with the title stamped as
            // though the `-b` in it were a body.
            let skips_value = match short {
                Some((_, c, None)) => valued_shorthand(c, review),
                _ => valued_long(a),
            };
            out.push(a.to_string());
            i += 1;
            // A `--` here is the value and not the end of the flags:
            // pflag hands it to the flag like any other word, and `gh pr
            // review 3 -F --` really does look for a file called `--`.
            if let Some(v) = args.get(i).filter(|_| skips_value) {
                out.push(v.clone());
                i += 1;
            }
        }
        // An approval needs no body, but should still say where it came from.
        // Without an action flag gh would prompt (or reject --body), so those
        // are left alone.
        // The walk above reads the whole line and not just what follows
        // the subcommand, because a boolean flag floats too, wherever
        // there is a spare positional for cobra to feed the word it
        // swallows during command lookup (`gh --approve 3 pr review`
        // parses; `gh --approve pr review 3` does not). Reading only the
        // subcommand's own flags left that approval unstamped, which is
        // the tag loss this whole change is about. `-a`, `-c` and `-r`
        // have no other meaning in it, since it is only asked on a
        // `review`.
        // Nothing was rewritten when `stamped` is false -- every branch
        // above sets it -- so `out` still matches `args` position for
        // position and `s + 1` is where the subcommand's flags begin.
        //
        // Unless a `--` got there first, in which case that position is
        // among gh's positionals and no flag can go in it. `gh pr -a --
        // review` reviews the current branch and posts; adding the body
        // there makes gh answer `accepts at most 1 arg(s)`, and adding
        // it before the `--` instead makes cobra pair the `-a` with the
        // `--body` and read the byline as the command name. There is no
        // third position, so the line is left as it is: an approval that
        // goes out untagged, exactly as it did before any of this, and
        // the one shape here that this change cannot reach.
        if !stamped && review && has_action && ends_flags.is_none_or(|t| t > s) {
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
            args(&["issue", "create", "-t", "t", "-b", "hi", "-R", "acme/other"]),
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

    /// Every spelling gh accepts for an action flag, and every spelling
    /// it refuses to treat as one. Each was run against the real gh
    /// binary -- not the one on a session's PATH, which is this shim --
    /// on a pull request number that does not exist. What tells the two
    /// groups apart is gh's own "--approve, --request-changes, or
    /// --comment required": the accepted ones get past that line and
    /// fail on something later, the number or a blank body, and the
    /// refused ones stop there or, for a value it cannot parse, before
    /// it. Only some reach the API, so reaching it is not the test.
    #[test]
    fn action_flags_are_seen_in_every_spelling() {
        for a in [
            // Bare, long and short.
            args(&["pr", "review", "7", "--approve"]),
            args(&["pr", "review", "7", "-a"]),
            args(&["pr", "review", "7", "--comment"]),
            args(&["pr", "review", "7", "-c"]),
            args(&["pr", "review", "7", "--request-changes"]),
            args(&["pr", "review", "7", "-r"]),
            // Attached: gh parses the value, so a true one is an action.
            args(&["pr", "review", "7", "--approve=true"]),
            args(&["pr", "review", "7", "--approve=1"]),
            args(&["pr", "review", "7", "--approve=True"]),
            args(&["pr", "review", "7", "-a=true"]),
            args(&["pr", "review", "7", "-a=t"]),
            args(&["pr", "review", "7", "-a=T"]),
            args(&["pr", "review", "7", "--request-changes=TRUE"]),
            args(&["pr", "review", "7", "--comment=true"]),
            // Each action flag in the attached short form, not just the
            // approval: narrowing the reading to `-a` posts a `-c=true`
            // review with no byline and no tag.
            args(&["pr", "review", "7", "-c=true"]),
            args(&["pr", "review", "7", "-r=true"]),
            // Clustered. `-ac` and `-ra` name two actions, so gh will
            // not run them either way -- bare, it asks for a body; with
            // one, it asks for exactly one action -- but both get past
            // the line that decides this test. `-aR o/r` is the one
            // cluster here that gh runs.
            args(&["pr", "review", "7", "-ac"]),
            args(&["pr", "review", "7", "-ra"]),
        ] {
            let mut want = a.clone();
            want.splice(2..2, [args(&["--body"])[0].clone(), line()]);
            assert_eq!(rewrite(a.clone()), want, "{a:?}");
        }
        // The action is clustered ahead of a flag that takes a value,
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
            // The flag is there, the action is not: gh still refuses the
            // line for want of one, so adding a body would be adding it
            // to a review that never posts.
            args(&["pr", "review", "7", "--approve=false"]),
            args(&["pr", "review", "7", "--approve=F"]),
            args(&["pr", "review", "7", "--comment=false"]),
            args(&["pr", "review", "7", "--request-changes=false"]),
            args(&["pr", "review", "7", "-a=false"]),
            args(&["pr", "review", "7", "-a=0"]),
            args(&["pr", "review", "7", "-c=false"]),
            args(&["pr", "review", "7", "-r=0"]),
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
        // A `--` of its own still ends them.
        let a = args(&["pr", "review", "7", "--", "--approve"]);
        assert_eq!(rewrite(a.clone()), a);
    }

    /// A body inside a shorthand cluster is still a body. Reading the
    /// action flag without reading this would put a second `--body` on
    /// the line, and gh takes the last: the agent's text would be
    /// dropped.
    #[test]
    fn bodies_inside_a_cluster_are_stamped() {
        let body = format!("{}\n\nhello", line());
        // The cluster is re-emitted whole, and the value goes back the
        // way it arrived: its own argument where it was one, attached
        // where it was attached.
        for (a, want) in [
            (
                args(&["pr", "review", "7", "-ab", "hello"]),
                args(&["pr", "review", "7", "-ab", &body]),
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
                args(&["pr", "review", "7", "-cb", &body]),
            ),
            (
                args(&["pr", "review", "7", "-rb", "hello"]),
                args(&["pr", "review", "7", "-rb", &body]),
            ),
            // Every value-less letter a body can legally sit behind.
            // `-e` is not one of them on any command here: gh refuses
            // `--editor` next to a `--body` outright, so a cluster it
            // leads is dead in every spelling and cannot be pinned.
            (
                args(&["pr", "create", "-t", "t", "-db", "hello"]),
                args(&["pr", "create", "-t", "t", "-db", &body]),
            ),
            (
                args(&["pr", "create", "-t", "t", "-fb", "hello"]),
                args(&["pr", "create", "-t", "t", "-fb", &body]),
            ),
            (
                args(&["pr", "create", "-t", "t", "-wb", "hello"]),
                args(&["pr", "create", "-t", "t", "-wb", &body]),
            ),
            // `-e` is dead next to a body on the two comment commands,
            // where gh refuses `--editor` alongside `--body`, but it is
            // live on the two creates, which prompt in a terminal.
            (
                args(&["issue", "create", "-t", "t", "-eb", "hello"]),
                args(&["issue", "create", "-t", "t", "-eb", &body]),
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
        // The body flag comes first here, so `a` is its value and there
        // is no approval on the line at all.
        assert_eq!(
            rewrite(args(&["pr", "review", "7", "-ba", "hello"])),
            args(&["pr", "review", "7", &format!("-b{}\n\na", line()), "hello",])
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
        // The attached form, which used to make the shim read a file
        // called `=notes.md`, fail, and hand the line to gh unstamped.
        assert_eq!(
            shim.rewrite(args(&["pr", "create", "-t", "t", "-F=notes.md"])),
            args(&[
                "pr",
                "create",
                "-t",
                "t",
                "--body",
                &format!("{}\n\nread notes.md", line())
            ])
        );
        // With no letters to keep, the two-word form gh has always had
        // is left as it is: cobra pairs `--body` with the word after it,
        // so the command is still found.
        assert_eq!(
            shim.rewrite(args(&["-F", "notes.md", "pr", "review", "7"])),
            args(&[
                "--body",
                &format!("{}\n\nread notes.md", line()),
                "pr",
                "review",
                "7"
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
            // action: `-R` takes the `-ab`, so this line has no body and
            // no action, and nothing is added to it.
            (
                args(&["pr", "review", "7", "-R", "-ab"]),
                args(&["pr", "review", "7", "-R", "-ab"]),
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
            "--repo",
            "--reviewer",
            "--template",
            "--title",
            "-a",
            "-B",
            "-H",
            "-l",
            "-m",
            "-p",
            "-R",
            "-r",
            "-T",
            "-t",
        ] {
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
                    "-la",
                    "OverlayBot",
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
}
