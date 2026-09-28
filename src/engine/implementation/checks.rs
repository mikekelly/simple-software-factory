//! CI on the pull requests sessions own (#641): one event when a head
//! commit's checks start, and one when they settle, delivered like any
//! other activity on the item.

use super::super::*;
use super::reconciliation::rate_limited;
use tracing::{debug, info, warn};

impl Engine {
    /// Read CI on every open pull request an active session owns, and tell
    /// the session what is new. Each read is conditional, so a quiet pull
    /// request costs four 304s. A rate limit stops the pass (it pauses
    /// every repository); anything else is logged and tried next pass.
    pub(in crate::engine) async fn watch_checks(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
    ) -> Result<()> {
        if self.cfg.daemon.ignored_events.iter().any(|k| k == "ci") {
            return Ok(());
        }
        let lost = self.channel_lost.clone();
        let prs: Vec<u64> = self
            .state
            .repo_mut(&repo.name)
            .issues
            .values()
            .filter(|s| {
                s.seeded
                    && s.active
                    && s.pr.is_some()
                    && s.blocked.is_none()
                    && s.handover.is_none()
                    && s.retirement_held_at.is_none()
                    && !matches!(s.github_state.as_deref(), Some("closed" | "merged"))
                    && !lost.contains(&(repo.name.clone(), s.number))
            })
            .map(|s| s.number)
            .collect();
        for number in prs {
            match self.check_ci(repo, owner, name, number).await {
                Ok(false) => {}
                Ok(true) => self.persist()?,
                Err(e) if rate_limited(&e).is_some() => return Err(e),
                Err(e) if is_held(&e) => {
                    debug!(repo = repo.name, issue = number, "CI news held: {e:#}")
                }
                Err(e) => warn!(
                    repo = repo.name,
                    issue = number,
                    "checking CI failed: {e:#}"
                ),
            }
        }
        Ok(())
    }

    /// One pull request's CI: whether something was delivered.
    async fn check_ci(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        number: u64,
    ) -> Result<bool> {
        let key = (repo.name.clone(), number);
        let mut poll = self.ci_polls.get(&key).cloned().unwrap_or_default();
        if let Conditional::Modified { value: sha, etag } = self
            .gh
            .pull_head(owner, name, number, poll.pull_etag.as_deref())
            .await?
        {
            if sha != poll.sha {
                poll = CiPoll {
                    sha,
                    ..Default::default()
                };
            }
            poll.pull_etag = etag;
        }
        if poll.sha.is_empty() {
            return Ok(false);
        }
        // Stored as each read lands, so a failed second read does not
        // cost the first one again.
        self.ci_polls.insert(key.clone(), poll.clone());
        if let Conditional::Modified { value, etag } = self
            .gh
            .commit_ci(
                owner,
                name,
                &poll.sha,
                "check-runs",
                poll.runs_etag.as_deref(),
            )
            .await?
        {
            poll.runs = Some(value);
            poll.runs_etag = etag;
            self.ci_polls.insert(key.clone(), poll.clone());
        }
        if let Conditional::Modified { value, etag } = self
            .gh
            .commit_ci(
                owner,
                name,
                &poll.sha,
                "status",
                poll.status_etag.as_deref(),
            )
            .await?
        {
            poll.status = Some(value);
            poll.status_etag = etag;
            self.ci_polls.insert(key.clone(), poll.clone());
        }
        if let Conditional::Modified { value, etag } = self
            .gh
            .commit_ci(
                owner,
                name,
                &poll.sha,
                "check-suites",
                poll.suites_etag.as_deref(),
            )
            .await?
        {
            poll.suites = Some(value);
            poll.suites_etag = etag;
            self.ci_polls.insert(key, poll.clone());
        }
        let checks = crate::github::ci_checks(poll.runs.as_ref(), poll.status.as_ref());
        let suites_running = crate::github::ci_suites_running(poll.suites.as_ref());
        let Some(event) = prompt::render_ci(&poll.sha, &checks, suites_running) else {
            return Ok(false);
        };
        let outcome = event
            .key
            .strip_prefix(&format!("ci:{}:", poll.sha))
            .unwrap_or_default()
            .to_string();
        let told = self
            .peek(repo, number)
            .and_then(|s| s.ci_notice.clone())
            .filter(|n| n.sha == poll.sha)
            .unwrap_or_else(|| CiNotice {
                sha: poll.sha.clone(),
                ..Default::default()
            });
        let mut next = told.clone();
        if outcome == prompt::CI_STARTED {
            // Once per commit: a re-run is not CI starting again.
            if told.started || told.result.is_some() {
                return Ok(false);
            }
            next.started = true;
        } else {
            if told.result.as_deref() == Some(outcome.as_str()) {
                return Ok(false);
            }
            next.started = true;
            next.result = Some(outcome);
        }
        let issue = self.gh.issue(owner, name, number).await?;
        let st = self.entry(repo, number).clone();
        let ctx = self.ctx(repo, &st);
        let text = prompt::followup_prompt(&issue, std::slice::from_ref(&event), &ctx);
        info!(
            repo = repo.name,
            issue = number,
            sha = poll.sha,
            event = event.key,
            "delivering CI news"
        );
        let d = self.deliver_to(repo, number, &text, None).await?;
        let e = self.entry(repo, number);
        e.terminal_handle = Some(d.handle);
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        e.ci_notice = Some(next);
        Ok(true)
    }
}
