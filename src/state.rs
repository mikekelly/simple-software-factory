//! Persistent daemon state: which issues are bound to which herdr workspaces,
//! and which timeline events have already been delivered.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
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
    /// The repository enrollment generation whose pre-existing allocations
    /// have been discovered. A different value means `repo add` enrolled the
    /// repository again and its current allocations need explicit adoption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrollment_seen: Option<String>,
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
    /// Items the bot opened that nothing binds to a session (an unassigned
    /// issue, or no origin tag, branch match or human trigger), keyed by
    /// number, with what
    /// they were last looked at with (see [`Ignored`]). Kept here rather
    /// than in memory so a daemon restart does not queue a walk of every
    /// such item: the listing ETags survive a restart, so the first pass
    /// after one on which any listing has changed would otherwise fetch
    /// each of them (issue and timeline) again to find nothing new.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ignored: BTreeMap<u64, Ignored>,
    /// Allocations that already existed when this factory first enrolled the
    /// repository. They do not own a workspace until a person adopts them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub adoption_candidates: BTreeMap<u64, AdoptionCandidate>,
    /// Scratch sessions (`owner/repo~id`), keyed by id: agent sessions on
    /// the repository that work on no item. Kept apart from `issues` so
    /// nothing an item goes through -- retirement, purge, the closed and
    /// merged rules, dependents -- ever reaches one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scratch: BTreeMap<String, ScratchState>,
    /// URLs of the Projects v2 linked to the repository, as last read
    /// (#499), and when they were read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects_checked_at: Option<String>,
    /// Reviewer sessions from before #115 (a second workspace per pull
    /// request, gone since): read so an old file still loads, dropped with
    /// one log line by [`State::load_from`], never written back.
    #[serde(default, rename = "reviewers", skip_serializing)]
    pub legacy_reviewers: BTreeMap<u64, serde_json::Value>,
}

impl RepoState {
    /// Forget an item: its ignore record, and the record of the item
    /// itself when that answers to nothing (see
    /// [`IssueState::answers_to_nothing`]). A record with a session, a
    /// subscriber, a workspace or something pending stays: each of those
    /// still answers to something once the item stops being looked at.
    pub fn forget(&mut self, number: u64) {
        self.ignored.remove(&number);
        if self
            .issues
            .get(&number)
            .is_some_and(IssueState::answers_to_nothing)
        {
            self.issues.remove(&number);
        }
    }

    /// The items whose record answers to nothing (see
    /// [`IssueState::answers_to_nothing`]).
    fn records_answering_to_nothing(&self) -> Vec<u64> {
        self.issues
            .values()
            .filter(|st| st.answers_to_nothing())
            .map(|st| st.number)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptionCandidate {
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub updated_at: String,
    pub kind: String,
    #[serde(default)]
    pub triggers: Vec<String>,
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
/// purposes even when `updated_at` stands still. Leaving one is not:
/// nothing has happened to the item, and a listing that comes back short
/// would otherwise put everything on it through onboarding again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Ignored {
    pub updated_at: String,
    /// Sorted, so two listings in any order compare equal.
    #[serde(default)]
    pub triggers: Vec<String>,
    /// When the item stopped being on any listing, if it is not on one
    /// now: a record whose item is open but stays off every listing is
    /// given up eventually, and the clock is kept here rather than in
    /// memory so a daemon restart does not set it back to zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absent_since: Option<String>,
    /// When GitHub was last asked what became of the item, while it was
    /// off the listings. Kept across the item coming back, so an item
    /// whose listing flaps is asked about at the rate the absence
    /// deserves rather than once per flap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Events {
    /// The item's own state changing: closed, merged, reopened, assigned,
    /// labeled, renamed, its review request moving. What a follower has to
    /// know, and nothing it has to read.
    #[default]
    State,
    /// Everything on the item, comments and reviews included. For a
    /// follower that really wants to watch what is said on it.
    All,
}

impl Events {
    /// The value the CLI takes (`ssf sub --events <value>`).
    pub fn id(self) -> &'static str {
        match self {
            Events::State => "state",
            Events::All => "all",
        }
    }
}

impl std::str::FromStr for Events {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "state" => Ok(Events::State),
            "all" => Ok(Events::All),
            other => bail!(
                "unknown --events value {other:?}; use state (the item's own state changes) or \
all (everything, comments included)"
            ),
        }
    }
}

