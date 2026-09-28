//! Rendering GitHub issue timelines into prompts for the agent.

#[cfg(test)]
use serde_json::Value;
use std::path::Path;
use tracing::warn;

use crate::config::{DaemonConfig, DriverKind, RepoConfig};
use crate::github::{Issue, PrInfo, ProjectCard};
use crate::origin;

#[cfg(test)]
pub mod fixtures;
mod guide;
mod timeline;
pub use guide::{VM_GUEST_LINE, guide};
#[cfg(test)]
pub use timeline::state_change;
#[cfg(test)]
use timeline::today_utc;
pub use timeline::{Rendered, actor_of, event_key, render_event, render_reaction};
use timeline::{fmt_when, quote, quote_lines};

#[cfg(not(test))]
fn global_prompt_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|home| home.join(".ssf"))
}

#[cfg(test)]
fn global_prompt_dir() -> Option<std::path::PathBuf> {
    crate::config::test_support::optional_home().map(|home| home.join(".ssf"))
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
    /// The item was handed over (`ssf handover`) by the session that had
    /// it: the display name of the harness that session ran. Set only on
    /// the first message the new session gets.
    pub handed_over_from: Option<&'a str>,
    /// Open project boards the item is on.
    pub projects: &'a [ProjectCard],
    /// Machine-wide SSF agent guidance, when `~/.ssf/SSF.md` exists.
    pub global_prompt: Option<ProjectPrompt>,
    /// Machine-wide notes for the harness actually running this session.
    pub global_harness_prompt: Option<ProjectPrompt>,
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

/// Contents of an SSF guidance file for the issue-owning main session.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectPrompt {
    /// The file as configured (`SSF.md`, `.ssf/prompt.md`, `~/notes/x.md`).
    pub source: String,
    pub text: String,
}

impl ProjectPrompt {
    /// Read optional machine-wide SSF guidance from `~/.ssf`.
    pub fn load_global(repo: &RepoConfig) -> Option<Self> {
        Self::load_global_from(repo, &global_prompt_dir()?, "SSF.md")
    }

    /// Read optional machine-wide guidance for the selected harness.
    pub fn load_global_harness(repo: &RepoConfig, harness: &str) -> Option<Self> {
        let filename = format!("SSF.{harness}.md");
        Self::load_global_from(repo, &global_prompt_dir()?, &filename)
    }

    fn load_global_from(repo: &RepoConfig, directory: &Path, filename: &str) -> Option<Self> {
        let source = format!("~/.ssf/{filename}");
        Self::load_file(repo, &directory.join(filename), &source)
    }

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

/// The header of a first message's `<issue>` or `<pull-request>` section:
/// the item named once with its URL, and the facts that the rest of the
/// message does not repeat.
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
            "GitHub pull request #{}: {}\n{}\n\nBranch `{}` into `{}`{}{}. Opened by @{}{session} on {opened}.",
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
            "GitHub issue #{}: {}\n{}\n\nOpened by @{}{session} on {opened}.",
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
    let mut s = String::new();
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
    s.push_str("\nKeep the card's Status accurate; which column fits is your call.");
    format!("\n\n{}", section("project-boards", &s))
}

/// The tag around a first message's replay of the item's timeline.
const HISTORY: &str = "history";
/// The tag around the events a later message delivers.
const NEW_ACTIVITY: &str = "new-activity";

/// `body` as a section of a message: `<tag>` and `</tag>` on lines of their
/// own around it. Every part of a message after its `[ssf]` lead line is
/// one of these, so where each part starts and ends cannot blur.
fn section(tag: &str, body: &str) -> String {
    format!("<{tag}>\n{}\n</{tag}>", body.trim_end())
}

/// `body` as a section named after the file it comes from: the only
/// attribute a section carries, since the file is not in its content.
fn file_section(tag: &str, file: &str, body: &str) -> String {
    let file: String = file
        .chars()
        .filter(|c| !matches!(c, '"' | '<' | '>' | '&') && !c.is_control())
        .collect();
    format!("<{tag} file=\"{file}\">\n{}\n</{tag}>", body.trim_end())
}

/// ssf's own words to the agent, as the section that follows a lead line.
fn instructions_section(body: &str) -> String {
    section(SSF_INSTRUCTIONS, body)
}

/// The tag around what ssf tells the agent to do.
const SSF_INSTRUCTIONS: &str = "ssf-instructions";

/// One relayed GitHub event, delimited so that where ssf's words end and
/// the item's begin cannot blur: `<event>`, the rendered lines, which
/// already name the kind, actor and time, `</event>`.
fn event_block(e: &Rendered) -> String {
    section("event", &e.text)
}

