//! The web pane mirror's factory side (#414): `ssf __pane watch` prints a
//! session's agent pane -- its visible screen with colours, and the history
//! above it -- as JSON lines, one each time it changes, and `ssf __pane send`
//! types into it.
//!
//! The mirror is the read-only view of an item's pane, in the extension and
//! on the server web page (#563); typing into it goes through the live
//! terminal instead, `ssf __pane control`, which streams `herdr terminal
//! session observe|control`.
//!
//! These run where the sessions run, as every factory command does: the web
//! endpoint starts them through the same client transport as its status
//! stream, which forwards into the guest when the factory is in a VM. They
//! read the state file and ask the driver directly rather than going through
//! the daemon, which answers its requests one at a time between passes: a
//! mirror behind a pass that is talking to GitHub would freeze for as long.

use anyhow::{Context, Result, bail};
use serde_json::json;
use std::io::Write;
use std::time::Duration;

use crate::config::Config;
use crate::driver::Driver;
use crate::origin::{Origin, Scratch};
use crate::state::State;

/// How often a watch looks for the pane again, in reads: a harness that was
/// started again runs in a new pane, and finding the pane costs a listing of
/// every agent, which is not worth doing on every read.
const RELOCATE_EVERY: u32 = 20;

/// How often a watch reads the history above the screen, in reads: a long
/// history is a large read and a large frame, and the screen already shows
/// what is new, so every two seconds is enough for scrolling back.
const HISTORY_EVERY: u32 = 8;

/// How many rows of a pane a watch reads for its history, the screen's own
/// included: herdr's recent lines, bounded so a long-running agent's frame
/// stays a few hundred kilobytes at most.
const HISTORY_LINES: u32 = 1000;

/// How long a watch goes without writing before it writes an empty line.
/// A watch forwarded into a VM runs behind ssh with no terminal, so the
/// only way it learns its viewer has gone is a write that fails: an idle
/// screen would otherwise keep it reading herdr forever.
const HEARTBEAT: Duration = Duration::from_secs(5);

/// Where a session's agent runs: a driver's terminal (an item's session,
/// or a scratch session still in a herdr pane from before #491), or a
/// scratch session's tmux session.
pub(crate) enum Target {
    Driver(Driver, String),
    Tmux(crate::tmux::Tmux, String),
}

impl Target {
    async fn screen_ansi(&self) -> Result<String> {
        match self {
            Target::Driver(driver, pane) => driver.screen_ansi(pane).await,
            Target::Tmux(tmux, name) => tmux.capture(name, None).await,
        }
    }

    async fn recent_ansi(&self, lines: u32) -> Result<String> {
        match self {
            Target::Driver(driver, pane) => driver.recent_ansi(pane, lines).await,
            Target::Tmux(tmux, name) => tmux.capture(name, Some(lines)).await,
        }
    }

    async fn type_input(&self, text: Option<&str>, keys: &[String]) -> Result<()> {
        match self {
            Target::Driver(driver, pane) => driver.type_input(pane, text, keys).await,
            Target::Tmux(tmux, name) => tmux.type_input(name, text, keys).await,
        }
    }
}

