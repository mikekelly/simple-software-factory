//! One harness run for one `/ssf` request (see [`crate::slash`]): the
//! repository's harness, in its headless form, in the repository's checkout.
//!
//! A task is not a session. There is no herdr pane, no worktree of its own
//! and no `[ssf]` message channel: it is a child process of the daemon given
//! one prompt, and what it posts on the item it posts itself (it runs with
//! the same credentials, git identity and `gh`/`git` shims as a session, so
//! its posts are the bot's). The daemon says on the item that the task
//! started and how it ended, and keeps the run's output in a log file under
//! the state directory, which is where a person looks when something went
//! wrong.
//!
//! Runs live in memory: the children are killed with the daemon, and the
//! item's record remembers the comment that asked, so a restart neither
//! leaves a task running nor runs the same request twice.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// How long one task may run before it is killed and the item is told. A
/// request that needs longer than this is work for a session, not a task.
pub const TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How many tasks may run at once, across every repository: each one is an
/// unattended agent spending money, and the daemon's poll loop is not a place
/// to discover that a hundred comments arrived.
pub const MAX_RUNNING: usize = 4;

/// How much of a log's end is read when a task fails.
const TAIL_BYTES: u64 = 64 * 1024;

/// What a task is: where it was asked from, which comment asked, and where
/// its output is going.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Repository name (`owner/name`).
    pub repo: String,
    /// The item the command was left on.
    pub number: u64,
    /// GitHub's id of the comment that carried the command.
    pub comment: u64,
    /// The login that asked.
    pub author: String,
    /// The harness that is running, for the posts.
    pub harness: String,
    /// The file the run's output goes to.
    pub log: PathBuf,
}

/// How a task ended.
#[derive(Debug)]
pub enum Outcome {
    /// The harness exited with this status.
    Exited(std::process::ExitStatus),
    /// It was still running at [`TIMEOUT`] and was killed.
    TimedOut,
    /// Its exit status could not be read any more.
    Lost(String),
}

impl Outcome {
    /// How the post on the item words it: the exit code, or what happened
    /// instead.
    pub fn describe(&self) -> String {
        match self {
            Self::Exited(status) => status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "killed by a signal".into()),
            Self::TimedOut => format!("killed after {}", crate::events::held_for(TIMEOUT)),
            Self::Lost(why) => format!("lost ({why})"),
        }
    }

    /// Whether the run can be called a success.
    pub fn ok(&self) -> bool {
        matches!(self, Self::Exited(s) if s.success())
    }
}

/// A task that has been started and not finished.
struct Active {
    task: Task,
    child: tokio::process::Child,
    started: Instant,
}

/// A task whose run is over.
pub struct Finished {
    pub task: Task,
    pub outcome: Outcome,
}

/// The tasks this daemon is running.
#[derive(Default)]
pub struct Tasks {
    running: BTreeMap<(String, u64), Active>,
}

impl Tasks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the item already has a task of its own: one at a time, so two
    /// comments cannot race each other over the same item.
    pub fn busy(&self, repo: &str, number: u64) -> bool {
        self.running.contains_key(&(repo.to_string(), number))
    }

    pub fn count(&self) -> usize {
        self.running.len()
    }

    /// Start `program` as the item's task, with its output in the task's log.
    ///
    /// The child gets a process group of its own: what ssf starts is a shell
    /// that runs the harness, and the harness may fork; killing the group is
    /// what stops all of it (the process is still `kill_on_drop`, for the
    /// paths that never reach [`Tasks::stop_all`]).
    pub fn start(
        &mut self,
        task: Task,
        program: &Path,
        args: &[String],
        env: &[(String, String)],
        cwd: &Path,
    ) -> Result<()> {
        let dir = task.log.parent().context("a task log with no directory")?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let out = std::fs::File::create(&task.log)
            .with_context(|| format!("creating {}", task.log.display()))?;
        let err = out.try_clone().context("duplicating the task's log")?;
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .current_dir(cwd)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            // A task must not outlive the daemon that started it.
            .kill_on_drop(true);
        command.process_group(0);
        let child = command
            .spawn()
            .with_context(|| format!("starting {}", program.display()))?;
        let key = (task.repo.clone(), task.number);
        self.running.insert(
            key,
            Active {
                task,
                child,
                started: Instant::now(),
            },
        );
        Ok(())
    }

    /// The tasks that have finished since the last look, in the order they
    /// were started; one still running past [`TIMEOUT`] is killed first and
    /// reported now.
    pub async fn poll(&mut self) -> Vec<Finished> {
        let mut done = Vec::new();
        let keys: Vec<(String, u64)> = self.running.keys().cloned().collect();
        for key in keys {
            let Some(active) = self.running.get_mut(&key) else {
                continue;
            };
            let outcome = if active.started.elapsed() > TIMEOUT {
                kill_group(&mut active.child).await;
                Some(match active.child.wait().await {
                    Ok(_) => Outcome::TimedOut,
                    Err(e) => Outcome::Lost(e.to_string()),
                })
            } else {
                match active.child.try_wait() {
                    Ok(Some(status)) => Some(Outcome::Exited(status)),
                    Ok(None) => None,
                    Err(e) => Some(Outcome::Lost(e.to_string())),
                }
            };
            if let Some(outcome) = outcome
                && let Some(active) = self.running.remove(&key)
            {
                done.push(Finished {
                    task: active.task,
                    outcome,
                });
            }
        }
        done
    }

    /// Kill everything running: the daemon is going away, and a task does not
    /// outlive it.
    pub fn stop_all(&mut self) {
        for (_, mut active) in std::mem::take(&mut self.running) {
            // Shutdown cannot wait: signal the group and let the child be
            // reaped by the drop that follows.
            if let Some(pid) = active.child.id() {
                signal_group(pid, libc::SIGKILL);
            }
            let _ = active.child.start_kill();
        }
    }
}

