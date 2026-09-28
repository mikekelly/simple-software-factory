//! A session's agent pane, for the web endpoint's live terminals: `ssf
//! __pane control` (a session's herdr pane: an item's, #563, or a scratch
//! session's, #565) and `ssf __pane input-check` (whether a pane takes
//! typing, `item_pane_input`).
//!
//! These run where the sessions run, as every factory command does: the web
//! endpoint starts them through the same client transport as its status
//! stream, which forwards into the guest when the factory is in a VM. They
//! read the state file and ask the driver directly rather than going through
//! the daemon, which answers its requests one at a time between passes.

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::driver::Driver;
use crate::origin::{Origin, Scratch};
use crate::state::State;

/// Where a session's agent is.
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
        // One started in tmux while scratch sessions ran there (#491) is
        // left there until it stops (#565), with no pane to control.
        if st
            .terminal_handle
            .as_deref()
            .and_then(crate::tmux::name_of)
            .is_some()
            && st
                .worktree_id
                .as_deref()
                .is_some_and(crate::driver::is_local_worktree)
        {
            bail!(
                "{session} still runs in tmux, from before scratch sessions moved to herdr; \
                 its terminal opens here once it has been started again"
            );
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
    Ok((driver, pane))
}

/// Exit status of `ssf __pane control` and `input-check` for a pane the factory does not let a
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

/// `ssf __pane control` (#563, #565): control a session's herdr pane as
/// `herdr terminal session control` NDJSON over stdin and stdout (pipes,
/// never a PTY): typing, and the pane takes this stream's size. The web
/// endpoint holds one per pane and shares it among every viewer.
/// `Ok(Some(why))` is a pane the configuration keeps view-only
/// (`item_pane_input`); nothing is started.
///
/// Control is never `--takeover`: a pane someone else controls refuses this
/// one, and one taken over ends it, with herdr's own words.
pub(crate) async fn control(session: &str, size: Option<(u16, u16)>) -> Result<Option<String>> {
    if Origin::parse(session).is_none() && Scratch::parse(session).is_none() {
        bail!("{session}: expected owner/repo#N or owner/repo~id");
    }
    let cfg = Config::load()?;
    if let Some(why) = input_refusal(&cfg, session) {
        return Ok(Some(why));
    }
    let state = State::load()?;
    let (herdr, pane) = match locate(&cfg, &state, session).await? {
        (Driver::Herdr(herdr), pane) => (herdr, pane),
        #[cfg(test)]
        _ => bail!("{session} has no herdr pane"),
    };
    let mut command = crate::herdr::command(crate::config::herdr_command_path(herdr.command()));
    command.args(["terminal", "session", "control", &pane]);
    if let Some((cols, rows)) = size {
        command.args(["--cols", &cols.to_string(), "--rows", &rows.to_string()]);
    }
    // Control is the stream alone: stdin is its commands, and closing it
    // releases the pane.
    use std::os::unix::process::CommandExt;
    let error = command.exec();
    Err(anyhow::Error::new(error).context("running herdr terminal session control"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    /// before any pane is looked for.
    #[tokio::test]
    async fn control_is_refused_where_item_pane_input_is_off() {
        let sandbox = crate::config::test_support::sandbox();
        std::fs::create_dir_all(sandbox.config_dir()).unwrap();
        std::fs::write(
            sandbox.config_dir().join("config.toml"),
            "[[repo]]\nname = \"o/r\"\nharness = \"claude\"\n",
        )
        .unwrap();
        let why = control("o/r#7", None).await.unwrap().unwrap();
        assert!(why.contains("view-only"), "{why}");
        // A scratch session always takes typing; one ssf does not know has
        // no pane.
        assert!(control("o/r~ab12", None).await.is_err());
        assert!(control("nonsense", None).await.is_err());
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
        // Without a workspace there is no pane, whichever item
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
