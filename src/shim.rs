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
}
