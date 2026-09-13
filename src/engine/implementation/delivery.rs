use super::super::*;
use tracing::{debug, error, info, warn};

impl Engine {
    pub(in crate::engine) async fn watch_subscribed(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
    ) -> Result<()> {
        let watched: Vec<IssueState> = self
            .state
            .repo_mut(&repo.name)
            .issues
            .values()
            .filter(|s| {
                s.subscriber_only
                    && !s.active
                    && !s.subscribers.is_empty()
                    && !matches!(s.github_state.as_deref(), Some("closed" | "merged"))
            })
            .cloned()
            .collect();
        let mut failed = false;
        for st in watched {
            let number = st.number;
            let issue = match self.gh.issue(owner, name, number).await {
                Ok(i) => i,
                Err(e) => {
                    failed = true;
                    warn!(
                        repo = repo.name,
                        issue = number,
                        "polling subscribed item failed: {e:#}"
                    );
                    continue;
                }
            };
            if st.updated_at.as_deref() == Some(issue.updated_at.as_str()) {
                continue;
            }
            let timeline = match self.gh.timeline(owner, name, number).await {
                Ok(t) => t,
                Err(e) => {
                    failed = true;
                    warn!(
                        repo = repo.name,
                        issue = number,
                        "polling subscribed item failed: {e:#}"
                    );
                    continue;
                }
            };
            self.record_origins(repo, &issue, &timeline);
            let diff = self.diff(repo, &st.seen, &timeline);
            let closed = issue.state == "closed";
            let merged = closed
                && issue.is_pull_request()
                && self
                    .gh
                    .pull(owner, name, number)
                    .await
                    .map(|p| p.merged)
                    .unwrap_or(false);
            info!(
                repo = repo.name,
                issue = number,
                events = diff.rendered.len(),
                closed,
                "subscribed item changed"
            );
            let what = if closed { Fyi::Closed } else { Fyi::Activity };
            self.fan_out(repo, &issue, &diff.rendered, what, merged, &[])
                .await;
            let e = self.entry(repo, number);
            e.updated_at = Some(issue.updated_at.clone());
            e.seen = diff.seen;
            e.title = issue.title.clone();
            e.github_state = Some(github_state(&issue, None, merged));
            if closed {
                // The subscribers have had the last word on it. An item that
                // never had a session is forgotten; one that did keeps its
                // workspace record for the cleanup.
                if st.seeded {
                    let e = self.entry(repo, number);
                    e.subscriber_only = false;
                    e.subscribers.clear();
                } else {
                    self.state.repo_mut(&repo.name).issues.remove(&number);
                }
            }
        }
        if failed {
            anyhow::bail!("polling a subscribed item failed");
        }
        Ok(())
    }

    /// The session (`owner/repo#N`) that acts on the item a post's origin
    /// tag names: the item's owner, in a watched repository; the tag itself
    /// anywhere else.
    pub(in crate::engine) fn acting_session(&self, origin: &str) -> String {
        let Some(o) = Origin::parse(origin) else {
            return origin.to_string();
        };
        match self.cfg.repos.iter().find(|r| r.matches_name(&o.repo)) {
            Some(r) => session_id(&r.name, self.owner_of(r, o.number)),
            None => origin.to_string(),
        }
    }

    /// `events` as `recipient` (a session id) should see them: without its
    /// own posts, which would only echo its work back at it. Other sessions'
    /// posts stay, labelled with where they came from by the renderer.
    pub(in crate::engine) fn for_recipient(
        &self,
        events: &[Rendered],
        recipient: &str,
    ) -> Vec<Rendered> {
        if self.cfg.daemon.include_own_events {
            return events.to_vec();
        }
        events
            .iter()
            .filter(|e| {
                e.origin
                    .as_deref()
                    .is_none_or(|o| !self.acting_session(o).eq_ignore_ascii_case(recipient))
            })
            .cloned()
            .collect()
    }

    /// The session id of whoever acts on `number`.
    pub(in crate::engine) fn acting_on(&self, repo: &RepoConfig, number: u64) -> String {
        session_id(&repo.name, self.owner_of(repo, number))
    }

