//! Documentation comes from the same source tree as the executing binary.
use super::prelude::*;

#[derive(Clone, Copy, Subcommand)]
pub(super) enum SkillTopic {
    /// From a fresh machine to a factory watching its first repository.
    Setup,
    /// Adding a repository to a running factory: access, stack, SSF.md, the first issue.
    Repo,
    /// Operating a running factory: targets, inspection, the service, upgrades.
    Operate,
    /// Symptom, check and remedy for a factory that is not behaving.
    Troubleshoot,
    /// Distro-, platform-, harness- and vendor-specific notes, and upgrading from an older ssf.
    Specifics,
    /// Rules for an agent installing or operating a factory for a person.
    Agent,
    /// Writing a repository's SSF.md: what goes there, what goes in AGENTS.md.
    SsfMd,
    /// The assistant that acts for a person, on the factory host or from theirs.
    Liaison,
    /// A bounded audit of a project's SSF.md and AGENTS.md.
    Audit,
    /// Every configuration key, and the client's server catalog.
    Config,
    /// VM lifecycle, sizing, and host versus guest ownership.
    Vm,
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
        Some(SkillTopic::Setup) => include_str!("../../docs/install.md"),
        Some(SkillTopic::Repo) => include_str!("../../docs/repositories.md"),
        Some(SkillTopic::Operate) => include_str!("../../docs/operate.md"),
        Some(SkillTopic::Troubleshoot) => include_str!("../../docs/troubleshooting.md"),
        Some(SkillTopic::Specifics) => include_str!("../../docs/platform-specifics.md"),
        Some(SkillTopic::Agent) => include_str!("../../docs/agent-guidance.md"),
        Some(SkillTopic::SsfMd) => include_str!("../../docs/ssf-md.md"),
        Some(SkillTopic::Liaison) => include_str!("../../docs/liaison.md"),
        Some(SkillTopic::Audit) => include_str!("../../docs/audit.md"),
        Some(SkillTopic::Config) => include_str!("../../docs/configuration.md"),
        Some(SkillTopic::Vm) => include_str!("../../docs/vm.md"),
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
