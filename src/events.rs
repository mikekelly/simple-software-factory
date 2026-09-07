//! The daemon's own posts on an item: one short comment per essential
//! event, so a person reading the issue can tell that ssf attached a
//! session to it, brought the session back, held its deliveries for a
//! login, gave a binding up or released its workspace, without the
//! daemon's journal.
//!
//! Every post has the same shape: the `🤖 ssf` byline with the origin tag
//! carrying `event=<name>` (`origin::Origin::event_line`), a blank line,
//! then one fenced `ssf` block: a header line `ssf <doing what> <item>:`
//! and `key: value` lines, one per line, no prose, no blank lines. The tag
//! is how the rest of ssf tells such a post from a session's or a
//! person's: it is never delivered to an agent and never counted as a
//! session's post (`origin::scan`, `Engine::diff`). Posting is switched by
//! `daemon.event_comments` and the per-repository `event_comments`.

use crate::origin::Origin;

/// What the harness was started with, as the `attached` post says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The harness's display name (`login::display_name`).
    pub harness: String,
    /// The configured model; `None` reads as the harness's default.
    pub model: Option<String>,
    /// The configured effort level; `None` reads as the harness's default.
    pub effort: Option<String>,
    /// The configured command that starts the harness, when there is one:
    /// then an unset model or effort is whatever that command says.
    pub command: Option<String>,
    /// The driver id the workspace was made under (`orca`, `herdr`).
    pub driver: String,
    /// The workspace's branch, when known.
    pub branch: Option<String>,
}

/// How an item got its agent: the three occasions of `attached`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attach {
    /// A harness was started in a workspace made for the item; a delegated
    /// item names the session that handed it off.
    Started {
        launch: Launch,
        handed_off_from: Option<String>,
    },
    /// The item was bound to another item's session (nothing was started).
    Bound { session: String, shares: u64 },
    /// The item was onboarded onto a workspace it already had (after a
    /// dropped binding, or a lost state file): the harness in it was
    /// started again, or was still there (`Conversation::Kept`).
    Kept {
        launch: Launch,
        handed_off_from: Option<String>,
        conversation: Conversation,
    },
    /// The workspace had to be re-created and the harness started in it
    /// again; `reason` is the word for why (`workspace gone`, `driver
    /// switch`).
    ReCreated {
        launch: Launch,
        reason: &'static str,
        conversation: Conversation,
    },
}

/// What became of the harness's conversation when it was started again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conversation {
    /// Picked up where it left off (`--resume` with a captured session id).
    Resumed,
    /// Started from scratch, with the item's story.
    Fresh,
    /// The harness was not started again at all: it carried on.
    Kept,
}

impl Conversation {
    /// From a delivery's `resumed` flag, for a harness that was started again.
    pub fn of(resumed: bool) -> Self {
        if resumed { Self::Resumed } else { Self::Fresh }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Resumed => "resumed",
            Self::Fresh => "fresh",
            Self::Kept => "kept",
        }
    }
}

/// The occasions the daemon speaks up on, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A session was started, bound or re-created for the item.
    Attached(Attach),
    /// The harness was started again in its existing workspace: `after`
    /// says why (`restart` for the daemon's startup pass, `lost terminal`
    /// otherwise).
    Resumed {
        harness: String,
        conversation: Conversation,
        after: &'static str,
    },
    /// Deliveries are held: the harness is not signed in; `fix` is what a
    /// person runs (`login::how_to_sign_in`).
    Blocked { harness: String, fix: String },
    /// The hold is over; `held` is how long it lasted.
    Unblocked {
        harness: String,
        held: std::time::Duration,
        conversation: Conversation,
    },
    /// The binding was dropped after `failures` consecutive failures and
    /// the item is re-onboarded.
    GaveUp { failures: u32, last_error: String },
    /// The workspace was removed, `by` `ssf release` or `ssf purge`.
    Released {
        by: &'static str,
        forced: bool,
        branch: Option<String>,
    },
}

/// What a model or effort line says when nothing is configured.
const HARNESS_DEFAULT: &str = "the harness's default";
/// The same when a command is configured: it decides.
const COMMAND_DEFAULT: &str = "the command's";
/// Values longer than this are cut (with an ellipsis), so a pasted error
/// cannot turn the block into a wall.
const MAX_VALUE_CHARS: usize = 200;

