use super::super::*;
use tracing::{debug, error, info, warn};

impl Engine {
    pub(in crate::engine) async fn handover(
        &mut self,
        session: &str,
        harness: &str,
        model: Option<&str>,
        effort: Option<&str>,
        summary: Option<&str>,
        by: Option<&str>,
    ) -> Result<Value> {
        let (repo, number, id) = self.known_session(session)?;
        let st = self.entry(&repo, number).clone();
        if !st.active || st.worktree_id.is_none() {
            anyhow::bail!(
                "{id}: the item has no running session; nothing to hand over (assign the bot to it instead)"
            );
        }
        let harness = harness.trim();
        if !crate::agents::is_known(harness) {
            anyhow::bail!("{harness} is not a harness ssf knows (see `ssf agents`)");
        }
        crate::models::validate(harness, model, effort)?;
        let name = login::display_name(harness);
        if !(self.installed)(harness) {
            anyhow::bail!("{name} is not installed where the daemon runs (see `ssf agents`)");
        }
        // Asked afresh rather than off the pass's memo: an operator who
        // signs the harness in and runs the command again must get the
        // new answer, not the one from up to a poll interval ago.
        self.probes.remove(harness);
        let probe = self.probe_harness(harness).await;
        if probe.state == LoginState::SignedOut {
            anyhow::bail!(
                "{name} is not signed in here; {}",
                login::how_to_sign_in(harness)
            );
        }
        if let Some(h) = st.handover.as_ref() {
            anyhow::bail!("a handover to {} is already pending", h.harness);
        }
        if st.release_pending {
            anyhow::bail!("a release is pending on this item");
        }
        // Trimmed as `models::validate` reads them, or a stray space
        // would be shell-quoted into the launch command and the harness
        // would refuse the model it was given.
        let overrides = Overrides {
            harness: harness.to_string(),
            model: model.map(|m| m.trim().to_string()),
            effort: effort.map(|e| e.trim().to_string()),
        };
        let from = self.effective(&repo, number);
        let to = repo.with_overrides(Some(&overrides));
        if to.harness == from.harness && to.model == from.model && to.effort == from.effort {
            anyhow::bail!("the item is already on {harness} with that model and effort");
        }
        if summary.is_some_and(|s| s.trim().is_empty()) {
            anyhow::bail!("the summary is empty: write a summary or hand over with no summary");
        }
        let chars = summary.map(|s| s.chars().count());
        if let Some(n) = chars.filter(|n| *n > crate::ipc::MAX_SUMMARY_CHARS) {
            anyhow::bail!(
                "the summary is {n} characters; the most a handover carries is {}",
                crate::ipc::MAX_SUMMARY_CHARS
            );
        }
        // The summary is pasted into the new session's terminal, where the
        // login check reads the screen: one that quotes a sign-in prompt
        // would block the session it starts. The CLI refuses it too, where
        // the author can fix it; this is the daemon's own guard.
        if let Some(line) = summary.and_then(crate::driver::login_prompt_line) {
            anyhow::bail!(
                "{}",
                crate::summary_quotes_a_sign_in_screen_text(&crate::driver::redact_login_phrases(
                    &line
                ))
            );
        }
        let pending = PendingHandover {
            harness: overrides.harness.clone(),
            model: overrides.model.clone(),
            effort: overrides.effort.clone(),
            summary: summary.map(str::to_string),
            by: by.map(str::to_string),
            requested_at: now_iso(),
        };
        self.entry(&repo, number).handover = Some(pending);
        info!(
            session = id,
            harness,
            model,
            effort,
            summary_chars = chars,
            by,
            "handover recorded"
        );
        // The command is on both sides because it is what decides an
        // unset model or effort, and the `handed-over` post says so; the
        // command itself is on the item in every `attached` post anyway.
        let launch = |c: &RepoConfig| {
            serde_json::json!({
                "harness": c.harness,
                "model": c.model,
                "effort": c.effort,
                "command": c.command,
            })
        };
        Ok(serde_json::json!({
            "session": id,
            "title": st.title,
            "from": launch(&from),
            "to": launch(&to),
            "summary_chars": chars,
            "poll_interval_secs": self.cfg.daemon.poll_interval_secs,
        }))
    }