/// Where a session's agent is.
pub(crate) async fn locate(cfg: &Config, state: &State, session: &str) -> Result<Target> {
    let (repo_name, worktree, handle) = if let Some(s) = Scratch::parse(session) {
        let repo = cfg
            .repos
            .iter()
            .find(|r| r.matches_name(&s.repo))
            .with_context(|| format!("{} is not a watched repository", s.repo))?;
        let st = state
            .repos
            .get(&repo.name)
            .and_then(|rs| rs.scratch.get(&s.id))
            .with_context(|| format!("{session} is not a session ssf knows"))?;
        // A scratch session runs in tmux (#491) unless it is still in the
        // herdr pane it was started in before that.
        let in_herdr = st
            .terminal_handle
            .as_deref()
            .is_some_and(|h| crate::tmux::name_of(h).is_none())
            && st
                .worktree_id
                .as_deref()
                .is_some_and(|w| !crate::driver::is_local_worktree(w));
        if !in_herdr {
            let tmux = crate::tmux::Tmux::new();
            let name = crate::tmux::session_name(&repo.name, &s.id);
            if st.worktree_id.is_none() {
                bail!("{session} has no workspace");
            }
            if !tmux.has_session(&name).await? {
                bail!("no agent is running in {session}'s workspace");
            }
            return Ok(Target::Tmux(tmux, name));
        }
        (
            repo.name.clone(),
            st.worktree_id.clone(),
            st.terminal_handle.clone(),
        )
    } else if let Some(o) = Origin::parse(session) {
        let repo = cfg
            .repos
            .iter()
            .find(|r| r.matches_name(&o.repo))
            .with_context(|| format!("{} is not a watched repository", o.repo))?;
        let issues = &state
            .repos
            .get(&repo.name)
            .with_context(|| format!("{session} is not a session ssf knows"))?
            .issues;
        // An item bound to another session's workspace is worked in that
        // session's pane.
        let owner = crate::state::owner_in(issues, o.number);
        let st = issues
            .get(&owner)
            .with_context(|| format!("{session} is not a session ssf knows"))?;
        (
            repo.name.clone(),
            st.worktree_id.clone(),
            st.terminal_handle.clone(),
        )
    } else {
        bail!("{session}: expected owner/repo#N or owner/repo~id");
    };
    let worktree = worktree.with_context(|| format!("{session} has no workspace"))?;
    let repo = cfg
        .repos
        .iter()
        .find(|r| r.name == repo_name)
        .context("the repository went away")?;
    let driver = Driver::new(cfg.driver_for(repo), cfg);
    let pane = driver
        .live_handle(&worktree, handle.as_deref())
        .await?
        .with_context(|| format!("no agent is running in {session}'s workspace"))?;
    Ok(Target::Driver(driver, pane))
}

/// The lines a watch prints, each only when it differs from the last one:
/// an idle agent's screen is read four times a second and sent once.
#[derive(Default)]
pub(crate) struct Changes {
    last: Option<String>,
}

impl Changes {
    /// `line` when it is not the last one this said, else `None`.
    pub(crate) fn fresh(&mut self, line: String) -> Option<String> {
        if self.last.as_deref() == Some(line.as_str()) {
            return None;
        }
        self.last = Some(line.clone());
        Some(line)
    }
}

/// `ssf __pane watch`: the session's screen as `{"screen": …}` lines, the
/// history above it as `{"history": …}` lines (each only when it changed),
/// or `{"error": …}` while it cannot be read, until whoever reads stdout goes.
///
/// A session whose pane cannot be found is said once and ends the watch,
/// rather than reloading the config and state four times a second for as
/// long as someone looks at a pane that is not there.
pub(crate) async fn watch(session: &str, interval: Duration) -> Result<()> {
    let cfg = Config::load()?;
    let mut changes = Changes::default();
    let mut history = Changes::default();
    let mut target: Option<Target> = None;
    let mut reads = 0u32;
    let mut out = std::io::stdout();
    let mut written = std::time::Instant::now();
    loop {
        if target.is_none() || reads.is_multiple_of(RELOCATE_EVERY) {
            // The state file is the daemon's; it is read afresh, since a
            // relaunch records the new pane there.
            let located = match State::load() {
                Ok(state) => locate(&cfg, &state, session).await,
                Err(error) => Err(error),
            };
            match located {
                Ok(found) => target = Some(found),
                Err(error) => {
                    emit(
                        &mut out,
                        &mut changes,
                        json!({"error": format!("{error:#}")}),
                    )?;
                    return Ok(());
                }
            }
        }
        reads = reads.wrapping_add(1);
        if let Some(found) = &target {
            let said = match found.screen_ansi().await {
                Ok(screen) => {
                    // History first, so a viewer has it before the screen
                    // it sits above. One that cannot be read is left for
                    // the next time: the screen is what matters.
                    let mut said = false;
                    if reads % HISTORY_EVERY == 1
                        && let Ok(recent) = found.recent_ansi(HISTORY_LINES).await
                    {
                        let above = redact_tokens(&history_above(&recent, &screen));
                        said = emit(&mut out, &mut history, json!({ "history": above }))?;
                    }
                    let shown = json!({"screen": redact_tokens(&screen)});
                    emit(&mut out, &mut changes, shown)? || said
                }
                Err(error) => {
                    // Found again on the next read, or the watch ends.
                    target = None;
                    emit(
                        &mut out,
                        &mut changes,
                        json!({"error": format!("{error:#}")}),
                    )?
                }
            };
            if said {
                written = std::time::Instant::now();
            }
        }
        if written.elapsed() >= HEARTBEAT {
            writeln!(out).context("the reader went away")?;
            out.flush().context("the reader went away")?;
            written = std::time::Instant::now();
        }
        tokio::time::sleep(interval).await;
    }
}