    /// Tell every subscriber of an item what happened on it, each without
    /// its own posts. Best effort: a subscriber that cannot be reached is
    /// logged and skipped, never retried, and a retired subscriber whose
    /// workspace is gone is not rebuilt for an FYI. Sessions in `skip` are
    /// left out (a delegating parent that gets a fuller message instead).
    pub(in crate::engine) async fn fan_out(
        &mut self,
        repo: &RepoConfig,
        issue: &Issue,
        events: &[Rendered],
        what: Fyi,
        merged: bool,
        skip: &[String],
    ) {
        let st = self.entry(repo, issue.number).clone();
        if st.subscribers.is_empty() {
            return;
        }
        let owner_session = if st.subscriber_only || !st.seeded {
            None
        } else {
            Some(self.acting_on(repo, issue.number))
        };
        for sub in st.subscribers.clone() {
            if owner_session
                .as_deref()
                .is_some_and(|o| o.eq_ignore_ascii_case(&sub))
                || skip.iter().any(|s| s.eq_ignore_ascii_case(&sub))
            {
                continue;
            }
            let Ok((srepo, snumber, sid)) = self.known_session(&sub) else {
                warn!(
                    repo = repo.name,
                    issue = issue.number,
                    subscriber = sub,
                    "subscriber is not a session ssf knows; skipping"
                );
                continue;
            };
            let sst = self.entry(&srepo, snumber).clone();
            let alive = match sst.worktree_id.as_deref() {
                Some(id) => self
                    .driver(&srepo)
                    .worktree_exists(id)
                    .await
                    .unwrap_or(false),
                None => false,
            };
            if !sst.active && !alive {
                debug!(
                    repo = repo.name,
                    issue = issue.number,
                    subscriber = sid,
                    "subscriber has retired; not telling it"
                );
                continue;
            }
            let mine = self.for_recipient(events, &sid);
            if mine.is_empty() && what == Fyi::Activity {
                continue;
            }
            let ctx = self.ctx(repo, &st);
            let text =
                prompt::fyi_prompt(issue, &mine, &ctx, owner_session.as_deref(), merged, what);
            match self.deliver_to(&srepo, snumber, &text, None).await {
                Ok(_) => {
                    info!(
                        repo = repo.name,
                        issue = issue.number,
                        subscriber = sid,
                        events = mine.len(),
                        "told a subscriber"
                    );
                    let e = self.entry(&srepo, snumber);
                    e.last_prompt_at = Some(now_iso());
                    e.prompts_sent += 1;
                }
                Err(e) if is_blocked(&e) => debug!(
                    repo = repo.name,
                    issue = issue.number,
                    subscriber = sid,
                    "subscriber not told: {e:#}"
                ),
                Err(e) => warn!(
                    repo = repo.name,
                    issue = issue.number,
                    subscriber = sid,
                    "could not tell a subscriber: {e:#}"
                ),
            }
        }
    }

    // ---- sessions blocked on a login ------------------------------------

    /// The sessions of a repository that can be blocked: the same ones the
    /// startup pass looks at (owning, active, with a workspace).
    pub(in crate::engine) async fn check_logins(&mut self, repo: &RepoConfig) {
        let candidates = self.resume_candidates(repo);
        if candidates.is_empty() {
            return;
        }
        let ps = match self.driver(repo).ps().await {
            Ok(p) => p,
            Err(e) => {
                debug!(repo = repo.name, "login check skipped: {e:#}");
                return;
            }
        };
        for number in candidates {
            let st = self.entry(repo, number).clone();
            let Some(wt) = st.worktree_id.clone() else {
                continue;
            };
            if let Some(b) = st.blocked.clone() {
                self.recover(repo, number, &st, b).await;
                continue;
            }
            // Only an idle harness is read: a working one is not at a login
            // prompt, and its screen may quote anything.
            if ps.iter().any(|w| w.worktree_id == wt && w.is_working()) {
                continue;
            }
            let handle = match self
                .driver(repo)
                .live_handle(&wt, st.terminal_handle.as_deref())
                .await
            {
                Ok(Some(h)) => h,
                _ => continue,
            };
            let Ok(screen) = self.driver(repo).screen(&handle).await else {
                continue;
            };
            let harness = self.effective(repo, number).harness;
            if let Some(detail) = crate::driver::login_dialog(&harness, &screen.join("\n")) {
                self.entry(repo, number).terminal_handle = Some(handle);
                self.set_blocked(repo, number, detail).await;
                self.report_blocked(repo, number).await;
            }
        }
        if let Err(e) = self.state.save() {
            error!("saving state: {e:#}");
        }
    }

