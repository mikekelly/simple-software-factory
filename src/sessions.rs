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
/// `exclude` names conversations that must not be picked up again: the
/// ones a handover retired, whose transcripts were written moments
/// before the new harness started in the same workspace and would
/// otherwise be the newest thing there (see `Engine::finish_handover`).
pub fn capture(harness: &str, cwd: &str, since: SystemTime, exclude: &[String]) -> Option<String> {
    match harness {
        "claude" => capture_claude(cwd, since, exclude),
        "codex" => capture_codex(cwd, since, exclude),
        _ => None,
    }
}

/// Is this conversation one the caller has retired?
fn excluded(id: &str, exclude: &[String]) -> bool {
    exclude.iter().any(|e| e == id)
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

fn capture_claude(cwd: &str, since: SystemTime, exclude: &[String]) -> Option<String> {
    newest_transcript(&claude_project_dir(cwd), since, exclude)
}

/// The newest `<session id>.jsonl` in `dir` written at or after `since`,
/// skipping the ids in `exclude`.
fn newest_transcript(dir: &Path, since: SystemTime, exclude: &[String]) -> Option<String> {
    let mut best: Option<(SystemTime, String)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if modified < since {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem.len() < 8 || excluded(stem, exclude) {
            continue;
        }
        if best.as_ref().is_none_or(|(t, _)| modified > *t) {
            best = Some((modified, stem.to_string()));
        }
    }
    best.map(|(_, id)| id)
}

fn capture_codex(cwd: &str, since: SystemTime, exclude: &[String]) -> Option<String> {
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
        if excluded(id, exclude) {
            return;
        }
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

    /// A handover starts the new harness in the workspace the old one
    /// was just stopped in, so the retired conversation's transcript is
    /// the newest file there: it must not be picked up as the new
    /// session's, or every later relaunch would resume the agent that
    /// handed the item away.
    #[test]
    fn a_retired_conversation_is_not_captured_again() {
        let dir = std::env::temp_dir().join(format!(
            "ssf-sessions-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let since = SystemTime::now() - std::time::Duration::from_secs(60);
        let older = "11111111-aaaa-4444-8888-000000000001";
        let newer = "22222222-bbbb-4444-8888-000000000002";
        std::fs::write(dir.join(format!("{older}.jsonl")), "{}\n").unwrap();
        std::fs::write(dir.join(format!("{newer}.jsonl")), "{}\n").unwrap();
        // The newest wins, as it does for a session that just started.
        let mtime = |name: &str, secs: u64| {
            let f = std::fs::File::options()
                .write(true)
                .open(dir.join(format!("{name}.jsonl")))
                .unwrap();
            f.set_modified(SystemTime::now() - std::time::Duration::from_secs(secs))
                .unwrap();
        };
        mtime(older, 30);
        mtime(newer, 10);
        assert_eq!(
            newest_transcript(&dir, since, &[]).as_deref(),
            Some(newer),
            "the newest transcript is the session's"
        );
        // The retired one is skipped, however new it is.
        assert_eq!(
            newest_transcript(&dir, since, &[newer.to_string()]).as_deref(),
            Some(older)
        );
        assert_eq!(
            newest_transcript(&dir, since, &[newer.to_string(), older.to_string()]),
            None,
            "with every conversation retired there is nothing to capture"
        );
        // Nothing written since the start is nothing to capture either.
        assert_eq!(newest_transcript(&dir, SystemTime::now(), &[]), None);
        let _ = std::fs::remove_dir_all(&dir);
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