impl std::fmt::Display for Events {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IssueState {
    pub number: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub html_url: String,
    /// Full driver workspace id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    /// Last known terminal handle running the harness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_handle: Option<String>,
    /// The checkout path for the repository the workspace belongs to.
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
    /// `Engine::capture_sessions`). A release or purge leaves it: it also
    /// tells a handover's overrides from an assignment's, and the
    /// transcript it guards went with the workspace. A handover also
    /// clears [`IssueState::assigned_at`], the stamp an assignment leaves
    /// on the overrides, so the two commands can be told apart by which
    /// stamp is the newer writer's.
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
    /// When a retirement was last held because the item itself still
    /// carried one of the triggers it was taken on, while the listings had
    /// dropped it. The listings stay wrong until GitHub's end catches up,
    /// so re-reading the item (and walking its whole timeline) on every
    /// pass buys nothing; this paces the re-check instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retirement_held_at: Option<String>,
    /// The base and branch commits of the last conflict notice that was
    /// delivered successfully. A failed delivery leaves this unchanged so
    /// the same conflict is retried, and a clean check clears it so a later
    /// divergence is a new incident.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_notice: Option<ConflictNotice>,
    /// Whether the log has already said that the listings and the item
    /// disagree about this one, so that a hold announces itself once per
    /// incident rather than once in the item's life.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retirement_announced: bool,
    /// `updated_at` of the issue when the timeline was last reconciled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Delivered timeline events: key -> updated_at marker (for edit detection).
    #[serde(default)]
    pub seen: BTreeMap<String, String>,
    /// The initial prompt has been delivered.
    #[serde(default)]
    pub seeded: bool,
    /// Terminal input for the initial prompt may already have been sent. This
    /// is committed before that external write and cleared only once delivery
    /// is confirmed, so restart recovery never pastes over a stranded prompt.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub first_prompt_attempted: bool,
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
    /// This pull request is owned by that item's session (same repo): it was
    /// opened from that session, or its branch is that session's branch.
    /// Every prompt about it goes to the owner's agent, and the workspace
    /// lifecycle belongs to the owner. Older state may contain issues here;
    /// [`State::load_from`] detaches those legacy authorship bindings.
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
    /// What each of them hears, for the ones that asked for more than the
    /// default (`ssf sub --events`); a session not named here hears the
    /// item's own state changes. Kept beside the list rather than in it so
    /// an older ssf, which ignores keys it does not know, still reads the
    /// file (#453). [`IssueState::subscribe`] and
    /// [`IssueState::unsubscribe`] are the only way in.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub subscriber_events: BTreeMap<String, Events>,
    /// Tracked only because sessions subscribed to it: polled for activity,
    /// but no workspace, no owner and no session of its own.
    #[serde(default)]
    pub subscriber_only: bool,
    /// Per-item launch overrides: the harness, model and effort this
    /// item's session runs with, whatever the repository is configured
    /// with. Written by a handover (`ssf handover`) or by an assignment
    /// of an item that had no session yet (`ssf assign`), used by every
    /// later launch, resume and re-creation -- including the one after a
    /// released workspace is rebuilt -- and replaced only by a later
    /// command that writes them (a handover, or an assignment of an item
    /// left with no session). Which of the two wrote them is recorded
    /// next to them: a handover stamps
    /// [`IssueState::handed_over_at`], an assignment
    /// [`IssueState::assigned_at`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides: Option<Overrides>,
    /// When `ssf assign` wrote this item's overrides (`overrides`). It is
    /// what tells an assignment's overrides from a handover's, so `ssf
    /// status` and `ssf peers` can word the stack by the command that put
    /// the item on it; a handover clears it when it replaces the
    /// overrides. A release or purge leaves it: the stack it stamps is
    /// still the item's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_at: Option<String>,
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

impl IssueState {
    /// Whether this record answers to nothing: no session of its own, no
    /// subscriber, no workspace, no prompt ever sent or attempted, and
    /// nothing waiting on it.
    ///
    /// `onboard` writes a record before it knows who acts on the item, and
    /// what it writes there outlives the answer: an item the daemon
    /// ignored at creation keeps such a record while it is on the `creator`
    /// listing, and so does one whose onboarding never got a session onto
    /// it. Both are consulted by nothing once the item leaves the
    /// listings -- there is no session to retire through the record, no
    /// subscriber to poll for, no workspace to release -- so the item's
    /// record is forgotten with its ignore record
    /// ([`RepoState::forget`]) and does not sit in `ssf status`, `ssf
    /// peers` or the dashboard advertising a closed item as open (#409).
    ///
    /// What stops a record from being one of those: a session that was
    /// bound to it and spoke on it ([`IssueState::had_a_session`]), a
    /// delivery in flight or being retried ([`IssueState::active`],
    /// [`IssueState::first_prompt_attempted`], a block), a subscription, a
    /// workspace of its own, a pending release, cleanup or handover, or a
    /// stack an assignment or a handover wrote for a session still to
    /// come.
    ///
    /// The link to another item's session ([`IssueState::shares_workspace_of`])
    /// deliberately does not count: a bind that lands seeds this record, so
    /// a record that never seeded did not get that session, and what it
    /// mirrors (the owner's workspace, the owner's branch) is still the
    /// owner's to answer for.
    pub fn answers_to_nothing(&self) -> bool {
        !self.had_a_session()
            && !self.seeded
            && !self.active
            && !self.first_prompt_attempted
            && self.blocked.is_none()
            && !self.subscriber_only
            && self.subscribers.is_empty()
            && self.worktree_id.is_none()
            && !self.cleanup_pending
            && !self.release_pending
            && self.handover.is_none()
            && self.handover_note.is_none()
            && self.overrides.is_none()
            && self.assigned_at.is_none()
    }

