//! Persistent daemon state: which issues are bound to which Orca workspaces,
//! and which timeline events have already been delivered.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{state_dir, write_atomic};
use tracing::info;

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
    /// Items the bot opened that nothing binds to a session (no origin
    /// tag, no branch match, no human trigger), keyed by number, with what
    /// they were last looked at with (see [`Ignored`]). Kept here rather
    /// than in memory so a daemon restart does not queue a walk of every
    /// such item: the listing ETags survive a restart, so the first pass
    /// after one on which any listing has changed would otherwise fetch
    /// each of them (issue and timeline) again to find nothing new.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ignored: BTreeMap<u64, Ignored>,
    /// Reviewer sessions from before #115 (a second workspace per pull
    /// request, gone since): read so an old file still loads, dropped with
    /// one log line by [`State::load_from`], never written back.
    #[serde(default, rename = "reviewers", skip_serializing)]
    pub legacy_reviewers: BTreeMap<u64, serde_json::Value>,
}

/// What an ignored item looked like when it was last examined: GitHub's
/// `updated_at` and the listings it was on. Both are the key, because an
/// assignment (or a mention, or a review request) can be older than the
/// `updated_at` the item was first seen with: when the bot has just opened
/// an item and is assigned to it in the same interval, the first pass may
/// meet it through the creator listing alone (the assignee listing being a
/// 304 against an ETag from before the assignment), ignore it as
/// created-only, and then find nothing "changed" when the assignee listing
/// does carry it. Showing up on another listing is a change for our
/// purposes even when `updated_at` stands still.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Ignored {
    pub updated_at: String,
    /// Sorted, so two listings in any order compare equal.
    #[serde(default)]
    pub triggers: Vec<String>,
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
    /// The driver's id for the repository the workspace belongs to: Orca's
    /// repo id (the first half of the worktree id) or, for herdr, the path
    /// of the checkout. Only meaningful to the driver named in `driver`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<String>,
    /// The driver (`orca`, `herdr`) that made the workspace and wrote
    /// `repo_id` and `worktree_id`. A record from before this was kept has
    /// none, and is judged by the shape of its `repo_id` when the driver of
    /// the repository has changed since (see `Engine::drop_foreign_binding`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    /// Name the workspace was created with, reused when it is re-created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_name: Option<String>,
    /// Git branch of the workspace, used as the base when re-creating it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Harness conversation id (Claude Code / Codex) for `--resume`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    /// Conversations of this item that must never be resumed or captured
    /// again: what a handover retired (`Engine::finish_handover`). The
    /// old harness's transcript is the newest one in the workspace when
    /// the new harness starts there, so without this the new session
    /// would be given the outgoing agent's conversation id and every
    /// later relaunch would resume the agent that handed the item away.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_session_ids: Vec<String>,
    /// When a handover last retired a conversation on this item. What it
    /// says is that the workspace holds a transcript that is not this
    /// session's: the newest one there was written by the agent that
    /// handed the item away, so the moments before a launch are no longer
    /// a safe place to look for the new session's own (see
    /// `Engine::capture_sessions`). Cleared when the workspace is
    /// released or the item purged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handed_over_at: Option<String>,
    /// When the harness was last launched, to find its session file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launched_at: Option<String>,
    /// No longer set: an older daemon marked a workspace to be removed on
    /// close with it (see `release_pending` for how a workspace goes now).
    /// A stale `true` is cleared on the next pass.
    #[serde(default)]
    pub cleanup_pending: bool,
    /// `ssf release` passed its checks: the workspace is removed on the
    /// daemon's next pass, after the checks are run once more.
    #[serde(default)]
    pub release_pending: bool,
    /// The pending release was forced by a person: the pass removes the
    /// workspace without running the checks again.
    #[serde(default)]
    pub release_forced: bool,
    /// When the workspace was removed by `ssf release` or `ssf purge`.
    /// Cleared when the next event re-creates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_at: Option<String>,
    /// Releases the daemon's own re-check refused (the tree changed after
    /// `ssf release` passed its checks). Each is reported to the agent up
    /// to a cap, after which the workspace is left for a person.
    #[serde(default)]
    pub release_refusals: u32,
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
    /// created (opened by the bot itself).
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
    /// Per-item launch overrides: the harness, model and effort this
    /// item's session runs with, whatever the repository is configured
    /// with. Written by a handover (`ssf handover`), used by every later
    /// launch, resume and re-creation, cleared when the workspace is
    /// released or the item purged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides: Option<Overrides>,
    /// A handover the daemon has accepted and not carried out yet: the
    /// next pass ends this session and starts the new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handover: Option<PendingHandover>,
    /// What a handover left for the session that takes the item on, kept
    /// until a session has actually been given it. The start that
    /// follows a handover can fail, or come up at a sign-in screen, and
    /// the outgoing agent is gone by then: without this the summary it
    /// wrote would be lost and the harness started again would be given
    /// the item's story alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handover_note: Option<HandoverNote>,
    /// The session's harness cannot act: its screen shows a login prompt
    /// (see [`Blocked`]). Nothing is delivered while this is set; the
    /// daemon checks every pass whether the login is back and resumes the
    /// session itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<Blocked>,
}

