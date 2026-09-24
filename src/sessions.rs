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

/// Whether ssf has a reader for `harness`'s local transcript, which is what
/// both dates a conversation (`last_activity`) and resumes one
/// (`resume_command`, `capture`). The rest report no activity time ever, which
/// is a fact about the harness rather than about the agent, so a dashboard says
/// which it is instead of leaving a gap (#439); and they start afresh rather
/// than resume.
pub fn reads_transcript(harness: &str) -> bool {
    crate::harness::harness(harness).is_some_and(|h| h.reads_transcript)
}

/// Last write to the live conversation's transcript. Herdr exposes a session
/// reference, but no wall-clock activity time. Missing or unsupported transcripts
/// stay unknown; a prompt delivery time is not a substitute for agent activity.
pub fn last_activity(harness: &str, cwd: &str, id: &str) -> Option<String> {
    let root = match harness {
        "claude" => std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".claude")),
        "codex" => std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".codex")),
        _ => return None,
    };
    transcript_modified(&root, harness, cwd, id).map(|time| {
        chrono::DateTime::<chrono::Utc>::from(time)
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    })
}

fn transcript_modified(root: &Path, harness: &str, cwd: &str, id: &str) -> Option<SystemTime> {
    // The reference comes from another process and must remain a file name.
    if id.is_empty() || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
        return None;
    }
    match harness {
        "claude" => {
            let encoded: String = cwd
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            std::fs::metadata(
                root.join("projects")
                    .join(encoded)
                    .join(format!("{id}.jsonl")),
            )
            .ok()?
            .modified()
            .ok()
        }
        "codex" => {
            let suffix = format!("-{id}.jsonl");
            let mut latest = None;
            walk(&root.join("sessions"), 0, &mut |path| {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with("rollout-")
                    && name.ends_with(&suffix)
                    && let Ok(time) = std::fs::metadata(path).and_then(|m| m.modified())
                {
                    latest = Some(latest.map_or(time, |old: SystemTime| old.max(time)));
                }
            });
            latest
        }
        _ => None,
    }
}