    /// Whether a session was ever bound to this item and spoken to on it.
    /// A closed item's record is kept once one was: it holds the item's
    /// story, the workspace to release and the way back in when it is
    /// re-opened, even after a run of failures gave the binding up
    /// ([`IssueState::seeded`] back to `false`).
    fn had_a_session(&self) -> bool {
        self.bound_at.is_some()
            || self.last_prompt_at.is_some()
            || self.prompts_sent > 0
            || self.agent_session_id.is_some()
    }

    /// Whether `session` follows this item.
    pub fn follows(&self, session: &str) -> bool {
        self.subscribers
            .iter()
            .any(|s| s.eq_ignore_ascii_case(session))
    }

    /// What `session` hears about this item: the level it asked for, or the
    /// default when it never asked for one.
    pub fn events_for(&self, session: &str) -> Events {
        self.subscriber_events
            .iter()
            .find(|(s, _)| s.eq_ignore_ascii_case(session))
            .map(|(_, e)| *e)
            .unwrap_or_default()
    }

    /// Record that `session` follows this item hearing `events`, adding it
    /// to the list when it was not there. Returns whether it was added, and
    /// whether following it again moved its level. Nothing follows at the
    /// default level, so that case writes no key.
    pub fn subscribe(&mut self, session: &str, events: Events) -> (bool, bool) {
        let added = !self.follows(session);
        let changed = !added && self.events_for(session) != events;
        if added {
            self.subscribers.push(session.to_string());
        }
        self.subscriber_events
            .retain(|s, _| !s.eq_ignore_ascii_case(session));
        if events != Events::default() {
            self.subscriber_events.insert(session.to_string(), events);
        }
        (added, changed)
    }

    /// Drop `session` from this item, with whatever level it held; true when
    /// it followed it.
    pub fn unsubscribe(&mut self, session: &str) -> bool {
        let followed = self.follows(session);
        self.subscribers
            .retain(|s| !s.eq_ignore_ascii_case(session));
        self.subscriber_events
            .retain(|s, _| !s.eq_ignore_ascii_case(session));
        followed
    }
}

/// A scratch session (see [`RepoState::scratch`]): lives until a person
/// kills it with `ssf release`, which removes only its workspace. The
/// record, its branch and the harness conversation stay, so `ssf scratch
/// resume` can bring it back.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScratchState {
    pub id: String,
    /// Whose session this is (a GitHub login); `None` for a shared one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_login: Option<String>,
    /// What the session runs, chosen when it was created.
    pub stack: Overrides,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    /// Git branch of the workspace (`refs/heads/scratch/<id>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_handle: Option<String>,
    /// Harness conversation id, kept across a kill for `ssf scratch resume`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launched_at: Option<String>,
    /// `ssf release` passed its checks (or was forced): the workspace is
    /// removed on the daemon's next pass.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub release_pending: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub release_forced: bool,
    /// When the workspace was removed; cleared by a resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_prompt_at: Option<String>,
    #[serde(default)]
    pub prompts_sent: u64,
}

impl ScratchState {
    /// What a scratch session is called where an item would show its title.
    pub fn title(&self) -> String {
        match &self.owner_login {
            Some(login) => format!("Scratch for @{login}"),
            None => "Scratch (shared)".to_string(),
        }
    }
}

/// The commit pair that identified one successfully delivered conflict
/// notice. Files are deliberately not persisted: they are recomputed from
/// the cached merge result when the pair is first seen after a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ConflictNotice {
    pub base_ref: String,
    pub base_sha: String,
    pub branch_sha: String,
}

