//! CLI request handling inside the daemon.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use tracing::{debug, error, info, warn};

use super::{Engine, github_state, mentions_bot};
use crate::config::RepoConfig;
use crate::ipc::{Refused, Request, Response};
use crate::origin::Origin;
use crate::state::Events;
use crate::status::session_id;

impl Engine {
    /// Answer one CLI connection.
    pub(super) async fn serve(&mut self, mut stream: tokio::net::UnixStream) {
        let resp = match crate::ipc::read_request(&mut stream).await {
            Ok(Some(req)) => {
                debug!(?req, "request from the CLI");
                let resp = self.handle_request(req).await;
                if let Err(e) = self.state.save() {
                    error!("saving state: {e:#}");
                }
                resp
            }
            // A peer that connected and closed without a request: a
            // liveness probe (a dashboard's `daemon_reachable`), which
            // has asked for nothing and is owed nothing.
            Ok(None) => return,
            Err(e) => Response::err(format!("bad request: {e:#}")),
        };
        if let Err(e) = crate::ipc::write_response(&mut stream, &resp).await {
            warn!("answering the CLI failed: {e:#}");
        }
    }

    /// `ssf sub|unsub`, run inside the daemon so the state and the
    /// delivery path are the daemon's own.
    pub async fn handle_request(&mut self, req: Request) -> Response {
        match req {
            Request::Ping => Response::ok(serde_json::json!({"login": self.login})),
            Request::Candidates { repo } => match self.candidates(repo.as_deref()) {
                Ok(v) => Response::ok(v),
                Err(e) => Response::refused(&e),
            },
            Request::Adopt { items } => match self.adopt(&items).await {
                Ok(v) => Response::ok(v),
                Err(e) => Response::refused(&e),
            },
            Request::Sub {
                from,
                target,
                events,
            } => match self.subscribe(&from, &target, events).await {
                Ok(v) => Response::ok(v),
                Err(e) => Response::refused(&e),
            },
            Request::Unsub { from, target } => match self.unsubscribe(&from, &target) {
                Ok(v) => Response::ok(v),
                Err(e) => Response::refused(&e),
            },
            Request::Release { session, force } => match self.release(&session, force).await {
                Ok(v) => Response::ok(v),
                Err(e) => Response::refused(&e),
            },
            Request::Handover {
                session,
                harness,
                model,
                effort,
                summary,
                by,
            } => match self
                .handover(
                    &session,
                    &harness,
                    model.as_deref(),
                    effort.as_deref(),
                    summary.as_deref(),
                    by.as_deref(),
                )
                .await
            {
                Ok(v) => Response::ok(v),
                Err(e) => Response::refused(&e),
            },
            Request::CancelHandover { session } => match self.cancel_handover(&session).await {
                Ok(v) => Response::ok(v),
                Err(e) => Response::refused(&e),
            },
            Request::Assign {
                item,
                harness,
                model,
                effort,
                by,
            } => match self
                .assign(
                    &item,
                    &harness,
                    model.as_deref(),
                    effort.as_deref(),
                    by.as_deref(),
                )
                .await
            {
                Ok(v) => Response::ok(v),
                Err(e) => Response::refused(&e),
            },
            Request::Purge {
                dry_run,
                older_than_days,
                force,
            } => match self.purge(dry_run, older_than_days, force).await {
                Ok(v) => Response::ok(v),
                Err(e) => Response::refused(&e),
            },
            Request::ScratchCreate {
                repo,
                harness,
                model,
                effort,
                owner_login,
            } => match self
                .create_scratch(
                    &repo,
                    &harness,
                    model.as_deref(),
                    effort.as_deref(),
                    owner_login.as_deref(),
                )
                .await
            {
                Ok(v) => Response::ok(v),
                Err(e) => Response::refused(&e),
            },
            Request::ScratchResume { session } => match self.resume_scratch(&session).await {
                Ok(v) => Response::ok(v),
                Err(e) => Response::refused(&e),
            },
        }
    }

    fn candidates(&self, filter: Option<&str>) -> Result<Value> {
        if let Some(name) = filter
            && !self.cfg.repos.iter().any(|r| r.matches_name(name))
        {
            anyhow::bail!("{name} is not a watched repository (see `ssf repo list`)");
        }
        let mut rows = Vec::new();
        for repo in &self.cfg.repos {
            if filter.is_some_and(|name| !repo.matches_name(name)) {
                continue;
            }
            let Some(rs) = self.state.repos.get(&repo.name) else {
                continue;
            };
            for candidate in rs.adoption_candidates.values() {
                rows.push(serde_json::json!({
                    "item": session_id(&repo.name, candidate.number),
                    "title": candidate.title,
                    "url": candidate.html_url,
                    "kind": candidate.kind,
                    "triggers": candidate.triggers,
                    "updated_at": candidate.updated_at,
                }));
            }
        }
        Ok(Value::Array(rows))
    }

