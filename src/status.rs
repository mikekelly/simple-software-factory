//! The joined view behind `ssf status` and `ssf peers`: what ssf knows about
//! each item (issue or PR, GitHub state, triggers, prompts, session id) next
//! to what herdr reports about the workspace working on it (agent state, last
//! assistant message, current tool, last activity, board column, branch). The
//! dashboards read this and never talk to herdr themselves.

use anyhow::Context;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

use crate::config::{Config, DriverKind, RepoConfig};
use crate::driver::{Drivers, WorkspaceInfo};
use crate::engine::MAX_RELEASE_REFUSALS;
use crate::github::PrInfo;
use crate::state::{
    Blocked, HandoverNote, IssueState, Overrides, PendingHandover, ScratchState, State,
};

/// How long `ssf status` waits for a driver before reporting it unavailable;
/// the dashboards poll this, so it must never hang.
const DRIVER_TIMEOUT: Duration = Duration::from_secs(8);

/// Session identity: `owner/repo#N`, the same form `--as` takes.
pub fn session_id(repo: &str, number: u64) -> String {
    format!("{repo}#{number}")
}

/// One tracked item and the agent session working on it.
#[derive(Debug, Clone, Serialize)]
pub struct Session {
    pub id: String,
    pub repo: String,
    /// The item's number; 0 for a scratch session, which has none.
    pub number: u64,
    /// `issue`, `pull_request`, or `scratch` for a scratch session
    /// (`owner/repo~id`, an agent session that works on no item).
    pub kind: String,
    /// Whose scratch session this is (a GitHub login); null for a shared
    /// one and for an item's session.
    pub owner_login: Option<String>,
    pub title: String,
    pub url: String,
    /// `open`, `closed`, `merged`, or `unknown` for items bound before this was recorded.
    pub github_state: String,
    /// Still assigned/mentioned/requested and open as of the last poll.
    pub active: bool,
    /// Why the bot got involved: `assigned`, `mentioned`, `review_requested`,
    /// `created` (the bot's own item).
    pub triggers: Vec<String>,
    /// The harness this item's session runs: what the driver reports for
    /// its pane (`AgentInfo.agent_type`), and the harness its record would
    /// launch when nothing live reports one (`next_launch` says whether
    /// the two differ).
    pub harness: String,
    /// Model and effort the session runs with, overrides applied; `None`
    /// is the harness's own default. Left out while `next_launch` is set:
    /// a pane on another harness was launched with a stack ssf does not
    /// have on the record, and these are the next launch's, not its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The stack the next launch, resume, relaunch or re-creation uses,
    /// when it is not the one running: the repository's config with the
    /// item's overrides applied. A config edit does not touch a session
    /// that is already live, so the change waits for the next launch, and
    /// this is what makes it visible rather than hidden.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_launch: Option<Stack>,
    /// Set when a handover or an assignment moved this session off the
    /// repository's configured harness, model or effort.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<Overrides>,
    /// `overrides` were written by `ssf assign` rather than by a
    /// handover: the two write the same thing, and each stamps the item
    /// it wrote (`IssueState::assigned_at` against
    /// `IssueState::handed_over_at`), so a session can say which command
    /// put the item on the stack it runs. This is what the status
    /// commands word them by.
    pub assigned_stack: bool,
    /// A handover the daemon has accepted and not carried out yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handover: Option<HandoverView>,
    /// What an earlier handover left for a session that has not read it
    /// yet: the harness that was handed over, and how long its summary
    /// is. It goes to whichever session takes the first message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handover_note: Option<HandoverNoteView>,
    /// Session that acts on this item: its own, or the session it is bound
    /// to. Empty for an item tracked only for its subscribers.
    pub owner: String,
    /// Sessions that hear about this item without acting on it.
    pub subscribers: Vec<String>,
    /// Tracked only because sessions subscribed to it: no workspace, no
    /// owner, no session of its own.
    pub subscriber_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shares_workspace_of: Option<String>,
    /// Session that handed this item off (`mode=delegate`); it hears about
    /// the closure once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delegated_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Harness conversation id (Claude Code / Codex).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    pub prompts_sent: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_prompt_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<String>,
    /// When a retirement was last held because the item still carried one
    /// of its triggers while the listings had dropped it. Present while
    /// the listings and the item disagree, which is why such an item is
    /// still active and `ssf release` still refuses it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retirement_held_at: Option<String>,
    /// When `ssf release` or `ssf purge` removed the workspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub released_at: Option<String>,
    /// For a retired item: `kept` (the workspace is still there), `released`
    /// (removed by `ssf release`/`ssf purge`), `gone` (removed some other
    /// way), `pending` (release approved, removal on the next pass), or
    /// `given-up` (kept after the daemon refused the agent's release
    /// three times; a person's to deal with).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<PrInfo>,
    /// Session that opened the item, from the origin tag in its body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Sessions whose posts landed on the item (origin tag -> number of posts).
    pub posts_by_session: BTreeMap<String, usize>,
    /// Posts by the bot that carried no origin tag.
    pub untagged_posts: usize,
    /// The driver's agent state (`idle`, `working`, `blocked`, `done`), or
    /// `no-agent` (workspace without an agent), `no-workspace` (ssf has a
    /// binding but herdr has no such workspace), `unbound` (no workspace yet),
    /// `unknown` (herdr could not be asked).
    pub agent_state: String,
    /// True only when the session driver reported an agent in this item's
    /// workspace. `active` is issue monitoring state and must not be used as
    /// evidence that an agent exists.
    pub agent_live: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
    /// Tool the agent is running right now, as `Name: input`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
    /// Driver board column, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    /// The matching driver workspace row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceInfo>,
    /// The session cannot take prompts: its harness is at a login prompt
    /// (`reason` is `login`; `harness`, `detail`, `since` say which, what
    /// the screen showed and from when). Deliveries are held until the
    /// login is back; a person has to sign the harness in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<BlockedView>,
}

/// A stack a session is launched with: a harness, and the model and effort
/// it takes (`None` is the harness's own default).
#[derive(Debug, Clone, Serialize)]
pub struct Stack {
    pub harness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// A handover waiting for the daemon's next pass, for `ssf status --json`
/// and `ssf peers`: what it is to, who asked, and how long the summary is
/// (the summary itself is the new session's first message, not status).
#[derive(Debug, Clone, Serialize)]
pub struct HandoverView {
    pub harness: String,
    /// The harness for people (`Pi`).
    pub harness_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_chars: Option<usize>,
    /// The session that asked; `None` for a person at a shell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    pub requested_at: String,
}

/// A handover's parting words, waiting on the item for the session that
/// reads them (see [`crate::state::HandoverNote`]): who wrote them and
/// how long they are. The words themselves are the new session's first
/// message, not status.
#[derive(Debug, Clone, Serialize)]
pub struct HandoverNoteView {
    /// Display name of the harness the item was handed over from.
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_chars: Option<usize>,
}

impl HandoverNoteView {
    pub fn of(n: &HandoverNote) -> Self {
        Self {
            from: n.from.clone(),
            summary_chars: n.summary.as_deref().map(|s| s.chars().count()),
        }
    }

