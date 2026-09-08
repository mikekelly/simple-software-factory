//! Desktop integration for Omarchy: the bar widget plugin (a dashboard of the
//! factory's state), the menu entries (status, the service toggle, restart,
//! logs) and the background service toggle. Setup is not done from here:
//! `ssf auth` and `ssf repo` are the CLI for that.

use crate::platform;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;
use tracing::{info, warn};

pub const PLUGIN_ID: &str = "ssf.factory";
const MENU_BEGIN: &str =
    "  // ssf:begin (managed by `ssf ui install`; edits inside are overwritten)";
const MENU_END: &str = "  // ssf:end";

/// Where the package installs the plugin sources.
pub fn plugin_source_dir() -> PathBuf {
    if let Ok(d) = std::env::var("SSF_PLUGIN_DIR") {
        return PathBuf::from(d);
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_default();
    let candidates = [
        PathBuf::from("/usr/share/ssf/omarchy-plugin"),
        exe_dir.join("../share/ssf/omarchy-plugin"),
        exe_dir.join("../../omarchy-plugin"),
    ];
    candidates
        .into_iter()
        .find(|p| p.join("manifest.json").exists())
        .unwrap_or_else(|| PathBuf::from("/usr/share/ssf/omarchy-plugin"))
}

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

pub fn plugin_target_dir() -> PathBuf {
    home().join(".config/omarchy/plugins").join(PLUGIN_ID)
}

pub fn menu_extension_path() -> PathBuf {
    home().join(".config/omarchy/extensions/omarchy-menu.jsonc")
}

pub fn disabled_marker() -> PathBuf {
    crate::config::state_dir().join("disabled")
}

fn run_quiet(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("running {cmd}"))?;
    if !out.status.success() {
        bail!(
            "{cmd} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Copy the packaged plugin into the user's plugin directory (the shell
/// refuses symlinked plugins) and add the widget to the bar. Idempotent: files
/// are rewritten only when the packaged copy differs. Off Omarchy there is
/// no bar: nothing is written.
pub fn install_plugin() -> Result<bool> {
    if !platform::is_omarchy() {
        return Ok(false);
    }
    let src = plugin_source_dir();
    if !src.join("manifest.json").exists() {
        bail!("plugin sources not found at {}", src.display());
    }
    let dst = plugin_target_dir();
    let mut changed = false;
    if let Ok(meta) = std::fs::symlink_metadata(&dst)
        && meta.file_type().is_symlink()
    {
        std::fs::remove_file(&dst)?;
        changed = true;
    }
    std::fs::create_dir_all(&dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in std::fs::read_dir(&src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let data = std::fs::read(&from)?;
        if std::fs::read(&to).ok().as_deref() != Some(data.as_slice()) {
            crate::config::write_atomic(&to, &data, 0o644)?;
            changed = true;
        }
    }
    if changed {
        info!(path = %dst.display(), "installed bar widget files");
    }
    if platform::which("omarchy-plugin-enable").is_none() {
        warn!(
            "omarchy shell commands not found; widget files installed but not enabled in the bar"
        );
        return Ok(changed);
    }
    if !widget_enabled()? {
        // Sit next to the Agents widget when it is present, otherwise on the right.
        let res = run_quiet(
            "omarchy-plugin-enable",
            &[
                PLUGIN_ID,
                "--section",
                "right",
                "--before",
                "omarchy.agents",
            ],
        )
        .or_else(|_| run_quiet("omarchy-plugin-enable", &[PLUGIN_ID, "--section", "right"]));
        match res {
            Ok(_) => {
                info!("enabled {PLUGIN_ID} in the Omarchy bar");
                changed = true;
            }
            Err(e) => warn!("could not enable bar widget: {e:#}"),
        }
    }
    Ok(changed)
}

pub fn widget_enabled() -> Result<bool> {
    let path = home().join(".config/omarchy/shell.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    Ok(raw.contains(&format!("\"{PLUGIN_ID}\"")))
}

/// Is any of the desktop integration in place: the widget files (a
/// directory, or the symlink older installs made) or the menu block?
pub fn desktop_present() -> bool {
    let widget = std::fs::symlink_metadata(plugin_target_dir()).is_ok();
    let menu = std::fs::read_to_string(menu_extension_path())
        .map(|t| t.contains(MENU_BEGIN))
        .unwrap_or(false);
    widget || menu
}

/// Removes the bar widget; `Ok(true)` when there was one to remove.
pub fn uninstall_plugin() -> Result<bool> {
    if platform::is_omarchy()
        && widget_enabled()?
        && let Err(e) = run_quiet("omarchy-plugin-disable", &[PLUGIN_ID])
    {
        warn!("could not disable bar widget: {e:#}");
    }
    let dst = plugin_target_dir();
    if let Ok(meta) = std::fs::symlink_metadata(&dst) {
        if meta.file_type().is_symlink() {
            std::fs::remove_file(&dst)?;
            return Ok(true);
        } else if meta.is_dir() {
            std::fs::remove_dir_all(&dst)?;
            return Ok(true);
        }
    }
    Ok(false)
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
    !disabled_marker().exists()
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
    let marker = disabled_marker();
    let (result, what) = if enabled {
        if marker.exists() {
            std::fs::remove_file(&marker)?;
        }
        (crate::platform::service_start(), "start")
    } else {
        if let Some(parent) = marker.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&marker, "created by `ssf ui service disable`\n")?;
        (crate::platform::service_stop(), "stop")
    };
    // The marker is written before the service command because it records
    // what was asked for, and `ssf ui service disable` means it even when
    // the stop went wrong (the daemon is meant to stay off, and the next
    // start is what clears it). A caller that treats the failure as fatal
    // is not asking for that: its run stops here, nothing else changes,
    // and a marker saying "disabled" over a service that is still running
    // would be the one lasting trace of a command that did nothing.
    if !enabled && result.is_err() && on_error == OnServiceError::Fail {
        let _ = std::fs::remove_file(&marker);
    }
    report_service(result, what, on_error)
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
            println!("not on Omarchy: no bar widget or menu to install");
        }
        return Ok(());
    }
    let mut notes = Vec::new();
    match install_plugin() {
        Ok(true) => notes.push("bar widget installed".to_string()),
        Ok(false) => notes.push("bar widget already installed".to_string()),
        Err(e) => notes.push(format!("bar widget: {e:#}")),
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
    let widget = uninstall_plugin()?;
    let menu = uninstall_menu()?;
    if !widget && !menu && !platform::is_omarchy() {
        println!("not on Omarchy: no bar widget or menu to remove");
    } else {
        println!("removed the Factory bar widget and menu entries");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ssf-ui is Linux desktop glue and is not shipped by the macOS package.
    #[cfg(target_os = "linux")]
    #[test]
    fn workspace_attachment_accepts_an_unknown_vm_state_but_not_a_stopped_one() {
        let root = std::env::temp_dir().join(format!(
            "ssf-ui-status-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = root.join("terminal.log");
        let ssf = bin.join("ssf");
        let terminal = bin.join("omarchy-launch-terminal");
        let browser = bin.join("omarchy-launch-browser");
        std::fs::write(
            &terminal,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$SSF_UI_TEST_LOG\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&terminal, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(&browser, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bin/ssf-ui");
        for (name, status, attaches) in [
            ("running", r#"{"enabled":true,"running":true}"#, true),
            ("unknown", r#"{"enabled":true,"running":null}"#, true),
            ("stopped", r#"{"enabled":true,"running":false}"#, false),
            ("disabled", r#"{"enabled":false,"running":true}"#, false),
        ] {
            std::fs::write(&ssf, format!("#!/bin/sh\nprintf '%s\\n' '{status}'\n")).unwrap();
            #[cfg(unix)]
            std::fs::set_permissions(&ssf, {
                use std::os::unix::fs::PermissionsExt;
                std::fs::Permissions::from_mode(0o755)
            })
            .unwrap();
            let before = std::fs::read_to_string(&log).unwrap_or_default();
            let mut paths = vec![bin.clone()];
            paths.extend(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ));
            let path = std::env::join_paths(paths).unwrap();
            let output = std::process::Command::new("bash")
                .arg(&script)
                .args(["open-workspace", "workspace", "https://example.com"])
                .env("PATH", path)
                .env("TERMINAL", &terminal)
                .env("SSF_UI_PRESENTED", "1")
                .env("SSF_UI_TEST_LOG", &log)
                .output()
                .unwrap();
            assert!(output.status.success(), "{name}: {output:?}");
            let after = std::fs::read_to_string(&log).unwrap_or_default();
            if attaches {
                assert_eq!(after, format!("{before}ssf vm attach\n"), "{name}");
            } else {
                assert_eq!(after, before, "{name} status should not attach");
            }
        }
        std::fs::remove_dir_all(root).unwrap();
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
            "\"factory.status\"",
            "\"factory.toggle\"",
            "\"factory.restart\"",
            "\"factory.logs\"",
        ] {
            assert!(block.contains(id), "menu lacks {id}: {block}");
        }
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
        assert_eq!(parsed.as_object().map(|o| o.len()), Some(5));
    }

    /// The shipped widget and its helper script only show and reach state.
    /// The setup flows (sign in, add or edit a repository) are gone; what
    /// remains reads `ssf status --json`, toggles the service, opens the log,
    /// the status-and-doctor terminal and a session's workspace.
    #[test]
    fn widget_and_helper_have_no_setup_flows() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let panel = std::fs::read_to_string(root.join("omarchy-plugin/Panel.qml")).unwrap();
        let helper = std::fs::read_to_string(root.join("bin/ssf-ui")).unwrap();
        for gone in [
            "ssf-ui login",
            "ssf-ui add-repo",
            "ssf-ui edit-repo",
            "ssf-ui manage-repos",
            "omarchy-menu-input",
            "omarchy-menu-select",
        ] {
            assert!(!panel.contains(gone), "Panel.qml still has {gone:?}");
            assert!(!helper.contains(gone), "ssf-ui still has {gone:?}");
        }
        // Naming the commands in advice text is fine; running them is not.
        for gone in [
            "\"ssf auth",
            "\"ssf repo",
            "run(\"ssf auth",
            "run(\"ssf repo",
        ] {
            assert!(!panel.contains(gone), "Panel.qml still runs {gone:?}");
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
        for kept in [
            "[\"ssf\", \"status\", \"--json\"]",
            "ssf ui service toggle",
            "ssf-ui logs",
            "ssf-ui service restart",
            "ssf-ui status",
            "ssf-ui peers",
            "ssf-ui open-workspace",
            "blocked_sessions",
            "anyone_allowed",
        ] {
            assert!(panel.contains(kept), "Panel.qml lost {kept:?}");
        }
        for cmd in ["service)", "logs)", "status)", "peers)", "open-workspace)"] {
            assert!(helper.contains(cmd), "ssf-ui lost the {cmd} command");
        }
        for gone in ["login)", "add-repo)", "edit-repo)", "manage-repos)"] {
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

    /// The widget's install and uninstall write and delete under
    /// `~/.config/omarchy`; in a test they must land in the sandbox
    /// instead, and without one they are refused (#140).
    #[test]
    fn the_omarchy_paths_hang_off_the_sandbox() {
        let sb = crate::config::test_support::sandbox();
        assert!(plugin_target_dir().starts_with(sb.home()));
        assert!(menu_extension_path().starts_with(sb.home()));
        assert_eq!(disabled_marker(), sb.state_dir().join("disabled"));
    }

    #[test]
    #[should_panic(expected = "reached the real home directory")]
    fn without_a_sandbox_the_home_directory_is_refused() {
        let _ = plugin_target_dir();
    }
}
