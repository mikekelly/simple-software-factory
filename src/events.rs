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
    /// A handover (`ssf handover`) ended the session that was on the item
    /// and started this one in the same workspace; `from` is the display
    /// name of the harness it was handed over from.
    HandedOver { launch: Launch, from: String },
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
    /// The hold ended because the item was handed over: the session it
    /// was held for is gone, and the one on the item now is another
    /// harness's (`ssf handover`).
    HandedOver,
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
            Self::HandedOver => "handed over",
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
    /// Deliveries are held: the harness is not signed in, or could not
    /// be started at all. `reason` says which (`not signed in`, `could
    /// not be started: <error>`), `fix` what a person does about it.
    Blocked {
        harness: String,
        reason: String,
        fix: String,
    },
    /// The hold is over; `held` is how long it lasted.
    Unblocked {
        harness: String,
        held: std::time::Duration,
        conversation: Conversation,
    },
    /// The binding was dropped after `failures` consecutive failures and
    /// the item is re-onboarded.
    GaveUp { failures: u32, last_error: String },
    /// A session handed its item to a new session on another harness,
    /// model or effort (`ssf handover`): `from` is what it ran with,
    /// `to` what the new one runs with. `by` is the session that asked
    /// (`None` for a person at a terminal). A `refused` handover was
    /// recorded and then could not be carried out; nothing changed.
    HandedOver {
        from: Launch,
        to: Launch,
        summary: bool,
        by: Option<String>,
        refused: Option<String>,
    },
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
            Self::HandedOver { .. } => "handed-over",
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
            Self::Attached(Attach::HandedOver { launch, from }) => {
                let mut lines = launch.lines();
                lines.push(("handed over from", from.clone()));
                (format!("attaching agent to {item_kind}"), lines)
            }
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
            Self::Blocked {
                harness,
                reason,
                fix,
            } => (
                format!("holding deliveries to agent on {item_kind}"),
                vec![
                    ("harness", harness.clone()),
                    ("reason", reason.clone()),
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
            Self::HandedOver {
                from,
                to,
                summary,
                by,
                refused,
            } => {
                let who = match by {
                    Some(session) => session.clone(),
                    None => "a person at the terminal".to_string(),
                };
                let mut lines = Vec::new();
                if refused.is_none() {
                    lines.push(("from", from.harness.clone()));
                    lines.push(("from model", from.model_value()));
                    lines.push(("from effort", from.effort_value()));
                    // The model and effort lines say `the command's` when
                    // a command is configured, so the command is named
                    // here as it is in the `attached` block.
                    if let Some(c) = set(&from.command) {
                        lines.push(("from command", c));
                    }
                }
                lines.push(("to", to.harness.clone()));
                lines.push(("to model", to.model_value()));
                lines.push(("to effort", to.effort_value()));
                if let Some(c) = set(&to.command) {
                    lines.push(("to command", c));
                }
                if refused.is_none() {
                    lines.push(("summary", if *summary { "yes" } else { "no" }.into()));
                }
                lines.push(("by", who));
                match refused {
                    None => (format!("handing over {item_kind}"), lines),
                    Some(why) => {
                        lines.push(("refused", one_line(why)));
                        (format!("not handing over {item_kind}"), lines)
                    }
                }
            }
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

/// A setting as it was given, or nothing when it is blank.
fn set(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl Launch {
    /// What decides an unset model or effort: the configured command when
    /// there is one, else the harness itself.
    fn fallback(&self) -> &'static str {
        if set(&self.command).is_some() {
            COMMAND_DEFAULT
        } else {
            HARNESS_DEFAULT
        }
    }

    /// The model line's value: as configured, or what decides it.
    pub fn model_value(&self) -> String {
        set(&self.model).unwrap_or_else(|| self.fallback().into())
    }

    /// The effort line's value: as configured, or what decides it.
    pub fn effort_value(&self) -> String {
        set(&self.effort).unwrap_or_else(|| self.fallback().into())
    }

    fn lines(&self) -> Vec<(&'static str, String)> {
        let command = set(&self.command);
        let mut lines = vec![
            ("harness", self.harness.clone()),
            ("model", self.model_value()),
            ("effort", self.effort_value()),
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

/// `v` as one `key: value` line carries it: whitespace (newlines
/// included) collapsed to single spaces, backticks dropped (three in a
/// row would close the fence, and markdown ones render literally inside
/// it), cut at `MAX_VALUE_CHARS`. Public so that text checked before it
/// is posted (`Engine::safe_error`) is checked in the form it is posted.
pub fn one_line(v: &str) -> String {
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

/// `one_line`, with nothing left rendered as `(empty)` rather than a
/// bare `key: `.
fn value(v: &str) -> String {
    let out = one_line(v);
    if out.is_empty() {
        "(empty)".into()
    } else {
        out
    }
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
    fn handed_over_names_both_ends() {
        let ev = Event::HandedOver {
            from: launch(Some("bot/issue-12-fix")),
            to: Launch {
                harness: "Pi".into(),
                model: None,
                effort: None,
                ..launch(None)
            },
            summary: true,
            by: Some("acme/widgets#12".into()),
            refused: None,
        };
        let text = comment(&o(), "issue", &ev);
        check_shape(&text, "handed-over");
        assert_eq!(
            text,
            "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=handed-over -->\n\n\
             ```ssf\n\
             ssf handing over issue:\n\
             from: Claude Code\n\
             from model: fable-5.1\n\
             from effort: high\n\
             to: Pi\n\
             to model: the harness's default\n\
             to effort: the harness's default\n\
             summary: yes\n\
             by: acme/widgets#12\n\
             ```"
        );
        // No summary, and nobody's session behind it.
        let ev = Event::HandedOver {
            from: launch(Some("bot/issue-12-fix")),
            to: Launch {
                harness: "Pi".into(),
                model: None,
                effort: None,
                ..launch(None)
            },
            summary: false,
            by: None,
            refused: None,
        };
        assert!(
            ev.block("pull request")
                .starts_with("```ssf\nssf handing over pull request:\n"),
            "{}",
            ev.block("pull request")
        );
        assert!(
            ev.block("issue")
                .ends_with("summary: no\nby: a person at the terminal\n```"),
            "{}",
            ev.block("issue")
        );
    }

    /// A configured command decides an unset model or effort, so the
    /// block names it on the side that has one, as the `attached` block
    /// does: without it `the command's` refers to nothing.
    #[test]
    fn a_handover_names_a_configured_command_on_each_side() {
        let ev = Event::HandedOver {
            from: Launch {
                model: None,
                effort: None,
                command: Some("claude --model opus".into()),
                ..launch(Some("bot/issue-12-fix"))
            },
            to: Launch {
                harness: "Codex".into(),
                model: None,
                effort: Some("medium".into()),
                command: Some("codex --search".into()),
                ..launch(None)
            },
            summary: true,
            by: None,
            refused: None,
        };
        check_shape(&comment(&o(), "issue", &ev), "handed-over");
        assert_eq!(
            ev.block("issue"),
            "```ssf\n\
             ssf handing over issue:\n\
             from: Claude Code\n\
             from model: the command's\n\
             from effort: the command's\n\
             from command: claude --model opus\n\
             to: Codex\n\
             to model: the command's\n\
             to effort: medium\n\
             to command: codex --search\n\
             summary: yes\n\
             by: a person at the terminal\n\
             ```"
        );
        // A refusal says nothing about the side that stays.
        let Event::HandedOver { from, to, .. } = ev else {
            unreachable!()
        };
        let refused = Event::HandedOver {
            from,
            to,
            summary: true,
            by: None,
            refused: Some("the item is no longer active".into()),
        };
        assert_eq!(
            refused.block("issue"),
            "```ssf\n\
             ssf not handing over issue:\n\
             to: Codex\n\
             to model: the command's\n\
             to effort: medium\n\
             to command: codex --search\n\
             by: a person at the terminal\n\
             refused: the item is no longer active\n\
             ```"
        );
    }

    #[test]
    fn a_refused_handover_says_only_what_it_would_have_been() {
        let ev = Event::HandedOver {
            from: launch(Some("bot/issue-12-fix")),
            to: Launch {
                harness: "Codex".into(),
                model: Some("gpt-6".into()),
                effort: None,
                ..launch(None)
            },
            summary: true,
            by: Some("acme/widgets#12".into()),
            refused: Some("could not stop the running agent:\nno such terminal".into()),
        };
        let text = comment(&o(), "issue", &ev);
        check_shape(&text, "handed-over");
        assert_eq!(
            ev.block("issue"),
            "```ssf\n\
             ssf not handing over issue:\n\
             to: Codex\n\
             to model: gpt-6\n\
             to effort: the harness's default\n\
             by: acme/widgets#12\n\
             refused: could not stop the running agent: no such terminal\n\
             ```"
        );
    }

    #[test]
    fn attached_after_a_handover_names_the_harness_it_came_from() {
        let ev = Event::Attached(Attach::HandedOver {
            launch: Launch {
                harness: "Pi".into(),
                model: Some("openai/gpt-6".into()),
                effort: None,
                ..launch(Some("bot/issue-12-fix"))
            },
            from: "Claude Code".into(),
        });
        let text = comment(&o(), "issue", &ev);
        check_shape(&text, "attached");
        assert_eq!(
            ev.block("issue"),
            "```ssf\n\
             ssf attaching agent to issue:\n\
             harness: Pi\n\
             model: openai/gpt-6\n\
             effort: the harness's default\n\
             driver: herdr\n\
             branch: bot/issue-12-fix\n\
             handed over from: Claude Code\n\
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
            reason: "not signed in".into(),
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
        // The other reason: the harness never came up (see
        // `Engine::finish_handover`).
        let ev = Event::Blocked {
            harness: "Pi".into(),
            reason: "could not be started: pi exited at once".into(),
            fix:
                "start Pi by hand in the workspace, or fix the model or effort and hand over again"
                    .into(),
        };
        let text = comment(&o(), "issue", &ev);
        check_shape(&text, "blocked");
        assert_eq!(
            text,
            "🤖 ssf <!-- ssf: origin=acme/widgets#12 event=blocked -->\n\n\
             ```ssf\n\
             ssf holding deliveries to agent on issue:\n\
             harness: Pi\n\
             reason: could not be started: pi exited at once\n\
             fix: start Pi by hand in the workspace, or fix the model or effort and hand over again\n\
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
        // A hold closed by a handover: the conversation is neither kept
        // nor resumed, it belongs to the session that has gone.
        let over = Event::Unblocked {
            harness: "Claude Code".into(),
            held: Duration::from_secs(3 * 60 * 60),
            conversation: Conversation::HandedOver,
        };
        assert!(
            over.block("issue")
                .contains("held for: 180 min\nconversation: handed over\n"),
            "{}",
            over.block("issue")
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
        let nothing = Event::GaveUp {
            failures: 5,
            last_error: " `` \n\t".into(),
        };
        assert!(
            nothing.block("issue").contains("\nlast error: (empty)\n"),
            "{}",
            nothing.block("issue")
        );
        assert_eq!(one_line("  a\n`b`  c "), "a b c");
        assert_eq!(one_line(" ` "), "");
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
                reason: "not signed in".into(),
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
            Event::HandedOver {
                from: launch(None),
                to: launch(None),
                summary: false,
                by: None,
                refused: None,
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
                "handed-over",
                "released"
            ]
        );
    }
}
