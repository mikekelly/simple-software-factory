//! Installed coding agents, following Omarchy's agent catalogue
//! (`omarchy-default-agent`): mise-managed packages plus anything on PATH.

use crate::harness::{HARNESSES, harness};
use serde::Serialize;
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct Agent {
    /// Agent id used by herdr and Omarchy (`claude`, `codex`, ...).
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
    /// Model ids the agent is known to take without asking it: the catalogue
    /// it wrote on this machine, else ssf's seeds. More may work.
    pub models: Vec<String>,
    /// Effort levels the agent accepts, lowest first; empty when it has none.
    pub effort_levels: Vec<String>,
}

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
    HARNESSES
        .iter()
        .map(|h| Agent {
            id: h.id.to_string(),
            name: h.omarchy_name.to_string(),
            command: h.command.to_string(),
            launch_command: crate::models::default_command(h.id),
            installed: on_path(h.command) || mise_has(h.mise_package),
            default: default.as_deref() == Some(h.id),
            takes_model: crate::models::supports_model(h.id),
            models: crate::models::known_models(h.id),
            effort_levels: crate::models::effort_levels(h.id)
                .iter()
                .map(|e| e.to_string())
                .collect(),
        })
        .collect()
}

/// Whether the harness's executable is on this machine: the check
/// `ssf agents` and `ssf doctor` report. An id nothing knows is not
/// installed.
pub fn installed(id: &str) -> bool {
    harness(id).is_some_and(|h| on_path(h.command) || mise_has(h.mise_package))
}

pub fn is_known(id: &str) -> bool {
    harness(id).is_some()
}
