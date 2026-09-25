//! Agent conversation sessions on disk, so a killed terminal or a removed
//! workspace can be brought back with the agent's memory intact.
//!
//! Claude Code writes `~/.claude/projects/<cwd with / as ->/<session>.jsonl`
//! and resumes any of them from any directory with `--resume <id>`. Codex
//! writes `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` whose first line
//! names the cwd and session id, resumable with `codex resume <id>`. Grok
//! writes `~/.grok/sessions/<URL-encoded cwd>/<session>/updates.jsonl`
//! (`$GROK_HOME` for `~/.grok`), resumable with `grok --resume <id>`.

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
        "grok" => grok_root(),
        _ => return None,
    };
    transcript_modified(&root, harness, cwd, id).map(|time| {
        chrono::DateTime::<chrono::Utc>::from(time)
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    })
}

/// The newest `<root>/sessions/**/rollout-*-<id>.jsonl`; `id` must already
/// be checked to be a plain file-name fragment.
fn codex_rollout(root: &Path, id: &str) -> Option<PathBuf> {
    let suffix = format!("-{id}.jsonl");
    let mut latest: Option<(SystemTime, PathBuf)> = None;
    walk(&root.join("sessions"), 0, &mut |path| {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with("rollout-")
            && name.ends_with(&suffix)
            && let Ok(time) = std::fs::metadata(path).and_then(|m| m.modified())
            && latest.as_ref().is_none_or(|(old, _)| time > *old)
        {
            latest = Some((time, path.to_path_buf()));
        }
    });
    latest.map(|(_, path)| path)
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
        "codex" => std::fs::metadata(codex_rollout(root, id)?)
            .ok()?
            .modified()
            .ok(),
        "grok" => grok_modified(&grok_group(root, cwd)?.join(id)),
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
        "grok" => Some(format!("{base} --resume {session_id}")),
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
        // grok, after trying the id locally and then remotely.
        "failed to restore session",
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
        "grok" => capture_grok(&grok_root(), cwd, since, exclude),
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
    let tail = tail_of(path)?;
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

/// The last 4 MiB of the transcript at `path`: a transcript grows with the
/// conversation, and only its latest turn matters.
fn tail_of(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 4 << 20;
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    Some(String::from_utf8_lossy(&tail).into_owned())
}

/// How full a Codex session's context is, as its byline shows it
/// (`12% of 258k`): Codex gives the commands it runs its session as
/// `CODEX_THREAD_ID` (and `CODEX_SESSION_ID`), which names its rollout, and
/// the usage is the rollout's latest `token_count` event. `None` whenever
/// any of that is missing.
pub fn codex_context() -> Option<String> {
    let id = std::env::var("CODEX_THREAD_ID")
        .or_else(|_| std::env::var("CODEX_SESSION_ID"))
        .ok()?;
    if id.is_empty() || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
        return None;
    }
    let root = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".codex"));
    codex_context_of(&codex_rollout(&root, &id)?)
}