/// What an item's session runs with instead of the repository's own
/// settings (`ssf handover`). `model` and `effort` unset mean the
/// harness's own defaults, not the repository's, when the harness
/// differs; see `Engine::effective`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Overrides {
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// A handover `ssf handover` recorded on an item: what the new session
/// runs with, what the outgoing agent wrote for it, and who asked. The
/// daemon carries it out on its next pass and clears it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PendingHandover {
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// What the outgoing agent left for the new one; `None` for
    /// `--no-summary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The session that asked (`owner/repo#N`); `None` for a person at a
    /// shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    pub requested_at: String,
}

impl PendingHandover {
    /// The overrides the item keeps once the handover has been carried out.
    pub fn overrides(&self) -> Overrides {
        Overrides {
            harness: self.harness.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
        }
    }
}

/// The parting words of a handover, kept on the item until a session has
/// read them (see [`IssueState::handover_note`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HandoverNote {
    /// Display name of the harness the item was handed over from.
    pub from: String,
    /// What the outgoing agent wrote; `None` for `--no-summary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Why a session cannot take prompts, and what has been done about it.
/// Two reasons: the harness is not signed in (its login expired, was
/// revoked, or was never there), or the harness could not be started at
/// all (a handover to a harness that exits the moment it is launched).
/// The record keeps what the screen or the driver said, when it was
/// seen, whether the item has been told, the credential file's identity
/// at the time (a new login rewrites it) and when the harness was last
/// started again to check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Blocked {
    /// `login` or `start`.
    pub reason: String,
    /// The harness that showed the prompt, or would not start.
    #[serde(default)]
    pub harness: String,
    /// The screen line that gave it away, or the start error.
    #[serde(default)]
    pub detail: String,
    pub since: String,
    /// A comment saying so has been left on the session's item.
    #[serde(default)]
    pub reported: bool,
    /// `login::fingerprint` of the credential when the block was noticed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
    /// When the harness was last started again to see whether the login
    /// is back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retried_at: Option<String>,
    /// How many such restarts came back to the prompt: the wait before
    /// the next doubles each time (from ten minutes, capped at an hour).
    #[serde(default)]
    pub retries: u32,
}

