//! Desktop integration for Omarchy: the Factory menu entries (status, the
//! service toggle, restart, logs) and the background service toggle. Setup is
//! not done from here: `ssf auth` and `ssf repo` are the CLI for that.

use crate::platform;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;
use tracing::info;

/// The Omarchy plugin id of the bar widget this package used to ship. It is
/// gone from ssf (#413), but an upgraded installation still has it in its
/// shell config and plugin directory until something removes it.
const SUPERSEDED_WIDGET_ID: &str = "ssf.factory";
const MENU_BEGIN: &str =
    "  // ssf:begin (managed by `ssf ui install`; edits inside are overwritten)";
const MENU_END: &str = "  // ssf:end";

/// The home directory the Omarchy integration writes into
/// (`~/.config/omarchy/...`). Guarded like `config::state_dir()`: the test
/// build has no real home to install a plugin into or delete one from, and
/// answers out of the calling thread's sandbox instead (#140).
fn home() -> PathBuf {
    #[cfg(test)]
    {
        crate::config::test_support::home()
    }
    #[cfg(not(test))]
    {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"))
    }
}

/// Where the bar widget's copy lived, still checked to find one to remove.
fn plugin_target_dir() -> PathBuf {
    home()
        .join(".config/omarchy/plugins")
        .join(SUPERSEDED_WIDGET_ID)
}

pub fn menu_extension_path() -> PathBuf {
    home().join(".config/omarchy/extensions/omarchy-menu.jsonc")
}

fn run_quiet(cmd: &str, args: &[&str]) -> Result<String> {
    run_quiet_path(
        std::path::Path::new(cmd),
        cmd,
        args,
        std::env::var_os("OMARCHY_PATH"),
    )
}

