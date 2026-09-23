//! Scratch sessions (`owner/repo~id`): agent sessions on a repository that
//! work on no item. Each has its own worktree on `scratch/<id>`, lives
//! until a person kills it with `ssf release` (the same checks as an
//! item's release), and can be brought back with `ssf scratch resume`,
//! which re-creates the workspace and resumes the harness conversation.
//! Nothing an item goes through (retirement, purge, the closed and merged
//! rules) reaches one: their records are in `RepoState::scratch`.

use super::super::*;
use crate::driver::{FirstPrompt, NO_ITEM, SCRATCH_PREFIX};
use crate::ipc::Refused;
use crate::origin::Scratch;
use crate::state::ScratchState;
use tracing::{debug, info, warn};

/// How many characters a generated scratch id has.
const ID_LEN: usize = 4;

/// A fresh scratch id: `ID_LEN` lowercase letters and digits, none that
/// `taken` says is in use.
pub(crate) fn new_scratch_id(taken: impl Fn(&str) -> bool) -> String {
    use std::hash::{BuildHasher, Hasher};
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    loop {
        // Each `RandomState` is keyed afresh, which is all the randomness an
        // id that only has to be unique on one repository needs.
        let mut n = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        let id: String = (0..ID_LEN)
            .map(|_| {
                let c = ALPHABET[(n % ALPHABET.len() as u64) as usize] as char;
                n /= ALPHABET.len() as u64;
                c
            })
            .collect();
        if !taken(&id) {
            return id;
        }
    }
}

impl Engine {
    /// The watched repository, the canonical reference and the record of
    /// the scratch session `session` (`owner/repo~id`) names.
    pub(in crate::engine) fn scratch(
        &self,
        session: &str,
    ) -> Result<(RepoConfig, Scratch, ScratchState)> {
        let s = Scratch::parse(session)
            .ok_or_else(|| Refused::bad_input(format!("{session}: expected owner/repo~id")))?;
        let repo = self
            .cfg
            .repos
            .iter()
            .find(|r| r.matches_name(&s.repo))
            .cloned()
            .ok_or_else(|| Refused::bad_input(format!("{} is not a watched repository", s.repo)))?;
        let st = self
            .state
            .repos
            .get(&repo.name)
            .and_then(|rs| rs.scratch.get(&s.id))
            .cloned()
            .ok_or_else(|| {
                Refused::conflict(format!(
                    "{session} is not a scratch session ssf knows (see `ssf status`)"
                ))
            })?;
        let s = Scratch {
            repo: repo.name.clone(),
            id: s.id,
        };
        Ok((repo, s, st))
    }

    fn scratch_entry(&mut self, repo: &RepoConfig, id: &str) -> &mut ScratchState {
        let e = self
            .state
            .repo_mut(&repo.name)
            .scratch
            .entry(id.to_string())
            .or_default();
        e.id = id.to_string();
        e
    }

