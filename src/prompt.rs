//! Rendering GitHub issue timelines into prompts for the agent.

use serde_json::Value;

use crate::config::{DaemonConfig, RepoConfig};
use crate::github::{Issue, PrInfo, value_str, value_u64};

/// A timeline event that should be shown to the agent.
#[derive(Debug, Clone)]
pub struct Rendered {
    pub key: String,
    pub text: String,
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

/// Render one timeline event, or `None` if it is not worth showing.
pub fn render_event(ev: &Value, edited: bool, cfg: &DaemonConfig) -> Option<Rendered> {
    let kind = value_str(ev, &["event"])?.to_string();
    if cfg.ignored_events.iter().any(|k| k == &kind) {
        return None;
    }
    let key = event_key(ev)?;
    let actor = actor_of(ev);
    let at = when(ev);
    let head = |what: &str| format!("- [{at}] @{actor} {what}");
    let text = match kind.as_str() {
        "commented" => {
            let body = value_str(ev, &["body"]).unwrap_or("");
            let url = value_str(ev, &["html_url"]).unwrap_or("");
            let verb = if edited {
                "edited their comment"
            } else {
                "commented"
            };
            format!(
                "{} ({url}):\n{}",
                head(verb),
                quote(body, cfg.max_body_chars)
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
            let body = value_str(ev, &["body"]).unwrap_or("");
            let mut s = head(&format!("reviewed ({state})"));
            if !body.trim().is_empty() {
                s.push_str(":\n");
                s.push_str(&quote(body, cfg.max_body_chars));
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
                let body = value_str(c, &["body"]).unwrap_or("");
                let url = value_str(c, &["html_url"]).unwrap_or("");
                let at = value_str(c, &["created_at"]).unwrap_or(&at);
                out.push(format!(
                    "- [{at}] @{who} commented on `{path}`{} ({url}):\n{}",
                    line.map(|l| format!(" line {l}")).unwrap_or_default(),
                    quote(body, cfg.max_body_chars)
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
                let body = value_str(c, &["body"]).unwrap_or("");
                out.push(format!(
                    "- [{at}] @{who} commented on commit {}:\n{}",
                    short(sha),
                    quote(body, cfg.max_body_chars)
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
    Some(Rendered { key, text })
}

pub struct PromptContext<'a> {
    pub repo: &'a RepoConfig,
    pub daemon: &'a DaemonConfig,
    pub bot_login: &'a str,
    /// Set when the item is a pull request.
    pub pr: Option<&'a PrInfo>,
    /// Why the bot is involved: assigned, mentioned, review_requested.
    pub triggers: &'a [String],
}

impl PromptContext<'_> {
    pub fn kind(&self) -> &'static str {
        if self.pr.is_some() {
            "pull request"
        } else {
            "issue"
        }
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
                other => other.to_string(),
            })
            .collect();
        if parts.is_empty() {
            format!("it was assigned to @{bot}")
        } else {
            parts.join(" and ")
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
    s
}

pub fn initial_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let mut s = String::new();
    s.push_str(&issue_header(issue, ctx));
    s.push_str("\n\n## Description\n\n");
    let body = issue.body.as_deref().unwrap_or("").trim();
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
    let mut s = format!(
        "\n## How to work on this\n\n\
You are the coding agent for the GitHub bot account @{bot}. This workspace was created by \
Simple Software Factory (ssf) because {because}. Work on the {kind} in this worktree, on its \
branch; commit as you go. New activity on the {kind} (comments, reviews, label changes, closure) \
will be delivered to you here as further messages prefixed with `[ssf]`, so re-read them and \
adjust course when they arrive.\n\n\
- Talk to the humans through the {kind}, as the bot account. The bot's GitHub credentials are \
already in your environment (`GH_TOKEN`, `GITHUB_TOKEN`, and a git credential helper for HTTPS), so \
plain `gh` commands and `git push` act as @{bot}, and commits are authored and signed as @{bot} \
automatically. `SSF_REPO` and `SSF_ISSUE` name this {kind}. Only ever act as @{bot}: never use \
another GitHub account, token or key you find on this machine, even if @{bot} lacks a permission; \
say so on the {kind} instead. Post a short comment when you start, when you need a decision, and \
when you finish.\n\
- Ask questions on the {kind} rather than guessing when the request is ambiguous; you will be \
woken up when someone answers.\n"
    );
    match ctx.pr {
        Some(pr) if pr.same_repo(repo) => s.push_str(&format!(
            "- This worktree is checked out on the pull request's branch `{}`. To change the PR, commit here and \
`git push` that branch; the PR updates itself. Reply to review comments and questions with \
`gh pr comment {n} --repo {repo} --body \"...\"` (or `gh api` for inline replies). If you were asked \
to review rather than to change anything, review with `gh pr review {n} --repo {repo}` \
(--comment, --approve or --request-changes) and be specific.\n\
- Do not merge the pull request; a human does that.\n",
            pr.head_ref
        )),
        Some(pr) => s.push_str(&format!(
            "- This pull request comes from a fork ({}), so you cannot push to its branch. Review it, answer \
questions with `gh pr comment {n} --repo {repo} --body \"...\"`, and if changes are needed describe \
them in a review (`gh pr review {n} --repo {repo} --request-changes --body \"...\"`) or open a \
separate PR from this worktree against `{}`.\n\
- Do not merge the pull request; a human does that.\n",
            pr.head_repo, pr.base_ref
        )),
        None => s.push_str(&format!(
            "- When the work is done, push the branch and open a pull request that references the issue \
(`Closes #{n}`), then comment on the issue with the PR link.\n\
- Do not close the issue yourself; a human reviews the PR.\n"
        )),
    }
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
    s
}

pub fn followup_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let mut s = format!(
        "[ssf] New activity on {}#{} \"{}\" ({}):\n\n",
        ctx.repo.name, issue.number, issue.title, issue.html_url
    );
    for e in events {
        s.push_str(&e.text);
        s.push('\n');
    }
    s.push_str(
        "\nTake this into account. If it changes what you should do, adjust now; if you had \
finished, address it and report back on the issue as before.",
    );
    s
}

pub fn closed_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let mut s = format!(
        "[ssf] {}#{} \"{}\" has been closed ({}).\n\n",
        ctx.repo.name,
        issue.number,
        issue.title,
        issue.state_reason.as_deref().unwrap_or("no reason given")
    );
    for e in events {
        s.push_str(&e.text);
        s.push('\n');
    }
    s.push_str(
        "\nStop working on this issue. If you have uncommitted work worth keeping, commit it now \
and leave a short final comment on the issue. You will not receive further updates for it.",
    );
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
    let mut s = format!("[ssf] {what}.\n\n");
    for e in events {
        s.push_str(&e.text);
        s.push('\n');
    }
    s.push_str(
        "\nStop working on this. Commit anything worth keeping and leave a short final comment \
summarising where things stand. You will not receive further updates unless you are brought back in.",
    );
    s
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

/// Short, filesystem-safe name for the worktree.
pub fn worktree_name_for(number: u64, title: &str, is_pr: bool) -> String {
    let base = worktree_name(number, title);
    if is_pr {
        base.replacen("issue-", "pr-", 1)
    } else {
        base
    }
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
        let r = render_event(&ev, false, &cfg()).unwrap();
        assert!(r.text.contains("@alice commented"));
        assert!(r.text.contains("  > hello\n  > world"));
        let edited = render_event(&ev, true, &cfg()).unwrap();
        assert!(edited.text.contains("edited their comment"));
        let noise = json!({"event":"subscribed","id":2,"actor":{"login":"bob"}});
        assert!(render_event(&noise, false, &cfg()).is_none());
    }

    #[test]
    fn truncates_long_bodies() {
        let mut c = cfg();
        c.max_body_chars = 5;
        let ev = json!({"event":"commented","id":1,"user":{"login":"a"},"body":"0123456789"});
        let r = render_event(&ev, false, &c).unwrap();
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
            command: None,
            clone_url: None,
            path: None,
            base_branch: None,
            instructions: Some("Run the tests.".into()),
        };
        let d = cfg();
        let ctx = PromptContext {
            repo: &repo,
            daemon: &d,
            bot_login: "bot",
            pr: None,
            triggers: &[],
        };
        let p = initial_prompt(&issue, &[], &ctx);
        assert!(p.contains("o/r#3: Add thing"));
        assert!(p.contains("Labels: feature"));
        assert!(p.contains("(no activity yet)"));
        assert!(p.contains("gh issue comment 3 --repo o/r"));
        assert!(p.contains("GH_TOKEN"));
        assert!(p.trim_end().ends_with("Run the tests."));
    }
}