/// `screen` with every GitHub token in it (`ghp_`, `gho_`, `ghs_`, `ghu_`,
/// `github_pat_` followed by its characters) replaced by `<redacted>`. The
/// pane is shown in a browser; a token a harness or a command printed is
/// not something to put there. The launch never prints the bot's (see
/// `launch_env`); this is for what else may.
pub(crate) fn redact_tokens(screen: &str) -> String {
    let body = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(screen.len());
    let mut rest = screen;
    loop {
        let found = ["ghp_", "gho_", "ghs_", "ghu_", "github_pat_"]
            .iter()
            .filter_map(|prefix| rest.find(prefix).map(|at| (at, prefix.len())))
            .min();
        let Some((at, prefix)) = found else { break };
        let tail = &rest[at + prefix..];
        let len = tail.find(|c: char| !body(c)).unwrap_or(tail.len());
        // A token has a real body; `ghp_x` in prose is left alone. What
        // comes before is not looked at: a colour escape (`\x1b[1m`) runs
        // straight into the token.
        if len >= 16 {
            out.push_str(&rest[..at]);
            out.push_str("<redacted>");
        } else {
            out.push_str(&rest[..at + prefix + len]);
        }
        rest = &tail[len..];
    }
    out.push_str(rest);
    out
}

/// The lines of `recent` above `screen`: herdr's recent lines end with the
/// visible screen, so the history is whatever comes before that many lines.
fn history_above(recent: &str, screen: &str) -> String {
    let lines: Vec<&str> = recent.split("\r\n").collect();
    let above = lines.len().saturating_sub(screen.split("\r\n").count());
    lines[..above].join("\r\n")
}

/// One line, if it says something new, and whether it did. A reader that
/// has gone is the end of the watch.
fn emit(out: &mut impl Write, changes: &mut Changes, value: serde_json::Value) -> Result<bool> {
    let Some(line) = changes.fresh(value.to_string()) else {
        return Ok(false);
    };
    writeln!(out, "{line}").context("the reader went away")?;
    out.flush().context("the reader went away")?;
    Ok(true)
}

/// Exit status of `ssf __pane send` for a pane the factory does not let a
/// person type into, so the web endpoint can tell it from a failure.
pub(crate) const INPUT_REFUSED: i32 = 2;

/// What `item_pane_input` says of `session`, or why it may not be typed
/// into: a scratch session always may; an item's session as its
/// repository's setting says (#439 keeps it off unless someone turns it on).
pub(crate) fn input_refusal(cfg: &Config, session: &str) -> Option<String> {
    if Scratch::parse(session).is_some() {
        return None;
    }
    let origin = Origin::parse(session)?;
    let repo = cfg.repos.iter().find(|r| r.matches_name(&origin.repo))?;
    (!cfg.item_pane_input(repo)).then(|| {
        format!(
            "{session}'s pane is view-only: speak to an item's agent by commenting on the item \
             (item_pane_input is off for {})",
            repo.name
        )
    })
}

/// `ssf __pane send`: type into the session's agent pane. `Ok(Some(why))`
/// is a pane the configuration keeps view-only; nothing is typed.
pub(crate) async fn send(
    session: &str,
    text: Option<&str>,
    keys: &[String],
) -> Result<Option<String>> {
    let cfg = Config::load()?;
    if let Some(why) = input_refusal(&cfg, session) {
        return Ok(Some(why));
    }
    let state = State::load()?;
    locate(&cfg, &state, session)
        .await?
        .type_input(text, keys)
        .await?;
    Ok(None)
}

