//! Agent conversation sessions on disk, so a killed terminal or a removed
//! workspace can be brought back with the agent's memory intact.
//!
//! Claude Code writes `~/.claude/projects/<cwd with / as ->/<session>.jsonl`
//! and resumes any of them from any directory with `--resume <id>`. Codex
//! writes `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` whose first line
//! names the cwd and session id, resumable with `codex resume <id>`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"))
}

pub fn supports_resume(harness: &str) -> bool {
    matches!(harness, "claude" | "codex")
}

/// Shell command that relaunches `harness` resuming `session_id`.
pub fn resume_command(harness: &str, base: &str, session_id: &str) -> Option<String> {
    match harness {
        "claude" => Some(format!("{base} --resume {session_id}")),
        "codex" => Some(format!("{base} resume {session_id}")),
        _ => None,
    }
}

/// Does the rendered screen say the resume did not happen?
pub fn resume_failed(screen: &[String]) -> bool {
    let text = screen.join("\n").to_lowercase();
    [
        "no conversation found",
        "session not found",
        "no session found",
        "could not find session",
        "not found: session",
    ]
    .iter()
    .any(|m| text.contains(m))
}

/// Find the session started in `cwd` after `since`, newest first.
pub fn capture(harness: &str, cwd: &str, since: SystemTime) -> Option<String> {
    match harness {
        "claude" => capture_claude(cwd, since),
        "codex" => capture_codex(cwd, since),
        _ => None,
    }
}

/// Where Claude Code keeps the transcripts of sessions started in `cwd`:
/// `~/.claude/projects/<cwd>` with every character outside `[A-Za-z0-9]`
/// turned into `-`, so `/a/b.worktrees/c_d` becomes `-a-b-worktrees-c-d`.
pub fn claude_project_dir(cwd: &str) -> PathBuf {
    let encoded: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    home().join(".claude/projects").join(encoded)
}

fn capture_claude(cwd: &str, since: SystemTime) -> Option<String> {
    let dir = claude_project_dir(cwd);
    let mut best: Option<(SystemTime, String)> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let modified = meta.modified().ok()?;
        if modified < since {
            continue;
        }
        let stem = path.file_stem()?.to_str()?.to_string();
        if stem.len() < 8 {
            continue;
        }
        if best.as_ref().is_none_or(|(t, _)| modified > *t) {
            best = Some((modified, stem));
        }
    }
    best.map(|(_, id)| id)
}

fn capture_codex(cwd: &str, since: SystemTime) -> Option<String> {
    let root = home().join(".codex/sessions");
    let mut best: Option<(SystemTime, String)> = None;
    walk(&root, 0, &mut |path: &Path| {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
            return;
        }
        let Ok(meta) = std::fs::metadata(path) else {
            return;
        };
        let Ok(modified) = meta.modified() else {
            return;
        };
        if modified < since {
            return;
        }
        let Ok(file) = std::fs::File::open(path) else {
            return;
        };
        use std::io::BufRead;
        let mut first = String::new();
        if std::io::BufReader::new(file).read_line(&mut first).is_err() {
            return;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&first) else {
            return;
        };
        let payload = v.get("payload").unwrap_or(&v);
        let session_cwd = payload.get("cwd").and_then(|c| c.as_str()).unwrap_or("");
        if session_cwd != cwd {
            return;
        }
        let Some(id) = payload.get("id").and_then(|i| i.as_str()) else {
            return;
        };
        if best.as_ref().is_none_or(|(t, _)| modified > *t) {
            best = Some((modified, id.to_string()));
        }
    });
    best.map(|(_, id)| id)
}

fn walk(dir: &Path, depth: usize, f: &mut dyn FnMut(&Path)) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, depth + 1, f);
        } else {
            f(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_dir_encoding_matches_observed_layout() {
        let d = claude_project_dir("/home/mk/orca/workspaces/x/issue-1");
        assert!(d.ends_with(".claude/projects/-home-mk-orca-workspaces-x-issue-1"));
        // Dots (herdr worktrees live in `<clone>.worktrees/`), underscores
        // and anything else non-alphanumeric become dashes too (seen on
        // disk during the #56 smoke test).
        let d = claude_project_dir(
            "/home/mk/ssf/projects/ssf-herdr-smoke.worktrees/issue-1-add-a-contributing-md",
        );
        assert!(
            d.ends_with(
                ".claude/projects/-home-mk-ssf-projects-ssf-herdr-smoke-worktrees-issue-1-add-a-contributing-md"
            ),
            "{}",
            d.display()
        );
        let d = claude_project_dir("/home/mk/code/my_project/a b");
        assert!(d.ends_with(".claude/projects/-home-mk-code-my-project-a-b"));
    }

    #[test]
    fn resume_commands() {
        assert_eq!(
            resume_command("claude", "claude", "abc").unwrap(),
            "claude --resume abc"
        );
        assert_eq!(
            resume_command("codex", "codex", "abc").unwrap(),
            "codex resume abc"
        );
        assert!(resume_command("pi", "pi", "abc").is_none());
        assert!(resume_failed(&[
            "Error: No conversation found with session ID abc".into()
        ]));
        assert!(!resume_failed(&["❯".into()]));
    }
}
