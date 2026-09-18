//! `/ssf <text>`: a comment that asks the factory itself to do something,
//! rather than telling the item's agent about it.
//!
//! A person writes the command as the first line of a comment on an item:
//!
//! ```text
//! /ssf please assign this to claude fable low
//! ```
//!
//! The request is the rest of that line; what follows on later lines is a
//! person talking to the item's readers, not to ssf. The daemon runs the
//! request as a task (see [`crate::task`]): the repository's harness, in its
//! headless form, with ssf's orientation, the item's story and then the
//! request. Nothing is pasted into the item's session: a command is
//! addressed to the server, not to the agent on the item.
//!
//! Only a comment whose *first non-blank line* is the command counts, so
//! quoting one in a reply does nothing, and the word has to stand alone
//! (`/ssf-x` and `/ssfx` are somebody else's words).

use crate::github::{value_str, value_u64};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The word a command starts with.
pub const COMMAND: &str = "/ssf";

/// One command a person left on an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    /// GitHub's id of the comment that carried it. It is what the item's
    /// record remembers, so a command is run once however often the timeline
    /// is walked.
    pub id: u64,
    /// The login that wrote the comment.
    pub author: String,
    /// The request: the rest of the command's line.
    pub text: String,
}

/// A command that has been taken and is running now, as the item's record
/// keeps it: the request, and the harness it is running on. Written when the
/// task starts and cleared when it ends, so a daemon that stops mid-run can
/// say so on the item when it comes back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Running {
    pub command: Command,
    pub harness: String,
}

/// The request in a comment body, when the body's first non-blank line is a
/// command with something after it.
pub fn parse(body: &str) -> Option<String> {
    let line = body.lines().find(|l| !l.trim().is_empty())?;
    let rest = line.trim_start().strip_prefix(COMMAND)?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let text = rest.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Every command in an item's timeline, oldest first. Plain comments only:
/// a review's body and inline comments are not a place to address the
/// server from.
pub fn commands(timeline: &[Value]) -> Vec<Command> {
    let mut out = Vec::new();
    for ev in timeline {
        if value_str(ev, &["event"]) != Some("commented") {
            continue;
        }
        let (Some(id), Some(body)) = (value_u64(ev, &["id"]), value_str(ev, &["body"])) else {
            continue;
        };
        let Some(text) = parse(body) else { continue };
        out.push(Command {
            id,
            author: value_str(ev, &["user", "login"])
                .unwrap_or("unknown")
                .to_string(),
            text,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_first_line_carries_the_request() {
        assert_eq!(
            parse("/ssf please assign this to claude fable low"),
            Some("please assign this to claude fable low".into())
        );
        // Blank lines and indentation in front are how people write a comment.
        assert_eq!(
            parse("\n\n   /ssf  do the thing  \nand then some prose\n"),
            Some("do the thing".into())
        );
        // Only the first line: what follows is for the item's readers.
        assert_eq!(
            parse("Prose first.\n/ssf not a command\n"),
            None,
            "a command quoted in a reply is not one"
        );
    }

    #[test]
    fn only_a_command_counts() {
        assert_eq!(parse("/ssf"), None, "nothing was asked for");
        assert_eq!(parse("/ssf   "), None);
        assert_eq!(parse("/ssfx do it"), None, "another word that starts alike");
        assert_eq!(parse("/ssf-x do it"), None);
        assert_eq!(parse("see /ssf above"), None);
        assert_eq!(parse("/ssfdo it"), None);
    }

    #[test]
    fn commands_are_read_out_of_a_timeline() {
        let timeline = vec![
            json!({"event": "commented", "id": 1, "user": {"login": "ann"}, "body": "morning"}),
            json!({"event": "commented", "id": 2, "user": {"login": "bob"}, "body": "/ssf take this over"}),
            json!({"event": "reviewed", "id": 3, "user": {"login": "bob"}, "body": "/ssf from a review"}),
            json!({"event": "line-commented", "id": 4, "comments": []}),
            json!({"event": "assigned", "id": 5, "actor": {"login": "ann"}}),
            json!({"event": "commented", "id": 6, "user": {"login": "cat"}, "body": "/ssf\n"}),
        ];
        assert_eq!(
            commands(&timeline),
            vec![Command {
                id: 2,
                author: "bob".into(),
                text: "take this over".into(),
            }]
        );
    }

    #[test]
    fn a_comment_without_an_id_is_not_a_command() {
        let timeline =
            vec![json!({"event": "commented", "user": {"login": "ann"}, "body": "/ssf hi"})];
        assert!(commands(&timeline).is_empty());
    }
}
