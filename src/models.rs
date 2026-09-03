//! Per-harness model and effort launch preferences.
//!
//! The identifiers are the ones Orca uses (`orca orchestration worker-start
//! --model <id> --effort <level>`): a model id is passed to the harness as-is
//! (Claude Code family aliases such as `opus`, Codex ids such as `gpt-5.5`),
//! and an effort level is one of the levels the harness accepts. This module
//! knows how each harness takes those on its command line, and seeds the
//! model lists shown by the menus; unknown model ids still pass through, as
//! they do in Orca.

use anyhow::{Result, bail};

pub struct Catalogue {
    pub harness: &'static str,
    /// Flag that selects the model.
    model_flag: &'static str,
    /// Model ids to offer in menus (the harness may know more).
    pub models: &'static [&'static str],
    /// Effort levels the harness accepts, lowest first; empty when it has no
    /// effort setting.
    pub effort_levels: &'static [&'static str],
    /// Arguments that select an effort level.
    effort_args: fn(&str) -> Vec<String>,
}

fn no_effort(_: &str) -> Vec<String> {
    Vec::new()
}
fn claude_effort(level: &str) -> Vec<String> {
    vec!["--effort".into(), level.into()]
}
fn codex_effort(level: &str) -> Vec<String> {
    vec!["-c".into(), format!("model_reasoning_effort={level}")]
}
fn grok_effort(level: &str) -> Vec<String> {
    vec!["--reasoning-effort".into(), level.into()]
}

const CATALOGUES: &[Catalogue] = &[
    Catalogue {
        harness: "claude",
        model_flag: "--model",
        models: &["fable", "opus", "sonnet", "haiku"],
        effort_levels: &["low", "medium", "high", "xhigh", "max"],
        effort_args: claude_effort,
    },
    Catalogue {
        harness: "codex",
        model_flag: "-m",
        models: &[
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
            "gpt-5.2-codex",
        ],
        effort_levels: &["minimal", "low", "medium", "high", "xhigh", "max", "ultra"],
        effort_args: codex_effort,
    },
    Catalogue {
        harness: "gemini",
        model_flag: "-m",
        models: &[
            "gemini-3-pro-preview",
            "gemini-3-flash-preview",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
        ],
        effort_levels: &[],
        effort_args: no_effort,
    },
    Catalogue {
        harness: "grok",
        model_flag: "-m",
        models: &["grok-4.6", "grok-4.5"],
        effort_levels: &["low", "medium", "high", "xhigh"],
        effort_args: grok_effort,
    },
];

pub fn catalogue(harness: &str) -> Option<&'static Catalogue> {
    CATALOGUES.iter().find(|c| c.harness == harness)
}

pub fn supports_model(harness: &str) -> bool {
    catalogue(harness).is_some()
}

pub fn known_models(harness: &str) -> &'static [&'static str] {
    catalogue(harness).map(|c| c.models).unwrap_or(&[])
}

pub fn effort_levels(harness: &str) -> &'static [&'static str] {
    catalogue(harness).map(|c| c.effort_levels).unwrap_or(&[])
}