/// The input of the last turn against the model's window, from the latest
/// `token_count` event in the rollout at `path`. Codex counts cached input
/// inside `input_tokens`.
fn codex_context_of(path: &Path) -> Option<String> {
    let tail = tail_of(path)?;
    tail.lines().rev().find_map(|line| {
        if !line.contains("\"token_count\"") {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let payload = v.get("payload")?;
        if payload.get("type")?.as_str()? != "token_count" {
            return None;
        }
        let info = payload.get("info")?;
        let used = info
            .get("last_token_usage")?
            .get("input_tokens")?
            .as_u64()?;
        let window = info.get("model_context_window")?.as_u64()?;
        if used == 0 || window == 0 {
            return None;
        }
        Some(format!(
            "{}% of {}",
            (used * 100 / window).min(100),
            crate::models::token_size(window)
        ))
    })
}

/// How full an Oh My Pi session's context is, as its byline shows it. An
/// ssf-launched OMP session keeps its transcript in the delivery mailbox's
/// `session/` directory (`harness/ssf-pi-launch`, the OMP, Pi and OpenCode launcher), and OMP's commands inherit
/// `SSF_DELIVERY_MAILBOX`; the usage is that transcript's latest assistant
/// turn, and the window is what omp lists for the turn's model
/// (`models::omp_context_window`). `None` whenever any of that is missing.
pub fn omp_context() -> Option<String> {
    mailbox_context(crate::models::omp_context_window)
}

/// How full a Pi session's context is: as for [`omp_context`], Pi writing the
/// same transcript entries, with the window `pi --list-models` gives
/// (`models::pi_context_window`).
pub fn pi_context() -> Option<String> {
    mailbox_context(crate::models::pi_context_window)
}

/// The latest assistant turn of the transcript in the delivery mailbox's
/// `session/` directory against the window `window` gives for its model.
fn mailbox_context(window: fn(&str, &str) -> Option<u64>) -> Option<String> {
    let mailbox = std::env::var_os("SSF_DELIVERY_MAILBOX")?;
    let session = Path::new(&mailbox).join("session");
    let (provider, model, used) = omp_turn(&newest_jsonl(&session)?)?;
    let window = window(&provider, &model)?;
    Some(format!(
        "{}% of {}",
        (used * 100 / window).min(100),
        crate::models::token_size(window)
    ))
}

/// The most recently written `*.jsonl` in `dir`: a fork or branch starts a
/// new transcript beside the old one, and the running session writes its own.
fn newest_jsonl(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max()
        .map(|(_, path)| path)
}

/// The provider, model and context tokens (input plus cache reads and
/// writes) of the last assistant turn in the OMP or Pi transcript at `path`.
fn omp_turn(path: &Path) -> Option<(String, String, u64)> {
    let tail = tail_of(path)?;
    tail.lines().rev().find_map(|line| {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        if v.get("type")?.as_str()? != "message" {
            return None;
        }
        let message = v.get("message")?;
        if message.get("role")?.as_str()? != "assistant" {
            return None;
        }
        let usage = message.get("usage")?;
        let used: u64 = ["input", "cacheRead", "cacheWrite"]
            .iter()
            .filter_map(|k| usage.get(*k).and_then(serde_json::Value::as_u64))
            .sum();
        if used == 0 {
            return None;
        }
        Some((
            message.get("provider")?.as_str()?.to_string(),
            message.get("model")?.as_str()?.to_string(),
            used,
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

/// Grok's home: `$GROK_HOME`, else `~/.grok`.
fn grok_root() -> PathBuf {
    std::env::var_os("GROK_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".grok"))
}

/// The directory under `<root>/sessions` that groups the sessions started in
/// `cwd`. Grok names it by URL-encoding the cwd, or, when that would be too
/// long, by a slug with the path in a `.cwd` file inside; decoding the name
/// rather than encoding the cwd leaves the exact escaped set Grok's to choose.
fn grok_group(root: &Path, cwd: &str) -> Option<PathBuf> {
    std::fs::read_dir(root.join("sessions"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|dir| {
            dir.is_dir()
                && (dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(percent_decode)
                    .is_some_and(|name| name == cwd)
                    || std::fs::read_to_string(dir.join(".cwd"))
                        .is_ok_and(|path| path.trim_end_matches('\n') == cwd))
        })
}

/// `%2Fa%2Fb` as `/a/b`; `None` for a name that is not valid escaping.
fn percent_decode(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// When a Grok session last changed: its `updates.jsonl`, the conversation
/// log, or the session directory before that is written.
fn grok_modified(session: &Path) -> Option<SystemTime> {
    std::fs::metadata(session.join("updates.jsonl"))
        .or_else(|_| std::fs::metadata(session))
        .ok()?
        .modified()
        .ok()
}

/// The ids of the subagent sessions the sessions in `group` spawned.
///
/// Grok keeps a subagent's child session in the normal sessions tree, beside
/// its parent, and records it in the parent's `subagents/<id>/meta.json` as
/// `child_session_id` (seen with grok 1.0.41, next to a `parent_session_id`
/// naming the parent, which must not count). The child's own `summary.json`
/// also says `"session_kind": "subagent"`; that is checked per session, and
/// this covers a child whose summary is not written yet.
fn grok_children(group: &Path) -> std::collections::HashSet<String> {
    let mut children = std::collections::HashSet::new();
    let Ok(sessions) = std::fs::read_dir(group) else {
        return children;
    };
    for session in sessions.flatten() {
        let Ok(subagents) = std::fs::read_dir(session.path().join("subagents")) else {
            continue;
        };
        for sub in subagents.flatten() {
            if let Some(child) = std::fs::read_to_string(sub.path().join("meta.json"))
                .ok()
                .and_then(|m| serde_json::from_str::<serde_json::Value>(&m).ok())
                .and_then(|m| m["child_session_id"].as_str().map(str::to_string))
            {
                children.insert(child);
            }
        }
    }
    children
}

/// Whether the session at `path` is a subagent's, by its own `summary.json`.
fn grok_subagent(path: &Path) -> bool {
    std::fs::read_to_string(path.join("summary.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .is_some_and(|s| s["session_kind"] == "subagent")
}

/// The newest top-level Grok session started in `cwd` and changed at or
/// after `since`; a subagent's child session is never the conversation.
fn capture_grok(root: &Path, cwd: &str, since: SystemTime, exclude: &[String]) -> Option<String> {
    let group = grok_group(root, cwd)?;
    let children = grok_children(&group);
    let mut best: Option<(SystemTime, String)> = None;
    for entry in std::fs::read_dir(&group).ok()?.flatten() {
        let path = entry.path();
        let Some(id) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !path.is_dir()
            || id.len() < 8
            || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            || excluded(id, exclude)
            || children.contains(id)
            || grok_subagent(&path)
        {
            continue;
        }
        let Some(modified) = grok_modified(&path).filter(|m| *m >= since) else {
            continue;
        };
        if best.as_ref().is_none_or(|(t, _)| modified > *t) {
            best = Some((modified, id.to_string()));
        }
    }
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
    fn codex_context_is_the_last_token_count() {
        let sandbox = crate::config::test_support::sandbox();
        let day = sandbox.root().join("sessions/2026/09/24");
        std::fs::create_dir_all(&day).unwrap();
        let count = |input: u64, window: u64| {
            serde_json::json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":900_000},"last_token_usage":{"input_tokens":input,"cached_input_tokens":input / 2,"output_tokens":80},"model_context_window":window}}}).to_string()
        };
        let lines = [
            r#"{"type":"session_meta","payload":{"id":"t-1","cwd":"/a"}}"#.to_string(),
            count(1_000, 258_400),
            count(31_008, 258_400),
            r#"{"type":"event_msg","payload":{"type":"token_count","info":null}}"#.to_string(),
            r#"{"type":"response_item","payload":{"type":"message"}}"#.to_string(),
        ];
        std::fs::write(
            day.join("rollout-2026-09-24T12-00-00-t-1.jsonl"),
            lines.join("\n"),
        )
        .unwrap();
        let path = codex_rollout(sandbox.root(), "t-1").unwrap();
        assert_eq!(codex_context_of(&path).as_deref(), Some("12% of 258k"));
        assert!(codex_rollout(sandbox.root(), "t-2").is_none());
        std::fs::write(day.join("rollout-x-t-2.jsonl"), count(50_000, 200_000)).unwrap();
        let path = codex_rollout(sandbox.root(), "t-2").unwrap();
        assert_eq!(codex_context_of(&path).as_deref(), Some("25% of 200k"));
        std::fs::write(&path, lines[0].as_str()).unwrap();
        assert_eq!(codex_context_of(&path), None);
    }

    #[test]
    fn omp_turn_is_the_last_assistant_message() {
        let sandbox = crate::config::test_support::sandbox();
        let turn = |role: &str, read: u64| {
            serde_json::json!({"type":"message","message":{"role":role,"provider":"deepseek","model":"deepseek-flash","usage":{"input":170,"output":181,"cacheRead":read,"cacheWrite":30,"totalTokens":1}}}).to_string()
        };
        let lines = [
            r#"{"type":"session","version":3}"#.to_string(),
            turn("assistant", 1_000),
            turn("assistant", 119_800),
            turn("toolResult", 900_000),
            r#"{"type":"custom","customType":"x"}"#.to_string(),
        ];
        let old = sandbox.root().join("a.jsonl");
        std::fs::write(&old, turn("assistant", 5)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let path = sandbox.root().join("b.jsonl");
        std::fs::write(&path, lines.join("\n")).unwrap();
        std::fs::write(sandbox.root().join("ready.json"), "{}").unwrap();
        assert_eq!(newest_jsonl(sandbox.root()), Some(path.clone()));
        assert_eq!(
            omp_turn(&path),
            Some(("deepseek".into(), "deepseek-flash".into(), 120_000))
        );
        assert!(newest_jsonl(&sandbox.root().join("missing")).is_none());
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
    fn grok_sessions_are_found_by_their_working_directory() {
        let sandbox = crate::config::test_support::sandbox();
        let root = sandbox.root();
        let cwd = "/work/my-repo.worktrees/issue_1";
        let group = root.join("sessions/%2Fwork%2Fmy-repo.worktrees%2Fissue_1");
        let other = root.join("sessions/%2Fwork%2Felsewhere");
        let long = root.join("sessions/work-long-abc123");
        let older = "019a0000-0000-7000-8000-000000000001";
        let newer = "019a0000-0000-7000-8000-000000000002";
        for dir in [
            group.join(older),
            group.join(newer),
            other.join("019a0000-0000-7000-8000-000000000003"),
        ] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("updates.jsonl"), "{}\n").unwrap();
        }
        let aged = |dir: &Path, secs: u64| {
            std::fs::File::options()
                .write(true)
                .open(dir.join("updates.jsonl"))
                .unwrap()
                .set_modified(SystemTime::now() - std::time::Duration::from_secs(secs))
                .unwrap();
        };
        aged(&group.join(older), 30);
        aged(&group.join(newer), 10);
        let since = SystemTime::now() - std::time::Duration::from_secs(60);
        assert_eq!(capture_grok(root, cwd, since, &[]).as_deref(), Some(newer));
        assert_eq!(
            capture_grok(root, cwd, since, &[newer.to_string()]).as_deref(),
            Some(older)
        );
        assert_eq!(capture_grok(root, cwd, SystemTime::now(), &[]), None);
        assert_eq!(capture_grok(root, "/work/none", since, &[]), None);
        assert!(transcript_modified(root, "grok", cwd, newer).is_some());
        assert!(transcript_modified(root, "grok", cwd, "../x").is_none());
        // A subagent's child session, newer than its parent, is not captured:
        // named by the parent's `subagents/<id>/meta.json` (which also names
        // the parent), or marked in its own `summary.json`.
        let child = "019a0000-0000-7000-8000-00000000000c";
        let marked = "019a0000-0000-7000-8000-00000000000d";
        for id in [child, marked] {
            std::fs::create_dir_all(group.join(id)).unwrap();
            std::fs::write(group.join(id).join("updates.jsonl"), "{}\n").unwrap();
        }
        std::fs::write(
            group.join(marked).join("summary.json"),
            r#"{"session_kind":"subagent"}"#,
        )
        .unwrap();
        let meta = group.join(newer).join("subagents").join(child);
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(
            meta.join("meta.json"),
            format!(r#"{{"subagent_id":"{child}","parent_session_id":"{newer}","child_session_id":"{child}"}}"#),
        )
        .unwrap();
        assert_eq!(capture_grok(root, cwd, since, &[]).as_deref(), Some(newer));
        // A path too long to encode is named by a `.cwd` file instead.
        std::fs::create_dir_all(long.join(older)).unwrap();
        std::fs::write(long.join(".cwd"), "/very/long/path\n").unwrap();
        assert_eq!(
            capture_grok(root, "/very/long/path", SystemTime::UNIX_EPOCH, &[]).as_deref(),
            Some(older)
        );
        assert_eq!(percent_decode("%2Fa%2fb-c"), Some("/a/b-c".into()));
        assert_eq!(percent_decode("bad%2"), None);
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
        assert_eq!(
            resume_command("grok", "grok --always-approve", "abc").unwrap(),
            "grok --always-approve --resume abc"
        );
        assert!(resume_command("pi", "pi", "abc").is_none());
        assert!(resume_failed(&[
            "Error: No conversation found with session ID abc".into()
        ]));
        assert!(resume_failed(&[
            "Error: Failed to restore session from remote: fetching session record: session get failed: 404 Not Found".into()
        ]));
        // Grok says this on its way to trying the id remotely, which may work.
        assert!(!resume_failed(&[
            "Session \"x\" not found locally, restoring conversation from remote...".into()
        ]));
        assert!(!resume_failed(&["❯".into()]));
    }
}