/// What an item's session runs with instead of the repository's own
/// settings. Written by `ssf handover` (a new session for an item that
/// has one) and by `ssf assign` (the first session of an item that has
/// none); `model` and `effort` unset mean the harness's own defaults,
/// not the repository's, when the harness differs; see `Engine::effective`.
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
/// The harness is not signed in (its login expired, was
/// revoked, or was never there), or the harness could not be started at
/// all (a handover to a harness that exits the moment it is launched),
/// or OMP first-run setup is incomplete despite a logged-in provider.
/// The record keeps what the screen or the driver said, when it was
/// seen, whether the item has been told, the credential file's identity
/// at the time (a new login rewrites it) and when the harness was last
/// started again to check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Blocked {
    /// `login`, `start`, or `setup`.
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
    /// When a harness that is running behind the block was last told
    /// what it took on (`Engine::tell_a_started_harness`), and how many
    /// of those messages did not land. The telling has a backoff of its
    /// own, on the same curve: a person who signs in at a terminal that
    /// has never been told is not left waiting for the restart's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub told_at: Option<String>,
    #[serde(default)]
    pub tell_failures: u32,
}

impl Blocked {
    pub const SETUP: &'static str = "setup";
    pub const LOGIN: &'static str = "login";
    /// The harness could not be started in the workspace at all.
    pub const START: &'static str = "start";
}

/// Follow `shares_workspace_of` to the session that acts on `number`:
/// the whole chain, since an item bound to a bound item is the first
/// one's session's too. Used by the daemon for everything that belongs to
/// a session (its harness, its overrides) and by the status commands for
/// what they say about it, so both name the same session.
pub fn owner_in(issues: &BTreeMap<u64, IssueState>, number: u64) -> u64 {
    let mut cur = number;
    let mut seen = std::collections::BTreeSet::new();
    while let Some(next) = issues.get(&cur).and_then(|s| s.shares_workspace_of) {
        if next == cur || !seen.insert(cur) {
            break;
        }
        cur = next;
    }
    cur
}

pub fn state_path() -> PathBuf {
    state_dir().join("state.json")
}

/// The exclusive, process-held lock for a state directory. The daemon keeps
/// its state in memory and writes it wholesale, so every engine, including a
/// one-shot one, must hold this before reading that state.
pub struct StateLock {
    _file: File,
}

impl StateLock {
    pub fn acquire() -> Result<Self> {
        Self::acquire_in(&state_dir())
    }

    fn acquire_in(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join("state.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        // `flock` locks the opened inode, rather than the pathname. It is
        // therefore atomic between processes even if they spell the state
        // directory differently. Keep `file` alive for the engine's life.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::WouldBlock {
                bail!(
                    "another ssf process holds the state lock {}; stop it first",
                    path.display()
                );
            }
            return Err(e).with_context(|| format!("locking {}", path.display()));
        }
        Ok(Self { _file: file })
    }
}

/// A test process forks children from other threads; until such a child
/// `exec`s it shares every open descriptor, the lock file's included, so a
/// released lock can stay held for that moment (#476).
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn reacquire(dir: &Path) -> StateLock {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match StateLock::acquire_in(dir) {
                Ok(lock) => return lock,
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                Err(e) => panic!("{e:#}"),
            }
        }
    }
}

