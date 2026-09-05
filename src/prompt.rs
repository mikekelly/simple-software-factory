//! Rendering GitHub issue timelines into prompts for the agent.

use serde_json::Value;
use std::path::Path;
use tracing::warn;

use crate::config::{DaemonConfig, DriverKind, RepoConfig};
use crate::github::{Issue, PrInfo, ProjectCard, value_str, value_u64};
use crate::origin;

/// A timeline event that should be shown to the agent.
#[derive(Debug, Clone)]
pub struct Rendered {
    pub key: String,
    pub text: String,
    /// For a post by the bot, the session its origin tag names
    /// (`owner/repo#N`, or `owner/repo#N:reviewer` for a reviewer session's
    /// post); `None` when the post carries no tag.
    pub origin: Option<String>,
    /// For a `labeled`/`unlabeled` event, the label's name.
    pub label: Option<String>,
}

impl Rendered {
    /// Whether this event asks the bot for a review of the item: a review
    /// request, or the review label being added.
    pub fn asks_review(&self, review_label: Option<&str>) -> bool {
        self.key.starts_with("review_requested:")
            || (self.key.starts_with("labeled:")
                && review_label.is_some_and(|l| {
                    self.label
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(l))
                }))
    }
}

/// Stable identity for a timeline event. Events without an id fall back to a
/// composite of type, timestamp and the most specific field available.
pub fn event_key(ev: &Value) -> Option<String> {
    let kind = value_str(ev, &["event"])?;
    if let Some(id) = value_u64(ev, &["id"]) {
        return Some(format!("{kind}:{id}"));
    }
    if let Some(node) = value_str(ev, &["node_id"]) {
        return Some(format!("{kind}:{node}"));
    }
    match kind {
        "committed" => value_str(ev, &["sha"]).map(|s| format!("committed:{s}")),
        "cross-referenced" => {
            let src = value_str(ev, &["source", "issue", "html_url"]).unwrap_or("");
            let at = value_str(ev, &["created_at"])
                .or(value_str(ev, &["updated_at"]))
                .unwrap_or("");
            Some(format!("cross-referenced:{src}:{at}"))
        }
        _ => {
            let at = value_str(ev, &["created_at"]).unwrap_or("");
            Some(format!("{kind}:{at}"))
        }
    }
}

pub fn actor_of(ev: &Value) -> String {
    value_str(ev, &["actor", "login"])
        .or_else(|| value_str(ev, &["user", "login"]))
        .or_else(|| value_str(ev, &["author", "name"]))
        .or_else(|| value_str(ev, &["source", "issue", "user", "login"]))
        .unwrap_or("unknown")
        .to_string()
}

fn when(ev: &Value) -> String {
    value_str(ev, &["created_at"])
        .or_else(|| value_str(ev, &["author", "date"]))
        .or_else(|| value_str(ev, &["updated_at"]))
        .unwrap_or("")
        .to_string()
}

/// A GitHub timestamp as the agent sees it: `2026-09-04 17:40Z`, or just
/// `17:40Z` when the date is `today` (`YYYY-MM-DD`; pass `""` to keep the
/// date always); order within a message stays clear either way. Anything
/// not in GitHub's ISO form is shown as is.
fn fmt_when(raw: &str, today: &str) -> String {
    let iso = raw
        .split_once('T')
        .filter(|(d, t)| d.len() == 10 && t.ends_with('Z'))
        .and_then(|(d, t)| t.get(..5).map(|hm| (d, hm)));
    match iso {
        Some((d, hm)) if d == today => format!("{hm}Z"),
        Some((d, hm)) => format!("{d} {hm}Z"),
        None => raw.to_string(),
    }
}

/// Today's date in UTC, spelled as GitHub timestamps spell it.
pub fn today_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn quote(body: &str, max: usize) -> String {
    let mut b: String = body.trim().to_string();
    if b.is_empty() {
        b = "(empty)".into();
    }
    if b.chars().count() > max {
        b = b.chars().take(max).collect::<String>() + "\n… (truncated)";
    }
    b.lines()
        .map(|l| format!("  > {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A post's text without its byline and origin tag, and where the tag
/// says it came from. Only the bot's own posts carry meaningful tags; a
/// human's text is shown as is. A post by the bot login without a tag was
/// typed by a person using the bot account (sessions always stamp), and is
/// marked so that the agent knows it is a human's.
fn body_and_session(body: &str, author: &str, bot: &str) -> (String, String) {
    if !author.eq_ignore_ascii_case(bot) {
        return (body.to_string(), String::new());
    }
    match origin::parse(body) {
        Some(t) if t.is_reviewer() => (
            origin::strip(body),
            format!(" (from the reviewer session on {})", t.origin),
        ),
        Some(t) => (
            origin::strip(body),
            format!(" (from the agent on {})", t.origin),
        ),
        None => (body.to_string(), " (not from a session)".to_string()),
    }
}

/// The session a post by the bot came from, per its origin tag.
fn post_origin(body: &str, author: &str, bot: &str) -> Option<String> {
    if !author.eq_ignore_ascii_case(bot) {
        return None;
    }
    origin::parse(body).map(|t| t.session())
}

/// Render one timeline event, or `None` if it is not worth showing. `bot` is
/// the bot login, whose posts carry origin tags.
pub fn render_event(ev: &Value, edited: bool, cfg: &DaemonConfig, bot: &str) -> Option<Rendered> {
    let kind = value_str(ev, &["event"])?.to_string();
    if cfg.ignored_events.iter().any(|k| k == &kind) {
        return None;
    }
    let key = event_key(ev)?;
    let actor = actor_of(ev);
    let today = today_utc();
    let at = fmt_when(&when(ev), &today);
    let head = |what: &str| format!("- {at} @{actor} {what}");
    let mut origin: Option<String> = None;
    let text = match kind.as_str() {
        "commented" => {
            let raw = value_str(ev, &["body"]).unwrap_or("");
            origin = post_origin(raw, &actor, bot);
            let (body, session) = body_and_session(raw, &actor, bot);
            let url = value_str(ev, &["html_url"]).unwrap_or("");
            let verb = if edited {
                "edited their comment"
            } else {
                "commented"
            };
            format!(
                "{}{session} ({url}):\n{}",
                head(verb),
                quote(&body, cfg.max_body_chars)
            )
        }
        "assigned" | "unassigned" => {
            let who = value_str(ev, &["assignee", "login"]).unwrap_or("someone");
            head(&format!("{kind} @{who}"))
        }
        "labeled" | "unlabeled" => {
            let label = value_str(ev, &["label", "name"]).unwrap_or("?");
            let verb = if kind == "labeled" {
                "added label"
            } else {
                "removed label"
            };
            head(&format!("{verb} \"{label}\""))
        }
        "renamed" => {
            let from = value_str(ev, &["rename", "from"]).unwrap_or("?");
            let to = value_str(ev, &["rename", "to"]).unwrap_or("?");
            head(&format!("renamed the issue from \"{from}\" to \"{to}\""))
        }
        "closed" => {
            let reason = value_str(ev, &["state_reason"]).unwrap_or("");
            if reason.is_empty() {
                head("closed the issue")
            } else {
                head(&format!("closed the issue ({reason})"))
            }
        }
        "reopened" => head("reopened the issue"),
        "milestoned" | "demilestoned" => {
            let m = value_str(ev, &["milestone", "title"]).unwrap_or("?");
            let verb = if kind == "milestoned" {
                "added to milestone"
            } else {
                "removed from milestone"
            };
            head(&format!("{verb} \"{m}\""))
        }
        "cross-referenced" => {
            let title = value_str(ev, &["source", "issue", "title"]).unwrap_or("");
            let url = value_str(ev, &["source", "issue", "html_url"]).unwrap_or("");
            let is_pr = ev.pointer("/source/issue/pull_request").is_some();
            let what = if is_pr { "pull request" } else { "issue" };
            head(&format!("referenced this from {what} \"{title}\" ({url})"))
        }
        "referenced" => {
            let sha = value_str(ev, &["commit_id"]).unwrap_or("");
            head(&format!("referenced this issue in commit {}", short(sha)))
        }
        "committed" => {
            let sha = value_str(ev, &["sha"]).unwrap_or("");
            let msg = value_str(ev, &["message"])
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("");
            format!("- {at} commit {} by {actor}: {msg}", short(sha))
        }
        "review_requested" => {
            let who = value_str(ev, &["requested_reviewer", "login"]).unwrap_or("someone");
            head(&format!("requested a review from @{who}"))
        }
        "reviewed" => {
            let state = value_str(ev, &["state"]).unwrap_or("reviewed");
            let raw = value_str(ev, &["body"]).unwrap_or("");
            origin = post_origin(raw, &actor, bot);
            let (body, session) = body_and_session(raw, &actor, bot);
            let mut s = head(&format!("reviewed ({state}){session}"));
            if !body.trim().is_empty() {
                s.push_str(":\n");
                s.push_str(&quote(&body, cfg.max_body_chars));
            }
            s
        }
        "line-commented" => {
            let comments = ev
                .get("comments")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut out = Vec::new();
            for c in &comments {
                let who = value_str(c, &["user", "login"]).unwrap_or("someone");
                let path = value_str(c, &["path"]).unwrap_or("?");
                let line = c
                    .get("line")
                    .or_else(|| c.get("original_line"))
                    .and_then(Value::as_u64);
                let raw = value_str(c, &["body"]).unwrap_or("");
                if origin.is_none() {
                    origin = post_origin(raw, who, bot);
                }
                let (body, session) = body_and_session(raw, who, bot);
                let url = value_str(c, &["html_url"]).unwrap_or("");
                let at = value_str(c, &["created_at"])
                    .map(|c| fmt_when(c, &today))
                    .unwrap_or_else(|| at.clone());
                out.push(format!(
                    "- {at} @{who} commented on `{path}`{}{session} ({url}):\n{}",
                    line.map(|l| format!(" line {l}")).unwrap_or_default(),
                    quote(&body, cfg.max_body_chars)
                ));
            }
            if out.is_empty() {
                return None;
            }
            out.join("\n")
        }
        "commit-commented" => {
            let comments = ev
                .get("comments")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut out = Vec::new();
            for c in &comments {
                let who = value_str(c, &["user", "login"]).unwrap_or("someone");
                let sha = value_str(c, &["commit_id"]).unwrap_or("");
                let raw = value_str(c, &["body"]).unwrap_or("");
                if origin.is_none() {
                    origin = post_origin(raw, who, bot);
                }
                let (body, session) = body_and_session(raw, who, bot);
                out.push(format!(
                    "- {at} @{who} commented on commit {}{session}:\n{}",
                    short(sha),
                    quote(&body, cfg.max_body_chars)
                ));
            }
            if out.is_empty() {
                return None;
            }
            out.join("\n")
        }
        "review_request_removed" => {
            let who = value_str(ev, &["requested_reviewer", "login"]).unwrap_or("someone");
            head(&format!("withdrew the review request for @{who}"))
        }
        "merged" => head("merged the pull request"),
        "head_ref_force_pushed" => head("force-pushed the pull request branch"),
        "head_ref_deleted" => head("deleted the pull request branch"),
        "ready_for_review" => head("marked the pull request ready for review"),
        "convert_to_draft" => head("converted the pull request to a draft"),
        "locked" => head("locked the issue"),
        "unlocked" => head("unlocked the issue"),
        "pinned" => head("pinned the issue"),
        "unpinned" => head("unpinned the issue"),
        "transferred" => head("transferred the issue"),
        "converted_to_discussion" => head("converted the issue to a discussion"),
        "connected" | "disconnected" => {
            let verb = if kind == "connected" {
                "linked"
            } else {
                "unlinked"
            };
            head(&format!("{verb} a pull request"))
        }
        other => head(&format!("{}", other.replace('_', " "))),
    };
    let label = matches!(kind.as_str(), "labeled" | "unlabeled")
        .then(|| value_str(ev, &["label", "name"]).unwrap_or("?").to_string());
    Some(Rendered {
        key,
        text,
        origin,
        label,
    })
}

#[derive(Clone)]
pub struct PromptContext<'a> {
    pub repo: &'a RepoConfig,
    pub daemon: &'a DaemonConfig,
    pub bot_login: &'a str,
    /// What runs the session (named in the first prompt).
    pub driver: DriverKind,
    /// Set when the item is a pull request.
    pub pr: Option<&'a PrInfo>,
    /// Why the bot is involved: assigned, mentioned, review_requested, created.
    pub triggers: &'a [String],
    /// The item belongs to that item's session (same repo): prompts about it
    /// go to that agent, whose own item this is not.
    pub owner: Option<u64>,
    /// Session (`owner/repo#N`) that opened the item as a hand-off.
    pub delegated_by: Option<&'a str>,
    /// Open project boards the item is on.
    pub projects: &'a [ProjectCard],
    /// The repository's own prompt file, when the worktree has one.
    pub project_prompt: Option<ProjectPrompt>,
}

/// Contents of the per-project prompt file (`SSF.md` by default): notes the
/// humans on a repository keep for ssf agents, outside CLAUDE.md/AGENTS.md.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectPrompt {
    /// The file as configured (`SSF.md`, `.ssf/prompt.md`, `~/notes/x.md`).
    pub source: String,
    pub text: String,
}

impl ProjectPrompt {
    /// Read the repository's prompt file from the checkout at `worktree`.
    /// A missing or empty file yields nothing; an unreadable one is logged.
    pub fn load(repo: &RepoConfig, worktree: &Path) -> Option<Self> {
        let path = repo.prompt_file_path(worktree);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                warn!(
                    repo = repo.name,
                    path = %path.display(),
                    "cannot read the project prompt file: {e}"
                );
                return None;
            }
        };
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        Some(Self {
            source: repo.prompt_file().to_string(),
            text: text.to_string(),
        })
    }
}