/// `events` as a `<tag>` block of `event_block`s, `lead` (ssf's words about
/// them, if any) first. No events reads `empty`.
fn events_section(tag: &str, lead: &str, events: &[Rendered], empty: &str) -> String {
    let mut s = lead.to_string();
    if events.is_empty() {
        s.push_str(empty);
        s.push('\n');
    }
    for e in events {
        s.push_str(&event_block(e));
        s.push('\n');
    }
    section(tag, &s)
}

/// `text` (a message: its `[ssf]` lead line, then its sections) with
/// `extra` as the first section after the lead line, so the message keeps
/// its one `[ssf]` marker.
fn with_section_after_lead(text: &str, extra: &str) -> String {
    match text.split_once('\n') {
        Some((lead, rest)) => format!("{lead}\n\n{extra}\n\n{}", rest.trim_start_matches('\n')),
        None => format!("{text}\n\n{extra}"),
    }
}

/// Two messages delivered as one (a fresh harness's story, then the
/// message that prompted the restart): `first` unchanged, then `second`
/// in a `<next-message>` section, its lead line without the `[ssf]`
/// marker, so the delivery keeps the one lead line every message has.
pub fn then(first: &str, second: &str) -> String {
    let second = second.trim();
    let second = second.strip_prefix("[ssf]").map_or(second, str::trim_start);
    format!(
        "{}\n\n{}\n",
        first.trim_end(),
        section("next-message", second)
    )
}

/// A delivery with the note ssf adds when it re-created the session's
/// workspace (a branch kept ahead of origin, say): a `<workspace-note>`
/// section after the message's lead line.
pub fn with_workspace_note(text: &str, note: &str) -> String {
    with_section_after_lead(text, &section("workspace-note", note))
}

/// What a first message says above the replayed history: that all of it
/// predates this session, that the bot's posts in it are earlier
/// sessions', and -- when the item was reopened or handed over -- that
/// work may already exist. `events` is the whole timeline, not only the
/// part shown.
fn history_lead(issue: &Issue, events: &[Rendered], ctx: &PromptContext, any: bool) -> String {
    let bot = ctx.bot_login;
    let mut s = String::new();
    if any {
        s.push_str(&format!(
            "Everything below happened before this session was spawned. Posts by @{bot} here \
were made by earlier sessions, not by you: their plans and promises are context, not your \
commitments. Act on the latest request.\n"
        ));
    }
    let n = issue.number;
    if let Some(from) = ctx.handed_over_from {
        s.push_str(&format!(
            "A previous session on {from} worked on #{n} and handed it over to you: check its \
branch, pull request and last comments before starting over.\n"
        ));
    }
    // The rendered event gives the time; the item's own state_reason
    // still tells of a reopen when `reopened` is in `ignored_events`.
    let reopened = events.iter().rev().find(|e| e.key.starts_with("reopened:"));
    if reopened.is_some() || issue.state_reason.as_deref() == Some("reopened") {
        let at = reopened
            .and_then(|e| e.at.as_deref())
            .map(|a| format!(" (last reopened {a})"))
            .unwrap_or_default();
        s.push_str(&format!(
            "#{n} was closed and reopened{at}: a previous session may have worked on it -- check \
for its branch and pull request before starting over.\n"
        ));
    }
    s
}

/// A message: `head` (its `[ssf]` lead line), then the events in a
/// `<new-activity>` section (when there are any), then `tail` in an
/// `<ssf-instructions>` section (when there is one).
fn assemble(head: &str, events: &[Rendered], tail: &str) -> String {
    let mut s = head.to_string();
    if !events.is_empty() {
        s.push_str("\n\n");
        s.push_str(&events_section(NEW_ACTIVITY, "", events, ""));
    }
    if !tail.is_empty() {
        s.push_str("\n\n");
        s.push_str(&instructions_section(tail));
    }
    s
}

