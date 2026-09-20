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
    /// state locations and selected target are passed along so the wrapper
    /// reads the same factory, and the VM guest flag so `ssf guide` in the
    /// session knows where it is. `stack` is what the session is being
    /// started with, which the wrapper exports for the byline; the stack a
    /// resume is launched with is the same one, since a resume changes the
    /// conversation and not the harness, model or effort.
    pub(in crate::engine) fn launch_command(
        &self,
        repo: &RepoConfig,
        number: u64,
        url: &str,
        inner: &str,
        stack: Option<&Stack>,
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
        let server =
            launch_server_argument(crate::server_catalog::selected_target_name().as_deref());
        let wrapper = format!("{prefix}{}{server}", shell_quote(&me));
        render_launch_command(&wrapper, repo, number, url, inner, stack)
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
        let first_prompt =
            crate::driver::FirstPrompt::for_state(st.seeded, st.first_prompt_attempted);
        if first_prompt == crate::driver::FirstPrompt::Send {
            self.entry(repo, target).first_prompt_attempted = true;
            self.state
                .save()
                .context("recording the first prompt before delivery")?;
        }
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
        // What the session is started with, decided here: the story below
        // is told to the harness this launches, and the relaunch, the title
        // and the resume all use it.
        let eff = self.effective(repo, target);
        let mut story = None;
        // A handover whose new session never came up left its summary on
        // the item: the harness started here is the one that takes it on.
        let mut note_given = false;
        if !live && (target != number || relaunch_text.is_none()) {
            let owed = self
                .peek(repo, target)
                .is_some_and(|s| s.handover_note.is_some());
            // The harness about to start, not the one a pane is showing:
            // this message goes to a session that is not there yet, and it
            // reads the guidance of the harness it is on.
            match self
                .first_message(repo, target, Some(eff.harness.as_str()))
                .await
            {
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
        let title = format!("{} · #{target}", eff.harness);
        // A resume changes the conversation, not the stack: both are
        // launched with what the item runs, which is what the byline of
        // everything the session posts will say.
        let stack = eff.stack();
        let resume = st
            .agent_session_id
            .as_deref()
            .and_then(|id| sessions::resume_command(&eff.harness, &eff.harness_command(), id))
            .map(|c| self.launch_command(repo, st.number, &st.html_url, &c, Some(&stack)));
        let relaunch = self.launch_command(
            repo,
            st.number,
            &st.html_url,
            &eff.harness_command(),
            Some(&stack),
        );
        let channel = self.driver(repo).delivery_channel(
            &repo.name,
            target,
            &eff.harness,
            st.prompts_sent + 1,
        );
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
                    first_prompt,
                    channel: channel
                        .as_ref()
                        .map(|(mailbox, sequence)| (mailbox.as_path(), *sequence)),
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
            && let Some((reason, detail)) =
                crate::driver::blocking_dialog(&eff.harness, &screen.join("\n"))
        {
            // The message that carried the note went into a sign-in
            // screen, so no session has read it: it waits on the item for
            // the start that gets through.
            if let Some(note) = spent_note {
                self.entry(repo, target).handover_note = Some(note);
            }
            let b = self
                .set_blocked_for(repo, target, &eff.harness, reason, detail)
                .await;
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
        Ok(repo_id)
    }

    /// Re-create the workspace for an issue whose worktree is gone,
    /// starting from its old branch when that still exists. Says why it
    /// was re-created (`workspace gone`), or `None` when the driver already
    /// had a workspace linked to the issue and nothing was made.
    pub(in crate::engine) async fn rehydrate(
        &mut self,
        repo: &RepoConfig,
        number: u64,
    ) -> Result<Option<&'static str>> {
        let repo_id = self.repo_id_for(repo, number).await?;
        let st = self.entry(repo, number).clone();
        if let Some(existing) = self
            .driver(repo)
            .find_worktree_for_issue(&repo_id, &repo.name, number)
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
            .create_worktree(
                &repo_id,
                &repo.name,
                &name,
                number,
                &comment,
                base.as_deref(),
            )
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
                        &repo.name,
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
        Ok(Some("workspace gone"))
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
        // The id recorded here is the *configured* harness's own
        // conversation (the capture reads that harness's transcripts), and
        // a relaunch resumes it with the same harness, so a session left on
        // another harness by a config edit records nothing: its transcripts
        // are not the ones this harness would start again from.
        let harnesses: Vec<(u64, String)> = candidates
            .into_iter()
            .filter_map(|n| {
                let harness = self.effective(repo, n).harness;
                match self.running_harness(repo, n) {
                    Some(running) if running != harness => None,
                    _ => Some((n, harness)),
                }
            })
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
            Err(e) if is_held(&e) => debug!(session, "refusal not taken yet: {e:#}"),
            Err(e) => warn!(
                session,
                "could not tell the agent about the refused release: {e:#}"
            ),
        }
    }

    /// The workspace of `number`'s session is gone by our hand: forget its
    /// bindings (the next event re-creates it) and record when. The item's
    /// own stack, and the conversation captured for it, stay on the
    /// record: the workspace that is gone is not the item.
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
        // The item keeps its launch overrides: they are the item's stack,
        // not the workspace's, and only a later command that writes one
        // (`ssf handover`, or `ssf assign` where the item has no session)
        // changes them. Releasing the workspace is routine -- every close
        // asks the agent for one -- and dropping the stack here would put
        // an item pinned to a chosen harness back on the repository's the
        // first time it is re-created, taking the captured conversation
        // with it where the two harnesses differ.
        // `handed_over_at` stays with them: it is what tells a handover's
        // overrides from an assignment's, and the transcripts it guards for
        // `capture_sessions` outlive the workspace -- a harness keeps them
        // under its own directory, keyed by the workspace's path -- so the
        // floor it puts on the capture window still means something when a
        // workspace of the same name is re-created.
        e.handover = None;
        e.handover_note = None;
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

fn launch_server_argument(server: Option<&str>) -> String {
    server
        .map(|name| format!(" --server {}", shell_quote(name)))
        .unwrap_or_default()
}

fn render_launch_command(
    wrapper: &str,
    repo: &RepoConfig,
    number: u64,
    url: &str,
    inner: &str,
    stack: Option<&Stack>,
) -> String {
    // What the session runs, for the byline of everything it posts. Naming
    // only the parts that are set keeps `ssf launch`'s reading of an unset
    // model or effort the harness's own.
    let mut flags = String::new();
    if let Some(stack) = stack {
        flags.push_str(&format!(" --harness {}", shell_quote(&stack.harness)));
        for (flag, value) in [("--model", &stack.model), ("--effort", &stack.effort)] {
            if let Some(value) = value {
                flags.push_str(&format!(" {flag} {}", shell_quote(value)));
            }
        }
    }
    format!(
        "{wrapper} launch --repo {} --issue {} --issue-url {}{flags} -- {}",
        shell_quote(&repo.name),
        number,
        shell_quote(url),
        shell_quote(inner)
    )
}

#[cfg(test)]
mod launch_command_tests {
    use super::{launch_server_argument, render_launch_command};
    use crate::origin::Stack;

    fn stack() -> Stack {
        Stack {
            harness: "claude".into(),
            model: Some("opus".into()),
            effort: Some("high".into()),
        }
    }

    /// The wrapper as `Engine::launch_command` builds it: the daemon's own
    /// directories, the executable, and the transport.
    fn wrapper(server: Option<&str>) -> String {
        format!("'/bin/ssf'{}", launch_server_argument(server))
    }

    #[test]
    fn selected_server_is_forwarded_to_the_launch_wrapper() {
        let repo = crate::config::RepoConfig {
            name: "owner/repo".into(),
            ..Default::default()
        };
        assert_eq!(
            render_launch_command(
                &wrapper(Some("local")),
                &repo,
                42,
                "https://example.test/owner/repo/issues/42",
                "agent --flag",
                None
            ),
            "'/bin/ssf' --server 'local' launch --repo 'owner/repo' --issue 42 --issue-url 'https://example.test/owner/repo/issues/42' -- 'agent --flag'"
        );
        assert_eq!(
            render_launch_command(
                &wrapper(None),
                &repo,
                42,
                "https://example.test/owner/repo/issues/42",
                "agent",
                None
            ),
            "'/bin/ssf' launch --repo 'owner/repo' --issue 42 --issue-url 'https://example.test/owner/repo/issues/42' -- 'agent'"
        );
    }

    /// The stack the session is started on travels to the wrapper, which
    /// exports it for the byline; only the parts that are set are named, so
    /// the wrapper reads an unset one as the harness's own default.
    #[test]
    fn the_launch_names_what_the_session_runs() {
        let repo = crate::config::RepoConfig {
            name: "owner/repo".into(),
            ..Default::default()
        };
        let line = |stack: Option<&Stack>| {
            render_launch_command(
                &wrapper(None),
                &repo,
                42,
                "https://example.test/owner/repo/issues/42",
                "agent",
                stack,
            )
        };
        assert!(line(Some(&stack())).contains(
            "launch --repo 'owner/repo' --issue 42 --issue-url \
'https://example.test/owner/repo/issues/42' --harness 'claude' --model 'opus' --effort 'high' -- 'agent'"
        ));
        let bare = Stack {
            harness: "omp".into(),
            model: None,
            effort: None,
        };
        assert!(line(Some(&bare)).contains("--harness 'omp' -- 'agent'"));
        assert!(!line(Some(&bare)).contains("--model"));
        assert!(!line(None).contains("--harness"));
    }
}
