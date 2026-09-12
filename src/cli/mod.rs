//! Command-line parsing and command handlers for the `ssf` binaries.
//!
//! Each command family lives in its own module so that the client entrypoint
//! remains a small dispatcher and command-specific code stays nearby.

mod auth;
mod client;
mod daemon;
mod definition;
mod doctor;
mod launch;
mod repo;
mod sessions;
mod ui;
mod vm;

#[cfg(test)]
mod tests;

use auth::*;
use daemon::*;
use definition::*;
use doctor::*;
use launch::*;
use repo::*;
use sessions::*;
use ui::*;
use vm::*;

mod prelude {
    pub(crate) use anyhow::{Context, Result, bail};
    pub(crate) use clap::{Parser, Subcommand};
    pub(crate) use serde_json::json;
    pub(crate) use std::io::{IsTerminal, Read, Write};
    pub(crate) use std::os::unix::process::CommandExt;
    pub(crate) use std::path::{Path, PathBuf};
    pub(crate) use tracing_subscriber::EnvFilter;

    pub(crate) use crate::config::{self, Config, RepoConfig, split_repo_name};
    pub(crate) use crate::{
        agents, allow, dashboard, dashboard_web, driver, engine, ghcli, github, ipc, keys, login,
        models, orca, origin, platform, prompt, release, setup, shim, state, status,
        ui as factory_ui, uninstall, vm as factory_vm,
    };
}

pub use auth::auth_logout;
pub use client::{client_main, server_main};
pub use sessions::{
    handover_cancelled_text, handover_recorded_text, summary_quotes_a_sign_in_screen_text,
};

pub(crate) use client::remote_client_command;
pub(crate) use launch::{client_executable, server_executable};
pub(crate) use sessions::purge;
