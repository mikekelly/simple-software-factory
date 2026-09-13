//! Rendering GitHub issue timelines into prompts for the agent.

#[cfg(test)]
use serde_json::Value;
use std::path::Path;
use tracing::warn;

use crate::config::{DaemonConfig, DriverKind, RepoConfig};
use crate::github::{Issue, PrInfo, ProjectCard};
use crate::origin;

mod guide;
mod timeline;
pub use guide::{VM_GUEST_LINE, guide};
#[cfg(test)]
use timeline::today_utc;
pub use timeline::{Rendered, actor_of, event_key, render_event};
use timeline::{fmt_when, quote};

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
    /// The item was handed over (`ssf handover`) by the session that had
    /// it: the display name of the harness that session ran. Set only on
    /// the first message the new session gets.
    pub handed_over_from: Option<&'a str>,
    /// Open project boards the item is on.
    pub projects: &'a [ProjectCard],
    /// The repository's SSF agent guidance, when the worktree has it.
    pub project_prompt: Option<ProjectPrompt>,
    /// Additional notes for the harness actually running this session.
    pub harness_prompt: Option<ProjectPrompt>,
    /// The factory runs inside its own VM, where the agent has root.
    pub vm_guest: bool,
    /// Who `git push` acts as when `[git].credential` names someone other
    /// than the bot (`Credential::prompt_pusher`); `None` is the bot.
    pub pushes_as: Option<String>,
}

/// Contents of the SSF agent guidance file (`SSF.md` by default): the operating
/// contract humans on a repository give the issue-owning main session.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectPrompt {
    /// The file as configured (`SSF.md`, `.ssf/prompt.md`, `~/notes/x.md`).
    pub source: String,
    pub text: String,
}

impl ProjectPrompt {
    /// Read the repository's SSF agent guidance from the checkout at `worktree`.
    /// A missing or empty file yields nothing; an unreadable one is logged.
    pub fn load(repo: &RepoConfig, worktree: &Path) -> Option<Self> {
        Self::load_file(repo, &repo.prompt_file_path(worktree), repo.prompt_file())
    }

    /// Harness notes always live at the checkout root, independently of
    /// the shared SSF guidance file configured by the repository.
    pub fn load_harness(repo: &RepoConfig, worktree: &Path, harness: &str) -> Option<Self> {
        let source = format!("SSF.{harness}.md");
        Self::load_file(repo, &worktree.join(&source), &source)
    }

    fn load_file(repo: &RepoConfig, path: &Path, source: &str) -> Option<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                warn!(
                    repo = repo.name,
                    path = %path.display(),
                    "cannot read the SSF agent guidance file: {e}"
                );
                return None;
            }
        };
        let (text, unclosed) = without_html_comments(&text);
        if let Some(line) = unclosed {
            warn!(
                repo = repo.name,
                path = %path.display(),
                line,
                "the SSF agent guidance opens an HTML comment that never closes; everything after it is left out of the prompt"
            );
        }
        if text.is_empty() {
            return None;
        }
        Some(Self {
            source: source.to_string(),
            text,
        })
    }
}

