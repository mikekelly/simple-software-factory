//! Installed coding agents, following Omarchy's agent catalogue
//! (`omarchy-default-agent`): mise-managed packages plus anything on PATH.

use serde::Serialize;
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct Agent {
    /// Orca agent id / Omarchy agent id (`claude`, `codex`, ...).
    pub id: String,
    pub name: String,
    /// Executable expected on PATH.
    pub command: String,
    /// How ssf starts the agent unless `repo.command` says otherwise: the
    /// executable plus the flags that let it run unattended.
    pub launch_command: String,
    pub installed: bool,
    /// Currently selected as the Omarchy default agent.
    pub default: bool,
    /// Whether the agent takes a model setting (`ssf models <id>` lists ids).
    pub takes_model: bool,
    /// Model ids the agent is known to take without asking it (Orca
    /// identifiers where Orca has them; more may work).
    pub models: Vec<String>,
    /// Effort levels the agent accepts, lowest first; empty when it has none.
    pub effort_levels: Vec<String>,
}

const KNOWN: &[(&str, &str, &str, &str)] = &[
    // id, display name, mise package, command
    ("claude", "Claude Code", "claude", "claude"),
    ("codex", "Codex", "codex", "codex"),
    ("omp", "Oh My Pi", "github:can1357/oh-my-pi", "omp"),
    ("pi", "Pi", "pi", "pi"),
    ("opencode", "OpenCode", "opencode", "opencode"),
    ("gemini", "Gemini", "gemini", "gemini"),
    ("copilot", "GitHub Copilot", "copilot", "copilot"),
    ("grok", "Grok", "npm:@xai-official/grok", "grok"),
    ("crush", "Crush", "crush", "crush"),
];

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

fn mise_has(pkg: &str) -> bool {
    Command::new("mise")
        .args(["where", pkg])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn omarchy_default_agent() -> Option<String> {
    let out = Command::new("omarchy-default-agent").output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !s.is_empty() {
        Some(s)
    } else {
        None
    }
}

pub fn list() -> Vec<Agent> {
    let default = omarchy_default_agent();
    KNOWN
        .iter()
        .map(|(id, name, pkg, cmd)| Agent {
            id: id.to_string(),
            name: name.to_string(),
            command: cmd.to_string(),
            launch_command: crate::models::default_command(id),
            installed: on_path(cmd) || mise_has(pkg),
            default: default.as_deref() == Some(*id),
            takes_model: crate::models::supports_model(id),
            models: crate::models::known_models(id)
                .iter()
                .map(|m| m.to_string())
                .collect(),
            effort_levels: crate::models::effort_levels(id)
                .iter()
                .map(|e| e.to_string())
                .collect(),
        })
        .collect()
}

pub fn is_known(id: &str) -> bool {
    KNOWN.iter().any(|(k, ..)| *k == id)
}
