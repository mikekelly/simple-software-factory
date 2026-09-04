//! The joined view behind `ssf status`, `ssf peers` and the bar widget: what
//! ssf knows about each item (issue or PR, GitHub state, triggers, prompts,
//! session id) next to what Orca reports about the workspace working on it
//! (agent state, last assistant message, current tool, last activity, board
//! column, branch). The widget reads this and never talks to Orca itself.

use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

use crate::config::{Config, RepoConfig};
use crate::github::PrInfo;
use crate::orca::{Orca, WorkspaceInfo};
use crate::state::{IssueState, State};

/// How long `ssf status` waits for Orca before reporting it unavailable; the
/// bar widget polls this, so it must never hang.
const ORCA_TIMEOUT: Duration = Duration::from_secs(8);

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
    pub harness: String,
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
    /// Orca's workspaces, or why they could not be listed.
    pub orca: Result<Vec<WorkspaceInfo>, String>,
}

impl Snapshot {
    pub async fn collect(cfg: Config) -> anyhow::Result<Self> {
        let state = State::load()?;
        let orca = Orca::new(cfg.orca.clone());
        let orca = match tokio::time::timeout(ORCA_TIMEOUT, orca.ps()).await {
            Ok(Ok(list)) => Ok(list),
            Ok(Err(e)) => Err(format!("{e:#}")),
            Err(_) => Err(format!(
                "orca worktree ps did not answer within {}s",
                ORCA_TIMEOUT.as_secs()
            )),
        };
        Ok(Self { cfg, state, orca })
    }

    pub fn sessions(&self) -> Vec<Session> {
        sessions(&self.cfg, &self.state, self.orca.as_deref().ok())
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
            "orca": {
                "available": self.orca.is_ok(),
                "error": self.orca.as_ref().err(),
                "workspaces": self.orca.as_ref().map(Vec::len).unwrap_or(0),
            },
            "sessions": sessions,
            "repos": repos,
        })
    }
}

/// Join every tracked item with its Orca workspace. `workspaces` is `None`
/// when Orca could not be asked.
pub fn sessions(cfg: &Config, state: &State, workspaces: Option<&[WorkspaceInfo]>) -> Vec<Session> {
    let mut out = Vec::new();
    for repo in &cfg.repos {
        let Some(rs) = state.repos.get(&repo.name) else {
            continue;
        };
        for item in rs.issues.values() {
            let ws = workspaces.and_then(|list| find_workspace(list, item));
            out.push(join(repo, item, ws, workspaces.is_some()));
        }
    }
    out
}

fn find_workspace<'a>(list: &'a [WorkspaceInfo], item: &IssueState) -> Option<&'a WorkspaceInfo> {
    if let Some(id) = &item.worktree_id {
        if let Some(w) = list.iter().find(|w| &w.worktree_id == id) {
            return Some(w);
        }
    }
    // The state file may be behind (or lost): fall back to Orca's own link.
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

fn join(
    repo: &RepoConfig,
    item: &IssueState,
    ws: Option<&WorkspaceInfo>,
    orca_available: bool,
) -> Session {
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
        harness: repo.harness.clone(),
        owner: if item.subscriber_only {
            String::new()
        } else {
            session_id(&repo.name, item.shares_workspace_of.unwrap_or(item.number))
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

fn one_line(text: &str, max: usize) -> String {
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
        if !s.subscribers.is_empty() {
            facts.push(format!("subscribers {}", s.subscribers.join(", ")));
        }
        if let Some(o) = &s.origin {
            facts.push(format!("opened by {o}"));
        }
        facts.push(format!("prompts {}", s.prompts_sent));
        if !s.active && !s.subscriber_only {
            facts.push("retired".into());
        }
        out.push_str(&format!("         {}\n", facts.join("  ·  ")));
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
    match &snap.orca {
        Ok(list) => out.push_str(&format!("orca:    {} workspaces\n", list.len())),
        Err(e) => out.push_str(&format!("orca:    unavailable ({e})\n")),
    }
    out.push_str(&format!(
        "config:  {}\n",
        crate::config::config_path().display()
    ));
    if snap.cfg.repos.is_empty() {
        out.push_str("\nno repositories configured\n");
    }
    let sessions = snap.sessions();
    for r in &snap.cfg.repos {
        out.push_str(&format!("\n{} (harness: {})\n", r.name, r.harness));
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
                out.push_str(&format!("          {p}\n"));
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
                    "          {} untagged post(s) by the bot (gh shim not in effect)\n",
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
        let snap = Snapshot {
            cfg: cfg(),
            state: state_with(vec![item(1, Some("r1::/w/one"))]),
            orca: Err("not running".into()),
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