    /// One line for a person: `from Claude Code, summary 1234 chars`.
    pub fn describe(&self) -> String {
        match self.summary_chars {
            Some(n) => format!("from {}, summary {n} chars", self.from),
            None => format!("from {}, no summary", self.from),
        }
    }
}

impl HandoverView {
    pub fn of(h: &PendingHandover) -> Self {
        Self {
            harness: h.harness.clone(),
            harness_name: crate::login::display_name(&h.harness),
            model: h.model.clone(),
            effort: h.effort.clone(),
            summary_chars: h.summary.as_deref().map(|s| s.chars().count()),
            by: h.by.clone(),
            requested_at: h.requested_at.clone(),
        }
    }

    /// One line for a person: `pi (model openai/gpt-6), asked by o/r#5`.
    pub fn describe(&self) -> String {
        let mut s = self.harness.clone();
        let mut extra: Vec<String> = Vec::new();
        if let Some(m) = &self.model {
            extra.push(format!("model {m}"));
        }
        if let Some(e) = &self.effort {
            extra.push(format!("effort {e}"));
        }
        if !extra.is_empty() {
            s.push_str(&format!(" ({})", extra.join(", ")));
        }
        s.push_str(&match &self.by {
            Some(by) => format!(", asked by {by}"),
            None => ", asked by a person at a terminal".to_string(),
        });
        s
    }
}

/// A session's block, for `ssf status --json` and the dashboards.
#[derive(Debug, Clone, Serialize)]
pub struct BlockedView {
    pub reason: String,
    pub harness: String,
    /// The harness for people (`Claude Code`).
    pub harness_name: String,
    /// The screen line that gave it away (for the log and this JSON; the
    /// texts people and agents see leave it out, see `driver::login_dialog`).
    pub detail: String,
    pub since: String,
    /// What a person runs to lift it.
    pub fix: String,
}

impl BlockedView {
    pub fn from_blocked(b: &Blocked) -> Self {
        Self {
            reason: b.reason.clone(),
            harness: b.harness.clone(),
            harness_name: crate::login::display_name(&b.harness),
            detail: b.detail.clone(),
            since: b.since.clone(),
            fix: fix_for(b),
        }
    }

    /// Did the harness never come up, rather than sit at its sign-in
    /// prompt?
    fn never_started(&self) -> bool {
        self.reason == Blocked::START
    }

    /// One line for a person: `Claude Code at its sign-in prompt since
    /// 3m; run `claude auth login` on the host`, or, for a harness that
    /// never came up, what to do about that.
    pub fn describe(&self) -> String {
        if self.reason == Blocked::SETUP {
            return format!(
                "{} setup incomplete since {}; {}",
                self.harness_name,
                ago(Some(&self.since)),
                self.fix
            );
        }
        if self.never_started() {
            return format!(
                "{} could not be started since {}; {}",
                self.harness_name,
                ago(Some(&self.since)),
                self.fix
            );
        }
        format!(
            "{} at its sign-in prompt since {}; run {}",
            self.harness_name,
            ago(Some(&self.since)),
            self.fix
        )
    }
}

/// What a person does about a block: sign the harness in, or, for one
/// that could not be started at all (a handover to a harness that exits
/// as it is launched, a model id the harness itself refuses), start it
/// by hand or hand the item over again with settings that work. Used by
/// the status commands and by the `blocked` post, so both say the same.
pub fn fix_for(b: &Blocked) -> String {
    if b.reason == Blocked::SETUP {
        return "run `omp` interactively as the factory user on the host (inside the guest in VM mode); press Esc through the remaining setup steps to complete or skip setup".into();
    }
    if b.reason == Blocked::START {
        return format!(
            "start {} by hand in the workspace, or fix the model or effort and hand over again",
            crate::login::display_name(&b.harness)
        );
    }
    crate::login::how_to_sign_in(&b.harness)
}

/// [`fix_for`] as the tail of a sentence that has not already said what
/// kind of fix it is: a login block's is a command to sign in with, a
/// start block's is an instruction of its own.
pub fn fix_clause(b: &Blocked) -> String {
    if b.reason == Blocked::START || b.reason == Blocked::SETUP {
        fix_for(b)
    } else {
        format!("sign in with {}", fix_for(b))
    }
}

impl Session {
    pub fn is_pull_request(&self) -> bool {
        self.kind == "pull_request"
    }

    pub fn is_scratch(&self) -> bool {
        self.kind == "scratch"
    }

    /// How the text reports name the session in its repository's list:
    /// `#N` for an item, `~id` for a scratch session.
    pub fn label(&self) -> String {
        match self.id.rsplit_once('~') {
            Some((_, id)) if self.is_scratch() => format!("~{id}"),
            _ => format!("#{}", self.number),
        }
    }

    /// The change waiting on this session, as the status commands word it:
    /// `harness codex → omp next launch`, with the model and effort the
    /// next launch uses. `None` while the pane is on the stack the record
    /// names, which is the ordinary case.
    pub fn next_launch_change(&self) -> Option<String> {
        let next = self.next_launch.as_ref()?;
        let mut extra: Vec<String> = Vec::new();
        if let Some(m) = &next.model {
            extra.push(format!("model {m}"));
        }
        if let Some(e) = &next.effort {
            extra.push(format!("effort {e}"));
        }
        Some(if extra.is_empty() {
            format!("harness {} → {} next launch", self.harness, next.harness)
        } else {
            format!(
                "harness {} → {} next launch ({})",
                self.harness,
                next.harness,
                extra.join(", ")
            )
        })
    }
}

/// Everything `ssf status` reports, gathered once.
pub struct Snapshot {
    pub cfg: Config,
    pub state: State,
    /// The workspaces of every driver that answered.
    pub workspaces: Vec<WorkspaceInfo>,
    /// Drivers that did not answer; their sessions' agent states are
    /// unknown, the others' are not affected.
    pub down: Vec<DriverKind>,
    pub errors: Vec<String>,
}

impl Snapshot {
    pub async fn collect(cfg: Config) -> anyhow::Result<Self> {
        let state = State::load()?;
        let mut workspaces = Vec::new();
        let mut errors = Vec::new();
        let mut down = Vec::new();
        for d in Drivers::from_config(&cfg).iter() {
            match tokio::time::timeout(DRIVER_TIMEOUT, d.ps()).await {
                Ok(Ok(list)) => workspaces.extend(list),
                Ok(Err(e)) => {
                    down.push(d.kind());
                    errors.push(format!("{}: {e:#}", d.label()));
                }
                Err(_) => {
                    down.push(d.kind());
                    errors.push(format!(
                        "{} did not answer within {}s",
                        d.label(),
                        DRIVER_TIMEOUT.as_secs()
                    ));
                }
            }
        }
        Ok(Self {
            cfg,
            state,
            workspaces,
            down,
            errors,
        })
    }

