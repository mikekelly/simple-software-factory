use super::super::*;
use tracing::{debug, error, info, warn};

impl Engine {
    pub async fn tick(&mut self) {
        self.reload_config();
        self.state.last_poll_at = Some(now_iso());
        // Per-pass state only. `refetch` is deliberately not reset here: it
        // has to outlive the pass that armed it (issue #141).
        self.probes.clear();
        if self.cfg.repos.is_empty() {
            warn!("no repos configured; nothing to do (see `ssf repo add`)");
            return;
        }
        let down = match self.check_drivers().await {
            Ok(down) => down,
            Err(e) => {
                warn!("skipping this pass: {e:#}");
                self.state.last_error = Some(format!("{e:#}"));
                let _ = self.state.save();
                return;
            }
        };
        // A driver that is down is a visible error even while the others
        // carry on; its repositories are skipped below.
        self.state.last_error = if down.is_empty() {
            None
        } else {
            Some(down.join("; "))
        };
        let ready: Vec<DriverKind> = self
            .startup_pending
            .iter()
            .copied()
            .filter(|k| !self.down.contains(k))
            .collect();
        if !ready.is_empty() {
            self.startup_pending.retain(|k| !ready.contains(k));
            self.resume_interrupted(&ready).await;
        }
        for repo in self.cfg.repos.clone() {
            if self.driver_down(&repo) {
                continue;
            }
            // Before anything is delivered or resumed: a session that has
            // been handed over is replaced first.
            self.run_handovers(&repo).await;
            if let Err(e) = self.tick_repo(&repo).await {
                warn!(repo = repo.name, "pass failed: {e:#}");
                self.state.last_error = Some(format!("{}: {e:#}", repo.name));
            }
            if let Err(e) = self.check_conflicts(&repo).await {
                warn!(repo = repo.name, "branch conflict check failed: {e:#}");
                self.state.last_error = Some(format!("{}: {e:#}", repo.name));
            }
            self.capture_sessions(&repo);
            self.run_cleanups(&repo).await;
            if let Err(e) = self.state.save() {
                error!("saving state: {e:#}");
            }
        }
    }

    /// The startup pass. A daemon restart is invisible to agents (Orca keeps
    /// their terminals), but after a machine restart every session's
    /// terminal is gone, and nothing would bring one back until the next
    /// GitHub event for its item. So, once, when Orca first answers: every
    /// active session that owns its workspace is looked at, and one whose
    /// workspace still exists but has no live agent is started again through
    /// the normal delivery path (resuming its conversation when a session
    /// id was captured, fresh with the item's story otherwise) with one
    /// message saying it was interrupted. One at a time, each waiting for
    /// its harness to settle. Live sessions are not touched, and a missing
    /// workspace is left to rehydration on the next event rather than
    /// re-created on boot.
    pub(in crate::engine) async fn resume_interrupted(&mut self, kinds: &[DriverKind]) {
        self.startup_pass = true;
        for repo in self.cfg.repos.clone() {
            if self.driver_down(&repo) || !kinds.contains(&self.cfg.driver_for(&repo)) {
                continue;
            }
            // A session started fresh gets its item's story, which is
            // filtered by the allow-list: the collaborators have to be
            // known first, or every human post would be left out of it.
            let refreshed = match repo.split() {
                Ok((owner, name)) => self.refresh_collaborators(&repo, owner, name).await,
                Err(e) => Err(e),
            };
            if let Err(e) = refreshed {
                warn!(
                    repo = repo.name,
                    "skipping the startup pass for this repository: {e:#}"
                );
                continue;
            }
            let candidates = self.resume_candidates(&repo);
            for number in candidates {
                let st = self.entry(&repo, number).clone();
                let Some(worktree_id) = st.worktree_id.clone() else {
                    continue;
                };
                let session = session_id(&repo.name, number);
                match self.driver(&repo).worktree_exists(&worktree_id).await {
                    Ok(true) => {}
                    Ok(false) => {
                        debug!(session, "workspace is gone; left to rehydration");
                        continue;
                    }
                    Err(e) => {
                        warn!(session, "could not check the workspace: {e:#}");
                        continue;
                    }
                }
                match self.driver(&repo).has_live_agent(&worktree_id).await {
                    Ok(true) => {
                        debug!(session, "agent is live; nothing to do");
                        continue;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        warn!(session, "could not list the workspace's terminals: {e:#}");
                        continue;
                    }
                }
                let text = prompt::interrupted_prompt(&prompt::Interrupted {
                    number: st.number,
                    title: &st.title,
                    url: &st.html_url,
                    branch: st.branch.as_deref(),
                    path: st.worktree_path.as_deref(),
                });
                info!(session, "session was interrupted; starting it again");
                match self.deliver_to(&repo, number, &text, None).await {
                    Ok(d) => {
                        let e = self.entry(&repo, number);
                        e.terminal_handle = Some(d.handle);
                        e.last_prompt_at = Some(now_iso());
                        e.prompts_sent += 1;
                    }
                    Err(e) => warn!(session, "could not start the session again: {e:#}"),
                }
                if let Err(e) = self.state.save() {
                    error!("saving state: {e:#}");
                }
            }
        }
        self.startup_pass = false;
    }

