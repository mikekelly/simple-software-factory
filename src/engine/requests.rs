//! CLI request handling inside the daemon.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use tracing::{debug, error, info, warn};

use super::{Engine, github_state};
use crate::config::RepoConfig;
use crate::ipc::{Request, Response};
use crate::origin::Origin;
use crate::prompt;
use crate::state::now_iso;
use crate::status::session_id;

impl Engine {
    /// Answer one CLI connection.
    pub(super) async fn serve(&mut self, mut stream: tokio::net::UnixStream) {
        let resp = match crate::ipc::read_request(&mut stream).await {
            Ok(req) => {
                debug!(?req, "request from the CLI");
                let resp = self.handle_request(req).await;
                if let Err(e) = self.state.save() {
                    error!("saving state: {e:#}");
                }
                resp
            }
            Err(e) => Response::err(format!("bad request: {e:#}")),
        };
        if let Err(e) = crate::ipc::write_response(&mut stream, &resp).await {
            warn!("answering the CLI failed: {e:#}");
        }
    }

    /// `ssf sub|unsub|tell`, run inside the daemon so the state and the
    /// delivery path are the daemon's own.
    pub async fn handle_request(&mut self, req: Request) -> Response {
        match req {
            Request::Ping => Response::ok(serde_json::json!({"login": self.login})),
            Request::Sub { from, target } => match self.subscribe(&from, &target).await {
                Ok(v) => Response::ok(v),
                Err(e) => Response::err(format!("{e:#}")),
            },
            Request::Unsub { from, target } => match self.unsubscribe(&from, &target) {
                Ok(v) => Response::ok(v),
                Err(e) => Response::err(format!("{e:#}")),
            },
            Request::Tell { from, target, text } => {
                match self.tell(from.as_deref(), &target, &text).await {
                    Ok(v) => Response::ok(v),
                    Err(e) => Response::err(format!("{e:#}")),
                }
            }
            Request::Release { session, force } => match self.release(&session, force).await {
                Ok(v) => Response::ok(v),
                Err(e) => Response::err(format!("{e:#}")),
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
                Err(e) => Response::err(format!("{e:#}")),
            },
            Request::CancelHandover { session } => match self.cancel_handover(&session).await {
                Ok(v) => Response::ok(v),
                Err(e) => Response::err(format!("{e:#}")),
            },
            Request::Purge {
                dry_run,
                older_than_days,
                force,
            } => match self.purge(dry_run, older_than_days, force).await {
                Ok(v) => Response::ok(v),
                Err(e) => Response::err(format!("{e:#}")),
            },
        }
    }

