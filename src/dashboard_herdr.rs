//! Optional navigation uses the Herdr server inherited by this terminal.
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{process::Stdio, time::Duration};

pub(super) fn available() -> bool {
    std::env::var("HERDR_ENV").as_deref() == Ok("1")
}

fn matched_pane<'a>(response: &'a Value, session: &str, harness: &str) -> Result<&'a str> {
    if session.is_empty() {
        bail!("This SSF agent has no captured session ID yet");
    }
    let agents = response["result"]["agents"]
        .as_array()
        .context("Herdr returned an unsupported agent list")?;
    let matches: Vec<_> = agents
        .iter()
        .filter(|agent| {
            agent["agent_session"]["kind"] == "id"
                && agent["agent_session"]["value"] == session
                && (harness.is_empty() || agent["agent"] == harness)
        })
        .collect();
    match matches.as_slice() {
        [] => bail!("No matching agent on this Herdr server (the pane may have closed)"),
        [agent] => agent["pane_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .context("Matched Herdr agent has no pane ID"),
        _ => bail!("More than one Herdr pane has this session ID; navigation is ambiguous"),
    }
}

async fn command(args: &[&str]) -> Result<Value> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("herdr")
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Herdr navigation timed out")?
    .context("Could not start herdr")?;
    if !output.status.success() {
        bail!(
            "Herdr navigation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let response: Value =
        serde_json::from_slice(&output.stdout).context("Herdr returned invalid JSON")?;
    if !response["error"].is_null() {
        bail!("Herdr navigation failed: {}", response["error"]);
    }
    Ok(response)
}

pub(super) async fn focus(session: &str, harness: &str) -> Result<String> {
    if !available() {
        bail!("Pane navigation is available only inside Herdr; dashboard refresh continues");
    }
    // Resolve again on every selection: panes can move or close between polls.
    let agents = command(&["agent", "list"]).await?;
    let pane = matched_pane(&agents, session, harness)?;
    command(&["agent", "focus", pane]).await?;
    Ok(format!(
        "Focused {pane}; return to this pane for the dashboard"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matches_session_identity_across_workspaces_not_titles_or_terminal_ids() {
        let response = json!({"result":{"agents":[
            {"agent":"codex","pane_id":"w9:p7","terminal_id":"wrong","agent_session":{"kind":"id","value":"wanted"}},
            {"agent":"codex","pane_id":"w1:p1","agent_session":{"kind":"path","value":"wanted"}}
        ]}});
        assert_eq!(matched_pane(&response, "wanted", "codex").unwrap(), "w9:p7");
        assert!(matched_pane(&response, "wanted", "claude").is_err());
        assert!(matched_pane(&response, "", "codex").is_err());
        assert!(matched_pane(&response, "missing", "codex").is_err());
    }

    #[test]
    fn malformed_and_ambiguous_matches_do_not_focus_an_arbitrary_pane() {
        assert!(matched_pane(&json!({}), "s", "").is_err());
        let agent = json!({"agent_session":{"kind":"id","value":"s"},"pane_id":"w1:p1"});
        assert!(matched_pane(&json!({"result":{"agents":[agent,agent]}}), "s", "").is_err());
    }
}