    /// Sessions the startup pass looks at: the session that acts on every
    /// active, seeded item (the item's own, or its owner's, which may itself
    /// be retired while it still owns open items and so keeps its
    /// workspace). A session whose workspace is gone, released or about to
    /// be removed is skipped.
    pub(in crate::engine) fn resume_candidates(&self, repo: &RepoConfig) -> Vec<u64> {
        let Some(rs) = self.state.repos.get(&repo.name) else {
            return Vec::new();
        };
        // A session with a handover pending is left alone: the pass ends
        // it and starts the new one, and bringing the old harness back
        // only to stop it would waste a launch (and a login check).
        let has_workspace = |s: &IssueState| {
            !s.cleanup_pending
                && !s.release_pending
                && s.handover.is_none()
                && s.worktree_id.is_some()
        };
        let owners: BTreeSet<u64> = rs
            .issues
            .values()
            .filter(|s| s.seeded && s.active)
            .map(|s| owner_in(&rs.issues, s.number))
            .collect();
        owners
            .into_iter()
            .filter(|n| rs.issues.get(n).is_some_and(&has_workspace))
            .collect()
    }

    pub(in crate::engine) async fn tick_repo(&mut self, repo: &RepoConfig) -> Result<()> {
        let (owner, name) = repo.split()?;
        self.refresh_collaborators(repo, owner, name).await?;
        // Sessions whose harness sits at a login prompt are found (and
        // brought back) before anything is delivered this pass.
        self.check_logins(repo).await;
        if self.refetch.remove(&repo.name) {
            self.clear_etags(repo);
        }
        let rs = self.state.repo_mut(&repo.name).clone();

        // Four listings, one per trigger. Each carries its own ETag; a 304
        // means that listing (and every item on it) is exactly as last time,
        // so its cached numbers stand in for the contents. The fourth is the
        // bot's own open items: a session hears about what it opened
        // without anyone having to assign or mention it.
        let assigned = self
            .gh
            .items(
                owner,
                name,
                "assignee",
                &self.login,
                rs.issues_etag.as_deref(),
            )
            .await?;
        let mentioned = self
            .gh
            .items(
                owner,
                name,
                "mentioned",
                &self.login,
                rs.mentioned_etag.as_deref(),
            )
            .await?;
        let reviews = self
            .gh
            .review_requested(owner, name, &self.login, rs.pulls_etag.as_deref())
            .await?;
        let created = self
            .gh
            .items(
                owner,
                name,
                "creator",
                &self.login,
                rs.created_etag.as_deref(),
            )
            .await?;
        if matches!(assigned, Conditional::NotModified)
            && matches!(mentioned, Conditional::NotModified)
            && matches!(reviews, Conditional::NotModified)
            && matches!(created, Conditional::NotModified)
        {
            debug!(repo = repo.name, "nothing changed");
            return self.watch_subscribed(repo, owner, name).await;
        }

        // number -> (fresh item if we have one, pr info, triggers)
        let mut items: BTreeMap<u64, (Option<Issue>, Option<PrInfo>, Vec<String>)> =
            BTreeMap::new();
        let mut note = |n: u64, issue: Option<Issue>, pr: Option<PrInfo>, trigger: &str| {
            let e = items.entry(n).or_insert((None, None, Vec::new()));
            if issue.is_some() {
                e.0 = issue;
            }
            if pr.is_some() {
                e.1 = pr;
            }
            if !e.2.iter().any(|t| t == trigger) {
                e.2.push(trigger.to_string());
            }
        };
        let (assigned_numbers, issues_etag) = match assigned {
            Conditional::Modified { value, etag } => {
                let nums: Vec<u64> = value.iter().map(|i| i.number).collect();
                for i in value {
                    note(i.number, Some(i), None, "assigned");
                }
                (nums, Some(etag))
            }
            Conditional::NotModified => {
                for n in &rs.assigned_numbers {
                    note(*n, None, None, "assigned");
                }
                (rs.assigned_numbers.clone(), None)
            }
        };
        let (mentioned_numbers, mentioned_etag) = match mentioned {
            Conditional::Modified { value, etag } => {
                let nums: Vec<u64> = value.iter().map(|i| i.number).collect();
                for i in value {
                    note(i.number, Some(i), None, "mentioned");
                }
                (nums, Some(etag))
            }
            Conditional::NotModified => {
                for n in &rs.mentioned_numbers {
                    note(*n, None, None, "mentioned");
                }
                (rs.mentioned_numbers.clone(), None)
            }
        };
        let (review_numbers, pulls_etag) = match reviews {
            Conditional::Modified { value, etag } => {
                let nums: Vec<u64> = value.iter().map(|(i, _)| i.number).collect();
                for (i, pr) in value {
                    note(i.number, Some(i), Some(pr), "review_requested");
                }
                (nums, Some(etag))
            }
            Conditional::NotModified => {
                for n in &rs.review_numbers {
                    note(*n, None, None, "review_requested");
                }
                (rs.review_numbers.clone(), None)
            }
        };
        let (created_numbers, created_etag) = match created {
            Conditional::Modified { value, etag } => {
                let nums: Vec<u64> = value.iter().map(|i| i.number).collect();
                for i in value {
                    note(i.number, Some(i), None, "created");
                }
                (nums, Some(etag))
            }
            Conditional::NotModified => {
                for n in &rs.created_numbers {
                    note(*n, None, None, "created");
                }
                (rs.created_numbers.clone(), None)
            }
        };
        debug!(
            repo = repo.name,
            count = items.len(),
            "open items involving the bot"
        );

        let mut all_ok = true;
        let present: BTreeSet<u64> = items.keys().copied().collect();
        // An item on a listing again settles whatever a hiccup held, even
        // when nothing else about it needs looking at this pass, so a
        // later hold is a new incident and starts its count again.
        for n in &present {
            if self
                .state
                .repos
                .get(&repo.name)
                .and_then(|r| r.issues.get(n))
                .is_some_and(|s| s.retirement_held_at.is_some())
            {
                self.clear_hold(repo, *n);
            }
        }
        for (number, (fresh, pr, triggers)) in items {
            if !self.needs_look(repo, number, fresh.as_ref(), &triggers) {
                continue;
            }
            let issue = match fresh {
                Some(i) => i,
                // Not on a listing that changed, and not handled by a
                // session (`needs_look`): fetch it to see what it is now.
                None => match self.gh.issue(owner, name, number).await {
                    Ok(i) => i,
                    Err(e) => {
                        all_ok = false;
                        self.note_failure(repo, number, &e).await;
                        continue;
                    }
                },
            };
            match self
                .reconcile_issue(repo, owner, name, &issue, pr, triggers)
                .await
            {
                Ok(()) => {
                    self.failures.remove(&(repo.name.clone(), issue.number));
                }
                // Held, not failed: the item is looked at again when its
                // listing changes, and in full once the session is back.
                Err(e) if is_blocked(&e) => {
                    debug!(repo = repo.name, issue = issue.number, "held: {e:#}");
                }
                Err(e) => {
                    all_ok = false;
                    self.note_failure(repo, issue.number, &e).await;
                }
            }
        }

        let stale: Vec<u64> = self
            .state
            .repo_mut(&repo.name)
            .issues
            .values()
            .filter(|s| s.active && !present.contains(&s.number))
            .map(|s| s.number)
            .collect();
        for number in stale {
            // A blocked session cannot be told its item closed; the item
            // stays active in the record until the session is back.
            let acting = self.owner_of(repo, number);
            if self.peek(repo, acting).is_some_and(|s| s.blocked.is_some()) {
                debug!(
                    repo = repo.name,
                    issue = number,
                    "retirement held: the session is blocked"
                );
                continue;
            }
            match self.retire_issue(repo, owner, name, number).await {
                Ok(()) => {}
                Err(e) if is_blocked(&e) => {
                    debug!(repo = repo.name, issue = number, "retirement held: {e:#}");
                }
                Err(e) => {
                    all_ok = false;
                    warn!(repo = repo.name, issue = number, "retiring failed: {e:#}");
                }
            }
        }

        self.prune_ignored(repo, owner, name, &present).await;

        // Only trust the ETags when every item was handled; otherwise the next
        // pass must see the full listings again to retry.
        let rs = self.state.repo_mut(&repo.name);
        if all_ok {
            if let Some(t) = issues_etag {
                rs.issues_etag = t;
                rs.assigned_numbers = assigned_numbers;
            }
            if let Some(t) = mentioned_etag {
                rs.mentioned_etag = t;
                rs.mentioned_numbers = mentioned_numbers;
            }
            if let Some(t) = pulls_etag {
                rs.pulls_etag = t;
                rs.review_numbers = review_numbers;
            }
            if let Some(t) = created_etag {
                rs.created_etag = t;
                rs.created_numbers = created_numbers;
            }
        } else {
            rs.issues_etag = None;
            rs.mentioned_etag = None;
            rs.pulls_etag = None;
            rs.created_etag = None;
        }
        self.watch_subscribed(repo, owner, name).await
    }

