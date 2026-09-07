//! The joined view behind `ssf status`, `ssf peers` and the bar widget: what
//! ssf knows about each item (issue or PR, GitHub state, triggers, prompts,
//! session id) next to what Orca reports about the workspace working on it
//! (agent state, last assistant message, current tool, last activity, board
//! column, branch). The widget reads this and never talks to Orca itself.

use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

use crate::config::{Config, DriverKind, RepoConfig};
use crate::driver::Drivers;
use crate::engine::MAX_RELEASE_REFUSALS;
use crate::github::PrInfo;
use crate::orca::WorkspaceInfo;
use crate::state::{Blocked, HandoverNote, IssueState, Overrides, PendingHandover, State};

/// How long `ssf status` waits for a driver before reporting it unavailable;
/// the bar widget polls this, so it must never hang.
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
    pub number: u64,
    /// `issue` or `pull_request`.
    pub kind: String,
    pub title: String,
    pub url: String,
    /// `open`, `closed`, `merged`, or `unknown` for items bound before this was recorded.
    pub github_state: String,
    /// Still assigned/mentioned/requested and open as of the last poll.
    pub active: bool,
    /// Why the bot got involved: `assigned`, `mentioned`, `review_requested`,
    /// `created` (the bot's own item).
    pub triggers: Vec<String>,
    /// The harness this item's session runs, the per-item overrides of a
    /// handover applied (`overrides` says whether they are in play).
    pub harness: String,
    /// Model and effort the session runs with, overrides applied; `None`
    /// is the harness's own default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Set when a handover moved this session off the repository's
    /// configured harness, model or effort.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<Overrides>,
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
    /// Orca's agent state (`working`, `done`, `open`, `waiting`, ...), or
    /// `no-agent` (workspace without an agent), `no-workspace` (ssf has a
    /// binding but Orca has no such workspace), `unbound` (no workspace yet),
    /// `unknown` (Orca could not be asked).
    pub agent_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
    /// Tool the agent is running right now, as `Name: input`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
    /// Orca board column.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    /// The matching `orca worktree ps` row, verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceInfo>,
    /// The session cannot take prompts: its harness is at a login prompt
    /// (`reason` is `login`; `harness`, `detail`, `since` say which, what
    /// the screen showed and from when). Deliveries are held until the
    /// login is back; a person has to sign the harness in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<BlockedView>,
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

    /// One line for a person: `from Claude Code, summary 1,234 chars`.
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

/// A session's block, for `ssf status --json` and the widget.
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
    if b.reason == Blocked::START {
        fix_for(b)
    } else {
        format!("sign in with {}", fix_for(b))
    }
}

impl Session {
    pub fn is_pull_request(&self) -> bool {
        self.kind == "pull_request"
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
        json!({
            "bot_login": self.state.bot_login,
            "token_configured": self.cfg.github_token().is_ok(),
            "service_enabled": crate::ui::service_enabled(),
            "service_active": crate::ui::service_active(),
            "last_poll_at": self.state.last_poll_at,
            "last_error": self.state.last_error,
            "poll_interval_secs": self.cfg.daemon.poll_interval_secs,
            "config_path": crate::config::config_path(),
            // The wildcard allow-list is in effect somewhere: the widget
            // shows a warning while it is.
            "anyone_allowed": self.cfg.anyone_allowed_anywhere(),
            // Sessions whose harness is not signed in (the widget shows an
            // urgent line per one).
            "blocked_sessions": sessions.iter().filter(|s| s.blocked.is_some()).map(|s| s.id.clone()).collect::<Vec<_>>(),
            // Keyed `orca` from when it was the only driver; the widget reads it.
            "orca": {
                "available": self.available(),
                "error": self.error(),
                "workspaces": self.workspaces.len(),
                "down": self.down.iter().map(|k| k.id()).collect::<Vec<_>>(),
            },
            "sessions": sessions,
            "repos": repos,
        })
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
            // so the harness a handover put in it. The binding is followed
            // to its end, as the daemon follows it: an item bound to a
            // bound item is the first one's session's too.
            let owner = crate::state::owner_in(&rs.issues, item.number);
            let overrides = rs
                .issues
                .get(&owner)
                .or(Some(item))
                .and_then(|o| o.overrides.as_ref());
            out.push(join(repo, item, owner, overrides, ws, workspaces.is_some()));
        }
    }
    out
}