impl PromptContext<'_> {
    pub fn kind(&self) -> &'static str {
        if self.pr.is_some() {
            "pull request"
        } else {
            "issue"
        }
    }

    /// The reviewer session that handles review requests on this item: a
    /// pull request another session wrote gets one, so the session that
    /// wrote it never reviews its own work.
    pub fn reviewer_session(&self, issue: &Issue) -> Option<String> {
        if self.pr.is_some() && self.owner.is_some() {
            Some(crate::status::reviewer_session_id(
                &self.repo.name,
                issue.number,
            ))
        } else {
            None
        }
    }

    /// What asked the bot for this review, from a reviewer session's
    /// triggers: "a review was requested from @bot", "the `review` label
    /// was added", or both.
    pub fn review_asked(&self) -> String {
        let bot = self.bot_login;
        let mut parts = Vec::new();
        if self.triggers.iter().any(|t| t == "review_requested") {
            parts.push(format!("a review was requested from @{bot}"));
        }
        if self.triggers.iter().any(|t| t == "review_label") {
            parts.push(format!(
                "the `{}` label was added",
                self.daemon.review_label().unwrap_or("review")
            ));
        }
        if parts.is_empty() {
            parts.push(format!("a review was asked of @{bot}"));
        }
        parts.join(" and ")
    }

    /// Why the item reached ssf, as "it was assigned to @bot and mentioned
    /// @bot" (the tracked messages' subject is "it").
    fn because(&self) -> String {
        self.because_for("it")
    }

    /// Why the session was spawned, with the item as the subject: "#18 was
    /// assigned to @bot and mentioned @bot" (the header above it has the
    /// title and URL).
    fn spawned_because(&self, number: u64) -> String {
        self.because_for(&format!("#{number}"))
    }

    /// One trigger table for both phrasings: `subject` followed by what
    /// happened to it, the parts joined with "and".
    fn because_for(&self, subject: &str) -> String {
        let bot = self.bot_login;
        let parts: Vec<String> = self
            .triggers
            .iter()
            .map(|t| match t.as_str() {
                "assigned" => format!("was assigned to @{bot}"),
                "mentioned" => format!("mentioned @{bot}"),
                "review_requested" => format!("requested a review from @{bot}"),
                "review_label" => format!(
                    "was given the `{}` label, which asks @{bot} for a review",
                    self.daemon.review_label().unwrap_or("review")
                ),
                "created" => match self.delegated_by {
                    Some(parent) => format!(
                        "was opened by the agent session working on {parent} and handed off to you"
                    ),
                    None => format!("was opened by @{bot}"),
                },
                other => other.to_string(),
            })
            .collect();
        if parts.is_empty() {
            format!("{subject} was assigned to @{bot}")
        } else {
            format!("{subject} {}", parts.join(" and "))
        }
    }

    /// [`because`](Self::because) for an explicit list of triggers.
    fn because_of(&self, triggers: &[&str]) -> String {
        let owned: Vec<String> = triggers.iter().map(|t| t.to_string()).collect();
        let ctx = PromptContext {
            triggers: &owned,
            ..self.clone()
        };
        ctx.because()
    }

    /// How the item came to be routed to another session's agent.
    fn owned_because(&self, issue: &Issue) -> String {
        let by_bot = issue.author().eq_ignore_ascii_case(self.bot_login);
        let tagged = issue
            .body
            .as_deref()
            .and_then(origin::parse)
            .filter(|_| by_bot)
            .is_some_and(|t| Some(t.origin.number) == self.owner);
        if tagged {
            "this session opened it".to_string()
        } else if let Some(pr) = self.pr {
            format!("its branch `{}` is this workspace's branch", pr.head_ref)
        } else {
            "it belongs to this session".to_string()
        }
    }
}

fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// The item as a message names it once, in full: `#N "title" (url)`.
fn full_ref(issue: &Issue) -> String {
    format!("#{} \"{}\" ({})", issue.number, issue.title, issue.html_url)
}

/// The item as a later message names it: `#N` for the session's own item,
/// which it knows; `#N "title"` for an item bound to the session (a pull
/// request it opened), of which it may have several.
fn short_ref(issue: &Issue, ctx: &PromptContext) -> String {
    match ctx.owner {
        Some(_) => format!("#{} \"{}\"", issue.number, issue.title),
        None => format!("#{}", issue.number),
    }
}

/// The header of a first message: the item, named once with its URL, and
/// the facts about it that the rest of the message does not repeat.
fn issue_header(issue: &Issue, ctx: &PromptContext) -> String {
    let labels: Vec<&str> = issue.labels.iter().map(|l| l.name.as_str()).collect();
    // The header keeps the full date: it is the anchor for the day-less
    // activity lines under it.
    let opened = fmt_when(&issue.created_at, "");
    let by_bot = issue.author().eq_ignore_ascii_case(ctx.bot_login);
    let session = issue
        .body
        .as_deref()
        .and_then(origin::parse)
        .filter(|_| by_bot)
        .map(|t| format!(" (from the agent on {})", t.origin))
        .unwrap_or_default();
    let mut s = match ctx.pr {
        Some(pr) => format!(
            "# GitHub pull request #{}: {}\n{}\n\nBranch `{}` into `{}`{}{}. Opened by @{}{session} on {opened}.",
            issue.number,
            issue.title,
            issue.html_url,
            pr.head_ref,
            pr.base_ref,
            if pr.same_repo(&ctx.repo.name) {
                ""
            } else {
                " (from a fork)"
            },
            if pr.draft { ", draft" } else { "" },
            issue.author(),
        ),
        None => format!(
            "# GitHub issue #{}: {}\n{}\n\nOpened by @{}{session} on {opened}.",
            issue.number,
            issue.title,
            issue.html_url,
            issue.author(),
        ),
    };
    if !labels.is_empty() {
        s.push_str(&format!(" Labels: {}.", labels.join(", ")));
    }
    s
}

/// The boards the item is on: where the card is now and what it could be
/// set to. Which column fits is the agent's call, so nothing here says.
/// `keep` adds the one rule about cards (for the session that works on
/// the item; a reviewer leaves cards alone).
fn project_boards(ctx: &PromptContext, keep: bool) -> String {
    if ctx.projects.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n\n## Project boards\n\n");
    for card in ctx.projects {
        s.push_str(&format!("- {} ({}): ", card.title, card.url));
        match (&card.status, &card.status_field_id) {
            (Some(status), _) => s.push_str(&format!("Status is \"{status}\".")),
            (None, Some(_)) => s.push_str("Status is not set."),
            (None, None) => s.push_str("this board has no Status field."),
        }
        if let Some(field) = &card.status_field_id {
            if !card.status_options.is_empty() {
                let names: Vec<String> = card
                    .status_options
                    .iter()
                    .map(|o| format!("\"{}\"", o.name))
                    .collect();
                s.push_str(&format!(" Options: {}.", names.join(", ")));
            }
            s.push_str(&format!(
                "\n  Change it with `gh project item-edit --project-id {} --id {} --field-id {} \
--single-select-option-id <option id>`",
                card.project_id, card.item_id, field
            ));
            if card.status_options.is_empty() {
                s.push('.');
            } else {
                let ids: Vec<String> = card
                    .status_options
                    .iter()
                    .map(|o| format!("\"{}\" = {}", o.name, o.id))
                    .collect();
                s.push_str(&format!(", where {}.", ids.join(", ")));
            }
        }
        s.push('\n');
    }
    if keep {
        s.push_str("\nKeep the card's Status accurate; which column fits is your call.\n");
    }
    s.trim_end().to_string()
}

/// A message: `head`, then the events under a blank line (when there are
/// any), then `tail` under another; no stray blank lines when a part is
/// empty.
fn assemble(head: &str, events: &[Rendered], tail: &str) -> String {
    let mut s = head.to_string();
    if !events.is_empty() {
        s.push_str("\n\n");
        for e in events {
            s.push_str(&e.text);
            s.push('\n');
        }
    }
    if !tail.is_empty() {
        s.push_str(if events.is_empty() { "\n\n" } else { "\n" });
        s.push_str(tail);
    }
    s
}

pub fn initial_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let mut s = String::new();
    s.push_str(&issue_header(issue, ctx));
    s.push_str(&project_boards(ctx, true));
    s.push_str("\n\n## Description\n\n");
    let body = origin::strip(issue.body.as_deref().unwrap_or(""));
    let body = body.trim();
    s.push_str(if body.is_empty() {
        "(no description)"
    } else {
        body
    });
    s.push_str("\n\n## Activity so far\n\n");
    if events.is_empty() {
        s.push_str("(no activity yet)\n");
    } else {
        for e in events {
            s.push_str(&e.text);
            s.push('\n');
        }
    }
    s.push_str(&instructions(issue, ctx));
    s
}

fn instructions(issue: &Issue, ctx: &PromptContext) -> String {
    let n = issue.number;
    let repo = &ctx.repo.name;
    let bot = ctx.bot_login;
    let kind = ctx.kind();
    let multiplexer = match ctx.driver {
        DriverKind::Orca => "the Orca multiplexer",
        DriverKind::Herdr => "the herdr multiplexer",
    };
    let mut s = format!(
        "\n## How to work on this\n\n\
You are an automatically spawned coding agent for the GitHub account @{bot}. Simple Software \
Factory (ssf) spawned you, through {multiplexer}, in a worktree of this repository, \
because {}.\n\n\
New activity on it arrives here as messages prefixed `[ssf]`; act on them. `ssf guide` \
explains the rest.\n\n\
- This terminal is unmanned: nobody reads it, so everything you want a person to see goes on \
GitHub.\n\
- Collaborate with humans and other ssf-managed agents through GitHub comments on the {kind}.\n\
- Before starting on a goal, say on the {kind} what you are about to do, and say when you need a \
decision or have delivered: silent work leaves the {kind} looking unattended until it lands.\n\
- `gh` and `git push` already act as @{bot}, and the `gh` on your PATH marks your posts as this \
session's. Act only as @{bot}; never use another account, token or key you find on this \
machine.\n",
        ctx.spawned_because(n)
    );
    match ctx.pr {
        Some(pr) if pr.same_repo(repo) => s.push_str(&format!(
            "- This worktree is on the pull request's branch `{}`; pushes to it change the PR. \
Answer on it with `gh pr comment {n} --repo {repo}`, or `gh pr review {n} --repo {repo}` when a \
review was asked.\n",
            pr.head_ref
        )),
        Some(pr) => s.push_str(&format!(
            "- The pull request comes from a fork ({}), so this worktree cannot push to its \
branch; it is on a branch of its own{}. Answer on it with `gh pr comment {n} --repo {repo}`, \
or `gh pr review {n} --repo {repo}` when a review was asked.\n",
            pr.head_repo,
            match ctx.repo.base_branch.as_deref() {
                Some(base) => format!(" off `{base}`"),
                None => String::new(),
            }
        )),
        None => {}
    }
    if let Some(parent) = ctx.delegated_by {
        s.push_str(&format!(
            "- The session on {parent}, which handed this off, follows the {kind} as a subscriber \
(it sees the activity but does not act) and gets your final comment when the {kind} closes, so \
make that comment a clear summary of the outcome. To ask it something, comment on this {kind}.\n"
        ));
    }
    s.push_str(&extras(ctx, false));
    s
}

/// The operator's and the repository's own instructions, after ssf's. A
/// reviewer gets the same notes with one line saying what they are to it.
fn extras(ctx: &PromptContext, reviewer: bool) -> String {
    let mut s = String::new();
    if let Some(extra) = ctx.daemon.instructions.as_deref() {
        s.push('\n');
        s.push_str(extra.trim());
        s.push('\n');
    }
    if let Some(extra) = ctx.repo.instructions.as_deref() {
        s.push('\n');
        s.push_str(extra.trim());
        s.push('\n');
    }
    if let Some(pp) = ctx.project_prompt.as_ref() {
        s.push_str(&format!("\n## Project notes (`{}`)\n\n", pp.source));
        if reviewer {
            s.push_str(
                "(What you review against; the steps about delivering changes are the author's.)\n\n",
            );
        }
        s.push_str(&pp.text);
        s.push('\n');
    }
    s
}

pub fn followup_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let head = format!("[ssf] New activity on {}:", short_ref(issue, ctx));
    let tail = match ctx.reviewer_session(issue).filter(|_| {
        events
            .iter()
            .any(|e| e.asks_review(ctx.daemon.review_label()))
    }) {
        Some(r) => format!(
            "The review asked of @{} is not yours to do: a separate reviewer session ({r}) \
reviews what this session wrote, and its review arrives here as activity.",
            ctx.bot_login
        ),
        None => String::new(),
    };
    assemble(&head, events, &tail)
}