    /// Whether a pass has to look at an item found on the listings: `fresh`
    /// is the item as a changed listing reported it, `None` when every
    /// listing carrying it was a 304, and `triggers` names the listings it
    /// is on. An item a session handles is looked at when a listing
    /// carrying it changed. One nothing handles is looked at unless it was
    /// ignored and is still as it was then (see [`Ignored`]): the same
    /// `updated_at` on the same listings. An item that was ignored as
    /// created-only and now shows up as assigned (or mentioned, or with a
    /// review requested) is looked at again, and onboarded as usual, even
    /// though GitHub's `updated_at` has not moved.
    pub(in crate::engine) fn needs_look(
        &self,
        repo: &RepoConfig,
        number: u64,
        fresh: Option<&Issue>,
        triggers: &[String],
    ) -> bool {
        let tracked = self
            .state
            .repos
            .get(&repo.name)
            .and_then(|rs| rs.issues.get(&number))
            .is_some_and(|s| s.seeded && s.active);
        if tracked {
            return fresh.is_some();
        }
        self.state
            .repos
            .get(&repo.name)
            .and_then(|rs| rs.ignored.get(&number))
            .is_none_or(|at| !at.stands(fresh, triggers))
    }

    /// Forget the ignore records that have nothing left to guard.
    ///
    /// A record only does anything while its item is on a listing, so an
    /// item missing from this pass's listings is no reason to drop one:
    /// GitHub's filtered listings come back short now and then, and
    /// dropping a record on absence alone means every item missing from
    /// one short listing is onboarded again when the listing recovers
    /// (issue #138: 21 of them, on and off for hours). `retire_issue`
    /// has guarded sessions against the same lag from the start.
    ///
    /// So a missing item is looked at instead: its record goes if the
    /// item is closed, or gone from GitHub altogether, and stands
    /// otherwise. A record whose item stays off every listing for
    /// [`ABSENT_GIVE_UP`] is given up then, without a request: that is no
    /// longer a listing that lagged, and a record nothing can consult is
    /// not worth keeping for ever. Both clocks are in the record itself,
    /// so a daemon restart does not set them back.
    ///
    /// The looks are rationed, the giving up is not: a record is looked at
    /// at most once per [`ABSENT_RECHECK`], longest-unlooked-at first and
    /// [`ABSENT_LOOKS_PER_PASS`] to a pass, so neither a listing that
    /// flaps nor one that drops a hundred items at once spends a pass or
    /// the rate limit, and every record's turn comes round.
    pub(in crate::engine) async fn prune_ignored(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        present: &BTreeSet<u64>,
    ) {
        let mut absent: Vec<(u64, Ignored)> = {
            let Some(rs) = self.state.repos.get_mut(&repo.name) else {
                return;
            };
            // Back on a listing: the absence is over. What was asked about
            // it stands, so a flapping listing does not buy a look every
            // time the item comes back.
            for (_, at) in rs.ignored.iter_mut().filter(|(n, _)| present.contains(n)) {
                at.absent_since = None;
            }
            rs.ignored
                .iter_mut()
                .filter(|(n, _)| !present.contains(n))
                .map(|(n, at)| {
                    // A stamp that cannot be read (a hand-edited state
                    // file, a clock that went backwards) is replaced
                    // rather than believed: an unreadable one would
                    // otherwise read as "just now" for ever, and freeze
                    // both the look and the giving up.
                    if at
                        .absent_since
                        .as_deref()
                        .is_some_and(|t| since(t).is_none())
                    {
                        at.absent_since = Some(now_iso());
                    }
                    if at.asked_at.as_deref().is_some_and(|t| since(t).is_none()) {
                        at.asked_at = None;
                    }
                    at.absent_since.get_or_insert_with(now_iso);
                    (*n, at.clone())
                })
                .collect()
        };

        // Giving up costs nothing, so it is not rationed.
        absent.retain(|(number, at)| {
            let absent_for = at
                .absent_since
                .as_deref()
                .and_then(since)
                .unwrap_or_default();
            if absent_for < ABSENT_GIVE_UP {
                return true;
            }
            info!(
                repo = repo.name,
                issue = number,
                hours = ABSENT_GIVE_UP.as_secs() / 3600,
                "on no listing for hours; giving up its ignore record"
            );
            self.state.repo_mut(&repo.name).ignored.remove(number);
            false
        });

        // The one asked about longest ago goes first, and one never asked
        // about before that, so a repository with more absent records than
        // a pass looks at works through them rather than round the first.
        absent.retain(|(_, at)| {
            at.asked_at
                .as_deref()
                .and_then(since)
                .is_none_or(|d| d >= ABSENT_RECHECK)
        });
        absent.sort_by(|a, b| a.1.asked_at.cmp(&b.1.asked_at));
        if absent.len() > ABSENT_LOOKS_PER_PASS {
            debug!(
                repo = repo.name,
                absent = absent.len(),
                "more absent ignore records than one pass looks at; the rest wait"
            );
            absent.truncate(ABSENT_LOOKS_PER_PASS);
        }
        for (number, _) in absent {
            let gone = match self.gh.issue_opt(owner, name, number).await {
                // Closed, or gone from GitHub altogether (deleted, or not
                // readable with this token any more). An item transferred
                // to another repository answers for its new home, where it
                // is open, so that record waits for the giving up.
                Ok(item) => item.is_none_or(|i| i.state == "closed"),
                // Nothing was answered: keep the record and ask again when
                // the next look is due rather than on every pass for as
                // long as the failure lasts.
                Err(e) => {
                    debug!(
                        repo = repo.name,
                        issue = number,
                        "keeping the ignore record of an item that could not be fetched: {e:#}"
                    );
                    false
                }
            };
            let rs = self.state.repo_mut(&repo.name);
            if gone {
                rs.ignored.remove(&number);
            } else if let Some(at) = rs.ignored.get_mut(&number) {
                at.asked_at = Some(now_iso());
            }
        }
    }
}
