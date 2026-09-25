//! Out-of-band delivery shared with harness-side bridges.
//!
//! The daemon publishes one file per event; the bridge injects it into the
//! session and acknowledges the file once the session's transcript records the
//! injected message.  Acknowledged therefore means "the agent has the record
//! of it", but publication is what makes the event durable: the file stays in
//! the mailbox until that record exists, so a harness that exits inside the
//! injection window takes the event again when it is relaunched, exactly as it
//! already reconciles an event whose record is in the resumed transcript
//! (#390).  A delivery that publishes without a receipt is queued in the
//! harness's own mailbox, not lost, and it stays pending for the relaunch path
//! to find.
//!
//! A mailbox no live bridge polls -- its ready marker is gone, or names a
//! process that has exited -- is a [`Hold`] too, not a failure: nothing is
//! published, so the item keeps its events, and the bridge repairs its own
//! marker while it runs, which leaves a session restart as the fix (#395).
//!
//! That contract is the bridge's, so the daemon serves the bridge it was built
//! with rather than whichever file the filesystem happens to hold: the two
//! [`Serving`]s below hand a session this build's own copy whenever the
//! installed one is from another build (#402).

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::Instant;

use crate::herdr::{Channel, Herdr, Journal};

/// How long one attempt waits for the harness to record the event before the
/// pass moves on.  The record lands at the agent's next step boundary, so a
/// session inside a long tool call will not reach it in this window; the wait
/// is long enough to catch an idle session's injection and short enough that a
/// pass is not held behind a busy one.
const RECORD_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a harness just relaunched gets to load its bridge before its
/// mailbox counts as unavailable.  OpenCode draws its TUI before its server
/// loads the plugin, so the marker can trail the pane by a few seconds.
const BRIDGE_START_TIMEOUT: Duration = Duration::from_secs(20);

/// What an attempt got: the transcript holds the event, or the mailbox does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Receipt {
    /// The session's transcript records the injected message.
    Recorded,
    /// Published to the mailbox and not recorded yet.  The bridge keeps the
    /// file until it is, and a relaunch injects it again if the harness went
    /// away first.
    Published,
}

/// A hold on the session's mailbox: a state only the harness can clear.  An
/// event it has not recorded yet, or a bridge that is not there to take one.
/// Neither is a failure -- nothing was lost, and a session that is gone wants
/// a restart, not a dropped binding -- so the caller keeps the item's events
/// un-seen and tries again rather than counting toward giving up on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hold {
    /// An earlier event of this sequence is published and not recorded.
    Unrecorded,
    /// No live bridge attests to this mailbox (its ready marker is absent, or
    /// names a process that has exited).
    Unavailable,
}

/// The hold this error is, if it is one.  Both clear inside the harness -- by
/// recording the event, or by the session coming back with a bridge -- so a
/// pass that meets one reads the item again rather than waiting for a listing
/// change that may be arbitrarily far off.
pub(crate) fn hold(e: &anyhow::Error) -> Option<Hold> {
    e.chain().find_map(|c| {
        if c.downcast_ref::<DeliveryUnrecorded>().is_some() {
            Some(Hold::Unrecorded)
        } else if c.downcast_ref::<DeliveryUnavailable>().is_some() {
            Some(Hold::Unavailable)
        } else {
            None
        }
    })
}

/// The same hold as an error value, for the driver stub: a test that needs a
/// delivery held must produce the real type, since the engine classifies it by
/// downcast.
#[cfg(test)]
pub(crate) fn held(sequence: u64) -> anyhow::Error {
    DeliveryUnrecorded { sequence }.into()
}

/// [`held`] for the mailbox no live bridge attests to.
#[cfg(test)]
pub(crate) fn unavailable(mailbox: &Path) -> anyhow::Error {
    DeliveryUnavailable {
        mailbox: mailbox.to_path_buf(),
    }
    .into()
}

pub(crate) fn mailbox(repo: &str, number: u64) -> PathBuf {
    repo_dir(repo).join(number.to_string())
}

/// A scratch session's mailbox (`owner/repo~id`), beside its repository's
/// items': `~` cannot start an item number, so the two never meet.
pub(crate) fn scratch_mailbox(repo: &str, id: &str) -> PathBuf {
    repo_dir(repo).join(format!("~{id}"))
}

fn repo_dir(repo: &str) -> PathBuf {
    let mut path = crate::config::state_dir().join("delivery");
    for component in repo.split('/') {
        path.push(match component {
            "" => "_empty_",
            "." => "_dot_",
            ".." => "_dot-dot_",
            other => other,
        });
    }
    path
}

// ---- the bridge and the launcher this build ships -------------------------