fn run_quiet_path(
    path: &std::path::Path,
    cmd: &str,
    args: &[&str],
    inherited_omarchy_path: Option<std::ffi::OsString>,
) -> Result<String> {
    let mut command = Command::new(path);
    command.args(args);
    // Omarchy's plugin scripts source their library through OMARCHY_PATH.
    // Services and other non-interactive callers do not inherit the shell
    // profile that exports it, while the supported system installation is
    // still rooted here.
    if cmd.starts_with("omarchy-plugin-") {
        command.env(
            "OMARCHY_PATH",
            inherited_omarchy_path.unwrap_or_else(|| "/usr/share/omarchy".into()),
        );
    }
    let out = command.output().with_context(|| format!("running {cmd}"))?;
    if !out.status.success() {
        bail!(
            "{cmd} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// What [`remove_superseded_widget`] did, so its callers can say it without
/// guessing at the state it left behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovedWidget {
    /// Nothing to do: none was here, or it had already been dealt with. A
    /// plugin manager checkout that is already disabled lands here too -- it
    /// is not this package's to remove, and [`superseded_widget_note`] is
    /// what reports it.
    Nothing,
    /// The widget is disabled and the package's copy of it is gone.
    Removed,
    /// The widget is disabled; a plugin manager checkout remains, and
    /// `omarchy plugin remove ssf.factory` is what removes that.
    Disabled,
}

/// Disable and remove the Omarchy bar widget this package used to ship
/// (#413). A marketplace git checkout is left for `omarchy plugin remove
/// ssf.factory`, which owns it, but its widget is disabled all the same, so
/// nothing is left in the bar.
pub fn remove_superseded_widget() -> Result<RemovedWidget> {
    remove_superseded_widget_with(platform::is_omarchy(), || {
        run_quiet("omarchy-plugin-disable", &[SUPERSEDED_WIDGET_ID])
    })
}

fn remove_superseded_widget_with(
    is_omarchy: bool,
    mut disable: impl FnMut() -> Result<String>,
) -> Result<RemovedWidget> {
    if !is_omarchy {
        return Ok(RemovedWidget::Nothing);
    }
    let mut disabled = false;
    if widget_enabled()? {
        // Disable first: deleting the files under a widget the shell still
        // shows would leave it in the bar with nothing behind it.
        disable().context("could not disable the superseded bar widget")?;
        disabled = true;
    }
    let dst = plugin_target_dir();
    if dst.join(".git").exists() {
        return Ok(match disabled {
            true => RemovedWidget::Disabled,
            // Disabled already, by an earlier run: what is left is the
            // plugin manager's.
            false => RemovedWidget::Nothing,
        });
    }
    let mut removed = disabled;
    if let Ok(meta) = std::fs::symlink_metadata(&dst) {
        if meta.file_type().is_symlink() {
            std::fs::remove_file(&dst)?;
            removed = true;
        } else if meta.is_dir() {
            std::fs::remove_dir_all(&dst)?;
            removed = true;
        }
    }
    if removed {
        info!("removed the superseded Omarchy bar widget");
    }
    Ok(match removed {
        true => RemovedWidget::Removed,
        false => RemovedWidget::Nothing,
    })
}

/// The line `ssf doctor` prints about the bar widget this package used to
/// ship, or `None` when there is nothing to say (#413): another desktop has
/// none, and neither has an installation that has already upgraded.
///
/// What it says follows what is actually left. `ssf setup` and `ssf ui
/// install` disable the widget and remove the package's copy of it, but a
/// checkout `omarchy plugin add` made is the plugin manager's and stays
/// ([`remove_superseded_widget_with`]); once it is disabled, those commands
/// have nothing left to do and the note must not send anyone back to them.
///
/// `ssf doctor` is answered inside the guest when the factory is in a VM,
/// and no guest can read this host's `~/.config/omarchy`, so the host prints
/// this same line before it forwards the command there.
pub fn superseded_widget_note() -> Option<String> {
    superseded_widget_note_with(platform::is_omarchy())
}

fn superseded_widget_note_with(is_omarchy: bool) -> Option<String> {
    if !is_omarchy {
        return None;
    }
    let target = plugin_target_dir();
    let enabled = widget_enabled().unwrap_or(false);
    if !enabled && target.join(".git").exists() {
        return Some(
            "superseded bar widget: disabled; `omarchy plugin remove ssf.factory` removes the checkout"
                .to_string(),
        );
    }
    (enabled || std::fs::symlink_metadata(&target).is_ok()).then(|| {
        "superseded bar widget: still installed; `ssf ui install` or `ssf setup` disables and removes it"
            .to_string()
    })
}

fn widget_enabled() -> Result<bool> {
    let path = home().join(".config/omarchy/shell.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    Ok(raw.contains(&format!("\"{SUPERSEDED_WIDGET_ID}\"")))
}

/// Is the Factory menu block in place?
pub fn menu_present() -> bool {
    std::fs::read_to_string(menu_extension_path())
        .map(|t| t.contains(MENU_BEGIN))
        .unwrap_or(false)
}

/// The Factory submenu: what shows state and the one control. Nothing here
/// signs the bot in or edits the watched repositories.
fn menu_block() -> String {
    let rows = [
        (
            "factory",
            r#"{"icon":"","label":"Factory","aliases":["ssf","software-factory"],"description":"Simple Software Factory: GitHub issues to agents"}"#,
        ),
        (
            "factory.dashboard",
            r#"{"icon":"","label":"Dashboard","action":"omarchy-launch-terminal ssf dashboard"}"#,
        ),
        (
            "factory.status",
            r#"{"icon":"󰋼","label":"Status","action":"omarchy-launch-floating-terminal-with-presentation ssf-ui status"}"#,
        ),
        (
            "factory.toggle",
            r#"{"icon":"","label":"Service enabled","checked":"ssf-ui service is-enabled","action":"ssf-ui service toggle"}"#,
        ),
        (
            "factory.restart",
            r#"{"icon":"","label":"Restart service","action":"ssf-ui service restart"}"#,
        ),
        (
            "factory.logs",
            r#"{"icon":"","label":"Logs","action":"ssf-ui logs"}"#,
        ),
    ];
    let mut s = String::new();
    s.push_str(MENU_BEGIN);
    s.push('\n');
    for (i, (id, body)) in rows.iter().enumerate() {
        s.push_str(&format!(
            "  \"{id}\": {body}{}\n",
            if i + 1 < rows.len() { "," } else { "" }
        ));
    }
    s.push_str(MENU_END);
    s
}

