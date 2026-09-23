//! Origin tags: `🤖#N says: <!-- ssf: origin=owner/repo#N -->`.
//!
//! GitHub has one bot identity and no notion of sessions, so nothing in the
//! API says which agent session posted a comment or opened a pull request.
//! The content carries it instead, on the first line of every post: a
//! visible byline (`🤖#N says:`, which GitHub renders with a link to the
//! session's item; `🤖owner/repo#N says:` on another repository) and, on
//! the same line, an invisible HTML comment naming the item whose
//! workspace the post came from. The `gh` shim
//! (`crate::shim`) prepends that line to everything an agent posts; the
//! daemon parses the tag back out of every body it reads, and honours it
//! only there, or on the last non-blank line, where posts made before the
//! byline carried it (the first line wins): a tag anywhere else in a body
//! (a fenced example, a pasted transcript, a quote reply) is content, not
//! the post's own tag. The byline is for people: someone who enrols their own account as the bot
//! can tell a session's posts from their own at a glance, and their own
//! untagged posts reach the agents as a person's.
//!
//! A session started by the daemon also names what it was started with
//! (see [`Stack`]): `🤖#N claude/opus/high says:`, so a reader can tell
//! which harness, model and effort that session is, and one session of a
//! handover from the next. The stack sits between the item and `says:`, and
//! is left out for a session nobody named one for (a hand-run `ssf launch`,
//! or a post made before this).
//!
//! The tag is a list of `key=value` fields after `ssf:`, so later features can
//! add fields without a new syntax. Two fields are defined: `mode=delegate`
//! (the post opened an item that is handed off to a new session rather than
//! kept by the one that opened it), and `event=<name>` (the post is the
//! daemon's own, one of the events in `crate::events`, about the item it
//! is on: not a session's, not a person's, and never delivered to an
//! agent). The byline does not encode the mode; an event post's byline is
//! `🤖 ssf`, since the daemon is not a session.
//! Posts made before #115 by the reviewer sessions of the time carry
//! `role=reviewer` and a `🤖#N (reviewer) says:` byline; both are read as
//! the item's session's.

use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;

use crate::github::{Issue, value_str, value_u64};

const OPEN: &str = "<!--";
const CLOSE: &str = "-->";
const MARK: &str = "ssf:";
/// What every byline starts with.
pub const ROBOT: &str = "\u{1F916}";
/// What every byline ends with.
pub const SAYS: &str = "says:";

/// The item (issue or pull request) a session works on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub repo: String,
    pub number: u64,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.repo, self.number)
    }
}

impl Origin {
    /// The session's own origin, from the environment `ssf launch` sets up.
    pub fn from_env() -> Option<Self> {
        let repo = std::env::var("SSF_REPO").ok()?;
        let number = std::env::var("SSF_ISSUE").ok()?.trim().parse().ok()?;
        Self::new(&repo, number)
    }

    pub fn new(repo: &str, number: u64) -> Option<Self> {
        let repo = repo.trim();
        crate::config::split_repo_name(repo).ok()?;
        Some(Self {
            repo: repo.to_string(),
            number,
        })
    }

    /// `owner/repo#N`.
    pub fn parse(s: &str) -> Option<Self> {
        let (repo, n) = s.trim().rsplit_once('#')?;
        Self::new(repo, n.parse().ok()?)
    }

    /// The machine-readable marker.
    pub fn tag(&self) -> String {
        format!("{OPEN} {MARK} origin={self} {CLOSE}")
    }

    /// The visible byline: `🤖#N says:` on the origin's own repository,
    /// `🤖owner/repo#N says:` on another (or when `on_repo`, the repository
    /// posted to, is not known), with `stack` between the item and `says:`
    /// when the session was launched with one. GitHub renders the item in
    /// either form as a link to it.
    pub fn byline(&self, on_repo: Option<&str>, stack: Option<&Stack>) -> String {
        let item = match on_repo {
            Some(r) if r.trim().eq_ignore_ascii_case(&self.repo) => format!("#{}", self.number),
            _ => self.to_string(),
        };
        let stack = match stack.and_then(Stack::label) {
            Some(label) => format!(" {label}"),
            None => String::new(),
        };
        format!("{ROBOT}{item}{stack} {SAYS}")
    }

    /// The line the shim prepends to a post made on `on_repo`: the byline,
    /// then the tag (a hand-off's when asked).
    pub fn first_line(
        &self,
        on_repo: Option<&str>,
        delegate: bool,
        stack: Option<&Stack>,
    ) -> String {
        let tag = if delegate {
            self.delegate_tag()
        } else {
            self.tag()
        };
        format!("{} {tag}", self.byline(on_repo, stack))
    }

    /// The marker for an item this session hands off to a new session.
    pub fn delegate_tag(&self) -> String {
        format!("{OPEN} {MARK} origin={self} {MODE}={DELEGATE} {CLOSE}")
    }

    /// The first line of one of the daemon's own posts on this item (see
    /// `crate::events`): the `🤖 ssf` byline, then a tag naming the item
    /// and the event.
    pub fn event_line(&self, event: &str) -> String {
        format!("{ROBOT} {DAEMON} {OPEN} {MARK} origin={self} {EVENT}={event} {CLOSE}")
    }
}

/// A scratch session: `owner/repo~id`, an agent session on a repository
/// that works on no item. The id is generated (lowercase letters and
/// digits); nobody names one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scratch {
    pub repo: String,
    pub id: String,
}

impl fmt::Display for Scratch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}~{}", self.repo, self.id)
    }
}

impl Scratch {
    pub fn new(repo: &str, id: &str) -> Option<Self> {
        let repo = repo.trim();
        crate::config::split_repo_name(repo).ok()?;
        let id = id.trim();
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            return None;
        }
        Some(Self {
            repo: repo.to_string(),
            id: id.to_string(),
        })
    }

    /// `owner/repo~id`.
    pub fn parse(s: &str) -> Option<Self> {
        let (repo, id) = s.trim().rsplit_once('~')?;
        Self::new(repo, id)
    }

    /// The scratch session this process runs in, from `SSF_SESSION`.
    pub fn from_env() -> Option<Self> {
        Self::parse(&std::env::var("SSF_SESSION").ok()?)
    }
}

/// Who a post is stamped as: an item's session (`Origin`) or a scratch
/// session. Both write the same byline and tag, naming themselves.
pub trait Poster {
    /// The session's id as its tag carries it (`owner/repo#N`,
    /// `owner/repo~id`).
    fn session(&self) -> String;
    /// The line the shim prepends to a post made on `on_repo`.
    fn first_line(&self, on_repo: Option<&str>, delegate: bool, stack: Option<&Stack>) -> String;
}

