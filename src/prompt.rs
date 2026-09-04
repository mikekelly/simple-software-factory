//! Rendering GitHub issue timelines into prompts for the agent.

use serde_json::Value;

use crate::config::{DaemonConfig, RepoConfig};
use crate::github::{Issue, PrInfo, value_str, value_u64};
use crate::origin;

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

/// A post's text without its origin tag, and where the tag says it came
/// from. Only the bot's own posts carry meaningful tags; a human's text is
/// shown as is.
fn body_and_session(body: &str, author: &str, bot: &str) -> (String, String) {
    if !author.eq_ignore_ascii_case(bot) {
        return (body.to_string(), String::new());
    }
    match origin::parse(body) {
        Some(t) => (origin::strip(body), format!(" (from session {})", t.origin)),
        None => (body.to_string(), String::new()),
    }
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
    let text = match kind.as_str() {
        "commented" => {
            let (body, session) =
                body_and_session(value_str(ev, &["body"]).unwrap_or(""), &actor, bot);
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
            let (body, session) =
                body_and_session(value_str(ev, &["body"]).unwrap_or(""), &actor, bot);
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
                let (body, session) =
                    body_and_session(value_str(c, &["body"]).unwrap_or(""), who, bot);
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
                let (body, session) =
                    body_and_session(value_str(c, &["body"]).unwrap_or(""), who, bot);
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
    Some(Rendered { key, text })
}

#[derive(Clone, Copy)]
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
            ..*self
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

pub fn initial_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let mut s = String::new();
    s.push_str(&issue_header(issue, ctx));
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
    let tag = origin::Origin::new(repo, n)
        .map(|o| o.tag())
        .unwrap_or_else(|| format!("<!-- ssf: origin={repo}#{n} -->"));
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
woken up when someone answers.\n\
- Every comment, review and pull request you post must end with the line `{tag}` so ssf can tell \
which session posted it (GitHub shows the same bot for every session). The `gh` on this PATH \
adds it for you on `issue create|comment` and `pr create|comment|review` when you pass `--body` \
or `--body-file`; add it yourself when you post any other way (`gh api`, `gh pr create --fill`, \
`gh pr edit --body`, ...).\n\
- Other agent sessions may be working on this repository at the same time. `ssf peers` lists them \
(item, GitHub state, agent state, branch, last message; `--json` for detail). Leave their branches \
and workspaces alone.\n\
- Issues and pull requests you open stay with you: ssf recognises the tag and delivers their \
activity (comments, reviews, review requests, assignments, closure) here instead of starting \
another session, and `SSF_ISSUE` stays {n}. To hand a piece of work to a separate agent instead, \
create the issue (or PR) with `--assignee {bot}` in the same `gh ... create` command; the tag then \
carries `mode=delegate`, the item gets a session of its own, and you hear nothing more about it \
until it closes, when you get one message with its final comment. Assigning @{bot} to an existing \
item you did not open gives it a fresh session too.\n"
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
    if let Some(parent) = ctx.delegated_by {
        s.push_str(&format!(
            "- This {kind} was handed off to you by the agent session working on {parent}. Work on it \
independently; that session is not watching it and will only be told, once, when it is closed, \
along with your final comment, so make that comment a clear summary of the outcome (what was \
done, the PR link, anything left open).\n"
        ));
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
    if !human.is_empty() {
        s.push_str(&format!(
            " It reached ssf because {}; that is for you to act on.",
            ctx.because_of(&human)
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
    match ctx.owner {
        Some(owner) => s.push_str(&format!(
            "\nYou will not receive further updates for this {kind}. Your own item, {}#{owner}, is \
unaffected; carry on with it.",
            ctx.repo.name
        )),
        None => s.push_str(&format!(
            "\nStop working on this {kind}. If you have uncommitted work worth keeping, commit it now \
and leave a short final comment on the {kind}. You will not receive further updates for it."
        )),
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
                Some(o) => format!("@{} (from session {o})", c.author),
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
    s.push_str(
        "\nThis is the only message you will get about it. Take the outcome into account for your \
own work; nothing else is expected of you unless you disagree with it.",
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
    match ctx.owner {
        Some(owner) => s.push_str(&format!(
            "\nYou will not receive further updates for it unless it is brought back in. Your own \
item, {}#{owner}, is unaffected.",
            ctx.repo.name
        )),
        None => s.push_str(
            "\nStop working on this. Commit anything worth keeping and leave a short final comment \
summarising where things stand. You will not receive further updates unless you are brought back in.",
        ),
    }
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
        };
        let p = initial_prompt(&issue, &[], &ctx);
        assert!(p.contains("o/r#3: Add thing"));
        assert!(p.contains("Labels: feature"));
        assert!(p.contains("(no activity yet)"));
        assert!(p.contains("Closes #3"));
        assert!(p.contains("it was assigned to @bot"));
        assert!(p.contains("GH_TOKEN"));
        assert!(p.contains("`<!-- ssf: origin=o/r#3 -->`"));
        assert!(p.contains("`ssf peers` lists them"));
        assert!(p.contains("create the issue (or PR) with `--assignee bot`"));
        assert!(!p.contains("handed off to you"));
        assert!(p.trim_end().ends_with("Run the tests."));

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
    }

    #[test]
    fn owned_items_get_tracking_and_closing_notes() {
        let pr_issue: Issue = serde_json::from_value(json!({
            "number": 4, "title": "Fix it", "body": "Fixes it\n\n<!-- ssf: origin=o/r#3 -->", "html_url": "https://gh/4",
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
        };
        let ev = Rendered {
            key: "k".into(),
            text: "- [t] @alice requested a review from @bot".into(),
        };
        let p = tracked_prompt(&pr_issue, &[ev], &ctx);
        assert!(p.starts_with(
            "[ssf] Now tracking pull request o/r#4 \"Fix it\" (https://gh/4) for this session, because this session opened it."
        ));
        assert!(p.contains("because a review was requested from @bot; that is for you to act on"));
        assert!(p.contains("@alice requested a review"));
        assert!(p.contains("gh pr comment 4 --repo o/r"));
        assert!(!p.contains("## How to work on this"));

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
        assert!(m.contains("Final comment by @bot (from session o/r#4) (https://gh/4#c1):\n  > Done, see PR #5."));
        let m = delegated_closed_prompt(&pr_issue, false, None, &ctx);
        assert!(m.contains("has been closed, completed."));
        assert!(m.contains("It has no comments."));
    }

    #[test]
    fn tags_are_stripped_from_bodies_and_shown_as_sessions() {
        let ev = json!({"event":"commented","id":1,"user":{"login":"bot"},"created_at":"t",
            "body":"done\n\n<!-- ssf: origin=o/r#9 -->","html_url":"https://x/1"});
        let r = render_event(&ev, false, &cfg(), "bot").unwrap();
        assert!(
            r.text
                .contains("@bot commented (from session o/r#9) (https://x/1):\n  > done")
        );
        assert!(!r.text.contains("<!--"));
        let review = json!({"event":"reviewed","id":2,"user":{"login":"bot"},"state":"approved",
            "body":"<!-- ssf: origin=o/r#9 -->"});
        let r = render_event(&review, false, &cfg(), "bot").unwrap();
        assert!(r.text.ends_with("reviewed (approved) (from session o/r#9)"));
        // A human quoting a bot comment is not "from a session".
        let human = json!({"event":"commented","id":5,"user":{"login":"alice"},"created_at":"t",
            "body":"> <!-- ssf: origin=o/r#9 -->\n\nthanks","html_url":"https://x/5"});
        let r = render_event(&human, false, &cfg(), "bot").unwrap();
        assert!(r.text.contains("@alice commented (https://x/5)"));
        assert!(
            r.text.contains("<!-- ssf: origin=o/r#9 -->"),
            "quoted text is shown verbatim"
        );

        let issue: Issue = serde_json::from_value(json!({
            "number": 4, "title": "PR", "body": "Fixes it\n\n<!-- ssf: origin=o/r#3 -->", "html_url": "https://gh/4",
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
        };
        let p = initial_prompt(&issue, &[], &ctx);
        assert!(p.contains("Opened by the agent session working on o/r#3."));
        assert!(p.contains("## Description\n\nFixes it\n\n## Activity"));
    }
}