    /// Is the harness signed in here? Asked once per harness per pass, off
    /// the runtime's workers (the status commands take a second or two).
    pub(in crate::engine) async fn probe_harness(&mut self, harness: &str) -> Probe {
        if let Some(p) = self.probes.get(harness) {
            return p.clone();
        }
        let probe = self.probe.clone();
        let h = harness.to_string();
        let p = match tokio::task::spawn_blocking(move || probe(&h)).await {
            Ok(p) => p,
            Err(e) => Probe {
                state: LoginState::Unknown,
                detail: format!("the login check did not finish: {e}"),
                fingerprint: None,
            },
        };
        self.probes.insert(harness.to_string(), p.clone());
        p
    }

    /// Record that the session's harness is at a login prompt. Nothing is
    /// delivered to it from now on; the item is told once (see
    /// `report_blocked`) and the login is checked every pass. On a record
    /// that already exists (the harness was started again and came back
    /// to the prompt) only the attempt is noted, so the item is not told
    /// twice and the next attempt waits longer.
    pub(in crate::engine) async fn set_blocked(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        detail: String,
    ) -> Blocked {
        self.set_blocked_for(repo, number, Blocked::LOGIN, detail)
            .await
    }

    /// [`set_blocked`](Self::set_blocked) for a harness that could not be
    /// started at all (`Blocked::START`): the item is held the same way,
    /// and `recover` starts it again with the same backoff. A harness
    /// that would not start and is not signed in where the daemon runs is
    /// recorded as the login block it really is, so the item is told the
    /// thing worth fixing.
    pub(in crate::engine) async fn set_blocked_for(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        reason: &str,
        detail: String,
    ) -> Blocked {
        let session = session_id(&repo.name, number);
        let harness = self.effective(repo, number).harness;
        let probe = self.probe_harness(&harness).await;
        let reason = if reason == Blocked::START && probe.state == LoginState::SignedOut {
            Blocked::LOGIN
        } else {
            reason
        };
        let e = self.entry(repo, number);
        if let Some(cur) = e.blocked.as_mut() {
            cur.reason = reason.to_string();
            cur.detail = detail;
            cur.credential = probe.fingerprint;
            cur.retried_at = Some(now_iso());
            cur.retries += 1;
            let b = cur.clone();
            warn!(
                repo = repo.name,
                session,
                retries = b.retries,
                "started again and is blocked still; next attempt in {}s",
                retry_wait(b.retries).as_secs()
            );
            return b;
        }
        let b = Blocked {
            reason: reason.to_string(),
            harness: harness.clone(),
            detail,
            since: now_iso(),
            reported: false,
            credential: probe.fingerprint,
            retried_at: None,
            retries: 0,
            told_at: None,
            tell_failures: 0,
        };
        warn!(
            repo = repo.name,
            session,
            harness,
            detail = b.detail,
            "session is blocked: {} {}; {}",
            login::display_name(&harness),
            if reason == Blocked::START {
                "could not be started"
            } else {
                "is at its sign-in prompt"
            },
            crate::status::fix_clause(&b)
        );
        e.blocked = Some(b.clone());
        b
    }

    /// The `blocked` event on the session's item, once per block: why
    /// deliveries are held (the harness is not signed in, or it could not
    /// be started at all) and how to fix it. The record says it has been
    /// posted whether or not the post went through (`post_event` is best
    /// effort), so a failed post is not tried again every pass.
    pub(in crate::engine) async fn report_blocked(&mut self, repo: &RepoConfig, number: u64) {
        let st = self.entry(repo, number).clone();
        let Some(b) = st.blocked.clone().filter(|b| !b.reported) else {
            return;
        };
        self.entry(repo, number).blocked.as_mut().unwrap().reported = true;
        // The start error is the driver's or the harness's own words, and
        // the post is read by something that looks for sign-in prompts.
        let reason = if b.reason == Blocked::START {
            format!(
                "could not be started: {}",
                safe_error(&events::one_line(&b.detail))
            )
        } else {
            "not signed in".to_string()
        };
        self.post_event(
            repo,
            number,
            Event::Blocked {
                harness: login::display_name(&b.harness),
                reason,
                fix: crate::status::fix_for(&b),
            },
        )
        .await;
    }

    pub(in crate::engine) async fn post_comment(
        &self,
        repo: &RepoConfig,
        number: u64,
        body: &str,
    ) -> Result<String> {
        let (owner, name) = repo.split()?;
        self.gh.comment(owner, name, number, body).await
    }

