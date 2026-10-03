use super::super::*;
use crate::ipc::Refused;
use crate::state::Parked;

impl Engine {
    pub(in crate::engine) async fn park(
        &mut self,
        session: &str,
        blocker: &str,
        next_action_owner: &str,
    ) -> Result<Value> {
        let (repo, number, id) = self.known_session(session)?;
        if blocker.trim().is_empty() || next_action_owner.trim().is_empty() {
            anyhow::bail!(Refused::bad_input(
                "provide a concrete blocker and next-action owner"
            ));
        }
        let st = self.entry(&repo, number).clone();
        if st.release_pending || st.cleanup_pending || st.handover.is_some() || st.blocked.is_some()
        {
            anyhow::bail!(Refused::conflict(
                "release, handover or blocked-agent transition pending; reach an idle safe boundary first"
            ));
        }
        let wt = st
            .worktree_id
            .as_deref()
            .context("the session has no retained workspace")?;
        if !self.refresh_workspaces(&repo).await {
            anyhow::bail!(
                "{id}: cannot establish a safe idle boundary while the driver is unavailable"
            );
        }
        let live = self
            .driver(&repo)
            .live_handle(wt, st.terminal_handle.as_deref())
            .await?;
        if let Some(handle) = live.as_deref() {
            let ws = self
                .workspaces_of(&repo)
                .iter()
                .find(|w| w.worktree_id == wt)
                .context("the live workspace is missing from the driver's fresh report")?;
            if ws.agents.len() != 1
                || ws.is_working()
                || !ws
                    .agents
                    .iter()
                    .all(|a| matches!(a.state.as_str(), "idle" | "done" | "open") && !a.interrupted)
            {
                anyhow::bail!(Refused::conflict(
                    "the agent is working, blocked or unknown; reach an idle safe boundary before parking"
                ));
            }
            if self.driver(&repo).at_question(handle).await? {
                anyhow::bail!(Refused::conflict(
                    "the agent is at an approval or question; parking does not answer dialogs"
                ));
            }
            let screen = self.driver(&repo).screen(handle).await?.join("\n");
            if screen.trim().is_empty() {
                anyhow::bail!(Refused::conflict(
                    "the idle agent has no readable composer; reach a confirmed idle boundary first"
                ));
            }
            let harness = self.live_harness(&repo, number);
            if crate::driver::blocking_dialog(&harness, &screen).is_some()
                || crate::driver::trust_dialog(&screen).is_some()
            {
                anyhow::bail!(Refused::conflict(
                    "the agent is at a blocking dialog; reach an idle safe boundary first"
                ));
            }
        }
        let mailbox = crate::delivery_channel::mailbox(&repo.name, number);
        if pending_delivery(&mailbox)? {
            anyhow::bail!(Refused::conflict(
                "the session has an unacknowledged delivery; let it reach a recorded idle boundary first"
            ));
        }
        self.capture_sessions(&repo);
        let st = self.entry(&repo, number).clone();
        let stack = st
            .parked
            .as_ref()
            .map(|p| p.stack.clone())
            .unwrap_or_else(|| {
                let current = self.live_config(&repo, number);
                Overrides {
                    harness: current.harness,
                    model: current.model,
                    effort: current.effort,
                }
            });
        let conversation = st
            .agent_session_id
            .as_deref()
            .context("no resumable conversation captured; parking would lose the conversation")?;
        let eff = repo.with_overrides(Some(&stack));
        if sessions::resume_command(
            &eff.harness,
            &eff.harness_command(self.cfg.auto_compaction_tokens_for(&eff)),
            conversation,
        )
        .is_none()
        {
            anyhow::bail!(Refused::conflict(
                "this harness cannot resume its conversation; parking refused"
            ));
        }
        self.entry(&repo, number).parked = Some(Parked {
            since: st
                .parked
                .as_ref()
                .map(|p| p.since.clone())
                .unwrap_or_else(now_iso),
            blocker: blocker.trim().into(),
            next_action_owner: next_action_owner.trim().into(),
            stack,
        });
        // Hold must be durable before stopping. Even a crash or failed stop
        // leaves ordinary activity unable to relaunch this session.
        if let Err(e) = self.persist() {
            self.entry(&repo, number).parked = st.parked;
            return Err(e);
        }
        if let Some(handle) = live {
            self.driver(&repo)
                .stop_agent(wt, &handle)
                .await
                .context("parking hold saved, but stopping failed; session remains parked")?;
        }
        self.entry(&repo, number).terminal_handle = None;
        self.persist()?;
        Ok(serde_json::json!({"session": id, "parked": self.entry(&repo, number).parked}))
    }

