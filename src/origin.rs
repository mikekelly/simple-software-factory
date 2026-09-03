//! Origin tags: `<!-- ssf: origin=owner/repo#N -->`.
//!
//! GitHub has one bot identity and no notion of sessions, so nothing in the
//! API says which agent session posted a comment or opened a pull request.
//! The content carries it instead: an invisible HTML comment naming the item
//! whose workspace the post came from. The `gh` shim (`crate::shim`) appends
//! it to everything an agent posts; the daemon parses it back out of every
//! body it reads.
//!
//! The tag is a list of `key=value` fields after `ssf:`, so later features can
//! add fields without a new syntax. The one field defined so far is
//! `mode=delegate`: the post opened an item that is handed off to a new
//! session rather than kept by the one that opened it.

use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;

use crate::github::{Issue, value_str, value_u64};

const OPEN: &str = "<!--";
const CLOSE: &str = "-->";
const MARK: &str = "ssf:";

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

    /// The marker the shim appends.
    pub fn tag(&self) -> String {
        format!("{OPEN} {MARK} origin={self} {CLOSE}")
    }

    /// The marker for an item this session hands off to a new session.
    pub fn delegate_tag(&self) -> String {
        format!("{OPEN} {MARK} origin={self} {MODE}={DELEGATE} {CLOSE}")
    }
}

/// Field naming how the origin session relates to the item it opened.
pub const MODE: &str = "mode";
/// `mode` value for a hand-off: the item gets its own session.
pub const DELEGATE: &str = "delegate";

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

/// The tag that identifies the post: the last one, since the shim appends
/// its own after anything the author quoted.
pub fn parse(body: &str) -> Option<Tag> {
    tags(body).pop()
}

/// `body` with the origin tag on its own final line, optionally marking the
/// post as a hand-off. A body that already carries this origin is left alone
/// (the agent added the tag by hand), except that a hand-written tag without
/// `mode=delegate` is not enough for a hand-off: the delegate tag goes after
/// it, and the last tag wins when read.
pub fn stamp_with(body: &str, origin: &Origin, delegate: bool) -> String {
    if tags(body)
        .iter()
        .any(|t| &t.origin == origin && (!delegate || t.is_delegate()))
    {
        return body.to_string();
    }
    let tag = if delegate {
        origin.delegate_tag()
    } else {
        origin.tag()
    };
    let trimmed = body.trim_end();
    if trimmed.is_empty() {
        tag
    } else {
        format!("{trimmed}\n\n{tag}")
    }
}

