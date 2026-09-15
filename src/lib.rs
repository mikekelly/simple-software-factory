//! Shared implementation of the `ssf` client and `ssf-server` daemon.
//!
//! Watches GitHub repos for issues assigned to a bot account and turns each one
//! into a workspace in herdr running a coding agent, feeding later issue activity
//! into that agent.

mod agents;
mod allow;
mod claude_delivery;
mod config;
mod dashboard;
mod dashboard_transport;
mod dashboard_web;
mod delivery_channel;
mod driver;
mod engine;
mod events;
mod ghcli;
mod github;
mod herdr;
mod ipc;
mod keys;
mod login;
mod models;
mod origin;
mod platform;
mod prompt;
mod release;
mod server_catalog;
mod sessions;
mod setup;
mod shim;
mod state;
mod status;
mod ui;
mod uninstall;
mod vm;

mod cli;

pub use cli::{
    auth_logout, client_main, handover_cancelled_text, handover_recorded_text, server_main,
    summary_quotes_a_sign_in_screen_text,
};

pub(crate) use cli::{
    client_executable, hostname, purge, remote_client_command, server_executable,
};