    /// Where a scratch session is, for what it is told.
    fn scratch_place<'a>(
        repo: &'a RepoConfig,
        session: &'a str,
        st: &'a ScratchState,
    ) -> prompt::ScratchPlace<'a> {
        prompt::ScratchPlace {
            session,
            repo: &repo.name,
            owner_login: st.owner_login.as_deref(),
            branch: st.branch.as_deref(),
            path: st.worktree_path.as_deref(),
        }
    }

    /// `ssf scratch create`: a new scratch session on `repo`, on the stack
    /// named, in a worktree of its own cut from the default branch. Shared
    /// without `owner_login`, that person's with it.
    pub(in crate::engine) async fn create_scratch(
        &mut self,
        repo: &str,
        harness: &str,
        model: Option<&str>,
        effort: Option<&str>,
        owner_login: Option<&str>,
    ) -> Result<Value> {
        let repo = self
            .cfg
            .repos
            .iter()
            .find(|r| r.matches_name(repo))
            .cloned()
            .ok_or_else(|| {
                Refused::bad_input(format!(
                    "{repo} is not a watched repository (see `ssf repo list`)"
                ))
            })?;
        let harness = harness.trim();
        self.check_stack(harness, model, effort).await?;
        let stack = Overrides {
            harness: harness.to_string(),
            model: model.map(|m| m.trim().to_string()),
            effort: effort.map(|e| e.trim().to_string()),
        };
        let eff = repo.with_overrides(Some(&stack));
        self.check_auto_compaction(&eff)?;
        let owner_login = owner_login
            .map(|l| l.trim().trim_start_matches('@').to_string())
            .filter(|l| !l.is_empty());
        let taken: BTreeSet<String> = self
            .state
            .repos
            .get(&repo.name)
            .map(|rs| rs.scratch.keys().cloned().collect())
            .unwrap_or_default();
        let id = new_scratch_id(|id| taken.contains(id));
        let session = Scratch {
            repo: repo.name.clone(),
            id: id.clone(),
        }
        .to_string();
        let (owner, name) = repo.split()?;
        let setup = self
            .driver(&repo)
            .ensure_project(
                owner,
                name,
                &repo.clone_url(),
                repo.path.as_deref(),
                &self.cfg.projects_dir(self.cfg.driver_for(&repo)),
            )
            .await?;
        let wt = self
            .driver(&repo)
            .create_worktree(
                &setup.repo_id,
                &repo.name,
                &format!("{SCRATCH_PREFIX}{id}"),
                NO_ITEM,
                &format!("ssf: scratch session {session}"),
                repo.base_branch.as_deref(),
            )
            .await?;
        info!(session, worktree = wt.id, "created scratch workspace");
        {
            let e = self.scratch_entry(&repo, &id);
            e.owner_login = owner_login.clone();
            e.stack = stack.clone();
            e.created_at = now_iso();
            e.repo_id = Some(setup.repo_id.clone());
            e.worktree_id = Some(wt.id.clone());
            e.worktree_path = Some(wt.path.clone());
            e.branch = wt.branch.clone();
            e.launched_at = Some(now_iso());
        }
        // The record comes first, so a harness that will not start leaves a
        // workspace ssf knows about (and `ssf release` can remove).
        self.state.save().context("recording the scratch session")?;
        let mailbox = crate::delivery_channel::scratch_mailbox(&repo.name, &id);
        crate::codex_delivery::retire_binding(&mailbox)
            .context("retiring native binding before a fresh conversation")?;
        let st = self.scratch_entry(&repo, &id).clone();
        let text = prompt::scratch_prompt(&Self::scratch_place(&repo, &session, &st));
        let tokens = self.cfg.auto_compaction_tokens_for(&eff);
        let cmd = self.scratch_launch_command(
            &repo,
            &session,
            &eff.harness_command(tokens),
            &eff.stack(),
            tokens,
        );
        let title = format!("{} · ~{id}", eff.harness);
        let handle = self
            .driver(&repo)
            .start(&wt.id, &cmd, &title, &eff.harness, &text)
            .await?;
        let _ = self.driver(&repo).set_status(&wt.id, "in-progress").await;
        let e = self.scratch_entry(&repo, &id);
        e.terminal_handle = Some(handle);
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        info!(session, harness = eff.harness, "started scratch session");
        Ok(serde_json::json!({
            "session": session,
            "repo": repo.name,
            "owner_login": owner_login,
            "harness": stack.harness,
            "model": stack.model,
            "effort": stack.effort,
            "branch": wt.branch,
            "path": wt.path,
        }))
    }

    /// Deliver `text` to a scratch session's agent, starting its harness
    /// again (resuming its conversation when one was captured, else fresh
    /// with `fresh` in place of `text` when given) if it is gone. The
    /// workspace has to be there: a killed session is only brought back by
    /// `ssf scratch resume`.
    pub(in crate::engine) async fn deliver_scratch(
        &mut self,
        repo: &RepoConfig,
        id: &str,
        text: &str,
        fresh: Option<&str>,
    ) -> Result<Delivery> {
        let st = self.scratch_entry(repo, id).clone();
        let session = Scratch {
            repo: repo.name.clone(),
            id: id.to_string(),
        }
        .to_string();
        let Some(wid) = st.worktree_id.clone() else {
            anyhow::bail!(Refused::conflict(format!(
                "{session} has no workspace (it was released); `ssf scratch resume {session}` \
brings it back"
            )));
        };
        // A kill is under way: the workspace is about to go, and nothing is
        // started in it again.
        if st.release_pending {
            anyhow::bail!(Refused::conflict(format!(
                "{session} is being released; `ssf scratch resume {session}` brings it back \
once it has been"
            )));
        }
        if !self.driver(repo).worktree_exists(&wid).await? {
            anyhow::bail!(Refused::conflict(format!(
                "{session}'s workspace is gone; `ssf scratch resume {session}` re-creates it"
            )));
        }
        let eff = repo.with_overrides(Some(&st.stack));
        let tokens = self.cfg.auto_compaction_tokens_for(&eff);
        let stack = eff.stack();
        let inner = eff.harness_command(tokens);
        let resume = st
            .agent_session_id
            .as_deref()
            .and_then(|c| sessions::resume_command(&eff.harness, &inner, c))
            .map(|c| self.scratch_launch_command(repo, &session, &c, &stack, tokens));
        let relaunch = self.scratch_launch_command(repo, &session, &inner, &stack, tokens);
        let title = format!("{} · ~{id}", eff.harness);
        let channel = self.driver(repo).channel_at(
            || crate::delivery_channel::scratch_mailbox(&repo.name, id),
            &eff.harness,
            st.prompts_sent + 1,
        );
        let d = self
            .driver(repo)
            .deliver(
                &wid,
                st.terminal_handle.as_deref(),
                Relaunch {
                    command: &relaunch,
                    resume_command: resume.as_deref(),
                    harness: &eff.harness,
                    title: &title,
                    text: fresh,
                    first_prompt: FirstPrompt::No,
                    channel: channel
                        .as_ref()
                        .map(|(mailbox, sequence)| (mailbox.as_path(), *sequence)),
                },
                text,
            )
            .await?;
        let e = self.scratch_entry(repo, id);
        if d.relaunched {
            e.launched_at = Some(now_iso());
            if !d.resumed {
                e.agent_session_id = None;
            }
            info!(session, resumed = d.resumed, "scratch harness relaunched");
        }
        e.terminal_handle = Some(d.handle.clone());
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        Ok(d)
    }

    /// `ssf scratch resume`: bring a killed scratch session back. The
    /// workspace is re-created on its old branch when that still exists
    /// (from the default branch otherwise) and the harness resumes the
    /// conversation it had. A session whose workspace is there and whose
    /// agent is gone is simply started again there.
    pub(in crate::engine) async fn resume_scratch(&mut self, session: &str) -> Result<Value> {
        let (repo, s, st) = self.scratch(session)?;
        let session = s.to_string();
        if st.release_pending {
            anyhow::bail!(Refused::conflict(format!(
                "{session}: a release of its workspace is pending; resume it once the pass has \
removed the workspace"
            )));
        }
        let alive = match st.worktree_id.as_deref() {
            Some(id) => self.driver(&repo).worktree_exists(id).await?,
            None => false,
        };
        if alive
            && let Some(id) = st.worktree_id.as_deref()
            && self.driver(&repo).has_live_agent(id).await?
        {
            anyhow::bail!(Refused::conflict(format!(
                "{session} is already running; nothing to resume"
            )));
        }
        let why = if alive {
            "stopped"
        } else {
            let repo_id = match st.repo_id.clone() {
                Some(r) => r,
                None => {
                    let (owner, name) = repo.split()?;
                    self.driver(&repo)
                        .ensure_project(
                            owner,
                            name,
                            &repo.clone_url(),
                            repo.path.as_deref(),
                            &self.cfg.projects_dir(self.cfg.driver_for(&repo)),
                        )
                        .await?
                        .repo_id
                }
            };
            let name = format!("{SCRATCH_PREFIX}{}", s.id);
            let branch = crate::driver::branch_for(&name);
            // The old branch when it is still there, so the work pushed or
            // committed on it comes back; the configured base otherwise.
            let base = match self
                .driver(&repo)
                .existing_branch_ref(&repo_id, &branch)
                .await
            {
                Ok(Some(r)) => Some(r),
                Ok(None) => repo.base_branch.clone(),
                Err(e) => {
                    warn!(session, "could not check the old branch: {e:#}");
                    repo.base_branch.clone()
                }
            };
            let wt = self
                .driver(&repo)
                .create_worktree(
                    &repo_id,
                    &repo.name,
                    &name,
                    NO_ITEM,
                    &format!("ssf: scratch session {session} (resumed)"),
                    base.as_deref(),
                )
                .await?;
            info!(session, worktree = wt.id, "re-created scratch workspace");
            let e = self.scratch_entry(&repo, &s.id);
            e.repo_id = Some(repo_id);
            e.worktree_id = Some(wt.id.clone());
            e.worktree_path = Some(wt.path.clone());
            e.branch = wt.branch.clone();
            e.terminal_handle = None;
            e.released_at = None;
            self.state
                .save()
                .context("recording the scratch workspace")?;
            "killed"
        };
        let st = self.scratch_entry(&repo, &s.id).clone();
        let place = Self::scratch_place(&repo, &session, &st);
        let text = prompt::scratch_restarted_prompt(&place, why);
        let fresh = format!("{}\n\n{text}", prompt::scratch_prompt(&place));
        let d = self
            .deliver_scratch(&repo, &s.id, &text, Some(&fresh))
            .await?;
        let st = self.scratch_entry(&repo, &s.id).clone();
        Ok(serde_json::json!({
            "session": session,
            "resumed": d.resumed,
            "recreated": why == "killed",
            "branch": st.branch,
            "path": st.worktree_path,
        }))
    }

    /// `ssf release` of a scratch session: kill it. Its workspace goes on
    /// the next pass if the release checks pass now (and again then);
    /// `force` skips them. The record, its branch and its conversation stay
    /// for `ssf scratch resume`.
    pub(in crate::engine) async fn release_scratch(
        &mut self,
        session: &str,
        force: bool,
    ) -> Result<Value> {
        let (repo, s, _) = self.scratch(session)?;
        // The conversation id is what a resume needs: capture it now, while
        // the workspace path is still recorded, rather than trust a pass to
        // have got to it first.
        self.capture_scratch(&repo);
        let st = self.scratch_entry(&repo, &s.id).clone();
        let id = s.to_string();
        let title = st.title();
        let Some(wid) = st.worktree_id.clone() else {
            anyhow::bail!(Refused::conflict(format!(
                "{id} has no workspace (already released)"
            )));
        };
        if !self
            .driver(&repo)
            .worktree_exists(&wid)
            .await
            .unwrap_or(true)
        {
            self.mark_scratch_released(&repo, &s.id);
            return Ok(serde_json::json!({
                "session": id, "title": title, "released": true, "already_gone": true,
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
                "session": id, "title": title, "path": path, "released": false, "check": check,
            }));
        }
        let e = self.scratch_entry(&repo, &s.id);
        e.release_pending = true;
        e.release_forced = force;
        info!(session = id, forced = force, "scratch release accepted");
        Ok(serde_json::json!({
            "session": id, "title": title, "path": path, "released": true,
            "forced": force, "pending": true, "check": check,
            "poll_interval_secs": self.cfg.daemon.poll_interval_secs,
        }))
    }

    /// The pass's half of a scratch release: the checks once more (unless
    /// forced), then the workspace goes.
    pub(in crate::engine) async fn run_scratch_cleanups(&mut self, repo: &RepoConfig) {
        let pending: Vec<ScratchState> = match self.state.repos.get(&repo.name) {
            Some(rs) => rs
                .scratch
                .values()
                .filter(|s| s.release_pending)
                .cloned()
                .collect(),
            None => return,
        };
        for st in pending {
            let session = Scratch {
                repo: repo.name.clone(),
                id: st.id.clone(),
            }
            .to_string();
            let Some(wid) = st.worktree_id.clone() else {
                self.mark_scratch_released(repo, &st.id);
                continue;
            };
            if !self
                .driver(repo)
                .worktree_exists(&wid)
                .await
                .unwrap_or(true)
            {
                info!(session, "scratch workspace is already gone");
                self.mark_scratch_released(repo, &st.id);
                continue;
            }
            if !st.release_forced {
                let problems = match st.worktree_path.as_deref() {
                    Some(path) => match release::inspect(path).await {
                        Ok(c) => c.problems(),
                        Err(e) => vec![format!("{e:#}")],
                    },
                    None => vec!["no workspace path recorded".into()],
                };
                if !problems.is_empty() {
                    // The session carried on after the checks passed: it is
                    // kept, and a person asks again (or forces it).
                    warn!(
                        session,
                        ?problems,
                        "scratch release refused: the workspace changed since the checks passed"
                    );
                    let e = self.scratch_entry(repo, &st.id);
                    e.release_pending = false;
                    e.release_forced = false;
                    continue;
                }
            }
            match self.driver(repo).remove_worktree(&wid).await {
                Ok(()) => {
                    info!(
                        session,
                        worktree = wid,
                        forced = st.release_forced,
                        "released the scratch workspace"
                    );
                    self.mark_scratch_released(repo, &st.id);
                }
                Err(e) => {
                    warn!(session, "removing the scratch workspace failed: {e:#}");
                    let e = self.scratch_entry(repo, &st.id);
                    e.release_pending = false;
                    e.release_forced = false;
                }
            }
        }
    }

    /// A scratch session's workspace is gone by our hand. Its branch and
    /// conversation stay on the record for `ssf scratch resume`.
    fn mark_scratch_released(&mut self, repo: &RepoConfig, id: &str) {
        let e = self.scratch_entry(repo, id);
        e.release_pending = false;
        e.release_forced = false;
        e.worktree_id = None;
        e.worktree_path = None;
        e.terminal_handle = None;
        e.released_at = Some(now_iso());
    }

    /// Record the harness conversation of scratch sessions that do not
    /// have one yet (see `capture_sessions`).
    pub(in crate::engine) fn capture_scratch(&mut self, repo: &RepoConfig) {
        let Some(rs) = self.state.repos.get_mut(&repo.name) else {
            return;
        };
        for st in rs.scratch.values_mut() {
            let harness = repo.with_overrides(Some(&st.stack)).harness;
            if st.agent_session_id.is_some() || !sessions::reads_transcript(&harness) {
                continue;
            }
            let (Some(path), Some(launched)) =
                (st.worktree_path.as_deref(), st.launched_at.as_deref())
            else {
                continue;
            };
            if let Some(id) = sessions::capture(&harness, path, capture_since(launched, None), &[])
            {
                info!(
                    repo = repo.name,
                    scratch = st.id,
                    session = id,
                    "captured {harness} session"
                );
                st.agent_session_id = Some(id);
            }
        }
    }

    /// The startup pass for scratch sessions: one whose workspace is there
    /// and whose agent is gone is started again, as an item's session is.
    pub(in crate::engine) async fn resume_scratch_sessions(&mut self, repo: &RepoConfig) {
        let candidates: Vec<ScratchState> = match self.state.repos.get(&repo.name) {
            Some(rs) => rs
                .scratch
                .values()
                .filter(|s| s.worktree_id.is_some() && !s.release_pending)
                .cloned()
                .collect(),
            None => return,
        };
        for st in candidates {
            let session = Scratch {
                repo: repo.name.clone(),
                id: st.id.clone(),
            }
            .to_string();
            let Some(wid) = st.worktree_id.as_deref() else {
                continue;
            };
            match self.driver(repo).worktree_exists(wid).await {
                Ok(true) => {}
                Ok(false) => {
                    debug!(
                        session,
                        "scratch workspace is gone; left for `ssf scratch resume`"
                    );
                    continue;
                }
                Err(e) => {
                    warn!(session, "could not check the workspace: {e:#}");
                    continue;
                }
            }
            match self.driver(repo).has_live_agent(wid).await {
                Ok(false) => {}
                Ok(true) => continue,
                Err(e) => {
                    warn!(session, "could not list the workspace's terminals: {e:#}");
                    continue;
                }
            }
            let place = Self::scratch_place(repo, &session, &st);
            let text = prompt::scratch_restarted_prompt(
                &place,
                "interrupted by a factory restart (the machine, the multiplexer or ssf itself)",
            );
            let fresh = format!("{}\n\n{text}", prompt::scratch_prompt(&place));
            info!(
                session,
                "scratch session was interrupted; starting it again"
            );
            if let Err(e) = self
                .deliver_scratch(repo, &st.id, &text, Some(&fresh))
                .await
            {
                warn!(session, "could not start the scratch session again: {e:#}");
            }
            if let Err(e) = self.state.save() {
                tracing::error!("saving state: {e:#}");
            }
        }
    }
}