/// First message about an item that is routed to another session's agent
/// (the item's owner): the session that opened it, or whose branch it is.
pub fn tracked_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let kind = ctx.kind();
    let mut s = format!(
        "[ssf] Now tracking {kind} {} for this session, because {}.",
        full_ref(issue),
        ctx.owned_because(issue)
    );
    let human: Vec<&str> = ctx
        .triggers
        .iter()
        .filter(|t| t.as_str() != "created")
        .map(String::as_str)
        .collect();
    let labelled = ctx
        .daemon
        .review_label()
        .is_some_and(|l| issue.has_label(l));
    let reviewer = ctx
        .reviewer_session(issue)
        .filter(|_| human.contains(&"review_requested") || labelled);
    let human: Vec<&str> = human
        .into_iter()
        .filter(|t| reviewer.is_none() || *t != "review_requested")
        .collect();
    if !human.is_empty() {
        s.push_str(&format!(
            " It reached ssf because {}; that is for you to act on.",
            ctx.because_of(&human)
        ));
    }
    if let Some(r) = &reviewer {
        s.push_str(&format!(
            " A review was asked of @{}{}: not yours to do; a separate reviewer session ({r}) \
reviews it, and its review arrives here as activity.",
            ctx.bot_login,
            if labelled {
                format!(
                    " (the `{}` label)",
                    ctx.daemon.review_label().unwrap_or_default()
                )
            } else {
                String::new()
            }
        ));
    }
    s.push_str("\n\nActivity so far:\n\n");
    if events.is_empty() {
        s.push_str("(no activity yet)\n");
    }
    for e in events {
        s.push_str(&e.text);
        s.push('\n');
    }
    match ctx.pr {
        Some(pr) if pr.same_repo(&ctx.repo.name) => s.push_str(&format!(
            "\nAnswer on it with `gh pr comment {} --repo {}`; pushes to `{}` update it.",
            issue.number, ctx.repo.name, pr.head_ref
        )),
        Some(_) => s.push_str(&format!(
            "\nIt comes from a fork; answer on it with `gh pr comment {} --repo {}`.",
            issue.number, ctx.repo.name
        )),
        None => s.push_str(&format!(
            "\nAnswer on it with `gh issue comment {} --repo {}`.",
            issue.number, ctx.repo.name
        )),
    }
    s
}

pub fn closed_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let head = format!(
        "[ssf] {} has been closed ({}).",
        short_ref(issue, ctx),
        issue.state_reason.as_deref().unwrap_or("no reason given")
    );
    let tail = match ctx.owner {
        Some(owner) => {
            format!("No further updates for it; your own item, #{owner}, is unaffected.")
        }
        None => "Stop working on it: commit anything worth keeping, push, and leave a short final \
comment on it; then, only if everything is on origin, `ssf release` gives this workspace back \
(it refuses if anything would be lost; a kept workspace is fine). No further updates for it."
            .to_string(),
    };
    assemble(&head, events, &tail)
}

/// The daemon's own re-check, on the pass after `ssf release`, found work
/// in the workspace: what, and whether ssf will say so again.
pub fn release_refused_prompt(
    repo: &str,
    number: u64,
    problems: &[String],
    attempt: u32,
    max: u32,
) -> String {
    let mut s = format!(
        "[ssf] Release of this workspace refused ({attempt} of {max}): when the daemon came to \
remove it, the checks found work that is not on origin for {repo}#{number}:\n\n"
    );
    for p in problems {
        s.push_str(&format!("- {p}\n"));
    }
    s.push('\n');
    if attempt >= max {
        s.push_str(
            "ssf will not ask again: the workspace is kept for a person to look at (`ssf purge` \
or `ssf release --force` from a shell). Stop here.",
        );
    } else {
        s.push_str(
            "Commit and push what is worth keeping (or drop it) and run `ssf release` again, \
or leave the workspace as it is; a kept workspace is fine.",
        );
    }
    s
}

/// A comment on an item, as shown to the session that handed the item off.
pub struct FinalComment {
    pub author: String,
    pub session: Option<String>,
    pub url: String,
    pub body: String,
}

/// The one message a delegating session gets: the item it handed off has
/// closed, and this is the last word its agent left on it.
pub fn delegated_closed_prompt(
    issue: &Issue,
    merged: bool,
    last: Option<&FinalComment>,
    ctx: &PromptContext,
) -> String {
    let outcome = if merged {
        "merged".to_string()
    } else {
        format!(
            "closed ({})",
            issue.state_reason.as_deref().unwrap_or("no reason given")
        )
    };
    let mut s = format!(
        "[ssf] {}, the {} this session handed off, has been {outcome}.\n\n",
        full_ref(issue),
        ctx.kind(),
    );
    match last {
        Some(c) => {
            let from = match &c.session {
                Some(o) => format!("@{} (from the agent on {o})", c.author),
                None => format!("@{}", c.author),
            };
            s.push_str(&format!(
                "Final comment by {from} ({}):\n{}\n",
                c.url,
                quote(&c.body, ctx.daemon.max_body_chars)
            ));
        }
        None => s.push_str("It has no comments.\n"),
    }
    s.push_str("\nThis is the only message you will get about it.");
    s
}

/// What an FYI to a subscriber is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fyi {
    /// New events on the item.
    Activity,
    /// The item was closed (or merged).
    Closed,
    /// The bot is no longer involved with the item (its session retired).
    Unassigned,
    /// The item, until now only subscribed to, has been given a session.
    Tracked,
}

/// A message to a session that subscribed to an item it does not act on.
/// `owner` is the session acting on the item, named only when that is the
/// news (`Fyi::Tracked`); `merged` matters only for `Fyi::Closed`.
pub fn fyi_prompt(
    issue: &Issue,
    events: &[Rendered],
    ctx: &PromptContext,
    owner: Option<&str>,
    merged: bool,
    what: Fyi,
) -> String {
    let item = format!("{} {}", ctx.kind(), full_ref(issue));
    let head = match what {
        Fyi::Activity => format!("[ssf] FYI: new activity on {item}:"),
        Fyi::Closed => format!(
            "[ssf] FYI: {item} has been {}.",
            if merged {
                "merged".to_string()
            } else {
                format!(
                    "closed ({})",
                    issue.state_reason.as_deref().unwrap_or("no reason given")
                )
            }
        ),
        Fyi::Unassigned => format!(
            "[ssf] FYI: @{} is no longer involved with {item}, so its session has retired.",
            ctx.bot_login
        ),
        Fyi::Tracked => format!(
            "[ssf] FYI: {item} now has an agent session of its own ({}), because {}.",
            owner.unwrap_or("?"),
            ctx.because()
        ),
    };
    let tail = match what {
        Fyi::Closed | Fyi::Unassigned => {
            "For information only; you will not hear about it again unless it comes back."
                .to_string()
        }
        Fyi::Activity | Fyi::Tracked => format!(
            "For information only; `ssf unsub {}` stops these messages.",
            issue.number
        ),
    };
    assemble(&head, events, &tail)
}

/// A message another session (or a human shell) pasted in with `ssf tell`.
pub fn tell_prompt(
    from: Option<&str>,
    from_title: Option<&str>,
    text: &str,
    max_body_chars: usize,
) -> String {
    let who = match (from, from_title) {
        (Some(f), Some(t)) => format!("the agent session on {f} (\"{t}\")"),
        (Some(f), None) => format!("the agent session on {f}"),
        (None, _) => "a human at the terminal".to_string(),
    };
    let mut s = format!("[ssf] Message from {who}, sent with `ssf tell`:\n\n");
    s.push_str(&quote(text, max_body_chars));
    match from {
        Some(f) => {
            let n = f.rsplit_once('#').map(|(_, n)| n).unwrap_or(f);
            s.push_str(&format!(
                "\n\nIf it needs an answer, comment on {f}; `ssf tell {n} \"...\"` only for an \
operational nudge."
            ));
        }
        None => s.push_str("\n\nIt comes from outside GitHub, so answer here."),
    }
    s
}

/// A session the startup pass found interrupted: the item it works on and
/// where its workspace is.
pub struct Interrupted<'a> {
    pub number: u64,
    pub title: &'a str,
    pub url: &'a str,
    /// `refs/heads/...` or short, as recorded; shown short.
    pub branch: Option<&'a str>,
    pub path: Option<&'a str>,
    /// A reviewer session (a read-only checkout of a pull request).
    pub reviewer: bool,
}

/// The one message a session gets when the factory finds it interrupted at
/// startup: the machine (or Orca) restarted, its terminal is gone, and it
/// has just been started again. A resumed harness has its memory; a fresh
/// one gets the item's story ahead of this.
pub fn interrupted_prompt(it: &Interrupted) -> String {
    let item = format!("#{} \"{}\" ({})", it.number, it.title, it.url);
    let mut s = String::from(
        "[ssf] The factory restarted (the machine, the multiplexer or ssf itself) and this session was \
interrupted: its terminal was gone, so it has been started again.\n\n",
    );
    let branch = it
        .branch
        .map(|b| b.strip_prefix("refs/heads/").unwrap_or(b))
        .map(|b| format!(" on branch `{b}`"))
        .unwrap_or_default();
    let path = it.path.map(|p| format!(" in `{p}`")).unwrap_or_default();
    if it.reviewer {
        s.push_str(&format!(
            "This is the reviewer session for pull request {item}, a read-only checkout of \
it{path}.\n\nIf your review has not been posted yet, pick it up where you left off (`git log` \
shows what you were reviewing) and post it. If it has, nothing is needed until the next `[ssf]` \
message."
        ));
        return s;
    }
    s.push_str(&format!(
        "This is the session for {item}{branch}{path}.\n\nWork out where you got to (`git \
status`, `git log`, your last comments on the item) and carry on from there. Anything that \
happened on the item while you were away arrives as further `[ssf]` messages. If you were \
part-way through something and cannot tell what is left, say so on the item: that the session \
was interrupted and what remains."
    ));
    s
}

pub fn unassigned_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let bot = ctx.bot_login;
    let item = short_ref(issue, ctx);
    let head = if ctx.triggers.iter().any(|t| t == "review_requested")
        && !ctx.triggers.iter().any(|t| t == "assigned")
    {
        format!("[ssf] The review request for @{bot} on {item} has been fulfilled or withdrawn.")
    } else {
        format!("[ssf] @{bot} is no longer assigned to or requested on {item}.")
    };
    let tail = match ctx.owner {
        Some(owner) => format!(
            "No further updates for it unless it is brought back in; your own item, #{owner}, is \
unaffected."
        ),
        None => "Stop working on it: commit anything worth keeping, push, and leave a short final \
comment saying where things stand; then, only if everything is on origin, `ssf release` gives \
this workspace back (it refuses if anything would be lost). No further updates unless you are \
brought back in."
            .to_string(),
    };
    assemble(&head, events, &tail)
}

pub fn reassigned_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let mut s = format!(
        "[ssf] {} has been assigned to @{} again. Activity since then:\n\n",
        short_ref(issue, ctx),
        ctx.bot_login
    );
    if events.is_empty() {
        s.push_str("(no new activity)\n");
    }
    for e in events {
        s.push_str(&e.text);
        s.push('\n');
    }
    s.push_str("\nResume work on it.");
    s
}

/// The first message to a reviewer session: the pull request as its author
/// session would have been told it, plus review instructions instead of
/// working ones. `ctx.owner` is the session that wrote the PR.
pub fn review_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let mut s = String::new();
    s.push_str(&issue_header(issue, ctx));
    s.push_str(&project_boards(ctx, false));
    s.push_str("\n\n## Description\n\n");
    let body = origin::strip(issue.body.as_deref().unwrap_or(""));
    let body = body.trim();
    s.push_str(if body.is_empty() {
        "(no description)"
    } else {
        body
    });
    s.push_str("\n\n## Activity so far\n\n");
    if events.is_empty() {
        s.push_str("(no activity yet)\n");
    } else {
        for e in events {
            s.push_str(&e.text);
            s.push('\n');
        }
    }
    s.push_str(&review_instructions(issue, ctx));
    s
}

fn review_instructions(issue: &Issue, ctx: &PromptContext) -> String {
    let n = issue.number;
    let repo = &ctx.repo.name;
    let bot = ctx.bot_login;
    let author = match ctx.owner {
        Some(o) => crate::status::session_id(repo, o),
        None => format!("{repo}#{n}"),
    };
    let line = origin::Origin::new(repo, n)
        .map(|o| o.first_line(Some(repo), false, true))
        .unwrap_or_else(|| {
            format!(
                "{}#{n} (reviewer) <!-- ssf: origin={repo}#{n} role=reviewer -->",
                origin::ROBOT
            )
        });
    let (head, base) = match ctx.pr {
        Some(pr) => (pr.head_ref.clone(), pr.base_ref.clone()),
        None => ("the PR branch".to_string(), "the base branch".to_string()),
    };
    let asked = ctx.review_asked();
    let fulfilled = match ctx.daemon.review_label() {
        Some(l) => format!(
            "ssf removes the `{l}` label, or GitHub drops the review request; leave the label \
alone yourself"
        ),
        None => "GitHub drops the review request".to_string(),
    };
    let mut s = format!(
        "\n## How to review this\n\n\
You are a reviewer for the GitHub account @{bot}. Simple Software Factory (ssf) started this \
session because {asked} on #{n}, which another session of the same bot ({author}) wrote and so \
must not review. Your job is the review, nothing else: you never change the pull request.\n\n\
- This worktree is a read-only checkout of the pull request's head (`origin/{head}`, against \
`{base}`), on a local branch of its own. Do not commit, push, merge or edit the PR, and do not \
change its board cards. `git fetch origin && git diff origin/{base}...origin/{head}` (or \
`gh pr diff {n} --repo {repo}`) shows the whole change; `git fetch origin && git reset --hard \
origin/{head}` brings the checkout up to date after the author pushes. Building and running \
tests here is fine.\n\
- Post the review with `gh pr review {n} --repo {repo} --approve|--request-changes|--comment \
--body \"...\"` (inline comments through `gh api` if useful). One review per request: once it \
is posted the request is fulfilled ({fulfilled}) and this session pauses until a review is \
asked again, when a message here says what happened since.\n\
- `gh` already acts as @{bot}; `SSF_ROLE` is `reviewer`. Act only as @{bot}; never use another \
account, token or key you find on this machine.\n\
- Every review and comment you post must start with the line `{line}`, then a blank line: it \
tells readers and ssf that it came from the reviewer session, not the author's. The `gh` on \
your PATH adds it when you pass `--body` or `--body-file` to `pr review` or `pr comment`; add \
it yourself when you post any other way (`gh api`, ...).\n\
- The author's session gets your review as activity and answers on the pull request; its \
replies reach you here, marked \"from the agent on {author}\". To speak to it, comment on the \
pull request. `ssf guide` explains the rest.\n"
    );
    s.push_str(&extras(ctx, true));
    s
}