    /// Every driver in use answered.
    pub fn available(&self) -> bool {
        self.down.is_empty()
    }

    /// Why some driver did not answer, if one did not.
    pub fn error(&self) -> Option<String> {
        if self.errors.is_empty() {
            None
        } else {
            Some(self.errors.join("; "))
        }
    }

    pub fn sessions(&self) -> Vec<Session> {
        sessions_with(&self.cfg, &self.state, &self.workspaces, &self.down)
    }

    /// A configured credential makes the daemon's last authenticated login
    /// meaningful. Before the daemon starts, use the configured account;
    /// after logout, do not present a stale daemon cache as a sign-in.
    fn bot_login(&self) -> Option<&str> {
        (self.cfg.token_source() != "none")
            .then(|| {
                self.state
                    .bot_login
                    .as_deref()
                    .or(self.cfg.github.login.as_deref())
            })
            .flatten()
    }

    pub fn to_json(&self) -> Value {
        let sessions = self.sessions();
        let repos: Vec<Value> = self
            .cfg
            .repos
            .iter()
            .map(|r| {
                let issues: Vec<&Session> = sessions.iter().filter(|s| s.repo == r.name).collect();
                json!({
                    "name": r.name,
                    "harness": r.harness,
                    "model": r.model,
                    "effort": r.effort,
                    "path": r.path,
                    "allowed_users": self.cfg.access_summary(r),
                    "anyone_allowed": self.cfg.anyone_allowed(r),
                    "issues": issues,
                })
            })
            .collect();
        let driver_status = json!({
            "available": self.available(),
            "error": self.error(),
            "workspaces": self.workspaces.len(),
            "down": self.down.iter().map(|k| k.id()).collect::<Vec<_>>(),
        });
        let mut payload = json!({
            "server": {"hostname": crate::hostname(), "location": "local"},
            "bot_login": self.bot_login(),
            "token_configured": self.cfg.github_token().is_ok(),
            "service_enabled": crate::ui::service_enabled(),
            "service_active": crate::ui::service_active(),
            "last_poll_at": self.state.last_poll_at,
            "last_error": self.state.last_error,
            "poll_interval_secs": self.cfg.daemon.poll_interval_secs,
            "config_path": crate::config::config_path(),
            // The wildcard allow-list is in effect somewhere: the
            // dashboards show a warning while it is.
            "anyone_allowed": self.cfg.anyone_allowed_anywhere(),
            // Sessions whose harness is not signed in (the dashboards show an
            // urgent line per one).
            "blocked_sessions": sessions.iter().filter(|s| s.blocked.is_some()).map(|s| s.id.clone()).collect::<Vec<_>>(),
            "driver": driver_status,
            "sessions": sessions,
            "repos": repos,
        });
        payload["dashboard"] =
            dashboard_presentation(&payload).expect("canonical status always contains sessions");
        payload
    }
}

/// Join every tracked item with its workspace. `workspaces` is `None`
/// when no driver could be asked.
#[cfg(test)]
pub fn sessions(cfg: &Config, state: &State, workspaces: Option<&[WorkspaceInfo]>) -> Vec<Session> {
    match workspaces {
        Some(list) => sessions_with(cfg, state, list, &[]),
        None => sessions_with(cfg, state, &[], &DriverKind::ALL),
    }
}

/// [`sessions`] for a mixed set of drivers: the repositories of a driver in
/// `down` get no workspace data (agent state unknown), the others do.
pub fn sessions_with(
    cfg: &Config,
    state: &State,
    list: &[WorkspaceInfo],
    down: &[DriverKind],
) -> Vec<Session> {
    let mut out = Vec::new();
    for repo in &cfg.repos {
        let workspaces = if down.contains(&cfg.driver_for(repo)) {
            None
        } else {
            Some(list)
        };
        let Some(rs) = state.repos.get(&repo.name) else {
            continue;
        };
        for item in rs.issues.values() {
            let ws = workspaces.and_then(|list| find_workspace(list, item));
            // An item bound to another session shares its workspace, and
            // so the harness a handover or an assignment put in it. The
            // binding is followed to its end, as the daemon follows it:
            // an item bound to a bound item is the first one's session's
            // too.
            let owner = crate::state::owner_in(&rs.issues, item.number);
            let source = rs.issues.get(&owner).unwrap_or(item);
            // Which command wrote the stack: `assigned_at` is set exactly
            // when an assignment wrote it, since only `finish_handover`
            // clears it (an assignment leaves the handover's own stamp
            // alone, because that one bounds the capture window).
            let assigned = source.assigned_at.is_some();
            out.push(join(
                repo,
                item,
                owner,
                source.overrides.as_ref(),
                assigned,
                ws,
                workspaces.is_some(),
            ));
        }
        for st in rs.scratch.values() {
            let ws = workspaces.and_then(|list| {
                let id = st.worktree_id.as_deref()?;
                list.iter().find(|w| w.worktree_id == id)
            });
            out.push(join_scratch(repo, st, ws, workspaces.is_some()));
        }
    }
    out
}

/// A scratch session as a status row: the fields an item's session has, with
/// `kind` `scratch`, no item behind it (number 0, no URL), and live exactly
/// while it has a workspace.
fn join_scratch(
    repo: &RepoConfig,
    st: &ScratchState,
    ws: Option<&WorkspaceInfo>,
    driver_available: bool,
) -> Session {
    // The fields shared with an item's row come from the same join, over a
    // record that holds what the scratch session has of an item's.
    let item = IssueState {
        title: st.title(),
        active: st.worktree_id.is_some(),
        worktree_id: st.worktree_id.clone(),
        worktree_path: st.worktree_path.clone(),
        repo_id: st.repo_id.clone(),
        worktree_name: Some(format!("{}{}", crate::driver::SCRATCH_PREFIX, st.id)),
        branch: st.branch.clone(),
        agent_session_id: st.agent_session_id.clone(),
        release_pending: st.release_pending,
        released_at: st.released_at.clone(),
        last_prompt_at: st.last_prompt_at.clone(),
        prompts_sent: st.prompts_sent,
        bound_at: Some(st.created_at.clone()),
        github_state: Some("open".into()),
        kind: Some("scratch".into()),
        ..Default::default()
    };
    let mut s = join(repo, &item, 0, Some(&st.stack), false, ws, driver_available);
    let id = crate::origin::Scratch {
        repo: repo.name.clone(),
        id: st.id.clone(),
    }
    .to_string();
    s.id = id.clone();
    s.owner = id;
    s.owner_login = st.owner_login.clone();
    // The stack is the session's own, chosen when it was made, and not an
    // override of the repository's.
    s.overrides = None;
    s
}

/// The driver workspace of a record: by id, else by its link to the
/// item number (the state file may be behind, or lost).
fn find_workspace<'a>(list: &'a [WorkspaceInfo], item: &IssueState) -> Option<&'a WorkspaceInfo> {
    if let Some(id) = &item.worktree_id {
        if let Some(w) = list.iter().find(|w| &w.worktree_id == id) {
            return Some(w);
        }
    }
    let repo_id = item.repo_id.as_deref()?;
    let wanted = if item.kind.as_deref() == Some("pull_request") {
        |w: &WorkspaceInfo, n| w.linked_pr == Some(n) || w.linked_issue == Some(n)
    } else {
        |w: &WorkspaceInfo, n| w.linked_issue == Some(n)
    };
    list.iter()
        .find(|w| !w.is_archived && w.repo_id == repo_id && wanted(w, item.number))
}