    /// Carry out the handovers `ssf handover` accepted, before anything
    /// else this repository does on this pass: the old session is not worth
    /// resuming or delivering to.
    pub(in crate::engine) async fn run_handovers(&mut self, repo: &RepoConfig) {
        let pending: Vec<(u64, PendingHandover)> = match self.state.repos.get(&repo.name) {
            Some(rs) => rs
                .issues
                .values()
                .filter_map(|s| s.handover.clone().map(|h| (s.number, h)))
                .collect(),
            None => Vec::new(),
        };
        if pending.is_empty() {
            return;
        }
        // The new session is given the item's story, which the allow-list
        // filters: the collaborators have to be known first, or every
        // human post would be left out of it. A handover the daemon
        // cannot read the repository for waits for the next pass.
        let refreshed = match repo.split() {
            Ok((owner, name)) => self.refresh_collaborators(repo, owner, name).await,
            Err(e) => Err(e),
        };
        if let Err(e) = refreshed {
            warn!(
                repo = repo.name,
                "handovers wait for the next pass on this repository: {e:#}"
            );
            return;
        }
        for (number, h) in pending {
            self.finish_handover(repo, number, h).await;
            if let Err(e) = self.state.save() {
                error!("saving state: {e:#}");
            }
        }
    }

    /// Second half of `ssf handover`: end the session that is there, keep
    /// its workspace and branch, write the item's launch overrides, and
    /// start the new session in the same workspace with the outgoing
    /// agent's summary ahead of the item's story.
    pub(in crate::engine) async fn finish_handover(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        h: PendingHandover,
    ) {
        let session = session_id(&repo.name, number);
        let st = self.entry(repo, number).clone();
        if !st.active {
            self.refuse_handover(repo, number, &h, "the item is no longer active".into())
                .await;
            return;
        }
        // The workspace normally stays; one that went missing between the
        // request and now is re-created rather than the handover lost.
        let alive = match st.worktree_id.as_deref() {
            Some(id) => self.driver(repo).worktree_exists(id).await.unwrap_or(false),
            None => false,
        };
        if !alive && let Err(e) = self.rehydrate(repo, number).await {
            self.refuse_handover(
                repo,
                number,
                &h,
                format!("the workspace is gone and could not be re-created: {e:#}"),
            )
            .await;
            return;
        }
        let st = self.entry(repo, number).clone();
        let Some(wt) = st.worktree_id.clone() else {
            self.refuse_handover(repo, number, &h, "the item has no workspace".into())
                .await;
            return;
        };
        // The new session's first message is built before anything is
        // stopped: a story that cannot be assembled is a refusal, not a
        // session ended with nothing to put in its place.
        let from_launch = self.launch_of(repo, number);
        let from_harness = self.effective(repo, number).harness;
        let from_name = login::display_name(&from_harness);
        // Who the new session really takes over from. Normally the
        // harness the item is on; but a handover whose harness never came
        // up left a note of its own, and the session that wrote it is
        // still the last one that worked the item, so its name is the one
        // carried forward. The `handed-over` post keeps saying what the
        // item was configured on.
        let pending_note = self
            .peek(repo, number)
            .and_then(|s| s.handover_note.clone());
        let note_from = pending_note
            .as_ref()
            .map(|n| n.from.clone())
            .unwrap_or_else(|| from_name.clone());
        // A summary nobody has read yet is not thrown away by a handover
        // that carries none of its own: the session that wrote it is long
        // gone, and the one starting now is the first that can act on it.
        // A handover that does bring a summary replaces it, since that is
        // the newer account of where the item stands.
        let summary = h
            .summary
            .clone()
            .or_else(|| pending_note.and_then(|n| n.summary));
        let story = match self
            .story(repo, number, Some(&note_from), Some(&h.harness))
            .await
        {
            Ok(s) => s,
            Err(e) => {
                self.refuse_handover(
                    repo,
                    number,
                    &h,
                    format!("the item could not be read for the new session: {e:#}"),
                )
                .await;
                return;
            }
        };
        let kind = self.item_kind(repo, number);
        let text = prompt::handover_prompt(&note_from, kind, summary.as_deref(), &story.text);
        // End the caller's pane. The workspace stays, so the new session
        // opens on the same checkout and branch.
        match self
            .driver(repo)
            .live_handle(&wt, st.terminal_handle.as_deref())
            .await
        {
            Ok(Some(handle)) => {
                if let Err(e) = self.driver(repo).stop_agent(&wt, &handle).await {
                    self.refuse_handover(
                        repo,
                        number,
                        &h,
                        format!("could not stop the running agent: {e:#}"),
                    )
                    .await;
                    return;
                }
            }
            Ok(None) => debug!(session, "no agent to stop; starting the new session"),
            Err(e) => {
                self.refuse_handover(
                    repo,
                    number,
                    &h,
                    format!("could not stop the running agent: {e:#}"),
                )
                .await;
                return;
            }
        }
        // A hold on the item ends here: the session it was held for is
        // gone. The item was told the hold was on, so it is told it is
        // over, before the handover itself is posted.
        if let Some(b) = st.blocked.clone() {
            self.unblock(repo, number, &b, Conversation::HandedOver)
                .await;
        }
        // Items bound to this session mirror its conversation id, so the
        // one being retired goes from them too (`capture_sessions` writes
        // the owner's new id to them once there is one).
        let bound: Vec<u64> = self
            .state
            .repo_mut(&repo.name)
            .issues
            .values()
            .filter(|s| s.shares_workspace_of == Some(number))
            .map(|s| s.number)
            .collect();
        // The conversations the outgoing agent leaves behind: the id on
        // the record, and whatever its harness last wrote in this
        // workspace, which is what a session ssf never captured an id for
        // leaves behind (see `retired_conversations`). The transcript
        // directories live under the real home, so no engine test covers
        // this call; `retired_conversations` is what the tests pin.
        let newest = st
            .worktree_path
            .as_deref()
            .and_then(|p| sessions::capture(&from_harness, p, SystemTime::UNIX_EPOCH, &[]));
        // The old session is retired on the record; everything about the
        // item and its workspace stays.
        let retired =
            retired_conversations(self.entry(repo, number).agent_session_id.take(), newest);
        {
            let e = self.entry(repo, number);
            // The conversations being dropped are remembered, so the
            // harness starting in this workspace is never given the
            // outgoing agent's transcript as its own (`capture_sessions`,
            // `sessions::capture`).
            retire(e, &retired);
            e.terminal_handle = None;
            // The hold, if there was one, was closed just above.
            e.launched_at = None;
            e.handed_over_at = Some(now_iso());
            e.overrides = Some(h.overrides());
            e.handover = None;
            // What the outgoing agent left is kept on the item until a
            // session has read it: the start below can fail, or come up
            // at a sign-in screen, and the harness started again minutes
            // later is the one that takes the work on.
            e.handover_note = Some(HandoverNote {
                from: note_from.clone(),
                summary: summary.clone(),
            });
        }
        for n in bound {
            let e = self.entry(repo, n);
            e.agent_session_id = None;
            retire(e, &retired);
        }
        let eff = self.effective(repo, number);
        let to_launch = self.launch_of(repo, number);
        let title = format!("{} · #{number}", eff.harness);
        let cmd = self.launch_command(repo, number, &st.html_url, &eff.harness_command());
        self.entry(repo, number).launched_at = Some(now_iso());
        let handle = match self
            .driver(repo)
            .start(&wt, &cmd, &title, &eff.harness, &text)
            .await
        {
            Ok(handle) => handle,
            Err(e) => {
                // The handover stands (the item keeps the overrides), but
                // nothing is running: the item is blocked as it is for a
                // harness that comes up at its sign-in prompt, with the
                // same restart-with-backoff recovery, and the old session
                // is not brought back.
                warn!(
                    session,
                    harness = eff.harness,
                    "handed over, but the new harness could not be started: {e:#}"
                );
                self.post_event(
                    repo,
                    number,
                    handed_over(&from_launch, &to_launch, &h, summary.is_some(), None),
                )
                .await;
                let why = safe_error(&events::one_line(&format!("{e:#}")));
                self.set_blocked_for(repo, number, Blocked::START, why)
                    .await;
                self.report_blocked(repo, number).await;
                return;
            }
        };
        info!(
            session,
            harness = eff.harness,
            handle,
            from = from_name,
            summary = summary.is_some(),
            "handed the item over to a new session"
        );
        {
            let e = self.entry(repo, number);
            e.terminal_handle = Some(handle.clone());
            e.last_prompt_at = Some(now_iso());
            e.prompts_sent += 1;
            // The story told the new session everything on the item, so
            // the pass that follows has nothing to deliver again (an
            // onboarding records the same two things for its session).
            e.seen = story.seen;
            e.updated_at = Some(story.updated_at);
        }
        // Nothing is owed to the session that is gone.
        self.failures.remove(&(repo.name.clone(), number));
        self.post_event(
            repo,
            number,
            handed_over(&from_launch, &to_launch, &h, summary.is_some(), None),
        )
        .await;
        // A harness started on a machine it is not signed in on shows its
        // sign-in prompt instead of taking the message: the new session is
        // blocked from here, and the old one is not brought back.
        if let Ok(screen) = self.driver(repo).screen(&handle).await
            && let Some(detail) = crate::driver::login_dialog(&eff.harness, &screen.join("\n"))
        {
            self.set_blocked(repo, number, detail).await;
            self.report_blocked(repo, number).await;
            return;
        }
        // The message went to a harness that took it, not to a sign-in
        // screen: what the outgoing agent left has been read.
        self.entry(repo, number).handover_note = None;
        self.post_event(
            repo,
            number,
            Event::Attached(Attach::HandedOver {
                launch: to_launch,
                from: from_name,
            }),
        )
        .await;
    }

