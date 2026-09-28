use super::*;
use serde_json::json;

fn cfg() -> DaemonConfig {
    DaemonConfig::default()
}

/// A first prompt split where the item's own part begins: what ssf tells
/// the agent (its prompt and the guidance that follows it), then the
/// item's own section, from its opening tag.
fn split_item(prompt: &str) -> (&str, &str) {
    let at = prompt
        .find("\n<issue>\n")
        .or_else(|| prompt.find("\n<pull-request>\n"))
        .map(|at| at + 1)
        .unwrap_or_else(|| panic!("no item header in:\n{prompt}"));
    prompt.split_at(at)
}

mod catalogue;
mod events;
mod instructions;
mod lifecycle;