/// Check that `model` and `effort` can be applied to `harness`. Model ids are
/// opaque (the harness decides whether it knows them); effort levels must be
/// ones the harness accepts.
pub fn validate(harness: &str, model: Option<&str>, effort: Option<&str>) -> Result<()> {
    if let Some(m) = model.map(str::trim) {
        if m.is_empty() {
            bail!("model must not be empty");
        }
        if m.contains(char::is_whitespace) {
            bail!("model must be a single identifier, got {m:?}");
        }
        if !supports_model(harness) {
            bail!(
                "{harness} does not take a model setting (supported: {})",
                CATALOGUES
                    .iter()
                    .map(|c| c.harness)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    if let Some(e) = effort.map(str::trim) {
        if e.is_empty() {
            bail!("effort must not be empty");
        }
        let levels = effort_levels(harness);
        if levels.is_empty() {
            bail!(
                "{harness} does not take an effort level (supported: {})",
                CATALOGUES
                    .iter()
                    .filter(|c| !c.effort_levels.is_empty())
                    .map(|c| c.harness)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if !levels.contains(&e) {
            bail!(
                "effort {e:?} is not a level {harness} accepts (one of: {})",
                levels.join(", ")
            );
        }
    }
    Ok(())
}

/// Command-line arguments that make `harness` use `model` and `effort`.
/// Settings the harness has no way to take are dropped.
pub fn launch_args(harness: &str, model: Option<&str>, effort: Option<&str>) -> Vec<String> {
    let Some(cat) = catalogue(harness) else {
        return Vec::new();
    };
    let mut args = Vec::new();
    if let Some(m) = model.map(str::trim).filter(|m| !m.is_empty()) {
        args.push(cat.model_flag.to_string());
        args.push(m.to_string());
    }
    if let Some(e) = effort
        .map(str::trim)
        .filter(|e| cat.effort_levels.contains(e))
    {
        args.extend((cat.effort_args)(e));
    }
    args
}

/// `command` with the model and effort arguments appended, shell-quoted.
pub fn apply_to_command(
    command: &str,
    harness: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> String {
    let mut out = command.trim_end().to_string();
    for arg in launch_args(harness, model, effort) {
        out.push(' ');
        out.push_str(&shell_word(&arg));
    }
    out
}

/// Quote a word for `sh -c` unless it is plain enough to leave alone.
fn shell_word(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.=/:,+@%".contains(c));
    if plain {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_gets_model_and_effort_flags() {
        assert_eq!(
            apply_to_command(
                "claude --dangerously-skip-permissions",
                "claude",
                Some("opus"),
                Some("high")
            ),
            "claude --dangerously-skip-permissions --model opus --effort high"
        );
        assert_eq!(
            apply_to_command("claude", "claude", None, Some("max")),
            "claude --effort max"
        );
        assert_eq!(apply_to_command("claude", "claude", None, None), "claude");
    }

    #[test]
    fn codex_uses_short_model_flag_and_config_override() {
        assert_eq!(
            apply_to_command("codex", "codex", Some("gpt-5.5"), Some("xhigh")),
            "codex -m gpt-5.5 -c model_reasoning_effort=xhigh"
        );
    }

    #[test]
    fn grok_and_gemini_flags() {
        assert_eq!(
            apply_to_command("grok", "grok", Some("grok-4.6"), Some("xhigh")),
            "grok -m grok-4.6 --reasoning-effort xhigh"
        );
        assert_eq!(
            apply_to_command("gemini", "gemini", Some("gemini-2.5-pro"), None),
            "gemini -m gemini-2.5-pro"
        );
    }

    #[test]
    fn unsupported_settings_are_dropped_from_the_command() {
        assert_eq!(
            apply_to_command("opencode", "opencode", Some("x"), Some("high")),
            "opencode"
        );
        // Gemini has no effort setting.
        assert_eq!(
            apply_to_command("gemini", "gemini", None, Some("high")),
            "gemini"
        );
        // A level the harness does not accept is not passed on.
        assert_eq!(
            apply_to_command("claude", "claude", None, Some("ultra")),
            "claude"
        );
    }

    #[test]
    fn validation() {
        assert!(validate("claude", Some("opus"), Some("high")).is_ok());
        assert!(validate("claude", Some("claude-opus-5"), None).is_ok());
        assert!(validate("claude", None, None).is_ok());
        assert!(validate("codex", Some("gpt-5.5"), Some("ultra")).is_ok());
        assert!(validate("claude", None, Some("ultra")).is_err());
        assert!(validate("claude", Some("opus sonnet"), None).is_err());
        assert!(validate("claude", Some(""), None).is_err());
        assert!(validate("gemini", None, Some("high")).is_err());
        assert!(validate("opencode", Some("x"), None).is_err());
        assert!(validate("opencode", None, None).is_ok());
    }

    #[test]
    fn odd_model_ids_are_quoted() {
        assert_eq!(
            apply_to_command("claude", "claude", Some("it's"), None),
            "claude --model 'it'\\''s'"
        );
    }
}
