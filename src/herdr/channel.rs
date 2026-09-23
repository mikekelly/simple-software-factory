//! How an event reaches a harness running in a herdr pane (#446). Each
//! harness names its [`Channel`] in its descriptor (`harness::Harness`), and
//! [`Herdr::deliver`](super::Herdr::deliver) asks that channel rather than the
//! harness id: a harness with a native channel is its implementation plus one
//! table field. The implementations live beside their protocols --
//! `claude_delivery::Claude`, `codex_delivery::Codex`,
//! `delivery_channel::Mailbox` (OMP and Pi) -- and [`Terminal`] here, the
//! herdr paste every other harness takes.

use anyhow::Result;
use futures_util::future::BoxFuture;
use std::path::Path;

use super::Herdr;

/// Where a native channel keeps its record of an event: the session's
/// mailbox and the event's stable sequence. `None` for a harness the driver
/// has no channel for (`Driver::channel_at`).
pub(crate) type Journal<'a> = Option<(&'a Path, u64)>;

pub(crate) trait Channel: Sync {
    /// Whether the channel addresses one session: a saved pane is then an
    /// address, not a preference, so a delivery never goes to a neighbour,
    /// and an event it has begun, or a session it is bound to, is settled
    /// only in that session resumed, never in a fresh one.
    fn session_bound(&self) -> bool {
        false
    }

    /// Whether a delivery goes through ssf's harness-side bridge and the
    /// launcher that loads it (`delivery_channel::bridge`), which the session
    /// has to be started with.
    fn bridged(&self) -> bool {
        false
    }

    /// Whether a delivery takes a [`Journal`]: the channels that record what
    /// they sent, so an ambiguous send is reconciled rather than resent.
    fn journaled(&self) -> bool {
        true
    }

    /// Give `text` to the harness live in `pane`.
    fn deliver<'a>(
        &'a self,
        herdr: &'a Herdr,
        pane: &'a str,
        journal: Journal<'a>,
        text: &'a str,
    ) -> BoxFuture<'a, Result<()>>;

    /// Whether an earlier attempt at this event is on record, for a
    /// relaunched harness to settle rather than be sent it again.
    fn has_record(&self, _mailbox: &Path, _sequence: u64, _text: &str) -> bool {
        false
    }

    /// Whether the session's channel is bound to one conversation, which
    /// only resuming that conversation reaches.
    fn has_binding(&self, _mailbox: &Path) -> bool {
        false
    }

    /// Deliver to the harness just relaunched in `pane` -- `resumed` when it
    /// took up its saved conversation -- given whether the event was
    /// `recorded` before the relaunch. `false` leaves the event to the
    /// terminal, as a first prompt.
    fn relaunched<'a>(
        &'a self,
        _herdr: &'a Herdr,
        _pane: &'a str,
        _journal: Journal<'a>,
        _recorded: bool,
        _resumed: bool,
        _text: &'a str,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }
}

/// The herdr paste: `agent prompt`, or a raw paste when the harness is at a
/// question or the text is too long for one argument.
pub(crate) struct Terminal;

impl Channel for Terminal {
    fn journaled(&self) -> bool {
        false
    }

    fn deliver<'a>(
        &'a self,
        herdr: &'a Herdr,
        pane: &'a str,
        _journal: Journal<'a>,
        text: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(herdr.send_prompt(pane, text))
    }
}