/// Kill a task's whole process group, then the child itself.
async fn kill_group(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        signal_group(pid, libc::SIGKILL);
    }
    let _ = child.start_kill();
}

/// Signal the process group `pid` leads. A child that has already exited,
/// or that never got its own group, is not an error: `kill` on a group with
/// no members is how this ends.
fn signal_group(pid: u32, signal: libc::c_int) {
    // A process group id is the leader's pid, and a negative pid addresses
    // the group. `pid` came from `Child::id`, so it is positive and the
    // negation cannot be confused with the signal's own pid arguments.
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    unsafe {
        libc::kill(-pid, signal);
    }
}

/// Where a task's output goes. Scoped by repository and item like a delivery
/// mailbox, so a person looking at a failure finds the run's own words by
/// the item they were about.
pub fn log_path(repo: &str, number: u64, comment: u64) -> PathBuf {
    let mut path = crate::config::state_dir().join("tasks");
    for component in repo.split('/') {
        path.push(match component {
            "" => "_empty_",
            "." => "_dot_",
            ".." => "_dotdot_",
            other => other,
        });
    }
    path.join(number.to_string()).join(format!("{comment}.log"))
}

/// The last line a task's log has to offer: the most recent thing it said,
/// for the post about a run that failed (a whole tail would not fit the
/// daemon's block; the file itself is named beside it).
pub fn last_words(path: &Path) -> Option<String> {
    let tail = log_tail(path, TAIL_BYTES as usize)?;
    tail.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(str::to_string)
}

/// The end of a task's log, for the post about a run that failed: whole
/// lines at the end of the file, at most `max` characters, with a marker
/// saying where the file itself is.
pub fn log_tail(path: &Path, max: usize) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let from = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let text = text.trim_end();
    if text.trim().is_empty() {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let tail: String = chars[chars.len().saturating_sub(max)..].iter().collect();
    // Whichever line the cut landed in is not worth keeping.
    let tail = if chars.len() > max {
        match tail.find('\n') {
            Some(i) => tail[i + 1..].to_string(),
            None => tail,
        }
    } else {
        tail
    };
    let tail = tail.trim();
    (!tail.is_empty()).then(|| tail.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn sandbox_task(name: &str) -> Task {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = crate::config::test_support::sandbox().state_dir();
        let log = dir.join(format!("tasks/{name}-{n}.log"));
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        Task {
            repo: "owner/repo".into(),
            number: 42,
            comment: 7,
            author: "ann".into(),
            harness: "claude".into(),
            log,
        }
    }

    async fn run(script: &str) -> (Task, Outcome, String) {
        let task = sandbox_task("run");
        let mut tasks = Tasks::new();
        tasks
            .start(
                task.clone(),
                Path::new("/bin/sh"),
                &["-c".into(), script.into()],
                &[],
                Path::new("/tmp"),
            )
            .unwrap();
        let done = loop {
            let done = tasks.poll().await;
            if let Some(done) = done.into_iter().next() {
                break done;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let log = std::fs::read_to_string(&task.log).unwrap();
        (task, done.outcome, log)
    }

    #[tokio::test]
    async fn a_task_runs_and_its_output_is_kept() {
        let (task, outcome, log) = run("echo hello; echo oops >&2").await;
        assert!(outcome.ok(), "{}", outcome.describe());
        assert_eq!(outcome.describe(), "0");
        assert!(log.contains("hello") && log.contains("oops"), "{log}");
        assert!(log_tail(&task.log, 4000).is_some());
    }

    #[tokio::test]
    async fn a_failed_task_reports_its_exit_and_its_commands_do_not_leak() {
        let (_, outcome, log) = run("echo before; printf 'no trailing newline'; exit 3").await;
        assert!(!outcome.ok());
        assert_eq!(outcome.describe(), "3");
        let tail = log_tail(Path::new("/nonexistent"), 4000);
        assert!(tail.is_none(), "an unreadable log has no tail");
        assert!(log.contains("before"));
    }

    #[tokio::test]
    async fn one_task_at_a_time_per_item() {
        let task = sandbox_task("busy");
        let mut tasks = Tasks::new();
        tasks
            .start(
                task.clone(),
                Path::new("/bin/sh"),
                &["-c".into(), "sleep 30".into()],
                &[],
                Path::new("/tmp"),
            )
            .unwrap();
        assert!(tasks.busy("owner/repo", 42));
        assert!(!tasks.busy("owner/repo", 43));
        assert_eq!(tasks.count(), 1);
        tasks.stop_all();
        assert_eq!(tasks.count(), 0);
    }

    #[test]
    fn the_tail_keeps_whole_lines_from_the_end() {
        let task = sandbox_task("tail");
        std::fs::write(&task.log, "one\ntwo\nthree\n").unwrap();
        assert_eq!(log_tail(&task.log, 4000).unwrap(), "one\ntwo\nthree");
        let tail = log_tail(&task.log, 11).unwrap();
        assert!(tail.ends_with("three"), "{tail}");
        assert!(!tail.starts_with("one"), "{tail}");
        std::fs::write(&task.log, "\n\n").unwrap();
        assert_eq!(log_tail(&task.log, 4000), None);
    }
}