impl Poster for Origin {
    fn session(&self) -> String {
        self.to_string()
    }
    fn first_line(&self, on_repo: Option<&str>, delegate: bool, stack: Option<&Stack>) -> String {
        Origin::first_line(self, on_repo, delegate, stack)
    }
}

impl Poster for Scratch {
    fn session(&self) -> String {
        self.to_string()
    }
    /// `🤖~id says:` on its own repository, `🤖owner/repo~id says:` on
    /// another, with the stack as an item's byline has it.
    fn first_line(&self, on_repo: Option<&str>, delegate: bool, stack: Option<&Stack>) -> String {
        let who = match on_repo {
            Some(r) if r.trim().eq_ignore_ascii_case(&self.repo) => format!("~{}", self.id),
            _ => self.to_string(),
        };
        let stack = match stack.and_then(Stack::label) {
            Some(label) => format!(" {label}"),
            None => String::new(),
        };
        let mode = if delegate {
            format!(" {MODE}={DELEGATE}")
        } else {
            String::new()
        };
        format!("{ROBOT}{who}{stack} {SAYS} {OPEN} {MARK} origin={self}{mode} {CLOSE}")
    }
}

/// What a session was launched with, as its byline names it: the harness
/// (its id, what `--harness` takes) and the configured model and effort,
/// which are `None` where the harness's own default applies. `ssf launch`
/// exports the three as `SSF_HARNESS`, `SSF_MODEL` and `SSF_EFFORT` for the
/// session, and the shim reads them back out of its environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stack {
    pub harness: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl Stack {
    /// The stack these parts name, when a harness names one: each part is
    /// trimmed, and a blank one is the same as an absent one.
    pub fn from_parts(
        harness: Option<&str>,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Option<Self> {
        let part = |v: Option<&str>| {
            v.map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        Some(Self {
            harness: part(harness)?,
            model: part(model),
            effort: part(effort),
        })
    }

    /// `claude/opus/high`: the parts the byline can spell, in that order. A
    /// part that is not known is left out where nothing follows it, and is
    /// `-` where one does (an effort configured with no model), so the
    /// fields keep their places.
    ///
    /// Only a part the byline can read back (`is_byline`) is written:
    /// anything else — a harness that is a display name with a space in it,
    /// a model id with punctuation of its own — would be a byline `strip`
    /// cannot recognise, and so would be left in front of every agent that
    /// reads a post carrying it. Such a part names nothing, and `None` when
    /// the harness itself is one of them.
    pub fn label(&self) -> Option<String> {
        if !is_stack(&self.harness) {
            return None;
        }
        let model = self.model.as_deref().filter(|m| is_stack(m));
        let effort = self.effort.as_deref().filter(|e| is_stack(e));
        let mut label = self.harness.clone();
        if model.is_none() && effort.is_none() {
            return Some(label);
        }
        label.push('/');
        label.push_str(model.unwrap_or("-"));
        if let Some(effort) = effort {
            label.push('/');
            label.push_str(effort);
        }
        Some(label)
    }
}

/// Field naming how the origin session relates to the item it opened.
pub const MODE: &str = "mode";
/// `mode` value for a hand-off: the item gets its own session.
pub const DELEGATE: &str = "delegate";
/// Field naming the daemon event a post reports (`attached`, `blocked`,
/// ...); the post is the daemon's, about the item in `origin`.
pub const EVENT: &str = "event";
/// The byline word of a daemon post: `🤖 ssf`.
const DAEMON: &str = "ssf";
/// The byline word the reviewer sessions of before #115 carried between
/// the item and `says:`; still recognised when their posts are read.
const OLD_REVIEWER: &str = "reviewer";

/// A parsed tag: the origin plus any other fields it carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub origin: Origin,
    pub fields: BTreeMap<String, String>,
}

impl Tag {
    /// Was the item handed off to a session of its own?
    pub fn is_delegate(&self) -> bool {
        self.fields.get(MODE).map(String::as_str) == Some(DELEGATE)
    }

    /// The daemon event the post reports, when it is one of the daemon's
    /// own posts rather than a session's.
    pub fn event(&self) -> Option<&str> {
        self.fields.get(EVENT).map(String::as_str)
    }
}

/// Is `body` one of the daemon's own event posts: its first non-blank
/// line is the `🤖 ssf` byline and then a tag carrying `event=`, as
/// `Origin::event_line` writes it? Such a post is about the item, not
/// from a session or a person, and is never shown to an agent. The byline
/// is required, not just the tag: a session's post that starts with a
/// pasted tag is the session's (and the shim puts its own line first,
/// see `stamp_with`).
pub fn is_event_post(body: &str) -> bool {
    let Some(first) = body.lines().find(|l| !l.trim().is_empty()) else {
        return false;
    };
    if is_code(first) {
        return false;
    }
    let Some(at) = first.find(OPEN) else {
        return false;
    };
    if first[..at].trim() != format!("{ROBOT} {DAEMON}") {
        return false;
    }
    tags(first)
        .into_iter()
        .next()
        .is_some_and(|t| t.event().is_some())
}

/// Every ssf tag in `body`, in order of appearance. Tags inside quoted
/// lines (`> ...`, what GitHub's "quote reply" produces) belong to the post
/// being quoted and are skipped.
pub fn tags(body: &str) -> Vec<Tag> {
    let mut out = Vec::new();
    for (start, end) in spans(body) {
        if is_quoted(body, start) {
            continue;
        }
        let inner = body[start + OPEN.len()..end].trim();
        let Some(rest) = inner.strip_prefix(MARK) else {
            continue;
        };
        let mut fields = BTreeMap::new();
        for field in rest.split_whitespace() {
            if let Some((k, v)) = field.split_once('=') {
                fields.insert(k.to_string(), v.to_string());
            }
        }
        let Some(origin) = fields.remove("origin").and_then(|o| Origin::parse(&o)) else {
            continue;
        };
        out.push(Tag { origin, fields });
    }
    out
}

/// The tags on `line` that name any session, an item's or a scratch
/// session's: its id, whether it is one of the daemon's event tags, and
/// whether it marks a hand-off.
fn session_tags(line: &str) -> Vec<(String, bool, bool)> {
    let mut out = Vec::new();
    for (start, end) in spans(line) {
        if is_quoted(line, start) {
            continue;
        }
        let inner = line[start + OPEN.len()..end].trim();
        let Some(rest) = inner.strip_prefix(MARK) else {
            continue;
        };
        let mut origin = None;
        let mut event = false;
        let mut delegate = false;
        for field in rest.split_whitespace() {
            match field.split_once('=') {
                Some(("origin", v)) => origin = Some(v.to_string()),
                Some((k, _)) if k == EVENT => event = true,
                Some((k, v)) if k == MODE => delegate = v == DELEGATE,
                _ => {}
            }
        }
        if let Some(o) =
            origin.filter(|o| Origin::parse(o).is_some() || Scratch::parse(o).is_some())
        {
            out.push((o, event, delegate));
        }
    }
    out
}

/// The session a post says it came from, an item's or a scratch
/// session's, read where `parse` reads an item's tag.
pub fn session(body: &str) -> Option<String> {
    let first = body.lines().find(|l| !l.trim().is_empty())?;
    if !is_code(first)
        && let Some((o, ..)) = session_tags(first).into_iter().next()
    {
        return Some(o);
    }
    let last = body.trim_end().lines().next_back()?;
    if is_code(last) {
        return None;
    }
    session_tags(last).pop().map(|(o, ..)| o)
}

/// The tag that identifies the post: the one on the body's first non-blank
/// line, where the shim puts it (the first one on that line: the shim's
/// line goes before anything the author wrote by hand), or failing that
/// the one on the last non-blank line, where posts made before the byline
/// carried it (the last one on that line, since the old shim appended its
/// own after anything hand-written). The first line wins when both carry
/// one. A tag anywhere else, in a fenced or indented code block, a pasted
/// transcript or a quote, is content and does not count; nor does a first
/// or last line that is itself quoted or indented as code.
pub fn parse(body: &str) -> Option<Tag> {
    // A scratch session's post names no item, even with an item's tag
    // further down.
    if session(body).is_some_and(|s| Scratch::parse(&s).is_some()) {
        return None;
    }
    parse_first(body).or_else(|| parse_last(body))
}

/// The tag on the body's first non-blank line, if any: what the shim puts
/// there, and the only place `stamp_with` looks.
fn parse_first(body: &str) -> Option<Tag> {
    let first = body.lines().find(|l| !l.trim().is_empty())?;
    if is_code(first) {
        return None;
    }
    tags(first).into_iter().next()
}

/// The tag on the body's last non-blank line, if any (the old form).
fn parse_last(body: &str) -> Option<Tag> {
    let last = body.trim_end().lines().next_back()?;
    if is_code(last) {
        return None;
    }
    tags(last).pop()
}

/// Is `line` an indented code line (four spaces or a tab)?
fn is_code(line: &str) -> bool {
    line.starts_with("    ") || line.starts_with('\t')
}

/// `body` with the byline and origin tag on a first line of its own,
/// optionally marking the post as a hand-off and naming the session's
/// `stack`. `on_repo` is the repository the post goes to, which decides the
/// byline's form. A body that already starts with this origin's tag is left
/// alone (the agent added the line by hand), except that a hand-written tag
/// without `mode=delegate` is not enough for a hand-off: the delegate line
/// goes before it, and the first tag wins when read. A tag of ours that is
/// not on the first line does not count, even at the end where `parse` still
/// accepts the old form, so the body gets the byline at the top anyway. Nor
/// does a pasted event tag (`event=`): that is the daemon's form, and a
/// session's post must not pass for one of the daemon's, so the session line
/// goes on top.
pub fn stamp_with(
    body: &str,
    origin: &dyn Poster,
    on_repo: Option<&str>,
    delegate: bool,
    stack: Option<&Stack>,
) -> String {
    let first = body.lines().find(|l| !l.trim().is_empty());
    let own = first
        .filter(|l| !is_code(l))
        .and_then(|l| session_tags(l).into_iter().next())
        .is_some_and(|(o, event, handed_off)| {
            o == origin.session() && !event && (!delegate || handed_off)
        });
    if own {
        return body.to_string();
    }
    let line = origin.first_line(on_repo, delegate, stack);
    let text = without_leading_blank_lines(body).trim_end();
    if text.is_empty() {
        line
    } else {
        format!("{line}\n\n{text}")
    }
}

/// `s` from its first non-blank line on.
fn without_leading_blank_lines(s: &str) -> &str {
    let mut rest = s;
    while let Some((line, tail)) = rest.split_once('\n') {
        if !line.trim().is_empty() {
            break;
        }
        rest = tail;
    }
    if rest.trim().is_empty() { "" } else { rest }
}

/// `body` without its byline and ssf tags (for showing the text to an
/// agent). Every unquoted tag goes, not only the first-line one `parse`
/// honours, so that pasted examples and hand-written tags mid-body do not
/// reach the agent as something to imitate; the tags in quoted lines stay,
/// as `tags` treats them. A byline in front of a tag on its line goes with
/// it, and a line left blank that way goes entirely.
pub fn strip(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut last = 0;
    let mut line_cut = false;
    let push = |out: &mut String, chunk: &str, line_cut: bool| {
        // Text that followed a tag at the start of its line loses the
        // space that separated them.
        out.push_str(if line_cut {
            chunk.trim_start_matches([' ', '\t'])
        } else {
            chunk
        });
    };
    for (start, end) in spans(body) {
        let inner = body[start + OPEN.len()..end].trim();
        if !inner.starts_with(MARK) || is_quoted(body, start) {
            continue;
        }
        let line_start = body[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let cut = if is_byline(&body[line_start..start]) {
            line_start
        } else {
            start
        };
        push(&mut out, &body[last..cut.max(last)], line_cut);
        line_cut = cut == line_start;
        last = end + CLOSE.len();
        // Swallow the blank line before a tag that ends a body.
        if let Some(stripped) = out.strip_suffix("\n\n") {
            out.truncate(stripped.len());
            out.push('\n');
        }
    }
    push(&mut out, &body[last..], line_cut);
    without_leading_blank_lines(&out).trim_end().to_string()
}

/// Is `s` (the text before a tag on its line) a byline and nothing else:
/// `🤖#N`, `🤖owner/repo#N`, either with ` (reviewer)` (posts by the
/// reviewer sessions of before #115), either with ` says:` (posts made
/// before #42 have no `says:`), either with a stack (`Stack::label`) between
/// the item and `says:`, or the daemon's `🤖 ssf`?
fn is_byline(s: &str) -> bool {
    let Some(after) = s.trim_start().strip_prefix(ROBOT) else {
        return false;
    };
    if after.trim() == DAEMON {
        return true;
    }
    let item_len = after.find(char::is_whitespace).unwrap_or(after.len());
    if item_len == 0 {
        return false;
    }
    let rest = after[item_len..].trim_start();
    let rest = rest
        .strip_prefix(&format!("({OLD_REVIEWER})"))
        .unwrap_or(rest)
        .trim();
    // The stack is a single word between the item and `says:`, which is
    // where `Stack::label` writes it. Text of any other shape there is what
    // the author wrote: a byline ends with `says:`, so anything else on the
    // line is nothing to do with one.
    match rest.strip_suffix(SAYS) {
        Some(head) => {
            let head = head.trim_end();
            head.is_empty() || (!head.contains(char::is_whitespace) && is_stack(head))
        }
        // Posts made before #42 carried no `says:` at all.
        None => rest.is_empty(),
    }
}

/// Is `word` a stack label: the shape `Stack::label` writes, `-` where a
/// part is not known, `/` between the parts? Model ids are opaque, so this
/// admits any single word of identifier characters; spaces, and the
/// punctuation of prose, are what keep a sentence from reading as one.
fn is_stack(word: &str) -> bool {
    !word.is_empty()
        && word.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | ':' | '@' | '/' | '-' | '~')
        })
        && word.contains(|c: char| c.is_ascii_alphanumeric())
}

