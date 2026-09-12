use super::*;
use serde_json::json;

fn cfg() -> DaemonConfig {
    DaemonConfig::default()
}

mod catalogue;
mod events;
mod instructions;
mod lifecycle;
