//! Real messages in ssf's framing, for tests elsewhere that read or match
//! what ssf delivers (echo detection, delivery confirmation).

use super::*;
use serde_json::json;

fn issue() -> Issue {
    serde_json::from_value(json!({
        "number": 5, "title": "Fix it", "body": "The daemon does not retry.\n\nIt should.",
        "html_url": "https://gh/5", "state": "open", "user": {"login": "mike"},
        "labels": [], "created_at": "2026-09-17T08:00:00Z", "updated_at": "t"
    }))
    .unwrap()
}

fn comment(daemon: &DaemonConfig, id: u64, body: &str) -> Rendered {
    let ev = json!({"event": "commented", "id": id, "actor": {"login": "mike"},
        "body": body, "html_url": format!("https://gh/5#c{id}"),
        "created_at": "2026-09-17T09:20:00Z"});
    render_event(&ev, false, daemon, "bot").unwrap()
}

fn with_ctx<T>(ssf_md: &str, f: impl FnOnce(&Issue, &DaemonConfig, &PromptContext) -> T) -> T {
    let repo = RepoConfig {
        name: "o/r".into(),
        harness: "claude".into(),
        ..Default::default()
    };
    let daemon = DaemonConfig::default();
    let triggers = vec!["assigned".to_string()];
    let ctx = PromptContext {
        repo: &repo,
        daemon: &daemon,
        bot_login: "bot",
        driver: DriverKind::Herdr,
        pr: None,
        triggers: &triggers,
        owner: None,
        delegated_by: None,
        handed_over_from: None,
        projects: &[],
        global_prompt: None,
        global_harness_prompt: None,
        project_prompt: Some(ProjectPrompt {
            source: "SSF.md".into(),
            text: ssf_md.into(),
        }),
        harness_prompt: None,
        vm_guest: false,
        pushes_as: None,
    };
    f(&issue(), &daemon, &ctx)
}

/// The first message of a session on issue #5, with `ssf_md` as the
/// repository's guidance and one comment of history.
pub fn spawn_prompt(ssf_md: &str) -> String {
    with_ctx(ssf_md, |issue, d, ctx| {
        initial_prompt(issue, &[comment(d, 1, "please fix the retry")], ctx)
    })
}

/// A later message on issue #5 carrying one new comment.
pub fn followup() -> String {
    with_ctx("x", |issue, d, ctx| {
        followup_prompt(issue, &[comment(d, 2, "any news?")], ctx)
    })
}