/// The first message: ssf's own prompt and the guidance that goes with it
/// (the operator's, the global and repository guidance files, the
/// harness's), then the item itself -- header, boards, description,
/// activity. What ssf is telling the agent to do comes before the material
/// it applies to.
pub fn initial_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let mut s = instructions(issue, ctx);
    let mut item = issue_header(issue, ctx);
    item.push_str(&project_boards(ctx));
    let body = origin::strip(issue.body.as_deref().unwrap_or(""));
    let body = body.trim();
    let description = if body.is_empty() {
        "(no description)".to_string()
    } else {
        // The description is the item author's text, quoted the way a
        // comment body is: the sign-in detector reads a line carrying
        // the `> ` marker as relayed words, and the first prompt now
        // ends with the item, so the description sits where a harness
        // draws its own dialog (#372).
        quote_lines(body)
    };
    item.push_str("\n\n");
    item.push_str(&section("description", &description));
    let tag = if ctx.pr.is_some() {
        "pull-request"
    } else {
        "issue"
    };
    s.push_str("\n\n");
    s.push_str(&section(tag, &item));
    let (shown, omitted) = first_prompt_events(events, ctx);
    // The handover and reopen callouts hold even with no events shown.
    let mut lead = history_lead(issue, events, ctx, !shown.is_empty());
    if !lead.is_empty() {
        lead = format!("{}\n", section("note", &lead));
    }
    if let Some(note) = omitted_notice(issue, ctx, shown.len(), omitted) {
        lead.push_str(&note);
        lead.push('\n');
    }
    s.push_str("\n\n");
    s.push_str(&events_section(HISTORY, &lead, shown, "(no activity yet)"));
    s.push('\n');
    s
}

/// The events the first message about an item carries, and how many of the
/// item's earlier ones that leaves out: the newest, up to the repository's
/// event count, and of those only as many as fit its character budget. The
/// newest is kept whatever its size -- an update aggregated out of many
/// bodies can pass the budget on its own, and an activity section with
/// nothing in it reads as "nothing happened". A cap of zero means no limit.
///
/// Used for the first message of a fresh session (`initial_prompt`) and
/// for the first message a live session gets about an item bound to it
/// (`tracked_prompt`): both are assembled against an empty `seen` map, so
/// both would otherwise carry the item's whole timeline. Follow-up
/// messages carry a delta by construction and are left alone.
fn first_prompt_events<'a>(events: &'a [Rendered], ctx: &PromptContext) -> (&'a [Rendered], usize) {
    let (max_events, max_chars) = ctx.repo.first_prompt_caps(ctx.daemon);
    let mut from = 0;
    if max_events > 0 && events.len() > max_events {
        from = events.len() - max_events;
    }
    if max_chars > 0 {
        let mut used = 0;
        let mut keep = 0;
        for e in events[from..].iter().rev() {
            let size = event_block(e).chars().count() + 1;
            if keep > 0 && used + size > max_chars {
                break;
            }
            used += size;
            keep += 1;
        }
        from = events.len() - keep;
    }
    (&events[from..], from)
}

/// What a first message about an item says about the events it left out,
/// in ssf's own words and outside the quoted item: how many, that they
/// will not arrive later, and where to read them. The point of saying so
/// is that the item still holds them -- and that reading all of it can
/// fill a session's context.
fn omitted_notice(
    issue: &Issue,
    ctx: &PromptContext,
    shown: usize,
    omitted: usize,
) -> Option<String> {
    if omitted == 0 {
        return None;
    }
    let (kind, view) = if ctx.pr.is_some() {
        ("pull request", "pr")
    } else {
        ("issue", "issue")
    };
    let repo = &ctx.repo.name;
    let n = issue.number;
    let left = if omitted == 1 {
        "1 earlier event is left out and will not be delivered later.".to_string()
    } else {
        format!("{omitted} earlier events are left out and will not be delivered later.")
    };
    let follow = if shown == 1 {
        "The newest one follows below.".to_string()
    } else {
        format!("The {shown} newest follow below.")
    };
    Some(format!(
        "<omitted>This {kind} carries more history than this first message: {left} {follow} Read \
the rest from the {kind} itself when the work needs it -- `gh {view} view {n} --comments` for the \
comments, `gh api repos/{repo}/issues/{n}/timeline` for every event -- rather than reading all of \
it, which can fill this session's context.</omitted>"
    ))
}