    async fn adopt(&mut self, items: &[String]) -> Result<Value> {
        if items.is_empty() {
            anyhow::bail!("name at least one candidate as owner/repo#N");
        }
        let mut selected = Vec::new();
        let mut selected_refs = Vec::new();
        for item in items {
            let (repo, number) = self.locate(item)?;
            self.state
                .repos
                .get(&repo.name)
                .and_then(|rs| rs.adoption_candidates.get(&number))
                .with_context(|| {
                    format!("{item} is not waiting for adoption (see `ssf candidates`)")
                })?;
            if selected_refs
                .iter()
                .any(|(name, n): &(String, u64)| name == &repo.name && *n == number)
            {
                anyhow::bail!("{item} was named more than once");
            }
            selected_refs.push((repo.name.clone(), number));
            let (owner, name) = repo.split()?;
            self.refresh_collaborators(&repo, owner, name).await?;
            let issue = self.gh.issue(owner, name, number).await?;
            let timeline = self.gh.timeline(owner, name, number).await?;
            let pr = if issue.is_pull_request() {
                Some(self.gh.pull(owner, name, number).await?)
            } else {
                None
            };
            let mut triggers = Vec::new();
            if issue.is_assigned_to(&self.login) {
                triggers.push("assigned".into());
            }
            if mentions_bot(&issue, &timeline, &self.login) {
                triggers.push("mentioned".into());
            }
            if pr
                .as_ref()
                .is_some_and(|pull| pull.requests_review_from(&self.login))
            {
                triggers.push("review_requested".into());
            }
            let has_allocation = !triggers.is_empty();
            if issue.author().eq_ignore_ascii_case(&self.login) {
                triggers.push("created".into());
            }
            if issue.state == "closed" || !has_allocation {
                self.state
                    .repo_mut(&repo.name)
                    .adoption_candidates
                    .remove(&number);
                anyhow::bail!(
                    "{item} is no longer allocated to the bot; removed it from the candidates"
                );
            }
            if let Err(why) = self.gate(&repo, &issue, &timeline, &triggers) {
                anyhow::bail!("{item} is not eligible for adoption: {why}");
            }
            selected.push((repo, issue, pr, triggers));
        }

        let mut rows = Vec::new();
        for (repo, issue, pr, triggers) in selected {
            let number = issue.number;
            let prior = self.peek(&repo, number).cloned();
            if let Some(worktree) = prior.as_ref().and_then(|st| st.worktree_id.as_deref())
                && let Some(handle) = self
                    .driver(&repo)
                    .live_handle(
                        worktree,
                        prior.as_ref().and_then(|st| st.terminal_handle.as_deref()),
                    )
                    .await?
            {
                self.driver(&repo).stop_agent(worktree, &handle).await?;
            }
            {
                let st = self.entry(&repo, number);
                st.terminal_handle = None;
                st.agent_session_id = None;
            }
            let (owner, name) = repo.split()?;
            self.adopting = Some((repo.name.clone(), number));
            let reconciled = self
                .reconcile_issue(&repo, owner, name, &issue, pr, triggers)
                .await;
            self.adopting = None;
            reconciled?;
            if !self.peek(&repo, number).is_some_and(|st| st.seeded) {
                anyhow::bail!(
                    "{} was not eligible for a session in its current GitHub state; it remains a candidate",
                    session_id(&repo.name, number)
                );
            }
            let rs = self.state.repo_mut(&repo.name);
            rs.adoption_candidates.remove(&number);
            // Items created by this issue may have been seen before their
            // parent was adopted. Reconsider them now that their origin can bind.
            rs.ignored.clear();
            self.forget_etags(&repo);
            rows.push(serde_json::json!({
                "item": session_id(&repo.name, number),
                "title": issue.title,
            }));
        }
        Ok(Value::Array(rows))
    }

    /// A watched repository and an item number out of `owner/repo#N` (a
    /// session or item reference from the CLI). What it fails on is the
    /// reference itself: an item named in a shape ssf does not read, or a
    /// repository the factory does not watch.
    pub(in crate::engine) fn locate(&self, item: &str) -> Result<(RepoConfig, u64)> {
        let o = Origin::parse(item)
            .ok_or_else(|| Refused::bad_input(format!("{item}: expected owner/repo#N")))?;
        let repo = self
            .cfg
            .repos
            .iter()
            .find(|r| r.matches_name(&o.repo))
            .cloned()
            .with_context(|| format!("{} is not a watched repository", o.repo))
            .map_err(|e| Refused::bad_input(format!("{e:#}")))?;
        Ok((repo, o.number))
    }

