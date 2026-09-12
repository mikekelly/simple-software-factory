use super::super::*;
use tracing::{debug, info, warn};

impl Engine {
    pub(in crate::engine) fn ctx<'a>(
        &'a self,
        repo: &'a RepoConfig,
        st: &'a IssueState,
    ) -> PromptContext<'a> {
        // The repository's prompt file is read from the item's own checkout,
        // so a PR branch that changes it is seen as the branch has it.
        let project_prompt = st
            .worktree_path
            .as_deref()
            .and_then(|p| ProjectPrompt::load(repo, Path::new(p)));
        let harness = self.effective(repo, st.number).harness;
        let harness_prompt = st
            .worktree_path
            .as_deref()
            .and_then(|p| ProjectPrompt::load_harness(repo, Path::new(p), &harness));
        PromptContext {
            repo,
            daemon: &self.cfg.daemon,
            bot_login: &self.login,
            driver: self.cfg.driver_for(repo),
            pr: st.pr.as_ref(),
            triggers: &st.triggers,
            owner: st.shares_workspace_of,
            delegated_by: st.delegated_by.as_deref(),
            handed_over_from: None,
            projects: &st.projects,
            project_prompt,
            harness_prompt,
            vm_guest: crate::vm::in_guest(),
            pushes_as: self.cfg.git_identity(Some(repo)).credential.prompt_pusher(),
        }
    }

    /// The full first message for an item, built once its workspace is
    /// known so the prompt file in that checkout can be included.
    pub(in crate::engine) fn initial_text(
        &mut self,
        repo: &RepoConfig,
        issue: &Issue,
        rendered: &[Rendered],
    ) -> String {
        let snapshot = self.entry(repo, issue.number).clone();
        let ctx = self.ctx(repo, &snapshot);
        prompt::initial_prompt(issue, rendered, &ctx)
    }

    /// Refresh which project boards the item is on. Best effort: a failed
    /// lookup (no `project` scope, GraphQL hiccup) keeps whatever was known
    /// and is logged, since the prompt is still useful without it.
    pub(in crate::engine) async fn refresh_projects(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        number: u64,
    ) {
        match self.gh.project_items(owner, name, number).await {
            Ok(projects) => self.entry(repo, number).projects = projects,
            Err(e) => warn!(
                repo = repo.name,
                issue = number,
                "could not look up project boards: {e:#}"
            ),
        }
    }

    /// The session that acts on an item: the item's own, or the one it is
    /// bound to (following a chain of bindings, which is normally one hop).
    pub(in crate::engine) fn owner_of(&self, repo: &RepoConfig, number: u64) -> u64 {
        match self.state.repos.get(&repo.name) {
            Some(rs) => owner_in(&rs.issues, number),
            None => number,
        }
    }

    /// Active items bound to `number`'s session.
    pub(in crate::engine) fn active_dependents(&self, repo: &RepoConfig, number: u64) -> Vec<u64> {
        self.state
            .repos
            .get(&repo.name)
            .map(|rs| {
                rs.issues
                    .values()
                    .filter(|s| s.active && s.shares_workspace_of == Some(number))
                    .map(|s| s.number)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Copy the owner's workspace and harness details onto an item bound to
    /// it, so status and delivery records agree.
    pub(in crate::engine) fn mirror_owner(&mut self, repo: &RepoConfig, number: u64, owner: u64) {
        let o = self.entry(repo, owner).clone();
        let e = self.entry(repo, number);
        e.worktree_id = o.worktree_id;
        e.worktree_path = o.worktree_path;
        e.repo_id = o.repo_id;
        e.driver = o.driver;
        e.branch = o.branch;
        e.terminal_handle = o.terminal_handle;
        e.agent_session_id = o.agent_session_id;
        e.launched_at = o.launched_at;
        e.cleanup_pending = false;
        e.release_pending = false;
        e.release_forced = false;
        e.released_at = o.released_at;
    }

    /// The session an item being discovered belongs to, if any. First the
    /// origin tag in its body (the session that opened it, unless that was
    /// a hand-off), then, for a same-repo pull request, the session whose
    /// workspace is on the PR's branch. A retired session still counts: it
    /// is brought back rather than duplicated.
    pub(in crate::engine) fn find_owner(
        &self,
        repo: &RepoConfig,
        issue: &Issue,
        pr: Option<&PrInfo>,
        scan: &origin::Scan,
    ) -> Option<u64> {
        let issues = &self.state.repos.get(&repo.name)?.issues;
        if let Some(tag) = &scan.origin_tag {
            if tag.is_delegate() {
                return None;
            }
            if !tag.origin.repo.eq_ignore_ascii_case(&repo.name) {
                info!(
                    repo = repo.name,
                    issue = issue.number,
                    origin = %tag.origin,
                    "opened from a session on another repository; not binding to it"
                );
            } else if tag.origin.number != issue.number {
                match issues.get(&tag.origin.number).filter(|s| s.seeded) {
                    Some(o) => return Some(owner_in(issues, o.number)),
                    None => warn!(
                        repo = repo.name,
                        issue = issue.number,
                        origin = %tag.origin,
                        "opened from a session ssf does not know; not binding to it"
                    ),
                }
            }
        }
        let p = pr.filter(|p| p.same_repo(&repo.name) && !p.head_ref.is_empty())?;
        let head = format!("refs/heads/{}", p.head_ref);
        issues
            .values()
            .filter(|s| {
                s.seeded
                    && s.number != issue.number
                    && s.shares_workspace_of.is_none()
                    && s.branch.as_deref() == Some(head.as_str())
            })
            .max_by_key(|s| (s.active, s.bound_at.clone()))
            .map(|s| s.number)
    }

    /// Parse origin tags out of the item body and its timeline, and flag posts
    /// by the bot that carry none: a person typed them as the bot, or the gh
    /// shim was not in effect in whichever session made them, and nothing
    /// can tell which.
    pub(in crate::engine) fn record_origins(
        &mut self,
        repo: &RepoConfig,
        issue: &Issue,
        timeline: &[Value],
    ) -> origin::Scan {
        let scan = origin::scan(issue, timeline, &self.login);
        let login = self.login.clone();
        let e = self.entry(repo, issue.number);
        for (key, url) in &scan.untagged {
            if !e.untagged.contains_key(key) {
                warn!(
                    repo = repo.name,
                    issue = issue.number,
                    url,
                    "post by @{login} without an origin tag (a person, or the gh shim not in effect)"
                );
            }
        }
        e.origin = scan.origin.clone();
        e.origins = scan.origins.clone();
        e.untagged = scan.untagged.clone();
        scan
    }

    /// What is new on an item's timeline since `seen`, rendered for the
    /// prompts. Events by a login that may not drive the repository are
    /// left out (and still counted as seen): they are neither delivered to
    /// the owner nor fanned out. Commits carry no login (`author.name` is
    /// a git name) and pass; pushing needs write access to the branch.
    pub(in crate::engine) fn diff(
        &self,
        repo: &RepoConfig,
        seen: &BTreeMap<String, String>,
        timeline: &[Value],
    ) -> Diff {
        let allowed = self.allow_list(repo);
        let mut rendered = Vec::new();
        let mut observed = BTreeMap::new();
        let mut filtered: Value;
        for ev in timeline {
            let mut ev = ev;
            let Some(key) = event_key(ev) else { continue };
            let kind = ev.get("event").and_then(Value::as_str).unwrap_or("");
            let marker = crate::github::value_str(ev, &["updated_at"])
                .filter(|_| kind == "commented")
                .unwrap_or("")
                .to_string();
            let previous = seen.get(&key);
            let is_new = previous.is_none();
            let edited = previous.is_some_and(|m| !m.is_empty() && *m != marker);
            observed.insert(key.clone(), marker.clone());
            if !is_new && !edited {
                continue;
            }
            // The bot's own commits and cross-references would only echo
            // the agent's work back at it. Things done *to* the bot, like
            // being assigned, always count. The bot's comments are kept:
            // one that carries an origin tag came from one session and may
            // be news to another, so it is sorted out per recipient
            // (`for_recipient`) instead; one without a tag was typed by a
            // person using the bot account (every session stamps its
            // posts), so it is delivered like any human's. The daemon's
            // own event posts (`event=` in the tag) are for people: no
            // agent, owner or subscriber, ever sees one.
            let actor = actor_of(ev);
            let own = actor.eq_ignore_ascii_case(&self.login);
            let echo = matches!(kind, "cross-referenced" | "referenced" | "committed");
            if own && echo && !self.cfg.daemon.include_own_events {
                debug!(key, "skipping bot's own event");
                continue;
            }
            if own
                && kind == "commented"
                && origin::is_event_post(crate::github::value_str(ev, &["body"]).unwrap_or(""))
            {
                debug!(key, "skipping the daemon's own event post");
                continue;
            }
            match kind {
                "committed" => {}
                // A batch of review comments: each has its own author.
                "line-commented" | "commit-commented" => {
                    let Some(comments) = ev.get("comments").and_then(Value::as_array) else {
                        continue;
                    };
                    let kept: Vec<Value> = comments
                        .iter()
                        .filter(|c| {
                            let who = crate::github::value_str(c, &["user", "login"])
                                .unwrap_or("unknown");
                            let ok = allowed.allows(who);
                            if !ok {
                                self.dropped(repo, &key, who);
                            }
                            ok
                        })
                        .cloned()
                        .collect();
                    if kept.is_empty() {
                        continue;
                    }
                    if kept.len() != comments.len() {
                        filtered = ev.clone();
                        filtered["comments"] = Value::Array(kept);
                        ev = &filtered;
                    }
                }
                _ => {
                    if !allowed.allows(&actor) {
                        self.dropped(repo, &key, &actor);
                        continue;
                    }
                }
            }
            if let Some(r) = render_event(ev, edited, &self.cfg.daemon, &self.login) {
                rendered.push(r);
            }
        }
        Diff {
            rendered,
            seen: observed,
        }
    }

    pub(in crate::engine) async fn reconcile_issue(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        issue: &Issue,
        pr: Option<PrInfo>,
        triggers: Vec<String>,
    ) -> Result<()> {
        let existing = self
            .state
            .repo_mut(&repo.name)
            .issues
            .get(&issue.number)
            .cloned();
        let mut existing = existing;
        if let Some(st) = existing
            .as_mut()
            .filter(|s| s.seeded && s.triggers != triggers)
        {
            let e = self.entry(repo, issue.number);
            e.triggers = triggers.clone();
            st.triggers = triggers.clone();
        }
        match existing {
            Some(st) if st.seeded && !st.active => {
                self.reactivate(repo, owner, name, issue, st).await?
            }
            Some(st) if st.seeded => {
                if st.updated_at.as_deref() != Some(issue.updated_at.as_str()) {
                    self.follow_up(repo, owner, name, issue, st).await?
                }
            }
            _ => {
                self.onboard(repo, owner, name, issue, pr, triggers.clone())
                    .await?
            }
        }
        Ok(())
    }

    pub(in crate::engine) fn entry(&mut self, repo: &RepoConfig, number: u64) -> &mut IssueState {
        let e = self
            .state
            .repo_mut(&repo.name)
            .issues
            .entry(number)
            .or_default();
        e.number = number;
        e
    }

    pub(in crate::engine) fn remember_worktree(
        &mut self,
        repo: &RepoConfig,
        number: u64,
        wt: &Worktree,
    ) {
        let driver = self.cfg.driver_for(repo);
        let e = self.entry(repo, number);
        e.worktree_id = Some(wt.id.clone());
        e.worktree_path = Some(wt.path.clone());
        e.driver = Some(driver.id().into());
        if let Some((r, _)) = wt.id.split_once("::") {
            e.repo_id = Some(r.to_string());
        }
        if wt.branch.is_some() {
            e.branch = wt.branch.clone();
        }
        e.cleanup_pending = false;
        e.release_pending = false;
        e.release_forced = false;
        e.released_at = None;
        e.release_refusals = 0;
    }
}