/// `ssf __pane attach`: attach this terminal to a scratch session's tmux
/// session (#491), for the web dashboard's `api/term`, which runs this in a
/// PTY. Only a scratch session in tmux can be attached to; an item's
/// session is herdr's and stays behind the mirror.
pub(crate) async fn attach(session: &str) -> Result<()> {
    if Scratch::parse(session).is_none() {
        bail!("{session}: only a scratch session (owner/repo~id) has a terminal to attach to");
    }
    let cfg = Config::load()?;
    let state = State::load()?;
    let Target::Tmux(tmux, name) = locate(&cfg, &state, session).await? else {
        bail!("{session} still runs in a herdr pane; it moves to tmux when it is next started");
    };
    use std::os::unix::process::CommandExt;
    let error = tmux.attach_command(&name).exec();
    Err(anyhow::Error::new(error).context("running tmux attach"))
}

/// How often an observed pane's size is read (#563): herdr says nothing
/// when a pane is resized, and an observer draws at the size it started
/// with, so it is started again at the new one.
const OBSERVE_SIZE_EVERY: Duration = Duration::from_secs(2);

/// `ssf __pane control` (#563): stream an item session's herdr pane as
/// `herdr terminal session` NDJSON over stdin and stdout (pipes, never a
/// PTY): `observe` (view only) or `control` (typing, and the pane takes this
/// terminal's size). `Ok(Some(why))` is control of a pane the configuration
/// keeps view-only (`item_pane_input`); nothing is started.
///
/// Control is never `--takeover`: a pane someone else controls ends this one
/// with herdr's own `terminal.closed`.
pub(crate) async fn control(
    session: &str,
    observe: bool,
    size: Option<(u16, u16)>,
) -> Result<Option<String>> {
    if Origin::parse(session).is_none() {
        bail!("{session}: only an item session (owner/repo#N) has a live terminal");
    }
    let cfg = Config::load()?;
    if !observe && let Some(why) = input_refusal(&cfg, session) {
        return Ok(Some(why));
    }
    let state = State::load()?;
    let Target::Driver(Driver::Herdr(herdr), pane) = locate(&cfg, &state, session).await? else {
        bail!("{session} has no herdr pane");
    };
    let session_command = |mode: &str| {
        let mut command =
            tokio::process::Command::new(crate::config::herdr_command_path(herdr.command()));
        command.args(["terminal", "session", mode, &pane]);
        for name in [
            "HERDR_WORKSPACE_ID",
            "HERDR_TAB_ID",
            "HERDR_PANE_ID",
            "HERDR_ENV",
        ] {
            command.env_remove(name);
        }
        command
    };
    if !observe {
        // Control is the stream alone: stdin is its commands, and closing it
        // releases the pane.
        let mut command = session_command("control");
        if let Some((cols, rows)) = size {
            command.args(["--cols", &cols.to_string(), "--rows", &rows.to_string()]);
        }
        use std::os::unix::process::CommandExt;
        let error = command.as_std_mut().exec();
        return Err(anyhow::Error::new(error).context("running herdr terminal session control"));
    }
    observe_pane(&herdr, &pane, || {
        let mut command = session_command("observe");
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);
        // The observer goes with this process however it ends, SIGKILL
        // included, which `kill_on_drop` cannot see.
        #[cfg(target_os = "linux")]
        // SAFETY: prctl is async-signal-safe and touches only this child.
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command
    })
    .await
    .map(|()| None)
}