/// New activity on a pull request under review, for its reviewer session.
pub fn review_followup_prompt(issue: &Issue, events: &[Rendered], _ctx: &PromptContext) -> String {
    let head = format!("[ssf] New activity on #{}:", issue.number);
    assemble(&head, events, "")
}

/// The review was requested again on a pull request this reviewer session
/// already looked at.
pub fn review_again_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let head = ctx
        .pr
        .map(|p| p.head_ref.clone())
        .unwrap_or_else(|| "<branch>".into());
    let mut s = format!(
        "[ssf] Another review of #{} is asked: {}. Activity since your last look:\n\n",
        issue.number,
        ctx.review_asked()
    );
    if events.is_empty() {
        s.push_str("(no new activity)\n");
    }
    for e in events {
        s.push_str(&e.text);
        s.push('\n');
    }
    s.push_str(&format!(
        "\nBring the checkout up to date (`git fetch origin && git reset --hard origin/{head}`), \
review what changed since your last look, and post it with `gh pr review {} --repo {}`.",
        issue.number, ctx.repo.name
    ));
    s
}

/// Why a reviewer session is being stood down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewEnd {
    /// The review request is gone: the review was posted, or it was withdrawn.
    Fulfilled,
    /// The pull request was closed or merged.
    Closed { merged: bool },
}

/// Tell a reviewer session its review is no longer wanted for now.
pub fn review_done_prompt(
    issue: &Issue,
    events: &[Rendered],
    ctx: &PromptContext,
    why: ReviewEnd,
) -> String {
    let head = match why {
        ReviewEnd::Fulfilled => format!(
            "[ssf] The review asked of @{} on #{} has been posted, or the request withdrawn.",
            ctx.bot_login, issue.number
        ),
        ReviewEnd::Closed { merged } => format!(
            "[ssf] #{} has been {}.",
            issue.number,
            if merged { "merged" } else { "closed" }
        ),
    };
    let tail = match why {
        ReviewEnd::Fulfilled => {
            "Stop here; post nothing more. If a review is asked again you will be told here, \
with what happened in between."
        }
        ReviewEnd::Closed { .. } => "This review session is over: stop, and post nothing more.",
    };
    assemble(&head, events, tail)
}

/// The reference an agent pulls on demand with `ssf guide`: how sessions,
/// other sessions, following items, hand-offs and reviewer sessions work.
/// The initial prompt points here and carries only what an agent needs in
/// order to act at all; printed by the binary so it cannot drift from it.
pub fn guide(bot: &str, review_label: Option<&str>) -> String {
    let ask_again = match review_label {
        Some(l) => format!(
            "add the `{l}` label again (`gh pr edit <n> --add-label {l}`): GitHub refuses a review \
request from a pull request's own author, and ssf clears the label once the review is posted"
        ),
        None => format!("request the review again (`gh pr edit <n> --add-reviewer {bot}`)"),
    };
    let how_asked = match review_label {
        Some(l) => format!("the `{l}` label, or a review request"),
        None => "a review request".to_string(),
    };
    format!(
        "# ssf guide\n\n\
Simple Software Factory (ssf) runs one agent session per GitHub issue or pull request that \
involves the bot account @{bot}. Each session has a workspace (a git worktree of the \
repository) and a terminal, and receives the item's activity as messages prefixed `[ssf]`. \
`SSF_REPO` and `SSF_ISSUE` name the session's item; `SSF_BOT` is the bot's login; `SSF_ROLE` \
is `reviewer` in a reviewer session. This guide is the reference behind the initial prompt.\n\n\
## Messages you receive\n\n\
- `[ssf] New activity on ...`: comments, reviews, label changes, renames, linked PRs and the \
like on your item. Your own posts are never echoed back.\n\
- `[ssf] Now tracking ...`: an item you opened, or a pull request on your branch, has been bound \
to this session; its activity comes here from now on.\n\
- `[ssf] FYI: ...`: activity on an item you follow but do not work on. For information only.\n\
- `[ssf] Message from ...`: a message pasted into this terminal with `ssf tell` (below).\n\
- `[ssf] ... has been closed`, `... no longer assigned`, `... assigned ... again`: your item's \
lifecycle; each says what to do.\n\
- `[ssf] The factory restarted ...`: the machine, the multiplexer or ssf restarted and this session was \
started again.\n\n\
## Other sessions\n\n\
`ssf peers` lists the agent sessions on this repository: item, GitHub state, agent state, \
branch, last message (`--json` for detail, `--all` to include retired ones).\n\n\
To speak to the agent on another item, comment on that item with `gh`: it reaches that session \
labelled as coming from you (\"from the agent on owner/repo#M\"), and stays on the item where \
anyone can find it later. Comments from other sessions on your items arrive the same way. \
Decisions, questions that change scope, status and anything someone might need to look up go \
on the item.\n\n\
`ssf tell <n> \"message\"` (or `ssf tell owner/repo#n \"...\"`; `<n>:reviewer` for a pull \
request's reviewer session) pastes a message straight into that session's terminal instead. It \
is not mirrored to GitHub, so it is the exception: for operational nudges that would be noise \
on the item (\"master moved, rebase\", \"terminal is being replaced\") and for reaching a session \
whose item is already closed.\n\n\
## Following items\n\n\
`ssf sub <n>` (or `ssf sub owner/repo#n`) follows an item without working on it: its activity \
then arrives here as `[ssf] FYI` messages. `ssf unsub <n>` stops them; `ssf subs` lists what \
this session follows and who follows its items.\n\n\
## Items you open, and hand-offs\n\n\
Issues and pull requests you open stay with you: ssf recognises the origin tag on them and \
delivers their activity (comments, reviews, review requests, assignments, closure) here \
instead of starting another session; `SSF_ISSUE` does not change. A pull request opened on \
this workspace's branch is yours too, tag or no tag. Referencing the item in a pull request's \
body (`Closes #N`) links the two on GitHub, which then closes the issue when the pull request \
is merged; the repository's own notes say how it wants pull requests.\n\n\
To hand a piece of work to a separate agent instead, create the issue (or pull request) with \
`--assignee {bot}` in the same `gh ... create` command: the tag then carries `mode=delegate` \
and the item gets a session of its own. You are subscribed to it automatically, so its \
activity comes to you as FYI messages, and when it closes you get one message with its final \
comment (the last comment the bot left on it). Assigning @{bot} to an existing item you did \
not open gives it a fresh session too. A session that was handed an item this way is told so, \
and its final comment on the item is all the delegating session gets, so it should sum up the \
outcome.\n\n\
## Reviews of your own pull requests\n\n\
A review asked of @{bot} on a pull request you opened ({how_asked}) is not for you to do: ssf \
starts a separate reviewer session for it (a read-only checkout of the pull request with its \
own agent, `owner/repo#P:reviewer` in `ssf peers`), and its review arrives here as activity, \
marked \"from the reviewer session on owner/repo#P\". Answer it and push fixes as you would \
for a human reviewer; when you want another look, {ask_again}.\n\n\
## Wrapping up\n\n\
When your item closes, or you are no longer assigned, ssf says so and leaves the workspace \
exactly as it is: nothing on disk is ever removed on that signal. Commit what is worth \
keeping, push, leave a final comment, and then, only if everything is on origin, run \
`ssf release`: the daemon checks that the tree is clean, the branch is on origin with no \
unpushed commits and no stash was made on it, and removes the workspace (with this terminal) \
on its next pass. If anything would be lost it says what and refuses; leave the workspace \
then, a kept workspace costs nothing, and a person cleans up with `ssf purge`. If the daemon's \
own re-check on that pass finds work instead, you get one `[ssf] Release ... refused` message \
naming it; after three such refusals ssf stops asking and keeps the workspace for a person. A \
released workspace is re-created from its branch if the item comes back to life.\n\n\
## The byline and origin tag\n\n\
GitHub shows the same bot for every session, so every comment, review and pull request a \
session posts starts with one line that is both a byline for people and a tag for ssf: \
`🤖#N says: <!-- ssf: origin=owner/repo#N -->` (`🤖owner/repo#N says:` when the post is on another \
repository; `🤖#N (reviewer) says:` and `role=reviewer` from a reviewer session; `mode=delegate` on \
a hand-off), then a blank line. GitHub links the byline to the session's item. The `gh` on the \
session's PATH adds the line when `--body` or `--body-file` is passed to `issue create|comment` \
or `pr create|comment|review`; any other way of posting (`gh api`, `gh pr create --fill`, \
`gh pr edit --body`, ...) needs it added by hand, as the first line of the body. A tag \
anywhere else, in a code block or a quote, is content and is ignored. A post by \
@{bot} without the line was typed by a person using the bot account; it reaches you marked \
\"(not from a session)\" and is a human's.\n"
    )
}

/// Short, filesystem-safe name for the worktree.
pub fn worktree_name_for(number: u64, title: &str, is_pr: bool) -> String {
    let base = worktree_name(number, title);
    if is_pr {
        base.replacen("issue-", "pr-", 1)
    } else {
        base
    }
}

/// Name for the reviewer session's worktree on pull request `number`.
pub fn review_worktree_name(number: u64, title: &str) -> String {
    worktree_name(number, title).replacen("issue-", "review-", 1)
}