    /// The session (`owner/repo#N`, normalised to the owning session)
    /// behind a session id the CLI gave. It must be one ssf has a workspace
    /// for. A reference ssf cannot read at all is the input's own fault
    /// ([`locate`]); one that reads but names no session ssf holds is the
    /// item's state, which is what the web API answers `409` for.
    pub(super) fn known_session(&self, id: &str) -> Result<(RepoConfig, u64, String)> {
        let (repo, n) = self.locate(id)?;
        let number = self.owner_of(&repo, n);
        let known = self.peek(&repo, number).is_some_and(|s| s.seeded);
        if !known {
            anyhow::bail!(Refused::conflict(format!(
                "{id} is not an agent session ssf knows (see `ssf peers --all`)"
            )));
        }
        let id = session_id(&repo.name, number);
        Ok((repo, number, id))
    }

    /// The session a subscription is made for: an item's (normalised to
    /// the owning session), or a scratch session.
    fn subscriber(&self, from: &str) -> Result<String> {
        if crate::origin::Scratch::parse(from).is_some() {
            return Ok(self.scratch(from)?.1.to_string());
        }
        Ok(self.known_session(from)?.2)
    }

    async fn subscribe(&mut self, from: &str, target: &str, events: Events) -> Result<Value> {
        let me = self.subscriber(from)?;
        let (repo, number) = self.locate(target)?;
        let (owner, name) = repo.split()?;
        let existing = self
            .state
            .repos
            .get(&repo.name)
            .and_then(|rs| rs.issues.get(&number))
            .cloned();
        let tracked = existing
            .as_ref()
            .is_some_and(|s| s.seeded || s.subscriber_only);
        if tracked && !existing.as_ref().unwrap().subscriber_only {
            let acting = session_id(&repo.name, self.owner_of(&repo, number));
            if acting.eq_ignore_ascii_case(&me) {
                anyhow::bail!("{target} is your own item (its session is {acting})");
            }
        }
        let (title, owner_session, kind, state) = if tracked {
            let st = existing.unwrap();
            let owner_session = if st.subscriber_only {
                None
            } else {
                Some(session_id(&repo.name, self.owner_of(&repo, number)))
            };
            (
                st.title.clone(),
                owner_session,
                st.kind.clone().unwrap_or_else(|| "issue".into()),
                st.github_state.clone().unwrap_or_else(|| "open".into()),
            )
        } else {
            // Nothing tracks it yet: start polling it for the subscriber,
            // from now on (what happened before is not new).
            let issue = self
                .gh
                .issue(owner, name, number)
                .await
                .with_context(|| format!("fetching {target}"))?;
            let timeline = self.gh.timeline(owner, name, number).await?;
            let seen = self.diff(&repo, &BTreeMap::new(), &timeline).seen;
            let is_pr = issue.is_pull_request();
            let e = self.entry(&repo, number);
            e.title = issue.title.clone();
            e.html_url = issue.html_url.clone();
            e.kind = Some(if is_pr {
                "pull_request".into()
            } else {
                "issue".into()
            });
            e.github_state = Some(github_state(&issue, None, false));
            e.updated_at = Some(issue.updated_at.clone());
            e.seen = seen;
            e.subscriber_only = true;
            e.active = false;
            info!(
                repo = repo.name,
                issue = number,
                "tracking for subscribers only"
            );
            (
                issue.title.clone(),
                None,
                e.kind.clone().unwrap(),
                github_state(&issue, None, false),
            )
        };
        let e = self.entry(&repo, number);
        // Following again is how a session changes its level, so what it
        // asks for here is what it hears from now on.
        let (added, changed) = e.subscribe(&me, events);
        info!(
            repo = repo.name,
            issue = number,
            subscriber = me,
            level = events.id(),
            added,
            "subscribed"
        );
        Ok(serde_json::json!({
            "item": session_id(&repo.name, number),
            "title": title,
            "kind": kind,
            "github_state": state,
            "owner": owner_session,
            "subscriber": me,
            "added": added,
            "changed": changed,
            "events": events.id(),
        }))
    }