impl State {
    /// Move a repository's state and canonicalise stored session references.
    /// Old names remain readable through `RepoConfig::aliases`; rewriting the
    /// cache keeps status and future writes consistent with the canonical name.
    pub fn rename_repo(&mut self, old: &str, new: &str) -> Result<bool> {
        if old.eq_ignore_ascii_case(new) {
            return Ok(false);
        }
        let mut changed = false;
        if let Some(repo) = self.repos.remove(old) {
            if self.repos.contains_key(new) {
                self.repos.insert(old.to_string(), repo);
                anyhow::bail!("state contains both repositories {old} and {new}");
            }
            let mut repo = repo;
            repo.issues_etag = None;
            repo.mentioned_etag = None;
            repo.pulls_etag = None;
            repo.created_etag = None;
            self.repos.insert(new.to_string(), repo);
            changed = true;
        }
        for repo in self.repos.values_mut() {
            for item in repo.issues.values_mut() {
                changed |= rewrite_session(&mut item.origin, old, new);
                changed |= rewrite_session(&mut item.delegated_by, old, new);
                if let Some(h) = &mut item.handover {
                    changed |= rewrite_session(&mut h.by, old, new);
                }
                for origin in item.origins.values_mut() {
                    changed |= rewrite_session_value(origin, old, new);
                }
                for subscriber in &mut item.subscribers {
                    changed |= rewrite_session_value(subscriber, old, new);
                }
                changed |= rewrite_levels(&mut item.subscriber_events, old, new);
            }
        }
        Ok(changed)
    }

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
        st.drop_legacy_issue_bindings();
        st.drop_records_answering_to_nothing();
        st.prune_subscriber_events();
        Ok(st)
    }

    /// A level names a subscriber. Anything else in `subscriber_events` is a
    /// leftover -- a session that stopped following on an ssf that did not
    /// keep the two together, or a hand-edited file -- and would otherwise
    /// wait to be applied to a session that follows the item again later.
    fn prune_subscriber_events(&mut self) {
        for rs in self.repos.values_mut() {
            for st in rs.issues.values_mut() {
                let subs = st.subscribers.clone();
                st.subscriber_events
                    .retain(|s, _| subs.iter().any(|k| k.eq_ignore_ascii_case(s)));
            }
        }
    }

    /// Before #305, an issue carrying a session origin tag shared the
    /// opener's workspace. Issues now use that tag only for attribution, so
    /// detach those persisted bindings on upgrade. An assigned or mentioned
    /// item is deliberately remembered as created-only: its additional
    /// listing membership then makes the next pass onboard a fresh session.
    fn drop_legacy_issue_bindings(&mut self) {
        for (repo, rs) in self.repos.iter_mut() {
            let numbers: Vec<u64> = rs
                .issues
                .values()
                .filter(|st| {
                    st.kind.as_deref() == Some("issue") && st.shares_workspace_of.is_some()
                })
                .map(|st| st.number)
                .collect();
            if numbers.is_empty() {
                continue;
            }
            info!(
                repo,
                ?numbers,
                "detaching issue bindings created by an older ssf"
            );
            // The first pass after upgrading must receive full listings. If
            // all four returned 304 against the old cache, reconciliation
            // would return before comparing assigned/mentioned membership
            // with the created-only ignore records below.
            rs.issues_etag = None;
            rs.mentioned_etag = None;
            rs.pulls_etag = None;
            rs.created_etag = None;
            for number in &numbers {
                let Some(st) = rs.issues.remove(number) else {
                    continue;
                };
                rs.ignored.insert(
                    *number,
                    Ignored {
                        updated_at: st.updated_at.clone().unwrap_or_default(),
                        triggers: vec!["created".into()],
                        ..Default::default()
                    },
                );
                if !st.subscribers.is_empty() {
                    rs.issues.insert(
                        *number,
                        IssueState {
                            number: *number,
                            title: st.title,
                            html_url: st.html_url,
                            kind: st.kind,
                            github_state: st.github_state,
                            projects: st.projects,
                            updated_at: st.updated_at,
                            seen: st.seen,
                            origin: st.origin,
                            origins: st.origins,
                            untagged: st.untagged,
                            subscribers: st.subscribers,
                            subscriber_events: st.subscriber_events,
                            subscriber_only: true,
                            ..Default::default()
                        },
                    );
                }
            }
        }
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
                let gone: Vec<String> = st
                    .subscribers
                    .iter()
                    .filter(|s| {
                        crate::origin::Origin::parse(s).is_none()
                            && crate::origin::Scratch::parse(s).is_none()
                    })
                    .cloned()
                    .collect();
                for s in gone {
                    ghosts.push(s.clone());
                    st.unsubscribe(&s);
                }
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

    /// Forget the records of items an ssf before #409 ignored at creation
    /// and left behind: it dropped such an item's ignore record when the
    /// item closed, merged or went off every listing, and kept the record
    /// of the item itself, still saying `open`. Nothing visits one again
    /// -- the item is on no listing, and the ignore record that would
    /// bring it back is gone -- so `ssf status`, `ssf peers` and the
    /// dashboard advertised a closed item for ever.
    ///
    /// Only records that answer to nothing go (see
    /// [`IssueState::answers_to_nothing`]), and only once the ignore
    /// record is gone: while it stands, the item is still being ignored
    /// rather than forgotten, and its clock is what keeps a listing that
    /// came back short from putting it through onboarding again (issue
    /// #138).
    fn drop_records_answering_to_nothing(&mut self) {
        for (repo, rs) in self.repos.iter_mut() {
            let numbers: Vec<u64> = rs
                .records_answering_to_nothing()
                .into_iter()
                .filter(|n| !rs.ignored.contains_key(n))
                .collect();
            if numbers.is_empty() {
                continue;
            }
            info!(
                repo,
                ?numbers,
                "dropping the records of items an older ssf ignored at creation; they read open \
for ever after the item closed"
            );
            for number in numbers {
                rs.forget(number);
            }
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
                if st.unsubscribe(session) {
                    dropped.push(format!("{repo}#{}", st.number));
                }
            }
        }
        dropped
    }
}