impl Event {
    /// The tag's `event=` value.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Attached(_) => "attached",
            Self::Resumed { .. } => "resumed",
            Self::Blocked { .. } => "blocked",
            Self::Unblocked { .. } => "unblocked",
            Self::GaveUp { .. } => "gave-up",
            Self::Released { .. } => "released",
        }
    }

    /// The fenced block: header, then `key: value` lines. `item_kind` is
    /// `issue` or `pull request`.
    pub fn block(&self, item_kind: &str) -> String {
        let (header, lines) = self.header_and_lines(item_kind);
        let mut out = format!("```ssf\nssf {header}:\n");
        for (key, val) in lines {
            out.push_str(&format!("{key}: {}\n", value(&val)));
        }
        out.push_str("```");
        out
    }

    fn header_and_lines(&self, item_kind: &str) -> (String, Vec<(&'static str, String)>) {
        match self {
            Self::Attached(Attach::Started {
                launch,
                handed_off_from,
            }) => {
                let mut lines = launch.lines();
                if let Some(parent) = handed_off_from {
                    lines.push(("handed off from", parent.clone()));
                }
                (format!("attaching agent to {item_kind}"), lines)
            }
            Self::Attached(Attach::Bound { session, shares }) => (
                format!("attaching agent to {item_kind}"),
                vec![
                    ("session", session.clone()),
                    ("shares", format!("workspace of #{shares}")),
                ],
            ),
            Self::Attached(Attach::ReCreated {
                launch,
                reason,
                conversation,
            }) => {
                let mut lines = launch.lines();
                lines.push(("re-created", reason.to_string()));
                lines.push(("conversation", conversation.as_str().into()));
                (format!("attaching agent to {item_kind} again"), lines)
            }
            Self::Attached(Attach::Kept {
                launch,
                handed_off_from,
                conversation,
            }) => {
                let mut lines = launch.lines();
                if let Some(parent) = handed_off_from {
                    lines.push(("handed off from", parent.clone()));
                }
                lines.push(("workspace", "kept".into()));
                lines.push(("conversation", conversation.as_str().into()));
                (format!("attaching agent to {item_kind} again"), lines)
            }
            Self::Resumed {
                harness,
                conversation,
                after,
            } => (
                format!("resuming agent on {item_kind}"),
                vec![
                    ("harness", harness.clone()),
                    ("conversation", conversation.as_str().into()),
                    ("after", after.to_string()),
                ],
            ),
            Self::Blocked { harness, fix } => (
                format!("holding deliveries to agent on {item_kind}"),
                vec![
                    ("harness", harness.clone()),
                    ("reason", "not signed in".into()),
                    ("fix", fix.clone()),
                ],
            ),
            Self::Unblocked {
                harness,
                held,
                conversation,
            } => (
                format!("resuming deliveries to agent on {item_kind}"),
                vec![
                    ("harness", harness.clone()),
                    ("held for", held_for(*held)),
                    ("conversation", conversation.as_str().into()),
                ],
            ),
            Self::GaveUp {
                failures,
                last_error,
            } => (
                format!("giving up on agent binding for {item_kind}"),
                vec![
                    ("failures", failures.to_string()),
                    ("last error", last_error.clone()),
                    ("next", "re-onboarding the item".into()),
                ],
            ),
            Self::Released { by, forced, branch } => {
                let mut lines = vec![("by", by.to_string())];
                if *forced {
                    lines.push(("forced", "yes".into()));
                }
                if let Some(b) = branch {
                    lines.push(("branch", short_branch(b)));
                }
                (format!("releasing workspace of {item_kind}"), lines)
            }
        }
    }
}

impl Launch {
    fn lines(&self) -> Vec<(&'static str, String)> {
        let set = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let command = set(&self.command);
        let fallback = if command.is_some() {
            COMMAND_DEFAULT
        } else {
            HARNESS_DEFAULT
        };
        let or_default = |v: &Option<String>| set(v).unwrap_or_else(|| fallback.into());
        let mut lines = vec![
            ("harness", self.harness.clone()),
            ("model", or_default(&self.model)),
            ("effort", or_default(&self.effort)),
        ];
        if let Some(c) = command {
            lines.push(("command", c));
        }
        lines.push(("driver", self.driver.clone()));
        if let Some(b) = &self.branch {
            lines.push(("branch", short_branch(b)));
        }
        lines
    }
}