    /// `ssf handover --cancel`: drop a handover the daemon has recorded
    /// and not carried out yet. The session that is there keeps the item,
    /// and hears so if it is still running -- it was told to stop working
    /// when the handover was recorded, and nothing else can reach it
    /// while one is pending. Nothing is posted on the item: the handover
    /// was never announced there.
    pub(in crate::engine) async fn cancel_handover(&mut self, session: &str) -> Result<Value> {
        let (repo, number, id) = self.known_session(session)?;
        let st = self.entry(&repo, number).clone();
        let Some(h) = st.handover.clone() else {
            anyhow::bail!("{id}: no handover is pending on this item");
        };
        self.entry(&repo, number).handover = None;
        let name = login::display_name(&h.harness);
        info!(session = id, harness = h.harness, "handover cancelled");
        let live = match st.worktree_id.as_deref() {
            Some(w) => self.driver(&repo).has_live_agent(w).await.unwrap_or(false),
            None => false,
        };
        let mut told = false;
        if live {
            let text = prompt::handover_cancelled_prompt(&name);
            match self.deliver_to(&repo, number, &text, None).await {
                Ok(d) => {
                    told = true;
                    let e = self.entry(&repo, number);
                    e.terminal_handle = Some(d.handle);
                    e.last_prompt_at = Some(now_iso());
                    e.prompts_sent += 1;
                }
                Err(e) => warn!(
                    session = id,
                    "could not tell the agent the handover was cancelled: {e:#}"
                ),
            }
        }
        Ok(serde_json::json!({
            "session": id,
            "title": st.title,
            "harness": h.harness,
            "harness_name": name,
            "requested_at": h.requested_at,
            "told": told,
        }))
    }