    /// One of the daemon's own posts on an item (see `events`), as the bot
    /// with the `🤖 ssf` byline and an `event=` tag, so nothing takes it
    /// for a session's or a person's. Best effort: a post that cannot be
    /// made is logged, never retried and never an error to the caller;
    /// nothing at all is posted where event comments are off.
    pub(in crate::engine) async fn post_event(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        event: Event,
    ) {
        if !self.cfg.event_comments(repo) {
            return;
        }
        let Some(origin) = Origin::new(&repo.name, number) else {
            return;
        };
        let kind = match self.peek(repo, number).and_then(|s| s.kind.as_deref()) {
            Some("pull_request") => "pull request",
            _ => "issue",
        };
        let body = events::comment(&origin, kind, &event);
        let session = session_id(&repo.name, number);
        match self.post_comment(repo, number, &body).await {
            Ok(url) => info!(
                repo = repo.name,
                session,
                event = event.name(),
                url,
                "posted the event on the item"
            ),
            Err(e) => warn!(
                repo = repo.name,
                session,
                event = event.name(),
                "could not post the event on the item: {e:#}"
            ),
        }
    }

    /// What the harness of `number`'s workspace runs with, for an
    /// `attached` post: the repository's harness, model and effort as
    /// configured, the driver, and the workspace's branch when known.
    pub(in crate::engine) fn launch_of(&self, repo: &RepoConfig, number: u64) -> events::Launch {
        self.launch_with(repo, number, self.overrides_of(repo, number).as_ref())
    }

    /// [`launch_of`](Self::launch_of) for overrides the item does not have
    /// (yet): what a handover's target would be started with.
    pub(in crate::engine) fn launch_with(
        &self,
        repo: &RepoConfig,
        number: u64,
        overrides: Option<&Overrides>,
    ) -> events::Launch {
        let eff = repo.with_overrides(overrides);
        events::Launch {
            harness: login::display_name(&eff.harness),
            model: eff.model.clone(),
            effort: eff.effort.clone(),
            command: eff.command.clone(),
            driver: self.cfg.driver_for(repo).id().to_string(),
            branch: self.peek(repo, number).and_then(|s| s.branch.clone()),
        }
    }

