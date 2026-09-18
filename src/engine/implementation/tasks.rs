//! `/ssf <request>` in an item's comments: taking the command, running it as
//! a task, and saying on the item how it went.
//!
//! A command is addressed to the factory, not to the item's agent, so it is
//! neither the item's own session's work nor anything a person has to wait
//! for a session to get to: the daemon runs it itself, now, on the
//! repository's harness in its headless form and in the repository's own
//! checkout (see [`crate::task`]). The item's comment stays on GitHub for
//! the session to read like any other, and the session may act on it too;
//! what the request asks for is the task's business either way.
//!
//! Commands are read from the item's timeline, which is fetched only where
//! the daemon already fetches it: on an item ssf polls (assigned, mentioned,
//! review-requested, opened by the bot) and looks at, which is the same
//! footing every other piece of activity is on. Each comment is taken once
//! (`IssueState::slash_done`), a request waits its turn per item
//! (`IssueState::slash_pending`), and one task per item runs at a time.

use super::super::*;
use tracing::{info, warn};

use crate::slash;
use crate::task::{self, Task};

impl Engine {
    /// Take the `/ssf` commands out of an item's timeline: queue the ones
    /// that may be run, and remember every one seen so that a pass over the
    /// same timeline again does not ask twice.
    ///
    /// The bot's own comments are never commands (a session that writes
    /// `/ssf ...` in a post is talking to its readers, and taking it would
    /// let one post start another task and so on), and neither is a comment
    /// by a login that may not drive this repository.
    pub(in crate::engine) fn take_commands(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        timeline: &[Value],
    ) {
        if !self.cfg.slash_commands(repo) {
            return;
        }
        let commands = slash::commands(timeline);
        if commands.is_empty() {
            return;
        }
        let allowed = self.allow_list(repo);
        let bot = self.login.clone();
        let mut queued: Vec<slash::Command> = Vec::new();
        {
            let e = self.entry(repo, number);
            for c in commands {
                // Taken, refused or queued: either way it is dealt with, and
                // the timeline may be walked again any number of times.
                if !e.slash_done.insert(c.id) {
                    continue;
                }
                if c.author.eq_ignore_ascii_case(&bot) {
                    info!(
                        repo = repo.name,
                        issue = number,
                        comment = c.id,
                        "not a command: the bot's own comment"
                    );
                    continue;
                }
                if !allowed.allows(&c.author) {
                    info!(
                        repo = repo.name,
                        issue = number,
                        comment = c.id,
                        author = c.author,
                        "not a command: @{} may not drive this repository",
                        c.author
                    );
                    continue;
                }
                e.slash_pending.push(c.clone());
                queued.push(c);
            }
        }
        for c in queued {
            info!(
                repo = repo.name,
                issue = number,
                comment = c.id,
                asked_by = c.author,
                request = c.text,
                "took a task request from a comment"
            );
        }
    }

    /// Start whatever can be started for one repository: one task per item,
    /// oldest request first, up to [`task::MAX_RUNNING`] across the daemon.
    pub(in crate::engine) async fn run_tasks(&mut self, repo: &RepoConfig) {
        // Switched off, nothing runs: what is already queued waits for the
        // switch to come back, or for the item to be purged.
        if !self.cfg.slash_commands(repo) {
            return;
        }
        let waiting: Vec<(u64, slash::Command)> = self
            .state
            .repos
            .get(&repo.name)
            .map(|rs| {
                rs.issues
                    .values()
                    .filter(|s| !s.slash_pending.is_empty())
                    .map(|s| (s.number, s.slash_pending[0].clone()))
                    .collect()
            })
            .unwrap_or_default();
        for (number, command) in waiting {
            if self.tasks.count() >= task::MAX_RUNNING {
                info!(
                    repo = repo.name,
                    issue = number,
                    running = self.tasks.count(),
                    "tasks are at their limit; this request waits for the next pass"
                );
                break;
            }
            if self.tasks.busy(&repo.name, number) {
                continue;
            }
            if let Err(e) = self.start_task(repo, number, &command).await {
                // The request stays queued: a lookup that failed now (a rate
                // limit, a network hiccup) is worth trying again, and the
                // item's own commands keep their order.
                warn!(
                    repo = repo.name,
                    issue = number,
                    comment = command.id,
                    "could not start the task asked for on the item: {e:#}"
                );
            }
        }
    }

