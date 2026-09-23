//! The web pane mirror's factory side (#414): `ssf __pane watch` prints a
//! session's agent pane -- its visible screen with colours -- as JSON lines,
//! one each time it changes, and `ssf __pane send` types into it.
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

/// Where a session's agent is: its driver, and the terminal it runs in.
pub(crate) async fn locate(cfg: &Config, state: &State, session: &str) -> Result<(Driver, String)> {
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
    Ok((driver, pane))
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

/// `ssf __pane watch`: the session's screen as `{"screen": …}` lines, or
/// `{"error": …}` while it cannot be read, until whoever reads stdout goes.
pub(crate) async fn watch(session: &str, interval: Duration) -> Result<()> {
    let cfg = Config::load()?;
    let mut changes = Changes::default();
    let mut target: Option<(Driver, String)> = None;
    let mut reads = 0u32;
    let mut out = std::io::stdout();
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
                    target = None;
                    emit(
                        &mut out,
                        &mut changes,
                        json!({"error": format!("{error:#}")}),
                    )?;
                }
            }
        }
        reads = reads.wrapping_add(1);
        if let Some((driver, pane)) = &target {
            match driver.screen_ansi(pane).await {
                Ok(screen) => emit(&mut out, &mut changes, json!({"screen": screen}))?,
                Err(error) => {
                    target = None;
                    emit(
                        &mut out,
                        &mut changes,
                        json!({"error": format!("{error:#}")}),
                    )?;
                }
            }
        }
        tokio::time::sleep(interval).await;
    }
}

/// One line, if it says something new. A reader that has gone is the end of
/// the watch.
fn emit(out: &mut impl Write, changes: &mut Changes, value: serde_json::Value) -> Result<()> {
    if let Some(line) = changes.fresh(value.to_string()) {
        writeln!(out, "{line}").context("the reader went away")?;
        out.flush().context("the reader went away")?;
    }
    Ok(())
}

/// `ssf __pane send`: type into the session's agent pane.
pub(crate) async fn send(session: &str, text: Option<&str>, keys: &[String]) -> Result<()> {
    let cfg = Config::load()?;
    let state = State::load()?;
    let (driver, pane) = locate(&cfg, &state, session).await?;
    driver.type_input(&pane, text, keys).await
}

#[cfg(test)]
mod tests {
    use super::*;

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