/// Last significant (non-comment, non-whitespace) character before `end`.
fn last_significant_char(text: &str) -> Option<(usize, char)> {
    // Strip comments in a single pass, remembering byte offsets.
    let bytes: Vec<char> = text.chars().collect();
    let mut result = None;
    let mut i = 0;
    let mut in_str = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            result = Some((i, c));
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
            result = Some((i, c));
        } else if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == '/' {
            while i < bytes.len() && bytes[i] != '\n' {
                i += 1;
            }
            continue;
        } else if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == '*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == '*' && bytes[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        } else if !c.is_whitespace() {
            result = Some((i, c));
        }
        i += 1;
    }
    result
}

/// Insert or refresh the managed block inside the user's menu extension file.
/// Off Omarchy there is no menu: nothing is written.
pub fn install_menu() -> Result<bool> {
    if !platform::is_omarchy() {
        return Ok(false);
    }
    let path = menu_extension_path();
    let existing = std::fs::read_to_string(&path).unwrap_or_else(|_| "{\n}\n".to_string());
    let block = menu_block();
    let updated = merge_menu_text(&existing, &block)?;
    if updated == existing {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::config::write_atomic(&path, updated.as_bytes(), 0o644)?;
    info!(path = %path.display(), "installed Factory entries in the Omarchy menu");
    Ok(true)
}

pub fn merge_menu_text(existing: &str, block: &str) -> Result<String> {
    if let (Some(b), Some(e)) = (existing.find(MENU_BEGIN), existing.find(MENU_END)) {
        if e < b {
            bail!("menu extension has a malformed ssf block");
        }
        let end = e + MENU_END.len();
        let mut s = String::new();
        s.push_str(&existing[..b]);
        s.push_str(block);
        s.push_str(&existing[end..]);
        return Ok(s);
    }
    let chars: Vec<char> = existing.chars().collect();
    // Find the final closing brace of the root object.
    let close_idx = chars
        .iter()
        .rposition(|c| *c == '}')
        .context("menu extension file has no closing brace")?;
    let head: String = chars[..close_idx].iter().collect();
    let tail: String = chars[close_idx..].iter().collect();
    let needs_comma = match last_significant_char(&head) {
        Some((_, '{')) | Some((_, ',')) | None => false,
        Some(_) => true,
    };
    let mut s = head.trim_end_matches([' ', '\t']).to_string();
    if !s.ends_with('\n') {
        s.push('\n');
    }
    if needs_comma {
        // Append the comma to the last significant line rather than starting a
        // line with it, so the file keeps reading naturally.
        let (idx, _) = last_significant_char(&s).expect("checked above");
        let byte_idx = s
            .char_indices()
            .nth(idx)
            .map(|(b, c)| b + c.len_utf8())
            .unwrap_or(s.len());
        s.insert(byte_idx, ',');
    }
    s.push_str(block);
    s.push('\n');
    s.push_str(&tail);
    Ok(s)
}

/// Removes the menu entries; `Ok(true)` when there were some to remove.
pub fn uninstall_menu() -> Result<bool> {
    let path = menu_extension_path();
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    let Some(updated) = remove_menu_text(&existing) else {
        return Ok(false);
    };
    crate::config::write_atomic(&path, updated.as_bytes(), 0o644)?;
    Ok(true)
}

/// The menu extension text without the managed block, or `None` when there
/// is no block to remove.
pub fn remove_menu_text(existing: &str) -> Option<String> {
    let (b, e) = (existing.find(MENU_BEGIN)?, existing.find(MENU_END)?);
    let mut end = e + MENU_END.len();
    if existing[end..].starts_with('\n') {
        end += 1;
    }
    let mut s = String::new();
    s.push_str(&existing[..b]);
    s.push_str(&existing[end..]);
    // Drop a comma we added if it is now dangling before the closing brace.
    let trimmed = s.trim_end();
    if let Some(stripped) = trimmed.strip_suffix('}')
        && let Some((idx, ',')) = last_significant_char(stripped)
    {
        let mut chars: Vec<char> = stripped.chars().collect();
        chars.remove(idx);
        let mut rebuilt: String = chars.into_iter().collect();
        rebuilt.push_str("}\n");
        s = rebuilt;
    }
    Some(s)
}

pub fn service_enabled() -> bool {
    if platform::is_macos() {
        if platform::service_target().is_some() {
            return platform::target_launchd_plist().is_some_and(|path| path.is_file());
        }
        return service_active();
    }
    let unit = platform::service_unit();
    Command::new("systemctl")
        .args(["--user", "is-enabled", "--quiet", &unit])
        .stdin(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub fn service_failed() -> bool {
    let unit = platform::service_unit();
    !platform::is_macos()
        && Command::new("systemctl")
            .args(["--user", "is-failed", "--quiet", &unit])
            .stdin(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

/// What to do with a service command (`systemctl`, `brew services`) that
/// failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnServiceError {
    /// Print it and carry on: the marker stands (the daemon is meant to
    /// stay off), `ssf ui service enable|disable` reports it, and `ssf ui
    /// service status` shows what came of the daemon.
    Warn,
    /// Return it, and undo a disable's marker: `ssf uninstall` stops on
    /// this step rather than destroying the VM and removing the state
    /// under a daemon that may still be running, so nothing is meant to
    /// have changed when it fails -- least of all a marker that says the
    /// service is disabled when it is still up.
    Fail,
}

pub fn set_service_enabled(enabled: bool) -> Result<()> {
    set_service_enabled_on_error(enabled, OnServiceError::Warn)
}

/// [`set_service_enabled`], choosing what a failed service command does.
pub fn set_service_enabled_on_error(enabled: bool, on_error: OnServiceError) -> Result<()> {
    if enabled && platform::service_target().is_some() && legacy_service_enabled_or_active()? {
        let stop = if platform::is_macos() {
            platform::service_hint_for("macos", "stop")
        } else {
            "systemctl --user disable --now ssf.service".into()
        };
        bail!(
            "the legacy singleton service is still enabled or active; stop it first with `{stop}`, then enable the selected target service"
        );
    }
    let (result, what) = if platform::is_macos() {
        if enabled {
            (crate::platform::service_start(), "start")
        } else {
            (crate::platform::service_stop(), "stop")
        }
    } else {
        let action = if enabled { "enable" } else { "disable" };
        let unit = platform::service_unit();
        let out = Command::new("systemctl")
            .args(["--user", action, "--now", &unit])
            .stdin(std::process::Stdio::null())
            .output()
            .context("running systemctl")
            .and_then(|out| {
                if out.status.success() {
                    Ok(())
                } else {
                    bail!(
                        "`systemctl --user {action} --now {}` failed: {}",
                        unit,
                        String::from_utf8_lossy(&out.stderr).trim()
                    )
                }
            });
        (out, action)
    };
    report_service(result, what, on_error)
}

pub(crate) fn legacy_service_enabled_or_active() -> Result<bool> {
    if platform::is_macos() {
        let enabled = dirs::home_dir().is_some_and(|home| {
            home.join("Library/LaunchAgents/homebrew.mxcl.ssf.plist")
                .is_file()
        });
        return Ok(enabled || platform::legacy_service_active());
    }
    for action in ["is-enabled", "is-active"] {
        let output = Command::new("systemctl")
            .args(["--user", action, "--quiet", platform::SERVICE])
            .stdin(std::process::Stdio::null())
            .output()
            .context("checking the legacy singleton service")?;
        if output.status.success() {
            return Ok(true);
        }
        if !matches!(output.status.code(), Some(1 | 3 | 4)) {
            bail!(
                "could not inspect the legacy singleton service: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
    }
    Ok(false)
}

/// Say what systemctl (or `brew services`) said when it failed. It used
/// to run with this terminal, so its error was on screen; it is captured
/// now, and dropping it left `ssf ui service enable` printing "service
/// enabled" over a daemon that had not started.
fn report_service(result: Result<()>, what: &str, on_error: OnServiceError) -> Result<()> {
    match (result, on_error) {
        (Ok(()), _) => Ok(()),
        (Err(e), OnServiceError::Warn) => {
            eprintln!("warning: could not {what} the service: {e:#}");
            Ok(())
        }
        (Err(e), OnServiceError::Fail) => Err(e.context(format!(
            "could not {what} the {} service",
            platform::service_name()
        ))),
    }
}

/// Is the daemon's service running (the guest's system unit inside the VM,
/// the user unit or the launchd service on the host)?
pub fn service_active() -> bool {
    crate::platform::service_active()
}

pub fn install_all(quiet: bool) -> Result<()> {
    if !platform::is_omarchy() {
        // Nothing under ~/.config/omarchy is made on another desktop.
        if !quiet {
            println!("not on Omarchy: no Factory menu entries to install");
        }
        return Ok(());
    }
    let mut notes = Vec::new();
    // The widget was this package's too, so an upgraded installation still
    // showing one is cleaned up here: `ssf ui install` is the one step it
    // takes to get rid of it (#413). It says what it did -- a plugin
    // manager's checkout is disabled and left, not removed.
    match remove_superseded_widget() {
        Ok(RemovedWidget::Removed) => notes.push("superseded bar widget removed".to_string()),
        Ok(RemovedWidget::Disabled) => notes.push(
            "superseded bar widget disabled; `omarchy plugin remove ssf.factory` removes the checkout"
                .to_string(),
        ),
        Ok(RemovedWidget::Nothing) => {}
        Err(e) => notes.push(format!("superseded bar widget: {e:#}")),
    }
    match install_menu() {
        Ok(true) => notes.push("menu entries installed".to_string()),
        Ok(false) => notes.push("menu entries already installed".to_string()),
        Err(e) => notes.push(format!("menu entries: {e:#}")),
    }
    if !quiet {
        for n in notes {
            println!("{n}");
        }
    }
    Ok(())
}

pub fn uninstall_all() -> Result<()> {
    // The two are independent, and both run: the leftover widget is no longer
    // this package's, so its disable failing -- a host without the Omarchy
    // shell commands can -- must not leave ssf's own menu entries in place
    // (#413). The failure is said out loud, as `install_all` says it, and the
    // menu entries come out either way.
    let widget = match remove_superseded_widget() {
        Ok(what) => what,
        Err(e) => {
            eprintln!("warning: could not remove the superseded bar widget: {e:#}");
            RemovedWidget::Nothing
        }
    };
    let menu = uninstall_menu()?;
    match widget {
        RemovedWidget::Removed => println!("removed the superseded bar widget"),
        RemovedWidget::Disabled => println!(
            "disabled the superseded bar widget; `omarchy plugin remove ssf.factory` removes the checkout"
        ),
        RemovedWidget::Nothing => {}
    }
    if menu {
        println!("removed the Factory menu entries");
    } else if widget == RemovedWidget::Nothing {
        if platform::is_omarchy() {
            println!("no Factory menu entries to remove");
        } else {
            println!("not on Omarchy: no Factory menu entries to remove");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn omarchy_commands_get_the_supported_root_without_a_shell_profile() {
        let root = std::env::temp_dir().join(format!("ssf-omarchy-command-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let command = root.join("disable");
        crate::test_support::write_executable(
            &command,
            "#!/bin/bash\n[[ ${1:-} == fail ]] && { echo failed >&2; exit 42; }\n[[ ${OMARCHY_PATH:-} == /usr/share/omarchy ]] || { echo wrong-root >&2; exit 43; }\nprintf disabled\n",
        );

        assert_eq!(
            run_quiet_path(
                &command,
                "omarchy-plugin-disable",
                &[SUPERSEDED_WIDGET_ID],
                None
            )
            .unwrap(),
            "disabled"
        );
        let custom = run_quiet_path(
            &command,
            "omarchy-plugin-disable",
            &[SUPERSEDED_WIDGET_ID],
            Some("/custom/omarchy".into()),
        )
        .unwrap_err();
        assert!(
            format!("{custom:#}").contains("wrong-root"),
            "an inherited custom Omarchy root must not be replaced: {custom:#}"
        );
        let err = run_quiet_path(&command, "omarchy-plugin-disable", &["fail"], None).unwrap_err();
        assert!(format!("{err:#}").contains("failed"));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The widget is disabled before anything of it is deleted, the outcome
    /// says what was actually done, and a marketplace checkout -- the plugin
    /// manager's, not the package's -- is left behind even then.
    #[test]
    fn removing_the_superseded_widget_disables_it_before_deleting_anything() {
        let sandbox = crate::config::test_support::sandbox();
        let shell_config = sandbox.home().join(".config/omarchy/shell.json");
        std::fs::create_dir_all(shell_config.parent().unwrap()).unwrap();
        std::fs::write(
            &shell_config,
            format!("{{\"right\":[\"{SUPERSEDED_WIDGET_ID}\"]}}"),
        )
        .unwrap();

        assert_eq!(
            remove_superseded_widget_with(false, || unreachable!("off Omarchy")).unwrap(),
            RemovedWidget::Nothing,
            "another desktop has no Omarchy widget to remove"
        );

        let checkout = plugin_target_dir();
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        let err =
            remove_superseded_widget_with(true, || anyhow::bail!("disable refused")).unwrap_err();
        assert!(format!("{err:#}").contains("disable refused"));
        assert!(
            checkout.join(".git").is_dir(),
            "a failed disable must abort before the checkout can be removed"
        );

        let disabled = std::cell::Cell::new(0);
        assert_eq!(
            remove_superseded_widget_with(true, || {
                disabled.set(disabled.get() + 1);
                Ok("disabled".to_string())
            })
            .unwrap(),
            RemovedWidget::Disabled,
            "a checkout is disabled and left, and the outcome says so"
        );
        assert_eq!(disabled.get(), 1);
        assert!(
            checkout.join(".git").is_dir(),
            "the plugin manager removes its own checkout"
        );

        // Disabled already -- what `omarchy-plugin-disable` leaves in the
        // shell config -- so the leftover is the plugin manager's, and this
        // command has nothing left to do with it.
        std::fs::write(&shell_config, "{\"right\":[\"omarchy.agents\"]}").unwrap();
        assert_eq!(
            remove_superseded_widget_with(true, || unreachable!("already disabled")).unwrap(),
            RemovedWidget::Nothing,
            "a disabled checkout is not this package's to remove"
        );

        // A copy the package made is not so protected.
        std::fs::remove_dir_all(&checkout).unwrap();
        std::fs::create_dir_all(checkout.join("marketplace")).unwrap();
        assert_eq!(
            remove_superseded_widget_with(true, || Ok("disabled".to_string())).unwrap(),
            RemovedWidget::Removed,
            "the packaged copy is removed, and the outcome says so"
        );
        assert!(!checkout.exists(), "the packaged copy is removed");
    }

    /// `ssf doctor` must not send anyone back to a command that will do
    /// nothing: after `ssf ui install` has disabled a checkout made by
    /// `omarchy plugin add`, the only thing left is the plugin manager's, and
    /// the note has to say so instead.
    #[test]
    fn the_doctor_note_says_what_is_still_to_do() {
        let sandbox = crate::config::test_support::sandbox();
        assert_eq!(
            superseded_widget_note_with(false),
            None,
            "another desktop has no widget to report"
        );
        assert_eq!(
            superseded_widget_note_with(true),
            None,
            "an upgraded installation is not nagged about"
        );

        let target = plugin_target_dir();
        std::fs::create_dir_all(target.join("marketplace")).unwrap();
        let note = superseded_widget_note_with(true).expect("the package's copy is reported");
        assert!(note.contains("still installed"), "{note}");
        assert!(note.contains("`ssf ui install` or `ssf setup`"), "{note}");

        std::fs::remove_dir_all(&target).unwrap();
        std::fs::create_dir_all(target.join(".git")).unwrap();
        let note = superseded_widget_note_with(true).expect("a disabled checkout is reported");
        assert!(note.contains("disabled"), "{note}");
        assert!(note.contains("omarchy plugin remove ssf.factory"), "{note}");
        assert!(
            !note.contains("ssf ui install"),
            "what is left is the plugin manager's to remove: {note}"
        );

        // Enabled again: whatever else is there, the upgrade step has work.
        let shell_config = sandbox.home().join(".config/omarchy/shell.json");
        std::fs::create_dir_all(shell_config.parent().unwrap()).unwrap();
        std::fs::write(
            &shell_config,
            format!("{{\"right\":[\"{SUPERSEDED_WIDGET_ID}\"]}}"),
        )
        .unwrap();
        let note = superseded_widget_note_with(true).expect("an enabled widget is reported");
        assert!(note.contains("still installed"), "{note}");
    }

    #[test]
    fn a_service_command_that_failed_is_a_warning_or_the_answer() {
        // `ssf ui service disable` reports the marker, which was written
        // whatever systemd said, so a failed `systemctl stop` is a
        // warning there. `ssf uninstall` is the other case: it goes on to
        // destroy the VM and remove the state, so "service stopped and
        // disabled" over a `brew services stop` that failed would leave a
        // live daemon working on files being deleted.
        assert!(report_service(Ok(()), "stop", OnServiceError::Warn).is_ok());
        assert!(report_service(Ok(()), "stop", OnServiceError::Fail).is_ok());
        assert!(
            report_service(
                Err(anyhow::anyhow!("brew services stop said no")),
                "stop",
                OnServiceError::Warn,
            )
            .is_ok()
        );
        let err = report_service(
            Err(anyhow::anyhow!("brew services stop said no")),
            "stop",
            OnServiceError::Fail,
        )
        .unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("could not stop the"), "{text}");
        assert!(text.contains("brew services stop said no"), "{text}");
    }

    #[test]
    fn merges_into_comment_only_file() {
        let existing = "{\n  // Extend the menu.\n  // \"personal\": {\"icon\":\"\",\"label\":\"Personal\"},\n}\n";
        let out = merge_menu_text(existing, "  // ssf:begin (managed by `ssf ui install`; edits inside are overwritten)\n  \"factory\": {}\n  // ssf:end").unwrap();
        assert!(out.contains("\"factory\": {}"));
        assert!(!out.contains(",  // ssf:begin"));
        assert!(out.trim_end().ends_with('}'));
    }

    #[test]
    fn adds_comma_after_existing_entry() {
        let existing = "{\n  \"personal\": {\"label\":\"Personal\"}\n}\n";
        let out = merge_menu_text(existing, "  // ssf:begin (managed by `ssf ui install`; edits inside are overwritten)\n  \"factory\": {}\n  // ssf:end").unwrap();
        assert!(out.contains("{\"label\":\"Personal\"},\n"), "{out}");
    }

    #[test]
    fn install_then_uninstall_round_trips() {
        let existing = "{\n  \"personal\": {\"label\":\"Personal\"}\n}\n";
        let installed = merge_menu_text(existing, &menu_block()).unwrap();
        assert!(installed.contains("\"factory.toggle\""));
        assert_eq!(remove_menu_text(&installed).as_deref(), Some(existing));
        assert_eq!(remove_menu_text(existing), None);
    }

    /// The menu is a dashboard: status, the service toggle, restart and
    /// logs. Signing the bot in and watching repositories are the CLI's.
    #[test]
    fn menu_has_state_entries_and_no_setup_flows() {
        let block = menu_block();
        for id in [
            "\"factory\"",
            "\"factory.dashboard\"",
            "\"factory.status\"",
            "\"factory.toggle\"",
            "\"factory.restart\"",
            "\"factory.logs\"",
        ] {
            assert!(block.contains(id), "menu lacks {id}: {block}");
        }
        assert!(block.contains("\"action\":\"omarchy-launch-terminal ssf dashboard\""));
        assert!(block.contains("\"checked\":\"ssf-ui service is-enabled\""));
        assert!(block.contains("\"action\":\"ssf-ui service toggle\""));
        for gone in [
            "login",
            "add-repo",
            "manage-repos",
            "edit-repo",
            "Sign in",
            "Watch a",
        ] {
            assert!(!block.contains(gone), "menu still has {gone}: {block}");
        }
        // Every row is one JSON object, and the block parses once wrapped.
        let body: String = block
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let parsed: serde_json::Value =
            serde_json::from_str(&format!("{{\n{body}\n}}")).expect("menu rows are JSON");
        assert_eq!(parsed.as_object().map(|o| o.len()), Some(6));
    }

    /// `bin/ssf-ui` is what the **Factory** menu entries call; it only shows
    /// and reaches state. The setup flows (sign in, add or edit a
    /// repository) went in #110, and the two commands the removed bar widget
    /// had of its own -- the session list and opening a session's workspace
    /// (#413) -- are gone with it.
    #[test]
    fn helper_keeps_only_the_menu_commands() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let helper = std::fs::read_to_string(root.join("bin/ssf-ui")).unwrap();
        for gone in [
            "ssf-ui login",
            "ssf-ui add-repo",
            "ssf-ui edit-repo",
            "ssf-ui manage-repos",
            "omarchy-menu-input",
            "omarchy-menu-select",
        ] {
            assert!(!helper.contains(gone), "ssf-ui still has {gone:?}");
        }
        for gone in [
            "ssf auth login --",
            "ssf repo add \"",
            "ssf repo set \"",
            "ssf repo remove \"",
            "ssf repo list --json",
            "ssf agents",
            "ssf models",
        ] {
            assert!(!helper.contains(gone), "ssf-ui still runs {gone:?}");
        }
        // The commands the menu block names, and nothing else.
        for cmd in ["service)", "logs)", "status)"] {
            assert!(helper.contains(cmd), "ssf-ui lost the {cmd} command");
        }
        for gone in [
            "login)",
            "add-repo)",
            "edit-repo)",
            "manage-repos)",
            "peers)",
            "open-workspace)",
        ] {
            assert!(!helper.contains(gone), "ssf-ui still dispatches {gone}");
        }
    }

    #[test]
    fn replaces_existing_block() {
        let existing = format!("{{\n{MENU_BEGIN}\n  \"old\": {{}}\n{MENU_END}\n}}\n");
        let out = merge_menu_text(
            &existing,
            &format!("{MENU_BEGIN}\n  \"new\": {{}}\n{MENU_END}"),
        )
        .unwrap();
        assert!(out.contains("\"new\""));
        assert!(!out.contains("\"old\""));
    }

    /// The menu's install and uninstall write and delete under
    /// `~/.config/omarchy`, and the superseded widget is removed under it
    /// too; in a test they must land in the sandbox instead, and without one
    /// they are refused (#140).
    #[test]
    fn the_omarchy_paths_hang_off_the_sandbox() {
        let sb = crate::config::test_support::sandbox();
        assert!(plugin_target_dir().starts_with(sb.home()));
        assert!(menu_extension_path().starts_with(sb.home()));
    }

    #[test]
    #[should_panic(expected = "reached the real home directory")]
    fn without_a_sandbox_the_home_directory_is_refused() {
        let _ = plugin_target_dir();
    }
}