/// ssf's own prompt: the lead line saying what ssf is and how it spawned
/// this session, the rules ssf owns (with the operator's and the
/// repository's configured instructions) in `<ssf-instructions>`, then
/// each guidance file in a section of its own. It ends with a closing
/// tag and no newline.
fn instructions(issue: &Issue, ctx: &PromptContext) -> String {
    let n = issue.number;
    let repo = &ctx.repo.name;
    let bot = ctx.bot_login;
    let kind = ctx.kind();
    let multiplexer = match ctx.driver {
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
        "[ssf] Simple Software Factory (ssf) spawned you as a coding agent for the GitHub \
account @{bot}, through {multiplexer}, into a worktree of this repository, because {}.\n\n\
<ssf-instructions>\n\
You are a remote colleague working this {kind} to delivery: clarify on it until the outcome is \
unambiguous, deliver (a pull request, a review, an answer), and let the people on it decide and \
review on GitHub. New activity on it arrives here as messages prefixed `[ssf]`; act on them. \
This terminal is unmanned: what a person, or another session, should see goes on the {kind} as \
a GitHub comment. Say there what you are about to do, and when you need a decision or have \
delivered.\n\n\
- Posts are read on GitHub: write GitHub Flavored Markdown, link the exact lines you mean \
(pinned to a commit), and use tables, Mermaid diagrams, task lists, `<details>` for long output, \
and screenshots or wireframes where they make a decision easier. Collaborators are remote: a \
live demo needs an address they can reach.\n\
- {acts_as}; your posts are marked as this session's. Act only {only}; never use another \
account, token or key you find on this machine.\n\
- `--assignee {bot}` on a `gh` create gives the new item a session of its own; `ssf sub` follows \
another item; `ssf handover` passes this one to another harness; `ssf release` retires this \
workspace; `ssf doctor` checks the machine. `ssf guide` is the reference behind all of this.\n",
        ctx.spawned_because(n)
    );
    if ctx.vm_guest {
        s.push_str(&format!("- {VM_GUEST_LINE}\n"));
    }
    match ctx.pr {
        Some(pr) if pr.same_repo(repo) => s.push_str(&format!(
            "- This worktree is on the pull request's branch `{}`; pushes to it change the PR.\n",
            pr.head_ref
        )),
        Some(pr) => s.push_str(&format!(
            "- The pull request comes from a fork ({}), so this worktree cannot push to its \
branch; it is on a branch of its own{}.\n",
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
            "- The session on {parent} handed this off and follows it as a subscriber; your final \
comment is all it gets, so sum up the outcome.\n"
        ));
    }
    s.push_str(&extras(ctx));
    s
}

/// The operator's and the repository's configured instructions, which end
/// the `<ssf-instructions>` section, then each guidance file in a section
/// of its own named after the file, so a file's own headings cannot mix
/// with ssf's.
fn extras(ctx: &PromptContext) -> String {
    let mut s = String::new();
    for extra in [
        ctx.daemon.instructions.as_deref(),
        ctx.repo.instructions.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        s.push('\n');
        s.push_str(extra.trim());
        s.push('\n');
    }
    s.push_str(&format!("</{SSF_INSTRUCTIONS}>"));
    for (tag, pp) in [
        ("global-guidance", &ctx.global_prompt),
        ("global-harness-guidance", &ctx.global_harness_prompt),
        ("repository-guidance", &ctx.project_prompt),
        ("harness-guidance", &ctx.harness_prompt),
    ] {
        if let Some(pp) = pp {
            s.push_str("\n\n");
            s.push_str(&file_section(tag, &pp.source, &pp.text));
        }
    }
    s
}

pub fn followup_prompt(issue: &Issue, events: &[Rendered], ctx: &PromptContext) -> String {
    let head = format!("[ssf] New activity on {}", short_ref(issue, ctx));
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
    let (shown, omitted) = first_prompt_events(events, ctx);
    let mut lead = String::new();
    if let Some(note) = omitted_notice(issue, ctx, shown.len(), omitted) {
        lead.push_str(&note);
        lead.push('\n');
    }
    s.push_str("\n\n");
    s.push_str(&events_section(HISTORY, &lead, shown, "(no activity yet)"));
    let answer = match ctx.pr {
        Some(pr) if pr.same_repo(&ctx.repo.name) => format!(
            "Answer on it with `gh pr comment {} --repo {}`; pushes to `{}` update it.",
            issue.number, ctx.repo.name, pr.head_ref
        ),
        Some(_) => format!(
            "It comes from a fork; answer on it with `gh pr comment {} --repo {}`.",
            issue.number, ctx.repo.name
        ),
        None => format!(
            "Answer on it with `gh issue comment {} --repo {}`.",
            issue.number, ctx.repo.name
        ),
    };
    s.push_str("\n\n");
    s.push_str(&instructions_section(&answer));
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
remove it, the checks found work that is not on origin for {repo}#{number}:\n\n\
<{SSF_INSTRUCTIONS}>\n"
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
    s.push_str(&format!("\n</{SSF_INSTRUCTIONS}>"));
    s
}