    /// A watched repository and an item number out of `owner/repo#N` (a
    /// session or item reference from the CLI).
    fn locate(&self, item: &str) -> Result<(RepoConfig, u64)> {
        let o = Origin::parse(item).with_context(|| format!("{item}: expected owner/repo#N"))?;
        let repo = self
            .cfg
            .repos
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(&o.repo))
            .cloned()
            .with_context(|| format!("{} is not a watched repository", o.repo))?;
        Ok((repo, o.number))
    }

    /// The session (`owner/repo#N`, normalised to the owning session)
    /// behind a session id the CLI gave. It must be one ssf has a workspace
    /// for.
    pub(super) fn known_session(&self, id: &str) -> Result<(RepoConfig, u64, String)> {
        let (repo, n) = self.locate(id)?;
        let number = self.owner_of(&repo, n);
        let known = self.peek(&repo, number).is_some_and(|s| s.seeded);
        if !known {
            anyhow::bail!("{id} is not an agent session ssf knows (see `ssf peers --all`)");
        }
        let id = session_id(&repo.name, number);
        Ok((repo, number, id))
    }

    async fn subscribe(&mut self, from: &str, target: &str) -> Result<Value> {
        let (_, _, me) = self.known_session(from)?;
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
        let added = if e.subscribers.iter().any(|s| s.eq_ignore_ascii_case(&me)) {
            false
        } else {
            e.subscribers.push(me.clone());
            true
        };
        info!(
            repo = repo.name,
            issue = number,
            subscriber = me,
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
        }))
    }

    fn unsubscribe(&mut self, from: &str, target: &str) -> Result<Value> {
        let (_, _, me) = self.known_session(from)?;
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
        let removed = st.subscribers.iter().any(|s| s.eq_ignore_ascii_case(&me));
        let e = self.entry(&repo, number);
        e.subscribers.retain(|s| !s.eq_ignore_ascii_case(&me));
        let dropped = e.subscriber_only && e.subscribers.is_empty();
        if dropped {
            // Nobody listens any more and nothing else remembers it.
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

    /// Paste a message into the terminal of the session acting on `target`.
    pub(super) async fn tell(
        &mut self,
        from: Option<&str>,
        target: &str,
        text: &str,
    ) -> Result<Value> {
        if text.trim().is_empty() {
            anyhow::bail!("nothing to say");
        }
        let (repo, number) = self.locate(target)?;
        let st = self
            .peek(&repo, number)
            .cloned()
            .filter(|s| s.seeded)
            .with_context(|| format!("{target} has no agent session (see `ssf peers --all`)"))?;
        let acting = self.owner_of(&repo, number);
        let ost = self.entry(&repo, acting).clone();
        let alive = match ost.worktree_id.as_deref() {
            Some(id) => self
                .driver(&repo)
                .worktree_exists(id)
                .await
                .unwrap_or(false),
            None => false,
        };
        if !ost.active && !alive {
            anyhow::bail!(
                "the session on {target} ({}) has retired and its workspace is gone",
                session_id(&repo.name, acting)
            );
        }
        if let Some(h) = ost.handover.as_ref() {
            anyhow::bail!(
                "a handover to {} is pending on {} ({}); the session is about to be replaced",
                h.harness,
                target,
                session_id(&repo.name, acting)
            );
        }
        let (sender, sender_title) = match from {
            Some(f) => {
                let (frepo, fnumber, fid) = self.known_session(f)?;
                let title = self.entry(&frepo, fnumber).title.clone();
                (Some(fid), Some(title).filter(|t| !t.is_empty()))
            }
            None => (None, None),
        };
        let prompt = prompt::tell_prompt(
            sender.as_deref(),
            sender_title.as_deref(),
            text,
            self.cfg.daemon.max_body_chars,
        );
        let d = self.deliver_to(&repo, number, &prompt, None).await?;
        let e = self.entry(&repo, acting);
        e.last_prompt_at = Some(now_iso());
        e.prompts_sent += 1;
        info!(
            repo = repo.name,
            issue = number,
            session = session_id(&repo.name, acting),
            from = sender.as_deref().unwrap_or("a human"),
            "delivered a message"
        );
        Ok(serde_json::json!({
            "item": session_id(&repo.name, number),
            "title": st.title,
            "session": session_id(&repo.name, acting),
            "terminal": d.handle,
            "relaunched": d.relaunched,
            "from": sender,
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

        // Session 1 follows item 3.
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#1".into(),
                target: "o/r#3".into(),
            })
            .await;
        assert!(resp.ok, "{:?}", resp.error);
        assert_eq!(resp.data["owner"], "o/r#3");
        assert_eq!(resp.data["title"], "Three");
        assert_eq!(resp.data["added"], true);
        assert_eq!(e.entry(&r, 3).subscribers, vec!["o/r#1"]);
        // Again: no duplicate.
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#1".into(),
                target: "o/r#3".into(),
            })
            .await;
        assert!(resp.ok);
        assert_eq!(resp.data["added"], false);
        assert_eq!(e.entry(&r, 3).subscribers.len(), 1);
        // From the PR's identity it is still session 1, and its own items
        // are refused.
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#7".into(),
                target: "o/r#1".into(),
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("your own item"));
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#7".into(),
                target: "o/r#7".into(),
            })
            .await;
        assert!(!resp.ok);
        // Unknown sessions and repositories are refused.
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#99".into(),
                target: "o/r#3".into(),
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("not an agent session"));
        let resp = e
            .handle_request(Request::Sub {
                from: "o/r#1".into(),
                target: "x/y#3".into(),
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

        // Telling a retired session with no workspace is refused, and an
        // empty message too.
        let resp = e
            .handle_request(Request::Tell {
                from: None,
                target: "o/r#3".into(),
                text: "  ".into(),
            })
            .await;
        assert!(!resp.ok);
        e.entry(&r, 3).active = false;
        let resp = e
            .handle_request(Request::Tell {
                from: Some("o/r#1".into()),
                target: "o/r#3".into(),
                text: "hello".into(),
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("retired"));
        let resp = e
            .handle_request(Request::Tell {
                from: None,
                target: "o/r#50".into(),
                text: "hello".into(),
            })
            .await;
        assert!(!resp.ok);
        assert!(resp.error.unwrap().contains("no agent session"));
        assert!(e.handle_request(Request::Ping).await.ok);
    }
}