fn rewrite_session(value: &mut Option<String>, old: &str, new: &str) -> bool {
    if let Some(value) = value {
        return rewrite_session_value(value, old, new);
    }
    false
}

/// Rewrite the sessions a level map is keyed by, keeping each level with the
/// session it was given for; the values are untouched.
fn rewrite_levels(levels: &mut BTreeMap<String, Events>, old: &str, new: &str) -> bool {
    let mut changed = false;
    for key in levels.keys().cloned().collect::<Vec<_>>() {
        let mut rewritten = key.clone();
        if rewrite_session_value(&mut rewritten, old, new)
            && let Some(level) = levels.remove(&key)
        {
            levels.insert(rewritten, level);
            changed = true;
        }
    }
    changed
}

fn rewrite_session_value(value: &mut String, old: &str, new: &str) -> bool {
    if let Some(mut scratch) = crate::origin::Scratch::parse(value) {
        if !scratch.repo.eq_ignore_ascii_case(old) {
            return false;
        }
        scratch.repo = new.to_string();
        *value = scratch.to_string();
        return true;
    }
    match crate::origin::Origin::parse(value) {
        Some(mut origin) if origin.repo.eq_ignore_ascii_case(old) => {
            origin.repo = new.to_string();
            *value = origin.to_string();
            true
        }
        _ => false,
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
    fn state_lock_is_exclusive_until_its_owner_drops_it() {
        let sandbox = crate::config::test_support::sandbox();
        let first = StateLock::acquire_in(&sandbox.state_dir()).unwrap();
        // These name the same directory but reach the lock through separate
        // path spellings, as independently started commands can.
        let err = StateLock::acquire_in(&sandbox.state_dir().join("."))
            .err()
            .expect("the second engine must not get the state lock");
        assert!(err.to_string().contains("holds the state lock"));
        drop(first);
        test_support::reacquire(&sandbox.state_dir());
    }

    /// Scratch sessions round-trip in a map of their own; a state file from
    /// before them loads with none, and a subscription one holds survives
    /// the load (it is no item reference, and not a stale reviewer's).
    #[test]
    fn scratch_sessions_round_trip_and_old_state_loads_without_them() {
        let path = crate::config::test_support::sandbox()
            .state_dir()
            .join("state.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"repos":{"o/r":{"issues":{"1":{"number":1,"subscribers":["o/r#9"]}}}}}"#,
        )
        .unwrap();
        let mut st = State::load_from(&path).unwrap();
        assert!(st.repos["o/r"].scratch.is_empty());

        let rs = st.repo_mut("o/r");
        rs.scratch.insert(
            "k3f9".into(),
            ScratchState {
                id: "k3f9".into(),
                owner_login: Some("alice".into()),
                stack: Overrides {
                    harness: "claude".into(),
                    model: Some("opus".into()),
                    effort: None,
                },
                created_at: "2026-01-01T00:00:00Z".into(),
                worktree_id: Some("w".into()),
                agent_session_id: Some("conv".into()),
                ..Default::default()
            },
        );
        rs.issues
            .get_mut(&1)
            .unwrap()
            .subscribe("o/r~k3f9", Events::All);
        st.save_to(&path).unwrap();

        let back = State::load_from(&path).unwrap();
        let s = &back.repos["o/r"].scratch["k3f9"];
        assert_eq!(s.owner_login.as_deref(), Some("alice"));
        assert_eq!(s.stack.model.as_deref(), Some("opus"));
        assert_eq!(s.agent_session_id.as_deref(), Some("conv"));
        assert!(back.repos["o/r"].issues[&1].follows("o/r~k3f9"));
        assert_eq!(
            back.repos["o/r"].issues[&1].events_for("o/r~k3f9"),
            Events::All
        );
        // No item record was made for it.
        assert_eq!(back.repos["o/r"].issues.len(), 1);
    }

    /// A follow's level lives beside the subscriber list, not in it, and
    /// both survive a restart. A level a session did not ask for writes no
    /// key at all, and one that names no follower is dropped on load (#453).
    #[test]
    fn a_subscription_carries_its_level_beside_the_session() {
        let path = crate::config::test_support::sandbox()
            .state_dir()
            .join("state.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"repos":{"o/r":{"issues":{
                "1":{"number":1,"subscribers":["o/r#9"]},
                "2":{"number":2,"subscribers":["o/r#9"],"subscriber_events":{"o/r#9":"all"}},
                "3":{"number":3,"subscribers":["o/r#4"],
                     "subscriber_events":{"o/r#4":"all","o/r#8":"all"}}
            }}}}"#,
        )
        .unwrap();

        let st = State::load_from(&path).unwrap();
        let item = |n: u64| st.repos["o/r"].issues[&n].clone();
        assert_eq!(
            item(1).events_for("o/r#9"),
            Events::State,
            "no key, default"
        );
        assert_eq!(item(2).events_for("o/r#9"), Events::All);
        assert_eq!(item(3).events_for("o/r#4"), Events::All);
        assert!(
            !item(3).subscriber_events.contains_key("o/r#8"),
            "a level naming no follower went"
        );

        // Written back: the list is what it always was, and only a level a
        // session asked for is written at all.
        st.save_to(&path).unwrap();
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let issues = &written["repos"]["o/r"]["issues"];
        assert_eq!(issues["1"]["subscribers"][0], serde_json::json!("o/r#9"));
        assert!(issues["1"].get("subscriber_events").is_none());
        assert_eq!(
            issues["2"]["subscriber_events"],
            serde_json::json!({"o/r#9": "all"})
        );
        // A state file an older ssf reads: its own `subscribers` list, and
        // keys it does not know rather than entries it cannot parse.
        let list: Vec<String> = serde_json::from_value(issues["2"]["subscribers"].clone()).unwrap();
        assert_eq!(list, vec!["o/r#9"]);
    }

    /// Following an item again is how a session asks for more, and asking
    /// for what it already has changes nothing: one entry either way, and
    /// the answer says which happened.
    #[test]
    fn following_again_moves_the_level_of_the_one_subscription() {
        let mut st = IssueState::default();
        assert_eq!(st.subscribe("o/r#9", Events::State), (true, false));
        assert_eq!(st.subscribers, vec!["o/r#9"]);
        assert!(st.subscriber_events.is_empty(), "the default is no key");
        assert_eq!(st.subscribe("o/r#9", Events::State), (false, false));
        assert_eq!(st.subscribers.len(), 1);
        assert_eq!(st.subscribe("O/R#9", Events::All), (false, true));
        assert_eq!(st.subscribers, vec!["o/r#9"], "one entry, not two");
        assert_eq!(st.events_for("o/r#9"), Events::All);
        assert_eq!(st.subscribe("o/r#9", Events::All), (false, false));
        assert_eq!(st.subscribe("o/r#9", Events::State), (false, true));
        assert!(st.subscriber_events.is_empty(), "back at the default");
        assert!(st.unsubscribe("o/r#9"));
        assert!(st.subscribers.is_empty() && !st.unsubscribe("o/r#9"));
    }

    #[test]
    fn a_subscription_level_is_spelled_out_in_a_refusal() {
        assert_eq!("state".parse::<Events>().unwrap(), Events::State);
        assert_eq!(" ALL ".parse::<Events>().unwrap(), Events::All);
        let err = "everything".parse::<Events>().unwrap_err().to_string();
        assert!(err.contains("state") && err.contains("all"), "{err}");
    }

    /// The state file the daemon is restarted against was written before
    /// the ignore record carried its clocks (issue #138), so a record with
    /// only `updated_at` and `triggers` has to load, mean "not known to be
    /// absent, never asked about", and write back the same two keys.
    #[test]
    fn an_ignore_record_from_before_its_clocks_loads_and_round_trips() {
        let sandbox = crate::config::test_support::sandbox();
        let path = sandbox.state_dir().join("state.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"repos":{"mikekelly/overlay-mono":{"ignored":{"337":{"updated_at":"2026-09-03T00:19:46Z","triggers":["created"]}}}}}"#,
        )
        .unwrap();
        let st = State::load_from(&path).unwrap();
        let at = &st.repos["mikekelly/overlay-mono"].ignored[&337];
        assert_eq!(at.updated_at, "2026-09-03T00:19:46Z");
        assert_eq!(at.triggers, vec!["created".to_string()]);
        assert!(at.absent_since.is_none() && at.asked_at.is_none());

        // And back out as it came in: an older ssf reading this file, or a
        // person reading it, sees no new keys until there is something to
        // say.
        st.save_to(&path).unwrap();
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            written["repos"]["mikekelly/overlay-mono"]["ignored"]["337"],
            serde_json::json!({"updated_at": "2026-09-03T00:19:46Z", "triggers": ["created"]})
        );
    }

    #[test]
    fn records_an_older_ssf_left_behind_for_ignored_items_are_dropped_on_load() {
        let sandbox = crate::config::test_support::sandbox();
        let path = sandbox.state_dir().join("state.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"repos":{"o/r":{
                "issues":{
                    "7":{"number":7,"title":"follow-up","kind":"issue","worktree_name":"issue-7-x","repo_id":"o/r","github_state":"open","triggers":["created"]},
                    "8":{"number":8,"title":"still ignored","kind":"issue","github_state":"open","triggers":["created"]},
                    "9":{"number":9,"title":"has a session","kind":"issue","seeded":true,"active":true,"worktree_id":"w9","github_state":"open","triggers":["created"]},
                    "10":{"number":10,"title":"followed","kind":"issue","subscriber_only":true,"github_state":"open","triggers":["created"],"subscribers":["o/r#9"]},
                    "11":{"number":11,"title":"bound to another session","kind":"pull_request","bound_at":"2026-09-20T09:21:40Z","github_state":"open","triggers":["created"],"shares_workspace_of":9,"origin":"o/r#9"},
                    "12":{"number":12,"title":"workspace to clean up","kind":"pull_request","worktree_id":"w12","worktree_name":"issue-12","github_state":"closed","triggers":["created"]}},
                "ignored":{"8":{"updated_at":"u8","triggers":["created"]}}}}}"#,
        )
        .unwrap();

        let st = State::load_from(&path).unwrap();
        let rs = &st.repos["o/r"];
        assert!(
            !rs.issues.contains_key(&7),
            "the record of an item an older ssf ignored and forgot reads open for ever (#409)"
        );
        assert!(
            rs.issues.contains_key(&8),
            "still being ignored, not forgotten"
        );
        assert!(rs.issues.contains_key(&9), "a session retires through it");
        assert!(
            rs.issues.contains_key(&10),
            "a subscriber is polled through it"
        );
        assert!(
            rs.issues.contains_key(&11),
            "a session was bound to it, so its record stays"
        );
        assert!(
            rs.issues.contains_key(&12),
            "a workspace is released through it"
        );

        st.save_to(&path).unwrap();
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            written["repos"]["o/r"]["issues"].get("7").is_none(),
            "{written}"
        );
    }

    #[test]
    fn loading_detaches_legacy_issue_bindings_but_keeps_prs_and_subscribers() {
        let sandbox = crate::config::test_support::sandbox();
        let path = sandbox.state_dir().join("state.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"repos":{"o/r":{"issues_etag":"a","mentioned_etag":"m","pulls_etag":"p","created_etag":"c","issues":{
                "7":{"number":7,"title":"assigned placeholder","kind":"issue","seeded":true,"active":true,"shares_workspace_of":1,"worktree_id":"w1","updated_at":"u7","triggers":["assigned","created"]},
                "8":{"number":8,"title":"followed placeholder","kind":"issue","seeded":true,"active":true,"shares_workspace_of":1,"worktree_id":"w1","updated_at":"u8","triggers":["created"],"subscribers":["o/r#2"]},
                "9":{"number":9,"title":"pull request","kind":"pull_request","seeded":true,"active":true,"shares_workspace_of":1,"worktree_id":"w1","updated_at":"u9","triggers":["created"]}
            }}}}"#,
        )
        .unwrap();

        let st = State::load_from(&path).unwrap();
        let rs = &st.repos["o/r"];
        assert!(
            rs.issues_etag.is_none()
                && rs.mentioned_etag.is_none()
                && rs.pulls_etag.is_none()
                && rs.created_etag.is_none(),
            "the first upgraded pass must fetch every listing in full"
        );
        assert!(!rs.issues.contains_key(&7), "unfollowed issue is unbound");
        let followed = &rs.issues[&8];
        assert!(followed.subscriber_only && !followed.seeded && !followed.active);
        assert_eq!(followed.subscribers, vec!["o/r#2"]);
        assert!(followed.worktree_id.is_none());
        assert_eq!(rs.issues[&9].shares_workspace_of, Some(1), "PR stays bound");
        for (number, updated_at) in [(7, "u7"), (8, "u8")] {
            let ignored = &rs.ignored[&number];
            assert_eq!(ignored.updated_at, updated_at);
            assert_eq!(ignored.triggers, vec!["created"]);
        }
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
