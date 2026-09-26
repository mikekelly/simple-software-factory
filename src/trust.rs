//! Pre-registering a new checkout with each harness's trust or first-run
//! state, so a session ssf starts there opens without a trust or init
//! dialog (#548). Each harness's row names how ([`Trust`]); the daemon's
//! trust-dialog answering stays the fallback.
//!
//! Every write merges into the harness's existing file, idempotently, and
//! replaces it atomically; nothing else in the file changes.

use anyhow::{Context, Result};
use std::path::Path;
use tracing::warn;

use crate::harness::HARNESSES;

/// How a harness is told a checkout is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// `~/.claude.json` → `projects["<main checkout>"].hasTrustDialogAccepted`.
    ClaudeJson,
    /// `~/.codex/config.toml` → `[projects."<main checkout>"] trust_level`.
    CodexToml,
    /// `~/.copilot/config.json` → `<main checkout>` in `trustedFolders`.
    CopilotJson,
    /// `<worktree>/.crush/init` exists.
    CrushInit,
    /// Nothing to write: a launch flag skips the question, or it asks none.
    None,
    /// Its first-run question is global, answered once at sign-in.
    Global,
}

/// Register the worktree at `worktree`, of the main checkout `repo_root`,
/// with every harness that keeps per-checkout trust and is set up on this
/// machine. Best effort: a failure is logged, and the dialog answering
/// covers it.
pub fn preregister(repo_root: &Path, worktree: &Path) {
    // Tests make worktrees too; they must not touch this machine's harnesses.
    if cfg!(test) {
        return;
    }
    let Some(home) = dirs::home_dir() else {
        return;
    };
    // Harnesses key projects by the real path, and a config kept as a
    // symlink (into dotfiles) is written through, not replaced.
    let repo_root = &repo_root.canonicalize().unwrap_or(repo_root.to_path_buf());
    let real = |p: std::path::PathBuf| p.canonicalize().unwrap_or(p);
    for h in HARNESSES {
        let result = match h.trust {
            // Signed in, it has these files; a harness never set up is left
            // alone.
            Trust::ClaudeJson if home.join(".claude.json").is_file() => {
                claude(&real(home.join(".claude.json")), repo_root)
            }
            Trust::CodexToml if home.join(".codex").is_dir() => {
                codex(&real(home.join(".codex/config.toml")), repo_root)
            }
            Trust::CopilotJson if home.join(".copilot").is_dir() => {
                copilot(&real(home.join(".copilot/config.json")), repo_root)
            }
            Trust::CrushInit if crate::agents::installed(h.id) => crush(repo_root, worktree),
            _ => Ok(()),
        };
        if let Err(e) = result {
            warn!(harness = h.id, error = %format!("{e:#}"), "pre-registering trust");
        }
    }
}

fn key(repo_root: &Path) -> String {
    repo_root.to_string_lossy().to_string()
}

/// The file's permissions, so a replacement keeps them (`~/.claude.json`
/// is private).
fn mode_of(path: &Path, default: u32) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).map_or(default, |m| m.permissions().mode() & 0o7777)
}

fn read_json(path: &Path) -> Result<serde_json::Value> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    let mut data = serde_json::to_vec_pretty(value)?;
    data.push(b'\n');
    crate::config::write_atomic(path, &data, mode_of(path, 0o600))
}

/// Mark the checkout's trust dialog accepted in Claude Code's state file.
pub fn claude(path: &Path, repo_root: &Path) -> Result<()> {
    let mut state = read_json(path)?;
    let root = state
        .as_object_mut()
        .context("~/.claude.json is not an object")?;
    let projects = root
        .entry("projects")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("projects is not an object")?;
    let project = projects
        .entry(key(repo_root))
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("the project entry is not an object")?;
    if project.get("hasTrustDialogAccepted") == Some(&serde_json::Value::Bool(true)) {
        return Ok(());
    }
    project.insert("hasTrustDialogAccepted".into(), true.into());
    write_json(path, &state)
}

/// Add the checkout to Copilot's trusted folders. Its `config.json` is
/// JSONC and opens with comment lines, which are kept; a missing or empty
/// file is created.
pub fn copilot(path: &Path, repo_root: &Path) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let body = strip_json_comments(&text);
    let mut config: serde_json::Value = if body.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&body).with_context(|| format!("parsing {}", path.display()))?
    };
    let root = config
        .as_object_mut()
        .context("config.json is not an object")?;
    let folders = root
        .entry("trustedFolders")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .context("trustedFolders is not a list")?;
    let root_key = key(repo_root);
    if folders.iter().any(|f| f.as_str() == Some(&root_key)) {
        return Ok(());
    }
    folders.push(root_key.into());
    // The comment lines before the object, as Copilot writes them.
    let header: String = text
        .lines()
        .take_while(|l| !l.trim_start().starts_with('{'))
        .filter(|l| l.trim_start().starts_with("//"))
        .map(|l| format!("{l}\n"))
        .collect();
    let mut data = header.into_bytes();
    data.extend(serde_json::to_vec_pretty(&config)?);
    data.push(b'\n');
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::config::write_atomic(path, &data, mode_of(path, 0o600))
}

