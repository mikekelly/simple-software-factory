//! Rendering GitHub issue timelines into prompts for the agent.

use serde_json::Value;
use std::path::Path;
use tracing::warn;

use crate::config::{DaemonConfig, RepoConfig};
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
    let at = when(ev);
    let head = |what: &str| format!("- [{at}] @{actor} {what}");
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
            format!("- [{at}] commit {} by {actor}: {msg}", short(sha))
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
                let at = value_str(c, &["created_at"]).unwrap_or(&at);
                out.push(format!(
                    "- [{at}] @{who} commented on `{path}`{}{session} ({url}):\n{}",
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
                    "- [{at}] @{who} commented on commit {}{session}:\n{}",
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

    fn because(&self) -> String {
        let bot = self.bot_login;
        let parts: Vec<String> = self
            .triggers
            .iter()
            .map(|t| match t.as_str() {
                "assigned" => format!("it was assigned to @{bot}"),
                "mentioned" => format!("@{bot} was mentioned on it"),
                "review_requested" => format!("a review was requested from @{bot}"),
                "review_label" => format!(
                    "it was given the `{}` label, which asks @{bot} for a review",
                    self.daemon.review_label().unwrap_or("review")
                ),
                "created" => match self.delegated_by {
                    Some(parent) => {
                        format!("the agent session working on {parent} opened it and handed it off")
                    }
                    None => format!("@{bot} opened it"),
                },
                other => other.to_string(),
            })
            .collect();
        if parts.is_empty() {
            format!("it was assigned to @{bot}")
        } else {
            parts.join(" and ")
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

fn issue_header(issue: &Issue, ctx: &PromptContext) -> String {
    let labels: Vec<&str> = issue.labels.iter().map(|l| l.name.as_str()).collect();
    let mut s = match ctx.pr {
        Some(pr) => format!(
            "# GitHub pull request {}#{}: {}\n{}\n\nBranch `{}` into `{}`{}{}. Opened by @{} on {}.",
            ctx.repo.name,
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
            issue.created_at
        ),
        None => format!(
            "# GitHub issue {}#{}: {}\n{}\n\nOpened by @{} on {}.",
            ctx.repo.name,
            issue.number,
            issue.title,
            issue.html_url,
            issue.author(),
            issue.created_at
        ),
    };
    if !labels.is_empty() {
        s.push_str(&format!(" Labels: {}.", labels.join(", ")));
    }
    let by_bot = issue.author().eq_ignore_ascii_case(ctx.bot_login);
    if let Some(t) = issue
        .body
        .as_deref()
        .and_then(origin::parse)
        .filter(|_| by_bot)
    {
        s.push_str(&format!(
            " Opened by the agent session working on {}.",
            t.origin
        ));
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
        s.push_str(
            "\nKeeping the card's Status accurate is part of the job: which column fits is your \
judgement, from what is actually happening. ssf never moves cards itself.\n",
        );
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
    let because = ctx.because();
    let (line, elsewhere) = match origin::Origin::new(repo, n) {
        Some(o) => (
            o.first_line(Some(repo), false, false),
            o.byline(None, false),
        ),
        None => (
            format!("{}#{n} <!-- ssf: origin={repo}#{n} -->", origin::ROBOT),
            format!("{}{repo}#{n}", origin::ROBOT),
        ),
    };
    let mut s = format!(
        "\n## How to work on this\n\n\
You are the coding agent for the GitHub bot account @{bot}. Simple Software Factory (ssf) \
created this workspace because {because}. Work on the {kind} in this worktree, on its branch; \
other sessions' branches and workspaces are not yours to touch. New activity on the {kind} (comments, reviews, label changes, closure) arrives here as messages \
prefixed `[ssf]`; act on them. `ssf guide` explains the rest: other sessions, following items, \
items you open, hand-offs, reviews of your own pull requests.\n\n\
- Talk to the humans through the {kind}, as the bot. `GH_TOKEN`, `GITHUB_TOKEN` and a git \
credential helper are set, so plain `gh` and `git push` act as @{bot}, and commits are authored \
and signed as @{bot}. `SSF_REPO` and `SSF_ISSUE` name this {kind}. Only ever act as @{bot}: \
never use another GitHub account, token or key you find on this machine, even if @{bot} lacks a \
permission; say so on the {kind} instead.\n\
- Every comment, review and pull request you post must start with the line `{line}` (`{elsewhere}` \
for `{}` on another repository) and a blank line; it tells readers and ssf which session posted \
it. The `gh` on this PATH adds it when you pass `--body` or `--body-file` to \
`issue create|comment` or `pr create|comment|review`; add it yourself when you post any other way \
(`gh api`, `gh pr create --fill`, `gh pr edit --body`, ...).\n",
        format!("{}#{n}", origin::ROBOT)
    );
    match ctx.pr {
        Some(pr) if pr.same_repo(repo) => s.push_str(&format!(
            "- This worktree is on the pull request's branch `{}`: commit here and `git push` to \
change the PR. Answer on it with `gh pr comment {n} --repo {repo} --body \"...\"` (or `gh api` \
for inline replies); if you were asked to review rather than to change anything, review with \
`gh pr review {n} --repo {repo}` (--comment, --approve or --request-changes). Do not merge the \
pull request; a human does that.\n",
            pr.head_ref
        )),
        Some(pr) => s.push_str(&format!(
            "- This pull request comes from a fork ({}), so you cannot push to its branch. Answer \
on it with `gh pr comment {n} --repo {repo} --body \"...\"`; describe changes it needs in a review \
(`gh pr review {n} --repo {repo} --request-changes --body \"...\"`) or open a separate PR from \
this worktree against `{}`. Do not merge the pull request; a human does that.\n",
            pr.head_repo, pr.base_ref
        )),
        None => s.push_str(&format!(
            "- When the work is done, push the branch and open a pull request that references the \
issue (`Closes #{n}`), then comment on the issue with the PR link. Do not close the issue \
yourself; a human reviews the PR.\n"
        )),
    }
    if let Some(parent) = ctx.delegated_by {
        s.push_str(&format!(
            "- This {kind} was handed off to you by the agent session working on {parent}. It \
follows this {kind} as a subscriber (it sees the activity but does not act) and is told when \
the {kind} closes, along with your final comment, so make that comment a clear summary of the \
outcome. To ask it something, comment on this {kind}.\n"
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
    let head = format!(
        "[ssf] New activity on {}#{} \"{}\" ({}):",
        ctx.repo.name, issue.number, issue.title, issue.html_url
    );
    let tail = match ctx.reviewer_session(issue).filter(|_| {
        events
            .iter()
            .any(|e| e.asks_review(ctx.daemon.review_label()))
    }) {
        Some(r) => format!(
            "The review asked of @{} is not for you: since this session wrote the pull request, \
a separate reviewer session ({r}) reviews it. Do not review it yourself; its review arrives \
here as activity.",
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
        "[ssf] Now tracking {kind} {}#{} \"{}\" ({}) for this session, because {}. \
Activity on it (comments, reviews, review requests, assignments, closure) will be delivered \
here; `SSF_ISSUE` is unchanged.",
        ctx.repo.name,
        issue.number,
        issue.title,
        issue.html_url,
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
            " A review was asked of @{}{}; since this session wrote the pull request, a \
separate reviewer session ({r}) reviews it. Do not review it yourself: its review arrives here \
as activity.",
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
            "\nReply to review comments and questions with `gh pr comment {} --repo {} --body \"...\"` \
(or `gh api` for inline replies); pushes to `{}` update the PR. Do not merge it; a human does that.",
            issue.number, ctx.repo.name, pr.head_ref
        )),
        Some(_) => s.push_str(
            "\nThis pull request comes from a fork, so answer on it with `gh pr comment` and do not merge it.",
        ),
        None => s.push_str(&format!(
            "\nAnswer on it with `gh issue comment {} --repo {} --body \"...\"`. Do not close it yourself.",
            issue.number, ctx.repo.name
        )),
    }
    s
}

pub fn closed_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let kind = ctx.kind();
    let head = format!(
        "[ssf] {}#{} \"{}\" has been closed ({}).",
        ctx.repo.name,
        issue.number,
        issue.title,
        issue.state_reason.as_deref().unwrap_or("no reason given")
    );
    let tail = match ctx.owner {
        Some(owner) => format!(
            "You will not receive further updates for this {kind}. Your own item, {}#{owner}, is \
unaffected; carry on with it.",
            ctx.repo.name
        ),
        None => format!(
            "Stop working on this {kind}. If you have uncommitted work worth keeping, commit it \
now and leave a short final comment on the {kind}. You will not receive further updates for it."
        ),
    };
    assemble(&head, events, &tail)
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
            "closed, {}",
            issue.state_reason.as_deref().unwrap_or("no reason given")
        )
    };
    let mut s = format!(
        "[ssf] {} {}#{} \"{}\" ({}), which this session handed off, has been {outcome}.\n\n",
        ctx.kind(),
        ctx.repo.name,
        issue.number,
        issue.title,
        issue.html_url
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
/// `owner` is the session acting on the item, if any; `merged` matters only
/// for `Fyi::Closed`.
pub fn fyi_prompt(
    issue: &Issue,
    events: &[Rendered],
    ctx: &PromptContext,
    owner: Option<&str>,
    merged: bool,
    what: Fyi,
) -> String {
    let kind = ctx.kind();
    let owned = match owner {
        Some(o) => format!("owned by another session ({o})"),
        None => "not owned by any session".to_string(),
    };
    let head = match what {
        Fyi::Activity => format!(
            "[ssf] FYI on {kind} {}#{} \"{}\" ({}), {owned}: new activity.",
            ctx.repo.name, issue.number, issue.title, issue.html_url
        ),
        Fyi::Closed => format!(
            "[ssf] FYI on {kind} {}#{} \"{}\" ({}), {owned}: it has been {}.",
            ctx.repo.name,
            issue.number,
            issue.title,
            issue.html_url,
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
            "[ssf] FYI on {kind} {}#{} \"{}\" ({}), {owned}: @{} is no longer involved with it, \
so its session has retired.",
            ctx.repo.name, issue.number, issue.title, issue.html_url, ctx.bot_login
        ),
        Fyi::Tracked => format!(
            "[ssf] FYI on {kind} {}#{} \"{}\" ({}): it now has an agent session of its own ({}), \
because {}.",
            ctx.repo.name,
            issue.number,
            issue.title,
            issue.html_url,
            owner.unwrap_or("?"),
            ctx.because()
        ),
    };
    let tail = match what {
        Fyi::Closed | Fyi::Unassigned => format!(
            "For information only; you will not hear about #{} again unless it comes back.",
            issue.number
        ),
        Fyi::Activity | Fyi::Tracked => format!(
            "For information only: you are subscribed to #{}, not working on it. `ssf unsub {}` \
stops these messages.",
            issue.number, issue.number
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
    pub repo: &'a str,
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
    let item = format!("{}#{}", it.repo, it.number);
    let mut s = String::from(
        "[ssf] The factory restarted (the machine, Orca or ssf itself) and this session was \
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
            "This is the reviewer session for pull request {item} \"{}\" ({}), a read-only \
checkout of the pull request{path}.\n\nIf your review has not been posted yet, pick it up where \
you left off (`git log` shows what you were reviewing) and post it. If it has, nothing is needed \
until the next `[ssf]` message.",
            it.title, it.url
        ));
        return s;
    }
    s.push_str(&format!(
        "This is the session for {item} \"{}\" ({}){branch}{path}.\n\nWork out where you got to \
(`git status`, `git log`, your last comments on the item) and carry on from there. Anything \
that happened on the item while you were away arrives as further `[ssf]` messages. If you were \
part-way through something and cannot tell what is left, say so on the item: that the session \
was interrupted and what remains.",
        it.title, it.url
    ));
    s
}

pub fn unassigned_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let what = if ctx.triggers.iter().any(|t| t == "review_requested")
        && !ctx.triggers.iter().any(|t| t == "assigned")
    {
        "the review request for @{bot} on {repo}#{n} \"{title}\" has been fulfilled or withdrawn"
    } else {
        "@{bot} is no longer assigned to or requested on {repo}#{n} \"{title}\""
    };
    let what = what
        .replace("{bot}", ctx.bot_login)
        .replace("{repo}", &ctx.repo.name)
        .replace("{n}", &issue.number.to_string())
        .replace("{title}", &issue.title);
    let head = format!("[ssf] {what}.");
    let tail = match ctx.owner {
        Some(owner) => format!(
            "You will not receive further updates for it unless it is brought back in. Your own \
item, {}#{owner}, is unaffected.",
            ctx.repo.name
        ),
        None => "Stop working on this. Commit anything worth keeping and leave a short final \
comment summarising where things stand. You will not receive further updates unless you are \
brought back in."
            .to_string(),
    };
    assemble(&head, events, &tail)
}

pub fn reassigned_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let mut s = format!(
        "[ssf] {}#{} \"{}\" has been assigned to @{} again. Activity since you last heard \
about it:\n\n",
        ctx.repo.name, issue.number, issue.title, ctx.bot_login
    );
    if events.is_empty() {
        s.push_str("(no new activity)\n");
    }
    for e in events {
        s.push_str(&e.text);
        s.push('\n');
    }
    s.push_str("\nResume work on the issue with the original instructions.");
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
You are a reviewer for the GitHub bot account @{bot}. Simple Software Factory (ssf) started \
this session because {asked} on pull request {repo}#{n}, which another agent session of the \
same bot ({author}) wrote; that session must not review its own work, so you do. Your job is \
the review, nothing else: you never change the pull request.\n\n\
- This worktree is a read-only checkout of the pull request's head (`origin/{head}`, against \
`{base}`), on a local branch of its own. Do not commit, push, merge, or edit the PR; do not \
change its board cards. `git fetch origin && git diff origin/{base}...origin/{head}` (or \
`gh pr diff {n} --repo {repo}`) shows the whole change; `git fetch origin && git reset --hard \
origin/{head}` brings the checkout up to date after the author pushes. Building and running \
tests here is fine.\n\
- Post the review with `gh pr review {n} --repo {repo} --approve|--request-changes|--comment \
--body \"...\"` (inline comments through `gh api` if useful). One review per request: when it is \
posted the request is fulfilled ({fulfilled}) and this session pauses until a review is asked \
for again, when you get a message here with what happened since.\n\
- `GH_TOKEN` and `GITHUB_TOKEN` are set, so plain `gh` commands act as @{bot}. `SSF_REPO` and \
`SSF_ISSUE` name this pull request and `SSF_ROLE` is `reviewer`. Only ever act as @{bot}: never \
use another GitHub account, token or key you find on this machine.\n\
- Every review and comment you post must start with the line `{line}`, then a blank line: it \
shows readers, and tells ssf, that it came from the reviewer session and not from the author's. \
The `gh` on this PATH adds it when you pass `--body` or `--body-file` to `pr review` or \
`pr comment`; add it yourself when you post any other way (`gh api`, ...).\n\
- The author's session receives your review as activity and answers on the pull request; its \
replies reach you here, marked \"from the agent on {author}\". To speak to it, comment on the \
pull request. `ssf guide` explains the rest: other sessions, `ssf tell`, following items.\n"
    );
    s.push_str(&extras(ctx, true));
    s
}

/// New activity on a pull request under review, for its reviewer session.
pub fn review_followup_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let head = format!(
        "[ssf] New activity on pull request {}#{} \"{}\" ({}), which you are reviewing:",
        ctx.repo.name, issue.number, issue.title, issue.html_url
    );
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
        "[ssf] Another review is asked of @{} on pull request {}#{} \"{}\" ({}): {}. \
Activity since you last looked:\n\n",
        ctx.bot_login,
        ctx.repo.name,
        issue.number,
        issue.title,
        issue.html_url,
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
look at what changed since your last review, and post a new review with `gh pr review {} --repo {}`.",
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
            "[ssf] The review asked of @{} on pull request {}#{} \"{}\" ({}) has been \
posted, or the request withdrawn.",
            ctx.bot_login, ctx.repo.name, issue.number, issue.title, issue.html_url
        ),
        ReviewEnd::Closed { merged } => format!(
            "[ssf] Pull request {}#{} \"{}\" ({}) has been {}.",
            ctx.repo.name,
            issue.number,
            issue.title,
            issue.html_url,
            if merged { "merged" } else { "closed" }
        ),
    };
    let tail = match why {
        ReviewEnd::Fulfilled => {
            "Stop here; do not post anything more. If a review is asked of the bot again you \
will be told here, with what happened in between."
        }
        ReviewEnd::Closed { .. } => {
            "This review session is over: stop, and do not post anything more."
        }
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
involves the bot account @{bot}. Each session has a workspace (a git worktree on the item's \
branch) and a terminal, and receives the item's activity as messages prefixed `[ssf]`. \
`SSF_REPO` and `SSF_ISSUE` name the session's item; `SSF_BOT` is the bot's login; `SSF_ROLE` \
is `reviewer` in a reviewer session. This guide is the reference behind the initial prompt.\n\n\
## Messages you receive\n\n\
- `[ssf] New activity on ...`: comments, reviews, label changes, renames, linked PRs and the \
like on your item. Your own posts are never echoed back.\n\
- `[ssf] Now tracking ...`: an item you opened, or a pull request on your branch, has been bound \
to this session; its activity comes here from now on.\n\
- `[ssf] FYI on ...`: activity on an item you follow but do not work on. For information only.\n\
- `[ssf] Message from ...`: a message pasted into this terminal with `ssf tell` (below).\n\
- `[ssf] ... has been closed`, `... no longer assigned`, `... assigned ... again`: your item's \
lifecycle; each says what to do.\n\
- `[ssf] The factory restarted ...`: the machine, Orca or ssf restarted and this session was \
started again.\n\n\
## Other sessions\n\n\
`ssf peers` lists the agent sessions on this repository: item, GitHub state, agent state, \
branch, last message (`--json` for detail, `--all` to include retired ones). Leave their \
branches and workspaces alone.\n\n\
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
this workspace's branch is yours too, tag or no tag.\n\n\
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
## The byline and origin tag\n\n\
GitHub shows the same bot for every session, so every comment, review and pull request a \
session posts starts with one line that is both a byline for people and a tag for ssf: \
`🤖#N <!-- ssf: origin=owner/repo#N -->` (`🤖owner/repo#N` when the post is on another \
repository; `🤖#N (reviewer)` and `role=reviewer` from a reviewer session; `mode=delegate` on \
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
            pr: None,
            triggers: &[],
            owner: None,
            delegated_by: None,
            projects: &[],
            project_prompt: None,
        };
        let p = initial_prompt(&issue, &[], &ctx);
        assert!(p.contains("o/r#3: Add thing"));
        assert!(p.contains("Labels: feature"));
        assert!(p.contains("(no activity yet)"));
        assert!(p.contains("Closes #3"));
        assert!(p.contains("it was assigned to @bot"));
        assert!(p.contains("GH_TOKEN"));
        assert!(p.contains("must start with the line `🤖#3 <!-- ssf: origin=o/r#3 -->` (`🤖o/r#3` for `🤖#3` on another repository) and a blank line"), "{p}");
        assert!(p.contains("Only ever act as @bot"));
        assert!(p.contains("other sessions' branches and workspaces are not yours to touch"));
        assert!(p.contains("Do not close the issue yourself"));
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
            "because it was assigned to @bot and the agent session working on o/r#1 opened it and handed it off"
        ));
        assert!(p.contains("handed off to you by the agent session working on o/r#1"));
        assert!(p.contains("follows this issue as a subscriber"));
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
        assert!(boards.contains("Keeping the card's Status accurate is part of the job"));
        let how = &p[p.find("## How to work on this").unwrap()..];
        assert!(!how.contains("card"));
    }

    #[test]
    fn guide_holds_the_moved_reference() {
        let g = guide("bot", Some("review"));
        assert!(g.starts_with("# ssf guide\n\n"));
        assert!(g.contains("`ssf peers` lists the agent sessions"));
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
        assert!(p.starts_with(
            "[ssf] FYI on issue o/r#5 \"Thing\" (https://gh/5), owned by another session (o/r#5): new activity.\n\n- [t] @alice"
        ));
        // One line of boilerplate after the activity, no more.
        assert!(p.ends_with(
            "  > hi\n\nFor information only: you are subscribed to #5, not working on it. `ssf unsub 5` stops these messages."
        ));
        assert!(!p.contains("again unless"));
        let p = fyi_prompt(&issue, &[], &ctx, None, false, Fyi::Closed);
        assert!(p.contains("not owned by any session: it has been closed (completed)."));
        assert!(p.ends_with(
            "(completed).\n\nFor information only; you will not hear about #5 again unless it comes back."
        ));
        assert!(!p.contains("unsub"));
        let p = fyi_prompt(&issue, &[], &ctx, Some("o/r#5"), true, Fyi::Closed);
        assert!(p.contains(": it has been merged."));
        let p = fyi_prompt(&issue, &[ev], &ctx, Some("o/r#5"), false, Fyi::Tracked);
        assert!(p.contains(
            "it now has an agent session of its own (o/r#5), because it was assigned to @bot."
        ));
        let p = fyi_prompt(&issue, &[], &ctx, Some("o/r#5"), false, Fyi::Unassigned);
        assert!(p.contains("@bot is no longer involved with it"));

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
            "[ssf] Now tracking pull request o/r#4 \"Fix it\" (https://gh/4) for this session, because this session opened it."
        ));
        // The review request is not the author's to act on: a reviewer
        // session takes it.
        assert!(!p.contains("that is for you to act on"));
        assert!(p.contains(
            "a separate reviewer session (o/r#4:reviewer) reviews it. Do not review it yourself"
        ));
        assert!(p.contains("@alice requested a review"));
        assert!(p.contains("gh pr comment 4 --repo o/r"));
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

        let c = closed_prompt(&pr_issue, &[], &ctx);
        assert!(c.contains("Your own item, o/r#3, is unaffected"));
        assert!(!c.contains("Stop working"));
        let u = unassigned_prompt(&pr_issue, &[], &ctx);
        assert!(u.contains("Your own item, o/r#3, is unaffected"));

        // The message a delegating parent gets.
        let last = FinalComment {
            author: "bot".into(),
            session: Some("o/r#4".into()),
            url: "https://gh/4#c1".into(),
            body: "Done, see PR #5.".into(),
        };
        let m = delegated_closed_prompt(&pr_issue, true, Some(&last), &ctx);
        assert!(m.starts_with(
            "[ssf] pull request o/r#4 \"Fix it\" (https://gh/4), which this session handed off, has been merged."
        ));
        assert!(m.contains("Final comment by @bot (from the agent on o/r#4) (https://gh/4#c1):\n  > Done, see PR #5."));
        let m = delegated_closed_prompt(&pr_issue, false, None, &ctx);
        assert!(m.contains("has been closed, completed."));
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
        assert!(p.contains("another agent session of the same bot (o/r#3) wrote"));
        assert!(p.contains(
            "read-only checkout of the pull request's head (`origin/bot/fix`, against `main`)"
        ));
        assert!(p.contains("git diff origin/main...origin/bot/fix"));
        assert!(p.contains("git reset --hard origin/bot/fix"));
        assert!(p.contains("gh pr review 4 --repo o/r --approve|--request-changes|--comment"));
        assert!(p.contains("must start with the line `🤖#4 (reviewer) <!-- ssf: origin=o/r#4 role=reviewer -->`"), "{p}");
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
        assert!(f.starts_with(
            "[ssf] New activity on pull request o/r#4 \"Fix it\" (https://gh/4), which you are reviewing:"
        ));
        // The header says what the message is; nothing follows the activity.
        assert!(f.ends_with("- [t] @alice requested a review from @bot\n"));

        assert!(p.contains(
            "this session because a review was requested from @bot on pull request o/r#4"
        ));
        assert!(p.contains(
            "the request is fulfilled (ssf removes the `review` label, or GitHub drops the review request; leave the label alone yourself)"
        ));

        let a = review_again_prompt(&pr_issue, &[], &ctx);
        assert!(a.starts_with(
            "[ssf] Another review is asked of @bot on pull request o/r#4 \"Fix it\" (https://gh/4): a review was requested from @bot."
        ));
        assert!(a.contains("(no new activity)"));
        assert!(a.contains("git reset --hard origin/bot/fix"));
        assert!(a.contains("gh pr review 4 --repo o/r"));

        let done = review_done_prompt(&pr_issue, &[ev.clone()], &ctx, ReviewEnd::Fulfilled);
        assert!(done.starts_with(
            "[ssf] The review asked of @bot on pull request o/r#4 \"Fix it\" (https://gh/4) has been posted, or the request withdrawn."
        ));
        assert!(done.contains("@alice requested a review"));
        assert!(done.contains("If a review is asked of the bot again you will be told here"));
        let merged = review_done_prompt(&pr_issue, &[], &ctx, ReviewEnd::Closed { merged: true });
        assert_eq!(
            merged,
            "[ssf] Pull request o/r#4 \"Fix it\" (https://gh/4) has been merged.\n\nThis review session is over: stop, and do not post anything more."
        );
        let closed = review_done_prompt(&pr_issue, &[], &ctx, ReviewEnd::Closed { merged: false });
        assert!(closed.contains("has been closed."));

        // The author's follow-up says who reviews, only when a request is among the events.
        let fu = followup_prompt(&pr_issue, &[ev.clone()], &ctx);
        assert!(fu.contains(
            "a separate reviewer session (o/r#4:reviewer) reviews it. Do not review it yourself"
        ));
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
        assert!(initial.contains("This worktree is on the pull request's branch `bot/fix`"));
        assert!(initial.contains("Do not merge the pull request; a human does that."));

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
            "The review asked of @bot is not for you: since this session wrote the pull request, a separate reviewer session (o/r#4:reviewer) reviews it"
        ));
        let fu = followup_prompt(&pr_issue, std::slice::from_ref(&other), &ctx);
        assert!(!fu.contains("reviewer session"));
        // ...also when the PR is first bound with the label already on it.
        let t = tracked_prompt(&pr_issue, std::slice::from_ref(&ev), &ctx);
        assert!(t.contains(
            "A review was asked of @bot (the `review` label); since this session wrote the pull request, a separate reviewer session (o/r#4:reviewer) reviews it"
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
            "this session because the `review` label was added on pull request o/r#4, which another agent session of the same bot (o/r#3) wrote"
        ));
        let a = review_again_prompt(&pr_issue, &[], &rctx);
        assert!(a.starts_with(
            "[ssf] Another review is asked of @bot on pull request o/r#4 \"Fix it\" (https://gh/4): the `review` label was added."
        ));
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
        assert!(
            bctx.because()
                .contains("it was given the `review` label, which asks @bot for a review")
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
            "- Bare (https://gh/p/2): this board has no Status field.\n\nKeeping the card's Status accurate is part of the job"
        ));
        assert!(p.contains("ssf never moves cards itself.\n\n## Description"));
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
            pr: None,
            triggers: &[],
            owner: None,
            delegated_by: None,
            projects: &[],
            project_prompt: None,
        };
        let p = initial_prompt(&issue, &[], &ctx);
        assert!(p.contains("Opened by the agent session working on o/r#3."));
        assert!(p.contains("## Description\n\nFixes it\n\n## Activity"));
    }

    #[test]
    fn interrupted_prompt_names_the_session_and_where_it_is() {
        let p = interrupted_prompt(&Interrupted {
            repo: "o/r",
            number: 18,
            title: "Resume sessions",
            url: "https://gh/18",
            branch: Some("refs/heads/bot/issue-18"),
            path: Some("/w/issue-18"),
            reviewer: false,
        });
        assert!(p.starts_with("[ssf] The factory restarted"));
        assert!(p.contains("session for o/r#18 \"Resume sessions\" (https://gh/18)"));
        assert!(p.contains("on branch `bot/issue-18` in `/w/issue-18`"));
        assert!(p.contains("`git status`, `git log`"));
        assert!(p.contains("say so on the item"));

        let r = interrupted_prompt(&Interrupted {
            repo: "o/r",
            number: 25,
            title: "Fix",
            url: "https://gh/25",
            branch: None,
            path: None,
            reviewer: true,
        });
        assert!(r.contains("reviewer session for pull request o/r#25 \"Fix\""));
        assert!(!r.contains("on branch"));
        assert!(r.contains("If your review has not been posted yet"));
    }
}