/// Does the line containing byte offset `at` start with a markdown quote?
fn is_quoted(body: &str, at: usize) -> bool {
    let line_start = body[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
    body[line_start..at].trim_start().starts_with('>')
}

/// Byte ranges `(start of "<!--", start of "-->")` of every HTML comment.
fn spans(body: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = body[from..].find(OPEN) {
        let start = from + rel;
        let Some(rel_end) = body[start + OPEN.len()..].find(CLOSE) else {
            break;
        };
        let end = start + OPEN.len() + rel_end;
        out.push((start, end));
        from = end + CLOSE.len();
    }
    out
}

/// Origins found on an item and its timeline.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Scan {
    /// Tag carried by the item's own body (the session that opened it).
    pub origin: Option<String>,
    /// The same tag with its fields (`mode=delegate` says the opening
    /// session handed the item off rather than keeping it).
    pub origin_tag: Option<Tag>,
    /// Timeline event key -> session (`owner/repo#N`) that made the
    /// comment or review.
    pub origins: BTreeMap<String, String>,
    /// Posts by the bot that carry no tag (the shim was not in effect where
    /// they were made): event key -> URL. The item body is keyed `body`.
    pub untagged: BTreeMap<String, String>,
    /// The daemon's own event posts (`event=` in the tag): event key ->
    /// event name. The bucket exists to keep them out of `origins` and
    /// `untagged`, where they would count as a session's or a person's;
    /// nothing reads it back and nothing persists it.
    pub events: BTreeMap<String, String>,
}