    /// A blocked session, once per pass: told the item if that is still
    /// owed; lifted if its harness has moved past the login prompt (a
    /// person ran `/login` in the terminal); otherwise, when the login
    /// check says the credential is back (a new credential file, or a
    /// signed-in answer once the retry wait is over), the stuck harness is
    /// quit (if it is still there) and started again with its conversation
    /// resumed, then given one message and, through the listings fetched
    /// afresh, whatever was held. `deliver` decides what the restart
    /// found: a working harness lifts the block, the prompt again notes
    /// the attempt.
    pub(in crate::engine) async fn recover(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        st: &IssueState,
        b: Blocked,
    ) {
        let session = session_id(&repo.name, number);
        let harness = self.effective(repo, number).harness;
        if !b.reported {
            self.report_blocked(repo, number).await;
        }
        let Some(wt) = st.worktree_id.clone() else {
            return;
        };
        let handle = match self
            .driver(repo)
            .live_handle(&wt, st.terminal_handle.as_deref())
            .await
        {
            Ok(h) => h,
            Err(e) => {
                debug!(session, "cannot check the blocked session: {e:#}");
                return;
            }
        };
        // What the item is still owed: a handover's summary sits here
        // until a session has actually read it, so a note that is still
        // there says no session has had its first message yet, whatever
        // the block was recorded as.
        let owed = self
            .peek(repo, number)
            .is_some_and(|s| s.handover_note.is_some());
        // Telling a harness that is running costs a listing read, so it
        // is not tried every pass: once when the block is first looked
        // at, then on the same curve as a restart -- but counted apart
        // from the restarts, so a person who signs in at a terminal a
        // failed restart just left behind is answered on the next pass
        // rather than at the end of the restart's wait.
        let tell_due = match b.told_at.as_deref() {
            None => true,
            Some(t) => age(t) >= retry_wait(b.tell_failures),
        };
        if let Some(h) = &handle {
            let Ok(screen) = self.driver(repo).screen(h).await else {
                return;
            };
            if crate::driver::login_dialog(&harness, &screen.join("\n")).is_none() {
                if b.reason == Blocked::LOGIN && !owed {
                    info!(
                        session,
                        "the harness is past its sign-in prompt; deliveries resume"
                    );
                    self.unblock(repo, number, &b, Conversation::Kept).await;
                    return;
                }
                // The other cases: a harness that would not start, running
                // all the same (`start` gave up on a pane that came up but
                // never settled), and a harness that came up at its
                // sign-in prompt with the handover's first message going
                // into that screen, now signed in by a person. Either way
                // the session is there and has never been told what it is
                // for, so the screen showing no sign-in prompt is not
                // enough to lift the block; what it is owed goes first.
                if !tell_due {
                    debug!(session, "the running harness was told already; waiting");
                    return;
                }
                self.tell_a_started_harness(repo, number, &b).await;
                return;
            }
        }
        let probe = self.probe_harness(&harness).await;
        let changed = probe.fingerprint.is_some() && probe.fingerprint != b.credential;
        let last = b.retried_at.as_deref().unwrap_or(&b.since);
        let due = changed || age(last) >= retry_wait(b.retries);
        if probe.state == LoginState::SignedOut || !due {
            debug!(session, state = ?probe.state, changed, "still blocked ({})", probe.detail);
            return;
        }
        info!(
            session,
            state = ?probe.state,
            changed,
            gone = handle.is_none(),
            reason = b.reason,
            "starting the harness again ({})",
            probe.detail
        );
        if let Some(h) = &handle
            && let Err(e) = self.driver(repo).stop_agent(&wt, h).await
        {
            warn!(session, "could not quit the blocked harness: {e:#}");
            if let Some(cur) = self.entry(repo, number).blocked.as_mut() {
                cur.retried_at = Some(now_iso());
                cur.credential = probe.fingerprint;
            }
            return;
        }
        let text = if b.reason == Blocked::START {
            prompt::start_again_prompt(&prompt::LoginBack {
                harness: &login::display_name(&harness),
                since: &b.since,
                number: st.number,
                title: &st.title,
                url: &st.html_url,
            })
        } else {
            prompt::login_back_prompt(&prompt::LoginBack {
                harness: &login::display_name(&harness),
                since: &b.since,
                number: st.number,
                title: &st.title,
                url: &st.html_url,
            })
        };
        match self.deliver_to(repo, number, &text, None).await {
            Ok(_) => {
                let e = self.entry(repo, number);
                e.last_prompt_at = Some(now_iso());
                e.prompts_sent += 1;
            }
            Err(e) if is_blocked(&e) => {
                debug!(session, "{e:#}");
            }
            Err(e) => {
                warn!(session, "could not start the harness again: {e:#}");
                if let Some(cur) = self.entry(repo, number).blocked.as_mut() {
                    cur.retried_at = Some(now_iso());
                    cur.credential = probe.fingerprint;
                }
            }
        }
    }