fn strip_ref(branch: &str) -> String {
    branch
        .strip_prefix("refs/heads/")
        .unwrap_or(branch)
        .to_string()
}

/// What became of a retired session's workspace; `None` for active items
/// and for items tracked only for subscribers.
fn workspace_state(
    item: &IssueState,
    ws: Option<&WorkspaceInfo>,
    driver_available: bool,
) -> Option<String> {
    if item.active || item.subscriber_only {
        return None;
    }
    // Retired before it was ever bound (unassigned during onboarding).
    if item.worktree_id.is_none() && item.repo_id.is_none() && item.worktree_name.is_none() {
        return None;
    }
    Some(
        if item.release_pending {
            "pending"
        } else if item.worktree_id.is_some() && item.release_refusals >= MAX_RELEASE_REFUSALS {
            "given-up"
        } else if item.worktree_id.is_none() && item.released_at.is_some() {
            "released"
        } else if item.worktree_id.is_none() {
            "gone"
        } else if ws.is_some() || !driver_available {
            "kept"
        } else {
            "gone"
        }
        .into(),
    )
}

/// `owner` is the item whose session acts on this one (itself, unless it
/// is bound), `overrides` the ones that govern it (the owner's), and
/// `assigned` whether those came from `ssf assign` rather than a handover.
fn join(
    repo: &RepoConfig,
    item: &IssueState,
    owner: u64,
    overrides: Option<&Overrides>,
    assigned: bool,
    ws: Option<&WorkspaceInfo>,
    driver_available: bool,
) -> Session {
    let eff = repo.with_overrides(overrides);
    let agent = ws.and_then(WorkspaceInfo::primary_agent);
    // What the pane is running, when the driver reports an agent type for
    // it. A session that has never been launched has no workspace to
    // report one, so the stack the record would launch stands in for it,
    // and the two agree.
    let running = agent.and_then(|a| a.agent_type.clone());
    let harness = running.unwrap_or_else(|| eff.harness.clone());
    // A config edit does not touch a session that is already live: the
    // harness the pane is on and the one the next launch starts differ,
    // and that difference is what the status commands show instead of
    // hiding behind the config.
    let next_launch = (harness != eff.harness).then(|| Stack {
        harness: eff.harness.clone(),
        model: eff.model.clone(),
        effort: eff.effort.clone(),
    });
    let agent_state = match (ws, agent) {
        (Some(_), Some(a)) => a.state.clone(),
        (Some(_), None) => "no-agent".into(),
        (None, _) if !driver_available => "unknown".into(),
        (None, _) if item.worktree_id.is_some() => "no-workspace".into(),
        (None, _) => "unbound".into(),
    };
    let tool = agent.and_then(|a| {
        let name = a.tool_name.as_deref()?;
        Some(match a.tool_input.as_deref() {
            Some(input) => format!("{name}: {}", input.lines().next().unwrap_or("")),
            None => name.to_string(),
        })
    });
    Session {
        id: session_id(&repo.name, item.number),
        repo: repo.name.clone(),
        number: item.number,
        kind: item.kind.clone().unwrap_or_else(|| {
            if item.pr.is_some() {
                "pull_request".into()
            } else {
                "issue".into()
            }
        }),
        owner_login: None,
        title: item.title.clone(),
        url: item.html_url.clone(),
        // Items bound before the state was recorded: an active item is open
        // by definition (it came from the open listings).
        github_state: item
            .github_state
            .clone()
            .unwrap_or_else(|| if item.active { "open" } else { "unknown" }.into()),
        active: item.active,
        triggers: item.triggers.clone(),
        harness,
        model: if next_launch.is_some() {
            None
        } else {
            eff.model.clone()
        },
        effort: if next_launch.is_some() {
            None
        } else {
            eff.effort.clone()
        },
        next_launch,
        overrides: overrides.cloned(),
        assigned_stack: assigned,
        handover: item.handover.as_ref().map(HandoverView::of),
        handover_note: item.handover_note.as_ref().map(HandoverNoteView::of),
        owner: if item.subscriber_only {
            String::new()
        } else {
            session_id(&repo.name, owner)
        },
        subscribers: item.subscribers.clone(),
        subscriber_only: item.subscriber_only,
        shares_workspace_of: item.shares_workspace_of.map(|n| session_id(&repo.name, n)),
        delegated_by: item.delegated_by.clone(),
        worktree_id: item.worktree_id.clone(),
        worktree_path: ws
            .map(|w| w.path.clone())
            .or_else(|| item.worktree_path.clone()),
        branch: ws
            .and_then(|w| w.branch.as_deref().map(strip_ref))
            .or_else(|| item.branch.as_deref().map(strip_ref)),
        agent_session_id: item.agent_session_id.clone(),
        prompts_sent: item.prompts_sent,
        last_prompt_at: item.last_prompt_at.clone(),
        bound_at: item.bound_at.clone(),
        retired_at: item.retired_at.clone(),
        retirement_held_at: item.retirement_held_at.clone(),
        released_at: item.released_at.clone(),
        workspace_state: workspace_state(item, ws, driver_available),
        pr: item.pr.clone(),
        origin: item.origin.clone(),
        posts_by_session: item.origins.values().fold(BTreeMap::new(), |mut by, o| {
            *by.entry(o.clone()).or_default() += 1;
            by
        }),
        untagged_posts: item.untagged.len(),
        agent_live: agent.is_some(),
        agent_state,
        last_assistant_message: agent.and_then(|a| a.last_assistant_message.clone()),
        tool,
        last_activity_at: ws.and_then(|w| w.last_activity_at.clone()),
        column: ws.and_then(|w| w.column.clone()),
        workspace: ws.cloned(),
        blocked: item.blocked.as_ref().map(BlockedView::from_blocked),
    }
}

// ---- terminal rendering ---------------------------------------------------