    /// A handover that was accepted and cannot be carried out: nothing
    /// changes, the item says so, and the agent that asked (if it is still
    /// there) hears it in one message.
    pub(in crate::engine) async fn refuse_handover(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        h: &PendingHandover,
        why: String,
    ) {
        let session = session_id(&repo.name, number);
        warn!(session, harness = h.harness, "handover refused: {why}");
        self.entry(repo, number).handover = None;
        // The reason can carry a driver's or a harness's own words, and
        // both places it goes (the item, and the agent's screen) are
        // read by something that looks for sign-in prompts.
        let why = safe_error(&events::one_line(&why));
        let from = self.launch_of(repo, number);
        let to = self.launch_with(repo, number, Some(&h.overrides()));
        self.post_event(
            repo,
            number,
            handed_over(&from, &to, h, h.summary.is_some(), Some(why.clone())),
        )
        .await;
        let st = self.entry(repo, number).clone();
        let live = match st.worktree_id.as_deref() {
            Some(id) => self.driver(repo).has_live_agent(id).await.unwrap_or(false),
            None => false,
        };
        if !live {
            debug!(session, "no live agent to tell about the refused handover");
            return;
        }
        let text = prompt::handover_refused_prompt(&login::display_name(&h.harness), &why);
        match self.deliver_to(repo, number, &text, None).await {
            Ok(d) => {
                let e = self.entry(repo, number);
                e.terminal_handle = Some(d.handle);
                e.last_prompt_at = Some(now_iso());
                e.prompts_sent += 1;
            }
            Err(e) => warn!(
                session,
                "could not tell the agent about the refused handover: {e:#}"
            ),
        }
    }