    /// The session of a `start` block whose harness turns out to be
    /// running after all: it was never given its first message, so
    /// nothing has told it what the item is, or what the agent that
    /// handed the item over left for it. That message goes now, and the
    /// block is lifted only once it has landed.
    pub(in crate::engine) async fn tell_a_started_harness(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        b: &Blocked,
    ) {
        let session = session_id(&repo.name, number);
        // The attempt is noted before it is made, so a message that
        // cannot be assembled or does not land waits for the backoff
        // instead of costing a listing read on every pass. The restart
        // backoff is left alone: this is not a restart.
        let attempted = Blocked {
            told_at: Some(now_iso()),
            tell_failures: b.tell_failures + 1,
            ..b.clone()
        };
        if let Some(cur) = self.entry(repo, number).blocked.as_mut() {
            cur.told_at = attempted.told_at.clone();
            cur.tell_failures = attempted.tell_failures;
        }
        let story = match self.first_message(repo, number).await {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    session,
                    "the harness is running but cannot be told what it took on: {e:#}"
                );
                return;
            }
        };
        // Everything is held for a session whose record says it is
        // blocked, this message included, so the record is cleared for
        // it -- and put back, with the note, if it does not land.
        let e = self.entry(repo, number);
        let note = e.handover_note.take();
        e.blocked = None;
        match self.deliver_to(repo, number, &story.text, None).await {
            Ok(d) => {
                let e = self.entry(repo, number);
                e.terminal_handle = Some(d.handle);
                e.last_prompt_at = Some(now_iso());
                e.prompts_sent += 1;
                // The story told it everything on the item, so the pass
                // that follows has nothing to deliver again.
                e.seen = story.seen;
                e.updated_at = Some(story.updated_at);
                info!(
                    session,
                    "the harness that would not start is running and has its first message; \
deliveries resume"
                );
                // The conversation was never restarted: this is the one
                // the handover started, told at last.
                self.unblock(repo, number, b, Conversation::Kept).await;
            }
            Err(e) => {
                warn!(session, "could not tell the running harness: {e:#}");
                let cur = self.entry(repo, number);
                cur.handover_note = note;
                // The delivery may have recorded a block of its own (the
                // pane died as the message went out, and the harness
                // started in its place came up at a sign-in screen): what
                // it saw is the fresher answer and says the thing worth
                // fixing, so it stands -- but the hold is the same hold,
                // so how long it has run, that the item was told of it,
                // and both backoffs come from the record it replaces.
                match cur.blocked.as_mut() {
                    Some(fresh) => {
                        fresh.since = attempted.since.clone();
                        fresh.reported = attempted.reported;
                        fresh.retried_at = attempted.retried_at.clone();
                        fresh.retries = attempted.retries;
                        fresh.told_at = attempted.told_at.clone();
                        fresh.tell_failures = attempted.tell_failures;
                    }
                    None => cur.blocked = Some(attempted),
                }
            }
        }
    }

    /// The session is back: forget the block, fetch every listing in full
    /// on this pass so what was held is delivered, and, when the item was
    /// told of the block, tell it the hold is over (`unblocked`, with how
    /// long it lasted and what became of the conversation: `relaunched`
    /// is the restart's `resumed` flag, `None` when the harness carried on
    /// because a person signed in at its terminal).
    pub(in crate::engine) async fn unblock(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        b: &Blocked,
        conversation: Conversation,
    ) {
        self.entry(repo, number).blocked = None;
        self.forget_etags(repo);
        if !b.reported {
            return;
        }
        self.post_event(
            repo,
            number,
            Event::Unblocked {
                harness: login::display_name(&b.harness),
                held: age(&b.since),
                conversation,
            },
        )
        .await;
    }

    /// One full listing set is owed, so that every item involving the bot
    /// is looked at again whatever the ETags said. Where it lands depends
    /// on when the session came back: an unblock before the pass reads its
    /// listings gets it on that pass, and the flag it armed is spent,
    /// unused, when that pass begins; one after gets it on the next
    /// pass, because the pass under way stores the ETags it read at its
    /// end and so puts back what was cleared here. Either way it is one:
    /// the pass that spends the flag clears the ETags without arming it
    /// again, so the pass after that is back to conditional requests.
    pub(in crate::engine) fn forget_etags(&mut self, repo: &RepoConfig) {
        self.clear_etags(repo);
        self.refetch.insert(repo.name.clone());
    }

    /// Drop the repository's cached listing ETags, so the listings this
    /// pass reads are full ones. Nothing beyond this pass is owed: the
    /// ETags it reads are stored at its end and used again next time.
    pub(in crate::engine) fn clear_etags(&mut self, repo: &RepoConfig) {
        let rs = self.state.repo_mut(&repo.name);
        rs.issues_etag = None;
        rs.mentioned_etag = None;
        rs.pulls_etag = None;
        rs.created_etag = None;
    }

    /// Count a failure against an item; at `MAX_DELIVERY_FAILURES` in a
    /// row the binding is given up (the item is onboarded afresh on its
    /// next look) and, when there was a binding to give up (the item was
    /// seeded), the item is told so (`gave-up`). An item that never got a
    /// session (nothing to clone, a workspace the driver cannot make)
    /// fails every look and drops nothing: it is not told each time.
    pub(in crate::engine) async fn note_failure(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        err: &anyhow::Error,
    ) {
        let key = (repo.name.clone(), number);
        let count = self.failures.entry(key.clone()).or_insert(0);
        *count += 1;
        let count = *count;
        warn!(
            repo = repo.name,
            issue = number,
            attempt = count,
            "handling issue failed: {err:#}"
        );
        if count >= MAX_DELIVERY_FAILURES {
            error!(
                repo = repo.name,
                issue = number,
                "giving up on the current workspace binding; the issue will be re-onboarded"
            );
            self.failures.insert(key, 0);
            let mut had_binding = false;
            if let Some(st) = self.state.repo_mut(&repo.name).issues.get_mut(&number) {
                had_binding = st.seeded;
                st.seeded = false;
                st.terminal_handle = None;
            }
            if had_binding {
                self.post_event(
                    repo,
                    number,
                    Event::GaveUp {
                        failures: count,
                        last_error: safe_error(&events::one_line(&format!("{err:#}"))),
                    },
                )
                .await;
            }
        }
    }
}