    fn unsubscribe(&mut self, from: &str, target: &str) -> Result<Value> {
        let me = self.subscriber(from)?;
        let (repo, number) = self.locate(target)?;
        let Some(st) = self
            .state
            .repos
            .get(&repo.name)
            .and_then(|rs| rs.issues.get(&number))
            .cloned()
        else {
            anyhow::bail!("{target} is not tracked");
        };
        let e = self.entry(&repo, number);
        let removed = e.unsubscribe(&me);
        // An item nobody listens to any more is forgotten.
        let dropped = e.subscriber_only && e.subscribers.is_empty();
        if dropped {
            // Nobody listens any more.
            self.state.repo_mut(&repo.name).issues.remove(&number);
            info!(
                repo = repo.name,
                issue = number,
                "no subscribers left; no longer tracked"
            );
        }
        info!(
            repo = repo.name,
            issue = number,
            subscriber = me,
            removed,
            "unsubscribed"
        );
        Ok(serde_json::json!({
            "item": session_id(&repo.name, number),
            "title": st.title,
            "subscriber": me,
            "removed": removed,
            "untracked": dropped,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::{engine, repo, seeded};

    #[tokio::test]
    async fn subscriptions_through_requests() {
        let mut e = engine();
        let r = repo();
        e.cfg.repos.push(r.clone());
        seeded(&mut e, 1, Some("bot/issue-1"), true);
        seeded(&mut e, 3, Some("bot/issue-3"), true);
        e.entry(&r, 3).title = "Three".into();
        seeded(&mut e, 7, None, true);
        e.entry(&r, 7).shares_workspace_of = Some(1);

        // Session 1 follows item 3, hearing its state changes.
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#1".into(),
                target: "o/r#3".into(),
                events: Events::State,
            })
            .await;
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.data["owner"], "o/r#3");
        assert_eq!(resp.data["title"], "Three");
        assert_eq!(resp.data["added"], true);
        assert_eq!(resp.data["changed"], false);
        assert_eq!(resp.data["events"], "state");
        assert_eq!(e.entry(&r, 3).subscribers, vec!["o/r#1"]);
        // Again: no duplicate, and the level stands.
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#1".into(),
                target: "o/r#3".into(),
                events: Events::State,
            })
            .await;
        assert!(resp.ok);
        assert_eq!(resp.data["added"], false);
        assert_eq!(resp.data["changed"], false);
        assert_eq!(e.entry(&r, 3).subscribers.len(), 1);
        // Following it again is how a session asks for more: the level
        // moves without a second subscription.
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#1".into(),
                target: "o/r#3".into(),
                events: Events::All,
            })
            .await;
        assert!(resp.ok);
        assert_eq!(resp.data["added"], false);
        assert_eq!(resp.data["changed"], true);
        assert_eq!(resp.data["events"], "all");
        assert_eq!(e.entry(&r, 3).subscribers, vec!["o/r#1"]);
        assert_eq!(e.entry(&r, 3).events_for("o/r#1"), Events::All);
        // From the PR's identity it is still session 1, and its own items
        // are refused.
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#7".into(),
                target: "o/r#1".into(),
                events: Events::State,
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("your own item"));
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#7".into(),
                target: "o/r#7".into(),
                events: Events::State,
            })
            .await;
        assert!(!resp.ok);
        // Unknown sessions and repositories are refused.
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#99".into(),
                target: "o/r#3".into(),
                events: Events::State,
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("not an agent session"));
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#1".into(),
                target: "x/y#3".into(),
                events: Events::State,
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("not a watched repository"));
        let resp = e
            .handle_request(Request::Unsub {
                from: "o/r#1".into(),
                target: "nonsense".into(),
            })
            .await;
        assert!(!resp.ok);

        // A subscriber-only item is dropped with its last subscriber.
        {
            let s = e.entry(&r, 20);
            s.subscriber_only = true;
            s.subscribers = vec!["o/r#1".into(), "o/r#3".into()];
        }
        let resp = e
            .handle_request(Request::Unsub {
                from: "o/r#7".into(),
                target: "o/r#20".into(),
            })
            .await;
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.data["removed"], true);
        assert_eq!(resp.data["untracked"], false);
        assert_eq!(e.entry(&r, 20).subscribers, vec!["o/r#3"]);
        let resp = e
            .handle_request(Request::Unsub {
                from: "o/r#3".into(),
                target: "o/r#20".into(),
            })
            .await;
        assert!(resp.ok);
        assert_eq!(resp.data["untracked"], true);
        assert!(!e.state.repos["o/r"].issues.contains_key(&20));
        // Unsubscribing from something never followed is fine.
        let resp = e
            .handle_request(Request::Unsub {
                from: "o/r#3".into(),
                target: "o/r#1".into(),
            })
            .await;
        assert!(resp.ok);
        assert_eq!(resp.data["removed"], false);

        assert!(e.handle_request(Request::Ping).await.ok);
    }
}