/// The harness files this build ships, embedded.  The daemon starts a session
/// with the paths it names (`SSF_PI_BRIDGE`, `SSF_PI_LAUNCHER`,
/// `SSF_OPENCODE_BRIDGE`) and the package installs the same files under
/// `share/ssf/harness`; embedding them keeps them in lockstep with the daemon
/// whose mailbox protocol they implement, whatever the filesystem holds.  A factory whose daemon was
/// replaced without its harness files -- a hand-installed binary, the dev build
/// `packaging/dev-install.sh` points the service at while the package's copies
/// stay behind, or a standalone binary install with no share tree at all --
/// otherwise runs a bridge from another build.  That is not a cosmetic
/// mismatch: the ready marker a bridge older than this daemon never repairs is
/// exactly what the daemon reads as "no live bridge", so every event for the
/// item is held, its agent never hears that the item closed, and nothing
/// releases its workspace (#402).
const BRIDGE: &[u8] = include_bytes!("../harness/ssf-delivery.ts");
const LAUNCHER: &[u8] = include_bytes!("../harness/ssf-pi-launch");
/// OpenCode's bridge: the same mailbox contract as an OpenCode plugin, which
/// the launcher loads from `SSF_OPENCODE_BRIDGE`.
const OPENCODE_BRIDGE: &[u8] = include_bytes!("../harness/ssf-opencode.ts");
/// Grok's bridge: the same mailbox contract as a sidecar the launcher runs
/// with node beside the TUI, from `SSF_GROK_BRIDGE`.
const GROK_BRIDGE: &[u8] = include_bytes!("../harness/ssf-grok.mjs");

const BRIDGE_NAME: &str = "harness/ssf-delivery.ts";
const LAUNCHER_NAME: &str = "harness/ssf-pi-launch";
const OPENCODE_BRIDGE_NAME: &str = "harness/ssf-opencode.ts";
const GROK_BRIDGE_NAME: &str = "harness/ssf-grok.mjs";

/// Where a session is pointed for one of those files, and whether that path
/// holds this build's copy.
pub(crate) struct Serving {
    /// The path a session is started with.
    pub path: PathBuf,
    /// False only when `path` does not hold this build's bytes, which is the
    /// one state a session cannot work around: its channel takes no events.
    pub sound: bool,
    /// An installed copy is there and is not this build's: a package and a
    /// binary from different builds, which is worth saying out loud.
    pub skewed: bool,
    /// Why `path` is not the installed copy, when it is not.
    pub note: Option<String>,
}

impl Serving {
    /// What a check line about this says after the path: nothing when the
    /// installed copy is the one this build ships.
    pub(crate) fn detail(&self) -> String {
        match (&self.note, self.sound) {
            (None, _) => String::new(),
            (Some(note), true) => format!("; {note}"),
            (Some(note), false) => format!(
                "; {note}; this ssf's own copy could not be written, so a session started now \
would have no bridge to load"
            ),
        }
    }
}

/// The bridge a session is started with.
pub(crate) fn bridge() -> PathBuf {
    bridge_serving().path
}

/// The launcher a session is started with.
pub(crate) fn launcher() -> PathBuf {
    launcher_serving().path
}

/// The OpenCode plugin a session is started with.
pub(crate) fn opencode_bridge() -> PathBuf {
    opencode_bridge_serving().path
}

pub(crate) fn opencode_bridge_serving() -> Serving {
    served(
        OPENCODE_BRIDGE_NAME,
        OPENCODE_BRIDGE,
        &installed_candidates(OPENCODE_BRIDGE_NAME),
        &crate::config::state_dir(),
    )
}

/// The Grok sidecar a session is started with.
pub(crate) fn grok_bridge() -> PathBuf {
    grok_bridge_serving().path
}

pub(crate) fn grok_bridge_serving() -> Serving {
    served(
        GROK_BRIDGE_NAME,
        GROK_BRIDGE,
        &installed_candidates(GROK_BRIDGE_NAME),
        &crate::config::state_dir(),
    )
}

/// The bridge `harness`'s sessions load: OpenCode's plugin, Grok's sidecar,
/// or the Pi/OMP extension.
pub(crate) fn bridge_serving_for(harness: &str) -> Serving {
    match harness {
        "opencode" => opencode_bridge_serving(),
        "grok" => grok_bridge_serving(),
        _ => bridge_serving(),
    }
}

pub(crate) fn bridge_serving() -> Serving {
    served(
        BRIDGE_NAME,
        BRIDGE,
        &installed_candidates(BRIDGE_NAME),
        &crate::config::state_dir(),
    )
}

pub(crate) fn launcher_serving() -> Serving {
    served(
        LAUNCHER_NAME,
        LAUNCHER,
        &installed_candidates(LAUNCHER_NAME),
        &crate::config::state_dir(),
    )
}

/// Where this platform installs that file, best first (see
/// [`crate::platform::share_candidates`]).
fn installed_candidates(name: &str) -> Vec<PathBuf> {
    let exe = std::env::current_exe().ok();
    crate::platform::share_candidates(
        name,
        exe.as_deref().and_then(Path::parent),
        crate::platform::detect().os,
    )
}