impl Blocked {
    pub const LOGIN: &'static str = "login";
    /// The harness could not be started in the workspace at all.
    pub const START: &'static str = "start";
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
        let mut st: Self =
            serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        st.drop_legacy_reviewers();
        Ok(st)
    }

    /// Forget the reviewer records an older daemon wrote (see
    /// `RepoState::legacy_reviewers`), and the subscriptions those sessions
    /// held (`owner/repo#N:reviewer` is no session id now, and a subscriber
    /// that is not a session is skipped with a warning on every delivery),
    /// saying once which ones.
    fn drop_legacy_reviewers(&mut self) {
        let mut ghosts = Vec::new();
        for (repo, rs) in self.repos.iter_mut() {
            if rs.legacy_reviewers.is_empty() {
                continue;
            }
            let numbers: Vec<u64> = rs.legacy_reviewers.keys().copied().collect();
            info!(
                repo,
                ?numbers,
                "dropping reviewer session records from an older ssf; ssf runs one session per item now"
            );
            rs.legacy_reviewers.clear();
        }
        for rs in self.repos.values_mut() {
            for st in rs.issues.values_mut() {
                st.subscribers.retain(|s| {
                    let keep = crate::origin::Origin::parse(s).is_some();
                    if !keep {
                        ghosts.push(s.clone());
                    }
                    keep
                });
            }
            // An item tracked only for a subscriber that is gone is not
            // tracked at all (what `unsubscribe` does when the last one
            // leaves): dropped when it never had a session, otherwise
            // back to an ordinary retired record.
            rs.issues
                .retain(|_, st| !(st.subscriber_only && st.subscribers.is_empty() && !st.seeded));
            for st in rs.issues.values_mut() {
                if st.subscriber_only && st.subscribers.is_empty() {
                    st.subscriber_only = false;
                }
            }
        }
        if !ghosts.is_empty() {
            ghosts.sort();
            ghosts.dedup();
            info!(
                subscribers = ?ghosts,
                "dropping subscriptions held by sessions that no longer exist"
            );
        }
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
        let back: State = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
        assert!(back.repos["a/b"].issues[&1].subscriber_only);
        assert_eq!(back.repos["a/b"].issues[&1].subscribers, vec!["x/y#2"]);
    }

    #[test]
    fn handover_fields_are_optional_and_round_trip() {
        let dir = std::env::temp_dir().join(format!("ssf-handover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        // A file from before handovers loads unchanged.
        std::fs::write(
            &path,
            r#"{"repos":{"a/b":{"issues":{"1":{"number":1,"seeded":true,"active":true}}}}}"#,
        )
        .unwrap();
        let mut st = State::load_from(&path).unwrap();
        let one = st.repos.get_mut("a/b").unwrap().issues.get_mut(&1).unwrap();
        assert!(one.overrides.is_none() && one.handover.is_none());
        one.overrides = Some(Overrides {
            harness: "pi".into(),
            model: Some("openai/gpt-6".into()),
            effort: None,
        });
        one.handover = Some(PendingHandover {
            harness: "codex".into(),
            model: None,
            effort: Some("high".into()),
            summary: Some("what is left".into()),
            by: Some("a/b#1".into()),
            requested_at: "2026-09-07T10:00:00Z".into(),
        });
        st.save_to(&path).unwrap();
        let back = State::load_from(&path).unwrap();
        let one = &back.repos["a/b"].issues[&1];
        assert_eq!(one.overrides.as_ref().unwrap().harness, "pi");
        assert!(one.overrides.as_ref().unwrap().effort.is_none());
        let h = one.handover.as_ref().unwrap();
        assert_eq!(h.harness, "codex");
        assert_eq!(h.summary.as_deref(), Some("what is left"));
        assert_eq!(h.overrides().effort.as_deref(), Some("high"));
        // Nothing set writes neither key.
        st.repos
            .get_mut("a/b")
            .unwrap()
            .issues
            .get_mut(&1)
            .unwrap()
            .overrides = None;
        st.repos
            .get_mut("a/b")
            .unwrap()
            .issues
            .get_mut(&1)
            .unwrap()
            .handover = None;
        st.save_to(&path).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            !written.contains("overrides") && !written.contains("handover"),
            "{written}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reviewer_records_from_an_older_daemon_are_dropped_on_load() {
        let dir = std::env::temp_dir().join(format!("ssf-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::fs::write(
            &path,
            r#"{"repos":{"a/b":{"issues":{"1":{"number":1,"seeded":true,
                "subscribers":["a/b#7:reviewer","x/y#2"]},
                "3":{"number":3,"subscriber_only":true,"subscribers":["a/b#7:reviewer"]},
                "4":{"number":4,"seeded":true,"subscriber_only":true,"subscribers":["a/b#7:reviewer"]},
                "5":{"number":5,"subscriber_only":true,"subscribers":["a/b#1"]}},
                "reviewers":{"7":{"number":7,"seeded":true,"kind":"reviewer"}}}}}"#,
        )
        .unwrap();
        let st = State::load_from(&path).unwrap();
        assert!(st.repos["a/b"].issues[&1].seeded);
        assert!(st.repos["a/b"].legacy_reviewers.is_empty());
        assert_eq!(
            st.repos["a/b"].issues[&1].subscribers,
            vec!["x/y#2"],
            "the reviewer's own subscriptions go with it"
        );
        assert!(
            !st.repos["a/b"].issues.contains_key(&3),
            "tracked only for the reviewer: not tracked any more"
        );
        let four = &st.repos["a/b"].issues[&4];
        assert!(!four.subscriber_only && four.subscribers.is_empty() && four.seeded);
        assert_eq!(st.repos["a/b"].issues[&5].subscribers, vec!["a/b#1"]);
        assert!(st.repos["a/b"].issues[&5].subscriber_only);
        st.save_to(&path).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.contains("reviewers"), "{written}");
        // A file whose reviewer records were already dropped by an earlier
        // load can still carry their subscriptions.
        std::fs::write(
            &path,
            r#"{"repos":{"a/b":{"issues":{"1":{"number":1,"seeded":true,
                "subscribers":["a/b#7:reviewer"]}}}}}"#,
        )
        .unwrap();
        let st = State::load_from(&path).unwrap();
        assert!(st.repos["a/b"].issues[&1].subscribers.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