/// Shell command that relaunches `harness` resuming `session_id`.
pub fn resume_command(harness: &str, base: &str, session_id: &str) -> Option<String> {
    match harness {
        "claude" => Some(format!("{base} --resume {session_id}")),
        "codex" => {
            // Remote tasks retain their server-side permission settings. Codex
            // rejects a permission override on remote resume, unlike local resume.
            let remote = base.starts_with("codex ")
                && base
                    .split_whitespace()
                    .any(|a| a == "--remote" || a.starts_with("--remote="));
            let command = if remote {
                base.split_inclusive(char::is_whitespace)
                    .filter(|a| a.trim() != "--dangerously-bypass-approvals-and-sandbox")
                    .collect::<String>()
            } else {
                base.to_owned()
            };
            Some(format!("{command} resume {session_id}"))
        }
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

/// How full a Claude Code session's context is, as its byline shows it
/// (`12% of 1M`): the session is `CLAUDE_CODE_SESSION_ID` in the
/// environment, which Claude Code gives the commands it runs, and the usage
/// is its transcript's latest main-thread turn. `None` whenever any of that
/// is missing, or the model's window is not known.
pub fn claude_context() -> Option<String> {
    let id = std::env::var("CLAUDE_CODE_SESSION_ID").ok()?;
    context_of(&claude_transcript(&home().join(".claude/projects"), &id)?)
}

/// `<root>/<any project>/<id>.jsonl`: the session's cwd can differ from the
/// command's, so every project directory is tried.
fn claude_transcript(root: &Path, id: &str) -> Option<PathBuf> {
    if id.is_empty() || id.contains(['/', '.']) {
        return None;
    }
    std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path().join(format!("{id}.jsonl")))
        .find(|p| p.is_file())
}

/// The context usage of the last main-thread assistant turn in the
/// transcript at `path`; only its tail is read, since a transcript grows
/// with the conversation.
fn context_of(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 4 << 20;
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail);
    tail.lines().rev().find_map(|line| {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        if v.get("type")?.as_str()? != "assistant" || v["isSidechain"].as_bool() == Some(true) {
            return None;
        }
        let message = v.get("message")?;
        let usage = message.get("usage")?;
        let used: u64 = [
            "input_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ]
        .iter()
        .filter_map(|k| usage.get(*k).and_then(serde_json::Value::as_u64))
        .sum();
        if used == 0 {
            return None;
        }
        let window = crate::models::claude_context_window(message.get("model")?.as_str()?)?;
        Some(format!(
            "{}% of {}",
            (used * 100 / window).min(100),
            crate::models::token_size(window)
        ))
    })
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
    fn activity_uses_only_the_named_transcript() {
        let sandbox = crate::config::test_support::sandbox();
        let root = sandbox.root();
        let claude = root.join("projects/-work-tree/session-1.jsonl");
        std::fs::create_dir_all(claude.parent().unwrap()).unwrap();
        std::fs::write(&claude, "{}\n").unwrap();
        let expected = std::fs::metadata(&claude).unwrap().modified().unwrap();
        assert_eq!(
            transcript_modified(root, "claude", "/work/tree", "session-1"),
            Some(expected)
        );
        assert_eq!(
            transcript_modified(root, "claude", "/other", "session-1"),
            None
        );
        assert_eq!(
            transcript_modified(root, "claude", "/work/tree", "missing"),
            None
        );
        assert_eq!(
            transcript_modified(root, "claude", "/work/tree", "../session-1"),
            None
        );

        let codex = root.join("sessions/2026/09/12/rollout-2026-09-12T15-00-00-session-2.jsonl");
        std::fs::create_dir_all(codex.parent().unwrap()).unwrap();
        std::fs::write(&codex, "{}\n").unwrap();
        let expected = std::fs::metadata(&codex).unwrap().modified().unwrap();
        assert_eq!(
            transcript_modified(root, "codex", "/work/tree", "session-2"),
            Some(expected)
        );
        assert_eq!(
            transcript_modified(root, "codex", "/work/tree", "session-1"),
            None
        );
        assert_eq!(
            transcript_modified(root, "pi", "/work/tree", "session-2"),
            None
        );
    }

    #[test]
    fn claude_context_is_the_last_main_thread_turn() {
        let sandbox = crate::config::test_support::sandbox();
        let project = sandbox.root().join("-a-b");
        std::fs::create_dir(&project).unwrap();
        let turn = |sidechain: bool, model: &str, read: u64| {
            serde_json::json!({"type":"assistant","isSidechain":sidechain,"message":{"model":model,"usage":{"input_tokens":2,"cache_creation_input_tokens":98,"cache_read_input_tokens":read}}}).to_string()
        };
        let lines = [
            turn(false, "claude-opus-5-5", 1_000),
            turn(false, "claude-opus-5-5", 119_900),
            turn(true, "claude-opus-5-5", 900_000),
            r#"{"type":"user"}"#.to_string(),
        ];
        std::fs::write(project.join("abc-123.jsonl"), lines.join("\n")).unwrap();
        let path = claude_transcript(sandbox.root(), "abc-123").unwrap();
        assert_eq!(context_of(&path).as_deref(), Some("12% of 1M"));
        assert!(claude_transcript(sandbox.root(), "missing").is_none());
        assert!(claude_transcript(sandbox.root(), "../x").is_none());
        std::fs::write(
            project.join("h.jsonl"),
            turn(false, "claude-haiku-4-5-20251001", 49_900),
        )
        .unwrap();
        assert_eq!(
            context_of(&project.join("h.jsonl")).as_deref(),
            Some("25% of 200k")
        );
        std::fs::write(project.join("u.jsonl"), turn(false, "mystery", 10)).unwrap();
        assert_eq!(context_of(&project.join("u.jsonl")), None);
    }

    #[test]
    fn claude_dir_encoding_matches_observed_layout() {
        let d = claude_project_dir("/home/mk/ssf/workspaces/x/issue-1");
        assert!(d.ends_with(".claude/projects/-home-mk-ssf-workspaces-x-issue-1"));
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
        assert_eq!(resume_command("codex", "codex --remote unix:///tmp/app.sock --dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust", "abc").unwrap(), "codex --remote unix:///tmp/app.sock --dangerously-bypass-hook-trust resume abc");
        assert_eq!(
            resume_command(
                "codex",
                "codex --dangerously-bypass-approvals-and-sandbox",
                "abc"
            )
            .unwrap(),
            "codex --dangerously-bypass-approvals-and-sandbox resume abc"
        );
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