/// The installed copy when it is byte-for-byte this build's file -- the normal
/// case, and the one the package's own paths name -- else this build's own copy
/// under the state directory, so a daemon and a bridge from different builds
/// cannot be paired.
fn served(name: &str, bytes: &[u8], candidates: &[PathBuf], state_dir: &Path) -> Serving {
    let installed = candidates
        .first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from(name));
    if let Some(found) = candidates.iter().find(|path| holds(path, bytes)) {
        return Serving {
            path: found.clone(),
            sound: true,
            skewed: false,
            note: None,
        };
    }
    let skewed = installed.exists();
    let note = if skewed {
        format!(
            "the installed copy at {} is not the one this ssf ships",
            installed.display()
        )
    } else {
        format!("no installed copy at {}", installed.display())
    };
    match own_copy(name, bytes, state_dir) {
        Ok(path) => Serving {
            path,
            sound: true,
            skewed,
            note: Some(note),
        },
        Err(error) => Serving {
            path: installed,
            sound: false,
            skewed,
            note: Some(format!(
                "{note}, and writing this ssf's own copy failed: {error:#}"
            )),
        },
    }
}

/// This build's own copy of that file, under the state directory, written when
/// it is not already there: a session started by this build always loads the
/// bridge this build's daemon reads the protocol of.
fn own_copy(name: &str, bytes: &[u8], state_dir: &Path) -> Result<PathBuf> {
    let file = name.rsplit('/').next().unwrap_or(name);
    let dir = state_dir.join("harness");
    let path = dir.join(file);
    // The launcher is executed; the bridge is only read.
    let mode = if file.ends_with("pi-launch") {
        0o755
    } else {
        0o644
    };
    if holds(&path, bytes) && !wrong_mode(&path, mode) {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    crate::config::write_atomic(&path, bytes, mode)?;
    // The mode is applied when the file is created, which an existing copy --
    // an unexecutable launcher, say -- does not go through.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("setting the mode on {}", path.display()))?;
    Ok(path)
}

fn holds(path: &Path, bytes: &[u8]) -> bool {
    std::fs::read(path).is_ok_and(|on_disk| on_disk == bytes)
}

fn wrong_mode(path: &Path, mode: u32) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o777 != mode)
        .unwrap_or(true)
}

pub(crate) fn available(path: &Path) -> bool {
    let pid = std::fs::read(path.join("ready.json"))
        .ok()
        .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).ok())
        .and_then(|ready| ready.get("pid")?.as_i64())
        .and_then(|pid| i32::try_from(pid).ok());
    pid.is_some_and(alive)
}

fn alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 changes no process state; it only checks whether the
    // bridge process named by its own marker still exists.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn fingerprint(text: &str) -> u64 {
    // FNV-1a is stable across daemon restarts; the sequence makes equal
    // messages in one session distinct.
    text.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

fn event_paths(path: &Path, sequence: u64, text: &str) -> (PathBuf, PathBuf) {
    let stem = format!("{sequence:020}-{:016x}", fingerprint(text));
    (
        path.join(format!("{stem}.json")),
        path.join(format!("{stem}.json.ack")),
    )
}

/// Does the mailbox hold this event?  A pending file was published and may
/// already have been handed to the harness; only an acknowledged one is in the
/// session's transcript.  Either way the event must not be submitted twice, by
/// the bridge or through the terminal.
pub(crate) fn has_record(path: &Path, sequence: u64, text: &str) -> bool {
    let (pending, ack) = event_paths(path, sequence, text);
    pending.exists() || ack.exists() || claimed(&pending, HANDED) || claimed(&pending, CLAIMED)
}

/// A pending file the Grok bridge has claimed to submit (`<name>.handed`), and
/// one the daemon has claimed to settle itself (`<name>.claimed`).  Each side
/// takes a file only by renaming it from the name the other side also renames
/// from, so exactly one of them has it.
const HANDED: &str = "handed";
const CLAIMED: &str = "claimed";

fn claimed_path(pending: &Path, how: &str) -> PathBuf {
    PathBuf::from(format!("{}.{how}", pending.display()))
}

fn claimed(pending: &Path, how: &str) -> bool {
    claimed_path(pending, how).exists()
}

/// Another attempt for this event sequence that the harness has not
/// acknowledged.  Its text carries the events this one carries too -- the
/// caller renders the delta since a watermark that has not moved -- so
/// publishing beside it would have the session record them twice.
fn unacknowledged(path: &Path, sequence: u64, here: &Path) -> bool {
    let prefix = format!("{sequence:020}-");
    let here = here.file_name().map(|n| n.to_string_lossy().into_owned());
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            // A file the Grok bridge has claimed is still unrecorded.
            let stem = name.strip_suffix(".handed").unwrap_or(&name);
            stem.ends_with(".json") && stem.starts_with(&prefix) && Some(stem) != here.as_deref()
        })
}

/// What an attempt published without a record yet, or the event it could not
/// publish at all.  A caller that treats the second as a delivery would move
/// the item's watermark past events the mailbox never received, so it is an
/// error the engine holds rather than counts.
#[derive(Debug)]
pub(crate) struct DeliveryUnrecorded {
    sequence: u64,
}

