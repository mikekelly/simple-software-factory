//! Desktop integration for Omarchy: the bar widget plugin, menu entries and
//! the background service toggle.

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;
use tracing::{info, warn};

pub const PLUGIN_ID: &str = "ssf.factory";
pub const SERVICE: &str = "ssf.service";
const MENU_BEGIN: &str =
    "  // ssf:begin (managed by `ssf ui install`; edits inside are overwritten)";
const MENU_END: &str = "  // ssf:end";

/// Where the package installs the plugin sources.
pub fn plugin_source_dir() -> PathBuf {
    if let Ok(d) = std::env::var("SSF_PLUGIN_DIR") {
        return PathBuf::from(d);
    }
    let candidates = [
        PathBuf::from("/usr/share/ssf/omarchy-plugin"),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("../../omarchy-plugin")))
            .unwrap_or_default(),
    ];
    candidates
        .into_iter()
        .find(|p| p.join("manifest.json").exists())
        .unwrap_or_else(|| PathBuf::from("/usr/share/ssf/omarchy-plugin"))
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"))
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

fn omarchy_available() -> bool {
    which("omarchy-plugin-enable").is_some()
}

fn which(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join(bin))
            .find(|p| p.is_file())
    })
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
/// are rewritten only when the packaged copy differs.
pub fn install_plugin() -> Result<bool> {
    let src = plugin_source_dir();
    if !src.join("manifest.json").exists() {
        bail!("plugin sources not found at {}", src.display());
    }
    let dst = plugin_target_dir();
    let mut changed = false;
    if let Ok(meta) = std::fs::symlink_metadata(&dst) {
        if meta.file_type().is_symlink() {
            std::fs::remove_file(&dst)?;
            changed = true;
        }
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
    if !omarchy_available() {
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

pub fn uninstall_plugin() -> Result<()> {
    if omarchy_available() && widget_enabled()? {
        if let Err(e) = run_quiet("omarchy-plugin-disable", &[PLUGIN_ID]) {
            warn!("could not disable bar widget: {e:#}");
        }
    }
    let dst = plugin_target_dir();
    if let Ok(meta) = std::fs::symlink_metadata(&dst) {
        if meta.file_type().is_symlink() {
            std::fs::remove_file(&dst)?;
        } else if meta.is_dir() {
            std::fs::remove_dir_all(&dst)?;
        }
    }
    Ok(())
}

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
            "factory.login",
            r#"{"icon":"","label":"Sign in bot account","action":"omarchy-launch-floating-terminal-with-presentation ssf-ui login"}"#,
        ),
        (
            "factory.add",
            r#"{"icon":"","label":"Watch a repository","action":"ssf-ui add-repo"}"#,
        ),
        (
            "factory.repos",
            r#"{"icon":"","label":"Manage repositories","action":"ssf-ui manage-repos"}"#,
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
pub fn install_menu() -> Result<bool> {
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

pub fn uninstall_menu() -> Result<()> {
    let path = menu_extension_path();
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let (Some(b), Some(e)) = (existing.find(MENU_BEGIN), existing.find(MENU_END)) else {
        return Ok(());
    };
    let mut end = e + MENU_END.len();
    if existing[end..].starts_with('\n') {
        end += 1;
    }
    let mut s = String::new();
    s.push_str(&existing[..b]);
    s.push_str(&existing[end..]);
    // Drop a comma we added if it is now dangling before the closing brace.
    let trimmed = s.trim_end();
    if let Some(stripped) = trimmed.strip_suffix('}') {
        if let Some((idx, ',')) = last_significant_char(stripped) {
            let mut chars: Vec<char> = stripped.chars().collect();
            chars.remove(idx);
            let mut rebuilt: String = chars.into_iter().collect();
            rebuilt.push_str("}\n");
            s = rebuilt;
        }
    }
    crate::config::write_atomic(&path, s.as_bytes(), 0o644)?;
    Ok(())
}

pub fn service_enabled() -> bool {
    !disabled_marker().exists()
}

pub fn set_service_enabled(enabled: bool) -> Result<()> {
    let marker = disabled_marker();
    if enabled {
        if marker.exists() {
            std::fs::remove_file(&marker)?;
        }
        let _ = Command::new("systemctl")
            .args(["--user", "start", SERVICE])
            .status();
    } else {
        if let Some(parent) = marker.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&marker, "created by `ssf ui service disable`\n")?;
        let _ = Command::new("systemctl")
            .args(["--user", "stop", SERVICE])
            .status();
    }
    Ok(())
}

pub fn service_active() -> bool {
    // Inside the VM the daemon is a system unit of the guest.
    let mut cmd = Command::new("systemctl");
    if !crate::vm::in_guest() {
        cmd.arg("--user");
    }
    cmd.args(["is-active", "--quiet", SERVICE])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn install_all(quiet: bool) -> Result<()> {
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
    uninstall_plugin()?;
    uninstall_menu()?;
    println!("removed the Factory bar widget and menu entries");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