    pub(in crate::engine) async fn resume_parked(&mut self, session: &str) -> Result<Value> {
        let (repo, number, id) = self.known_session(session)?;
        let st = self.entry(&repo, number).clone();
        let parked = st.parked.as_ref().context("the session is not parked")?;
        if st.release_pending || st.handover.is_some() || st.blocked.is_some() {
            anyhow::bail!(Refused::conflict(
                "release, handover or blocked-agent transition pending"
            ));
        }
        let path = st
            .worktree_path
            .as_deref()
            .context("the retained checkout path is missing")?;
        // Never recreate from a base branch: unpublished work must be here.
        if !Path::new(path).join(".git").exists() {
            anyhow::bail!("{id}: retained checkout is missing; keeping the session parked");
        }
        let branch = git(path, &["symbolic-ref", "--quiet", "HEAD"])
            .await
            .context("cannot verify the retained checkout branch; keeping the session parked")?;
        if st
            .branch
            .as_deref()
            .map(|b| b.strip_prefix("refs/heads/").unwrap_or(b))
            != Some(
                branch
                    .trim()
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch.trim()),
            )
        {
            anyhow::bail!(
                "{id}: retained checkout branch differs from the parked branch; keeping the session parked"
            );
        }
        let mut wt = st
            .worktree_id
            .clone()
            .context("the retained workspace identity is missing")?;
        if self
            .driver(&repo)
            .live_handle(&wt, st.terminal_handle.as_deref())
            .await?
            .is_some()
        {
            anyhow::bail!(Refused::conflict(
                "a live agent remains in the parked workspace; stop it at a safe boundary before resume"
            ));
        }
        if !self.driver(&repo).worktree_exists(&wt).await? {
            let repo_id = st
                .repo_id
                .as_deref()
                .context("the retained repository identity is missing")?;
            wt = self
                .driver(&repo)
                .reopen_worktree(repo_id, &repo.name, number, path)
                .await?;
            self.entry(&repo, number).worktree_id = Some(wt.clone());
            self.persist()?;
        }
        if self.driver(&repo).live_handle(&wt, None).await?.is_some() {
            anyhow::bail!(Refused::conflict(
                "a live agent owns the retained checkout; keeping the session parked"
            ));
        }
        let eff = repo.with_overrides(Some(&parked.stack));
        let tokens = self.cfg.auto_compaction_tokens_for(&eff);
        let conversation = st
            .agent_session_id
            .as_deref()
            .context("the parked conversation identity is missing")?;
        let resume =
            sessions::resume_command(&eff.harness, &eff.harness_command(tokens), conversation)
                .context("the parked harness cannot resume its conversation")?;
        let command = self.launch_command(
            &repo,
            number,
            &st.html_url,
            &resume,
            Some(&eff.stack()),
            tokens,
        );
        // Start only the original conversation; no fallback to a fresh one.
        // Keep the hold until launch succeeds, including an interrupted request.
        let text = format!(
            "[ssf] Explicitly resumed {id} after parking for {} (next action: {}). Read the issue and its owned PRs for current completion and pending activity before continuing.",
            parked.blocker, parked.next_action_owner
        );
        let handle = self
            .driver(&repo)
            .start_parked(
                &wt,
                &command,
                &format!("{} · #{number}", eff.harness),
                &eff.harness,
                &text,
            )
            .await?;
        self.entry(&repo, number).terminal_handle = Some(handle);
        self.record_launch(&repo, number, &eff);
        self.entry(&repo, number).parked = None;
        // Listings may have answered 304 while parked; force a fresh look so
        // their held activity reaches this conversation after explicit resume.
        let rs = self.state.repo_mut(&repo.name);
        rs.issues_etag = None;
        rs.mentioned_etag = None;
        rs.pulls_etag = None;
        rs.created_etag = None;
        if let Err(e) = self.persist() {
            self.entry(&repo, number).parked = Some(parked.clone());
            return Err(e);
        }
        Ok(
            serde_json::json!({"session": id, "resumed": true, "worktree_path": path, "branch": st.branch, "agent_session_id": conversation}),
        )
    }
}

/// Unrecorded mailbox events must not be stranded or injected after parking.
fn pending_delivery(path: &Path) -> Result<bool> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // Native routing bindings and confirmed native journals are retained
        // evidence, not unrecorded events.
        if matches!(
            name.as_str(),
            "codex-binding.json" | "ready.json" | "bridge.json"
        ) || name.starts_with("codex-binding-retired-")
        {
            continue;
        }
        if (name.starts_with("codex-") || name.starts_with("claude-")) && name.ends_with(".json") {
            let journal: Value = serde_json::from_slice(&std::fs::read(entry.path())?)?;
            if journal["confirmed"] == true {
                continue;
            }
            return Ok(true);
        }
        if name.ends_with(".json")
            || name.ends_with(".json.handed")
            || name.ends_with(".json.claimed")
        {
            return Ok(true);
        }
    }
    Ok(false)
}