impl std::fmt::Display for DeliveryUnrecorded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the harness has not recorded this session's event {} yet; it stays in the mailbox and \
the newer events wait for it rather than being marked delivered",
            self.sequence
        )
    }
}

impl std::error::Error for DeliveryUnrecorded {}

/// The mailbox's bridge is not there to take an event: no ready marker, or
/// one naming a process that has exited.  The marker is the running poller's
/// own attestation and the poller repairs it, so this is what a session that
/// is gone, or that never loaded the bridge, looks like -- and it is a hold,
/// not a failure: nothing is published, the item's events stay un-seen, and a
/// restart takes them.
#[derive(Debug)]
pub(crate) struct DeliveryUnavailable {
    mailbox: PathBuf,
}

impl std::fmt::Display for DeliveryUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the harness delivery bridge is unavailable at {}; restart the session to load it",
            self.mailbox.display()
        )
    }
}

impl std::error::Error for DeliveryUnavailable {}

pub(crate) async fn deliver(path: &Path, sequence: u64, text: &str) -> Result<Receipt> {
    if !available(path) {
        return Err(DeliveryUnavailable {
            mailbox: path.to_path_buf(),
        }
        .into());
    }
    std::fs::create_dir_all(path)
        .with_context(|| format!("creating delivery mailbox {}", path.display()))?;
    let (pending, ack) = event_paths(path, sequence, text);
    if ack.exists() {
        return Ok(Receipt::Recorded);
    }
    // One attempt per sequence is in flight at a time.  A later attempt for
    // the same sequence re-renders the events an unrecorded file already
    // carries -- the item's watermark has not moved -- so publishing beside it
    // would put those events in the session twice.  It waits for the record
    // instead, and reports a hold if it does not come: the caller keeps its
    // events un-seen and the next pass tries again.
    let started = Instant::now();
    while unacknowledged(path, sequence, &pending) {
        if started.elapsed() >= RECORD_TIMEOUT {
            return Err(DeliveryUnrecorded { sequence }.into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if !pending.exists() && !claimed(&pending, HANDED) {
        let stem = pending
            .file_stem()
            .and_then(|stem| stem.to_str())
            .context("delivery path has no UTF-8 file stem")?;
        let temporary = path.join(format!(".{stem}.tmp-{}", std::process::id()));
        let bytes = serde_json::to_vec(&json!({ "text": text }))?;
        std::fs::write(&temporary, bytes)
            .with_context(|| format!("writing delivery {}", temporary.display()))?;
        std::fs::rename(&temporary, &pending)
            .with_context(|| format!("publishing delivery {}", pending.display()))?;
    }
    while started.elapsed() < RECORD_TIMEOUT {
        if ack.exists() {
            return Ok(Receipt::Recorded);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(Receipt::Published)
}

/// Say what became of an out-of-band delivery.  `Published` is not a failure:
/// the mailbox holds the event until the session's transcript records it, but
/// the operator should be able to see that the agent has not taken it yet.
fn report_receipt(receipt: Receipt, handle: &str, mailbox: &Path) {
    if receipt == Receipt::Published {
        // Logged under herdr, where this line was before #446.
        tracing::info!(
            target: "ssf::herdr",
            pane_id = handle,
            mailbox = %mailbox.display(),
            "out-of-band delivery is in the mailbox; the session has not recorded it yet"
        );
    }
}

/// The mailbox channel OMP, Pi and OpenCode take events through: the daemon
/// publishes to the session's mailbox and the bridge the session was launched
/// with injects it.
pub(crate) struct Mailbox;

impl Channel for Mailbox {
    fn bridged(&self) -> bool {
        true
    }

    fn deliver<'a>(
        &'a self,
        _herdr: &'a Herdr,
        pane: &'a str,
        journal: Journal<'a>,
        text: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let (mailbox, sequence) = journal.context("native harness delivery has no mailbox")?;
            let receipt = deliver(mailbox, sequence, text).await?;
            // The event is published either way; a mailbox without the
            // record yet is a busy session, not a lost event (#390).
            report_receipt(receipt, pane, mailbox);
            Ok(())
        })
    }

    fn has_record(&self, mailbox: &Path, sequence: u64, text: &str) -> bool {
        has_record(mailbox, sequence, text)
    }

    /// The per-session Pi/OMP/OpenCode command resumes the transcript. Its
    /// bridge either finds this delivery ID there and acknowledges it, or
    /// injects the still-pending event. In neither case should the relaunch path
    /// submit the same body through the terminal.
    fn relaunched<'a>(
        &'a self,
        _herdr: &'a Herdr,
        pane: &'a str,
        journal: Journal<'a>,
        recorded: bool,
        _resumed: bool,
        text: &'a str,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            let Some((mailbox, sequence)) = journal.filter(|_| recorded) else {
                return Ok(false);
            };
            let started = Instant::now();
            while !available(mailbox) && started.elapsed() < BRIDGE_START_TIMEOUT {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let receipt = deliver(mailbox, sequence, text).await?;
            report_receipt(receipt, pane, mailbox);
            Ok(true)
        })
    }
}