    /// `ssf release`: the session's workspace goes on the next pass if the
    /// checks pass now (and again then); `force` skips them, for a person
    /// who has looked, including the check for open items bound to the
    /// session.
    pub(in crate::engine) async fn release(&mut self, session: &str, force: bool) -> Result<Value> {
        let (repo, number, id) = self.known_session(session)?;
        let st = self.entry(&repo, number).clone();
        if st.active {
            let (why, fix) = why_active(&st.triggers);
            anyhow::bail!("{id} {why}; its workspace is in use. {fix} first");
        }
        if let Some(h) = st.handover.as_ref() {
            anyhow::bail!("{id}: a handover to {} is pending", h.harness);
        }
        let deps = self.active_dependents(&repo, number);
        if !deps.is_empty() && !force {
            let deps: Vec<String> = deps.iter().map(|n| format!("#{n}")).collect();
            anyhow::bail!(
                "{id} still owns open items ({}); close them first, or a person can release the workspace with `ssf release --as {id} --force` (work in it may be lost)",
                deps.join(", ")
            );
        }
        if st.release_refusals >= MAX_RELEASE_REFUSALS && !force {
            anyhow::bail!(
                "{id}: release given up after {} refusals by the daemon's own re-check; the workspace is kept for a person (`ssf release --as {id} --force` from a shell, or `ssf purge`)",
                st.release_refusals
            );
        }
        let Some(wid) = st.worktree_id.clone() else {
            anyhow::bail!(
                "{id} has no workspace{}",
                if st.released_at.is_some() {
                    " (already released)"
                } else {
                    ""
                }
            );
        };
        // Orca not answering is not a reason to refuse: the pass checks
        // again before removing anything.
        if !self
            .driver(&repo)
            .worktree_exists(&wid)
            .await
            .unwrap_or(true)
        {
            self.mark_released(&repo, number);
            return Ok(serde_json::json!({
                "session": id, "title": st.title, "released": true, "already_gone": true,
            }));
        }
        let path = st
            .worktree_path
            .clone()
            .with_context(|| format!("{id}: no workspace path recorded"))?;
        let check = match release::inspect(&path).await {
            Ok(c) => c.to_json(),
            Err(e) => serde_json::json!({
                "state": "unknown", "safe": false, "problems": [format!("{e:#}")],
            }),
        };
        let safe = check["safe"].as_bool().unwrap_or(false);
        if !safe && !force {
            info!(session = id, "release refused: the workspace holds work");
            return Ok(serde_json::json!({
                "session": id, "title": st.title, "path": path, "released": false, "check": check,
            }));
        }
        let e = self.entry(&repo, number);
        e.release_pending = true;
        // Forced: `finish_release` must not run the checks again, and a
        // person has taken over from the agent's refused attempts.
        e.release_forced = force;
        if force {
            e.release_refusals = 0;
        }
        info!(session = id, forced = force, "release accepted");
        Ok(serde_json::json!({
            "session": id, "title": st.title, "path": path, "released": true,
            "forced": force, "pending": true, "check": check,
            "poll_interval_secs": self.cfg.daemon.poll_interval_secs,
        }))
    }

