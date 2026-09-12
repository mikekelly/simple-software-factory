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
//! its environment, explicit body files/stdin, and `git config` for that
//! remote. Large bodies use an inherited anonymous file rather than argv.
//! Interactive flows and the terminal remain gh’s responsibility.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::origin::{Origin, stamp_with};

/// Keep individual body arguments well below exec limits, including Linux’s
/// per-argument limit. Larger bodies travel through an inherited file.
const MAX_INLINE_BODY: usize = 32_000;

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
    let mut body_files = Vec::new();
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
            let args = shim
                .try_rewrite(args)
                .and_then(|args| transport_bodies(args, &mut body_files));
            match args {
                Ok(args) => {
                    cmd.args(args);
                }
                Err(err) => {
                    eprintln!("gh: ssf could not prepare the body: {err:#}");
                    std::process::exit(1);
                }
            }
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
    let mut bytes = Vec::new();
    if path == "-" {
        std::io::stdin().read_to_end(&mut bytes)?;
    } else {
        std::fs::File::open(path)?.read_to_end(&mut bytes)?;
    }
    String::from_utf8(bytes)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

/// Anonymous file kept open across exec; gh reopens the descriptor path.
/// Linux uses memory, other Unix systems use an immediately unlinked private
/// temporary file. No pathname survives a normal return or successful exec.
fn body_file(body: &str) -> Result<std::fs::File> {
    use std::io::{Seek, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    #[cfg(target_os = "linux")]
    let mut file = {
        let fd = unsafe { libc::memfd_create(c"ssf-gh-body".as_ptr(), 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        unsafe { std::fs::File::from_raw_fd(fd) }
    };
    #[cfg(not(target_os = "linux"))]
    let mut file = {
        use std::os::unix::ffi::OsStrExt;
        let path = std::env::temp_dir().join("ssf-gh-body-XXXXXX");
        let mut name = std::ffi::CString::new(path.as_os_str().as_bytes())?.into_bytes_with_nul();
        let fd = unsafe { libc::mkstemp(name.as_mut_ptr().cast()) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        if unsafe { libc::unlink(name.as_ptr().cast()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        file
    };
    file.write_all(body.as_bytes())?;
    file.rewind()?;
    // Keep the descriptor inheritable across exec.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(file)
}

fn transport_bodies(mut args: Vec<String>, files: &mut Vec<std::fs::File>) -> Result<Vec<String>> {
    use std::os::fd::AsRawFd;
    let Some((c, s)) = command_words(&args) else {
        return Ok(args);
    };
    let review = args[c] == "pr" && args[s] == "review";
    let needs_file = args.iter().map(|a| a.len() + 1).sum::<usize>() > MAX_INLINE_BODY
        || args.iter().any(|a| a.contains('\0'));
    if !needs_file {
        return Ok(args);
    }
    let mut n = 0;
    while n < args.len() {
        let a = args[n].as_str();
        if a == "--" {
            break;
        }
        let short = cluster(a, review).and_then(|cl| cl.valued);
        // An unrevised file flag means the rewrite deliberately left this
        // invocation to gh (for example its mutually exclusive flags).
        if a == "--body-file" || a.starts_with("--body-file=") || matches!(short, Some(('F', _))) {
            return Ok(args);
        }
        let takes = valued_long(a)
            || short.is_some_and(|(ch, v)| v.is_none() && valued_shorthand(ch, review));
        n += if takes { 2 } else { 1 };
    }
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            break;
        }
        let short = cluster(a, review);
        let body = if a == "--body" {
            args.get(i + 1).map(|v| (v.as_str(), None, true))
        } else if let Some(v) = a.strip_prefix("--body=") {
            Some((v, None, false))
        } else if let Some(cl) = &short {
            match cl.valued {
                Some(('b', Some(v))) => Some((v, Some(cl.bools), false)),
                Some(('b', None)) => args.get(i + 1).map(|v| (v.as_str(), Some(cl.bools), true)),
                _ => None,
            }
        } else {
            None
        };
        if let Some((body, bools, separate)) = body {
            let file = body_file(body)?;
            let path = format!("/dev/fd/{}", file.as_raw_fd());
            let flag = match bools {
                Some(bools) => format!("-{bools}F"),
                None => "--body-file".to_string(),
            };
            args[i] = if separate {
                flag
            } else if bools.is_some() {
                format!("{flag}{path}")
            } else {
                format!("{flag}={path}")
            };
            if separate {
                args[i + 1] = path;
            }
            files.push(file);
            i += if separate { 2 } else { 1 };
        } else {
            let skips = short
                .and_then(|cl| cl.valued)
                .is_some_and(|(c, v)| v.is_none() && valued_shorthand(c, review))
                || valued_long(a);
            i += if skips { 2 } else { 1 };
        }
    }
    Ok(args)
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

/// Infer the destination from --repo/-R, otherwise a positional item URL.
/// Known differences from gh: the first --repo wins over later flags and
/// item URLs, and an invalid --repo stops inference. This affects byline
/// link selection, not whether the post receives an origin tag.
fn repo_in_args(args: &[String], review: bool) -> Option<String> {
    let mut i = 0;
    let mut url = None;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            // After the flag terminator, continue looking for a positional URL.
            return url.or_else(|| args[i + 1..].iter().find_map(|a| item_url(a)));
        }
        if (a == "--repo" || a == "-R") && i + 1 < args.len() {
            return repo_of(&args[i + 1]);
        }
        if let Some(v) = a.strip_prefix("--repo=") {
            return repo_of(v);
        }
        // -R may follow boolean letters in a cluster.
        if let Some(('R', attached)) = cluster(a, review).and_then(|cl| cl.valued) {
            return match attached {
                Some(v) => repo_of(v),
                None => args.get(i + 1).and_then(|v| repo_of(v)),
            };
        }
        // Flag values, including --, are not positional item URLs.
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

/// Cobra command lookup consumes a following word for --long or -x
/// without '='; attached values and clusters consume only themselves.
/// Help/version are exceptions but do not post. The caller handles --.
fn may_take_a_value(a: &str) -> bool {
    !a.contains('=') && a.starts_with('-') && (a.starts_with("--") || a.len() == 2)
}

/// Find the first two command words using Cobra's flag-skipping rules.
/// Subcommand flags may precede the command; clusters consume one word.
/// Accept only a tagged command pair, never later matching positionals.
fn command_words(args: &[String]) -> Option<(usize, usize)> {
    let mut words = Vec::with_capacity(2);
    let mut i = 0;
    while i < args.len() && words.len() < 2 {
        let a = args[i].as_str();
        if a == "--" {
            break;
        }
        if a.starts_with('-') {
            // A flag's value may itself be --; that does not end flag parsing.
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
        // On create, clustered -a takes an assignee rather than an action.
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
        // Logins in body/title values must not trigger delegation.
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

/// The letters `pr review` gives its three action flags, which is what
/// makes them value-less there. Only `-a` decides anything further on:
/// see `approves`.
const ACTIONS: [char; 3] = ['a', 'c', 'r'];

/// Boolean shorthands from gh 2.98.0 help. Review and create assign
/// different meanings to -a/-r. Unknown letters stop cluster scanning;
/// revisit this table when gh adds flags. Help does not post.
fn boolean_shorthand(c: char, review: bool) -> bool {
    if review {
        ACTIONS.contains(&c)
    } else {
        matches!(c, 'd' | 'e' | 'f' | 'w')
    }
}

/// Value-taking shorthands on supported commands. Do not add boolean
/// flags: skipping their following word could hide a body or item URL.
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

/// Value-taking long flags shared by body, assignee and repository scans.
/// Body/title/relation values may contain logins or URLs without naming
/// the assignee or destination item.
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
        // pflag binds =value before boolean handling; a lone '=' is the value.
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

/// Go strconv.ParseBool's true spellings; false or invalid values do not act.
fn flag_true(v: &str) -> bool {
    matches!(v, "1" | "t" | "T" | "TRUE" | "true" | "True")
}

fn review_action(a: &str, long: &str, letter: char) -> Option<bool> {
    if a == long {
        return Some(true);
    }
    if let Some(v) = a.strip_prefix(long).and_then(|rest| rest.strip_prefix('=')) {
        return Some(flag_true(v));
    }
    let cl = cluster(a, true)?;
    if let Some((ch, Some(v))) = cl.valued
        && ch == letter
    {
        return Some(flag_true(v));
    }
    cl.bools.contains(letter).then_some(true)
}

/// Only approval may receive a byline-only body; comment/request-changes
/// require user content. Repeated flags are not resolved last-wins here;
/// gh still validates the resulting action combination.
fn approves(a: &str) -> bool {
    if a == "--approve" {
        return true;
    }
    if let Some(v) = a
        .strip_prefix("--approve")
        .and_then(|r| r.strip_prefix('='))
    {
        return flag_true(v);
    }
    cluster(a, true).is_some_and(|cl| {
        cl.bools.contains('a')
            || cl
                .valued
                .is_some_and(|(c, v)| c == 'a' && v.is_some_and(flag_true))
    })
}

impl Shim<'_> {
    /// The gh arguments with the byline and origin tag prepended to the
    /// body, where there is one.
    #[cfg(test)]
    pub fn rewrite(&self, args: Vec<String>) -> Vec<String> {
        self.try_rewrite(args).unwrap()
    }

    pub fn try_rewrite(&self, args: Vec<String>) -> Result<Vec<String>> {
        // `command_words` only ever names a pair from `TAGGED`, so
        // finding one is the whole of the test.
        let Some((c, s)) = command_words(&args) else {
            return Ok(args);
        };
        let review = args[c] == "pr" && args[s] == "review";
        // Cobra removes command words before pflag binds values. Normalize
        // only lines where a separated value crosses one of those words.
        let mut n = 0;
        while n < args.len() {
            if args[n] == "--" {
                break;
            }
            let takes = valued_long(&args[n])
                || cluster(&args[n], review)
                    .and_then(|cl| cl.valued)
                    .is_some_and(|(ch, v)| v.is_none() && valued_shorthand(ch, review));
            if takes && (n + 1 == c || n + 1 == s) {
                let mut normalized = vec![args[c].clone(), args[s].clone()];
                normalized.extend(
                    args.iter()
                        .enumerate()
                        .filter(|(i, _)| *i != c && *i != s)
                        .map(|(_, a)| a.clone()),
                );
                let rewritten = self.try_rewrite(normalized.clone())?;
                return Ok(if rewritten == normalized {
                    args
                } else {
                    rewritten
                });
            }
            n += if takes { 2 } else { 1 };
        }
        // Read only pflag's final body-file value, and preserve gh's
        // body/body-file mutual exclusion before touching stdin.
        let mut comments = false;
        let mut requests_changes = false;
        let mut inline = false;
        let mut last_inline = None;
        let mut last_file = None;
        let mut n = 0;
        while n < args.len() {
            let a = args[n].as_str();
            if a == "--" {
                break;
            }
            let cl = cluster(a, review);
            if review {
                if let Some(value) = review_action(a, "--comment", 'c') {
                    comments = value;
                }
                if let Some(value) = review_action(a, "--request-changes", 'r') {
                    requests_changes = value;
                }
            }
            let short = cl.and_then(|cl| cl.valued);
            if a == "--body" || a.starts_with("--body=") || matches!(short, Some(('b', _))) {
                inline = true;
            }
            if a == "--body" {
                last_inline = args.get(n + 1).map(String::as_str);
            } else if let Some(v) = a.strip_prefix("--body=") {
                last_inline = Some(v);
            } else if let Some(('b', v)) = short {
                last_inline = v.or_else(|| args.get(n + 1).map(String::as_str));
            }
            if a == "--body-file" {
                last_file = args.get(n + 1).map(String::as_str);
            } else if let Some(v) = a.strip_prefix("--body-file=") {
                last_file = Some(v);
            } else if let Some(('F', v)) = short {
                last_file = v.or_else(|| args.get(n + 1).map(String::as_str));
            }
            let takes = valued_long(a)
                || short.is_some_and(|(ch, v)| v.is_none() && valued_shorthand(ch, review));
            if takes && n + 1 == args.len() {
                return Ok(args);
            }
            n += if takes { 2 } else { 1 };
        }
        if inline && last_file.is_some() {
            return Ok(args);
        }
        let file_text = match last_file {
            Some(path) => match (self.read)(path) {
                Ok(text) => Some(text),
                Err(err) if path == "-" || err.kind() == std::io::ErrorKind::InvalidData => {
                    return Err(err).context(
                        "reading body; refusing to run gh with consumed stdin or invalid UTF-8",
                    );
                }
                Err(_) => return Ok(args),
            },
            None => None,
        };
        if (comments || requests_changes)
            && file_text
                .as_deref()
                .or(last_inline)
                .is_some_and(|body| body.trim().is_empty())
        {
            anyhow::bail!("body cannot be blank for comment or request-changes review");
        }
        let origin = self.origin;
        // Repository and assignee flags may precede the command words.
        let delegate = creates(&args[s]) && assigns_bot(&args, self.bot);
        // Which letters take a value depends on the command: `-a` is an
        // assignee on a create and an approval on a review.
        let review = args[c] == "pr" && args[s] == "review";
        let on_repo = repo_in_args(&args, review)
            .or_else(|| self.gh_repo.and_then(repo_of))
            .or_else(|| (self.checkout)());
        let stamp = |body: &str| stamp_with(body, origin, on_repo.as_deref(), delegate);
        // Body flags may also precede the command words.
        let mut out: Vec<String> = Vec::with_capacity(args.len());
        let mut stamped = false;
        let mut approved = false;
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
            // Read actions while walking values so a value's -- is not a terminator.
            approved = approved || (review && approves(a));
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
                // Attach the body to a cluster so Cobra cannot mistake it for
                // a command word. A standalone -b consumes the following word.
                match attached {
                    Some(v) => {
                        out.push(format!("-{bools}b{}", stamp(v)));
                        stamped = true;
                        i += 1;
                        continue;
                    }
                    None if i + 1 < args.len() => {
                        let body = stamp(&args[i + 1]);
                        if bools.is_empty() {
                            out.push("-b".to_string());
                            out.push(body);
                        } else {
                            out.push(format!("-{bools}b{body}"));
                        }
                        stamped = true;
                        i += 2;
                        continue;
                    }
                    // A `-b` with nothing after it is gh's to complain
                    // about.
                    None => {}
                }
            }
            // Rewrite the pre-read body file, preserving any clustered booleans.
            // transport_bodies moves large results into inherited files.
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
            if let Some((bools, _path, used)) = file {
                let body = stamp(
                    file_text
                        .as_deref()
                        .expect("body file was read before rewriting"),
                );
                // Preserve attached spellings and attach after clustered flags:
                // a bare body there could become Cobra's command word.
                if !bools.is_empty() {
                    out.push(format!("-{bools}"));
                }
                if used == 1 || !bools.is_empty() {
                    out.push(format!("--body={body}"));
                } else {
                    out.push("--body".to_string());
                    out.push(body);
                }
                stamped = true;
                i += used;
                continue;
            }
            // Skip values rather than interpreting body-like text as flags.
            let skips_value = match short {
                Some((_, c, None)) => valued_shorthand(c, review),
                _ => valued_long(a),
            };
            out.push(a.to_string());
            i += 1;
            // A consumed -- is a value, not the end of flags.
            if let Some(v) = args.get(i).filter(|_| skips_value) {
                out.push(v.clone());
                i += 1;
            }
        }
        // Add attribution only to bodyless approvals. Do not insert after a
        // terminator preceding the subcommand: gh would read it as positional.
        // With no rewrite, output indices still match the original arguments.
        if !stamped && review && approved && ends_flags.is_none_or(|t| t > s) {
            out.insert(s + 1, stamp_with("", origin, on_repo.as_deref(), delegate));
            out.insert(s + 1, "--body".to_string());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