/// `3m`, `2h`, `5d` or `now`; empty when the time cannot be read.
pub fn ago(iso: Option<&str>) -> String {
    let Some(t) = iso.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) else {
        return String::new();
    };
    let secs = (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds();
    match secs {
        i64::MIN..=44 => "now".into(),
        45..=5399 => format!("{}m", (secs + 30) / 60),
        5400..=129_599 => format!("{}h", (secs + 1800) / 3600),
        _ => format!("{}d", (secs + 43_200) / 86_400),
    }
}

pub fn one_line(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// The `ssf peers` table: one block per session, grouped by repository.
pub fn render_peers(sessions: &[Session], me: Option<&str>) -> String {
    let mut out = String::new();
    let mut current_repo = "";
    for s in sessions {
        if s.repo != current_repo {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&s.repo);
            out.push('\n');
            current_repo = &s.repo;
        }
        let kind = if s.is_scratch() {
            "scratch"
        } else if s.is_pull_request() {
            "PR"
        } else {
            "issue"
        };
        let marker = if me == Some(s.id.as_str()) {
            " (you)"
        } else {
            ""
        };
        let activity = ago(s.last_activity_at.as_deref());
        out.push_str(&format!(
            "  {:<6} {:<5} {:<7} {:<12} {:>4}  {}{}\n",
            s.label(),
            kind,
            s.github_state,
            s.agent_state,
            activity,
            one_line(&s.title, 70),
            marker
        ));
        let mut facts: Vec<String> = Vec::new();
        if let Some(b) = &s.branch {
            facts.push(format!("branch {b}"));
        }
        if let Some(c) = &s.column {
            facts.push(format!("column {c}"));
        }
        if !s.triggers.is_empty() {
            facts.push(format!("via {}", s.triggers.join("+")));
        }
        if s.subscriber_only {
            facts.push("subscribed only, no session".into());
        } else if s.owner != s.id {
            facts.push(format!("owned by {}", s.owner));
        }
        if let Some(p) = &s.delegated_by {
            facts.push(format!("handed off by {p}"));
        }
        if let Some(next) = s.next_launch_change() {
            facts.push(next);
        }
        if let Some(o) = &s.overrides {
            facts.push(if s.assigned_stack {
                format!("harness {}", o.harness)
            } else {
                format!("handed over to {}", o.harness)
            });
            if let Some(m) = &o.model {
                facts.push(format!("model {m}"));
            }
            if let Some(e) = &o.effort {
                facts.push(format!("effort {e}"));
            }
        }
        if !s.subscribers.is_empty() {
            facts.push(format!("subscribers {}", s.subscribers.join(", ")));
        }
        if let Some(o) = &s.origin {
            facts.push(format!("opened by {o}"));
        }
        facts.push(format!("prompts {}", s.prompts_sent));
        if !s.active && !s.subscriber_only {
            facts.push(match s.workspace_state.as_deref() {
                Some("kept") => "retired, workspace kept".into(),
                Some("released") => "retired, workspace released".into(),
                Some("pending") => "retired, workspace being released".into(),
                Some("given-up") => "retired, release given up, workspace kept".into(),
                _ => "retired".into(),
            });
        }
        out.push_str(&format!("         {}\n", facts.join("  ·  ")));
        if let Some(h) = &s.handover {
            out.push_str(&format!("         handover pending: {}\n", h.describe()));
        }
        if let Some(n) = &s.handover_note {
            out.push_str(&format!(
                "         handover note waiting: {}\n",
                n.describe()
            ));
        }
        if let Some(b) = &s.blocked {
            out.push_str(&format!("         BLOCKED: {}\n", b.describe()));
        }
        if let Some(t) = &s.tool {
            out.push_str(&format!("         tool: {}\n", one_line(t, 100)));
        }
        if let Some(m) = &s.last_assistant_message {
            out.push_str(&format!("         said: {}\n", one_line(m, 100)));
        }
    }
    out
}

