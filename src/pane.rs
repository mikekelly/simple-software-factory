//! A session's agent pane, for the web endpoint's live terminals:
//! `ssf __pane attach` (a scratch session's tmux session, #491), `ssf __pane
//! control` (an item session's herdr pane, #563) and `ssf __pane
//! input-check` (whether a pane takes typing, `item_pane_input`).
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

/// Where a session's agent runs: a driver's terminal (an item's session,
/// or a scratch session still in a herdr pane from before #491), or a
/// scratch session's tmux session.
pub(crate) enum Target {
    Driver(Driver, String),
    Tmux(crate::tmux::Tmux, String),
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

/// `ssf __pane attach`: attach this terminal to a scratch session's tmux
/// session (#491), for the web dashboard's `api/term`, which runs this in a
/// PTY. Only a scratch session in tmux can be attached to; an item's
/// session is herdr's, controlled by `ssf __pane control` instead.
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

/// `ssf __pane control` (#563): control an item session's herdr pane as
/// `herdr terminal session control` NDJSON over stdin and stdout (pipes,
/// never a PTY): typing, and the pane takes this stream's size. The web
/// endpoint holds one per pane and shares it among every viewer.
/// `Ok(Some(why))` is a pane the configuration keeps view-only
/// (`item_pane_input`); nothing is started.
///
/// Control is never `--takeover`: a pane someone else controls refuses this
/// one, and one taken over ends it, with herdr's own words.
pub(crate) async fn control(session: &str, size: Option<(u16, u16)>) -> Result<Option<String>> {
    if Origin::parse(session).is_none() {
        bail!("{session}: only an item session (owner/repo#N) has a live terminal");
    }
    let cfg = Config::load()?;
    if let Some(why) = input_refusal(&cfg, session) {
        return Ok(Some(why));
    }
    let state = State::load()?;
    let Target::Driver(Driver::Herdr(herdr), pane) = locate(&cfg, &state, session).await? else {
        bail!("{session} has no herdr pane");
    };
    let mut command =
        tokio::process::Command::new(crate::config::herdr_command_path(herdr.command()));
    command.args(["terminal", "session", "control", &pane]);
    for name in [
        "HERDR_WORKSPACE_ID",
        "HERDR_TAB_ID",
        "HERDR_PANE_ID",
        "HERDR_ENV",
    ] {
        command.env_remove(name);
    }
    if let Some((cols, rows)) = size {
        command.args(["--cols", &cols.to_string(), "--rows", &rows.to_string()]);
    }
    // Control is the stream alone: stdin is its commands, and closing it
    // releases the pane.
    use std::os::unix::process::CommandExt;
    let error = command.as_std_mut().exec();
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
        // A scratch session has no live terminal of this kind.
        assert!(control("o/r~ab12", None).await.is_err());
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
