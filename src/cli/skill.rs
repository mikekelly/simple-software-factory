//! Documentation comes from the same source tree as the executing binary.
use super::prelude::*;

#[derive(Clone, Copy, Subcommand)]
pub(super) enum SkillTopic {
    /// First installation, bot enrollment, and the first repository.
    Setup,
    /// Agent-specific setup and operating rules.
    Agent,
    /// Writing a repository's SSF.md: what goes there, what goes in AGENTS.md.
    SsfMd,
    /// The assistant that acts for a person, on the factory host or from theirs.
    Liaison,
    /// Everyday client commands and server selection.
    ClientCli,
    /// Daemon operation, service ownership, and troubleshooting.
    Server,
    /// Configuration options, repository settings, and access control.
    Config,
    /// VM lifecycle, sizing, and host versus guest ownership.
    Vm,
    /// Grok Bot and stripped headless Linux hosts.
    Headless,
    /// Standalone binaries and client-only SSH installations.
    InstallBinaries,
    /// Harnesses and workspace drivers.
    Drivers,
    /// Session lifecycle, collaboration, release, and purge.
    Sessions,
    /// Terminal dashboard and optional web UI.
    Dashboard,
    /// Safe removal and retained data.
    Uninstall,
}

pub(super) fn print(topic: Option<SkillTopic>) -> Result<()> {
    let document = match topic {
        None => include_str!("../../docs/skills/root.md"),
        Some(SkillTopic::Setup) => include_str!("../../docs/setup.md"),
        Some(SkillTopic::Agent) => include_str!("../../docs/agent-guidance.md"),
        Some(SkillTopic::SsfMd) => include_str!("../../docs/ssf-md.md"),
        Some(SkillTopic::Liaison) => include_str!("../../docs/liaison.md"),
        Some(SkillTopic::ClientCli) => include_str!("../../docs/skills/client-cli.md"),
        Some(SkillTopic::Server) => include_str!("../../docs/skills/server.md"),
        Some(SkillTopic::Config) => include_str!("../../docs/configuration.md"),
        Some(SkillTopic::Vm) => include_str!("../../docs/vm.md"),
        Some(SkillTopic::Headless) => include_str!("../../docs/headless-host.md"),
        Some(SkillTopic::InstallBinaries) => include_str!("../../docs/install-binaries.md"),
        Some(SkillTopic::Drivers) => include_str!("../../docs/drivers.md"),
        Some(SkillTopic::Sessions) => include_str!("../../docs/sessions.md"),
        Some(SkillTopic::Dashboard) => include_str!("../../docs/dashboard.md"),
        Some(SkillTopic::Uninstall) => include_str!("../../docs/uninstall.md"),
    };
    let mut stdout = std::io::stdout().lock();
    writeln!(
        stdout,
        "SSF {} — bundled guidance\n",
        env!("CARGO_PKG_VERSION")
    )?;
    stdout.write_all(document.as_bytes())?;
    Ok(())
}
