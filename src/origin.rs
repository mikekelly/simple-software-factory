//! Origin tags: `🤖#N <!-- ssf: origin=owner/repo#N -->`.
//!
//! GitHub has one bot identity and no notion of sessions, so nothing in the
//! API says which agent session posted a comment or opened a pull request.
//! The content carries it instead, on the first line of every post: a
//! visible byline (`🤖#N`, which GitHub renders and links to the session's
//! item; `🤖owner/repo#N` on another repository; `🤖#N (reviewer)` from a
//! reviewer session) and, on the same line, an invisible HTML comment
//! naming the item whose workspace the post came from. The `gh` shim
//! (`crate::shim`) prepends that line to everything an agent posts; the
//! daemon parses the tag back out of every body it reads, and honours it
//! only there, or on the last non-blank line, where posts made before the
//! byline carried it (the first line wins): a tag anywhere else in a body
//! (a fenced example, a pasted transcript, a quote reply) is content, not
//! the post's own tag. The byline is for people: someone who enrols their own account as the bot
//! can tell a session's posts from their own at a glance, and their own
//! untagged posts reach the agents as a person's.
//!
//! The tag is a list of `key=value` fields after `ssf:`, so later features can
//! add fields without a new syntax. Two fields are defined: `mode=delegate`
//! (the post opened an item that is handed off to a new session rather than
//! kept by the one that opened it) and `role=reviewer` (the post came from
//! the reviewer session of the pull request the origin names, not from the
//! session that wrote it). The byline does not encode the mode.

use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;

use crate::github::{Issue, value_str, value_u64};

const OPEN: &str = "<!--";
const CLOSE: &str = "-->";
const MARK: &str = "ssf:";
/// What every byline starts with.
pub const ROBOT: &str = "\u{1F916}";

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

    /// Is this the reviewer session of its item (`SSF_ROLE=reviewer`)?
    pub fn reviewer_from_env() -> bool {
        std::env::var("SSF_ROLE").is_ok_and(|r| r.trim() == REVIEWER)
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

    /// The visible byline: `🤖#N` on the origin's own repository, `🤖owner/repo#N`
    /// on another (or when `on_repo`, the repository posted to, is not
    /// known), with ` (reviewer)` after it for the reviewer session. GitHub
    /// renders either form as a link to the origin item.
    pub fn byline(&self, on_repo: Option<&str>, reviewer: bool) -> String {
        let item = match on_repo {
            Some(r) if r.trim().eq_ignore_ascii_case(&self.repo) => format!("#{}", self.number),
            _ => self.to_string(),
        };
        if reviewer {
            format!("{ROBOT}{item} ({REVIEWER})")
        } else {
            format!("{ROBOT}{item}")
        }
    }

    /// The line the shim prepends to a post made on `on_repo`: the byline,
    /// then the tag (a hand-off's or the reviewer's when asked).
    pub fn first_line(&self, on_repo: Option<&str>, delegate: bool, reviewer: bool) -> String {
        let tag = if delegate {
            self.delegate_tag()
        } else if reviewer {
            self.reviewer_tag()
        } else {
            self.tag()
        };
        format!("{} {tag}", self.byline(on_repo, reviewer))
    }

    /// The marker for an item this session hands off to a new session.
    pub fn delegate_tag(&self) -> String {
        format!("{OPEN} {MARK} origin={self} {MODE}={DELEGATE} {CLOSE}")
    }

    /// The marker the reviewer session of this item appends to its posts.
    pub fn reviewer_tag(&self) -> String {
        format!("{OPEN} {MARK} origin={self} {ROLE}={REVIEWER} {CLOSE}")
    }

    /// Session id of this item's session (`owner/repo#N`), or of its
    /// reviewer session (`owner/repo#N:reviewer`).
    pub fn session(&self, reviewer: bool) -> String {
        if reviewer {
            format!("{self}:{REVIEWER}")
        } else {
            self.to_string()
        }
    }
}

/// Field naming how the origin session relates to the item it opened.
pub const MODE: &str = "mode";
/// `mode` value for a hand-off: the item gets its own session.
pub const DELEGATE: &str = "delegate";
/// Field naming which of an item's sessions posted: absent for the session
/// that works on it, `reviewer` for the one reviewing it.
pub const ROLE: &str = "role";
/// `role` value for the reviewer session of a pull request.
pub const REVIEWER: &str = "reviewer";