/// How long a Grok bridge may say it is starting before the daemon stops
/// waiting for it: its own budget for the TUI's leader and conversation
/// (120 seconds) and for its two ACP requests (30 each), with room to spare.
const GROK_BRIDGE_BUDGET: Duration = Duration::from_secs(240);

/// What the Grok bridge says of itself in the mailbox's `bridge.json`, which
/// the launcher writes `starting` (with the bridge's pid) and the bridge moves
/// to `ready`, `declined` (its startup probe failed) or `stopped`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GrokBridge {
    /// Attesting to the mailbox: it takes events.
    Serving,
    /// A live bridge that has not finished its probe: events wait for it.
    Starting,
    /// Declined, stopped, dead, too slow, or never started: nothing will take
    /// events from the mailbox, so the daemon settles them itself.
    Gone,
}

fn grok_bridge_state(mailbox: &Path) -> GrokBridge {
    if available(mailbox) {
        return GrokBridge::Serving;
    }
    let marker = mailbox.join("bridge.json");
    let state = std::fs::read(&marker)
        .ok()
        .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).ok());
    let fresh = std::fs::metadata(&marker)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.elapsed().ok())
        .is_some_and(|age| age < GROK_BRIDGE_BUDGET);
    let Some(state) = state else {
        return GrokBridge::Gone;
    };
    let pid = state
        .get("pid")
        .and_then(serde_json::Value::as_i64)
        .and_then(|pid| i32::try_from(pid).ok());
    let live = pid.is_some_and(alive);
    match state.get("state").and_then(serde_json::Value::as_str) {
        // A ready bridge whose marker went missing repairs it on its next tick.
        Some("ready") if live => GrokBridge::Starting,
        Some("starting") if live && fresh => GrokBridge::Starting,
        _ => GrokBridge::Gone,
    }
}

/// Where the session's Grok keeps its conversations: what the launcher
/// recorded in `session/grok-home`, else what the session would have resolved.
fn grok_home(mailbox: &Path) -> PathBuf {
    std::fs::read_to_string(mailbox.join("session/grok-home"))
        .ok()
        .map(|home| home.trim().to_owned())
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("GROK_HOME").map(PathBuf::from))
        .or_else(|| dirs::home_dir().map(|home| home.join(".grok")))
        .unwrap_or_else(|| PathBuf::from(".grok"))
}

/// Whether the conversation the bridge pinned records the event the mailbox
/// file `name` carried: the bridge tags each prompt with it, and Grok keeps
/// the tag in the conversation's `updates.jsonl`.
fn grok_recorded(mailbox: &Path, name: &str) -> bool {
    let Ok(id) = std::fs::read_to_string(mailbox.join("session/grok-session")) else {
        return false;
    };
    let id = id.trim();
    if id.is_empty() || id.contains(['/', '.']) {
        return false;
    }
    let needle = format!("\"ssfDeliveryId\":\"{name}\"");
    std::fs::read_dir(grok_home(mailbox).join("sessions"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|group| group.path().join(id).join("updates.jsonl"))
        .filter_map(|file| std::fs::read_to_string(file).ok())
        .any(|updates| updates.contains(&needle))
}

/// Settle every event a Grok mailbox holds that no bridge will take: each is
/// claimed (renamed to `.claimed`, so a bridge that comes up late cannot also
/// submit it), acknowledged when the conversation already records it, and
/// otherwise pasted -- except an attempt at `sequence` itself, whose events
/// `text` carries again.  Says whether the conversation records `text`'s own
/// file, which then needs no paste either.
async fn settle_grok(
    herdr: &Herdr,
    pane: &str,
    mailbox: &Path,
    sequence: u64,
    text: &str,
) -> Result<bool> {
    let (own, own_ack) = event_paths(mailbox, sequence, text);
    let own = own.file_name().map(|n| n.to_string_lossy().into_owned());
    let prefix = format!("{sequence:020}-");
    let mut names: Vec<(String, String)> = std::fs::read_dir(mailbox)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name().to_string_lossy().into_owned();
            let stem = file.strip_suffix(".handed").unwrap_or(&file).to_owned();
            let event =
                stem.ends_with(".json") && stem.as_bytes().first().is_some_and(u8::is_ascii_digit);
            event.then_some((stem, file))
        })
        .collect();
    names.sort();
    let mut recorded = own_ack.exists();
    for (stem, file) in names {
        let from = mailbox.join(&file);
        let claim = mailbox.join(format!("{stem}.{CLAIMED}"));
        if std::fs::rename(&from, &claim).is_err() {
            // Taken by someone else meanwhile.
            continue;
        }
        let ack = mailbox.join(format!("{stem}.ack"));
        if grok_recorded(mailbox, &stem) {
            recorded |= Some(&stem) == own.as_ref();
        } else if !stem.starts_with(&prefix) {
            let body = std::fs::read(&claim).unwrap_or_default();
            let earlier = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("text")?.as_str().map(str::to_owned));
            if let Some(earlier) = earlier {
                let sent = crate::herdr::Terminal
                    .deliver(herdr, pane, None, &earlier)
                    .await;
                if let Err(e) = sent {
                    // Back to pending, for the next attempt.
                    let _ = std::fs::rename(&claim, &from);
                    return Err(e);
                }
            }
        }
        std::fs::rename(&claim, &ack).with_context(|| format!("settling {}", claim.display()))?;
    }
    Ok(recorded)
}

