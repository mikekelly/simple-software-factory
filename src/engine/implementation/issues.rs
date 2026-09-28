use super::super::*;
use tracing::{debug, info, warn};
/// Marks `seen` as written by a daemon that lists review comments (#612).
const REVIEW_COMMENTS_SEEN: &str = "review-comments:listed";

impl Engine {
    pub(in crate::engine) fn ctx<'a>(
        &'a self,
        repo: &'a RepoConfig,
        st: &'a IssueState,
    ) -> PromptContext<'a> {
        // The repository's SSF agent guidance is read from the item's own checkout,
        // so a PR branch that changes it is seen as the branch has it.
        let project_prompt = st
            .worktree_path
            .as_deref()
            .and_then(|p| ProjectPrompt::load(repo, Path::new(p)));
        // The guidance is the running harness's: a session left on another
        // one by a config edit reads the notes for the harness it is
        // actually on, not for the one the next launch would start.
        let harness = self.live_harness(repo, st.number);
        let global_prompt = ProjectPrompt::load_global(repo);
        let global_harness_prompt = ProjectPrompt::load_global_harness(repo, &harness);
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
            global_prompt,
            global_harness_prompt,
            project_prompt,
            harness_prompt,
            vm_guest: crate::vm::in_guest(),
            pushes_as: self.cfg.git_identity(Some(repo)).credential.prompt_pusher(),
        }
    }

    /// The full first message for an item, built once its workspace is
    /// known so the SSF agent guidance in that checkout can be included.
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
        e.branch = o.branch;
        e.terminal_handle = o.terminal_handle;
        e.agent_session_id = o.agent_session_id;
        e.launched_at = o.launched_at;
        e.launched_stack = o.launched_stack;
        e.cleanup_pending = false;
        e.release_pending = false;
        e.release_forced = false;
        e.released_at = o.released_at;
    }

    /// The session a pull request being discovered belongs to, if any. First
    /// the origin tag in its body (the session that opened it, unless that
    /// was a hand-off), then the session whose workspace is on its same-repo
    /// branch. Issues are never bound by authorship: an assignment, mention
    /// or other action trigger gives them their own session. A retired PR
    /// owner still counts: it is brought back rather than duplicated.
    pub(in crate::engine) fn find_owner(
        &self,
        repo: &RepoConfig,
        issue: &Issue,
        pr: Option<&PrInfo>,
        scan: &origin::Scan,
    ) -> Option<u64> {
        let issues = &self.state.repos.get(&repo.name)?.issues;
        if issue.is_pull_request()
            && let Some(tag) = &scan.origin_tag
        {
            if tag.is_delegate() {
                return None;
            }
            if !repo.matches_name(&tag.origin.repo) {
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
        // Before #612 review comments were not listed, so state seen by an
        // older daemon holds none of them: count them as seen once rather
        // than deliver a PR's whole review history as new.
        let adopt = !seen.is_empty() && !seen.contains_key(REVIEW_COMMENTS_SEEN);
        observed.insert(REVIEW_COMMENTS_SEEN.to_string(), String::new());
        let mut filtered: Value;
        for ev in timeline {
            let mut ev = ev;
            let Some(key) = event_key(ev) else { continue };
            let kind = ev.get("event").and_then(Value::as_str).unwrap_or("");
            let marker = crate::github::value_str(ev, &["updated_at"])
                .filter(|_| matches!(kind, "commented" | "line-commented"))
                .unwrap_or("")
                .to_string();
            let previous = seen.get(&key);
            let is_new = previous.is_none();
            let edited = previous.is_some_and(|m| !m.is_empty() && *m != marker);
            observed.insert(key.clone(), marker.clone());
            if !is_new && !edited || adopt && kind == "line-commented" {
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

    /// Reactions added to or removed from the item's body and its comments
    /// since `seen`, rendered, and the reaction records to keep in `seen`
    /// (`reactions:body`, `reactions:<comment key>`: who reacted with what).
    /// The timeline and the item carry counts only; a post whose counts
    /// moved has its reactions listed to tell who. A post with no record
    /// yet (a new session, a new comment, state from before reactions were
    /// followed) is recorded as it stands and nothing is delivered for it,
    /// so reactions that were already there never arrive as news. The
    /// bot's own reactions, and those by logins the allow-list refuses,
    /// are recorded but not delivered; `daemon.ignored_events` naming
    /// `reacted` records them all without delivering any.
    pub(in crate::engine) async fn reaction_diff(
        &self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
        issue: &Issue,
        seen: &BTreeMap<String, String>,
        timeline: &[Value],
    ) -> Result<Diff> {
        let counts = reaction_counts;
        // An item built from a listing that does not carry the body's
        // counts (`/pulls`) says nothing about them: its record stands.
        let mut targets = Vec::new();
        let mut observed = BTreeMap::new();
        if issue.reactions.is_some() {
            targets.push((
                "reactions:body".to_string(),
                format!("issues/{}", issue.number),
                issue.html_url.clone(),
                counts(issue.reactions.as_ref()),
            ));
        } else if let Some(v) = seen.get("reactions:body") {
            observed.insert("reactions:body".to_string(), v.clone());
        }
        for ev in timeline {
            if ev.get("event").and_then(Value::as_str) != Some("commented") {
                continue;
            }
            let (Some(key), Some(id)) = (event_key(ev), crate::github::value_u64(ev, &["id"]))
            else {
                continue;
            };
            targets.push((
                format!("reactions:{key}"),
                format!("issues/comments/{id}"),
                crate::github::value_str(ev, &["html_url"])
                    .unwrap_or("")
                    .to_string(),
                counts(ev.get("reactions")),
            ));
        }
        let allowed = self.allow_list(repo);
        let ignored = self
            .cfg
            .daemon
            .ignored_events
            .iter()
            .any(|k| k == "reacted");
        let mut rendered = Vec::new();
        for (key, on, url, now) in targets {
            let stored = seen.get(&key).map(|s| parse_reactions(s));
            if let Some(old) = &stored
                && tally(old) == now
            {
                let kept = seen[&key].clone();
                observed.insert(key, kept);
                continue;
            }
            if stored.is_none() && now.is_empty() {
                observed.insert(key, String::new());
                continue;
            }
            let list = self.gh.reactions(owner, name, &on).await?;
            let new: BTreeSet<(String, String)> = list
                .iter()
                .map(|(l, c, _)| (l.clone(), c.clone()))
                .collect();
            observed.insert(key.clone(), format_reactions(&new));
            let Some(old) = stored else { continue };
            let when = |login: &str, content: &str| {
                list.iter()
                    .find(|(l, c, _)| l == login && c == content)
                    .map(|(_, _, at)| at.clone())
                    .unwrap_or_else(now_iso)
            };
            let changes = new
                .difference(&old)
                .map(|r| (r, true))
                .chain(old.difference(&new).map(|r| (r, false)));
            for ((login, content), added) in changes {
                if login.eq_ignore_ascii_case(&self.login) {
                    continue;
                }
                if !allowed.allows(login) {
                    self.dropped(repo, &key, login);
                    continue;
                }
                if ignored {
                    continue;
                }
                let at = if added {
                    when(login, content)
                } else {
                    now_iso()
                };
                let verb = if added { "added" } else { "removed" };
                rendered.push(Rendered {
                    key: format!("reacted:{key}:{login}:{content}:{verb}:{at}"),
                    text: prompt::render_reaction(login, content, &url, added, &at),
                    origin: None,
                    assignee: None,
                    state_change: false,
                    at: Some(at.clone()),
                });
            }
        }
        Ok(Diff {
            rendered,
            seen: observed,
        })
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
                // A reaction on the body moves the listing (whose items
                // carry the counts) but neither `updated_at` nor the timeline.
                if st.updated_at.as_deref() != Some(issue.updated_at.as_str())
                    || body_reactions_moved(&st.seen, issue)
                {
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
        let e = self.entry(repo, number);
        e.worktree_id = Some(wt.id.clone());
        e.worktree_path = Some(wt.path.clone());
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

/// A post's reactions as `reaction_diff` records them in `seen`: one
/// `login:content` per reaction, space separated (neither has a space or
/// a colon in it).
fn format_reactions(set: &BTreeSet<(String, String)>) -> String {
    set.iter()
        .map(|(l, c)| format!("{l}:{c}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_reactions(s: &str) -> BTreeSet<(String, String)> {
    s.split_whitespace()
        .filter_map(|r| r.split_once(':'))
        .map(|(l, c)| (l.to_string(), c.to_string()))
        .collect()
}

/// Per-content reaction counts GitHub reports on a post (`reactions`).
fn reaction_counts(v: Option<&Value>) -> BTreeMap<String, u64> {
    const KINDS: [&str; 8] = [
        "+1", "-1", "laugh", "hooray", "confused", "heart", "rocket", "eyes",
    ];
    KINDS
        .iter()
        .filter_map(|k| {
            let n = v?.get(*k)?.as_u64()?;
            (n > 0).then(|| (k.to_string(), n))
        })
        .collect()
}

fn tally(set: &BTreeSet<(String, String)>) -> BTreeMap<String, u64> {
    let mut m = BTreeMap::<String, u64>::new();
    for (_, c) in set {
        *m.entry(c.clone()).or_default() += 1;
    }
    m
}

/// Whether the item's body reactions differ from the record in `seen`.
/// No record yet is not a change: the next look records them as they are.
/// Neither is an item without counts (one built from the `/pulls` listing).
pub(in crate::engine) fn body_reactions_moved(
    seen: &BTreeMap<String, String>,
    issue: &Issue,
) -> bool {
    let Some(now) = issue.reactions.as_ref() else {
        return false;
    };
    seen.get("reactions:body")
        .is_some_and(|s| tally(&parse_reactions(s)) != reaction_counts(Some(now)))
}