/// JSONC to JSON: `//` and `/* */` comments outside strings removed.
fn strip_json_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

/// Trust the checkout in Codex's config. The file is TOML people edit, so
/// a missing entry is appended as text rather than the file re-serialized,
/// which would drop its comments and layout; a trust level already set,
/// whatever it is, is left as it is.
pub fn codex(path: &Path, repo_root: &Path) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let config: toml::Table =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let root_key = key(repo_root);
    let project = config
        .get("projects")
        .and_then(|p| p.get(&root_key))
        .and_then(|p| p.as_table());
    let quoted = toml::Value::String(root_key.clone()).to_string();
    let header = format!("[projects.{quoted}]");
    let new = match project {
        Some(p) if p.contains_key("trust_level") => return Ok(()),
        // The table exists without a level: add the level under its header.
        Some(_) => {
            let Some(at) = text.lines().position(|l| l.trim() == header) else {
                anyhow::bail!("{header} is not written as a plain table header");
            };
            let mut lines: Vec<&str> = text.lines().collect();
            lines.insert(at + 1, "trust_level = \"trusted\"");
            lines.join("\n") + "\n"
        }
        None => {
            let mut out = text.clone();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("{header}\ntrust_level = \"trusted\"\n"));
            out
        }
    };
    // Never write a file Codex could not read.
    toml::from_str::<toml::Table>(&new).context("the merged config does not parse")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::config::write_atomic(path, new.as_bytes(), mode_of(path, 0o600))
}