    /// Item records whose workspace `ssf purge` may look at: retired and
    /// closed, owning their workspace, with no open item bound to them, and
    /// (with `older_than_days`) retired long enough ago.
    pub(in crate::engine) fn purge_candidates(
        &self,
        repo: &RepoConfig,
        older_than_days: Option<u64>,
    ) -> Vec<IssueState> {
        let Some(rs) = self.state.repos.get(&repo.name) else {
            return Vec::new();
        };
        let cutoff = older_than_days.map(|d| chrono::Utc::now() - chrono::Duration::days(d as i64));
        rs.issues
            .values()
            .filter(|s| {
                !s.active
                    && !s.subscriber_only
                    && s.shares_workspace_of.is_none()
                    && s.worktree_id.is_some()
                    && matches!(s.github_state.as_deref(), Some("closed" | "merged"))
                    && self.active_dependents(repo, s.number).is_empty()
                    && cutoff.is_none_or(|c| {
                        s.retired_at
                            .as_deref()
                            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                            .is_some_and(|t| t < c)
                    })
            })
            .cloned()
            .collect()
    }

    /// `ssf purge`: every candidate workspace with its state; the clean and
    /// pushed ones (all of them with `force`) are removed unless `dry_run`.
    /// A workspace with a live agent terminal is only listed.
    pub(in crate::engine) async fn purge(
        &mut self,
        dry_run: bool,
        older_than_days: Option<u64>,
        force: bool,
    ) -> Result<Value> {
        let mut rows = Vec::new();
        for repo in self.cfg.repos.clone() {
            for st in self.purge_candidates(&repo, older_than_days) {
                let session = session_id(&repo.name, st.number);
                let id = st.worktree_id.clone().unwrap_or_default();
                let mut row = serde_json::json!({
                    "session": session,
                    "title": st.title,
                    "path": st.worktree_path,
                    "retired_at": st.retired_at,
                    "removed": false,
                    "release_given_up": st.release_refusals >= MAX_RELEASE_REFUSALS,
                });
                let workspace_gone = !self.driver(&repo).worktree_exists(&id).await?;
                // The driver's workspace can be gone while the git checkout
                // is not (a workspace closed by hand leaves it behind), and
                // the checkout is what holds the work: it is judged like
                // any other, and removed with git rather than the driver.
                let checkout = st
                    .worktree_path
                    .as_deref()
                    .filter(|p| Path::new(p).join(".git").exists());
                if workspace_gone && checkout.is_none() {
                    row["state"] = "already gone".into();
                    if !dry_run {
                        self.mark_released(&repo, st.number);
                        row["removed"] = true.into();
                    }
                    rows.push(row);
                    continue;
                }
                if workspace_gone {
                    row["workspace"] = "gone".into();
                } else if self.driver(&repo).has_live_agent(&id).await.unwrap_or(true) {
                    row["state"] = "agent running".into();
                    rows.push(row);
                    continue;
                }
                let (state, safe, problems) = checkout_state(st.worktree_path.as_deref()).await;
                row["state"] = state.into();
                row["problems"] = problems.into();
                if !dry_run && (safe || force) {
                    let removed = match checkout {
                        Some(path) if workspace_gone => {
                            crate::driver::remove_stray_worktree(path).await
                        }
                        _ => self.driver(&repo).remove_worktree(&id).await,
                    };
                    match removed {
                        Ok(()) => {
                            info!(
                                session,
                                worktree = id,
                                forced = !safe,
                                workspace_gone,
                                "purged the workspace"
                            );
                            self.mark_released(&repo, st.number);
                            self.post_event(
                                &repo,
                                st.number,
                                Event::Released {
                                    by: "ssf purge",
                                    forced: force,
                                    branch: st.branch.clone(),
                                },
                            )
                            .await;
                            row["removed"] = true.into();
                        }
                        Err(e) => {
                            warn!(session, "removing the workspace failed: {e:#}");
                            row["error"] = format!("{e:#}").into();
                        }
                    }
                }
                rows.push(row);
            }
        }
        Ok(serde_json::json!({ "dry_run": dry_run, "force": force, "workspaces": rows }))
    }
}
