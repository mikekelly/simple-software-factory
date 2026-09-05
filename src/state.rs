//! Persistent daemon state: which issues are bound to which Orca workspaces,
//! and which timeline events have already been delivered.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{state_dir, write_atomic};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct State {
    /// Login of the bot account the daemon last authenticated as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_login: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_poll_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub repos: BTreeMap<String, RepoState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepoState {
    /// ETags and contents of the last successful listings, per trigger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issues_etag: Option<String>,
    #[serde(default)]
    pub assigned_numbers: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mentioned_etag: Option<String>,
    #[serde(default)]
    pub mentioned_numbers: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pulls_etag: Option<String>,
    #[serde(default)]
    pub review_numbers: Vec<u64>,
    /// Open items the bot account opened (a session's own issues and PRs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_etag: Option<String>,
    #[serde(default)]
    pub created_numbers: Vec<u64>,
    #[serde(default)]
    pub issues: BTreeMap<u64, IssueState>,
    /// Reviewer sessions, keyed by the pull request they review: a second
    /// workspace on the PR's branch with an agent that only reviews, started
    /// when a review is requested from the bot on a PR one of its own
    /// sessions wrote. Same record shape as an item, but the session id is
    /// `owner/repo#N:reviewer` and it never owns anything.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reviewers: BTreeMap<u64, IssueState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IssueState {
    pub number: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub html_url: String,
    /// Full Orca worktree id (`<repoId>::<path>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    /// Last known terminal handle running the harness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_handle: Option<String>,
    /// Orca repo id the workspace belongs to (first half of the worktree id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<String>,
    /// Name the workspace was created with, reused when it is re-created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_name: Option<String>,
    /// Git branch of the workspace, used as the base when re-creating it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Harness conversation id (Claude Code / Codex) for `--resume`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    /// When the harness was last launched, to find its session file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launched_at: Option<String>,
    /// Reviewer workspace to be removed once its agent has wrapped up.
    /// Item workspaces are never removed on this flag any more (see
    /// `release_pending`); a stale `true` on one is cleared.
    #[serde(default)]
    pub cleanup_pending: bool,
    /// `ssf release` passed its checks: the workspace is removed on the
    /// daemon's next pass, after the checks are run once more.
    #[serde(default)]
    pub release_pending: bool,
    /// When the workspace was removed by `ssf release` or `ssf purge`.
    /// Cleared when the next event re-creates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<String>,
    /// `updated_at` of the issue when the timeline was last reconciled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Delivered timeline events: key -> updated_at marker (for edit detection).
    #[serde(default)]
    pub seen: BTreeMap<String, String>,
    /// The initial prompt has been delivered.
    #[serde(default)]
    pub seeded: bool,
    /// Still assigned and open as of the last poll.
    #[serde(default)]
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_prompt_at: Option<String>,
    #[serde(default)]
    pub prompts_sent: u64,
    /// `issue` or `pull_request`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// GitHub state as of the last poll: `open`, `closed` or `merged`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_state: Option<String>,
    /// Why the bot got involved: assigned, mentioned, review_requested,
    /// created (opened by the bot itself); review_label on a reviewer
    /// session started by the review label.
    #[serde(default)]
    pub triggers: Vec<String>,
    /// Pull request branch details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<crate::github::PrInfo>,
    /// Open project boards the item is on, as of the last lookup.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<crate::github::ProjectCard>,
    /// This item is owned by that item's session (same repo): it was opened
    /// from that session, or its PR branch is that session's branch. Every
    /// prompt about this item goes to the owner's agent, and the workspace
    /// lifecycle belongs to the owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shares_workspace_of: Option<u64>,
    /// Session (`owner/repo#N`) that opened this item as a hand-off
    /// (`mode=delegate`). It gets one message when this item closes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_by: Option<String>,
    /// The closing message has been sent to `delegated_by`.
    #[serde(default)]
    pub parent_notified: bool,
    /// Origin tag in the item's own body: the session that opened it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Timeline event key -> origin of the comment or review that carried a tag.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub origins: BTreeMap<String, String>,
    /// Posts by the bot without a tag (event key, or `body`, -> URL); the gh
    /// shim was not in effect in whichever session made them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub untagged: BTreeMap<String, String>,
    /// Sessions (`owner/repo#N`, always an owning session) that hear about
    /// this item without acting on it: every delivery is fanned out to them
    /// with FYI framing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscribers: Vec<String>,
    /// Tracked only because sessions subscribed to it: polled for activity,
    /// but no workspace, no owner and no session of its own.
    #[serde(default)]
    pub subscriber_only: bool,
}