/// Grok's channel: the mailbox while its sidecar bridge attests to it, the
/// terminal otherwise.  The bridge attests only once its ACP handshake and
/// session load have worked, so a Grok it cannot drive -- a protocol that
/// changed, a sandbox profile that refuses leader mode, no node -- keeps the
/// paste delivery it had before the bridge.  Events wait while a bridge is
/// starting; once none is serving, those still in the mailbox are settled
/// against the conversation's record and the rest are pasted, so nothing
/// stalls there and nothing is submitted twice.
pub(crate) struct Grok;

impl Channel for Grok {
    fn bridged(&self) -> bool {
        true
    }

    fn deliver<'a>(
        &'a self,
        herdr: &'a Herdr,
        pane: &'a str,
        journal: Journal<'a>,
        text: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let Some((mailbox, sequence)) = journal else {
                return crate::herdr::Terminal
                    .deliver(herdr, pane, None, text)
                    .await;
            };
            match grok_bridge_state(mailbox) {
                GrokBridge::Serving => Mailbox.deliver(herdr, pane, journal, text).await,
                GrokBridge::Starting => Err(DeliveryUnavailable {
                    mailbox: mailbox.to_path_buf(),
                }
                .into()),
                GrokBridge::Gone => {
                    if settle_grok(herdr, pane, mailbox, sequence, text).await? {
                        return Ok(());
                    }
                    crate::herdr::Terminal
                        .deliver(herdr, pane, None, text)
                        .await
                }
            }
        })
    }

    fn has_record(&self, mailbox: &Path, sequence: u64, text: &str) -> bool {
        has_record(mailbox, sequence, text)
    }

    /// [`Mailbox`]'s relaunch once the relaunched session's bridge serves.
    /// One that declines, dies or outlasts its budget leaves the events to the
    /// terminal, settled first against the conversation's record.
    fn relaunched<'a>(
        &'a self,
        herdr: &'a Herdr,
        pane: &'a str,
        journal: Journal<'a>,
        recorded: bool,
        resumed: bool,
        text: &'a str,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            let Some((mailbox, sequence)) = journal.filter(|_| recorded) else {
                return Ok(false);
            };
            let started = Instant::now();
            while grok_bridge_state(mailbox) == GrokBridge::Starting
                && started.elapsed() < GROK_BRIDGE_BUDGET
            {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            if grok_bridge_state(mailbox) == GrokBridge::Serving {
                return Mailbox
                    .relaunched(herdr, pane, journal, recorded, resumed, text)
                    .await;
            }
            settle_grok(herdr, pane, mailbox, sequence, text).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_is_scoped_to_repository_and_session() {
        let sandbox = crate::config::test_support::sandbox();
        assert_eq!(
            mailbox("owner/repo", 42),
            sandbox.state_dir().join("delivery/owner/repo/42")
        );
        assert!(mailbox("../repo", 42).starts_with(sandbox.state_dir().join("delivery")));
    }

    /// The bytes a session is pointed at are this build's, whatever the share
    /// tree holds.  A bridge from another build is what an item whose every
    /// event is held looks like -- its ready marker goes unrepaired and the
    /// daemon reads that as "no live bridge" -- so the daemon must not depend
    /// on a human to keep the two in lockstep (#402).
    #[test]
    fn the_installed_copy_is_used_when_it_is_this_builds() {
        let sandbox = crate::config::test_support::sandbox();
        let share = sandbox.root().join("share");
        std::fs::create_dir(&share).unwrap();
        let installed = share.join("ssf-delivery.ts");
        std::fs::write(&installed, BRIDGE).unwrap();

        let serving = served(
            BRIDGE_NAME,
            BRIDGE,
            std::slice::from_ref(&installed),
            &sandbox.state_dir(),
        );
        assert_eq!(serving.path, installed, "the package's own copy is served");
        assert!(serving.sound && !serving.skewed);
        assert!(
            serving.detail().is_empty(),
            "an aligned install is not worth a remark: {}",
            serving.detail()
        );
    }

    #[test]
    fn a_copy_from_another_build_is_not_the_one_a_session_gets() {
        let sandbox = crate::config::test_support::sandbox();
        let share = sandbox.root().join("share");
        std::fs::create_dir(&share).unwrap();
        let installed = share.join("ssf-delivery.ts");
        std::fs::write(&installed, "// a bridge from an earlier build\n").unwrap();

        let serving = served(
            BRIDGE_NAME,
            BRIDGE,
            std::slice::from_ref(&installed),
            &sandbox.state_dir(),
        );
        assert!(
            serving.sound,
            "delivery is still sound: {}",
            serving.detail()
        );
        assert!(serving.skewed, "the mismatch has to be visible");
        assert_ne!(
            serving.path, installed,
            "a session must not be handed another build's bridge"
        );
        assert_eq!(
            std::fs::read(&serving.path).unwrap(),
            BRIDGE,
            "the served bridge is not the one this build ships"
        );
        assert!(
            serving.detail().contains(&installed.display().to_string()),
            "the check line must name the copy it refused: {}",
            serving.detail()
        );
        assert!(
            std::path::Path::new(&serving.path).starts_with(sandbox.state_dir()),
            "the copy served instead is this ssf's own: {}",
            serving.path.display()
        );
    }

    /// A standalone binary install has no share tree at all, which used to
    /// hand a session a path to a file that was never installed.
    #[test]
    fn a_standalone_install_still_gets_a_bridge() {
        let sandbox = crate::config::test_support::sandbox();
        let missing = sandbox.root().join("share/ssf-delivery.ts");

        let serving = served(
            BRIDGE_NAME,
            BRIDGE,
            std::slice::from_ref(&missing),
            &sandbox.state_dir(),
        );
        assert!(serving.sound, "{}", serving.detail());
        assert!(!serving.skewed, "a share tree that is absent is not a skew");
        assert!(serving.path.is_file());
        assert_eq!(std::fs::read(&serving.path).unwrap(), BRIDGE);
    }

    /// OpenCode's plugin is served like the Pi/OMP extension, and only to
    /// OpenCode's sessions.
    #[test]
    fn opencode_sessions_get_the_opencode_plugin() {
        let sandbox = crate::config::test_support::sandbox();
        let missing = sandbox.root().join("share/ssf-opencode.ts");
        let serving = served(
            OPENCODE_BRIDGE_NAME,
            OPENCODE_BRIDGE,
            &[missing],
            &sandbox.state_dir(),
        );
        assert!(serving.sound, "{}", serving.detail());
        assert_eq!(std::fs::read(&serving.path).unwrap(), OPENCODE_BRIDGE);
        assert!(serving.path.ends_with("harness/ssf-opencode.ts"));
        assert!(
            bridge_serving_for("opencode")
                .path
                .ends_with("ssf-opencode.ts")
        );
        assert!(bridge_serving_for("pi").path.ends_with("ssf-delivery.ts"));
    }

    /// The launcher is executed, not read.
    #[test]
    fn the_launcher_a_session_gets_can_be_executed() {
        let sandbox = crate::config::test_support::sandbox();
        let missing = sandbox.root().join("share/ssf-pi-launch");

        let serving = served(LAUNCHER_NAME, LAUNCHER, &[missing], &sandbox.state_dir());
        assert_eq!(std::fs::read(&serving.path).unwrap(), LAUNCHER);
        let mode = std::fs::metadata(&serving.path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o111,
            0o111,
            "the launcher was written without its exec bits: {mode:o}"
        );
    }

    /// Nowhere to write this build's own copy: the check says so rather than
    /// reporting a bridge a session cannot take events through.
    #[test]
    fn a_copy_that_cannot_be_written_is_not_reported_sound() {
        let sandbox = crate::config::test_support::sandbox();
        let missing = sandbox.root().join("share/ssf-delivery.ts");
        std::fs::create_dir_all(sandbox.state_dir()).unwrap();
        // The directory the copy needs is a file.
        std::fs::write(sandbox.state_dir().join("harness"), b"not a directory").unwrap();

        let serving = served(BRIDGE_NAME, BRIDGE, &[missing], &sandbox.state_dir());
        assert!(!serving.sound, "{}", serving.detail());
        assert!(serving.detail().contains("could not be written"));
    }

    /// The window #390 closes: a bridge that has published the event but has
    /// no session record of it yet.  The attempt must report the mailbox, not
    /// the transcript, and leave the event there for the harness to take and
    /// for a relaunch to find.
    #[tokio::test(start_paused = true)]
    async fn an_event_the_harness_has_not_recorded_stays_pending() {
        let sandbox = crate::config::test_support::sandbox();
        let root = sandbox.root().join("mailbox");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("ready.json"),
            format!("{{\"pid\":{}}}", std::process::id()),
        )
        .unwrap();
        assert_eq!(
            deliver(&root, 3, "[ssf] busy").await.unwrap(),
            Receipt::Published
        );
        let (pending, ack) = event_paths(&root, 3, "[ssf] busy");
        assert!(
            !ack.exists(),
            "an event the transcript does not hold was acknowledged"
        );
        let body: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&pending).unwrap()).unwrap();
        assert_eq!(
            body["text"], "[ssf] busy",
            "the event is no longer in the mailbox for the harness to take"
        );

        // The record lands at the agent's next step boundary, however long
        // that takes: the next attempt observes it and reports the record.
        std::fs::rename(&pending, &ack).unwrap();
        assert_eq!(
            deliver(&root, 3, "[ssf] busy").await.unwrap(),
            Receipt::Recorded
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
    }

    /// A second attempt for one sequence renders the events the first one
    /// already carries (the item's watermark has not moved), so it must not
    /// publish beside a file the session has not recorded: the session would
    /// see those events twice.  It must not report a delivery either -- a
    /// caller that took this as delivered would move its watermark past events
    /// the mailbox never received.
    #[tokio::test(start_paused = true)]
    async fn an_unrecorded_attempt_holds_a_newer_one_without_claiming_delivery() {
        let sandbox = crate::config::test_support::sandbox();
        let root = sandbox.root().join("mailbox");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("ready.json"),
            format!("{{\"pid\":{}}}", std::process::id()),
        )
        .unwrap();
        assert_eq!(
            deliver(&root, 4, "[ssf] one").await.unwrap(),
            Receipt::Published
        );
        let blocked = deliver(&root, 4, "[ssf] one and two").await.unwrap_err();
        assert_eq!(
            hold(&blocked),
            Some(Hold::Unrecorded),
            "the held attempt was reported as a delivery: {blocked:#}"
        );
        assert!(
            !event_paths(&root, 4, "[ssf] one and two").0.exists(),
            "a superset of an unrecorded event was published beside it"
        );
        // Once the first is recorded, the fuller text goes out on its own.
        std::fs::rename(
            event_paths(&root, 4, "[ssf] one").0,
            event_paths(&root, 4, "[ssf] one").1,
        )
        .unwrap();
        assert_eq!(
            deliver(&root, 4, "[ssf] one and two").await.unwrap(),
            Receipt::Published
        );
        assert!(event_paths(&root, 4, "[ssf] one and two").0.exists());
    }

    #[tokio::test]
    async fn delivery_waits_for_the_bridge_acknowledgement() {
        let sandbox = crate::config::test_support::sandbox();
        let root = sandbox.root().join("mailbox");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("ready.json"),
            format!("{{\"pid\":{}}}", std::process::id()),
        )
        .unwrap();
        let path = root.clone();
        let bridge = tokio::spawn(async move {
            loop {
                let entry = std::fs::read_dir(&path)
                    .unwrap()
                    .flatten()
                    .map(|entry| entry.path())
                    .find(|path| {
                        path.extension().is_some_and(|ext| ext == "json")
                            && path.file_name().unwrap() != "ready.json"
                    });
                if let Some(pending) = entry {
                    let text: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&pending).unwrap()).unwrap();
                    std::fs::rename(&pending, format!("{}.ack", pending.display())).unwrap();
                    return text["text"].as_str().unwrap().to_string();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        deliver(&root, 7, "[ssf] hello").await.unwrap();
        assert_eq!(bridge.await.unwrap(), "[ssf] hello");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
        // A retry after the handoff observes the durable acknowledgement and
        // cannot publish a second copy.
        deliver(&root, 7, "[ssf] hello").await.unwrap();
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);

        std::fs::write(event_paths(&root, 8, "[ssf] later").0, b"pending").unwrap();
        assert!(has_record(&root, 8, "[ssf] later"));
        assert_eq!(
            std::fs::read_dir(&root)
                .unwrap()
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json.ack"))
                .count(),
            1,
            "a later event must not delete the earlier durable acknowledgement"
        );
    }

    /// A mailbox no live bridge attests to: the ready marker is gone, or it
    /// names a process that has exited.  The running poller repairs its own
    /// marker (#395), so this is what a session that is gone -- or one that
    /// never loaded the bridge -- looks like, and it must hold the item's
    /// events rather than count toward giving the binding up: a session
    /// restart takes them, and a dropped binding would deliver nothing.
    #[tokio::test]
    async fn a_mailbox_no_live_bridge_attests_to_is_held_not_failed() {
        let sandbox = crate::config::test_support::sandbox();
        let root = sandbox.root().join("mailbox");
        std::fs::create_dir(&root).unwrap();

        let bare = deliver(&root, 2, "[ssf] hello").await.unwrap_err();
        assert_eq!(hold(&bare), Some(Hold::Unavailable), "{bare:#}");
        assert!(
            bare.to_string().contains("restart the session to load it"),
            "the refusal must still name the fix: {bare}"
        );
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            0,
            "nothing may be published into a mailbox no bridge polls"
        );

        // The shapes a lost marker takes: an unparseable file, a pid no
        // process can have (which stands for one that has exited), and a
        // file that is not there at all (the case above).
        for marker in [&b"not json"[..], &b"{\"pid\":2147483647}"[..]] {
            std::fs::write(root.join("ready.json"), marker).unwrap();
            let e = deliver(&root, 2, "[ssf] hello").await.unwrap_err();
            assert_eq!(hold(&e), Some(Hold::Unavailable), "{e:#}");
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        }

        // A marker a live poller attests to publishes as it did before.
        std::fs::write(
            root.join("ready.json"),
            format!("{{\"pid\":{}}}", std::process::id()),
        )
        .unwrap();
        assert_eq!(
            deliver(&root, 2, "[ssf] hello").await.unwrap(),
            Receipt::Published
        );
        assert_eq!(hold(&anyhow::anyhow!("elsewhere")), None);
    }
}
