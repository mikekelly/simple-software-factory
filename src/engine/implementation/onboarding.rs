use super::super::*;
use tracing::{debug, info, warn};

impl Engine {
    pub(in crate::engine) async fn onboard(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        issue: &Issue,
        pr: Option<PrInfo>,
        triggers: Vec<String>,
    ) -> Result<()> {
        let is_pr = issue.is_pull_request();
        info!(
            repo = repo.name,
            issue = issue.number,
            title = issue.title,
            pr = is_pr,
            ?triggers,
            "onboarding"
        );
        // Who asked comes first: an item nobody allowed asked for gets no
        // project, no workspace and no session.
        let timeline = self.gh.timeline(owner, name, issue.number).await?;
        if let Err(why) = self.gate(repo, issue, &timeline, &triggers) {
            self.refuse(repo, issue, &triggers, &why);
            return Ok(());
        }
        // Whatever it was ignored as before, it is being looked at afresh.
        self.state
            .repo_mut(&repo.name)
            .ignored
            .remove(&issue.number);
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
        let pr = match (is_pr, pr) {
            (true, Some(p)) => Some(p),
            (true, None) => Some(self.gh.pull(owner, name, issue.number).await?),
            (false, _) => None,
        };
        let diff = self.diff(repo, &BTreeMap::new(), &timeline);

        let prior = self
            .state
            .repo_mut(&repo.name)
            .issues
            .get(&issue.number)
            .cloned();
        let wt_name = prior
            .as_ref()
            .and_then(|s| s.worktree_name.clone())
            .unwrap_or_else(|| prompt::worktree_name_for(issue.number, &issue.title, is_pr));
        let driver = self.cfg.driver_for(repo);
        {
            let e = self.entry(repo, issue.number);
            e.title = issue.title.clone();
            e.html_url = issue.html_url.clone();
            e.repo_id = Some(setup.repo_id.clone());
            e.driver = Some(driver.id().into());
            e.worktree_name = Some(wt_name.clone());
            e.kind = Some(if is_pr {
                "pull_request".into()
            } else {
                "issue".into()
            });
            e.triggers = triggers.clone();
            e.github_state = Some(github_state(issue, pr.as_ref(), false));
            e.pr = pr.clone();
            e.shares_workspace_of = None;
            e.delegated_by = None;
            e.parent_notified = false;
            e.subscriber_only = false;
        }
        // Subscribers of an item that had no session until now hear that it
        // got one, with whatever happened since they last heard.
        let since_prior = prior
            .as_ref()
            .filter(|p| p.subscriber_only)
            .map(|p| self.diff(repo, &p.seen, &timeline).rendered);
        let scan = self.record_origins(repo, issue, &timeline);
        let by_bot = issue.author().eq_ignore_ascii_case(&self.login);

        // Who acts on this item. An item a session opened belongs to that
        // session (first binding wins: it never spawns a second one), unless
        // the session handed it off, in which case it gets its own session
        // and the parent hears about it once, when it closes.
        if let Some(tag) = scan
            .origin_tag
            .as_ref()
            .filter(|t| t.is_delegate() && by_bot)
        {
            info!(
                repo = repo.name,
                issue = issue.number,
                parent = %tag.origin,
                "handed off by a session; starting its own"
            );
            self.entry(repo, issue.number).delegated_by = Some(tag.origin.to_string());
            // The delegating parent follows its child.
            let parent = self.acting_session(&tag.origin.to_string());
            let e = self.entry(repo, issue.number);
            if !e
                .subscribers
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&parent))
            {
                e.subscribers.push(parent);
            }
        } else if let Some(owner) = self.find_owner(repo, issue, pr.as_ref(), &scan) {
            return self
                .bind_to(repo, issue, owner, diff, triggers, since_prior)
                .await;
        } else if triggers.iter().all(|t| t == "created") {
            // Opened by the bot, but from nowhere ssf can name and with no
            // human asking for it: not worth a session.
            info!(
                repo = repo.name,
                issue = issue.number,
                "opened by the bot without a usable origin tag; ignoring until it changes"
            );
            self.state
                .repo_mut(&repo.name)
                .ignored
                .insert(issue.number, Ignored::new(issue, &triggers));
            // Still polled for whoever subscribed to it.
            if prior.as_ref().is_some_and(|p| p.subscriber_only) {
                self.entry(repo, issue.number).subscriber_only = true;
            }
            return Ok(());
        }
        self.refresh_projects(repo, owner, name, issue.number).await;
        let mine = self.for_recipient(&diff.rendered, &session_id(&repo.name, issue.number));

        let mut existing: Option<Worktree> = None;
        if let Some(id) = prior.as_ref().and_then(|s| s.worktree_id.clone()) {
            if self.driver(repo).worktree_exists(&id).await? {
                existing = Some(Worktree {
                    id,
                    path: prior
                        .as_ref()
                        .and_then(|s| s.worktree_path.clone())
                        .unwrap_or_default(),
                    branch: prior.as_ref().and_then(|s| s.branch.clone()),
                });
            }
        }
        if existing.is_none() {
            existing = self
                .driver(repo)
                .find_worktree_for_issue(&setup.repo_id, issue.number)
                .await?;
        }

        let comment = format!(
            "ssf: bound to {} #{}",
            if is_pr { "PR" } else { "issue" },
            issue.number
        );
        let handle = match existing {
            Some(wt) => {
                info!(
                    repo = repo.name,
                    issue = issue.number,
                    worktree = wt.id,
                    "reusing existing workspace"
                );
                self.remember_worktree(repo, issue.number, &wt);
                let _ = self.driver(repo).set_comment(&wt.id, &comment).await;
                let text = self.initial_text(repo, issue, &mine);
                // The item is attached again, to what it had: `deliver_to`
                // leaves a relaunch for this onboarding to be told here.
                self.onboarding = Some((repo.name.clone(), issue.number));
                let delivered = self.deliver_to(repo, issue.number, &text, None).await;
                self.onboarding = None;
                let d = delivered?;
                let launch = self.launch_of(repo, issue.number);
                let handed_off_from = self.entry(repo, issue.number).delegated_by.clone();
                self.post_event(
                    repo,
                    issue.number,
                    Event::Attached(Attach::Kept {
                        launch,
                        handed_off_from,
                        conversation: if d.relaunched {
                            Conversation::of(d.resumed)
                        } else {
                            Conversation::Kept
                        },
                    }),
                )
                .await;
                d.handle
            }
            None => {
                let created = self
                    .create_workspace(
                        repo,
                        &setup.repo_id,
                        &wt_name,
                        issue.number,
                        &comment,
                        pr.as_ref(),
                    )
                    .await?;
                info!(
                    repo = repo.name,
                    issue = issue.number,
                    worktree = created.id,
                    "created workspace"
                );
                self.remember_worktree(repo, issue.number, &created);
                {
                    let e = self.entry(repo, issue.number);
                    e.launched_at = Some(now_iso());
                    e.agent_session_id = None;
                }
                let eff = self.effective(repo, issue.number);
                let title = format!("{} · #{}", eff.harness, issue.number);
                let cmd = self.launch_command(
                    repo,
                    issue.number,
                    &issue.html_url,
                    &eff.harness_command(),
                );
                let text = self.initial_text(repo, issue, &mine);
                let handle = self
                    .driver(repo)
                    .start(&created.id, &cmd, &title, &eff.harness, &text)
                    .await?;
                info!(
                    repo = repo.name,
                    issue = issue.number,
                    handle,
                    "launched {} and sent the {}",
                    eff.harness,
                    if is_pr { "pull request" } else { "issue" }
                );
                let launch = self.launch_of(repo, issue.number);
                let handed_off_from = self.entry(repo, issue.number).delegated_by.clone();
                self.post_event(
                    repo,
                    issue.number,
                    Event::Attached(Attach::Started {
                        launch,
                        handed_off_from,
                    }),
                )
                .await;
                handle
            }
        };
        if let Some(id) = self.entry(repo, issue.number).worktree_id.clone() {
            let _ = self.driver(repo).set_status(&id, "in-progress").await;
        }

        let e = self.entry(repo, issue.number);
        e.terminal_handle = Some(handle);
        e.updated_at = Some(issue.updated_at.clone());
        e.seen = diff.seen;
        e.seeded = true;
        e.active = true;
        e.bound_at = Some(now_iso());
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        if let Some(since) = since_prior {
            self.fan_out(repo, issue, &since, Fyi::Tracked, false, &[])
                .await;
        }
        // The session file appears once the first message is processed.
        tokio::time::sleep(Duration::from_secs(3)).await;
        self.capture_sessions(repo);
        Ok(())
    }

    /// Bind an item to another item's session and tell that agent about it.
    /// The owner is brought back first if it was retired or its workspace is
    /// gone (delivery does that), and is kept from being cleaned up while it
    /// has active dependents.
    pub(in crate::engine) async fn bind_to(
        &mut self,
        repo: &RepoConfig,
        issue: &Issue,
        owner: u64,
        diff: Diff,
        triggers: Vec<String>,
        since_prior: Option<Vec<Rendered>>,
    ) -> Result<()> {
        info!(
            repo = repo.name,
            issue = issue.number,
            owner,
            ?triggers,
            "bound to the owning session"
        );
        self.entry(repo, owner).cleanup_pending = false;
        {
            let e = self.entry(repo, issue.number);
            e.shares_workspace_of = Some(owner);
            e.triggers = triggers;
        }
        self.mirror_owner(repo, issue.number, owner);
        let snapshot = self.entry(repo, issue.number).clone();
        let mine = self.for_recipient(&diff.rendered, &session_id(&repo.name, owner));
        let ctx = self.ctx(repo, &snapshot);
        let text = prompt::tracked_prompt(issue, &mine, &ctx);
        let d = self.deliver_to(repo, issue.number, &text, None).await?;
        let e = self.entry(repo, issue.number);
        e.terminal_handle = Some(d.handle);
        e.updated_at = Some(issue.updated_at.clone());
        e.seen = diff.seen;
        e.seeded = true;
        e.active = true;
        e.bound_at = Some(now_iso());
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        // The bound item hears which session took it (the owner's own item
        // heard about that session when it started).
        self.post_event(
            repo,
            issue.number,
            Event::Attached(Attach::Bound {
                session: session_id(&repo.name, owner),
                shares: owner,
            }),
        )
        .await;
        if let Some(since) = since_prior {
            self.fan_out(repo, issue, &since, Fyi::Tracked, false, &[])
                .await;
        }
        Ok(())
    }

    /// The item's word in a prompt or a post: `issue` or `pull request`.
    pub(in crate::engine) fn item_kind(&self, repo: &RepoConfig, number: u64) -> &'static str {
        match self.peek(repo, number).and_then(|s| s.kind.as_deref()) {
            Some("pull_request") => "pull request",
            _ => "issue",
        }
    }

    /// What a session that has been told nothing yet is owed: whatever a
    /// handover left for it (the outgoing agent's summary, or the fact
    /// that it left none) ahead of the item's whole story. The note stays
    /// on the record until a session has actually been given it -- a
    /// start that fails is tried again later, and the words the outgoing
    /// agent left go with that attempt rather than being lost with the
    /// pane that never came up.
    pub(in crate::engine) async fn first_message(
        &mut self,
        repo: &RepoConfig,
        number: u64,
    ) -> Result<Story> {
        let note = self
            .peek(repo, number)
            .and_then(|s| s.handover_note.clone());
        let kind = self.item_kind(repo, number);
        let mut story = self
            .story(repo, number, note.as_ref().map(|n| n.from.as_str()), None)
            .await?;
        if let Some(n) = note {
            story.text = prompt::handover_prompt(&n.from, kind, n.summary.as_deref(), &story.text);
        }
        Ok(story)
    }

    /// The whole story of an item as its initial prompt would tell it, for
    /// a harness that starts from scratch and needs context for whatever is
    /// about to be delivered, with what telling it counts as seen.
    pub(in crate::engine) async fn story(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        handed_over_from: Option<&str>,
        target_harness: Option<&str>,
    ) -> Result<Story> {
        let (owner, name) = repo.split()?;
        let issue = self.gh.issue(owner, name, number).await?;
        let timeline = self.gh.timeline(owner, name, number).await?;
        let diff = self.diff(repo, &BTreeMap::new(), &timeline);
        let me = self.acting_on(repo, number);
        let all = self.for_recipient(&diff.rendered, &me);
        let st = self.entry(repo, number).clone();
        let mut ctx = PromptContext {
            handed_over_from,
            ..self.ctx(repo, &st)
        };
        if let Some(harness) = target_harness {
            ctx.harness_prompt = st
                .worktree_path
                .as_deref()
                .and_then(|p| ProjectPrompt::load_harness(repo, Path::new(p), harness));
        }
        Ok(Story {
            text: prompt::initial_prompt(&issue, &all, &ctx),
            seen: diff.seen,
            updated_at: issue.updated_at,
        })
    }

    /// Tell the session that handed an item off that it has closed, with the
    /// last comment its agent left. Best effort, and never worth rebuilding a
    /// retired parent's workspace for.
    pub(in crate::engine) async fn notify_parent(
        &mut self,
        repo: &RepoConfig,
        issue: &Issue,
        merged: bool,
        timeline: &[Value],
        parent: &str,
    ) {
        let Some(origin) = Origin::parse(parent) else {
            warn!(
                repo = repo.name,
                issue = issue.number,
                parent,
                "unparseable parent session"
            );
            return;
        };
        let Some(prepo) = self
            .cfg
            .repos
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(&origin.repo))
            .cloned()
        else {
            warn!(
                repo = repo.name,
                issue = issue.number,
                parent,
                "parent session's repository is not watched; not notifying"
            );
            return;
        };
        let known = self
            .state
            .repos
            .get(&prepo.name)
            .and_then(|rs| rs.issues.get(&origin.number))
            .is_some_and(|s| s.seeded);
        if !known {
            warn!(
                repo = repo.name,
                issue = issue.number,
                parent,
                "parent session is unknown to ssf; not notifying"
            );
            return;
        }
        let owner = self.owner_of(&prepo, origin.number);
        let ost = self.entry(&prepo, owner).clone();
        let alive = match ost.worktree_id.as_deref() {
            Some(id) => self.driver(repo).worktree_exists(id).await.unwrap_or(false),
            None => false,
        };
        if !ost.active && !alive {
            info!(
                repo = repo.name,
                issue = issue.number,
                parent,
                "parent session is retired and its workspace gone; not notifying"
            );
            return;
        }
        let last = last_bot_comment(timeline, &self.login);
        let cst = self.entry(repo, issue.number).clone();
        let ctx = self.ctx(repo, &cst);
        let text = prompt::delegated_closed_prompt(issue, merged, last.as_ref(), &ctx);
        match self.deliver_to(&prepo, origin.number, &text, None).await {
            Ok(d) => {
                info!(
                    repo = repo.name,
                    issue = issue.number,
                    parent,
                    "told the parent session"
                );
                let e = self.entry(&prepo, origin.number);
                e.terminal_handle = Some(d.handle);
                e.last_prompt_at = Some(now_iso());
                e.prompts_sent += 1;
            }
            Err(e) => warn!(
                repo = repo.name,
                issue = issue.number,
                parent,
                "could not tell the parent session: {e:#}"
            ),
        }
    }

    /// Create the git worktree for an item. Pull requests from this repo are
    /// checked out on their head branch so pushes update the PR; anything
    /// else starts from the configured base.
    pub(in crate::engine) async fn create_workspace(
        &mut self,
        repo: &RepoConfig,
        repo_id: &str,
        wt_name: &str,
        number: u64,
        comment: &str,
        pr: Option<&PrInfo>,
    ) -> Result<Worktree> {
        let mut base = repo.base_branch.clone();
        let mut checkout: Option<String> = None;
        if let Some(p) = pr.filter(|p| p.same_repo(&repo.name) && !p.head_ref.is_empty()) {
            let main = self.driver(repo).repo_path(repo_id).await?;
            if let Err(e) = git(&main, &["fetch", "origin", &p.head_ref]).await {
                warn!(
                    repo = repo.name,
                    issue = number,
                    "fetching PR branch failed: {e:#}"
                );
            }
            if self
                .driver(repo)
                .existing_branch_ref(repo_id, &p.head_ref)
                .await?
                .is_some()
            {
                base = Some(format!("origin/{}", p.head_ref));
                checkout = Some(p.head_ref.clone());
            }
        }
        let created = match self
            .driver(repo)
            .create_worktree(repo_id, wt_name, number, comment, base.as_deref())
            .await
        {
            Ok(w) => w,
            Err(e) if checkout.is_some() => {
                warn!(
                    repo = repo.name,
                    issue = number,
                    "creating from the PR branch failed ({e:#}); using the default base"
                );
                checkout = None;
                self.driver(repo)
                    .create_worktree(
                        repo_id,
                        wt_name,
                        number,
                        comment,
                        repo.base_branch.as_deref(),
                    )
                    .await?
            }
            Err(e) => return Err(e),
        };
        let mut created = created;
        if let Some(branch) = checkout {
            match checkout_branch(&created.path, &branch).await {
                Ok(()) => created.branch = Some(format!("refs/heads/{branch}")),
                Err(e) => warn!(
                    repo = repo.name,
                    issue = number,
                    branch,
                    "could not check out the PR branch: {e:#}"
                ),
            }
        }
        Ok(created)
    }

    /// The issue changed since we last looked: deliver whatever is new.
    pub(in crate::engine) async fn follow_up(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        issue: &Issue,
        st: IssueState,
    ) -> Result<()> {
        let timeline = self.gh.timeline(owner, name, issue.number).await?;
        self.record_origins(repo, issue, &timeline);
        let diff = self.diff(repo, &st.seen, &timeline);
        // Subscribers hear first: the owner's own posts are news to them,
        // and a failed delivery to the owner must not replay to them.
        self.fan_out(repo, issue, &diff.rendered, Fyi::Activity, false, &[])
            .await;
        let mine = self.for_recipient(&diff.rendered, &self.acting_on(repo, issue.number));
        if mine.is_empty() {
            debug!(
                repo = repo.name,
                issue = issue.number,
                "updated but nothing new to deliver"
            );
            let e = self.entry(repo, issue.number);
            e.updated_at = Some(issue.updated_at.clone());
            e.seen = diff.seen;
            e.title = issue.title.clone();
            e.github_state = Some(github_state(issue, st.pr.as_ref(), false));
            return Ok(());
        }
        info!(
            repo = repo.name,
            issue = issue.number,
            events = mine.len(),
            "delivering new activity"
        );
        self.refresh_projects(repo, owner, name, issue.number).await;
        let st = IssueState {
            projects: self.entry(repo, issue.number).projects.clone(),
            ..st
        };
        let ctx = self.ctx(repo, &st);
        let text = prompt::followup_prompt(issue, &mine, &ctx);
        // A harness started from scratch has lost its memory, so it gets the
        // whole story rather than just the delta.
        let mut all = self.diff(repo, &BTreeMap::new(), &timeline).rendered;
        all.retain(|r| !diff.rendered.iter().any(|n| n.key == r.key));
        let all = self.for_recipient(&all, &self.acting_on(repo, issue.number));
        let mut relaunch_text = prompt::initial_prompt(issue, &all, &ctx);
        relaunch_text.push_str("\n\n");
        relaunch_text.push_str(&text);
        let d = self
            .deliver_to(repo, issue.number, &text, Some(&relaunch_text))
            .await?;
        let e = self.entry(repo, issue.number);
        e.updated_at = Some(issue.updated_at.clone());
        e.title = issue.title.clone();
        e.github_state = Some(github_state(issue, st.pr.as_ref(), false));
        e.seen = diff.seen;
        e.terminal_handle = Some(d.handle);
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        Ok(())
    }

    /// Previously retired issue is assigned to the bot again.
    pub(in crate::engine) async fn reactivate(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        issue: &Issue,
        st: IssueState,
    ) -> Result<()> {
        let timeline = self.gh.timeline(owner, name, issue.number).await?;
        if let Err(why) = self.gate(repo, issue, &timeline, &st.triggers) {
            self.refuse(repo, issue, &st.triggers, &why);
            return Ok(());
        }
        info!(
            repo = repo.name,
            issue = issue.number,
            "issue assigned again; reactivating"
        );
        self.record_origins(repo, issue, &timeline);
        let diff = self.diff(repo, &st.seen, &timeline);
        self.refresh_projects(repo, owner, name, issue.number).await;
        let st = IssueState {
            projects: self.entry(repo, issue.number).projects.clone(),
            ..st
        };
        self.entry(repo, issue.number).subscriber_only = false;
        let me = self.acting_on(repo, issue.number);
        let mine = self.for_recipient(&diff.rendered, &me);
        let ctx = self.ctx(repo, &st);
        let text = prompt::reassigned_prompt(issue, &mine, &ctx);
        let all = self.diff(repo, &BTreeMap::new(), &timeline).rendered;
        let all = self.for_recipient(&all, &me);
        let relaunch_text = prompt::initial_prompt(issue, &all, &ctx);
        let d = self
            .deliver_to(repo, issue.number, &text, Some(&relaunch_text))
            .await?;
        if let Some(id) = self.entry(repo, issue.number).worktree_id.clone() {
            let _ = self.driver(repo).set_status(&id, "in-progress").await;
        }
        let e = self.entry(repo, issue.number);
        e.updated_at = Some(issue.updated_at.clone());
        e.title = issue.title.clone();
        e.github_state = Some(github_state(issue, st.pr.as_ref(), false));
        e.seen = diff.seen;
        e.active = true;
        e.cleanup_pending = false;
        // A release the agent asked for before the item came back is off:
        // the session is live again in this workspace.
        e.release_pending = false;
        e.release_forced = false;
        e.retired_at = None;
        e.terminal_handle = Some(d.handle);
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        self.fan_out(repo, issue, &diff.rendered, Fyi::Tracked, false, &[])
            .await;
        Ok(())
    }

    /// Whether a retirement for this item was held recently enough that
    /// the item does not need reading again yet.
    pub(in crate::engine) fn held_recently(&self, repo: &RepoConfig, number: u64) -> bool {
        let Some(st) = self
            .state
            .repos
            .get(&repo.name)
            .and_then(|r| r.issues.get(&number))
        else {
            return false;
        };
        let Some(at) = st.retirement_held_at.as_deref() else {
            return false;
        };
        let window = chrono::Duration::from_std(RETIREMENT_RECHECK).unwrap();
        chrono::DateTime::parse_from_rfc3339(at)
            .map(|t| chrono::Utc::now().signed_duration_since(t.with_timezone(&chrono::Utc)))
            // A stamp ahead of the clock (a backward step from NTP, or a
            // guest resuming without a reliable one) would otherwise read
            // as fresh until the clock caught up, holding for hours.
            .is_ok_and(|age| age >= chrono::Duration::zero() && age < window)
    }

    /// Whether an open item still carries any of the triggers it was
    /// onboarded on, read from the item rather than from a listing, and on
    /// what. The timeline is fetched only if a mention has to be
    /// re-checked, and is handed back so the caller does not fetch it
    /// twice.
    pub(in crate::engine) async fn still_ours(
        &self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        issue: &Issue,
        triggers: &[String],
        timeline: &mut Option<Vec<Value>>,
    ) -> StillOurs {
        let has = |t: &str| triggers.iter().any(|x| x == t);
        // Not gated on the triggers: they are rewritten from the listings
        // every pass, so the assigned listing hiccupping is exactly when
        // this is worth asking, and the item is already in hand.
        if issue.is_assigned_to(&self.login) {
            return StillOurs::Certain;
        }
        if has("created") && issue.author().eq_ignore_ascii_case(&self.login) {
            return StillOurs::Certain;
        }
        if has("mentioned") {
            // The timeline walk is the expensive part, so it is what the
            // hold paces; everything above is read from the item already
            // in hand, and a closed item never reaches here at all.
            if self.held_recently(repo, issue.number) {
                return StillOurs::Paced;
            }
            match self.gh.timeline(owner, name, issue.number).await {
                Ok(tl) => {
                    let found = mentions_bot(issue, &tl, &self.login);
                    *timeline = Some(tl);
                    if found {
                        return StillOurs::Paced;
                    }
                }
                // Without the timeline there is no evidence either way. A
                // retirement is the destructive reading, so the item keeps
                // the benefit of the doubt until a pass can read it.
                Err(e) => {
                    warn!(
                        repo = repo.name,
                        issue = issue.number,
                        "could not re-check the mention before retiring: {e:#}"
                    );
                    return StillOurs::Paced;
                }
            }
        }
        if has("review_requested") && issue.is_pull_request() {
            match self.gh.pull(owner, name, issue.number).await {
                Ok(pr) => {
                    if pr.requests_review_from(&self.login) {
                        return StillOurs::Certain;
                    }
                }
                Err(e) => {
                    warn!(
                        repo = repo.name,
                        issue = issue.number,
                        "could not re-check the review request before retiring: {e:#}"
                    );
                    return StillOurs::Paced;
                }
            }
        }
        StillOurs::No
    }

    /// Forget a held retirement: the item is on a listing again, or has
    /// been judged on something that needs no pacing.
    pub(in crate::engine) fn clear_hold(&mut self, repo: &RepoConfig, number: u64) {
        let e = self.entry(repo, number);
        e.retirement_held_at = None;
        e.retirement_announced = false;
    }

    /// Record a hold that rests on the paced re-check. The walk that
    /// answers it is the expensive part, so this stamps the item and the
    /// next walk waits out `RETIREMENT_RECHECK`. The stamp is only written
    /// by a pass that actually read the item, so a hold expires rather
    /// than rolling forward.
    pub(in crate::engine) fn note_paced_hold(&mut self, repo: &RepoConfig, number: u64) -> bool {
        if self.held_recently(repo, number) {
            debug!(
                repo = repo.name,
                issue = number,
                "retirement still held: the item carried a trigger recently"
            );
            return true;
        }
        let e = self.entry(repo, number);
        e.retirement_held_at = Some(now_iso());
        if e.retirement_announced {
            debug!(
                repo = repo.name,
                issue = number,
                "retirement held again: the listings still disagree with the item"
            );
        } else {
            e.retirement_announced = true;
            // A listing disagreeing with an item is worth seeing once.
            info!(
                repo = repo.name,
                issue = number,
                "retirement held: the listings dropped the item but it still carries a trigger"
            );
        }
        true
    }

    /// Item left the set of open things involving the bot: tell the agent to stop.
    pub(in crate::engine) async fn retire_issue(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        number: u64,
    ) -> Result<()> {
        let st = self
            .state
            .repo_mut(&repo.name)
            .issues
            .get(&number)
            .cloned()
            .context("retiring unknown item")?;
        let issue = self.gh.issue(owner, name, number).await?;
        let closed = issue.state == "closed";
        // A listing can lag, and can come back without an item that is
        // still the bot's. The item itself is the only reliable evidence,
        // so every trigger it was onboarded on is re-checked against it
        // before a session is told to stop. Testing only assignment and
        // authorship retired a mention-triggered session whenever its
        // listing hiccupped, even though the mention was still sitting in
        // the issue, and the session was reattached on the next pass.
        let mut timeline = None;
        let verdict = if closed {
            StillOurs::No
        } else {
            self.still_ours(repo, owner, name, &issue, &st.triggers, &mut timeline)
                .await
        };
        let hold = match verdict {
            StillOurs::No => false,
            // Judged on the item itself, so there is no walk to pace and
            // no bookkeeping to keep.
            StillOurs::Certain => {
                self.clear_hold(repo, number);
                debug!(
                    repo = repo.name,
                    issue = number,
                    "retirement held: the item itself still names the bot"
                );
                true
            }
            StillOurs::Paced => self.note_paced_hold(repo, number),
        };
        if hold {
            return Ok(());
        }
        let merged = closed
            && issue.is_pull_request()
            && match self.gh.pull(owner, name, number).await {
                Ok(info) => info.merged,
                Err(_) => st.pr.as_ref().is_some_and(|p| p.merged),
            };
        info!(
            repo = repo.name,
            issue = number,
            closed,
            "item no longer active for the bot"
        );
        let timeline = match timeline {
            Some(t) => t,
            None => self.gh.timeline(owner, name, number).await?,
        };
        self.record_origins(repo, &issue, &timeline);
        let diff = self.diff(repo, &st.seen, &timeline);
        let session = self.owner_of(repo, number);
        let mine = self.for_recipient(&diff.rendered, &session_id(&repo.name, session));
        let ctx = self.ctx(repo, &st);
        let text = if closed {
            prompt::closed_prompt(&issue, &mine, &ctx)
        } else {
            prompt::unassigned_prompt(&issue, &mine, &ctx)
        };
        // Subscribers hear about the end of it too; a delegating parent gets
        // its own, fuller message below instead.
        let parent_to_tell = if closed && !st.parent_notified {
            st.delegated_by.clone()
        } else {
            None
        };
        let skip: Vec<String> = parent_to_tell
            .iter()
            .map(|p| self.acting_session(p))
            .collect();
        self.fan_out(
            repo,
            &issue,
            &diff.rendered,
            if closed { Fyi::Closed } else { Fyi::Unassigned },
            merged,
            &skip,
        )
        .await;
        // Retirement is best-effort: a deleted workspace must not keep us
        // retrying, and it is not worth rebuilding one just to say goodbye.
        // An owned item's workspace is its owner's.
        let workspace_alive = match self.entry(repo, session).worktree_id.clone() {
            Some(id) => self
                .driver(repo)
                .worktree_exists(&id)
                .await
                .unwrap_or(false),
            None => false,
        };
        let handle = if workspace_alive {
            match self.deliver_to(repo, number, &text, None).await {
                Ok(d) => Some(d.handle),
                Err(e) => {
                    warn!(
                        repo = repo.name,
                        issue = number,
                        "could not notify agent: {e:#}"
                    );
                    None
                }
            }
        } else {
            None
        };
        let shared = st.shares_workspace_of.is_some();
        // The workspace is done with only when nothing bound to this session
        // is still open.
        let dependents = self.active_dependents(repo, number);
        let done = closed && workspace_alive && !shared && dependents.is_empty();
        if done {
            if let Some(id) = &st.worktree_id {
                let _ = self.driver(repo).set_status(id, "completed").await;
            }
        }
        // The workspace stays, whatever state it is in: the agent releases
        // it with `ssf release` when everything is on origin, and `ssf
        // purge` deals with the rest.
        let e = self.entry(repo, number);
        e.active = false;
        e.retirement_held_at = None;
        e.retirement_announced = false;
        e.title = issue.title.clone();
        e.github_state = Some(github_state(&issue, st.pr.as_ref(), merged));
        e.updated_at = Some(issue.updated_at.clone());
        e.seen = diff.seen;
        e.retired_at = Some(now_iso());
        e.cleanup_pending = false;
        if handle.is_some() {
            e.terminal_handle = handle;
            e.last_prompt_at = Some(now_iso());
            e.prompts_sent += 1;
        }
        if closed && !dependents.is_empty() {
            info!(
                repo = repo.name,
                issue = number,
                ?dependents,
                "closed, but its session still owns open items; keeping the workspace"
            );
        }
        // The last open item bound to a retired owner: the owner's session
        // is done now (its workspace stays until released or purged).
        if shared && workspace_alive {
            self.release_owner(repo, session).await;
        }
        if let Some(parent) = parent_to_tell {
            self.notify_parent(repo, &issue, merged, &timeline, &parent)
                .await;
            self.entry(repo, number).parent_notified = true;
        }
        // A retired session hears nothing more: it is unsubscribed everywhere.
        // Its own item keeps its subscribers, and stays polled for them while
        // it is open.
        if !shared {
            let me = session_id(&repo.name, number);
            let dropped = self.state.unsubscribe_everywhere(&me);
            if !dropped.is_empty() {
                info!(
                    repo = repo.name,
                    issue = number,
                    ?dropped,
                    "retired session unsubscribed"
                );
            }
        }
        let e = self.entry(repo, number);
        if !closed && !e.subscribers.is_empty() {
            e.subscriber_only = true;
        }
        Ok(())
    }
}