    /// Run one request: the harness in its headless form, given ssf's task
    /// prompt, in the repository's checkout.
    async fn start_task(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        command: &slash::Command,
    ) -> Result<()> {
        let (owner, name) = repo.split()?;
        // What a session on this item would run with: the repository's
        // harness and settings, and the item's own overrides where it has
        // any. Not `repo.command`: that command starts a session.
        let eff = self.effective(repo, number);
        let issue = self.gh.issue(owner, name, number).await?;
        let timeline = self.gh.timeline(owner, name, number).await?;
        // The same material a session's first prompt has: what the request
        // is about, and everything said on the item so far.
        let diff = self.diff(repo, &BTreeMap::new(), &timeline);
        let me = self.acting_on(repo, number);
        let all = self.for_recipient(&diff.rendered, &me, OwnPosts::Shown);
        let st = self.entry(repo, number).clone();
        let ctx = self.ctx(repo, &st);
        let text = prompt::task_prompt(&issue, &all, &ctx, &command.author, &command.text);
        let Some(inner) = crate::models::headless_command(
            &eff.harness,
            eff.model.as_deref(),
            eff.effort.as_deref(),
            &text,
        ) else {
            let why = format!(
                "ssf does not know how to run {} without a terminal; \
                 `ssf assign` the item to another harness instead",
                login::display_name(&eff.harness)
            );
            self.refuse_task(repo, number, command, &eff.harness, &why)
                .await;
            return Ok(());
        };
        // The checkout every workspace of this repository is made from:
        // the task works in it, and is told not to change it.
        let setup = self
            .driver(repo)
            .ensure_project(
                owner,
                name,
                &repo.clone_url(),
                repo.path.as_deref(),
                &self.cfg.projects_dir(self.cfg.driver_for(repo)),
            )
            .await?;
        let cwd = self.driver(repo).repo_path(&setup.repo_id).await?;
        let wrapped = self.launch_command(repo, number, &issue.html_url, &inner);
        let log = task::log_path(&repo.name, number, command.id);
        let started = Task {
            repo: repo.name.clone(),
            number,
            comment: command.id,
            author: command.author.clone(),
            harness: eff.harness.clone(),
            log: log.clone(),
        };
        {
            let e = self.entry(repo, number);
            e.slash_pending.retain(|c| c.id != command.id);
            // Remembered in the state as well as in memory, so a daemon that
            // stops mid-run can say so on the item when it comes back
            // (`Engine::recover_tasks`).
            e.slash_running = Some(slash::Running {
                command: command.clone(),
                harness: eff.harness.clone(),
            });
        }
        if let Err(e) = self.state.save() {
            // Nothing was started, so nothing may look as if it had: the
            // request goes back to the front of the item's queue and the
            // record stops claiming a task is on it.
            let e2 = self.entry(repo, number);
            e2.slash_running = None;
            e2.slash_pending.insert(0, command.clone());
            return Err(e).context("recording the task before starting it");
        }
        match self.tasks.start(
            started,
            Path::new("sh"),
            &["-c".to_string(), wrapped],
            &[],
            Path::new(&cwd),
        ) {
            Ok(()) => {
                info!(
                    repo = repo.name,
                    issue = number,
                    harness = eff.harness,
                    asked_by = command.author,
                    log = %log.display(),
                    "started the task asked for on the item"
                );
                self.post_event(
                    repo,
                    number,
                    Event::TaskStarted {
                        harness: login::display_name(&eff.harness),
                        model: eff.model.clone(),
                        effort: eff.effort.clone(),
                        by: command.author.clone(),
                        request: command.text.clone(),
                    },
                )
                .await;
            }
            Err(e) => {
                let why = format!("could not be started: {e:#}");
                self.entry(repo, number).slash_running = None;
                self.refuse_task(repo, number, command, &eff.harness, &why)
                    .await;
            }
        }
        Ok(())
    }

    /// Drop a request nothing can be done with, and say so on the item
    /// rather than leaving the person who asked with no answer.
    async fn refuse_task(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        command: &slash::Command,
        harness: &str,
        why: &str,
    ) {
        warn!(
            repo = repo.name,
            issue = number,
            comment = command.id,
            "not running the task asked for on the item: {why}"
        );
        self.entry(repo, number)
            .slash_pending
            .retain(|c| c.id != command.id);
        self.post_event(
            repo,
            number,
            Event::TaskRefused {
                harness: login::display_name(harness),
                by: command.author.clone(),
                request: command.text.clone(),
                reason: why.to_string(),
            },
        )
        .await;
    }