/// `N min`, or `less than a minute`.
fn held_for(held: std::time::Duration) -> String {
    match held.as_secs() / 60 {
        0 => "less than a minute".into(),
        m => format!("{m} min"),
    }
}

/// A branch without its `refs/heads/` prefix.
fn short_branch(branch: &str) -> String {
    branch
        .strip_prefix("refs/heads/")
        .unwrap_or(branch)
        .to_string()
}

/// `v` fit for one `key: value` line: whitespace (newlines included)
/// collapsed to single spaces, backticks dropped (three in a row would
/// close the fence, and markdown ones render literally inside it), cut
/// at `MAX_VALUE_CHARS`.
fn value(v: &str) -> String {
    let mut out: String = v
        .replace('`', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if out.chars().count() > MAX_VALUE_CHARS {
        out = out.chars().take(MAX_VALUE_CHARS).collect();
        out.push('\u{2026}');
    }
    out
}

/// The whole comment: the byline line with the tag, a blank line, the
/// block. `origin` is the item the comment goes on.
pub fn comment(origin: &Origin, item_kind: &str, event: &Event) -> String {
    format!(
        "{}\n\n{}",
        origin.event_line(event.name()),
        event.block(item_kind)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn o() -> Origin {
        Origin::new("acme/widgets", 12).unwrap()
    }

    fn launch(branch: Option<&str>) -> Launch {
        Launch {
            harness: "Claude Code".into(),
            model: Some("fable-5.1".into()),
            effort: Some("high".into()),
            command: None,
            driver: "herdr".into(),
            branch: branch.map(str::to_string),
        }
    }

    /// The shape every post shares: byline line, blank line, one fenced
    /// block whose lines are all non-blank `key: value` lines under an
    /// `ssf ...:` header.
    fn check_shape(text: &str, event: &str) {
        let mut lines = text.lines();
        assert_eq!(
            lines.next().unwrap(),
            format!("🤖 ssf <!-- ssf: origin=acme/widgets#12 event={event} -->")
        );
        assert_eq!(lines.next().unwrap(), "");
        assert_eq!(lines.next().unwrap(), "```ssf");
        let header = lines.next().unwrap();
        assert!(
            header.starts_with("ssf ") && header.ends_with(':'),
            "{header}"
        );
        let body: Vec<&str> = lines.collect();
        assert_eq!(body.last().copied(), Some("```"));
        for l in &body[..body.len() - 1] {
            assert!(!l.trim().is_empty(), "blank line inside the block:\n{text}");
            let (k, v) = l.split_once(": ").expect(l);
            assert!(!k.is_empty() && !v.is_empty() && !v.contains('\n'), "{l}");
        }
        assert!(!text.ends_with('\n'));
        // The whole thing parses back as an event post, not a session's.
        let tag = crate::origin::parse(text).unwrap();
        assert_eq!(tag.origin, o());
        assert_eq!(tag.event(), Some(event));
        assert!(crate::origin::is_event_post(text));
    }

    #[test]
    fn attached_names_the_launch() {
        let ev = Event::Attached(Attach::Started {
            launch: launch(Some("refs/heads/bot/issue-12-fix")),
            handed_off_from: None,
        });
        let text = comment(&o(), "issue", &ev);
        check_shape(&text, "attached");
        assert_eq!(
            text,
            "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=attached -->\n\n\
             ```ssf\n\
             ssf attaching agent to issue:\n\
             harness: Claude Code\n\
             model: fable-5.1\n\
             effort: high\n\
             driver: herdr\n\
             branch: bot/issue-12-fix\n\
             ```"
        );
        // Unset model and effort read as the harness's default; no branch,
        // no line; a delegated item names its parent.
        let ev = Event::Attached(Attach::Started {
            launch: Launch {
                model: None,
                effort: Some("  ".into()),
                branch: None,
                ..launch(None)
            },
            handed_off_from: Some("acme/widgets#4".into()),
        });
        let text = comment(&o(), "issue", &ev);
        check_shape(&text, "attached");
        assert_eq!(
            ev.block("issue"),
            "```ssf\n\
             ssf attaching agent to issue:\n\
             harness: Claude Code\n\
             model: the harness's default\n\
             effort: the harness's default\n\
             driver: herdr\n\
             handed off from: acme/widgets#4\n\
             ```"
        );
    }

    #[test]
    fn a_configured_command_is_named_and_decides_the_defaults() {
        let ev = Event::Attached(Attach::Started {
            launch: Launch {
                model: None,
                effort: None,
                command: Some("claude --dangerously-skip-permissions --model opus".into()),
                ..launch(None)
            },
            handed_off_from: None,
        });
        assert_eq!(
            ev.block("issue"),
            "```ssf\n\
             ssf attaching agent to issue:\n\
             harness: Claude Code\n\
             model: the command's\n\
             effort: the command's\n\
             command: claude --dangerously-skip-permissions --model opus\n\
             driver: herdr\n\
             ```"
        );
        // Set alongside a command, model and effort are named as given.
        let ev = Event::Attached(Attach::Started {
            launch: Launch {
                command: Some("  claude  ".into()),
                ..launch(Some("bot/x"))
            },
            handed_off_from: None,
        });
        assert!(
            ev.block("issue")
                .contains("model: fable-5.1\neffort: high\ncommand: claude\ndriver: herdr\n")
        );
    }

    #[test]
    fn bound_names_the_owning_session() {
        let ev = Event::Attached(Attach::Bound {
            session: "acme/widgets#4".into(),
            shares: 4,
        });
        let text = comment(&o(), "pull request", &ev);
        check_shape(&text, "attached");
        assert_eq!(
            ev.block("pull request"),
            "```ssf\n\
             ssf attaching agent to pull request:\n\
             session: acme/widgets#4\n\
             shares: workspace of #4\n\
             ```"
        );
    }

    #[test]
    fn re_created_says_why_and_how_the_conversation_went() {
        let ev = Event::Attached(Attach::ReCreated {
            launch: launch(Some("bot/issue-12-fix")),
            reason: "driver switch",
            conversation: Conversation::of(false),
        });
        let text = comment(&o(), "issue", &ev);
        check_shape(&text, "attached");
        assert_eq!(
            ev.block("issue"),
            "```ssf\n\
             ssf attaching agent to issue again:\n\
             harness: Claude Code\n\
             model: fable-5.1\n\
             effort: high\n\
             driver: herdr\n\
             branch: bot/issue-12-fix\n\
             re-created: driver switch\n\
             conversation: fresh\n\
             ```"
        );
    }

    #[test]
    fn kept_says_the_workspace_was_there_already() {
        let ev = Event::Attached(Attach::Kept {
            launch: launch(Some("bot/issue-12-fix")),
            handed_off_from: Some("acme/widgets#4".into()),
            conversation: Conversation::Kept,
        });
        let text = comment(&o(), "issue", &ev);
        check_shape(&text, "attached");
        assert_eq!(
            ev.block("issue"),
            "```ssf\n\
             ssf attaching agent to issue again:\n\
             harness: Claude Code\n\
             model: fable-5.1\n\
             effort: high\n\
             driver: herdr\n\
             branch: bot/issue-12-fix\n\
             handed off from: acme/widgets#4\n\
             workspace: kept\n\
             conversation: kept\n\
             ```"
        );
    }

    #[test]
    fn resumed_says_after_what() {
        let ev = Event::Resumed {
            harness: "Codex".into(),
            conversation: Conversation::of(true),
            after: "restart",
        };
        let text = comment(&o(), "issue", &ev);
        check_shape(&text, "resumed");
        assert_eq!(
            text,
            "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=resumed -->\n\n\
             ```ssf\n\
             ssf resuming agent on issue:\n\
             harness: Codex\n\
             conversation: resumed\n\
             after: restart\n\
             ```"
        );
    }

    #[test]
    fn blocked_and_unblocked() {
        let ev = Event::Blocked {
            harness: "Claude Code".into(),
            fix: "`claude auth login` on the host".into(),
        };
        let text = comment(&o(), "issue", &ev);
        check_shape(&text, "blocked");
        // Markdown backticks would render literally inside the fence.
        assert_eq!(
            text,
            "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=blocked -->\n\n\
             ```ssf\n\
             ssf holding deliveries to agent on issue:\n\
             harness: Claude Code\n\
             reason: not signed in\n\
             fix: claude auth login on the host\n\
             ```"
        );
        let ev = Event::Unblocked {
            harness: "Claude Code".into(),
            held: Duration::from_secs(12 * 60 + 30),
            conversation: Conversation::Resumed,
        };
        let text = comment(&o(), "issue", &ev);
        check_shape(&text, "unblocked");
        assert_eq!(
            text,
            "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=unblocked -->\n\n\
             ```ssf\n\
             ssf resuming deliveries to agent on issue:\n\
             harness: Claude Code\n\
             held for: 12 min\n\
             conversation: resumed\n\
             ```"
        );
        let quick = Event::Unblocked {
            harness: "Claude Code".into(),
            held: Duration::from_secs(59),
            conversation: Conversation::Kept,
        };
        assert!(
            quick
                .block("pull request")
                .contains("held for: less than a minute\nconversation: kept\n")
        );
    }

    #[test]
    fn gave_up_keeps_the_error_on_one_line() {
        let ev = Event::GaveUp {
            failures: 5,
            last_error: "orca worktree deliver failed:\n  exit status 1\n\tno such terminal".into(),
        };
        let text = comment(&o(), "issue", &ev);
        check_shape(&text, "gave-up");
        assert_eq!(
            text,
            "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=gave-up -->\n\n\
             ```ssf\n\
             ssf giving up on agent binding for issue:\n\
             failures: 5\n\
             last error: orca worktree deliver failed: exit status 1 no such terminal\n\
             next: re-onboarding the item\n\
             ```"
        );
        // Backticks go: three in a row would end the block early.
        let hostile = Event::GaveUp {
            failures: 5,
            last_error: "said:\n```\nboom\n```\nand `more`".into(),
        };
        let block = hostile.block("issue");
        assert!(
            block.contains("\nlast error: said: boom and more\n"),
            "{block}"
        );
        assert_eq!(block.matches("```").count(), 2, "{block}");
        let long = Event::GaveUp {
            failures: 5,
            last_error: "x".repeat(500),
        };
        let block = long.block("issue");
        let line = block
            .lines()
            .find(|l| l.starts_with("last error: "))
            .unwrap();
        assert_eq!(
            line.chars().count(),
            "last error: ".len() + MAX_VALUE_CHARS + 1
        );
        assert!(line.ends_with('\u{2026}'));
    }

    #[test]
    fn released_says_by_whom() {
        let ev = Event::Released {
            by: "ssf release",
            forced: false,
            branch: Some("refs/heads/bot/issue-12-fix".into()),
        };
        let text = comment(&o(), "issue", &ev);
        check_shape(&text, "released");
        assert_eq!(
            text,
            "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=released -->\n\n\
             ```ssf\n\
             ssf releasing workspace of issue:\n\
             by: ssf release\n\
             branch: bot/issue-12-fix\n\
             ```"
        );
        let forced = Event::Released {
            by: "ssf purge",
            forced: true,
            branch: None,
        };
        check_shape(&comment(&o(), "pull request", &forced), "released");
        assert_eq!(
            forced.block("pull request"),
            "```ssf\n\
             ssf releasing workspace of pull request:\n\
             by: ssf purge\n\
             forced: yes\n\
             ```"
        );
    }

    #[test]
    fn names_are_the_tag_values() {
        let names: Vec<&str> = [
            Event::Attached(Attach::Bound {
                session: "a/b#1".into(),
                shares: 1,
            }),
            Event::Resumed {
                harness: "x".into(),
                conversation: Conversation::Fresh,
                after: "lost terminal",
            },
            Event::Blocked {
                harness: "x".into(),
                fix: "y".into(),
            },
            Event::Unblocked {
                harness: "x".into(),
                held: Duration::ZERO,
                conversation: Conversation::Kept,
            },
            Event::GaveUp {
                failures: 1,
                last_error: "e".into(),
            },
            Event::Released {
                by: "ssf release",
                forced: false,
                branch: None,
            },
        ]
        .iter()
        .map(Event::name)
        .collect();
        assert_eq!(
            names,
            vec![
                "attached",
                "resumed",
                "blocked",
                "unblocked",
                "gave-up",
                "released"
            ]
        );
    }
}