/// The Orca workspace of a record: by id, else by Orca's own link to the
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
    orca_available: bool,
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
        } else if ws.is_some() || !orca_available {
            "kept"
        } else {
            "gone"
        }
        .into(),
    )
}

/// `owner` is the item whose session acts on this one (itself, unless it
/// is bound), and `overrides` the ones that govern it: the owner's.
fn join(
    repo: &RepoConfig,
    item: &IssueState,
    owner: u64,
    overrides: Option<&Overrides>,
    ws: Option<&WorkspaceInfo>,
    orca_available: bool,
) -> Session {
    let eff = repo.with_overrides(overrides);
    let agent = ws.and_then(WorkspaceInfo::primary_agent);
    let agent_state = match (ws, agent) {
        (Some(_), Some(a)) => a.state.clone(),
        (Some(_), None) => "no-agent".into(),
        (None, _) if !orca_available => "unknown".into(),
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
        harness: eff.harness.clone(),
        model: eff.model.clone(),
        effort: eff.effort.clone(),
        overrides: overrides.cloned(),
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
            .and_then(|w| w.branch.clone())
            .or_else(|| item.branch.as_deref().map(strip_ref)),
        agent_session_id: item.agent_session_id.clone(),
        prompts_sent: item.prompts_sent,
        last_prompt_at: item.last_prompt_at.clone(),
        bound_at: item.bound_at.clone(),
        retired_at: item.retired_at.clone(),
        released_at: item.released_at.clone(),
        workspace_state: workspace_state(item, ws, orca_available),
        pr: item.pr.clone(),
        origin: item.origin.clone(),
        posts_by_session: item.origins.values().fold(BTreeMap::new(), |mut by, o| {
            *by.entry(o.clone()).or_default() += 1;
            by
        }),
        untagged_posts: item.untagged.len(),
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
        let kind = if s.is_pull_request() { "PR" } else { "issue" };
        let marker = if me == Some(s.id.as_str()) {
            " (you)"
        } else {
            ""
        };
        let activity = ago(s.last_activity_at.as_deref());
        out.push_str(&format!(
            "  #{:<5} {:<5} {:<7} {:<12} {:>4}  {}{}\n",
            s.number,
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
        if let Some(o) = &s.overrides {
            facts.push(format!("handed over to {}", o.harness));
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
        st.bot_login.as_deref().unwrap_or("(not signed in)")
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
                "  #{:<6} {:<8} {:<7} {:<12} prompts={:<3} last={}  {}\n",
                s.number,
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
            if let Some(o) = &s.overrides {
                let mut what = format!("harness={}", o.harness);
                if let Some(m) = &o.model {
                    what.push_str(&format!(" model={m}"));
                }
                if let Some(e) = &o.effort {
                    what.push_str(&format!(" effort={e}"));
                }
                out.push_str(&format!("          handed over: {what}\n"));
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
mod tests {
    use super::*;
    use crate::orca::AgentInfo;
    use crate::state::RepoState;

    fn cfg() -> Config {
        let mut cfg = Config::default();
        cfg.repos.push(RepoConfig {
            name: "acme/widgets".into(),
            harness: "claude".into(),
            ..Default::default()
        });
        cfg
    }

    fn item(number: u64, worktree_id: Option<&str>) -> IssueState {
        IssueState {
            number,
            title: format!("Item {number}"),
            html_url: format!("https://github.com/acme/widgets/issues/{number}"),
            worktree_id: worktree_id.map(str::to_string),
            repo_id: Some("r1".into()),
            branch: Some("refs/heads/bot/issue-1".into()),
            active: true,
            kind: Some("issue".into()),
            github_state: None,
            triggers: vec!["assigned".into()],
            agent_session_id: Some("sess-1".into()),
            prompts_sent: 2,
            ..Default::default()
        }
    }

    fn state_with(items: Vec<IssueState>) -> State {
        let mut st = State::default();
        let mut rs = RepoState::default();
        for i in items {
            rs.issues.insert(i.number, i);
        }
        st.repos.insert("acme/widgets".into(), rs);
        st
    }

    fn workspace(id: &str, issue: Option<u64>, agent: Option<AgentInfo>) -> WorkspaceInfo {
        WorkspaceInfo {
            worktree_id: id.into(),
            repo_id: "r1".into(),
            path: format!("/w/{id}"),
            branch: Some("bot/issue-1".into()),
            column: Some("in-progress".into()),
            linked_issue: issue,
            last_activity_at: Some("2026-09-03T11:00:00Z".into()),
            agents: agent.into_iter().collect(),
            ..Default::default()
        }
    }

    #[test]
    fn joins_by_worktree_id() {
        let st = state_with(vec![item(1, Some("r1::/w/one"))]);
        let ws = vec![workspace(
            "r1::/w/one",
            None,
            Some(AgentInfo {
                state: "working".into(),
                tool_name: Some("Bash".into()),
                tool_input: Some("cargo test\n--all".into()),
                last_assistant_message: Some("Running the tests now.".into()),
                ..Default::default()
            }),
        )];
        let s = sessions(&cfg(), &st, Some(&ws));
        assert_eq!(s.len(), 1);
        let s = &s[0];
        assert_eq!(s.id, "acme/widgets#1");
        assert_eq!(s.owner, "acme/widgets#1");
        assert_eq!(s.agent_state, "working");
        assert_eq!(s.tool.as_deref(), Some("Bash: cargo test"));
        assert_eq!(
            s.last_assistant_message.as_deref(),
            Some("Running the tests now.")
        );
        assert_eq!(s.branch.as_deref(), Some("bot/issue-1"));
        assert_eq!(s.column.as_deref(), Some("in-progress"));
        assert_eq!(s.last_activity_at.as_deref(), Some("2026-09-03T11:00:00Z"));
        assert_eq!(s.worktree_path.as_deref(), Some("/w/r1::/w/one"));
        assert_eq!(s.github_state, "open");
        assert!(s.workspace.is_some());
    }

    #[test]
    fn falls_back_to_orca_link_when_binding_is_stale() {
        let st = state_with(vec![item(7, Some("r1::/w/gone"))]);
        let ws = vec![workspace("r1::/w/seven", Some(7), None)];
        let s = sessions(&cfg(), &st, Some(&ws));
        assert_eq!(s[0].agent_state, "no-agent");
        assert_eq!(s[0].worktree_path.as_deref(), Some("/w/r1::/w/seven"));
    }

    #[test]
    fn reports_missing_workspace_and_unavailable_orca() {
        let st = state_with(vec![item(1, Some("r1::/w/one")), item(2, None)]);
        let s = sessions(&cfg(), &st, Some(&[]));
        assert_eq!(s[0].agent_state, "no-workspace");
        assert_eq!(s[1].agent_state, "unbound");
        // Without Orca the binding's own branch still shows, without the ref prefix.
        let s = sessions(&cfg(), &st, None);
        assert_eq!(s[0].agent_state, "unknown");
        assert_eq!(s[0].branch.as_deref(), Some("bot/issue-1"));
        assert!(s[0].workspace.is_none());
        // Recorded state wins; without it an active item is open, a retired one unknown.
        let mut retired = item(3, None);
        retired.active = false;
        let mut merged = item(4, None);
        merged.github_state = Some("merged".into());
        let st = state_with(vec![retired, merged]);
        let s = sessions(&cfg(), &st, None);
        assert_eq!(s[0].github_state, "unknown");
        assert_eq!(s[1].github_state, "merged");
    }

    #[test]
    fn pr_joining_an_issue_workspace_is_owned_by_that_session() {
        let mut pr = item(9, Some("r1::/w/one"));
        pr.kind = Some("pull_request".into());
        pr.shares_workspace_of = Some(1);
        let st = state_with(vec![item(1, Some("r1::/w/one")), pr]);
        let s = sessions(&cfg(), &st, Some(&[]));
        let pr = s.iter().find(|s| s.number == 9).unwrap();
        assert_eq!(pr.owner, "acme/widgets#1");
        assert_eq!(pr.shares_workspace_of.as_deref(), Some("acme/widgets#1"));
        assert!(pr.is_pull_request());
    }

    #[test]
    fn json_keeps_the_old_issue_fields() {
        let mut cfg = cfg();
        cfg.driver = Some(DriverKind::Orca);
        let snap = Snapshot {
            cfg,
            state: state_with(vec![item(1, Some("r1::/w/one"))]),
            workspaces: Vec::new(),
            down: vec![DriverKind::Orca],
            errors: vec!["not running".into()],
        };
        let v = snap.to_json();
        assert_eq!(v["orca"]["available"], false);
        assert_eq!(v["orca"]["error"], "not running");
        let issue = &v["repos"][0]["issues"][0];
        for key in [
            "number",
            "title",
            "url",
            "active",
            "worktree_id",
            "prompts_sent",
        ] {
            assert!(!issue[key].is_null(), "{key} missing");
        }
        assert_eq!(v["sessions"][0]["agent_state"], "unknown");
        assert_eq!(v["sessions"][0]["subscribers"], json!([]));
        assert_eq!(v["sessions"][0]["untagged_posts"], 0);
    }

    #[test]
    fn origin_tags_are_summarised_per_session() {
        let mut it = item(1, None);
        it.origin = Some("acme/widgets#9".into());
        it.origins.insert("c1".into(), "acme/widgets#9".into());
        it.origins.insert("c2".into(), "acme/widgets#9".into());
        it.origins.insert("c3".into(), "acme/widgets#4".into());
        it.untagged.insert("c4".into(), "https://x".into());
        let st = state_with(vec![it]);
        let s = sessions(&cfg(), &st, None);
        assert_eq!(s[0].origin.as_deref(), Some("acme/widgets#9"));
        assert_eq!(s[0].posts_by_session["acme/widgets#9"], 2);
        assert_eq!(s[0].posts_by_session["acme/widgets#4"], 1);
        assert_eq!(s[0].untagged_posts, 1);
        assert!(render_peers(&s, None).contains("opened by acme/widgets#9"));
    }

    #[test]
    fn peers_table_lists_each_session_with_its_facts() {
        let st = state_with(vec![item(1, Some("r1::/w/one"))]);
        let ws = vec![workspace(
            "r1::/w/one",
            None,
            Some(AgentInfo {
                state: "done".into(),
                last_assistant_message: Some("Opened PR #2.\nDone.".into()),
                ..Default::default()
            }),
        )];
        let s = sessions(&cfg(), &st, Some(&ws));
        let text = render_peers(&s, Some("acme/widgets#1"));
        assert!(text.starts_with("acme/widgets\n"));
        assert!(text.contains("#1     issue open    done"), "{text}");
        assert!(text.contains("Item 1 (you)"), "{text}");
        assert!(
            text.contains(
                "branch bot/issue-1  ·  column in-progress  ·  via assigned  ·  prompts 2"
            ),
            "{text}"
        );
        assert!(text.contains("said: Opened PR #2. Done."), "{text}");
    }

    #[test]
    fn a_blocked_session_is_flagged_everywhere() {
        let mut it = item(1, Some("r1::/w/one"));
        it.blocked = Some(Blocked {
            reason: "login".into(),
            harness: "claude".into(),
            detail: "Login expired · Please run /login".into(),
            since: "2026-09-06T14:30:00Z".into(),
            reported: true,
            credential: None,
            retried_at: None,
            retries: 0,
            told_at: None,
            tell_failures: 0,
        });
        let st = state_with(vec![it, item(2, None)]);
        let s = sessions(&cfg(), &st, Some(&[]));
        let b = s[0].blocked.as_ref().unwrap();
        assert_eq!(b.reason, "login");
        assert!(b.fix.contains("claude auth login"), "{}", b.fix);
        assert_eq!(b.harness_name, "Claude Code");
        assert!(
            b.describe()
                .starts_with("Claude Code at its sign-in prompt since"),
            "{}",
            b.describe()
        );
        assert!(s[1].blocked.is_none());
        let table = render_peers(&s, None);
        assert!(
            table.contains("BLOCKED: Claude Code at its sign-in prompt"),
            "{table}"
        );
        let snap = Snapshot {
            cfg: cfg(),
            state: st,
            workspaces: Vec::new(),
            down: Vec::new(),
            errors: Vec::new(),
        };
        let v = snap.to_json();
        assert_eq!(v["blocked_sessions"], json!(["acme/widgets#1"]));
        assert_eq!(v["sessions"][0]["blocked"]["harness"], "claude");
        assert!(v["sessions"][1]["blocked"].is_null());
        let text = render_status(&snap);
        assert!(
            text.contains("BLOCKED: acme/widgets#1: Claude Code at its sign-in prompt"),
            "{text}"
        );
        // The other reason: the harness never came up (a handover to a
        // harness that exits as it is launched).
        let mut it = item(1, Some("r1::/w/one"));
        it.blocked = Some(Blocked {
            reason: Blocked::START.into(),
            harness: "pi".into(),
            detail: "pi exited at once: ambiguous model".into(),
            since: "2026-09-06T14:30:00Z".into(),
            reported: true,
            ..Default::default()
        });
        let s = sessions(&cfg(), &state_with(vec![it]), Some(&[]));
        let b = s[0].blocked.as_ref().unwrap();
        assert!(b.fix.contains("start Pi by hand"), "{}", b.fix);
        assert!(
            b.describe().starts_with("Pi could not be started since"),
            "{}",
            b.describe()
        );
        assert!(
            render_peers(&s, None).contains("BLOCKED: Pi could not be started since"),
            "{}",
            render_peers(&s, None)
        );
    }

    #[test]
    fn a_handed_over_session_shows_its_own_harness_and_a_pending_handover() {
        // #1 has been handed over to Pi; #2 shares its workspace, so it
        // runs the same harness; #3 has a handover waiting for the pass.
        let mut one = item(1, Some("r1::/w/one"));
        one.overrides = Some(Overrides {
            harness: "pi".into(),
            model: Some("openai/gpt-6".into()),
            effort: Some("high".into()),
        });
        // The harness it went to never read what the outgoing session
        // left: the note waits on the item for the one that does.
        one.handover_note = Some(HandoverNote {
            from: "Claude Code".into(),
            summary: Some("half migrated".into()),
        });
        let mut two = item(2, Some("r1::/w/one"));
        two.shares_workspace_of = Some(1);
        // Bound to the bound item: the chain leads to #1 all the same.
        let mut four = item(4, Some("r1::/w/one"));
        four.shares_workspace_of = Some(2);
        let mut three = item(3, Some("r1::/w/three"));
        three.handover = Some(PendingHandover {
            harness: "codex".into(),
            model: None,
            effort: None,
            summary: Some("half done".into()),
            by: Some("acme/widgets#3".into()),
            requested_at: "2026-09-07T10:00:00Z".into(),
        });
        let st = state_with(vec![one, two, three, four]);
        let s = sessions(&cfg(), &st, Some(&[]));
        assert_eq!(s[0].harness, "pi");
        assert_eq!(s[0].model.as_deref(), Some("openai/gpt-6"));
        assert_eq!(s[0].effort.as_deref(), Some("high"));
        assert_eq!(s[1].harness, "pi", "the bound item shares the workspace");
        assert_eq!(s[2].harness, "claude", "not handed over yet");
        assert!(s[2].overrides.is_none());
        assert_eq!(s[3].harness, "pi", "two hops to the session that acts");
        assert_eq!(s[3].owner, "acme/widgets#1");
        let h = s[2].handover.as_ref().unwrap();
        assert_eq!(h.harness_name, "Codex");
        assert_eq!(h.summary_chars, Some(9));
        assert_eq!(
            h.describe(),
            "codex, asked by acme/widgets#3",
            "{}",
            h.describe()
        );
        let table = render_peers(&s, None);
        assert!(table.contains("handed over to pi"), "{table}");
        assert!(table.contains("model openai/gpt-6"), "{table}");
        assert!(
            table.contains("handover pending: codex, asked by acme/widgets#3"),
            "{table}"
        );
        assert!(
            table.contains("handover note waiting: from Claude Code, summary 13 chars"),
            "{table}"
        );
        let snap = Snapshot {
            cfg: cfg(),
            state: st,
            workspaces: Vec::new(),
            down: Vec::new(),
            errors: Vec::new(),
        };
        let v = snap.to_json();
        assert_eq!(v["sessions"][0]["overrides"]["harness"], "pi");
        assert!(v["sessions"][1]["overrides"]["harness"] == "pi");
        assert_eq!(v["sessions"][2]["handover"]["harness"], "codex");
        assert!(v["sessions"][2]["overrides"].is_null());
        let text = render_status(&snap);
        assert!(
            text.contains("handed over: harness=pi model=openai/gpt-6 effort=high"),
            "{text}"
        );
        assert!(text.contains("handover pending: codex"), "{text}");
        assert!(
            text.contains("handover note waiting: from Claude Code, summary 13 chars"),
            "{text}"
        );
        assert_eq!(v["sessions"][0]["handover_note"]["from"], "Claude Code");
        assert_eq!(v["sessions"][0]["handover_note"]["summary_chars"], 13);
        assert!(v["sessions"][2]["handover_note"].is_null());
    }

    #[test]
    fn ago_buckets() {
        let t = |secs: i64| {
            (chrono::Utc::now() - chrono::Duration::seconds(secs))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        };
        assert_eq!(ago(Some(&t(5))), "now");
        assert_eq!(ago(Some(&t(150))), "3m");
        assert_eq!(ago(Some(&t(7200))), "2h");
        assert_eq!(ago(Some(&t(3 * 86_400))), "3d");
        assert_eq!(ago(None), "");
        assert_eq!(ago(Some("garbage")), "");
    }
}
