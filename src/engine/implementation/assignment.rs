use super::super::*;
use tracing::info;

impl Engine {
    /// `ssf assign`: assign the bot to an item on GitHub and write the
    /// item's launch overrides, so the session that onboards it comes up
    /// on that stack instead of the repository's. The inverse of
    /// `ssf handover`: this one wants an item with no session, that one an
    /// item with one, and neither accepts what the other does.
    ///
    /// The GitHub assignment and the override write are one request
    /// because the daemon answers the CLI between polls (`serve`): an
    /// override written from the CLI could land after a pass that had
    /// already onboarded the item, which starts it on the old stack.
    pub(in crate::engine) async fn assign(
        &mut self,
        item: &str,
        harness: &str,
        model: Option<&str>,
        effort: Option<&str>,
        by: Option<&str>,
    ) -> Result<Value> {
        let (repo, number) = self.locate(item)?;
        let id = session_id(&repo.name, number);
        let harness = harness.trim();
        let st = self.peek(&repo, number).cloned();
        // A seat on the item is a refusal and not a silent no-op: an item
        // with a session is `ssf handover`'s to move, and an item whose
        // session is committed elsewhere (a handover or a release on its
        // way, a binding to another item's workspace) is the one the other
        // command acts on.
        if let Some(h) = st.as_ref().and_then(|s| s.handover.as_ref()) {
            anyhow::bail!(
                "{id}: a handover to {} is already pending; it is carried out on the next pass, \
or called off with `ssf handover {id} --cancel`",
                h.harness
            );
        }
        if st.as_ref().is_some_and(|s| s.release_pending) {
            anyhow::bail!(
                "{id}: a release of this item's workspace is pending; assign it again once the \
pass has removed the workspace"
            );
        }
        // An item bound to another item's session runs that session's
        // stack (`overrides_of` reads the owner's record), so overrides
        // written here would be inert.
        let bound = |owner: u64| {
            let owner = session_id(&repo.name, owner);
            anyhow::anyhow!(
                "{id} is worked by {owner} and shares its workspace, so it runs that session's \
stack; hand that one over instead: `ssf handover {owner} --harness {harness}`"
            )
        };
        let recorded = self.owner_of(&repo, number);
        if recorded != number {
            return Err(bound(recorded));
        }
        // A retired item is not a seat: nothing is running, and it is
        // exactly where a first session on a chosen stack comes from --
        // the assignment brings the item back, and it reuses the workspace
        // it kept, or the one its branch is re-created from. A session
        // that is there is `ssf handover`'s to move, and no login probe is
        // worth running for an item this refuses.
        if st
            .as_ref()
            .is_some_and(|s| s.active && s.worktree_id.is_some())
        {
            anyhow::bail!(
                "{id} already has a session; `ssf handover {id} --harness {harness}` moves that \
session to another stack"
            );
        }
        self.check_stack(harness, model, effort).await?;
        let (owner, name) = repo.split()?;
        let issue = self.gh.issue(owner, name, number).await?;
        // A pull request is also bound by what the daemon reads off it when
        // it onboards (an origin tag naming another item, or another
        // session's branch), which its record does not have until then.
        if let Some(o) = self.bound_in_github(&repo, number, &issue).await? {
            return Err(bound(o));
        }
        let overrides = Overrides {
            harness: harness.to_string(),
            model: model.map(|m| m.trim().to_string()),
            effort: effort.map(|e| e.trim().to_string()),
        };
        let from = self.effective(&repo, number);
        let to = repo.with_overrides(Some(&overrides));
        // The same comparison `ssf handover` refuses on, except that
        // asking for the stack the item already runs is allowed here (it
        // is where an unassigned item starts) and writes nothing: an
        // override nobody needs would pin the item out of `ssf repo set`.
        let pin = to.harness != from.harness || to.model != from.model || to.effort != from.effort;
        // The assignment lands first: a GitHub call that fails leaves the
        // item exactly as it was, overrides included.
        let assigned = !issue.is_assigned_to(&self.login);
        if assigned {
            self.gh
                .add_assignee(owner, name, number, &self.login)
                .await?;
        }
        if pin {
            // A conversation the record still carries is the old
            // harness's: the resume path would hand its id to the new one
            // (`deliver_to`), so it is dropped and remembered the way a
            // handover drops it. The workspace's last transcript goes with
            // it, for a session ssf never captured an id for.
            if harness != from.harness
                && let Some(st) = st.as_ref()
                && (st.agent_session_id.is_some() || st.worktree_id.is_some())
            {
                let newest = st
                    .worktree_path
                    .as_deref()
                    .filter(|_| st.worktree_id.is_some())
                    .and_then(|p| sessions::capture(&from.harness, p, SystemTime::UNIX_EPOCH, &[]));
                let retired = retired_conversations(st.agent_session_id.clone(), newest);
                let e = self.entry(&repo, number);
                e.agent_session_id = None;
                retire(e, &retired);
            }
            self.entry(&repo, number).overrides = Some(overrides);
        }
        info!(
            session = id,
            harness,
            model,
            effort,
            assigned,
            overrides = pin,
            by,
            "assigned to the item"
        );
        // The command is on both sides because it is what decides an unset
        // model or effort, as it is for a handover's `handed-over` post.
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
            "title": issue.title,
            "from": launch(&from),
            "to": launch(&to),
            "assigned": assigned,
            "overrides_written": pin,
            "poll_interval_secs": self.cfg.daemon.poll_interval_secs,
        }))
    }

    /// The session `issue` belongs to when it is not its own, read off the
    /// item as an onboarding would: `find_owner` over the origin tag in
    /// its body and timeline and over the branch of a pull request. `None`
    /// for an item that gets a session of its own -- a `mode=delegate`
    /// item among them, since that one was handed off to be worked.
    async fn bound_in_github(
        &self,
        repo: &RepoConfig,
        number: u64,
        issue: &Issue,
    ) -> Result<Option<u64>> {
        // Only a pull request is bound by anything but its record: an
        // issue gets a session of its own however it was opened.
        if !issue.is_pull_request() {
            return Ok(None);
        }
        let (owner, name) = repo.split()?;
        let timeline = self.gh.timeline(owner, name, number).await?;
        let pr = self.gh.pull(owner, name, number).await?;
        let scan = origin::scan(issue, &timeline, &self.login);
        Ok(self.find_owner(repo, issue, Some(&pr), &scan))
    }
}