    /// Report the tasks that have finished since the last pass.
    pub(in crate::engine) async fn collect_tasks(&mut self) {
        for done in self.tasks.poll().await {
            let ok = done.outcome.ok();
            let Some(repo) = self.repo_of(&done.task.repo) else {
                // The repository was removed while its task ran: the item is
                // nobody's any more, so the run's end is only logged and the
                // record is cleared rather than left claiming one is running.
                warn!(
                    repo = done.task.repo,
                    issue = done.task.number,
                    result = done.outcome.describe(),
                    "task finished for a repository ssf no longer watches"
                );
                self.clear_running(&done.task.repo, done.task.number);
                continue;
            };
            self.entry(&repo, done.task.number).slash_running = None;
            let output = if ok {
                None
            } else {
                task::last_words(&done.task.log)
            };
            info!(
                repo = repo.name,
                issue = done.task.number,
                harness = done.task.harness,
                result = done.outcome.describe(),
                log = %done.task.log.display(),
                "task finished"
            );
            self.post_event(
                &repo,
                done.task.number,
                Event::TaskEnded {
                    harness: login::display_name(&done.task.harness),
                    by: done.task.author.clone(),
                    result: done.outcome.describe(),
                    log: Some(done.task.log.display().to_string()),
                    output,
                },
            )
            .await;
            self.forget_if_idle(&repo.name, done.task.number);
        }
    }

    /// Give up the record of an item that is nobody's and has nothing left to
    /// say: no session of its own, no subscribers (`subscriber_only` or not:
    /// the flag says why the item was tracked, and with nobody left there is
    /// no why), no request waiting, and no allocation waiting to be adopted.
    /// An item the bot opened and nothing acted on is normally kept out of the
    /// records altogether; one whose request ran was kept while it did (an
    /// item with a request waiting on it outlives its last subscriber, see
    /// `Engine::unsubscribe`), and this is where that record goes when the run
    /// is over. What an ignored item is remembered by is its ignore record,
    /// and what an unadopted allocation is remembered by is
    /// `adoption_candidates` (whose items keep a record holding the workspace
    /// a later adoption reuses); neither is touched here.
    pub(in crate::engine) fn forget_if_idle(&mut self, repo: &str, number: u64) {
        let Some(rs) = self.state.repos.get_mut(repo) else {
            return;
        };
        let idle = !rs.adoption_candidates.contains_key(&number)
            && rs.issues.get(&number).is_some_and(|s| {
                !s.seeded
                    && s.subscribers.is_empty()
                    && s.slash_pending.is_empty()
                    && s.slash_running.is_none()
            });
        if idle {
            rs.issues.remove(&number);
            info!(
                repo,
                issue = number,
                "no session and nothing waiting; forgetting it again"
            );
        }
    }

    /// The configuration of a watched repository, by the name its tasks and
    /// records carry.
    fn repo_of(&self, name: &str) -> Option<RepoConfig> {
        self.cfg.repos.iter().find(|r| r.name == name).cloned()
    }

    /// Forget that an item was running a task, for an item whose repository
    /// is no longer watched (so there is nothing to post on).
    fn clear_running(&mut self, repo: &str, number: u64) {
        if let Some(st) = self
            .state
            .repos
            .get_mut(repo)
            .and_then(|rs| rs.issues.get_mut(&number))
        {
            st.slash_running = None;
        }
    }

    /// Say on the item what became of the tasks this daemon was running when
    /// it last stopped. The task itself is gone with the daemon that started
    /// it (its process group was killed), so without this the item would
    /// show a task that started and never ended.
    pub(in crate::engine) async fn recover_tasks(&mut self) {
        let orphaned: Vec<(String, u64, slash::Running)> = self
            .state
            .repos
            .iter()
            .flat_map(|(name, rs)| {
                rs.issues.values().filter_map(|s| {
                    s.slash_running
                        .as_ref()
                        .map(|r| (name.clone(), s.number, r.clone()))
                })
            })
            .collect();
        for (name, number, running) in orphaned {
            let Some(repo) = self.repo_of(&name) else {
                self.clear_running(&name, number);
                continue;
            };
            self.entry(&repo, number).slash_running = None;
            let log = task::log_path(&name, number, running.command.id);
            warn!(
                repo = name,
                issue = number,
                harness = running.harness,
                "the daemon stopped while a task was running"
            );
            self.post_event(
                &repo,
                number,
                Event::TaskEnded {
                    harness: login::display_name(&running.harness),
                    by: running.command.author.clone(),
                    result: "the daemon stopped while it ran".to_string(),
                    log: Some(log.display().to_string()),
                    output: None,
                },
            )
            .await;
        }
    }
}