/// `text` without its HTML comments (`<!-- ... -->`), trimmed, with the
/// blank runs a removed comment leaves behind collapsed: the comments in
/// a notes file are for the person editing it (`SSF.example.md` explains
/// itself in one, and names the other end of its autonomy line and where
/// to write the models its delegation line asks for in two more), and
/// read as instructions if they reach the agent. A comment
/// that never closes runs to the end, as in HTML; the line it opens on
/// comes back with the text so the caller can say so.
fn without_html_comments(text: &str) -> (String, Option<usize>) {
    // Each comment becomes one marker, so a line that held nothing but a
    // comment can be told from a blank line the author wrote.
    const MARK: char = '\u{0}';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut unclosed = None;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        out.push(MARK);
        match rest[start + 4..].find("-->") {
            Some(end) => rest = &rest[start + 4 + end + 3..],
            None => {
                let consumed = text.len() - rest.len() + start;
                unclosed = Some(text[..consumed].lines().count().max(1));
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    let mut lines: Vec<String> = Vec::new();
    for line in out.lines() {
        let had_comment = line.contains(MARK);
        let line = line.replace(MARK, "");
        let line = line.trim_end();
        if line.trim().is_empty() {
            if had_comment || lines.last().is_some_and(|l| l.is_empty()) {
                continue;
            }
            lines.push(String::new());
        } else {
            lines.push(line.to_string());
        }
    }
    (lines.join("\n").trim().to_string(), unclosed)
}

impl PromptContext<'_> {
    pub fn kind(&self) -> &'static str {
        if self.pr.is_some() {
            "pull request"
        } else {
            "issue"
        }
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
        match self.handed_over_from {
            Some(h) => {
                format!("the agent session on {h} working on it handed #{number} over to you")
            }
            None => self.because_for(&format!("#{number}")),
        }
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

    /// The item was opened by the session it is bound to: the bot's own
    /// item, whose origin tag names the owner.
    fn creator_owned(&self, issue: &Issue) -> bool {
        let by_bot = issue.author().eq_ignore_ascii_case(self.bot_login);
        issue
            .body
            .as_deref()
            .and_then(origin::parse)
            .filter(|_| by_bot)
            .is_some_and(|t| Some(t.origin.number) == self.owner)
    }

    /// How the item came to be routed to another session's agent.
    fn owned_because(&self, issue: &Issue) -> String {
        if self.creator_owned(issue) {
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
/// set to. Which column fits is the agent's call, so nothing here says
/// beyond the one rule about cards.
fn project_boards(ctx: &PromptContext) -> String {
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
    s.push_str("\nKeep the card's Status accurate; which column fits is your call.\n");
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
    s.push_str(&project_boards(ctx));
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
    // Pushes go out as the bot unless `[git].credential` says someone
    // else; the agent needs no other behaviour, but a refused push then
    // names that account.
    let (acts_as, only) = match &ctx.pushes_as {
        None => (
            format!("`gh` and `git push` already act as @{bot}"),
            format!("as @{bot}"),
        ),
        Some(who) => (
            format!("`gh` already acts as @{bot} and `git push` as {who}"),
            "through those".to_string(),
        ),
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
- {acts_as}, and the `gh` on your PATH marks your posts as this \
session's. Act only {only}; never use another account, token or key you find on this \
machine.\n",
        ctx.spawned_because(n)
    );
    if ctx.vm_guest {
        s.push_str(&format!("- {VM_GUEST_LINE}\n"));
    }
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
    s.push_str(&extras(ctx));
    s
}

/// The operator's and the repository's own instructions, after ssf's.
fn extras(ctx: &PromptContext) -> String {
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
        s.push_str(&format!("\n## SSF agent guidance (`{}`)\n\n", pp.source));
        s.push_str(&pp.text);
        s.push('\n');
    }
    if let Some(pp) = ctx.harness_prompt.as_ref() {
        s.push_str(&format!("\n## Harness guidance (`{}`)\n\n", pp.source));
        s.push_str(&pp.text);
        s.push('\n');
    }
    s
}

pub fn followup_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let head = format!("[ssf] New activity on {}:", short_ref(issue, ctx));
    let tail = owned_tail(issue, events, ctx).unwrap_or_default();
    assemble(&head, events, &tail)
}

/// An assignment arriving on an item the session filed itself reads like
/// bookkeeping ("assigned @bot") unless the consequence is said: the item
/// is that session's to work on, and no other session is started for it.
fn owned_tail(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> Option<String> {
    if !ctx.creator_owned(issue) {
        return None;
    }
    let bot = ctx.bot_login;
    if !events.iter().any(|e| e.assigns(bot)) {
        return None;
    }
    Some(format!(
        "#{} is now assigned to @{bot}. You filed it, so it is yours: work on it in this \
workspace; nobody else is spawned for it.",
        issue.number
    ))
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

/// Tell a live session that its committed branch cannot merge cleanly into
/// the current base. This is advisory: the daemon never changes the
/// worktree, index or branch for the agent.
pub fn conflict_prompt(base_ref: &str, base_sha: &str, files: &[String]) -> String {
    let mut s =
        format!("[ssf] Your branch conflicts with {base_ref} at base commit `{base_sha}`.\n\n");
    if files.is_empty() {
        s.push_str("Git reported a merge conflict, but did not name the affected files.\n\n");
    } else {
        s.push_str("Conflicting files:\n");
        for file in files {
            s.push_str(&format!("- `{file}`\n"));
        }
        s.push('\n');
    }
    s.push_str(
        "If your final round has started, rebase your branch onto the base, resolve the conflict, ",
    );
    s.push_str("and re-run the round. If the round has not started, do nothing now; resolve the conflict before starting it.");
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
    s.push_str(&format!(
        "This is the session for {item}{branch}{path}.\n\nWork out where you got to (`git \
status`, `git log`, your last comments on the item) and carry on from there. Anything that \
happened on the item while you were away arrives as further `[ssf]` messages. If you were \
part-way through something and cannot tell what is left, say so on the item: that the session \
was interrupted and what remains."
    ));
    s
}

pub struct LoginBack<'a> {
    /// The harness's display name (`Claude Code`).
    pub harness: &'a str,
    /// When the login prompt was first seen.
    pub since: &'a str,
    pub number: u64,
    pub title: &'a str,
    pub url: &'a str,
}

/// The one message a session gets after its harness sat at a login prompt
/// (an expired or revoked login) and has been started again now that the
/// login is back. A resumed harness has its memory; a fresh one gets the
/// item's story ahead of this. Worded, like every text ssf puts on a
/// screen, without the phrases `driver::login_dialog` looks for.
pub fn login_back_prompt(it: &LoginBack) -> String {
    let item = format!("#{} \"{}\" ({})", it.number, it.title, it.url);
    let what = format!("the session for {item}");
    format!(
        "[ssf] Your {} sign-in lapsed at {} and is back: this terminal was started again with \
your conversation resumed. This is {what}.\n\nNothing you sent while it was lapsed reached \
anyone, and no `[ssf]` message reached you; what happened on the item meanwhile follows as \
further `[ssf]` messages. Work out where you got to (`git status`, `git log`, your last \
comments) and carry on.",
        it.harness, it.since
    )
}

/// The one message a session gets after its harness could not be started
/// at all (a handover to a harness that exits as it is launched, say) and
/// has now been started again. A fresh harness gets the item's story
/// ahead of this. Worded, like every text ssf puts on a screen, without
/// the phrases `driver::login_dialog` looks for.
pub fn start_again_prompt(it: &LoginBack) -> String {
    let item = format!("#{} \"{}\" ({})", it.number, it.title, it.url);
    format!(
        "[ssf] Your {} terminal could not be started at {} and has been started again. This is \
the session for {item}.\n\nNothing reached you while it was down; what happened on the item \
meanwhile follows as further `[ssf]` messages. Work out where the work got to (`git status`, \
`git log`, the comments on the item) and carry on.",
        it.harness, it.since
    )
}

/// What a session started by a handover (`ssf handover`) is told ahead of
/// the item's own story: what happened, and the outgoing agent's summary
/// when it left one. `from` is the display name of the harness the
/// outgoing session ran, `kind` the item's word (`issue`, `pull
/// request`). The summary is the outgoing agent's own text and is passed
/// through unchanged. Kept apart from the story because the item holds on
/// to it until a session has read it: a start that fails is tried again
/// later, and the words the outgoing agent left go with that attempt.
pub fn handover_note(from: &str, kind: &str, summary: Option<&str>) -> String {
    match summary {
        Some(text) => format!(
            "You took over this {kind} from a session on {from} that handed it over; its summary \
follows, then the {kind} as ssf tells it to a new session.\n\n\
## Summary from the outgoing session\n\n{}",
            text.trim()
        ),
        None => format!(
            "You took over this {kind} from a session on {from} that handed it over. It left no \
summary; read the {kind} below."
        ),
    }
}

/// The first message of a session started by a handover: the note above,
/// then the item's story exactly as a new session gets it.
pub fn handover_prompt(from: &str, kind: &str, summary: Option<&str>, story: &str) -> String {
    format!("{}\n\n{story}", handover_note(from, kind, summary))
}

/// The one message the outgoing agent gets when a handover it asked for
/// cannot be carried out: it is still the session on the item.
pub fn handover_refused_prompt(harness: &str, reason: &str) -> String {
    format!("[ssf] Handover to {harness} refused: {reason}. Carry on.")
}

/// The one message the agent gets when a handover on its item is called
/// off (`ssf handover --cancel`): it was told to stop working, and this
/// is what takes that back.
pub fn handover_cancelled_prompt(harness: &str) -> String {
    format!("[ssf] The handover to {harness} was cancelled: this session keeps the item. Carry on.")
}

pub fn unassigned_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let bot = ctx.bot_login;
    let item = short_ref(issue, ctx);
    let head = if ctx.triggers.iter().any(|t| t == "review_requested")
        && !ctx.triggers.iter().any(|t| t == "assigned")
    {
        format!("[ssf] The review request for @{bot} on {item} has been fulfilled or withdrawn.")
    } else if ctx.triggers.iter().any(|t| t == "mentioned")
        && !ctx.triggers.iter().any(|t| t == "assigned")
    {
        // The login is written without its `@`: an agent that quotes this
        // line back into a comment would otherwise mention the bot on the
        // item and start the session it was just told to end.
        format!("[ssf] The mention of {bot} that started this session on {item} is gone.")
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
mod tests;
