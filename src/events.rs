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
mod tests;