/// `body` without its ssf tags (for showing the text to an agent).
pub fn strip(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut last = 0;
    for (start, end) in spans(body) {
        let inner = body[start + OPEN.len()..end].trim();
        if !inner.starts_with(MARK) || is_quoted(body, start) {
            continue;
        }
        out.push_str(&body[last..start]);
        last = end + CLOSE.len();
        // Swallow the blank line the shim put before the tag.
        if let Some(stripped) = out.strip_suffix("\n\n") {
            out.truncate(stripped.len());
            out.push('\n');
        }
    }
    out.push_str(&body[last..]);
    out.trim_end().to_string()
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
    /// Timeline event key -> origin of the comment or review.
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
                s.origins.insert(key, t.origin.to_string());
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

    fn stamp(body: &str, origin: &Origin) -> String {
        stamp_with(body, origin, false)
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
    fn parses_extra_fields_and_loose_spacing() {
        let t = parse("hi\n<!--ssf: origin=a/b#3 mode=delegate-->").unwrap();
        assert_eq!(t.origin.to_string(), "a/b#3");
        assert_eq!(t.fields.get("mode").map(String::as_str), Some("delegate"));
    }

    #[test]
    fn last_tag_wins_and_other_comments_are_ignored() {
        let body = "quoting:\n> <!-- ssf: origin=a/b#1 -->\n<!-- plain html comment -->\n\n<!-- ssf: origin=a/b#2 -->";
        assert_eq!(parse(body).unwrap().origin.to_string(), "a/b#2");
        assert_eq!(tags(body).len(), 1, "the quoted tag does not count");
        assert!(parse("<!-- ssf: origin=nonsense -->").is_none());
        // A quote reply carries the quoted post's tag, not one of its own.
        assert!(parse("> hi\n> <!-- ssf: origin=a/b#1 -->\n\nthanks").is_none());
        assert!(parse("  > <!-- ssf: origin=a/b#1 -->").is_none());
        assert_eq!(
            parse("> <!-- ssf: origin=a/b#1 -->\n<!-- ssf: origin=a/b#2 -->")
                .unwrap()
                .origin
                .number,
            2
        );
        assert!(parse("no tag here").is_none());
        assert!(parse("<!-- ssf: origin=a/b#1").is_none());
    }

    #[test]
    fn delegate_tag_marks_a_hand_off() {
        let t = o().delegate_tag();
        assert_eq!(t, "<!-- ssf: origin=acme/widgets#12 mode=delegate -->");
        let parsed = parse(&t).unwrap();
        assert!(parsed.is_delegate());
        assert!(!parse(&o().tag()).unwrap().is_delegate());
        let s = stamp_with("hand this off", &o(), true);
        assert!(s.ends_with(&t));
        assert_eq!(stamp_with(&s, &o(), true), s, "not stamped twice");
        assert_eq!(stamp_with(&s, &o(), false), s, "a delegate tag is a tag");
        // A hand-written plain tag does not make a hand-off: the delegate tag
        // goes after it and is the one that counts.
        let plain = stamp("x", &o());
        let both = stamp_with(&plain, &o(), true);
        assert!(both.starts_with(&plain));
        assert!(parse(&both).unwrap().is_delegate());
        assert_eq!(strip(&both), "x");
    }

    #[test]
    fn stamp_appends_once() {
        let s = stamp("hello  \n", &o());
        assert_eq!(s, "hello\n\n<!-- ssf: origin=acme/widgets#12 -->");
        assert_eq!(stamp(&s, &o()), s);
        assert_eq!(stamp("", &o()), o().tag());
        // A different origin quoted in the body does not count as ours.
        let quoted = "see <!-- ssf: origin=acme/widgets#99 -->";
        assert!(stamp(quoted, &o()).ends_with(&o().tag()));
    }

    #[test]
    fn strip_removes_tags_only() {
        let s = stamp("hello\n<!-- keep me -->", &o());
        assert_eq!(strip(&s), "hello\n<!-- keep me -->");
        assert_eq!(strip("plain"), "plain");
        assert_eq!(strip(&o().tag()), "");
        let quoted = "> <!-- ssf: origin=a/b#1 -->\nreply";
        assert_eq!(strip(quoted), quoted);
        assert_eq!(
            strip("caf\u{e9} \u{1F600}\n\n<!-- ssf: origin=a/b#1 -->"),
            "caf\u{e9} \u{1F600}"
        );
        assert_eq!(strip("<!-- never closed"), "<!-- never closed");
    }

    #[test]
    fn scan_collects_origins_and_untagged_bot_posts() {
        let issue: Issue = serde_json::from_value(json!({
            "number": 5, "title": "t", "body": "opened by a session\n\n<!-- ssf: origin=a/b#1 -->",
            "html_url": "https://gh/5", "state": "open", "user": {"login": "bot"},
            "created_at": "x", "updated_at": "x"
        }))
        .unwrap();
        let timeline = vec![
            json!({"event":"commented","id":1,"user":{"login":"bot"},"body":"tagged <!-- ssf: origin=a/b#1 -->","html_url":"u1"}),
            json!({"event":"commented","id":2,"user":{"login":"bot"},"body":"untagged","html_url":"u2"}),
            json!({"event":"commented","id":3,"user":{"login":"alice"},"body":"human","html_url":"u3"}),
            json!({"event":"commented","id":30,"user":{"login":"alice"},"body":"pasted <!-- ssf: origin=a/b#1 -->","html_url":"u30"}),
            json!({"event":"reviewed","id":4,"user":{"login":"bot"},"body":"<!-- ssf: origin=a/b#7 -->","html_url":"u4"}),
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
            s.untagged.get("commented:2").map(String::as_str),
            Some("u2")
        );
        assert_eq!(
            s.untagged.get("line-commented:8").map(String::as_str),
            Some("u8")
        );
        assert!(!s.untagged.contains_key("commented:3"));
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
        let mut delegated = issue.clone();
        delegated.body = Some("child\n\n<!-- ssf: origin=a/b#1 mode=delegate -->".into());
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