pub fn worktree_name(number: u64, title: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in title.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        format!("issue-{number}")
    } else {
        format!("issue-{number}-{slug}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_prompt_names_the_driver() {
        let issue: Issue = serde_json::from_value(serde_json::json!({
            "number": 3, "title": "T", "html_url": "https://x/3", "body": "", "state": "open",
            "user": {"login": "h"}, "labels": [], "assignees": [],
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap();
        let repo = RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            ..Default::default()
        };
        let d = DaemonConfig::default();
        let triggers = vec!["assigned".to_string()];
        let mut ctx = PromptContext {
            repo: &repo,
            daemon: &d,
            bot_login: "bot",
            driver: DriverKind::Herdr,
            pr: None,
            triggers: &triggers,
            owner: None,
            delegated_by: None,
            projects: &[],
            project_prompt: None,
        };
        assert!(instructions(&issue, &ctx).contains("through the herdr multiplexer"));
        ctx.driver = DriverKind::Orca;
        assert!(instructions(&issue, &ctx).contains("through the Orca multiplexer"));
    }
    use serde_json::json;

    fn cfg() -> DaemonConfig {
        DaemonConfig::default()
    }

    #[test]
    fn keys_prefer_ids_and_fall_back_sensibly() {
        assert_eq!(
            event_key(&json!({"event":"commented","id":42})).unwrap(),
            "commented:42"
        );
        assert_eq!(
            event_key(&json!({"event":"committed","sha":"abc"})).unwrap(),
            "committed:abc"
        );
        assert_eq!(
            event_key(&json!({"event":"cross-referenced","created_at":"t","source":{"issue":{"html_url":"u"}}})).unwrap(),
            "cross-referenced:u:t"
        );
        assert!(event_key(&json!({"id": 1})).is_none());
    }

    #[test]
    fn renders_comment_with_quote_and_ignores_noise() {
        let ev = json!({"event":"commented","id":1,"user":{"login":"alice"},"created_at":"2026-01-01T00:00:00Z",
            "updated_at":"2026-01-01T00:00:00Z","body":"hello\nworld","html_url":"https://x/1"});
        let r = render_event(&ev, false, &cfg(), "bot").unwrap();
        assert!(r.text.contains("@alice commented"));
        assert!(r.text.contains("  > hello\n  > world"));
        let edited = render_event(&ev, true, &cfg(), "bot").unwrap();
        assert!(edited.text.contains("edited their comment"));
        let noise = json!({"event":"subscribed","id":2,"actor":{"login":"bob"}});
        assert!(render_event(&noise, false, &cfg(), "bot").is_none());
    }

    #[test]
    fn timestamps_are_short_and_drop_todays_date() {
        assert_eq!(
            fmt_when("2026-09-04T17:40:02Z", "2026-09-05"),
            "2026-09-04 17:40Z"
        );
        assert_eq!(fmt_when("2026-09-05T09:03:59Z", "2026-09-05"), "09:03Z");
        assert_eq!(fmt_when("t", "2026-09-05"), "t");
        assert_eq!(fmt_when("", "2026-09-05"), "");
        assert_eq!(fmt_when("2026-09-05T09", "2026-09-05"), "2026-09-05T09");
        assert_eq!(today_utc().len(), 10);
        let ev = json!({"event":"assigned","id":7,"actor":{"login":"carol"},"assignee":{"login":"bot"},
            "created_at":"2026-01-02T03:04:05Z"});
        let r = render_event(&ev, false, &cfg(), "bot").unwrap();
        assert_eq!(r.text, "- 2026-01-02 03:04Z @carol assigned @bot");
        let inline = json!({"event":"line-commented","comments":[{"id":8,"user":{"login":"alice"},"path":"a.rs",
            "line":3,"body":"typo","html_url":"u8","created_at":"2026-01-02T03:04:05Z"}]});
        let r = render_event(&inline, false, &cfg(), "bot").unwrap();
        assert!(
            r.text
                .starts_with("- 2026-01-02 03:04Z @alice commented on `a.rs` line 3 (u8):"),
            "{}",
            r.text
        );
    }

    #[test]
    fn truncates_long_bodies() {
        let mut c = cfg();
        c.max_body_chars = 5;
        let ev = json!({"event":"commented","id":1,"user":{"login":"a"},"body":"0123456789"});
        let r = render_event(&ev, false, &c, "bot").unwrap();
        assert!(r.text.contains("01234"));
        assert!(r.text.contains("truncated"));
        assert!(!r.text.contains("56789"));
    }

    #[test]
    fn worktree_names_are_slugged_and_bounded() {
        assert_eq!(
            worktree_name(7, "Fix the Login Bug!"),
            "issue-7-fix-the-login-bug"
        );
        assert_eq!(worktree_name(8, "   "), "issue-8");
        let long = worktree_name(9, &"a".repeat(200));
        assert!(long.len() <= "issue-9-".len() + 40);
    }

    #[test]
    fn initial_prompt_mentions_bot_and_issue() {
        let issue: Issue = serde_json::from_value(json!({
            "number": 3, "title": "Add thing", "body": "Please add", "html_url": "https://gh/3", "state": "open",
            "user": {"login": "carol"}, "assignees": [{"login":"bot"}], "labels": [{"name":"feature"}],
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
        })).unwrap();
        let repo = RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            driver: None,
            model: None,
            effort: None,
            command: None,
            clone_url: None,
            path: None,
            base_branch: None,
            instructions: Some("Run the tests.".into()),
            prompt_file: None,
        };
        let d = cfg();
        let ctx = PromptContext {
            repo: &repo,
            daemon: &d,
            bot_login: "bot",
            driver: DriverKind::Orca,
            pr: None,
            triggers: &[],
            owner: None,
            delegated_by: None,
            projects: &[],
            project_prompt: None,
        };
        let p = initial_prompt(&issue, &[], &ctx);
        assert!(p.starts_with(
            "# GitHub issue #3: Add thing\nhttps://gh/3\n\nOpened by @carol on 2026-01-01 00:00Z. Labels: feature.\n"
        ), "{p}");
        // The item is named once: the header has the URL, the reason has `#3`.
        assert_eq!(p.matches("https://gh/3").count(), 1);
        assert!(!p.contains("o/r#3"));
        assert!(p.contains("(no activity yet)"));
        assert!(
            p.contains(
                "## How to work on this\n\nYou are an automatically spawned coding agent for the \
GitHub account @bot. Simple Software Factory (ssf) spawned you, through the Orca multiplexer, \
in a worktree of this repository, because #3 was assigned to @bot.\n\n\
New activity on it arrives here as messages prefixed `[ssf]`; act on them. `ssf guide` \
explains the rest.\n\n\
- This terminal is unmanned: nobody reads it, so everything you want a person to see goes on \
GitHub.\n\
- Collaborate with humans and other ssf-managed agents through GitHub comments on the issue.\n\
- Before starting on a goal, say on the issue what you are about to do, and say when you need a \
decision or have delivered: silent work leaves the issue looking unattended until it lands.\n\
- `gh` and `git push` already act as @bot, and the `gh` on your PATH marks your posts as this \
session's. Act only as @bot; never use another account, token or key you find on this \
machine.\n"
            ),
            "{p}"
        );
        // Branches and worktrees are the agent's own business, the byline's
        // mechanics are the guide's, and the PR conventions (reference the
        // issue, do not close it, do not merge) are the repository's.
        for dropped in [
            "Closes #3",
            "on its branch",
            "not yours to touch",
            "must start with the line",
            "<!-- ssf: origin=",
            "Do not close",
            "Do not merge",
            "open a pull request",
            "GH_TOKEN",
            "SSF_ISSUE",
        ] {
            assert!(
                !p.contains(dropped),
                "{dropped} is no longer the prompt's to say:\n{p}"
            );
        }
        // The reference lives behind `ssf guide`; the prompt only points at it.
        assert!(p.contains("`ssf guide` explains the rest"));
        for moved in [
            "ssf peers",
            "ssf sub",
            "ssf tell",
            "--assignee",
            "reviewer session",
            "mode=delegate",
        ] {
            assert!(
                !p.contains(moved),
                "{moved} belongs in the guide, not the prompt"
            );
        }
        // Advice about how to work is the repository's to give (SSF.md).
        for advice in [
            "commit as you go",
            "Post a short comment",
            "rather than guessing",
            "careful colleague",
        ] {
            assert!(!p.contains(advice), "{advice} is advice, not a rule");
        }
        assert!(!p.contains("handed off to you"));
        assert!(p.trim_end().ends_with("Run the tests."));

        let ctx = PromptContext {
            project_prompt: Some(ProjectPrompt {
                source: "SSF.md".into(),
                text: "Cards go to Review when a PR is open.".into(),
            }),
            ..ctx
        };
        let p = initial_prompt(&issue, &[], &ctx);
        assert!(p.contains("Run the tests.\n\n## Project notes (`SSF.md`)\n\nCards go to Review"));
        assert!(!p.contains("They say"));
        let triggers = vec!["assigned".to_string(), "created".to_string()];
        let child = PromptContext {
            triggers: &triggers,
            delegated_by: Some("o/r#1"),
            ..ctx
        };
        let p = initial_prompt(&issue, &[], &child);
        assert!(p.contains(
            "because #3 was assigned to @bot and was opened by the agent session working on o/r#1 and handed off to you."
        ));
        // The bullet does not say "handed off" again; the reason did.
        assert!(p.contains(
            "- The session on o/r#1, which handed this off, follows the issue as a subscriber"
        ));
        assert_eq!(p.matches("handed").count(), 2, "{p}");
        assert!(p.contains("To ask it something, comment on this issue."));
    }

    #[test]
    fn initial_prompt_is_the_bare_minimum() {
        use crate::github::StatusOption;
        let issue: Issue = serde_json::from_value(json!({
            "number": 24, "title": "Prompts: bare functional minimum", "body": "Cut the prompts down.",
            "html_url": "https://github.com/o/r/issues/24", "state": "open",
            "user": {"login": "carol"}, "created_at": "2026-09-04T20:03:47Z", "updated_at": "t"
        }))
        .unwrap();
        let repo = RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            ..Default::default()
        };
        let d = cfg();
        let boards = vec![ProjectCard {
            project_id: "PVT_kwHN2ebOAYlXgQ".into(),
            title: "Simple Software Factory".into(),
            url: "https://github.com/users/o/projects/5".into(),
            item_id: "PVTI_lAHN2ebOAYlXgc4OYASe".into(),
            status: Some("In Progress".into()),
            status_field_id: Some("PVTSSF_lAHN2ebOAYlXgc4YTeZE".into()),
            status_options: ["Longrunners", "Todo", "In Progress", "Done"]
                .iter()
                .map(|n| StatusOption {
                    id: "6ea12c8d".into(),
                    name: n.to_string(),
                })
                .collect(),
        }];
        let triggers = vec!["assigned".to_string()];
        let ctx = PromptContext {
            repo: &repo,
            daemon: &d,
            bot_login: "OverlayBot",
            driver: DriverKind::Orca,
            pr: None,
            triggers: &triggers,
            owner: None,
            delegated_by: None,
            projects: &boards,
            project_prompt: None,
        };
        let ev = Rendered {
            key: "k".into(),
            text: "- [2026-09-04T20:45:16Z] @OverlayBot assigned @OverlayBot".into(),
            origin: None,
            label: None,
        };
        let p = initial_prompt(&issue, &[ev], &ctx);
        assert!(
            p.chars().count() < 2500,
            "initial prompt is {} chars:\n{p}",
            p.chars().count()
        );
        // The board rule sits with the boards, not among the instructions.
        let boards = &p[p.find("## Project boards").unwrap()..p.find("## Description").unwrap()];
        assert!(
            boards.contains("Keep the card's Status accurate; which column fits is your call.")
        );
        let how = &p[p.find("## How to work on this").unwrap()..];
        assert!(!how.contains("card"));
    }

    #[test]
    fn guide_holds_the_moved_reference() {
        let g = guide("bot", Some("review"));
        assert!(g.starts_with("# ssf guide\n\n"));
        assert!(g.contains("`ssf peers` lists the agent sessions"));
        assert!(!g.contains("Leave their branches and workspaces alone"));
        assert!(g.contains("Referencing the item in a pull request's body (`Closes #N`)"));
        assert!(g.contains("needs it added by hand, as the first line of the body"));
        assert!(
            g.contains("To speak to the agent on another item, comment on that item with `gh`")
        );
        assert!(g.contains("`ssf tell <n> \"message\"`"));
        assert!(g.contains("\"master moved, rebase\", \"terminal is being replaced\""));
        assert!(g.contains("a session whose item is already closed"));
        assert!(g.contains("`ssf sub <n>`"));
        assert!(g.contains("`ssf unsub <n>` stops them; `ssf subs` lists"));
        assert!(g.contains("from the agent on owner/repo#M"));
        assert!(g.contains("`--assignee bot` in the same `gh ... create` command"));
        assert!(g.contains("You are subscribed to it automatically"));
        assert!(g.contains("ssf starts a separate reviewer session for it"));
        assert!(g.contains("(the `review` label, or a review request)"));
        assert!(g.contains("add the `review` label again (`gh pr edit <n> --add-label review`)"));
        assert!(!g.contains("--add-reviewer"));
        assert!(g.contains("<!-- ssf: origin=owner/repo#N -->"));
        let plain = guide("bot", None);
        assert!(plain.contains("(a review request)"));
        assert!(plain.contains("request the review again (`gh pr edit <n> --add-reviewer bot`)"));
        assert!(!plain.contains("`review` label"));
    }

    #[test]
    fn fyi_and_tell_prompts() {
        let issue: Issue = serde_json::from_value(json!({
            "number": 5, "title": "Thing", "body": null, "html_url": "https://gh/5", "state": "closed",
            "state_reason": "completed", "user": {"login": "carol"}, "created_at": "t", "updated_at": "t"
        }))
        .unwrap();
        let repo = RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            ..Default::default()
        };
        let d = cfg();
        let triggers = vec!["assigned".to_string()];
        let ctx = PromptContext {
            repo: &repo,
            daemon: &d,
            bot_login: "bot",
            driver: DriverKind::Orca,
            pr: None,
            triggers: &triggers,
            owner: None,
            delegated_by: None,
            projects: &[],
            project_prompt: None,
        };
        let ev = Rendered {
            key: "k".into(),
            text: "- [t] @alice commented (u):\n  > hi".into(),
            origin: None,
            label: None,
        };
        let p = fyi_prompt(
            &issue,
            &[ev.clone()],
            &ctx,
            Some("o/r#5"),
            false,
            Fyi::Activity,
        );
        // The item is named once, in the header; the owner is not.
        assert!(p.starts_with(
            "[ssf] FYI: new activity on issue #5 \"Thing\" (https://gh/5):\n\n- [t] @alice"
        ));
        assert!(!p.contains("owned by"));
        // One line of boilerplate after the activity, no more.
        assert!(p.ends_with("  > hi\n\nFor information only; `ssf unsub 5` stops these messages."));
        assert!(!p.contains("again unless"));
        let p = fyi_prompt(&issue, &[], &ctx, None, false, Fyi::Closed);
        assert_eq!(
            p,
            "[ssf] FYI: issue #5 \"Thing\" (https://gh/5) has been closed (completed).\n\n\
For information only; you will not hear about it again unless it comes back."
        );
        let p = fyi_prompt(&issue, &[], &ctx, Some("o/r#5"), true, Fyi::Closed);
        assert!(p.contains("(https://gh/5) has been merged."));
        let p = fyi_prompt(&issue, &[ev], &ctx, Some("o/r#5"), false, Fyi::Tracked);
        assert!(p.contains(
            "[ssf] FYI: issue #5 \"Thing\" (https://gh/5) now has an agent session of its own (o/r#5), because it was assigned to @bot."
        ));
        let p = fyi_prompt(&issue, &[], &ctx, Some("o/r#5"), false, Fyi::Unassigned);
        assert!(p.starts_with(
            "[ssf] FYI: @bot is no longer involved with issue #5 \"Thing\" (https://gh/5), so its session has retired."
        ));

        let t = tell_prompt(Some("o/r#3"), Some("Fix it"), "are you done?", 100);
        assert!(t.starts_with(
            "[ssf] Message from the agent session on o/r#3 (\"Fix it\"), sent with `ssf tell`:\n\n  > are you done?"
        ));
        assert!(t.ends_with(
            "  > are you done?\n\nIf it needs an answer, comment on o/r#3; `ssf tell 3 \"...\"` only for an operational nudge."
        ));
        let t = tell_prompt(None, None, "hello", 100);
        assert!(t.starts_with("[ssf] Message from a human at the terminal, sent with `ssf tell`:"));
        assert!(t.ends_with("  > hello\n\nIt comes from outside GitHub, so answer here."));
    }

    #[test]
    fn owned_items_get_tracking_and_closing_notes() {
        let pr_issue: Issue = serde_json::from_value(json!({
            "number": 4, "title": "Fix it", "body": "<!-- ssf: origin=o/r#3 -->\n\nFixes it", "html_url": "https://gh/4",
            "state": "open", "state_reason": "completed", "user": {"login": "bot"}, "created_at": "t", "updated_at": "t",
            "pull_request": {}
        })).unwrap();
        let repo = RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            ..Default::default()
        };
        let d = cfg();
        let pr = PrInfo {
            head_ref: "bot/fix".into(),
            head_repo: "o/r".into(),
            base_ref: "main".into(),
            ..Default::default()
        };
        let triggers = vec!["created".to_string(), "review_requested".to_string()];
        let ctx = PromptContext {
            repo: &repo,
            daemon: &d,
            bot_login: "bot",
            driver: DriverKind::Orca,
            pr: Some(&pr),
            triggers: &triggers,
            owner: Some(3),
            delegated_by: None,
            projects: &[],
            project_prompt: None,
        };
        let ev = Rendered {
            key: "k".into(),
            text: "- [t] @alice requested a review from @bot".into(),
            origin: None,
            label: None,
        };
        let p = tracked_prompt(&pr_issue, &[ev], &ctx);
        assert!(p.starts_with(
            "[ssf] Now tracking pull request #4 \"Fix it\" (https://gh/4) for this session, because this session opened it. A review was asked of @bot: not yours to do; a separate reviewer session (o/r#4:reviewer) reviews it, and its review arrives here as activity.\n\nActivity so far:\n\n- [t] @alice requested a review from @bot\n"
        ), "{p}");
        // The review request is not the author's to act on: a reviewer
        // session takes it.
        assert!(!p.contains("that is for you to act on"));
        assert!(!p.contains("SSF_ISSUE"));
        assert!(p.ends_with(
            "\nAnswer on it with `gh pr comment 4 --repo o/r`; pushes to `bot/fix` update it."
        ));
        assert!(!p.contains("## How to work on this"));
        // Other human triggers on the owned PR still are.
        let assigned = vec!["created".to_string(), "assigned".to_string()];
        let actx = PromptContext {
            triggers: &assigned,
            ..ctx.clone()
        };
        let p = tracked_prompt(&pr_issue, &[], &actx);
        assert!(p.contains("because it was assigned to @bot; that is for you to act on"));
        assert!(!p.contains("reviewer session"));

        // A branch match rather than a tag.
        let mut untagged = pr_issue.clone();
        untagged.body = None;
        let p = tracked_prompt(&untagged, &[], &ctx);
        assert!(p.contains("because its branch `bot/fix` is this workspace's branch"));
        assert!(p.contains("(no activity yet)"));

        // A bound item is named with its title (the session may have
        // several); the session's own item, by number only.
        let c = closed_prompt(&pr_issue, &[], &ctx);
        assert_eq!(
            c,
            "[ssf] #4 \"Fix it\" has been closed (completed).\n\nNo further updates for it; your own item, #3, is unaffected."
        );
        let u = unassigned_prompt(&pr_issue, &[], &ctx);
        assert_eq!(
            u,
            "[ssf] The review request for @bot on #4 \"Fix it\" has been fulfilled or withdrawn.\n\nNo further updates for it unless it is brought back in; your own item, #3, is unaffected."
        );
        let own = PromptContext {
            owner: None,
            ..ctx.clone()
        };
        let c = closed_prompt(&pr_issue, &[], &own);
        assert_eq!(
            c,
            "[ssf] #4 has been closed (completed).\n\nStop working on it: commit anything worth keeping, push, and leave a short final comment on it; then, only if everything is on origin, `ssf release` gives this workspace back (it refuses if anything would be lost; a kept workspace is fine). No further updates for it."
        );
        let u = unassigned_prompt(&pr_issue, &[], &own);
        assert!(u.starts_with("[ssf] The review request for @bot on #4 has been fulfilled or withdrawn.\n\nStop working on it:"));
        let assigned_ctx = PromptContext {
            triggers: &assigned,
            ..own.clone()
        };
        let u = unassigned_prompt(&pr_issue, &[], &assigned_ctx);
        assert!(u.starts_with("[ssf] @bot is no longer assigned to or requested on #4.\n\n"));
        let r = reassigned_prompt(&pr_issue, &[], &assigned_ctx);
        assert_eq!(
            r,
            "[ssf] #4 has been assigned to @bot again. Activity since then:\n\n(no new activity)\n\nResume work on it."
        );
        let f = followup_prompt(&pr_issue, &[], &own);
        assert!(f.starts_with("[ssf] New activity on #4:"), "{f}");
        let f = followup_prompt(&pr_issue, &[], &ctx);
        assert!(f.starts_with("[ssf] New activity on #4 \"Fix it\":"), "{f}");

        // The message a delegating parent gets.
        let last = FinalComment {
            author: "bot".into(),
            session: Some("o/r#4".into()),
            url: "https://gh/4#c1".into(),
            body: "Done, see PR #5.".into(),
        };
        let m = delegated_closed_prompt(&pr_issue, true, Some(&last), &ctx);
        assert!(m.starts_with(
            "[ssf] #4 \"Fix it\" (https://gh/4), the pull request this session handed off, has been merged."
        ));
        assert!(m.contains("Final comment by @bot (from the agent on o/r#4) (https://gh/4#c1):\n  > Done, see PR #5."));
        let m = delegated_closed_prompt(&pr_issue, false, None, &ctx);
        assert!(m.contains("has been closed (completed)."));
        assert!(m.contains("It has no comments."));
    }

    #[test]
    fn reviewer_sessions_get_review_prompts() {
        let pr_issue: Issue = serde_json::from_value(json!({
            "number": 4, "title": "Fix it", "body": "<!-- ssf: origin=o/r#3 -->\n\nFixes it", "html_url": "https://gh/4",
            "state": "open", "user": {"login": "bot"}, "created_at": "t", "updated_at": "t",
            "pull_request": {}
        })).unwrap();
        let repo = RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            instructions: Some("Run the tests.".into()),
            ..Default::default()
        };
        let d = cfg();
        let pr = PrInfo {
            head_ref: "bot/fix".into(),
            head_repo: "o/r".into(),
            base_ref: "main".into(),
            ..Default::default()
        };
        let triggers = vec!["review_requested".to_string()];
        let ctx = PromptContext {
            repo: &repo,
            daemon: &d,
            bot_login: "bot",
            driver: DriverKind::Orca,
            pr: Some(&pr),
            triggers: &triggers,
            owner: Some(3),
            delegated_by: None,
            projects: &[],
            project_prompt: Some(ProjectPrompt {
                source: "SSF.md".into(),
                text: "Keep cargo test green.".into(),
            }),
        };
        assert_eq!(
            ctx.reviewer_session(&pr_issue).as_deref(),
            Some("o/r#4:reviewer")
        );
        let ev = Rendered {
            key: "review_requested:1".into(),
            text: "- [t] @alice requested a review from @bot".into(),
            origin: None,
            label: None,
        };
        let p = review_prompt(&pr_issue, &[ev.clone()], &ctx);
        assert!(p.contains(
            "## Description\n\nFixes it\n\n## Activity so far\n\n- [t] @alice requested"
        ));
        assert!(p.contains("## How to review this"));
        assert!(p.contains(
            "on #4, which another session of the same bot (o/r#3) wrote and so must not review."
        ));
        assert!(p.contains(
            "read-only checkout of the pull request's head (`origin/bot/fix`, against `main`)"
        ));
        assert!(p.contains("git diff origin/main...origin/bot/fix"));
        assert!(p.contains("git reset --hard origin/bot/fix"));
        assert!(p.contains("gh pr review 4 --repo o/r --approve|--request-changes|--comment"));
        assert!(p.contains("must start with the line `🤖#4 (reviewer) says: <!-- ssf: origin=o/r#4 role=reviewer -->`"), "{p}");
        assert!(p.contains("`SSF_ROLE` is `reviewer`"));
        assert!(p.contains("marked \"from the agent on o/r#3\""));
        assert!(p.contains("To speak to it, comment on the pull request."));
        assert!(p.contains("Run the tests."));
        assert!(p.contains("## Project notes"));
        assert!(p.contains("Keep cargo test green."));
        assert!(!p.contains("## How to work on this"));
        assert!(
            !p.contains("<!-- ssf: origin=o/r#3 -->"),
            "the body's tag is stripped"
        );
        assert!(!p.contains("careful colleague"));
        assert!(!p.contains("ssf peers"));
        assert!(p.contains("`ssf guide` explains the rest"));
        assert!(p.contains(
            "## Project notes (`SSF.md`)\n\n(What you review against; the steps about delivering changes are the author's.)\n\nKeep cargo test green."
        ));
        assert!(!p.contains("They say"));

        let f = review_followup_prompt(&pr_issue, &[ev.clone()], &ctx);
        // The reviewer's own item needs no title or URL; nothing follows
        // the activity.
        assert_eq!(
            f,
            "[ssf] New activity on #4:\n\n- [t] @alice requested a review from @bot\n"
        );

        assert!(p.contains("this session because a review was requested from @bot on #4"));
        assert!(!p.contains("GH_TOKEN"));
        assert!(p.contains("(or `gh pr diff 4 --repo o/r`)"));
        assert!(p.contains(
            "the request is fulfilled (ssf removes the `review` label, or GitHub drops the review request; leave the label alone yourself)"
        ));

        let a = review_again_prompt(&pr_issue, &[], &ctx);
        assert!(a.starts_with(
            "[ssf] Another review of #4 is asked: a review was requested from @bot. Activity since your last look:"
        ));
        assert!(a.contains("(no new activity)"));
        assert!(a.contains("git reset --hard origin/bot/fix"));
        assert!(a.contains("gh pr review 4 --repo o/r"));

        let done = review_done_prompt(&pr_issue, &[ev.clone()], &ctx, ReviewEnd::Fulfilled);
        assert!(done.starts_with(
            "[ssf] The review asked of @bot on #4 has been posted, or the request withdrawn.\n\n- [t] @alice requested a review"
        ));
        assert!(done.ends_with(
            "Stop here; post nothing more. If a review is asked again you will be told here, with what happened in between."
        ));
        let merged = review_done_prompt(&pr_issue, &[], &ctx, ReviewEnd::Closed { merged: true });
        assert_eq!(
            merged,
            "[ssf] #4 has been merged.\n\nThis review session is over: stop, and post nothing more."
        );
        let closed = review_done_prompt(&pr_issue, &[], &ctx, ReviewEnd::Closed { merged: false });
        assert!(closed.contains("has been closed."));

        // The author's follow-up says who reviews, only when a request is among the events.
        let fu = followup_prompt(&pr_issue, &[ev.clone()], &ctx);
        assert!(fu.ends_with(
            "\nThe review asked of @bot is not yours to do: a separate reviewer session (o/r#4:reviewer) reviews what this session wrote, and its review arrives here as activity."
        ), "{fu}");
        let other = Rendered {
            key: "commented:2".into(),
            text: "- [t] @alice commented".into(),
            origin: None,
            label: None,
        };
        let fu = followup_prompt(&pr_issue, &[other], &ctx);
        assert!(!fu.contains("reviewer session"));
        // An issue, or an unowned PR, has no reviewer session.
        let unowned = PromptContext {
            owner: None,
            ..ctx.clone()
        };
        assert!(unowned.reviewer_session(&pr_issue).is_none());
        let fu = followup_prompt(&pr_issue, &[ev], &unowned);
        assert!(!fu.contains("reviewer session"));

        // The author's own instructions leave the reviewer rule to the
        // guide; the follow-up says it when a review is actually asked.
        let initial = initial_prompt(&pr_issue, &[], &unowned);
        assert!(!initial.contains("reviewer session"));
        assert!(initial.contains("`ssf guide` explains the rest"));
        assert!(
            initial.contains("because #4 requested a review from @bot."),
            "{initial}"
        );
        assert!(initial.contains(
            "- This worktree is on the pull request's branch `bot/fix`; pushes to it change the PR. \
Answer on it with `gh pr comment 4 --repo o/r`, or `gh pr review 4 --repo o/r` when a review was asked.\n"
        ), "{initial}");
        assert!(initial.contains("GitHub comments on the pull request."));
        assert!(!initial.contains("Do not merge"));
        let fork = PrInfo {
            head_repo: "someone/r".into(),
            ..pr.clone()
        };
        let forked = PromptContext {
            pr: Some(&fork),
            ..unowned.clone()
        };
        let initial = initial_prompt(&pr_issue, &[], &forked);
        // The worktree's base is the repository's configured base branch (or
        // Orca's default when none is set), never the PR's base ref.
        assert!(initial.contains(
            "- The pull request comes from a fork (someone/r), so this worktree cannot push to its \
branch; it is on a branch of its own. Answer on it with `gh pr comment 4 --repo o/r`, or \
`gh pr review 4 --repo o/r` when a review was asked.\n"
        ), "{initial}");
        let based = RepoConfig {
            base_branch: Some("develop".into()),
            ..repo.clone()
        };
        let initial = initial_prompt(
            &pr_issue,
            &[],
            &PromptContext {
                repo: &based,
                ..forked.clone()
            },
        );
        assert!(
            initial.contains("it is on a branch of its own off `develop`. Answer"),
            "{initial}"
        );
        assert!(!initial.contains("off `main`"));
        let mentioned = vec!["assigned".to_string(), "mentioned".to_string()];
        let both = PromptContext {
            triggers: &mentioned,
            ..unowned.clone()
        };
        let initial = initial_prompt(&pr_issue, &[], &both);
        assert!(initial.contains("because #4 was assigned to @bot and mentioned @bot."));
        let created = vec!["created".to_string()];
        let opened = PromptContext {
            triggers: &created,
            ..unowned.clone()
        };
        let initial = initial_prompt(&pr_issue, &[], &opened);
        assert!(
            initial.contains("because #4 was opened by @bot."),
            "{initial}"
        );
        let none = PromptContext {
            triggers: &[],
            ..unowned.clone()
        };
        let initial = initial_prompt(&pr_issue, &[], &none);
        assert!(
            initial.contains("because #4 was assigned to @bot."),
            "{initial}"
        );

        assert_eq!(review_worktree_name(4, "Fix it"), "review-4-fix-it");
    }

    #[test]
    fn the_review_label_asks_for_a_review() {
        let repo = RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            ..Default::default()
        };
        let d = cfg();
        let pr = PrInfo {
            head_ref: "bot/fix".into(),
            head_repo: "o/r".into(),
            base_ref: "main".into(),
            ..Default::default()
        };
        let pr_issue: Issue = serde_json::from_value(json!({
            "number": 4, "title": "Fix it", "body": "<!-- ssf: origin=o/r#3 -->\n\nFixes it",
            "html_url": "https://gh/4", "state": "open", "user": {"login": "bot"},
            "labels": [{"name": "Review"}], "pull_request": {},
            "created_at": "t", "updated_at": "t"
        }))
        .unwrap();
        assert!(pr_issue.has_label("review"));
        assert!(!pr_issue.has_label("bug"));
        let created = vec!["created".to_string()];
        let ctx = PromptContext {
            repo: &repo,
            daemon: &d,
            bot_login: "bot",
            driver: DriverKind::Orca,
            pr: Some(&pr),
            triggers: &created,
            owner: Some(3),
            delegated_by: None,
            projects: &[],
            project_prompt: None,
        };
        let labeled = json!({"event": "labeled", "id": 5, "actor": {"login": "alice"},
            "label": {"name": "Review"}, "created_at": "t"});
        let ev = render_event(&labeled, false, &d, "bot").unwrap();
        assert_eq!(ev.label.as_deref(), Some("Review"));
        assert!(ev.asks_review(Some("review")));
        assert!(!ev.asks_review(Some("needs-review")));
        assert!(!ev.asks_review(None));
        let other = render_event(
            &json!({"event": "labeled", "id": 6, "actor": {"login": "alice"},
                "label": {"name": "bug"}, "created_at": "t"}),
            false,
            &d,
            "bot",
        )
        .unwrap();
        assert!(!other.asks_review(Some("review")));
        let request = Rendered {
            key: "review_requested:1".into(),
            text: String::new(),
            origin: None,
            label: None,
        };
        assert!(request.asks_review(None));

        // The author is told the label is not its to act on.
        let fu = followup_prompt(&pr_issue, std::slice::from_ref(&ev), &ctx);
        assert!(fu.contains(
            "The review asked of @bot is not yours to do: a separate reviewer session (o/r#4:reviewer) reviews what this session wrote"
        ));
        let fu = followup_prompt(&pr_issue, std::slice::from_ref(&other), &ctx);
        assert!(!fu.contains("reviewer session"));
        // ...also when the PR is first bound with the label already on it.
        let t = tracked_prompt(&pr_issue, std::slice::from_ref(&ev), &ctx);
        assert!(t.contains(
            "A review was asked of @bot (the `review` label): not yours to do; a separate reviewer session (o/r#4:reviewer) reviews it"
        ));
        assert!(!t.contains("that is for you to act on"));
        let mut plain = pr_issue.clone();
        plain.labels.clear();
        let t = tracked_prompt(&plain, &[], &ctx);
        assert!(!t.contains("reviewer session"));

        // The reviewer is told what asked for the review.
        let asked = vec!["review_label".to_string()];
        let rctx = PromptContext {
            triggers: &asked,
            ..ctx.clone()
        };
        let p = review_prompt(&pr_issue, std::slice::from_ref(&ev), &rctx);
        assert!(p.contains(
            "this session because the `review` label was added on #4, which another session of the same bot (o/r#3) wrote"
        ));
        let a = review_again_prompt(&pr_issue, &[], &rctx);
        assert!(
            a.starts_with("[ssf] Another review of #4 is asked: the `review` label was added.")
        );
        let both = vec!["review_requested".to_string(), "review_label".to_string()];
        let bctx = PromptContext {
            triggers: &both,
            ..ctx.clone()
        };
        assert_eq!(
            bctx.review_asked(),
            "a review was requested from @bot and the `review` label was added"
        );
        assert_eq!(ctx.review_asked(), "a review was asked of @bot");
        assert_eq!(
            bctx.because(),
            "it requested a review from @bot and was given the `review` label, which asks @bot for a review"
        );
    }

    #[test]
    fn initial_prompt_lists_project_boards_without_prescribing_columns() {
        use crate::github::StatusOption;
        let issue: Issue = serde_json::from_value(json!({
            "number": 3, "title": "Add thing", "body": "Please add", "html_url": "https://gh/3", "state": "open",
            "user": {"login": "carol"}, "created_at": "t", "updated_at": "t"
        })).unwrap();
        let repo = RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            ..Default::default()
        };
        let d = cfg();
        let boards = vec![
            ProjectCard {
                project_id: "PVT_1".into(),
                title: "Roadmap".into(),
                url: "https://gh/p/1".into(),
                item_id: "PVTI_1".into(),
                status: Some("Todo".into()),
                status_field_id: Some("PVTSSF_1".into()),
                status_options: vec![
                    StatusOption {
                        id: "a1".into(),
                        name: "Todo".into(),
                    },
                    StatusOption {
                        id: "b2".into(),
                        name: "In Progress".into(),
                    },
                ],
            },
            ProjectCard {
                project_id: "PVT_2".into(),
                title: "Bare".into(),
                url: "https://gh/p/2".into(),
                item_id: "PVTI_2".into(),
                ..Default::default()
            },
        ];
        let ctx = PromptContext {
            repo: &repo,
            daemon: &d,
            bot_login: "bot",
            driver: DriverKind::Orca,
            pr: None,
            triggers: &[],
            owner: None,
            delegated_by: None,
            projects: &boards,
            project_prompt: None,
        };
        let p = initial_prompt(&issue, &[], &ctx);
        assert!(p.contains("## Project boards\n\n- Roadmap (https://gh/p/1): Status is \"Todo\". Options: \"Todo\", \"In Progress\".\n"));
        assert!(p.contains(
            "`gh project item-edit --project-id PVT_1 --id PVTI_1 --field-id PVTSSF_1 --single-select-option-id <option id>`, where \"Todo\" = a1, \"In Progress\" = b2."
        ));
        assert!(p.contains(
            "- Bare (https://gh/p/2): this board has no Status field.\n\nKeep the card's Status \
accurate; which column fits is your call.\n\n## Description"
        ));
        assert!(!p.contains("never moves cards"));
        assert!(p.find("## Project boards").unwrap() < p.find("## Description").unwrap());
        // No column is prescribed for any situation: the option names appear
        // only in the board listing, never in the instructions.
        let how = &p[p.find("## How to work on this").unwrap()..];
        assert!(!how.contains("card"));
        assert!(!how.contains("Todo") && !how.contains("In Progress"));

        let none = PromptContext {
            projects: &[],
            ..ctx
        };
        let p = initial_prompt(&issue, &[], &none);
        assert!(!p.contains("Project boards"));
        assert!(!p.contains("gh project item-edit"));
    }

    #[test]
    fn project_prompt_is_read_from_the_worktree() {
        let dir = std::env::temp_dir().join(format!(
            "ssf-prompt-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".ssf")).unwrap();
        let repo = RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            ..Default::default()
        };
        assert_eq!(ProjectPrompt::load(&repo, &dir), None, "no file, no notes");
        std::fs::write(dir.join("SSF.md"), "  \n").unwrap();
        assert_eq!(
            ProjectPrompt::load(&repo, &dir),
            None,
            "blank file, no notes"
        );
        std::fs::write(dir.join("SSF.md"), "\n# Notes\n\nBe brief.\n\n").unwrap();
        assert_eq!(
            ProjectPrompt::load(&repo, &dir),
            Some(ProjectPrompt {
                source: "SSF.md".into(),
                text: "# Notes\n\nBe brief.".into()
            })
        );

        std::fs::write(dir.join(".ssf/prompt.md"), "From the dotdir.").unwrap();
        let repo = RepoConfig {
            prompt_file: Some(".ssf/prompt.md".into()),
            ..repo
        };
        let pp = ProjectPrompt::load(&repo, &dir).unwrap();
        assert_eq!(pp.source, ".ssf/prompt.md");
        assert_eq!(pp.text, "From the dotdir.");

        let outside = dir.join("elsewhere.md");
        std::fs::write(&outside, "Absolute.").unwrap();
        let repo = RepoConfig {
            prompt_file: Some(outside.to_string_lossy().to_string()),
            ..repo
        };
        assert_eq!(
            ProjectPrompt::load(&repo, Path::new("/nonexistent"))
                .unwrap()
                .text,
            "Absolute."
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tags_are_stripped_from_bodies_and_shown_as_sessions() {
        let ev = json!({"event":"commented","id":1,"user":{"login":"bot"},"created_at":"t",
            "body":"🤖#9 <!-- ssf: origin=o/r#9 -->\n\ndone","html_url":"https://x/1"});
        let r = render_event(&ev, false, &cfg(), "bot").unwrap();
        assert!(
            r.text
                .contains("@bot commented (from the agent on o/r#9) (https://x/1):\n  > done"),
            "{}",
            r.text
        );
        assert!(!r.text.contains("<!--"));
        assert!(!r.text.contains("🤖"), "the byline goes with the tag");
        assert_eq!(r.origin.as_deref(), Some("o/r#9"));
        // A comment by the bot login with no tag was typed by a person.
        let ev = json!({"event":"commented","id":11,"user":{"login":"bot"},"created_at":"t",
            "body":"typed as the bot","html_url":"https://x/11"});
        let r = render_event(&ev, false, &cfg(), "bot").unwrap();
        assert!(
            r.text.contains(
                "@bot commented (not from a session) (https://x/11):\n  > typed as the bot"
            ),
            "{}",
            r.text
        );
        assert!(r.origin.is_none());
        // A tag at the end of the body (posts made before the byline) still
        // attributes the post to its session.
        let ev = json!({"event":"commented","id":12,"user":{"login":"bot"},"created_at":"t",
            "body":"old style\n\n<!-- ssf: origin=o/r#9 -->","html_url":"https://x/12"});
        let r = render_event(&ev, false, &cfg(), "bot").unwrap();
        assert!(
            r.text
                .contains("(from the agent on o/r#9) (https://x/12):\n  > old style"),
            "{}",
            r.text
        );
        assert_eq!(r.origin.as_deref(), Some("o/r#9"));
        let review = json!({"event":"reviewed","id":2,"user":{"login":"bot"},"state":"approved",
            "body":"<!-- ssf: origin=o/r#9 -->"});
        let r = render_event(&review, false, &cfg(), "bot").unwrap();
        assert!(
            r.text
                .ends_with("reviewed (approved) (from the agent on o/r#9)")
        );
        assert_eq!(r.origin.as_deref(), Some("o/r#9"));
        // A reviewer session's posts are told apart from the author's.
        let review = json!({"event":"reviewed","id":3,"user":{"login":"bot"},"state":"changes_requested",
            "body":"<!-- ssf: origin=o/r#9 role=reviewer -->\n\nnits"});
        let r = render_event(&review, false, &cfg(), "bot").unwrap();
        assert!(r.text.contains(
            "reviewed (changes_requested) (from the reviewer session on o/r#9):\n  > nits"
        ));
        assert_eq!(r.origin.as_deref(), Some("o/r#9:reviewer"));
        let inline = json!({"event":"line-commented","comments":[{"id":8,"user":{"login":"bot"},"path":"a.rs",
            "line":3,"body":"<!-- ssf: origin=o/r#9 role=reviewer -->\n\ntypo","html_url":"u8"}]});
        let r = render_event(&inline, false, &cfg(), "bot").unwrap();
        assert!(
            r.text
                .contains("commented on `a.rs` line 3 (from the reviewer session on o/r#9)")
        );
        assert_eq!(r.origin.as_deref(), Some("o/r#9:reviewer"));
        // A human quoting a bot comment is not "from a session".
        let human = json!({"event":"commented","id":5,"user":{"login":"alice"},"created_at":"t",
            "body":"> <!-- ssf: origin=o/r#9 -->\n\nthanks","html_url":"https://x/5"});
        let r = render_event(&human, false, &cfg(), "bot").unwrap();
        assert!(r.text.contains("@alice commented (https://x/5)"));
        assert!(r.origin.is_none());
        assert!(
            r.text.contains("<!-- ssf: origin=o/r#9 -->"),
            "quoted text is shown verbatim"
        );

        let issue: Issue = serde_json::from_value(json!({
            "number": 4, "title": "PR", "body": "<!-- ssf: origin=o/r#3 -->\n\nFixes it", "html_url": "https://gh/4",
            "state": "open", "user": {"login": "bot"}, "created_at": "t", "updated_at": "t"
        })).unwrap();
        let repo = RepoConfig {
            name: "o/r".into(),
            harness: "claude".into(),
            ..Default::default()
        };
        let d = cfg();
        let ctx = PromptContext {
            repo: &repo,
            daemon: &d,
            bot_login: "bot",
            driver: DriverKind::Orca,
            pr: None,
            triggers: &[],
            owner: None,
            delegated_by: None,
            projects: &[],
            project_prompt: None,
        };
        let p = initial_prompt(&issue, &[], &ctx);
        assert!(
            p.contains("\n\nOpened by @bot (from the agent on o/r#3) on t.\n"),
            "{p}"
        );
        assert!(p.contains("## Description\n\nFixes it\n\n## Activity"));
    }

    #[test]
    fn interrupted_prompt_names_the_session_and_where_it_is() {
        let p = interrupted_prompt(&Interrupted {
            number: 18,
            title: "Resume sessions",
            url: "https://gh/18",
            branch: Some("refs/heads/bot/issue-18"),
            path: Some("/w/issue-18"),
            reviewer: false,
        });
        assert!(p.starts_with("[ssf] The factory restarted"));
        assert!(p.contains("session for #18 \"Resume sessions\" (https://gh/18)"));
        assert!(p.contains("on branch `bot/issue-18` in `/w/issue-18`"));
        assert!(p.contains("`git status`, `git log`"));
        assert!(p.contains("say so on the item"));

        let r = interrupted_prompt(&Interrupted {
            number: 25,
            title: "Fix",
            url: "https://gh/25",
            branch: None,
            path: None,
            reviewer: true,
        });
        assert!(r.contains("reviewer session for pull request #25 \"Fix\" (https://gh/25), a read-only checkout of it."));
        assert!(!r.contains("on branch"));
        assert!(r.contains("If your review has not been posted yet"));
    }

    /// Every message kind, rendered with the fixtures the catalogue on #16
    /// used (issue #18, PR #22, issue #21, the board, `SSF.md`), with sizes.
    /// Run with `cargo test prompt_catalogue -- --ignored --nocapture` to
    /// measure a wording change; nothing is asserted.
    #[test]
    #[ignore = "prints the prompt catalogue with sizes"]
    fn prompt_catalogue() {
        use crate::github::StatusOption;
        let d = cfg();
        let bot = "OverlayBot";
        let repo = RepoConfig {
            name: "mikekelly/simple-software-factory".into(),
            harness: "claude".into(),
            ..Default::default()
        };
        let base = "https://github.com/mikekelly/simple-software-factory";
        let issue18: Issue = serde_json::from_value(json!({
            "number": 18,
            "title": "Resume sessions after a machine restart: startup reconciliation pass",
            "body": "After a reboot Orca's terminals are gone and ssf only notices when the next GitHub event arrives.\n\nProposal: a startup reconciliation pass that resumes every active session whose worktree exists but has no live agent terminal.",
            "html_url": format!("{base}/issues/18"), "state": "open", "state_reason": "completed",
            "user": {"login": bot}, "labels": [{"name": "daemon"}],
            "created_at": "2026-09-04T13:56:29Z", "updated_at": "2026-09-04T17:29:10Z"
        })).unwrap();
        let pr22_title = "Comment by default, tell as the exception: prompt and README wording";
        let pr22: Issue = serde_json::from_value(json!({
            "number": 22, "title": pr22_title,
            "body": "<!-- ssf: origin=mikekelly/simple-software-factory#21 -->\n\nCloses #21\n\nReworded the peers/sub/tell paragraph.",
            "html_url": format!("{base}/pull/22"), "state": "open", "pull_request": {},
            "user": {"login": bot},
            "created_at": "2026-09-04T17:17:00Z", "updated_at": "2026-09-04T17:17:00Z"
        })).unwrap();
        let pr = PrInfo {
            head_ref: "mikekelly/issue-21-comment-by-default-tell-as-the-exception".into(),
            head_repo: "mikekelly/simple-software-factory".into(),
            base_ref: "master".into(),
            ..Default::default()
        };
        let issue21: Issue = serde_json::from_value(json!({
            "number": 21, "title": pr22_title, "body": "",
            "html_url": format!("{base}/issues/21"), "state": "closed", "state_reason": "completed",
            "user": {"login": bot},
            "created_at": "2026-09-04T17:10:00Z", "updated_at": "2026-09-04T18:10:00Z"
        }))
        .unwrap();
        let boards = vec![ProjectCard {
            project_id: "PVT_kwHN2ebOAYlXgQ".into(),
            title: "Simple Software Factory".into(),
            url: "https://github.com/users/mikekelly/projects/5".into(),
            item_id: "PVTI_lAHN2ebOAYlXgc4OXRyA".into(),
            status: Some("Todo".into()),
            status_field_id: Some("PVTSSF_lAHN2ebOAYlXgc4YTeZE".into()),
            status_options: [
                ("Longrunners", "6ea12c8d"),
                ("Todo", "f75ad846"),
                ("In Progress", "47fc9ee4"),
                ("Done", "98236657"),
            ]
            .iter()
            .map(|(n, i)| StatusOption {
                id: i.to_string(),
                name: n.to_string(),
            })
            .collect(),
        }];
        let notes = ProjectPrompt {
            source: "SSF.md".into(),
            text: "# Notes for ssf agents\n\n\
- Work on the issue's branch and open a PR that references the issue.\n\
- Keep `cargo test` green and run `cargo fmt` and `cargo clippy` before pushing.\n\
- Update `README.md` and `config.example.toml` for any user-visible behaviour.\n\
- Rebuild the package with `cd packaging && makepkg -fd` before calling something done; commit the `pkgver` bump makepkg makes to `packaging/PKGBUILD`.\n\
- The installed service runs the last package the maintainer installed, so verify daemon behaviour with unit tests and scratch `SSF_CONFIG_DIR`/`SSF_STATE_DIR` runs rather than expecting to see your change live."
                .into(),
        };
        let render = |v: Value| render_event(&v, false, &d, bot).unwrap();
        let ev = |kind: &str, actor: &str, at: &str, extra: Value| {
            let mut v =
                json!({"event": kind, "id": 1, "actor": {"login": actor}, "created_at": at});
            if let (Some(m), Some(e)) = (v.as_object_mut(), extra.as_object()) {
                for (k, val) in e {
                    m.insert(k.clone(), val.clone());
                }
            }
            render(v)
        };
        let added = ev(
            "added_to_project_v2",
            bot,
            "2026-09-04T13:56:29Z",
            json!({}),
        );
        let assigned = ev(
            "assigned",
            "mikekelly",
            "2026-09-04T17:29:10Z",
            json!({"assignee": {"login": bot}}),
        );
        let comment = ev(
            "commented",
            "mikekelly",
            "2026-09-04T17:40:02Z",
            json!({"html_url": format!("{base}/issues/18#issuecomment-1"),
                "body": "Please stagger the relaunches by at least ten seconds; nine Claude sessions starting at once will thrash the machine."}),
        );
        let requested = ev(
            "review_requested",
            "mikekelly",
            "2026-09-04T17:45:00Z",
            json!({"requested_reviewer": {"login": bot}}),
        );
        let closed = ev(
            "closed",
            "mikekelly",
            "2026-09-04T18:00:00Z",
            json!({"state_reason": "completed"}),
        );
        let unassigned = ev(
            "unassigned",
            "mikekelly",
            "2026-09-04T18:00:00Z",
            json!({"assignee": {"login": bot}}),
        );
        let reassigned = ev(
            "assigned",
            "mikekelly",
            "2026-09-04T18:05:00Z",
            json!({"assignee": {"login": bot}}),
        );
        let bot_closed = ev(
            "closed",
            bot,
            "2026-09-04T18:10:00Z",
            json!({"state_reason": "completed"}),
        );
        let reply = ev(
            "commented",
            bot,
            "2026-09-04T17:50:00Z",
            json!({"html_url": format!("{base}/pull/22#issuecomment-2"),
                "body": "🤖#21 <!-- ssf: origin=mikekelly/simple-software-factory#21 -->\n\nAddressed both points in 3f2a1c0."}),
        );
        let again = ev(
            "review_requested",
            "mikekelly",
            "2026-09-04T18:20:00Z",
            json!({"requested_reviewer": {"login": bot}}),
        );

        let assigned_t = vec!["assigned".to_string()];
        let created_t = vec!["created".to_string()];
        let delegated_t = vec!["assigned".to_string(), "created".to_string()];
        let requested_t = vec!["review_requested".to_string()];
        let own = PromptContext {
            repo: &repo,
            daemon: &d,
            bot_login: bot,
            driver: DriverKind::Orca,
            pr: None,
            triggers: &assigned_t,
            owner: None,
            delegated_by: None,
            projects: &boards,
            project_prompt: Some(notes.clone()),
        };
        let owned_pr = PromptContext {
            pr: Some(&pr),
            triggers: &created_t,
            owner: Some(21),
            projects: &[],
            project_prompt: None,
            ..own.clone()
        };
        let child = PromptContext {
            triggers: &delegated_t,
            delegated_by: Some("mikekelly/simple-software-factory#16"),
            ..own.clone()
        };
        let sub = PromptContext {
            projects: &[],
            project_prompt: None,
            ..own.clone()
        };
        let reviewer = PromptContext {
            triggers: &requested_t,
            project_prompt: Some(notes.clone()),
            ..owned_pr.clone()
        };
        let final_comment = FinalComment {
            author: bot.into(),
            session: Some("mikekelly/simple-software-factory#21".into()),
            url: format!("{base}/issues/21#issuecomment-5544055473"),
            body: "Done: PR #22. Prompt reworded, README updated, pkgver bumped.".into(),
        };
        let owner21 = Some("mikekelly/simple-software-factory#21");
        let catalogue: Vec<(&str, String)> = vec![
            (
                "initial_prompt (issue, assigned, on a board, repo has SSF.md)",
                initial_prompt(&issue18, &[added.clone(), assigned.clone()], &own),
            ),
            (
                "tracked_prompt (session's own PR picked up, no human trigger)",
                tracked_prompt(&pr22, &[], &owned_pr),
            ),
            (
                "followup_prompt (new activity on the session's item)",
                followup_prompt(&issue18, std::slice::from_ref(&comment), &own),
            ),
            (
                "followup_prompt on an owned PR (review requested from the bot)",
                followup_prompt(&pr22, std::slice::from_ref(&requested), &owned_pr),
            ),
            (
                "initial_prompt for a delegated (handed-off) issue",
                initial_prompt(&issue18, &[], &child),
            ),
            (
                "closed_prompt (the session's issue was closed)",
                closed_prompt(&issue18, std::slice::from_ref(&closed), &own),
            ),
            (
                "unassigned_prompt",
                unassigned_prompt(&issue18, std::slice::from_ref(&unassigned), &own),
            ),
            (
                "reassigned_prompt",
                reassigned_prompt(&issue18, std::slice::from_ref(&reassigned), &own),
            ),
            (
                "delegated_closed_prompt (parent hears its hand-off finished)",
                delegated_closed_prompt(&issue21, false, Some(&final_comment), &sub),
            ),
            (
                "fyi_prompt (subscriber sees activity on someone else's item)",
                fyi_prompt(
                    &issue21,
                    std::slice::from_ref(&comment),
                    &sub,
                    owner21,
                    false,
                    Fyi::Activity,
                ),
            ),
            (
                "fyi_prompt (subscribed item closed)",
                fyi_prompt(
                    &issue21,
                    std::slice::from_ref(&bot_closed),
                    &sub,
                    owner21,
                    false,
                    Fyi::Closed,
                ),
            ),
            (
                "tell_prompt (message from another session)",
                tell_prompt(
                    Some("mikekelly/simple-software-factory#16"),
                    Some("Project management"),
                    "master moved after you branched (#14 and #13 merged); please rebase onto origin/master before pushing again.",
                    d.max_body_chars,
                ),
            ),
            (
                "tell_prompt (from a human shell, no session)",
                tell_prompt(None, None, "stop, I'm changing the spec", d.max_body_chars),
            ),
            (
                "review_prompt (reviewer session's first message)",
                review_prompt(&pr22, &[added.clone(), assigned.clone()], &reviewer),
            ),
            (
                "review_followup_prompt",
                review_followup_prompt(&pr22, std::slice::from_ref(&reply), &reviewer),
            ),
            (
                "review_again_prompt (repeated request)",
                review_again_prompt(&pr22, std::slice::from_ref(&again), &reviewer),
            ),
            (
                "review_done_prompt (request fulfilled)",
                review_done_prompt(&pr22, &[], &reviewer, ReviewEnd::Fulfilled),
            ),
            (
                "review_done_prompt (PR merged)",
                review_done_prompt(&pr22, &[], &reviewer, ReviewEnd::Closed { merged: true }),
            ),
        ];
        println!("\n| # | prompt | chars | ~tokens |\n|---|---|---|---|");
        let mut total = 0;
        for (i, (name, text)) in catalogue.iter().enumerate() {
            let n = text.chars().count();
            total += n;
            println!("| {} | {name} | {n} | ~{} |", i + 1, n / 4);
        }
        println!("| | total | {total} | ~{} |", total / 4);
        for (i, (name, text)) in catalogue.iter().enumerate() {
            println!(
                "\n<details>\n<summary><b>{}. {name}</b> ({} chars)</summary>\n\n```text\n{text}\n```\n\n</details>",
                i + 1,
                text.chars().count()
            );
        }
    }
}
