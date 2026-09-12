use super::super::*;
use tracing::{debug, info, warn};

impl Engine {
    pub(in crate::engine) async fn release_owner(&mut self, repo: &RepoConfig, owner: u64) {
        let o = self.entry(repo, owner).clone();
        let closed = matches!(o.github_state.as_deref(), Some("closed" | "merged"));
        if o.active || !closed || !self.active_dependents(repo, owner).is_empty() {
            return;
        }
        info!(
            repo = repo.name,
            issue = owner,
            "retired session has no open items left; its workspace can be released"
        );
        if let Some(id) = &o.worktree_id {
            let _ = self.driver(repo).set_status(id, "completed").await;
        }
        let e = self.entry(repo, owner);
        e.retired_at = Some(now_iso());
        e.cleanup_pending = false;
    }

    /// `ssf launch ...` wrapper that puts the bot credentials and issue
    /// identity into the harness's environment. The daemon's own config and
    /// state locations are passed along so the wrapper reads the same files,
    /// and the VM guest flag so `ssf guide` in the session knows where it is.
    pub(in crate::engine) fn launch_command(
        &self,
        repo: &RepoConfig,
        number: u64,
        url: &str,
        inner: &str,
    ) -> String {
        let me = crate::client_executable()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "ssf".to_string());
        let mut prefix = String::new();
        for var in [
            "SSF_CONFIG_DIR",
            "SSF_STATE_DIR",
            "SSF_GITHUB_TOKEN",
            crate::vm::GUEST_ENV,
        ] {
            if let Ok(v) = std::env::var(var) {
                prefix.push_str(&format!("{var}={} ", shell_quote(&v)));
            }
        }
        format!(
            "{prefix}{} launch --repo {} --issue {} --issue-url {} -- {}",
            shell_quote(&me),
            shell_quote(&repo.name),
            number,
            shell_quote(url),
            shell_quote(inner)
        )
    }

    /// Deliver a prompt to the agent that acts on an item (its own session,
    /// or its owner's), bringing the workspace and the agent back first if
    /// either is gone. A harness that has to start from scratch gets
    /// `relaunch_text` instead, or, when that is missing or the prompt is
    /// about another session's item, the session's own story followed by
    /// the prompt.
    pub(in crate::engine) async fn deliver_to(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        text: &str,
        relaunch_text: Option<&str>,
    ) -> Result<Delivery> {
        let target = self.owner_of(repo, number);
        let st = self.entry(repo, target).clone();
        let alive = match st.worktree_id.as_deref() {
            Some(id) => self.driver(repo).worktree_exists(id).await?,
            None => false,
        };
        // A session at a login prompt takes nothing; the prompt is held.
        // With the stuck harness gone the delivery goes ahead: starting it
        // again is the way to find out whether the login is back, and what
        // it shows decides the record below.
        let held = match st.blocked.clone() {
            Some(b) => {
                let stuck = alive
                    && match st.worktree_id.as_deref() {
                        Some(id) => self.driver(repo).has_live_agent(id).await.unwrap_or(false),
                        None => false,
                    };
                if stuck {
                    return Err(SessionBlocked {
                        session: session_id(&repo.name, target),
                        blocked: b,
                    }
                    .into());
                }
                debug!(
                    repo = repo.name,
                    session = session_id(&repo.name, target),
                    "blocked harness is gone; starting it again decides"
                );
                Some(b)
            }
            None => None,
        };
        let re_created = if alive {
            None
        } else {
            self.rehydrate(repo, target).await?
        };
        let st = self.entry(repo, target).clone();
        let worktree_id = st
            .worktree_id
            .clone()
            .context("issue has no workspace bound")?;
        let live = alive
            && self
                .driver(repo)
                .has_live_agent(&worktree_id)
                .await
                .unwrap_or(false);
        let mut story = None;
        // A handover whose new session never came up left its summary on
        // the item: the harness started here is the one that takes it on.
        let mut note_given = false;
        if !live && (target != number || relaunch_text.is_none()) {
            let owed = self
                .peek(repo, target)
                .is_some_and(|s| s.handover_note.is_some());
            match self.first_message(repo, target).await {
                Ok(s) => {
                    story = Some(format!("{}\n\n{text}", s.text));
                    note_given = owed;
                }
                Err(e) => warn!(
                    repo = repo.name,
                    session = session_id(&repo.name, target),
                    "could not assemble the session's story for a fresh harness: {e:#}"
                ),
            }
        }
        let relaunch_text = story.as_deref().or(relaunch_text);
        // The note is taken off the record when the message carrying it
        // goes out, and put back if that message turns out to have gone
        // into a sign-in screen (below).
        let mut spent_note = None;
        let eff = self.effective(repo, target);
        let title = format!("{} · #{target}", eff.harness);
        let resume = st
            .agent_session_id
            .as_deref()
            .and_then(|id| sessions::resume_command(&eff.harness, &eff.harness_command(), id))
            .map(|c| self.launch_command(repo, st.number, &st.html_url, &c));
        let relaunch = self.launch_command(repo, st.number, &st.html_url, &eff.harness_command());
        let d = self
            .driver(repo)
            .deliver(
                &worktree_id,
                st.terminal_handle.as_deref(),
                Relaunch {
                    command: &relaunch,
                    resume_command: resume.as_deref(),
                    harness: &eff.harness,
                    title: &title,
                    text: relaunch_text,
                },
                text,
            )
            .await?;
        if d.relaunched {
            let e = self.entry(repo, target);
            e.launched_at = Some(now_iso());
            if !d.resumed {
                e.agent_session_id = None;
                // A resumed conversation is not shown the relaunch text,
                // so the note is spent only on a fresh one.
                if note_given {
                    spent_note = e.handover_note.take();
                }
            }
            info!(
                repo = repo.name,
                session = session_id(&repo.name, target),
                resumed = d.resumed,
                "harness relaunched"
            );
            // A re-created workspace is news whatever the harness shows.
            if let Some(reason) = re_created {
                let launch = self.launch_of(repo, target);
                self.post_event(
                    repo,
                    target,
                    Event::Attached(Attach::ReCreated {
                        launch,
                        reason,
                        conversation: Conversation::of(d.resumed),
                    }),
                )
                .await;
            }
        }
        self.entry(repo, target).terminal_handle = Some(d.handle.clone());
        // A harness started again on a machine that is not signed in shows
        // its login prompt instead of taking the prompt: the session is
        // blocked from here, and the prompt is held for later.
        if d.relaunched
            && let Ok(screen) = self.driver(repo).screen(&d.handle).await
            && let Some(detail) = crate::driver::login_dialog(&eff.harness, &screen.join("\n"))
        {
            // The message that carried the note went into a sign-in
            // screen, so no session has read it: it waits on the item for
            // the start that gets through.
            if let Some(note) = spent_note {
                self.entry(repo, target).handover_note = Some(note);
            }
            let b = self.set_blocked(repo, target, detail).await;
            return Err(SessionBlocked {
                session: session_id(&repo.name, target),
                blocked: b,
            }
            .into());
        }
        // A harness started again in its existing workspace is `resumed`;
        // one started to lift a login block is told of in `unblocked`, and
        // one started by the target's own onboarding onto a kept workspace
        // in that onboarding's `attached`.
        let onboarding = self
            .onboarding
            .as_ref()
            .is_some_and(|(r, n)| r.eq_ignore_ascii_case(&repo.name) && *n == target);
        if d.relaunched && re_created.is_none() && held.is_none() && !onboarding {
            self.post_event(
                repo,
                target,
                Event::Resumed {
                    harness: login::display_name(&eff.harness),
                    conversation: Conversation::of(d.resumed),
                    after: if self.startup_pass {
                        "restart"
                    } else {
                        "lost terminal"
                    },
                },
            )
            .await;
        }
        if let Some(b) = held {
            let conversation = d
                .relaunched
                .then_some(d.resumed)
                .map_or(Conversation::Kept, Conversation::of);
            self.unblock(repo, target, &b, conversation).await;
        }
        if target != number {
            self.mirror_owner(repo, number, target);
        }
        Ok(d)
    }

    /// A binding written by a driver other than the one the repository runs
    /// in now (`driver` changed since the workspace was made) is dropped
    /// before the workspace is looked for, so the item goes through the
    /// current driver's project setup as a new one would, rather than the
    /// old driver's repo id being handed to the new driver as its own (an
    /// Orca uuid taken for a checkout path, or the other way round). The
    /// workspace name and branch stay, so the branch is picked up as the
    /// base as for any re-created workspace. A record from before the
    /// driver was written down is judged by the shape of its repo id.
    /// Says whether a binding was dropped.
    pub(in crate::engine) fn drop_foreign_binding(
        &mut self,
        repo: &RepoConfig,
        number: u64,
    ) -> bool {
        let current = self.cfg.driver_for(repo);
        let st = self.entry(repo, number).clone();
        let Some(repo_id) = st.repo_id.as_deref() else {
            return false;
        };
        let made_by = match st.driver.as_deref() {
            Some(d) => d.to_string(),
            None if self.driver(repo).owns_repo_id(repo_id) => return false,
            None => DriverKind::of_repo_id(repo_id)
                .map(|k| k.id().to_string())
                .unwrap_or_else(|| "another driver".into()),
        };
        if made_by == current.id() {
            return false;
        }
        info!(
            repo = repo.name,
            session = session_id(&repo.name, number),
            "workspace was made by {made_by}; re-creating it on {}",
            current.id()
        );
        let e = self.entry(repo, number);
        e.repo_id = None;
        e.driver = None;
        e.worktree_id = None;
        e.worktree_path = None;
        e.terminal_handle = None;
        true
    }

    /// The current driver's id for the repository, from the record when
    /// it has one and from the driver's project setup otherwise; written
    /// back so the next look does not set the project up again.
    pub(in crate::engine) async fn repo_id_for(
        &mut self,
        repo: &RepoConfig,
        number: u64,
    ) -> Result<String> {
        if let Some(r) = self.entry(repo, number).repo_id.clone() {
            return Ok(r);
        }
        let (owner, name) = repo.split()?;
        let driver = self.cfg.driver_for(repo);
        let repo_id = self
            .driver(repo)
            .ensure_project(
                owner,
                name,
                &repo.clone_url(),
                repo.path.as_deref(),
                &self.cfg.projects_dir(driver),
            )
            .await?
            .repo_id;
        let e = self.entry(repo, number);
        e.repo_id = Some(repo_id.clone());
        e.driver = Some(driver.id().into());
        Ok(repo_id)
    }

    /// Re-create the workspace for an issue whose worktree is gone,
    /// starting from its old branch when that still exists. Says why it
    /// was re-created (`workspace gone`, or `driver switch` when a binding
    /// made by another driver was dropped first), or `None` when the
    /// driver already had a workspace linked to the issue and nothing was
    /// made.
    pub(in crate::engine) async fn rehydrate(
        &mut self,
        repo: &RepoConfig,
        number: u64,
    ) -> Result<Option<&'static str>> {
        let switched = self.drop_foreign_binding(repo, number);
        let repo_id = self.repo_id_for(repo, number).await?;
        let st = self.entry(repo, number).clone();
        if let Some(existing) = self
            .driver(repo)
            .find_worktree_for_issue(&repo_id, number)
            .await?
        {
            info!(
                repo = repo.name,
                issue = number,
                worktree = existing.id,
                "found workspace linked to the issue"
            );
            self.remember_worktree(repo, number, &existing);
            let e = self.entry(repo, number);
            e.terminal_handle = None;
            return Ok(None);
        }
        let name = st
            .worktree_name
            .clone()
            .unwrap_or_else(|| prompt::worktree_name(number, &st.title));
        let comment = format!("ssf: bound to issue #{number} (re-created)");
        let mut base = repo.base_branch.clone();
        if let Some(branch) = st.branch.as_deref() {
            let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
            match self.driver(repo).existing_branch_ref(&repo_id, short).await {
                Ok(Some(r)) => base = Some(r),
                Ok(None) => debug!(
                    repo = repo.name,
                    issue = number,
                    branch = short,
                    "old branch is gone"
                ),
                Err(e) => warn!(
                    repo = repo.name,
                    issue = number,
                    "could not check old branch: {e:#}"
                ),
            }
        }
        info!(
            repo = repo.name,
            issue = number,
            name,
            base = base.as_deref().unwrap_or("default"),
            "re-creating workspace"
        );
        let created = match self
            .driver(repo)
            .create_worktree(&repo_id, &name, number, &comment, base.as_deref())
            .await
        {
            Ok(w) => w,
            Err(e) if base.is_some() => {
                warn!(
                    repo = repo.name,
                    issue = number,
                    "re-create from old branch failed ({e:#}); using default base"
                );
                self.driver(repo)
                    .create_worktree(
                        &repo_id,
                        &name,
                        number,
                        &comment,
                        repo.base_branch.as_deref(),
                    )
                    .await?
            }
            Err(e) => return Err(e),
        };
        let mut created = created;
        if let Some(branch) = st
            .branch
            .as_deref()
            .map(|b| b.strip_prefix("refs/heads/").unwrap_or(b).to_string())
        {
            if base.is_some() {
                match checkout_branch(&created.path, &branch).await {
                    Ok(()) => created.branch = Some(format!("refs/heads/{branch}")),
                    Err(e) => warn!(
                        repo = repo.name,
                        issue = number,
                        branch,
                        "could not check out the old branch: {e:#}"
                    ),
                }
            }
        }
        self.remember_worktree(repo, number, &created);
        let e = self.entry(repo, number);
        e.terminal_handle = None;
        e.worktree_name = Some(name);
        Ok(Some(if switched {
            "driver switch"
        } else {
            "workspace gone"
        }))
    }

    /// Record harness session ids for workspaces that do not have one yet.
    pub(in crate::engine) fn capture_sessions(&mut self, repo: &RepoConfig) {
        // Per item, since a handed-over item runs a harness of its own:
        // the records worth looking at, each with the harness it runs.
        let candidates: Vec<u64> = match self.state.repos.get(&repo.name) {
            Some(rs) => rs
                .issues
                .values()
                .filter(|s| s.agent_session_id.is_none() && s.worktree_id.is_some())
                .map(|s| s.number)
                .collect(),
            None => Vec::new(),
        };
        let harnesses: Vec<(u64, String)> = candidates
            .into_iter()
            .map(|n| (n, self.effective(repo, n).harness))
            .filter(|(_, h)| sessions::supports_resume(h))
            .collect();
        let rs = self.state.repo_mut(&repo.name);
        for (number, harness) in harnesses {
            let Some(st) = rs.issues.get_mut(&number) else {
                continue;
            };
            let (Some(path), Some(launched)) =
                (st.worktree_path.as_deref(), st.launched_at.as_deref())
            else {
                continue;
            };
            let retired = st.retired_session_ids.clone();
            let since = capture_since(launched, st.handed_over_at.as_deref());
            if let Some(id) = sessions::capture(&harness, path, since, &retired) {
                info!(
                    repo = repo.name,
                    issue = st.number,
                    session = id,
                    "captured {harness} session"
                );
                st.agent_session_id = Some(id);
            }
        }
        let owned: Vec<(u64, u64)> = rs
            .issues
            .values()
            .filter_map(|s| s.shares_workspace_of.map(|o| (s.number, o)))
            .collect();
        for (number, owner) in owned {
            if let Some(id) = rs
                .issues
                .get(&owner)
                .and_then(|o| o.agent_session_id.clone())
            {
                if let Some(s) = rs.issues.get_mut(&number) {
                    s.agent_session_id = Some(id);
                }
            }
        }
    }

    /// Remove the workspaces `ssf release` approved (checked once more
    /// here, since the agent may have carried on). Workspaces are never
    /// removed on the old close-time flag; a stale one is cleared.
    pub(in crate::engine) async fn run_cleanups(&mut self, repo: &RepoConfig) {
        let rs = self.state.repo_mut(&repo.name);
        let pending: Vec<IssueState> = rs
            .issues
            .values()
            .filter(|s| s.release_pending || s.cleanup_pending)
            .cloned()
            .collect();
        for st in pending {
            if st.cleanup_pending {
                self.entry(repo, st.number).cleanup_pending = false;
            }
            if st.release_pending {
                self.finish_release(repo, st).await;
            }
        }
    }

    /// Second half of `ssf release`: the checks again, then the removal.
    pub(in crate::engine) async fn finish_release(&mut self, repo: &RepoConfig, st: IssueState) {
        let session = session_id(&repo.name, st.number);
        let Some(id) = st.worktree_id.clone() else {
            self.mark_released(repo, st.number);
            return;
        };
        if !self.driver(repo).worktree_exists(&id).await.unwrap_or(true) {
            info!(session, "workspace is already gone");
            self.mark_released(repo, st.number);
            return;
        }
        if st.active {
            warn!(session, "release dropped: the item is active again");
            self.drop_release(repo, st.number);
            return;
        }
        if !st.release_forced && !self.active_dependents(repo, st.number).is_empty() {
            warn!(
                session,
                "release dropped: the session owns open items again"
            );
            self.drop_release(repo, st.number);
            return;
        }
        if !st.release_forced {
            let verdict = match st.worktree_path.as_deref() {
                Some(path) => release::inspect(path).await.map(|c| c.problems()),
                None => Err(anyhow::anyhow!("no workspace path recorded")),
            };
            let problems = match verdict {
                Ok(p) => p,
                Err(e) => vec![format!("{e:#}")],
            };
            if !problems.is_empty() {
                self.refuse_release(repo, &st, problems).await;
                return;
            }
        }
        match self.driver(repo).remove_worktree(&id).await {
            Ok(()) => {
                info!(session, worktree = id, "released the workspace");
                self.mark_released(repo, st.number);
                self.post_event(
                    repo,
                    st.number,
                    Event::Released {
                        by: "ssf release",
                        forced: st.release_forced,
                        branch: st.branch.clone(),
                    },
                )
                .await;
            }
            Err(e) => {
                warn!(session, "removing the workspace failed: {e:#}");
                self.drop_release(repo, st.number);
            }
        }
    }

    /// A pending release is off; a later `ssf release` starts from scratch.
    pub(in crate::engine) fn drop_release(&mut self, repo: &RepoConfig, number: u64) {
        let e = self.entry(repo, number);
        e.release_pending = false;
        e.release_forced = false;
    }

    /// The pass's own re-check found work in a workspace `ssf release` had
    /// approved: drop the release, count it, and tell the agent what would
    /// be lost so it can fix that and ask again. After
    /// `MAX_RELEASE_REFUSALS` the agent hears no more and the workspace is
    /// kept for a person (`release given up` in status, peers and purge).
    pub(in crate::engine) async fn refuse_release(
        &mut self,
        repo: &RepoConfig,
        st: &IssueState,
        problems: Vec<String>,
    ) {
        let session = session_id(&repo.name, st.number);
        let e = self.entry(repo, st.number);
        e.release_pending = false;
        e.release_forced = false;
        e.release_refusals = e.release_refusals.saturating_add(1);
        let n = e.release_refusals;
        warn!(
            session,
            ?problems,
            refusals = n,
            "release refused: the workspace changed since the checks passed"
        );
        if n > MAX_RELEASE_REFUSALS {
            return;
        }
        // Only an agent that is there hears about it; a refusal is not
        // worth starting a harness for.
        let live = match st.worktree_id.as_deref() {
            Some(id) => self.driver(repo).has_live_agent(id).await.unwrap_or(false),
            None => false,
        };
        if !live {
            debug!(session, "no live agent to tell about the refused release");
            return;
        }
        let text = prompt::release_refused_prompt(
            &repo.name,
            st.number,
            &problems,
            n,
            MAX_RELEASE_REFUSALS,
        );
        match self.deliver_to(repo, st.number, &text, None).await {
            Ok(d) => {
                let e = self.entry(repo, st.number);
                e.terminal_handle = Some(d.handle);
                e.last_prompt_at = Some(now_iso());
                e.prompts_sent += 1;
            }
            Err(e) => warn!(
                session,
                "could not tell the agent about the refused release: {e:#}"
            ),
        }
    }

    /// The workspace of `number`'s session is gone by our hand: forget its
    /// bindings (the next event re-creates it) and record when.
    pub(in crate::engine) fn mark_released(&mut self, repo: &RepoConfig, number: u64) {
        let now = now_iso();
        let e = self.entry(repo, number);
        e.release_pending = false;
        e.release_forced = false;
        e.cleanup_pending = false;
        e.release_refusals = 0;
        e.worktree_id = None;
        e.worktree_path = None;
        e.terminal_handle = None;
        e.released_at = Some(now.clone());
        // The item comes back on the repository's own harness.
        e.overrides = None;
        e.handover = None;
        e.handover_note = None;
        e.handed_over_at = None;
        // Items bound to this session mirror its workspace.
        let bound: Vec<u64> = self
            .state
            .repo_mut(&repo.name)
            .issues
            .values()
            .filter(|s| s.shares_workspace_of == Some(number))
            .map(|s| s.number)
            .collect();
        for n in bound {
            let e = self.entry(repo, n);
            e.worktree_id = None;
            e.worktree_path = None;
            e.terminal_handle = None;
            e.released_at = Some(now.clone());
        }
    }

    // ---- handovers ------------------------------------------------------
}