/// The human `ssf status` report.
pub fn render_status(snap: &Snapshot) -> String {
    let st = &snap.state;
    let mut out = String::new();
    out.push_str(&format!(
        "bot:     {}\n",
        snap.bot_login().unwrap_or("(not signed in)")
    ));
    out.push_str(&format!(
        "service: {}{}\n",
        if crate::ui::service_active() {
            "running"
        } else {
            "stopped"
        },
        if crate::ui::service_enabled() {
            ""
        } else {
            " (disabled)"
        }
    ));
    if let Some(t) = &st.last_poll_at {
        out.push_str(&format!("polled:  {t}\n"));
    }
    if let Some(e) = &st.last_error {
        out.push_str(&format!("error:   {e}\n"));
    }
    match snap.error() {
        None => out.push_str(&format!("driver:  {} workspaces\n", snap.workspaces.len())),
        Some(e) => out.push_str(&format!(
            "driver:  {} workspaces; unavailable: {e}\n",
            snap.workspaces.len()
        )),
    }
    out.push_str(&format!(
        "config:  {}\n",
        crate::config::config_path().display()
    ));
    if snap.cfg.anyone_allowed_anywhere() {
        out.push_str(
            "WARNING: allowed_users is \"*\": anyone with a GitHub account can drive the agents\n",
        );
    }
    if snap.cfg.repos.is_empty() {
        out.push_str("\nno repositories configured\n");
    }
    let sessions = snap.sessions();
    for s in sessions.iter().filter(|s| s.blocked.is_some()) {
        out.push_str(&format!(
            "BLOCKED: {}: {}\n",
            s.id,
            s.blocked
                .as_ref()
                .map(BlockedView::describe)
                .unwrap_or_default()
        ));
    }
    for r in &snap.cfg.repos {
        out.push_str(&format!("\n{} (harness: {})\n", r.name, r.harness));
        out.push_str(&format!(
            "  allowed users: {}\n",
            snap.cfg.access_summary(r)
        ));
        if !st.repos.contains_key(&r.name) {
            out.push_str("  (not polled yet)\n");
            continue;
        }
        let mine: Vec<&Session> = sessions.iter().filter(|s| s.repo == r.name).collect();
        if mine.is_empty() {
            out.push_str("  no issues tracked\n");
        }
        for s in mine {
            out.push_str(&format!(
                "  {:<7} {:<8} {:<7} {:<12} prompts={:<3} last={}  {}\n",
                s.label(),
                if s.active { "active" } else { "retired" },
                s.github_state,
                s.agent_state,
                s.prompts_sent,
                s.last_prompt_at.as_deref().unwrap_or("-"),
                s.title
            ));
            if let Some(p) = &s.worktree_path {
                out.push_str(&format!(
                    "          {p}{}\n",
                    match s.workspace_state.as_deref() {
                        Some("kept") => "  (workspace kept)",
                        Some("pending") => "  (being released)",
                        Some("given-up") => "  (release given up, workspace kept)",
                        Some("gone") => "  (workspace gone)",
                        _ => "",
                    }
                ));
            } else if s.workspace_state.as_deref() == Some("released") {
                out.push_str(&format!(
                    "          workspace released {}\n",
                    ago(s.released_at.as_deref())
                ));
            }
            if let Some(next) = s.next_launch_change() {
                out.push_str(&format!("          {next}\n"));
            }
            if let Some(o) = &s.overrides {
                let mut what = format!("harness={}", o.harness);
                if let Some(m) = &o.model {
                    what.push_str(&format!(" model={m}"));
                }
                if let Some(e) = &o.effort {
                    what.push_str(&format!(" effort={e}"));
                }
                out.push_str(&format!(
                    "          {}: {what}\n",
                    if s.assigned_stack {
                        "assigned"
                    } else {
                        "handed over"
                    }
                ));
            }
            if let Some(h) = &s.handover {
                out.push_str(&format!("          handover pending: {}\n", h.describe()));
            }
            if let Some(n) = &s.handover_note {
                out.push_str(&format!(
                    "          handover note waiting: {}\n",
                    n.describe()
                ));
            }
            if let Some(b) = &s.blocked {
                out.push_str(&format!("          BLOCKED: {}\n", b.describe()));
            }
            if let Some(m) = &s.last_assistant_message {
                out.push_str(&format!("          said: {}\n", one_line(m, 100)));
            }
            if let Some(o) = &s.origin {
                out.push_str(&format!("          opened by session {o}\n"));
            }
            if !s.posts_by_session.is_empty() {
                let parts: Vec<String> = s
                    .posts_by_session
                    .iter()
                    .map(|(o, n)| format!("{o} ({n})"))
                    .collect();
                out.push_str(&format!(
                    "          posts from sessions: {}\n",
                    parts.join(", ")
                ));
            }
            if s.untagged_posts > 0 {
                out.push_str(&format!(
                    "          {} untagged post(s) by the bot (a person, or the gh shim not in effect)\n",
                    s.untagged_posts
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;

/// Cards are built once in the canonical server model for both dashboard clients.
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}
fn issue(row: &Value, fallback: &str) -> Value {
    let id = row["id"].as_str().unwrap_or(fallback);
    let url = row["url"]
        .as_str()
        .and_then(|url| reqwest::Url::parse(url).ok())
        .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some());
    // Whether ssf has a workspace recorded for the item, and the branch it is
    // on when the driver reports one. A client cannot ask a factory to start a
    // session on an item that already has a workspace -- `ssf assign` refuses
    // it, because `ssf release` is what frees it -- so the client that offers
    // the write has to be able to tell (#428). An item bound to another
    // session's workspace carries that one's, which is why this is the
    // record's own `worktree_id` and not a workspace the driver happens to
    // report.
    let has_workspace = row["worktree_id"].as_str().is_some_and(|id| !id.is_empty());
    let branch = match text(row, "branch") {
        "" => Value::Null,
        branch => Value::String(branch.to_owned()),
    };
    json!({"id":id,"title":row["title"].as_str().unwrap_or(id),"url":url.map(|url|url.to_string()),
        "kind":row["kind"].as_str().unwrap_or("issue"),"active":row["active"] == true,
        "has_workspace":has_workspace,"branch":branch})
}

/// Why a card carries no `last_activity_at`, when it carries none: ssf dates a
/// session from the local transcript its harness keeps, and every way that can
/// be absent is a fact about the session rather than a silence. A client that
/// printed "unknown" instead was making a claim about the agent out of a gap in
/// the record, which is what the overlay did on every card of every factory
/// whose sessions run OMP (#439).
///
/// The reasons are the model's own, one per way the fact can be missing, so
/// every client says the same sentence rather than inventing its own.
fn activity_note(runtime: &Value) -> Option<&'static str> {
    if !crate::sessions::reports_activity(text(runtime, "harness")) {
        return Some("the harness keeps no local transcript ssf can read");
    }
    if text(runtime, "agent_session_id").is_empty() {
        return Some("the session's conversation is not identified yet");
    }
    Some("ssf has not found the session's transcript yet")
}

/// The factory a payload came from, on every card: the name the server was
/// started under when it answers for a catalog target (`ssf-server --target
/// …`; the status stream names it in `server`), else the machine's own
/// hostname. A client holding several factories needs it on the card rather
/// than only on the page it came from.
fn factory_label(payload: &Value) -> String {
    match &payload["server"] {
        Value::String(name) if !name.is_empty() => name.clone(),
        server => {
            let hostname = text(server, "hostname");
            if hostname.is_empty() {
                crate::hostname()
            } else {
                hostname.to_owned()
            }
        }
    }
}

/// The repositories this factory watches, as `owner/name`: the one fact that
/// lets a client offering to start a session tell an item ssf has no record of
/// from an item the factory does not know at all. `ssf assign` accepts the
/// first -- any open item in a watched repository takes a session -- and
/// refuses the second, and without this the two are the same nothing in the
/// model, so a client that could offer the write for one offers it for neither
/// (#435). A payload without the repository list publishes an empty one, and a
/// client that reads it falls back to what it drew before.
fn watched_repositories(payload: &Value) -> Vec<String> {
    payload["repos"]
        .as_array()
        .map(|repos| {
            repos
                .iter()
                .map(|repo| text(repo, "name").to_owned())
                .filter(|name| !name.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn dashboard_presentation(payload: &Value) -> anyhow::Result<Value> {
    let rows = payload["sessions"]
        .as_array()
        .context("SSF returned status data in an unexpected format")?;
    let factory = factory_label(payload);
    let relevant: Vec<_> = rows
        .iter()
        .filter(|row| {
            (row["active"] == true || row["agent_live"] == true)
                && row["subscriber_only"] != true
                && !text(row, "owner").is_empty()
        })
        .collect();
    let mut owners = Vec::new();
    let mut cards = Vec::new();
    let mut unattached = Vec::new();
    for row in &relevant {
        let owner = text(row, "owner");
        if owners.contains(&owner) {
            continue;
        }
        owners.push(owner);
        let primary = rows
            .iter()
            .find(|row| text(row, "id") == owner)
            .unwrap_or(&Value::Null);
        let owned: Vec<_> = relevant
            .iter()
            .copied()
            .filter(|row| row["active"] == true && text(row, "owner") == owner)
            .collect();
        let mut candidates: Vec<_> = rows
            .iter()
            .filter(|candidate| {
                candidate["agent_live"] == true
                    && (text(candidate, "id") == owner || text(candidate, "owner") == owner)
            })
            .collect();
        if candidates.is_empty() {
            unattached.extend(owned.iter().map(|row| issue(row, owner)));
            continue;
        }
        candidates.sort_by(|a, b| text(b, "last_activity_at").cmp(text(a, "last_activity_at")));
        let runtime = candidates[0];
        let message = candidates
            .iter()
            .map(|row| text(row, "last_assistant_message").trim())
            .find(|message| !message.is_empty())
            .map(|message| message.chars().take(4000).collect::<String>());
        let metadata = |key| {
            let value = text(primary, key);
            if value.is_empty() {
                text(runtime, key).to_owned()
            } else {
                value.to_owned()
            }
        };
        // What a card carries as null: the tool call an idle agent is not
        // making, and the branch of an item with no workspace.
        let optional = |key| {
            let value = metadata(key);
            if value.is_empty() {
                Value::Null
            } else {
                Value::String(value)
            }
        };
        // The stack the next launch uses when it is not the one running:
        // the owning row's own, or the live row's (a bound item is worked
        // in its owner's workspace, so the pane there is what is running).
        let next_launch = [primary, runtime]
            .into_iter()
            .map(|row| row["next_launch"].clone())
            .find(|value| !value.is_null())
            .unwrap_or(Value::Null);
        // A handover the daemon has accepted and not carried out yet, same
        // rows as the stack: it is the item's own fact, and it is waiting on
        // this session's workspace (#439).
        let handover = [primary, runtime]
            .into_iter()
            .map(|row| row["handover"].clone())
            .find(|value| !value.is_null())
            .unwrap_or(Value::Null);
        // Why there is no activity time, where there is none: the card's own
        // fact, so every client says the same sentence instead of "unknown"
        // (#439).
        let activity_note = if runtime["last_activity_at"].is_null() {
            activity_note(runtime)
        } else {
            None
        };
        cards.push(json!({"owner":owner,"origin":issue(primary,owner),"additional":owned.iter().filter(|row|text(row,"id") != owner).map(|row|issue(row,owner)).collect::<Vec<_>>(),"agent_state":runtime["agent_state"].as_str().unwrap_or("unknown"),"last_activity_at":runtime["last_activity_at"],"activity_note":activity_note,"last_assistant_message":message,"harness":metadata("harness"),"model":metadata("model"),"effort":metadata("effort"),"tool":optional("tool"),"branch":optional("branch"),"worktree_path":optional("worktree_path"),"factory":factory,"next_launch":next_launch,"handover":handover,"agent_session_id":metadata("agent_session_id"),"owner_login":primary["owner_login"]}));
    }
    let warning = if payload["factory_reachable"] == false {
        let state = text(&payload["host_vm"], "state");
        Some(format!(
            "SSF could not reach the guest factory{}",
            if state.is_empty() {
                String::new()
            } else {
                format!(" (VM {state})")
            }
        ))
    } else if payload["driver"]["available"] == false {
        let detail = text(&payload["driver"], "error").trim();
        Some(
            if detail.is_empty() {
                "SSF could not reach one or more session drivers"
            } else {
                detail
            }
            .to_owned(),
        )
    } else if payload["service_active"] == false {
        Some("SSF service is inactive; showing latest saved state".to_owned())
    } else if let Some(last_poll) = payload["last_poll_at"].as_str() {
        match chrono::DateTime::parse_from_rfc3339(last_poll) {
            Ok(at)
                if chrono::Utc::now().signed_duration_since(at).num_seconds()
                    > (payload["poll_interval_secs"].as_i64().unwrap_or(60) * 3).max(60) =>
            {
                Some("SSF daemon state is stale; last successful poll is overdue".to_owned())
            }
            _ => None,
        }
    } else {
        None
    };
    Ok(
        json!({"cards":cards,"monitored_items":unattached,"repositories":watched_repositories(payload),"warning":warning,"refreshed_at":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs_f64()}),
    )
}

#[cfg(test)]
mod dashboard_tests {
    use super::*;
    #[test]
    fn presents_server_ownership_and_latest_message_safely() {
        let snapshot = dashboard_presentation(&json!({"server":"factory-one","sessions":[
            {"id":"r#1","title":"Origin","active":false,"harness":"codex","url":"javascript:alert(1)"},
            {"id":"r#2","owner":"r#1","active":true,"agent_live":true,"agent_state":"working","last_activity_at":"2026-09-12T12:00:00Z","last_assistant_message":" Earlier "},
            {"id":"r#3","owner":"r#1","active":true,"agent_live":true,"agent_state":"idle","last_activity_at":"2026-09-12T13:00:00Z","last_assistant_message":" <script>latest</script> ","tool":"Bash: cargo test","branch":"bot/issue-1-origin","effort":"high"},
            {"id":"r#4","owner":"r#4","active":true,"subscriber_only":true}
        ]})).unwrap();
        let cards = snapshot["cards"].as_array().unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["origin"]["title"], "Origin");
        assert!(cards[0]["origin"]["url"].is_null());
        assert_eq!(cards[0]["additional"].as_array().unwrap().len(), 2);
        assert_eq!(cards[0]["agent_state"], "idle");
        assert_eq!(cards[0]["harness"], "codex");
        assert_eq!(cards[0]["tool"], "Bash: cargo test");
        assert_eq!(cards[0]["branch"], "bot/issue-1-origin");
        // The effort the card's stack is on, so a hand-over from the overlay
        // can prefill every picker rather than only the harness and model.
        assert_eq!(cards[0]["effort"], "high");
        assert_eq!(cards[0]["factory"], "factory-one");
        assert_eq!(
            cards[0]["last_assistant_message"],
            "<script>latest</script>"
        );
    }

    /// A card with no activity time says why it has none, in the model rather
    /// than in each client: ssf dates a session from a local transcript, and
    /// the ways that can be missing are different facts. Printing "unknown"
    /// for all of them read as a claim about the agent (#439).
    #[test]
    fn cards_without_an_activity_time_say_why() {
        let snapshot = dashboard_presentation(&json!({"sessions":[
            {"id":"r#1","owner":"r#1","active":true,"agent_live":true,"agent_state":"working",
                "harness":"omp","agent_session_id":"abc"},
            {"id":"r#2","owner":"r#2","active":true,"agent_live":true,"agent_state":"idle",
                "harness":"claude"},
            {"id":"r#3","owner":"r#3","active":true,"agent_live":true,"agent_state":"idle",
                "harness":"codex","agent_session_id":"9f2c"},
            {"id":"r#4","owner":"r#4","active":true,"agent_live":true,"agent_state":"idle",
                "harness":"claude","agent_session_id":"9f2c","last_activity_at":"2026-09-12T13:00:00Z"}
        ]}))
        .unwrap();
        let cards = snapshot["cards"].as_array().unwrap();
        assert_eq!(
            cards[0]["activity_note"],
            "the harness keeps no local transcript ssf can read"
        );
        assert_eq!(
            cards[1]["activity_note"],
            "the session's conversation is not identified yet"
        );
        assert_eq!(
            cards[2]["activity_note"],
            "ssf has not found the session's transcript yet"
        );
        // A card with a time has nothing to explain.
        assert!(cards[3]["activity_note"].is_null());
        assert_eq!(cards[3]["last_activity_at"], "2026-09-12T13:00:00Z");
    }

    /// A card says which factory it came from, and what an item without a
    /// live agent has none of: a tool call to show, or a workspace branch.
    #[test]
    fn cards_name_the_factory_and_leave_what_is_absent_null() {
        let snapshot = dashboard_presentation(&json!({
            "server":{"hostname":"host-one","location":"local"},
            "sessions":[{"id":"r#2","owner":"r#2","active":true,"agent_live":true,"agent_state":"idle"}]
        }))
        .unwrap();
        let card = &snapshot["cards"][0];
        assert_eq!(card["factory"], "host-one");
        assert!(card["tool"].is_null());
        assert!(card["branch"].is_null());
    }

    /// Details on a card is drawn from the model's own fields, so every one the
    /// overlay's rows read is published: where the session runs, and a handover
    /// the daemon has accepted and not carried out (#439).
    #[test]
    fn cards_publish_the_workspace_and_a_pending_handover() {
        let snapshot = dashboard_presentation(&json!({"sessions":[
            {"id":"r#1","owner":"r#1","active":true,"agent_live":true,"agent_state":"idle",
                "worktree_path":"/home/bot/ssf/projects/issue-1",
                "handover":{"harness":"claude","harness_name":"Claude Code","model":"opus",
                    "effort":"low","summary_chars":120,"by":"o/r#12",
                    "requested_at":"2026-09-12T13:00:00Z"}}
        ]}))
        .unwrap();
        let card = &snapshot["cards"][0];
        assert_eq!(card["worktree_path"], "/home/bot/ssf/projects/issue-1");
        assert_eq!(card["handover"]["harness"], "claude");
        assert_eq!(card["handover"]["effort"], "low");
        assert_eq!(card["handover"]["by"], "o/r#12");
        // No handover pending is null, not an empty object: a client draws the
        // row only where there is one.
        let quiet = dashboard_presentation(&json!({"sessions":[
            {"id":"r#1","owner":"r#1","active":true,"agent_live":true,"agent_state":"idle"}
        ]}))
        .unwrap();
        assert!(quiet["cards"][0]["handover"].is_null());
        assert!(quiet["cards"][0]["worktree_path"].is_null());
    }

    #[test]
    fn distinguishes_unreachable_vm_and_driver_errors_from_empty_factory() {
        let snapshot = dashboard_presentation(
            &json!({"sessions":[],"factory_reachable":false,"host_vm":{"state":"stopped"}}),
        )
        .unwrap();
        assert!(snapshot["warning"].as_str().unwrap().contains("VM stopped"));
        let snapshot = dashboard_presentation(
            &json!({"sessions":[],"driver":{"available":false,"error":"driver unavailable"}}),
        )
        .unwrap();
        assert_eq!(snapshot["warning"], "driver unavailable");
        assert!(dashboard_presentation(&json!({"sessions":[]})).unwrap()["warning"].is_null());
        assert!(dashboard_presentation(&json!({"error":"not a snapshot"})).is_err());
    }

    #[test]
    fn cards_carry_canonical_conversation_and_stale_daemon_warning() {
        let snapshot = dashboard_presentation(&json!({
            "sessions":[{"id":"r#1", "owner":"r#1", "active":true, "agent_live":true,
                "agent_session_id":"conversation-id"}],
            "service_active":true, "last_poll_at":"2000-01-01T00:00:00Z",
            "poll_interval_secs":10
        }))
        .unwrap();
        assert_eq!(snapshot["cards"][0]["agent_session_id"], "conversation-id");
        assert!(snapshot["warning"].as_str().unwrap().contains("stale"));
        let stopped =
            dashboard_presentation(&json!({"sessions":[],"service_active":false})).unwrap();
        assert!(stopped["warning"].as_str().unwrap().contains("inactive"));
    }

    #[test]
    fn monitored_unbound_items_never_become_live_agent_cards() {
        let snapshot = dashboard_presentation(&json!({"sessions":[
            {"id":"r#225","title":"Released origin","owner":"r#225","active":false,
                "agent_live":false,"agent_state":"unbound","released_at":"2026-09-12T15:42:00Z"},
            {"id":"r#226","title":"Still monitored","owner":"r#225","active":true,
                "agent_live":false,"agent_state":"unbound"},
            {"id":"r#227","title":"Also monitored","owner":"r#225","active":true,
                "agent_live":false,"agent_state":"unbound"}
        ]}))
        .unwrap();
        assert!(snapshot["cards"].as_array().unwrap().is_empty());
        assert_eq!(snapshot["monitored_items"].as_array().unwrap().len(), 2);
        assert_eq!(snapshot["monitored_items"][0]["id"], "r#226");
    }

    /// A monitored item says whether ssf has a workspace for it, and on which
    /// branch: `ssf assign` refuses an item that has one, so the client that
    /// offers the write has to be able to tell it apart from an item that
    /// takes one (#428).
    #[test]
    fn monitored_items_say_whether_a_workspace_is_in_the_way() {
        let snapshot = dashboard_presentation(&json!({"sessions":[
            {"id":"r#1","title":"Has a workspace","owner":"r#1","active":true,
                "agent_live":false,"agent_state":"no-agent","worktree_id":"w1",
                "branch":"bot/issue-1-thing"},
            {"id":"r#2","title":"Recorded, no longer in the driver","owner":"r#2","active":true,
                "agent_live":false,"agent_state":"no-workspace","worktree_id":"w2"},
            {"id":"r#3","title":"Free","owner":"r#3","active":true,
                "agent_live":false,"agent_state":"unbound"}
        ]}))
        .unwrap();
        let monitored = snapshot["monitored_items"].as_array().unwrap();
        assert_eq!(monitored.len(), 3);
        assert_eq!(monitored[0]["has_workspace"], true);
        // The branch is the workspace's, as the model normalises it: the
        // `refs/heads/` prefix is stripped however the driver reported it.
        assert_eq!(monitored[0]["branch"], "bot/issue-1-thing");
        assert_eq!(monitored[1]["has_workspace"], true);
        assert!(monitored[1]["branch"].is_null());
        assert_eq!(monitored[2]["has_workspace"], false);
    }

    /// The model names the repositories the factory watches, so a client that
    /// offers `ssf assign` can tell an item this factory has no record of --
    /// which takes a session -- from an item it does not know at all, which
    /// the write refuses. Before this the two were the same emptiness in the
    /// model, and the client offered neither (#435).
    #[test]
    fn the_model_names_the_repositories_the_factory_watches() {
        let snapshot = dashboard_presentation(&json!({
            "sessions":[],
            "repos":[{"name":"o/r","harness":"omp"},{"name":"o/other"},{"harness":"omp"}]
        }))
        .unwrap();
        assert_eq!(
            snapshot["repositories"],
            json!(["o/r", "o/other"]),
            "every configured repository, in the order the factory lists them"
        );
        // A payload from a server that does not publish them leaves the fact
        // empty rather than absent, so a client reads one shape.
        let older = dashboard_presentation(&json!({"sessions":[]})).unwrap();
        assert_eq!(older["repositories"], json!([]));
    }
}