/// A session id as the CLI, subscriber lists and origin tags name it:
/// `owner/repo#N` for the session on an item, `owner/repo#N:reviewer` for
/// the session reviewing pull request N.
pub fn parse_session(s: &str) -> Option<(Origin, bool)> {
    let s = s.trim();
    match s.strip_suffix(&format!(":{REVIEWER}")) {
        Some(item) => Origin::parse(item).map(|o| (o, true)),
        None => Origin::parse(s).map(|o| (o, false)),
    }
}

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

    /// Did the post come from the reviewer session of the item?
    pub fn is_reviewer(&self) -> bool {
        self.fields.get(ROLE).map(String::as_str) == Some(REVIEWER)
    }

    /// The session that made the post: the origin item's own session, or
    /// its reviewer.
    pub fn session(&self) -> String {
        self.origin.session(self.is_reviewer())
    }
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
/// optionally marking the post as a hand-off, or as the reviewer session's.
/// `on_repo` is the repository the post goes to, which decides the byline's
/// form. A body that already starts with this origin's tag is left alone
/// (the agent added the line by hand), except that a hand-written tag
/// without `mode=delegate` is not enough for a hand-off, and one without
/// `role=reviewer` is not enough for a reviewer: the right line goes before
/// it, and the first tag wins when read. A tag of ours that is not on the
/// first line does not count, even at the end where `parse` still accepts
/// the old form, so the body gets the byline at the top anyway.
pub fn stamp_with(
    body: &str,
    origin: &Origin,
    on_repo: Option<&str>,
    delegate: bool,
    reviewer: bool,
) -> String {
    if parse_first(body).is_some_and(|t| {
        &t.origin == origin && (!delegate || t.is_delegate()) && (!reviewer || t.is_reviewer())
    }) {
        return body.to_string();
    }
    let line = origin.first_line(on_repo, delegate, reviewer);
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
/// `🤖#N`, `🤖owner/repo#N`, either with ` (reviewer)`?
fn is_byline(s: &str) -> bool {
    let Some(after) = s.trim_start().strip_prefix(ROBOT) else {
        return false;
    };
    let item_len = after.find(char::is_whitespace).unwrap_or(after.len());
    if item_len == 0 {
        return false;
    }
    let rest = after[item_len..].trim_start();
    let rest = rest.strip_prefix(&format!("({REVIEWER})")).unwrap_or(rest);
    rest.trim().is_empty()
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
    /// Timeline event key -> session (`owner/repo#N`, or
    /// `owner/repo#N:reviewer`) that made the comment or review.
    pub origins: BTreeMap<String, String>,
    /// Posts by the bot that carry no tag (the shim was not in effect where
    /// they were made): event key -> URL. The item body is keyed `body`.
    pub untagged: BTreeMap<String, String>,
}

/// Parse the tags out of the item body and every comment-like event on its
/// timeline, noting the bot's posts that have none. Only the bot's own posts
/// are read: a tag in a human's text is something they quoted or pasted.
pub fn scan(issue: &Issue, timeline: &[Value], bot: &str) -> Scan {
    let mut s = Scan::default();
    let mut body_tag = None;
    let mut note = |key: String, author: &str, body: Option<&str>, url: &str| {
        if !author.eq_ignore_ascii_case(bot) {
            return;
        }
        match parse(body.unwrap_or("")) {
            Some(t) => {
                if key == "body" {
                    body_tag = Some(t.clone());
                }
                s.origins.insert(key, t.session());
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
        stamp_with(body, origin, Some("acme/widgets"), false, false)
    }

    fn line() -> String {
        o().first_line(Some("acme/widgets"), false, false)
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
        assert_eq!(o().byline(Some("acme/widgets"), false), "🤖#12");
        assert_eq!(o().byline(Some("ACME/Widgets"), false), "🤖#12");
        assert_eq!(o().byline(Some("acme/other"), false), "🤖acme/widgets#12");
        assert_eq!(o().byline(None, false), "🤖acme/widgets#12");
        assert_eq!(o().byline(Some("acme/widgets"), true), "🤖#12 (reviewer)");
        assert_eq!(
            o().byline(Some("acme/other"), true),
            "🤖acme/widgets#12 (reviewer)"
        );
        assert_eq!(
            line(),
            "🤖#12 <!-- ssf: origin=acme/widgets#12 -->",
            "byline, then the tag, on one line"
        );
        assert_eq!(
            o().first_line(None, true, false),
            "🤖acme/widgets#12 <!-- ssf: origin=acme/widgets#12 mode=delegate -->",
            "the byline does not encode the mode"
        );
        assert_eq!(
            o().first_line(Some("acme/widgets"), false, true),
            "🤖#12 (reviewer) <!-- ssf: origin=acme/widgets#12 role=reviewer -->"
        );
        // The whole line parses back to the tag.
        assert_eq!(parse(&line()).unwrap().origin, o());
        assert!(
            parse(&o().first_line(None, false, true))
                .unwrap()
                .is_reviewer()
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
        let s = stamp_with("hand this off", &o(), Some("acme/widgets"), true, false);
        assert_eq!(s, format!("🤖#12 {t}\n\nhand this off"));
        assert_eq!(
            stamp_with(&s, &o(), Some("acme/widgets"), true, false),
            s,
            "not stamped twice"
        );
        assert_eq!(
            stamp_with(&s, &o(), Some("acme/widgets"), false, false),
            s,
            "a delegate tag is a tag"
        );
        // A hand-written plain tag does not make a hand-off: the delegate
        // line goes before it and is the one that counts.
        let plain = stamp("x", &o());
        let both = stamp_with(&plain, &o(), Some("acme/widgets"), true, false);
        assert!(both.ends_with(&plain));
        assert!(parse(&both).unwrap().is_delegate());
        assert_eq!(strip(&both), "x");
    }

    #[test]
    fn reviewer_tag_names_the_reviewer_session() {
        let t = o().reviewer_tag();
        assert_eq!(t, "<!-- ssf: origin=acme/widgets#12 role=reviewer -->");
        let parsed = parse(&t).unwrap();
        assert!(parsed.is_reviewer());
        assert!(!parsed.is_delegate());
        assert_eq!(parsed.session(), "acme/widgets#12:reviewer");
        assert_eq!(parse(&o().tag()).unwrap().session(), "acme/widgets#12");
        let s = stamp_with("looks good", &o(), Some("acme/widgets"), false, true);
        assert_eq!(s, format!("🤖#12 (reviewer) {t}\n\nlooks good"));
        assert_eq!(
            stamp_with(&s, &o(), Some("acme/widgets"), false, true),
            s,
            "not stamped twice"
        );
        assert_eq!(strip(&s), "looks good");
        // A plain tag the reviewer wrote by hand is not enough: the reviewer
        // line goes before it and wins.
        let both = stamp_with(&stamp("x", &o()), &o(), Some("acme/widgets"), false, true);
        assert!(parse(&both).unwrap().is_reviewer());
        assert_eq!(strip(&both), "x");
        // Session ids parse back, with or without the role.
        let (org, rev) = parse_session("acme/widgets#12:reviewer").unwrap();
        assert_eq!(org, o());
        assert!(rev);
        let (org, rev) = parse_session(" acme/widgets#12 ").unwrap();
        assert_eq!(org, o());
        assert!(!rev);
        assert_eq!(o().session(true), "acme/widgets#12:reviewer");
        assert_eq!(o().session(false), "acme/widgets#12");
        assert!(parse_session("acme/widgets#12:author").is_none());
        assert!(parse_session("nonsense").is_none());
    }

    #[test]
    fn stamp_prepends_once() {
        let s = stamp("hello  \n", &o());
        assert_eq!(s, "🤖#12 <!-- ssf: origin=acme/widgets#12 -->\n\nhello");
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
            stamp_with("hi", &o(), Some("acme/other"), false, false),
            "🤖acme/widgets#12 <!-- ssf: origin=acme/widgets#12 -->\n\nhi"
        );
        assert_eq!(
            stamp_with("hi", &o(), None, false, false),
            "🤖acme/widgets#12 <!-- ssf: origin=acme/widgets#12 -->\n\nhi"
        );
        // A hand-written first line with the right tag is left alone, byline
        // or not.
        let by_hand = format!("{}\n\nhello", o().tag());
        assert_eq!(stamp(&by_hand, &o()), by_hand);
        let cross = format!("🤖acme/widgets#12 {}\n\nhello", o().tag());
        assert_eq!(stamp(&cross, &o()), cross);
    }

    #[test]
    fn strip_removes_bylines_and_tags_only() {
        let s = stamp("hello\n<!-- keep me -->", &o());
        assert_eq!(strip(&s), "hello\n<!-- keep me -->");
        assert_eq!(strip("plain"), "plain");
        assert_eq!(strip(&o().tag()), "");
        assert_eq!(strip(&line()), "");
        assert_eq!(strip(&o().first_line(None, false, true)), "");
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
            strip("🤖#12 <!-- ssf: origin=a/b#12 --> hello\nmore"),
            "hello\nmore"
        );
        assert_eq!(
            strip("🤖#12 (reviewer) <!-- ssf: origin=a/b#12 role=reviewer --> hello"),
            "hello"
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
        ];
        let s = scan(&issue, &timeline, "Bot");
        assert_eq!(s.origin.as_deref(), Some("a/b#1"));
        assert!(!s.origin_tag.as_ref().unwrap().is_delegate());
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
            Some("a/b#5:reviewer"),
            "the reviewer session is told apart from the item's own"
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