/// Tell a live session that its committed branch cannot merge cleanly into
/// the current base. This is advisory: the daemon never changes the
/// worktree, index or branch for the agent.
pub fn conflict_prompt(base_ref: &str, base_sha: &str, files: &[String]) -> String {
    let mut body = String::new();
    if files.is_empty() {
        body.push_str("Git reported a merge conflict, but did not name the affected files.\n\n");
    } else {
        body.push_str("Conflicting files:\n");
        for file in files {
            body.push_str(&format!("- `{file}`\n"));
        }
        body.push('\n');
    }
    body.push_str(
        "If your final round has started, rebase your branch onto the base, resolve the conflict, ",
    );
    body.push_str("and re-run the round. If the round has not started, do nothing now; resolve the conflict before starting it.");
    format!(
        "[ssf] Your branch conflicts with {base_ref} at base commit `{base_sha}`.\n\n{}",
        instructions_section(&body)
    )
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
            s.push_str(&section(
                "event",
                &format!(
                    "Final comment by {from} ({}):\n{}",
                    c.url,
                    quote(&c.body, ctx.daemon.max_body_chars)
                ),
            ));
            s.push_str("\n\n");
            s.push_str(&instructions_section(
                "This is the only message you will get about it.",
            ));
        }
        None => s.push_str(&instructions_section(
            "It has no comments.\n\nThis is the only message you will get about it.",
        )),
    }
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
        Fyi::Activity => format!("[ssf] FYI: new activity on {item}"),
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
/// startup: the machine or herdr restarted, its terminal is gone, and it
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
    s.push_str(&instructions_section(&format!(
        "This is the session for {item}{branch}{path}.\n\nWork out where you got to (`git \
status`, `git log`, your last comments on the item) and carry on from there. Anything that \
happened on the item while you were away arrives as further `[ssf]` messages. If you were \
part-way through something and cannot tell what is left, say so on the item: that the session \
was interrupted and what remains."
    )));
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
your conversation resumed. This is {what}.\n\n<{SSF_INSTRUCTIONS}>\nNothing you sent while it was lapsed reached \
anyone, and no `[ssf]` message reached you; what happened on the item meanwhile follows as \
further `[ssf]` messages. Work out where you got to (`git status`, `git log`, your last \
comments) and carry on.\n</{SSF_INSTRUCTIONS}>",
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
the session for {item}.\n\n<{SSF_INSTRUCTIONS}>\nNothing reached you while it was down; what happened on the item \
meanwhile follows as further `[ssf]` messages. Work out where the work got to (`git status`, \
`git log`, the comments on the item) and carry on.\n</{SSF_INSTRUCTIONS}>",
        it.harness, it.since
    )
}

/// What a session started by a handover (`ssf handover`) is told on top of
/// the item's own story: the outgoing agent's summary, in a
/// `<handover-summary>` section. `from` is the display name of the harness
/// the outgoing session ran, `kind` the item's word (`issue`, `pull
/// request`). The summary is the outgoing agent's own text and is passed
/// through unchanged. Kept apart from the story because the item holds on
/// to it until a session has read it: a start that fails is tried again
/// later, and the words the outgoing agent left go with that attempt.
pub fn handover_note(from: &str, kind: &str, summary: Option<&str>) -> String {
    let body = match summary {
        Some(text) => format!(
            "You took over this {kind} from a session on {from} that handed it over; its summary \
follows, then the {kind} as ssf tells it to a new session.\n\n{}",
            text.trim()
        ),
        None => format!(
            "You took over this {kind} from a session on {from} that handed it over. It left no \
summary; read the {kind} below."
        ),
    };
    section("handover-summary", &body)
}

/// The first message of a session started by a handover: the item's story
/// exactly as a new session gets it, with the note above as the first
/// section after its `[ssf]` lead line.
pub fn handover_prompt(from: &str, kind: &str, summary: Option<&str>, story: &str) -> String {
    with_section_after_lead(story, &handover_note(from, kind, summary))
}

/// The one message the outgoing agent gets when a handover it asked for
/// cannot be carried out: it is still the session on the item.
pub fn handover_refused_prompt(harness: &str, reason: &str) -> String {
    format!(
        "[ssf] Handover to {harness} refused: {reason}.\n\n{}",
        instructions_section("Carry on.")
    )
}

/// The one message the agent gets when a handover on its item is called
/// off (`ssf handover --cancel`): it was told to stop working, and this
/// is what takes that back.
pub fn handover_cancelled_prompt(harness: &str) -> String {
    format!(
        "[ssf] The handover to {harness} was cancelled: this session keeps the item.\n\n{}",
        instructions_section("Carry on.")
    )
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
    format!(
        "[ssf] {} has been assigned to @{} again.\n\n{}\n\n{}",
        short_ref(issue, ctx),
        ctx.bot_login,
        events_section(NEW_ACTIVITY, "", events, "(no new activity)"),
        instructions_section("Resume work on it.")
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