/// Run the observer, and start it again whenever the pane's size changes,
/// until it ends (herdr's `terminal.closed` is the last line it passed on) or
/// whoever reads this goes. Its lines are passed on whole, so a restart never
/// cuts one. An observer takes no input, and never ends at its stdin's end:
/// that end is this process's cue to stop it.
async fn observe_pane(
    herdr: &crate::herdr::Herdr,
    pane: &str,
    command: impl Fn() -> tokio::process::Command,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    let mut input = tokio::io::stdin();
    let mut out = tokio::io::stdout();
    let mut sink = [0u8; 256];
    loop {
        let size = pane_size(herdr, pane).await;
        let mut command = command();
        if let Some((cols, rows)) = size {
            command.args(["--cols", &cols.to_string(), "--rows", &rows.to_string()]);
        }
        let mut child = command
            .spawn()
            .context("starting herdr terminal session observe")?;
        let mut lines =
            tokio::io::BufReader::new(child.stdout.take().context("the observer's stdout")?)
                .lines();
        let mut tick = tokio::time::interval(OBSERVE_SIZE_EVERY);
        tick.tick().await;
        let resized = loop {
            tokio::select! {
                line = lines.next_line() => match line? {
                    Some(line) => {
                        out.write_all(format!("{line}\n").as_bytes()).await?;
                        out.flush().await?;
                    }
                    None => {
                        let _ = child.wait().await;
                        return Ok(());
                    }
                },
                read = input.read(&mut sink) => {
                    if matches!(read, Ok(0) | Err(_)) {
                        return Ok(());
                    }
                },
                _ = tick.tick() => {
                    if size.is_some() && pane_size(herdr, pane).await.is_some_and(|now| Some(now) != size) {
                        break true;
                    }
                }
            }
        };
        if resized {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

/// The pane's size, as near as herdr 0.9 tells it: its width in its tab's
/// layout (`pane layout`), and its terminal's rows (`pane get`), which the
/// layout does not follow while a controller has resized the pane.
async fn pane_size(herdr: &crate::herdr::Herdr, pane: &str) -> Option<(u64, u64)> {
    let layout = herdr.run(&["pane", "layout", "--pane", pane]).await.ok()?;
    let info = herdr.run(&["pane", "get", pane]).await.ok()?;
    pane_size_of(&layout, &info, pane)
}

fn pane_size_of(
    layout: &serde_json::Value,
    info: &serde_json::Value,
    pane: &str,
) -> Option<(u64, u64)> {
    let rect = layout
        .pointer("/layout/panes")?
        .as_array()?
        .iter()
        .find(|p| p.get("pane_id").and_then(|id| id.as_str()) == Some(pane))?
        .get("rect")?;
    let rows = info
        .pointer("/pane/scroll/viewport_rows")
        .and_then(|rows| rows.as_u64())
        .or_else(|| rect.get("height")?.as_u64())?;
    Some((rect.get("width")?.as_u64()?, rows)).filter(|&(cols, rows)| cols > 0 && rows > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panes_size_is_its_layout_width_and_terminal_rows() {
        let layout = json!({"layout": {"panes": [
            {"pane_id": "w1:p1", "rect": {"width": 80, "height": 24, "x": 0, "y": 0}},
            {"pane_id": "w1:p2", "rect": {"width": 40, "height": 24, "x": 80, "y": 0}},
        ]}});
        let info = json!({"pane": {"scroll": {"viewport_rows": 30}}});
        assert_eq!(pane_size_of(&layout, &info, "w1:p2"), Some((40, 30)));
        assert_eq!(pane_size_of(&layout, &json!({}), "w1:p2"), Some((40, 24)));
        assert_eq!(pane_size_of(&layout, &info, "w1:p3"), None);
    }

    #[test]
    fn a_line_is_said_once_until_it_changes() {
        let mut changes = Changes::default();
        assert_eq!(changes.fresh("a".into()).as_deref(), Some("a"));
        assert_eq!(changes.fresh("a".into()), None);
        assert_eq!(changes.fresh("b".into()).as_deref(), Some("b"));
        assert_eq!(changes.fresh("a".into()).as_deref(), Some("a"));
    }

    #[test]
    fn emit_writes_only_what_changed() {
        let mut changes = Changes::default();
        let mut out = Vec::new();
        for screen in ["one", "one", "two", "two", "two"] {
            emit(&mut out, &mut changes, json!({ "screen": screen })).unwrap();
        }
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text, "{\"screen\":\"one\"}\n{\"screen\":\"two\"}\n");
    }

    /// herdr's recent lines end with the visible screen (checked against
    /// real panes: both drop trailing blank rows alike), so the history is
    /// what comes before the screen's own number of lines.
    #[test]
    fn history_is_what_scrolled_off_above_the_screen() {
        let recent = "old 1\r\nold 2\r\nshown 1\r\nshown 2";
        assert_eq!(
            history_above(recent, "shown 1\r\nshown 2"),
            "old 1\r\nold 2"
        );
        assert_eq!(
            history_above("shown 1\r\nshown 2", "shown 1\r\nshown 2"),
            ""
        );
        // A screen longer than what was read leaves no history, not a panic.
        assert_eq!(history_above("x", "a\r\nb"), "");
    }

    #[test]
    fn tokens_are_not_shown() {
        let screen = "$ SSF_GITHUB_TOKEN='gho_AbCdEf0123456789xyz' ssf launch\r\n\
                      \x1b[1mghp_0123456789abcdefABCD\x1b[0m github_pat_11ABCDEFG0123456789_abcdefghij";
        let shown = redact_tokens(screen);
        assert!(!shown.contains("gho_AbC"), "{shown}");
        assert!(!shown.contains("ghp_0123"), "{shown}");
        assert!(!shown.contains("github_pat_11"), "{shown}");
        assert_eq!(shown.matches("<redacted>").count(), 3, "{shown}");
        // Prose and short look-alikes stay.
        assert_eq!(
            redact_tokens("the ghp_ prefix, ghs_x"),
            "the ghp_ prefix, ghs_x"
        );
    }

    #[test]
    fn item_panes_take_typing_only_where_the_setting_allows() {
        let load = |text: &str| -> Config { toml::from_str(text).unwrap() };
        let repos = "[[repo]]\nname = \"o/on\"\nharness = \"claude\"\nitem_pane_input = true\n\
                     [[repo]]\nname = \"o/off\"\nharness = \"claude\"\nitem_pane_input = false\n\
                     [[repo]]\nname = \"o/r\"\nharness = \"claude\"\n";
        // Off by default; a scratch session always takes typing.
        let cfg = load(repos);
        assert!(input_refusal(&cfg, "o/r#7").unwrap().contains("view-only"));
        assert_eq!(input_refusal(&cfg, "o/r~ab12"), None);
        // A repository's own say wins either way.
        assert_eq!(input_refusal(&cfg, "o/on#7"), None);
        assert!(input_refusal(&cfg, "o/off#7").is_some());
        // On for the factory: every repository that does not say otherwise.
        let cfg = load(&format!("[daemon]\nitem_pane_input = true\n{repos}"));
        assert_eq!(input_refusal(&cfg, "o/r#7"), None);
        assert_eq!(input_refusal(&cfg, "o/on#7"), None);
        assert!(input_refusal(&cfg, "o/off#7").is_some());
    }

    /// Control of an item's pane is refused where `item_pane_input` is off,
    /// before any pane is looked for; observing it is not.
    #[tokio::test]
    async fn control_is_refused_where_item_pane_input_is_off() {
        let sandbox = crate::config::test_support::sandbox();
        std::fs::create_dir_all(sandbox.config_dir()).unwrap();
        std::fs::write(
            sandbox.config_dir().join("config.toml"),
            "[[repo]]\nname = \"o/r\"\nharness = \"claude\"\n",
        )
        .unwrap();
        let why = control("o/r#7", false, None).await.unwrap().unwrap();
        assert!(why.contains("view-only"), "{why}");
        // Observing goes on to look for the pane, which is not there.
        let error = control("o/r#7", true, None).await.unwrap_err();
        assert!(format!("{error:#}").contains("o/r#7"), "{error:#}");
        // A scratch session has no live terminal of this kind.
        assert!(control("o/r~ab12", true, None).await.is_err());
    }

    #[tokio::test]
    async fn locating_names_what_has_no_pane() {
        let _sandbox = crate::config::test_support::sandbox();
        let cfg: Config =
            toml::from_str("[[repo]]\nname = \"o/r\"\nharness = \"claude\"\n").unwrap();
        let mut state = State::default();
        let rs = state.repo_mut("o/r");
        rs.issues.insert(
            7,
            crate::state::IssueState {
                number: 7,
                shares_workspace_of: Some(3),
                ..Default::default()
            },
        );
        rs.issues.insert(
            3,
            crate::state::IssueState {
                number: 3,
                ..Default::default()
            },
        );
        // Without a workspace there is nothing to mirror, whichever item
        // is named.
        let error = locate(&cfg, &state, "o/r#7").await.err().unwrap();
        assert!(
            format!("{error:#}").contains("has no workspace"),
            "{error:#}"
        );
        let error = locate(&cfg, &state, "o/r~zzzz").await.err().unwrap();
        assert!(
            format!("{error:#}").contains("not a session ssf knows"),
            "{error:#}"
        );
        let error = locate(&cfg, &state, "x/y#1").await.err().unwrap();
        assert!(
            format!("{error:#}").contains("not a watched repository"),
            "{error:#}"
        );
        let error = locate(&cfg, &state, "nonsense").await.err().unwrap();
        assert!(
            format!("{error:#}").contains("expected owner/repo"),
            "{error:#}"
        );
    }
}
