//! The session environment the shim runs with.
//!
//! A harness may start a tool of its own with a scrubbed environment: OMP's
//! Python tool keeps `HOME`, `PATH` and a handful more, and everything `ssf
//! launch` exported for the pane is gone. The shim is still reached — `PATH`
//! survives — but with no `SSF_REPO` it stamps nothing, with no `GH_CONFIG_DIR`
//! and `GH_TOKEN` gh reads the human's `~/.config/gh`, and with no
//! `GIT_SSH_COMMAND`, `GIT_CONFIG_*` or credential helper a push goes out as
//! the operator. So the shim reads the session's own variables back out of the
//! environment of an ancestor process, which still has them, and hands them to
//! the program it execs.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::process::Command;

use super::Origin;

/// What marks an environment as a session's: `ssf launch` exports the item it
/// was started for, and neither the daemon nor a person's shell has one. The
/// nearest ancestor with it is the session this process belongs to.
const MARKER: &str = "SSF_REPO";

/// How far up the process tree to look for that ancestor. A pane's harness is
/// a few steps away; a chain without a session reaches the top of the tree or
/// an unreadable process first.
const ANCESTORS: u32 = 16;

/// The session variables this process does not have, read from the nearest
/// ancestor that has them: empty outside a session, when this process still has
/// the session itself (so a variable it lacks was dropped on purpose), and when
/// `/proc` cannot be read.
pub(super) struct Session {
    missing: Vec<(OsString, OsString)>,
}

impl Session {
    /// What the current process is missing of its session, if it is in one.
    pub(super) fn current() -> Self {
        Self { missing: recover() }
    }

    /// A session variable as text: this process's own value, else the
    /// recovered one.
    pub(super) fn var(&self, name: &str) -> Option<String> {
        if let Ok(value) = std::env::var(name) {
            return Some(value);
        }
        self.missing
            .iter()
            .find(|(key, _)| key.as_os_str() == OsStr::new(name))
            .map(|(_, value)| value.to_string_lossy().into_owned())
    }

    /// The item this process's session works on, as the shim stamps posts.
    pub(super) fn origin(&self) -> Option<Origin> {
        let repo = self.var("SSF_REPO")?;
        let number = self.var("SSF_ISSUE")?.trim().parse().ok()?;
        Origin::new(&repo, number)
    }

    /// Hand the recovered environment to the program about to be exec'd, so
    /// it sees the session's token, git identity, ssh command and credential
    /// helper rather than the operator's.
    pub(super) fn apply(&self, cmd: &mut Command) {
        for (name, value) in &self.missing {
            cmd.env(name, value);
        }
    }
}

fn recover() -> Vec<(OsString, OsString)> {
    match ancestor_environ() {
        Some(environ) => recover_for(&environ, |name| std::env::var_os(name).is_some()),
        None => Vec::new(),
    }
}

/// The session variables to hand a process whose environment was `environ`,
/// for a process that has the ones `present` reports.
///
/// A process that still carries the marker was not stripped of its session: it
/// is the pane's own shell, or a program of it, and a session variable such a
/// process does not have was dropped deliberately — ssf itself clears
/// `GH_CONFIG_DIR` and `GH_TOKEN` around `gh auth token --user <login>` to read
/// another account's token from gh's own store. Only an environment a tool
/// runner built from scratch is put back, and then only what it lost: a value
/// the process was given itself wins, because the tool that started it decided
/// that value.
fn recover_for(environ: &[u8], present: impl Fn(&OsStr) -> bool) -> Vec<(OsString, OsString)> {
    if present(OsStr::new(MARKER)) {
        return Vec::new();
    }
    missing(parse(environ), present)
}

/// The recovered entries whose name the process already carries, dropped.
fn missing(
    entries: Vec<(OsString, OsString)>,
    present: impl Fn(&OsStr) -> bool,
) -> Vec<(OsString, OsString)> {
    entries
        .into_iter()
        .filter(|(name, _)| !present(name))
        .collect()
}

/// The watched variables out of `NUL`-separated `/proc/<pid>/environ` bytes.
fn parse(environ: &[u8]) -> Vec<(OsString, OsString)> {
    environ
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let at = entry.iter().position(|byte| *byte == b'=')?;
            let (name, value) = entry.split_at(at);
            watched(OsStr::from_bytes(name)).then(|| {
                (
                    OsStr::from_bytes(name).to_os_string(),
                    OsStr::from_bytes(&value[1..]).to_os_string(),
                )
            })
        })
        .collect()
}

/// What a session's environment carries that the shim must not lose: the item
/// and the factory's directories (`SSF_`), the bot's gh identity (`GH_`,
/// `GITHUB_`), and the git identity, signing key, pinned ssh command and
/// credential helper (`GIT_`). Whole families rather than a list of names, so
/// a variable `ssf launch` starts exporting needs no change here: the values
/// are what that process was launched with, including the `SSF_ROLE` and
/// `SSF_SERVER` it removes and so never carries.
fn watched(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name.starts_with("SSF_")
        || name.starts_with("GH_")
        || name.starts_with("GITHUB_")
        || name.starts_with("GIT_")
}

/// `/proc/<pid>/environ` of the nearest ancestor that carries the marker.
/// `None` outside a session, and where the process tree cannot be read.
fn ancestor_environ() -> Option<Vec<u8>> {
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..ANCESTORS {
        if pid <= 1 {
            break;
        }
        if let Ok(environ) = std::fs::read(format!("/proc/{pid}/environ"))
            && carries_marker(&environ)
        {
            return Some(environ);
        }
        pid = parent_of(pid)?;
    }
    None
}

/// Is this the environment of a process in a session?
fn carries_marker(environ: &[u8]) -> bool {
    environ.split(|byte| *byte == 0).any(|entry| {
        entry
            .strip_prefix(MARKER.as_bytes())
            .is_some_and(|rest| rest.starts_with(b"="))
    })
}

/// `/proc/<pid>/stat` field 4: the parent, read after the parenthesized
/// command name, which can itself contain spaces and parentheses.
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parent_of_stat(&stat)
}

/// The parent out of a `/proc/<pid>/stat` line: field 4, the one after the
/// state letter that follows the command name.
fn parent_of_stat(stat: &str) -> Option<u32> {
    stat.rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests;