/// Parse the tags out of the item body and every comment-like event on its
/// timeline, noting the bot's posts that have none and setting the
/// daemon's own event posts apart. Only the bot's own posts are read: a
/// tag in a human's text is something they quoted or pasted.
pub fn scan(issue: &Issue, timeline: &[Value], bot: &str) -> Scan {
    let mut s = Scan::default();
    let mut body_tag = None;
    let mut note = |key: String, author: &str, body: Option<&str>, url: &str| {
        if !author.eq_ignore_ascii_case(bot) {
            return;
        }
        let body = body.unwrap_or("");
        match parse(body) {
            Some(t) if is_event_post(body) => {
                s.events.insert(key, t.event().unwrap_or("").to_string());
            }
            Some(t) => {
                if key == "body" {
                    body_tag = Some(t.clone());
                }
                s.origins.insert(key, t.origin.to_string());
            }
            // A scratch session's post: whose it is, and no item's.
            None if let Some(session) = session(body) => {
                s.origins.insert(key, session);
            }
            None => {
                s.untagged.insert(key, url.to_string());
            }
        }
    };
    note(
        "body".into(),
        issue.author(),
        issue.body.as_deref(),
        &issue.html_url,
    );
    for ev in timeline {
        let Some(kind) = value_str(ev, &["event"]) else {
            continue;
        };
        match kind {
            "commented" | "reviewed" => {
                let Some(key) = crate::prompt::event_key(ev) else {
                    continue;
                };
                note(
                    key,
                    &crate::prompt::actor_of(ev),
                    value_str(ev, &["body"]),
                    value_str(ev, &["html_url"]).unwrap_or(""),
                );
            }
            "line-commented" | "commit-commented" => {
                let comments = ev.get("comments").and_then(Value::as_array);
                for (i, c) in comments.into_iter().flatten().enumerate() {
                    let key = match value_u64(c, &["id"]) {
                        Some(id) => format!("{kind}:{id}"),
                        None => {
                            format!("{kind}:{}:{i}", value_str(c, &["created_at"]).unwrap_or(""))
                        }
                    };
                    note(
                        key,
                        value_str(c, &["user", "login"]).unwrap_or(""),
                        value_str(c, &["body"]),
                        value_str(c, &["html_url"]).unwrap_or(""),
                    );
                }
            }
            _ => {}
        }
    }
    s.origin = s.origins.remove("body");
    s.origin_tag = body_tag;
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn o() -> Origin {
        Origin::new("acme/widgets", 12).unwrap()
    }

    /// Stamp for a post on the origin's own repository.
    fn stamp(body: &str, origin: &Origin) -> String {
        stamp_with(body, origin, Some("acme/widgets"), false, None)
    }

    fn stack() -> Stack {
        Stack {
            harness: "claude".into(),
            model: Some("opus".into()),
            effort: Some("high".into()),
        }
    }

    fn line() -> String {
        o().first_line(Some("acme/widgets"), false, None)
    }

    #[test]
    fn scratch_ids_parse_and_print() {
        let s = Scratch::parse(" acme/widgets~k3f9 ").unwrap();
        assert_eq!(s.repo, "acme/widgets");
        assert_eq!(s.id, "k3f9");
        assert_eq!(s.to_string(), "acme/widgets~k3f9");
        for bad in [
            "acme/widgets#12",
            "acme/widgets~",
            "acme/widgets~K3F9",
            "acme/widgets~k3-9",
            "widgets~k3f9",
            "~k3f9",
        ] {
            assert!(Scratch::parse(bad).is_none(), "{bad}");
        }
        // An item reference is never read as a scratch session, nor the
        // other way round.
        assert!(Origin::parse("acme/widgets~k3f9").is_none());
    }

    #[test]
    fn tag_round_trips() {
        let t = o().tag();
        assert_eq!(t, "<!-- ssf: origin=acme/widgets#12 -->");
        let parsed = parse(&t).unwrap();
        assert_eq!(parsed.origin, o());
        assert!(parsed.fields.is_empty());
    }

    #[test]
    fn byline_names_the_item_the_way_github_links_it() {
        assert_eq!(o().byline(Some("acme/widgets"), None), "🤖#12 says:");
        assert_eq!(o().byline(Some("ACME/Widgets"), None), "🤖#12 says:");
        assert_eq!(
            o().byline(Some("acme/other"), None),
            "🤖acme/widgets#12 says:"
        );
        assert_eq!(o().byline(None, None), "🤖acme/widgets#12 says:");
        assert_eq!(
            line(),
            "🤖#12 says: <!-- ssf: origin=acme/widgets#12 -->",
            "byline, then the tag, on one line"
        );
        assert_eq!(
            o().first_line(None, true, None),
            "🤖acme/widgets#12 says: <!-- ssf: origin=acme/widgets#12 mode=delegate -->",
            "the byline does not encode the mode"
        );
        // The whole line parses back to the tag: whatever the byline says
        // about the session, the daemon reads the same item.
        assert_eq!(parse(&line()).unwrap().origin, o());
        let stack = stack();
        assert_eq!(
            o().byline(Some("acme/widgets"), Some(&stack)),
            "🤖#12 claude/opus/high says:"
        );
        assert_eq!(
            o().byline(Some("acme/other"), Some(&stack)),
            "🤖acme/widgets#12 claude/opus/high says:"
        );
        let stacked = o().first_line(Some("acme/widgets"), false, Some(&stack));
        assert_eq!(
            parse(&stacked).unwrap().origin,
            o(),
            "a stack is between the item and `says:`, not a tag of its own"
        );
        assert_eq!(strip(&format!("{stacked}\n\nhi")), "hi");
    }

    /// Only the parts a session has, and that the byline can read back, are
    /// named; a missing one keeps the places of the parts after it.
    #[test]
    fn a_stack_names_the_parts_that_are_known() {
        let stack = |harness: &str, model: Option<&str>, effort: Option<&str>| Stack {
            harness: harness.into(),
            model: model.map(str::to_string),
            effort: effort.map(str::to_string),
        };
        let label = |harness: &str, model: Option<&str>, effort: Option<&str>| {
            stack(harness, model, effort).label()
        };
        assert_eq!(
            label("claude", Some("opus"), Some("high")).as_deref(),
            Some("claude/opus/high")
        );
        assert_eq!(label("omp", None, None).as_deref(), Some("omp"));
        assert_eq!(
            label("claude", Some("opus"), None).as_deref(),
            Some("claude/opus")
        );
        assert_eq!(
            label("claude", None, Some("high")).as_deref(),
            Some("claude/-/high"),
            "an effort with no model keeps the model's place"
        );
        // A part the byline could not read back is not written into one: it
        // would be left in front of every agent that reads the post.
        assert_eq!(label("Claude Code", Some("opus"), None), None);
        assert_eq!(
            label("claude", Some("two words"), None).as_deref(),
            Some("claude")
        );
        assert_eq!(
            label("claude", Some("two words"), Some("high")).as_deref(),
            Some("claude/-/high")
        );
        assert_eq!(
            label("claude", Some("opus"), Some("on, high")).as_deref(),
            Some("claude/opus")
        );
        // Whatever a config holds, the byline the shim writes comes back
        // off: the writer and the reader agree on one alphabet, including
        // for a harness ssf runs sessions for and one it would not accept.
        for harness in [
            "claude",
            "omp",
            "pi",
            "codex",
            "opencode",
            "copilot",
            "gemini",
            "Claude Code",
        ] {
            let stack = stack(harness, Some("gpt-5.6-sol"), Some("xhigh"));
            let line = o().first_line(Some("acme/widgets"), false, Some(&stack));
            let body = format!("{line}\n\nhi");
            assert_eq!(strip(&body), "hi", "{harness}: {body}");
        }
        assert_eq!(
            Stack::from_parts(Some(" claude "), Some("opus"), Some("  ")),
            Some(stack("claude", Some("opus"), None))
        );
        assert_eq!(Stack::from_parts(Some("  "), Some("opus"), None), None);
        assert_eq!(Stack::from_parts(None, None, None), None);
        // What the config makes of an item, which is what launch is given.
        let repo = crate::config::RepoConfig {
            harness: "codex".into(),
            model: Some("gpt-5.6-sol".into()),
            effort: Some("high".into()),
            ..Default::default()
        };
        assert_eq!(
            repo.stack().label().as_deref(),
            Some("codex/gpt-5.6-sol/high")
        );
    }

    /// A byline that carries a stack is still just a byline: what the agent
    /// is shown keeps only the words the author wrote.
    #[test]
    fn a_stack_byline_is_stripped_like_any_other() {
        let stack = stack();
        let first = o().first_line(Some("acme/widgets"), false, Some(&stack));
        assert_eq!(strip(&format!("{first}\n\nlook at this")), "look at this");
        // The same on another repository, with a model id that has a slash
        // of its own, and with the old reviewer word.
        assert_eq!(
            strip(&format!(
                "🤖acme/widgets#12 omp/deepseek/deepseek-flash/high says: {}\n\nhi",
                o().tag()
            )),
            "hi"
        );
        assert_eq!(
            strip(&format!(
                "🤖#12 (reviewer) claude/opus/high says: {}\n\nhi",
                o().tag()
            )),
            "hi"
        );
        // Two words after the item are prose, not a stack: they stay.
        assert_eq!(
            strip(&format!("🤖#12 the model is opus: {}\n\nhi", o().tag())),
            "🤖#12 the model is opus: \n\nhi"
        );
        assert_eq!(
            strip(&format!("🤖#12 said: {}\n\nhi", o().tag())),
            "🤖#12 said: \n\nhi"
        );
        // Only `says:` ends a byline, however the stack in front of it is
        // spelled; the item is what tells a byline from prose, and a word
        // that is prose after it is nobody's stack.
        assert_eq!(
            strip(&format!("🤖#12 claude/opus say: {}\n\nhi", o().tag())),
            "🤖#12 claude/opus say: \n\nhi"
        );
    }

    #[test]
    fn event_line_marks_a_daemon_post() {
        let first = o().event_line("attached");
        assert_eq!(
            first,
            "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=attached -->"
        );
        let t = parse(&first).unwrap();
        assert_eq!(t.origin, o());
        assert_eq!(t.event(), Some("attached"));
        assert!(!t.is_delegate());
        assert!(is_event_post(&first));
        let post =
            format!("{first}\n\n```ssf\nssf attaching agent to issue:\nharness: Claude Code\n```");
        assert!(is_event_post(&post));
        assert_eq!(parse(&post).unwrap().event(), Some("attached"));
        // A session's post, with or without other fields, is not one.
        assert!(!is_event_post(&line()));
        assert!(!is_event_post(&o().delegate_tag()));
        assert!(parse(&o().tag()).unwrap().event().is_none());
        assert!(!is_event_post("plain"));
        // Quoted or fenced, it is content like any other tag.
        assert!(!is_event_post(&format!("> {first}\n\nreply")));
        assert!(!is_event_post(&format!("see:\n```\n{first}\n```\n")));
        // The daemon byline goes with its tag when a body is stripped.
        assert_eq!(
            strip(&post),
            "```ssf\nssf attaching agent to issue:\nharness: Claude Code\n```"
        );
        assert_eq!(strip(&first), "");
        // A session's post that starts with a pasted event line, or the
        // bare tag, is the session's: the shim puts its own line on top
        // and the result is not an event post.
        let pasted = format!("{post}\n\nI saw this on the item");
        assert!(is_event_post(&pasted), "the raw paste looks like one");
        let stamped = stamp(&pasted, &o());
        assert!(
            stamped.starts_with(&format!("{}\n\n{first}", line())),
            "{stamped}"
        );
        assert!(!is_event_post(&stamped));
        let t = parse(&stamped).unwrap();
        assert_eq!(t.origin, o());
        assert!(t.event().is_none());
        let bare = format!(
            "{}\n\nlook at this tag",
            o().tag().replace(" -->", " event=blocked -->")
        );
        assert!(!is_event_post(&bare), "no byline, no event post");
        assert!(stamp(&bare, &o()).starts_with(&line()));
        assert!(!is_event_post(&stamp(&bare, &o())));
        // The daemon's own line gets the session line too when the shim
        // sees it (not that it ever does: the daemon posts through the API).
        assert_eq!(stamp(&post, &o()), format!("{}\n\n{post}", line()));
        // A tag with `event=` but another word before it is not the byline.
        assert!(!is_event_post(&format!("🤖 said {first}")));
        assert!(
            !is_event_post(&format!("  🤖 ssf {}", o().tag())),
            "no event field"
        );
    }

    #[test]
    fn parses_extra_fields_and_loose_spacing() {
        let t = parse("<!--ssf: origin=a/b#3 mode=delegate-->\nhi").unwrap();
        assert_eq!(t.origin.to_string(), "a/b#3");
        assert_eq!(t.fields.get("mode").map(String::as_str), Some("delegate"));
    }

    #[test]
    fn first_tag_wins_and_other_comments_are_ignored() {
        let body = "<!-- plain html comment -->\n<!-- ssf: origin=a/b#2 -->\n\nquoting:\n> <!-- ssf: origin=a/b#1 -->";
        assert!(
            parse(body).is_none(),
            "the first line is a plain comment, not a tag"
        );
        assert_eq!(tags(body).len(), 1, "the quoted tag does not count");
        assert_eq!(
            parse("<!-- ssf: origin=a/b#2 --> <!-- ssf: origin=a/b#3 -->")
                .unwrap()
                .origin
                .number,
            2,
            "the first tag on the line is the post's"
        );
        assert!(parse("<!-- ssf: origin=nonsense -->").is_none());
        // A quote reply carries the quoted post's tag, not one of its own.
        assert!(parse("> hi\n> <!-- ssf: origin=a/b#1 -->\n\nthanks").is_none());
        assert!(parse("  > <!-- ssf: origin=a/b#1 -->").is_none());
        assert_eq!(
            parse("<!-- ssf: origin=a/b#2 -->\n> <!-- ssf: origin=a/b#1 -->")
                .unwrap()
                .origin
                .number,
            2
        );
        assert!(parse("no tag here").is_none());
        assert!(parse("<!-- ssf: origin=a/b#1").is_none());
        assert!(parse("").is_none());
        assert!(parse("\n\n  \n").is_none());
    }

    #[test]
    fn only_the_first_line_is_the_posts_own_tag() {
        // A tag quoted in a fenced block is content, not the post's tag.
        let fenced = "the tag looks like:\n```text\n<!-- ssf: origin=a/b#1 -->\n```\n";
        assert!(parse(fenced).is_none());
        assert_eq!(tags(fenced).len(), 1, "tags() still lists it");
        // ... until the shim prepends the real one.
        let stamped = stamp(fenced, &o());
        assert!(stamped.starts_with(&format!("{}\n\nthe tag", line())));
        assert_eq!(parse(&stamped).unwrap().origin, o());
        assert_eq!(strip(&stamped), "the tag looks like:\n```text\n\n```");
        // A tag on the last line, where posts used to carry it, still
        // counts (the last one on that line); the first line wins.
        assert_eq!(
            parse("more\n\n<!-- ssf: origin=a/b#1 -->")
                .unwrap()
                .origin
                .number,
            1
        );
        assert_eq!(
            parse("signed off\n\npasted <!-- ssf: origin=a/b#1 --> <!-- ssf: origin=a/b#2 -->")
                .unwrap()
                .origin
                .number,
            2
        );
        assert_eq!(
            parse("<!-- ssf: origin=a/b#3 -->\n\ntext\n\n<!-- ssf: origin=a/b#1 -->")
                .unwrap()
                .origin
                .number,
            3,
            "first line wins"
        );
        assert!(parse("thanks\n\n> <!-- ssf: origin=a/b#1 -->").is_none());
        assert!(parse("example:\n\n    <!-- ssf: origin=a/b#1 -->").is_none());
        // A tag in the middle is content (the #23 rule).
        assert!(parse("a\n<!-- ssf: origin=a/b#1 -->\nb").is_none());
        // An indented code line at the start is code, not a tag.
        assert!(parse("    <!-- ssf: origin=a/b#1 -->\nexample").is_none());
        assert!(parse("\t<!-- ssf: origin=a/b#1 -->\nexample").is_none());
        // A quoted first line is the quoted post's tag.
        assert!(parse("> <!-- ssf: origin=a/b#1 -->\n\nthanks").is_none());
        // A tag on the first non-blank line is honoured, leading blank lines
        // and all, with or without text after it on that line.
        assert_eq!(
            parse("\n  \n<!-- ssf: origin=a/b#2 -->\n\nhello")
                .unwrap()
                .origin
                .number,
            2
        );
        assert_eq!(
            parse("<!-- ssf: origin=a/b#3 --> tagged\nmore")
                .unwrap()
                .origin
                .number,
            3
        );
        assert_eq!(
            parse("  <!-- ssf: origin=a/b#4 -->").unwrap().origin.number,
            4,
            "one-space indentation is not code"
        );
        // A body that is only a tag still parses.
        assert_eq!(parse(&o().tag()).unwrap().origin, o());
        assert_eq!(parse(&format!("\n{}\n", o().tag())).unwrap().origin, o());
        // Our own tag at the end still attributes (old form) but is not
        // enough for the shim: the byline goes on top, and wins.
        let end = format!("more\n\n{}", o().tag());
        assert_eq!(parse(&end).unwrap().origin, o());
        let s = stamp(&end, &o());
        assert_eq!(s, format!("{}\n\n{end}", line()));
        assert_eq!(parse(&s).unwrap().origin, o());
        assert_eq!(strip(&s), "more");
        let old_delegate = format!("child\n\n{}", o().delegate_tag());
        assert!(parse(&old_delegate).unwrap().is_delegate());
    }

    #[test]
    fn delegate_tag_marks_a_hand_off() {
        let t = o().delegate_tag();
        assert_eq!(t, "<!-- ssf: origin=acme/widgets#12 mode=delegate -->");
        let parsed = parse(&t).unwrap();
        assert!(parsed.is_delegate());
        assert!(!parse(&o().tag()).unwrap().is_delegate());
        let s = stamp_with("hand this off", &o(), Some("acme/widgets"), true, None);
        assert_eq!(s, format!("🤖#12 says: {t}\n\nhand this off"));
        assert_eq!(
            stamp_with(&s, &o(), Some("acme/widgets"), true, None),
            s,
            "not stamped twice"
        );
        assert_eq!(
            stamp_with(&s, &o(), Some("acme/widgets"), false, None),
            s,
            "a delegate tag is a tag"
        );
        // A hand-written plain tag does not make a hand-off: the delegate
        // line goes before it and is the one that counts.
        let plain = stamp("x", &o());
        let both = stamp_with(&plain, &o(), Some("acme/widgets"), true, None);
        assert!(both.ends_with(&plain));
        assert!(parse(&both).unwrap().is_delegate());
        assert_eq!(strip(&both), "x");
    }

    #[test]
    fn old_reviewer_posts_read_as_the_items_session() {
        // Before #115 a pull request had a second, reviewing session whose
        // posts carried `role=reviewer`; the field is just a field now.
        let t = "<!-- ssf: origin=acme/widgets#12 role=reviewer -->";
        let parsed = parse(t).unwrap();
        assert_eq!(parsed.origin, o());
        assert!(!parsed.is_delegate());
        assert_eq!(
            parsed.fields.get("role").map(String::as_str),
            Some("reviewer")
        );
        assert_eq!(
            strip(&format!("🤖#12 (reviewer) says: {t}\n\nlooks good")),
            "looks good"
        );
        // A session id with the old suffix is not one any more.
        assert!(Origin::parse("acme/widgets#12:reviewer").is_none());
        assert_eq!(Origin::parse(" acme/widgets#12 ").unwrap(), o());
    }

    #[test]
    fn stamp_prepends_once() {
        let s = stamp("hello  \n", &o());
        assert_eq!(
            s,
            "🤖#12 says: <!-- ssf: origin=acme/widgets#12 -->\n\nhello"
        );
        assert_eq!(stamp(&s, &o()), s);
        assert_eq!(stamp("", &o()), line());
        assert_eq!(stamp("\n \n", &o()), line());
        assert_eq!(
            stamp("\n\nhello", &o()),
            s,
            "leading blank lines go, the byline is the first line"
        );
        assert_eq!(
            stamp("    code first", &o()),
            format!("{}\n\n    code first", line()),
            "indentation of the first line is kept"
        );
        // A different origin quoted in the body does not count as ours.
        let quoted = "see <!-- ssf: origin=acme/widgets#99 -->";
        assert!(stamp(quoted, &o()).starts_with(&line()));
        // On another repository the byline spells the repository out.
        assert_eq!(
            stamp_with("hi", &o(), Some("acme/other"), false, None),
            "🤖acme/widgets#12 says: <!-- ssf: origin=acme/widgets#12 -->\n\nhi"
        );
        assert_eq!(
            stamp_with("hi", &o(), None, false, None),
            "🤖acme/widgets#12 says: <!-- ssf: origin=acme/widgets#12 -->\n\nhi"
        );
        // A hand-written first line with the right tag is left alone, byline
        // or not.
        let by_hand = format!("{}\n\nhello", o().tag());
        assert_eq!(stamp(&by_hand, &o()), by_hand);
        let cross = format!("🤖acme/widgets#12 says: {}\n\nhello", o().tag());
        assert_eq!(stamp(&cross, &o()), cross);
        // So is the byline from before #42, without `says:`.
        let old = format!("🤖#12 {}\n\nhello", o().tag());
        assert_eq!(stamp(&old, &o()), old);
    }

    #[test]
    fn strip_removes_bylines_and_tags_only() {
        let s = stamp("hello\n<!-- keep me -->", &o());
        assert_eq!(strip(&s), "hello\n<!-- keep me -->");
        assert_eq!(strip("plain"), "plain");
        assert_eq!(strip(&o().tag()), "");
        assert_eq!(strip(&line()), "");
        assert_eq!(
            strip(
                "🤖acme/widgets#12 (reviewer) says: <!-- ssf: origin=acme/widgets#12 role=reviewer -->"
            ),
            ""
        );
        let quoted = "> <!-- ssf: origin=a/b#1 -->\nreply";
        assert_eq!(strip(quoted), quoted);
        assert_eq!(
            strip("<!-- ssf: origin=a/b#1 -->\n\ncaf\u{e9} \u{1F600}"),
            "caf\u{e9} \u{1F600}"
        );
        assert_eq!(
            strip("caf\u{e9} \u{1F600}\n\n<!-- ssf: origin=a/b#1 -->"),
            "caf\u{e9} \u{1F600}",
            "a trailing tag (older posts) still goes"
        );
        assert_eq!(strip("<!-- never closed"), "<!-- never closed");
        // Text on the first line after the tag stays; only byline and tag go.
        assert_eq!(
            strip("🤖#12 says: <!-- ssf: origin=a/b#12 --> hello\nmore"),
            "hello\nmore"
        );
        assert_eq!(
            strip("🤖#12 (reviewer) says: <!-- ssf: origin=a/b#12 role=reviewer --> hello"),
            "hello"
        );
        // Bylines from before #42 have no `says:`; they go too.
        assert_eq!(
            strip("🤖#12 <!-- ssf: origin=a/b#12 --> hello\nmore"),
            "hello\nmore"
        );
        assert_eq!(
            strip("🤖acme/widgets#12 (reviewer) <!-- ssf: origin=a/b#12 role=reviewer -->\n\nhi"),
            "hi"
        );
        // Other words before the tag are content, not a byline.
        assert_eq!(
            strip("🤖#12 said <!-- ssf: origin=a/b#12 -->\n\nhi"),
            "🤖#12 said \n\nhi"
        );
        assert_eq!(strip("<!-- ssf: origin=a/b#12 --> hello"), "hello");
        // A robot that is not the byline (no tag on its line) is content.
        assert_eq!(strip("🤖 beep\n\nhi"), "🤖 beep\n\nhi");
        assert_eq!(
            strip("<!-- ssf: origin=a/b#1 -->\n\n🤖#1 said so"),
            "🤖#1 said so",
            "the byline is only looked for on the tag's line"
        );
    }

    #[test]
    fn scan_reads_a_scratch_sessions_post_as_its_and_no_items() {
        let issue: Issue = serde_json::from_value(json!({
            "number": 5, "title": "t", "body": "🤖~k3f9 says: <!-- ssf: origin=a/b~k3f9 -->\n\nopened",
            "html_url": "https://gh/5", "state": "open", "user": {"login": "bot"},
            "created_at": "x", "updated_at": "x"
        }))
        .unwrap();
        let timeline = vec![
            json!({"event":"commented","id":1,"user":{"login":"bot"},"body":"🤖~k3f9 says: <!-- ssf: origin=a/b~k3f9 -->\n\nhi","html_url":"u1"}),
        ];
        let s = scan(&issue, &timeline, "bot");
        assert_eq!(s.origin.as_deref(), Some("a/b~k3f9"));
        assert!(s.origin_tag.is_none(), "no item opened it");
        assert_eq!(
            s.origins.get("commented:1").map(String::as_str),
            Some("a/b~k3f9")
        );
        assert!(s.untagged.is_empty());
    }

    #[test]
    fn scan_collects_origins_and_untagged_bot_posts() {
        let issue: Issue = serde_json::from_value(json!({
            "number": 5, "title": "t", "body": "🤖a/b#1 <!-- ssf: origin=a/b#1 -->\n\nopened by a session",
            "html_url": "https://gh/5", "state": "open", "user": {"login": "bot"},
            "created_at": "x", "updated_at": "x"
        }))
        .unwrap();
        let timeline = vec![
            json!({"event":"commented","id":1,"user":{"login":"bot"},"body":"🤖#1 <!-- ssf: origin=a/b#1 -->\n\ntagged","html_url":"u1"}),
            json!({"event":"commented","id":2,"user":{"login":"bot"},"body":"untagged","html_url":"u2"}),
            json!({"event":"commented","id":3,"user":{"login":"alice"},"body":"human","html_url":"u3"}),
            json!({"event":"commented","id":30,"user":{"login":"alice"},"body":"<!-- ssf: origin=a/b#1 --> pasted","html_url":"u30"}),
            json!({"event":"reviewed","id":4,"user":{"login":"bot"},"body":"<!-- ssf: origin=a/b#7 -->","html_url":"u4"}),
            json!({"event":"commented","id":9,"user":{"login":"bot"},"body":"see\n```\n<!-- ssf: origin=a/b#7 role=reviewer -->\n```\n","html_url":"u9"}),
            json!({"event":"reviewed","id":5,"user":{"login":"bot"},"body":"🤖#5 (reviewer) <!-- ssf: origin=a/b#5 role=reviewer -->\n\nlgtm","html_url":"u5"}),
            json!({"event":"commented","id":10,"user":{"login":"bot"},"body":"old style\n\n<!-- ssf: origin=a/b#1 -->","html_url":"u10"}),
            json!({"event":"line-commented","comments":[{"id":8,"user":{"login":"bot"},"body":"inline","html_url":"u8"}]}),
            json!({"event":"commented","id":11,"user":{"login":"bot"},"body":"🤖 ssf <!-- ssf: origin=a/b#5 event=blocked -->\n\n```ssf\nssf holding deliveries to agent on issue:\nharness: Codex\n```","html_url":"u11"}),
            json!({"event":"commented","id":12,"user":{"login":"alice"},"body":"🤖 ssf <!-- ssf: origin=a/b#5 event=blocked -->\n\npasted by a person","html_url":"u12"}),
            json!({"event":"commented","id":13,"user":{"login":"bot"},"body":"<!-- ssf: origin=a/b#5 event=blocked -->\n\npasted tag, no byline","html_url":"u13"}),
        ];
        let s = scan(&issue, &timeline, "Bot");
        assert_eq!(s.origin.as_deref(), Some("a/b#1"));
        assert!(!s.origin_tag.as_ref().unwrap().is_delegate());
        // The daemon's own post is an event: in neither map, and a
        // person's copy of one is nothing at all.
        assert_eq!(
            s.events.get("commented:11").map(String::as_str),
            Some("blocked")
        );
        assert!(!s.origins.contains_key("commented:11"));
        assert!(!s.untagged.contains_key("commented:11"));
        assert!(!s.events.contains_key("commented:12"));
        assert_eq!(s.events.len(), 1);
        assert_eq!(
            s.origins.get("commented:13").map(String::as_str),
            Some("a/b#5"),
            "a bot post with a pasted event tag and no byline is a session's"
        );
        assert_eq!(
            s.origins.get("commented:1").map(String::as_str),
            Some("a/b#1")
        );
        assert_eq!(
            s.origins.get("reviewed:4").map(String::as_str),
            Some("a/b#7")
        );
        assert_eq!(
            s.origins.get("reviewed:5").map(String::as_str),
            Some("a/b#5"),
            "an old reviewer post is the item's session's"
        );
        assert_eq!(
            s.untagged.get("commented:2").map(String::as_str),
            Some("u2")
        );
        assert_eq!(
            s.untagged.get("line-commented:8").map(String::as_str),
            Some("u8")
        );
        assert!(!s.untagged.contains_key("commented:3"));
        assert!(
            !s.origins.contains_key("commented:9"),
            "a tag inside a code block is not the post's own"
        );
        assert_eq!(
            s.untagged.get("commented:9").map(String::as_str),
            Some("u9"),
            "a post whose only tag is quoted counts as untagged"
        );
        assert_eq!(
            s.origins.get("commented:10").map(String::as_str),
            Some("a/b#1"),
            "an old-form comment, tag on the last line, still attributes"
        );
        assert!(!s.untagged.contains_key("commented:10"));
        assert!(
            !s.origins.contains_key("commented:30"),
            "a human's tag is not an origin"
        );
        assert!(!s.untagged.contains_key("body"));

        let human: Issue = serde_json::from_value(json!({
            "number": 6, "title": "t", "body": null, "html_url": "https://gh/6", "state": "open",
            "user": {"login": "alice"}, "created_at": "x", "updated_at": "x"
        }))
        .unwrap();
        let s = scan(&human, &[], "bot");
        assert!(s.origin.is_none() && s.origin_tag.is_none() && s.untagged.is_empty());
        let mut quoting = issue.clone();
        quoting.body =
            Some("about tags:\n\n```\n<!-- ssf: origin=a/b#9 -->\n```\n\nposted by hand".into());
        let s = scan(&quoting, &[], "bot");
        assert!(s.origin.is_none(), "a quoted tag does not bind the item");
        assert!(s.untagged.contains_key("body"));
        let mut delegated = issue.clone();
        delegated.body = Some("🤖#1 <!-- ssf: origin=a/b#1 mode=delegate -->\n\nchild".into());
        let s = scan(&delegated, &[], "bot");
        assert_eq!(s.origin.as_deref(), Some("a/b#1"));
        assert!(s.origin_tag.as_ref().unwrap().is_delegate());
        let mut by_bot = human.clone();
        by_bot.user = Some(crate::github::User {
            login: "bot".into(),
            id: 0,
            kind: String::new(),
            email: None,
        });
        assert_eq!(
            scan(&by_bot, &[], "bot")
                .untagged
                .get("body")
                .map(String::as_str),
            Some("https://gh/6")
        );
    }
}