/// Mark the worktree initialized for Crush. The marker is excluded in the
/// main checkout's `info/exclude`, which its worktrees share, so it does not
/// leave the worktree dirty for `ssf release`.
pub fn crush(repo_root: &Path, worktree: &Path) -> Result<()> {
    let info = repo_root.join(".git/info");
    let exclude = info.join("exclude");
    let text = std::fs::read_to_string(&exclude).unwrap_or_default();
    if !text.lines().any(|l| l.trim() == "/.crush/") {
        std::fs::create_dir_all(&info).with_context(|| format!("creating {}", info.display()))?;
        let sep = if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        let new = format!("{text}{sep}/.crush/\n");
        crate::config::write_atomic(&exclude, new.as_bytes(), mode_of(&exclude, 0o644))?;
    }
    let dir = worktree.join(".crush");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let init = dir.join("init");
    if !init.exists() {
        std::fs::write(&init, b"").with_context(|| format!("creating {}", init.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Tmp(std::path::PathBuf);
    impl Tmp {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[track_caller]
    fn tmp() -> Tmp {
        let line = std::panic::Location::caller().line();
        let dir = std::env::temp_dir().join(format!("ssf-trust-{line}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Tmp(dir)
    }

    #[test]
    fn every_harness_says_how_it_is_trusted() {
        // Adding a harness without a decision fails to compile (the row
        // needs a `trust`); this pins the decisions the audit on #546 made.
        let got: Vec<_> = HARNESSES.iter().map(|h| (h.id, h.trust)).collect();
        assert_eq!(
            got,
            [
                ("claude", Trust::ClaudeJson),
                ("codex", Trust::CodexToml),
                ("omp", Trust::Global),
                ("pi", Trust::Global),
                ("opencode", Trust::None),
                ("gemini", Trust::None),
                ("copilot", Trust::CopilotJson),
                ("grok", Trust::None),
                ("crush", Trust::CrushInit),
            ]
        );
    }

    #[test]
    fn claude_merges_and_is_idempotent() {
        let d = tmp();
        let path = d.path().join(".claude.json");
        std::fs::write(
            &path,
            r#"{"numStartups":3,"projects":{"/other":{"hasTrustDialogAccepted":true},"/r":{"allowedTools":["x"]}}}"#,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        claude(&path, Path::new("/r")).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(v["numStartups"], 3);
        assert_eq!(v["projects"]["/other"]["hasTrustDialogAccepted"], true);
        assert_eq!(v["projects"]["/r"]["allowedTools"][0], "x");
        assert_eq!(v["projects"]["/r"]["hasTrustDialogAccepted"], true);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        claude(&path, Path::new("/r")).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
    }

    #[test]
    fn copilot_merges_and_is_idempotent() {
        let d = tmp();
        let path = d.path().join("config.json");
        std::fs::write(&path, r#"{"theme":"dark","trustedFolders":["/a"]}"#).unwrap();
        copilot(&path, Path::new("/r")).unwrap();
        copilot(&path, Path::new("/r")).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["trustedFolders"], serde_json::json!(["/a", "/r"]));
    }

    #[test]
    fn copilot_reads_its_real_jsonc_file_and_keeps_the_comments() {
        let d = tmp();
        let path = d.path().join("config.json");
        let real = "// User settings belong in settings.json.\n// This file is managed automatically.\n{\n  \"firstLaunchAt\": \"2026-09-26T10:00:00Z\",\n  \"appTipShown\": true\n}\n";
        std::fs::write(&path, real).unwrap();
        copilot(&path, Path::new("/r")).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.starts_with(
            "// User settings belong in settings.json.\n// This file is managed automatically.\n{"
        ));
        let v: serde_json::Value = serde_json::from_str(&strip_json_comments(&first)).unwrap();
        assert_eq!(v["appTipShown"], true);
        assert_eq!(v["trustedFolders"], serde_json::json!(["/r"]));
        copilot(&path, Path::new("/r")).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
    }

    #[test]
    fn copilot_creates_a_missing_or_empty_file() {
        let d = tmp();
        let missing = d.path().join("a/config.json");
        copilot(&missing, Path::new("/r")).unwrap();
        let empty = d.path().join("config.json");
        std::fs::write(&empty, "").unwrap();
        copilot(&empty, Path::new("/r")).unwrap();
        for p in [missing, empty] {
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
            assert_eq!(v, serde_json::json!({"trustedFolders": ["/r"]}));
        }
    }

    #[test]
    fn json_comments_are_stripped_outside_strings_only() {
        let v: serde_json::Value = serde_json::from_str(&strip_json_comments(
            "/* a */ {\"u\": \"http://x/*y*/\", // c\n \"q\": \"\\\"//\"}",
        ))
        .unwrap();
        assert_eq!(v["u"], "http://x/*y*/");
        assert_eq!(v["q"], "\"//");
    }

    #[test]
    fn codex_appends_keeps_comments_and_is_idempotent() {
        let d = tmp();
        let path = d.path().join("config.toml");
        let before = "# mine\nmodel = \"gpt\"\n\n[projects.\"/a\"]\ntrust_level = \"untrusted\"";
        std::fs::write(&path, before).unwrap();
        codex(&path, Path::new("/r")).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.starts_with(before), "{first}");
        let t: toml::Table = toml::from_str(&first).unwrap();
        assert_eq!(t["projects"]["/r"]["trust_level"].as_str(), Some("trusted"));
        assert_eq!(
            t["projects"]["/a"]["trust_level"].as_str(),
            Some("untrusted")
        );
        codex(&path, Path::new("/r")).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        // A level already chosen is not overridden.
        codex(&path, Path::new("/a")).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
    }

    #[test]
    fn codex_fills_a_table_without_a_level_and_creates_a_missing_file() {
        let d = tmp();
        let path = d.path().join("config.toml");
        std::fs::write(&path, "[projects.\"/r\"]\nnote = 1\n").unwrap();
        codex(&path, Path::new("/r")).unwrap();
        let t: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(t["projects"]["/r"]["trust_level"].as_str(), Some("trusted"));
        assert_eq!(t["projects"]["/r"]["note"].as_integer(), Some(1));

        let fresh = d.path().join("new/config.toml");
        codex(&fresh, Path::new("/r")).unwrap();
        assert_eq!(
            std::fs::read_to_string(&fresh).unwrap(),
            "[projects.\"/r\"]\ntrust_level = \"trusted\"\n"
        );
    }

    #[test]
    fn crush_creates_the_marker_once() {
        let d = tmp();
        let (root, wt) = (d.path().join("r"), d.path().join("wt"));
        std::fs::create_dir_all(root.join(".git/info")).unwrap();
        std::fs::write(root.join(".git/info/exclude"), "# git's own").unwrap();
        crush(&root, &wt).unwrap();
        std::fs::write(wt.join(".crush/init"), "kept").unwrap();
        crush(&root, &wt).unwrap();
        assert_eq!(
            std::fs::read_to_string(wt.join(".crush/init")).unwrap(),
            "kept"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".git/info/exclude")).unwrap(),
            "# git's own\n/.crush/\n"
        );
    }
}