pub fn state_path() -> PathBuf {
    state_dir().join("state.json")
}

impl State {
    pub fn load() -> Result<Self> {
        Self::load_from(&state_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&state_path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let body = serde_json::to_vec_pretty(self)?;
        write_atomic(path, &body, 0o600)
    }

    pub fn repo_mut(&mut self, name: &str) -> &mut RepoState {
        self.repos.entry(name.to_string()).or_default()
    }

    /// Drop `session` from every subscriber list, in every repository.
    /// Returns the items (`owner/repo#N`) it was subscribed to.
    pub fn unsubscribe_everywhere(&mut self, session: &str) -> Vec<String> {
        let mut dropped = Vec::new();
        for (repo, rs) in self.repos.iter_mut() {
            for st in rs.issues.values_mut() {
                let before = st.subscribers.len();
                st.subscribers.retain(|s| !s.eq_ignore_ascii_case(session));
                if st.subscribers.len() != before {
                    dropped.push(format!("{repo}#{}", st.number));
                }
            }
        }
        dropped
    }

    /// The reviewer session record for pull request `number`, if any.
    pub fn reviewer(&self, repo: &str, number: u64) -> Option<&IssueState> {
        self.repos.get(repo)?.reviewers.get(&number)
    }
}

pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsubscribe_everywhere_spans_repositories() {
        let mut st = State::default();
        for (repo, n, subs) in [
            ("a/b", 1, vec!["a/b#9", "x/y#2"]),
            ("a/b", 2, vec!["A/B#9"]),
            ("x/y", 3, vec!["x/y#2"]),
        ] {
            let e = st.repo_mut(repo).issues.entry(n).or_default();
            e.number = n;
            e.subscribers = subs.into_iter().map(String::from).collect();
        }
        let dropped = st.unsubscribe_everywhere("a/b#9");
        assert_eq!(dropped, vec!["a/b#1", "a/b#2"]);
        assert_eq!(st.repos["a/b"].issues[&1].subscribers, vec!["x/y#2"]);
        assert!(st.repos["a/b"].issues[&2].subscribers.is_empty());
        assert_eq!(st.repos["x/y"].issues[&3].subscribers, vec!["x/y#2"]);
        assert!(st.unsubscribe_everywhere("nobody#1").is_empty());
        // Round-trips through JSON with the new fields.
        st.repos
            .get_mut("a/b")
            .unwrap()
            .issues
            .get_mut(&1)
            .unwrap()
            .subscriber_only = true;
        let rv = IssueState {
            number: 1,
            seeded: true,
            ..Default::default()
        };
        st.repo_mut("a/b").reviewers.insert(1, rv);
        let back: State = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
        assert!(back.repos["a/b"].issues[&1].subscriber_only);
        assert_eq!(back.repos["a/b"].issues[&1].subscribers, vec!["x/y#2"]);
        assert!(back.reviewer("a/b", 1).unwrap().seeded);
        assert!(back.reviewer("a/b", 2).is_none());
        assert!(back.reviewer("x/y", 1).is_none());
        // An empty reviewer map is not written out.
        let plain: State = serde_json::from_str(r#"{"repos":{"a/b":{}}}"#).unwrap();
        assert!(!serde_json::to_string(&plain).unwrap().contains("reviewers"));
    }
}
