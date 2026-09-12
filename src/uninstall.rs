//! `ssf uninstall`: take this machine back to just the package.
//!
//! Everything `ssf` set up outside its package is undone in one go: the
//! closed workspaces that are safe to remove are purged, the service is
//! stopped and disabled, the bot is
//! signed out (its keys revoked on GitHub), the microVM is destroyed, and
//! with `--data` the config and state directories too. The projects
//! directory (clones and worktrees) is never touched: it may hold work
//! that is on no remote. Nothing here runs sudo; removing the package is
//! the one step left to the person.
//!
//! The command reports first and asks once. In VM mode the workspaces
//! live on the guest's data disk, so the host asks the guest for the
//! report (`ssf uninstall --report`, JSON) and refuses to destroy the VM
//! unchecked unless `--force` says so.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::{self, Config};
use crate::state::State;
use crate::{ipc, release, status, ui, vm};

/// One workspace a tracked item still points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// `owner/repo#N`.
    pub session: String,
    pub title: String,
    /// The item is still open (or its session active), so `ssf purge`
    /// leaves the workspace alone.
    pub open: bool,
    pub path: String,
    /// `release::Check::state()`, `already gone` for a missing directory,
    /// `unknown` when git could not answer.
    pub state: String,
    pub problems: Vec<String>,
}

/// What the machine holding the workspaces knows: whether its daemon
/// answers, and every workspace with its state. Built where the state
/// file is, which in VM mode is the guest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub daemon: bool,
    #[serde(default)]
    pub items: Vec<Item>,
}

impl Report {
    /// Workspaces that would lose something if removed: anything not
    /// known to be clean and pushed, so `unknown` counts (fail closed).
    pub fn unpushed(&self) -> Vec<&Item> {
        self.items
            .iter()
            .filter(|i| i.state != "clean and pushed" && i.state != "already gone")
            .collect()
    }
}

/// What looking at one workspace directory found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    pub state: String,
    pub problems: Vec<String>,
}

/// `release::inspect` with the two answers it cannot give folded in: a
/// directory that is not there is `already gone`, and an error (not a
/// git worktree, git missing) is `unknown` with the error as the reason.
pub async fn inspect_path(path: String) -> Inspection {
    if !Path::new(&path).exists() {
        return Inspection {
            state: "already gone".into(),
            problems: Vec::new(),
        };
    }
    match release::inspect(&path).await {
        Ok(c) => Inspection {
            state: c.state(),
            problems: c.problems(),
        },
        Err(e) => Inspection {
            state: "unknown".into(),
            problems: vec![format!("{e:#}")],
        },
    }
}

/// The report for this machine: the daemon on its socket, the state file
/// on disk, git in each workspace.
pub async fn report() -> Report {
    let daemon = ipc::call(&ipc::Request::Ping).await.is_ok();
    let state = State::load().unwrap_or_default();
    report_from(&state, daemon, inspect_path).await
}

/// The report from a given state and a way of looking at a path. Every
/// repository in the state counts, not only the watched ones: a
/// repository removed from the config still has records pointing at
/// workspaces. Records that follow an item without a workspace of their
/// own (subscriber-only, or sharing another session's workspace) are
/// skipped; the owner's record covers the directory.
pub async fn report_from<F, Fut>(state: &State, daemon: bool, inspect: F) -> Report
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Inspection>,
{
    let mut items = Vec::new();
    for (repo, rs) in &state.repos {
        for s in rs.issues.values() {
            let Some(path) = &s.worktree_path else {
                continue;
            };
            if s.subscriber_only || s.shares_workspace_of.is_some() {
                continue;
            }
            let found = inspect(path.clone()).await;
            items.push(Item {
                session: status::session_id(repo, s.number),
                title: s.title.clone(),
                open: s.active || s.github_state.as_deref() == Some("open"),
                path: path.clone(),
                state: found.state,
                problems: found.problems,
            });
        }
    }
    Report { daemon, items }
}

/// What is set up on this machine (the host), gathered without the daemon.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub service_active: bool,
    pub service_enabled: bool,
    /// The widget files or the menu block are in place.
    pub desktop_present: bool,
    /// `[github] login`: the bot account.
    pub bot: Option<String>,
    pub has_token: bool,
    /// An SSH or signing key is enrolled on GitHub (`ssf auth login`).
    pub key_ids: bool,
    /// `[vm] enabled`: the factory runs in the VM.
    pub vm_mode: bool,
    pub vm_name: String,
    /// There is a VM to destroy, as the backend answers it
    /// ([`vm::Survey`]): under lima the instance and the data disk are
    /// in lima's home, not under `[vm] dir`. `None` when the backend
    /// could not be asked, which is not "no".
    pub vm_present: Option<bool>,
    /// The guest is up. `None` when the probe could not be made: under
    /// lima it forks `limactl`, and reading that as "not running" put
    /// "`ssf vm start` first, or --force to destroy them unchecked" in
    /// front of a person whose VM was working the whole time.
    pub vm_running: Option<bool>,
    /// There is a guest `ssf vm start` could bring up. Under lima a data
    /// disk outlives a deleted instance: there is then something to
    /// destroy and nothing to start, so "start it and try again" is no
    /// remedy.
    pub vm_startable: bool,
    /// There is a data disk that may hold clones and worktrees: the one
    /// part of a VM whose loss cannot be undone, and so the one that
    /// decides whether the command refuses. `None` when that could not
    /// be established, which refuses too. What `[vm] dir` holds without
    /// it is ssf's own -- a template, an ssh key, a share -- and stops
    /// nothing.
    pub vm_data: Option<bool>,
    /// The data disk's name in lima's home, for the one refusal whose
    /// whole remedy is "remove it by hand": `limactl disk delete` needs
    /// an argument, and it is not `[vm] name`. `None` under Firecracker,
    /// where the disk is a file inside `[vm] dir` and no such refusal is
    /// reachable.
    pub vm_disk: Option<String>,
    /// What `ssf vm destroy` takes with it, in words: the VM's directory
    /// under Firecracker, where its disks are; the lima instance and its
    /// data disk (both in lima's own home, not under `[vm] dir`) as well
    /// under lima.
    pub vm_removed: String,
    /// `[vm] dir`: the image and downloads, left in place.
    pub vm_base: PathBuf,
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    /// The projects directory of every driver in use (clones and
    /// worktrees), never removed.
    pub projects: Vec<PathBuf>,
    /// A Firecracker data disk a change of `[vm] backend` left in the
    /// VM's own directory, or could have; see
    /// [`vm::Vm::stranded_data_disk`]. Read on the host, so it does not
    /// depend on the guest -- the lima guest cannot see this disk by
    /// construction, and its report says nothing about the clones on it.
    pub vm_stranded_disk: Option<PathBuf>,
}

impl Facts {
    pub fn gather(cfg: &Config, vm: &vm::Vm) -> Self {
        let mut projects: Vec<PathBuf> = cfg
            .drivers_in_use()
            .into_iter()
            .map(|d| cfg.projects_dir(d))
            .filter(|p| p.exists())
            .collect();
        projects.dedup();
        let survey = vm.survey();
        Facts {
            service_active: ui::service_active(),
            service_enabled: ui::service_enabled(),
            desktop_present: ui::desktop_present(),
            bot: cfg.github.login.clone(),
            has_token: config::token_path().exists(),
            key_ids: cfg.github.ssh_key_id.is_some() || cfg.github.signing_key_id.is_some(),
            vm_mode: cfg.vm.enabled,
            vm_name: cfg.vm.name.clone(),
            vm_present: survey.present,
            vm_running: survey.running,
            vm_startable: survey.startable,
            vm_data: survey.data,
            vm_stranded_disk: vm.stranded_data_disk(),
            vm_disk: match vm.backend() {
                vm::BackendKind::Lima => Some(vm.lima_disk_name()),
                vm::BackendKind::Firecracker => None,
            },
            vm_removed: match vm.backend() {
                vm::BackendKind::Firecracker => {
                    format!("its disks in {}", vm.dir.display())
                }
                vm::BackendKind::Lima => lima_removed(
                    &vm.lima_name(),
                    &vm.lima_disk_name(),
                    vm.dir.exists().then_some(vm.dir.as_path()),
                    &survey,
                ),
            },
            vm_base: vm.base.clone(),
            config_dir: config::config_dir(),
            state_dir: config::state_dir(),
            projects,
        }
    }

    /// The guest answered on ssh, which settles what the backend could
    /// not: there is a VM, it is up, and it is startable. Without this
    /// the refusal blamed `limactl` for a report the guest itself failed
    /// to give and sent the person to fix the wrong thing -- `limactl
    /// list` will not make the guest answer, so it would not clear on
    /// the next run either -- and the report hedged with "if it is
    /// there" directly above workspaces just fetched from that guest.
    ///
    /// The data disk is *not* settled by it. Under lima the sshd that
    /// answered is lima's own and is not gated on the mount, so an
    /// instance whose disk was deleted by hand answers ssh perfectly
    /// well; asserting a disk from that would promise to destroy clones
    /// that are not there, and refuse over them.
    pub fn ssh_answered(&mut self, vm: &vm::Vm) {
        let survey = vm::Survey {
            present: Some(true),
            running: Some(true),
            startable: true,
            data: self.vm_data,
        };
        self.vm_present = survey.present;
        self.vm_running = survey.running;
        self.vm_startable = survey.startable;
        self.vm_data = survey.data;
        if vm.backend() == vm::BackendKind::Lima {
            self.vm_removed = lima_removed(
                &vm.lima_name(),
                &vm.lima_disk_name(),
                vm.dir.exists().then_some(vm.dir.as_path()),
                &survey,
            );
        }
    }

    /// Something of the bot is here to forget or revoke.
    pub fn bot_signed_in(&self) -> bool {
        self.bot.is_some() || self.has_token || self.key_ids
    }
}

/// How the report is shown.
#[derive(Debug, Clone, Default)]
pub struct Opts {
    /// `--data`: the config and state directories go too.
    pub data: bool,
    /// VM mode with the VM stopped (or the guest not answering): the
    /// workspaces on its data disk could not be looked at.
    pub vm_unchecked: bool,
    /// Why the guest gave no report, when it did not.
    pub report_error: Option<String>,
}

/// What removes the ssf package on this machine (`sudo pacman -R ssf`,
/// `sudo apt remove ssf`, `sudo dnf remove ssf`, ...), by platform.
/// Printed, never run: nothing in ssf runs sudo.
pub fn package_removal_command() -> String {
    crate::platform::package_removal_command()
}

/// Remove a marketplace-owned executable and user unit after the ordinary
/// uninstall has safely stopped the daemon and dealt with user data. Package
/// binaries have no adjacent helper, so their uninstall remains unchanged.
fn marketplace_runtime_helper(exe: &Path) -> Option<PathBuf> {
    let runtime = exe.parent().and_then(Path::parent)?;
    let helper = runtime.join("ssf-marketplace");
    let metadata = runtime.join("install.env");
    if !helper.is_file() || !metadata.is_file() {
        return None;
    }
    Some(helper)
}

struct MarketplaceLock {
    file: File,
}

fn lock_marketplace_runtime() -> Result<Option<MarketplaceLock>> {
    let exe = crate::client_executable().context("finding the ssf client executable")?;
    let Some(helper) = marketplace_runtime_helper(&exe) else {
        return Ok(None);
    };
    let home = std::env::var_os("HOME").context("HOME is required")?;
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(home).join(".cache"));
    let dir = cache.join("ssf");
    let path = dir.join("marketplace.lock");
    for unsafe_path in [&dir, &path] {
        if std::fs::symlink_metadata(unsafe_path).is_ok_and(|m| m.file_type().is_symlink()) {
            bail!(
                "refusing unsafe marketplace lifecycle lock path {}",
                unsafe_path.display()
            );
        }
    }
    std::fs::create_dir_all(&dir)?;
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    if !file.metadata()?.is_file() {
        bail!(
            "refusing unsafe marketplace lifecycle lock path {}",
            path.display()
        );
    }
    // SAFETY: `file` owns this valid descriptor for the lifetime of the guard.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error()).context("locking marketplace lifecycle");
    }
    let status = Command::new(&helper).arg("validate-uninstall").status()?;
    if !status.success() {
        bail!(
            "{} validate-uninstall exited with {status}",
            helper.display()
        );
    }
    Ok(Some(MarketplaceLock { file }))
}

fn remove_marketplace_runtime(data: bool, lock: Option<&MarketplaceLock>) -> Result<bool> {
    let exe = crate::client_executable().context("finding the ssf client executable")?;
    let Some(helper) = marketplace_runtime_helper(&exe) else {
        return Ok(false);
    };
    let mut command = Command::new(&helper);
    command.arg("uninstall");
    if let Some(lock) = lock {
        let fd = lock.file.as_raw_fd();
        // SAFETY: the descriptor remains owned by `lock`; only its exec flag changes.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error())
                .context("passing marketplace lifecycle lock");
        }
        command.arg("--lock-held");
        command.env("SSF_MARKETPLACE_LOCK_FD", fd.to_string());
    }
    if data {
        command.arg("--data");
    }
    let status = command
        .status()
        .with_context(|| format!("running {helper:?} uninstall"))?;
    if !status.success() {
        bail!("{} uninstall exited with {status}", helper.display());
    }
    Ok(true)
}

/// The report as text: what will stop, go and be revoked, what stays,
/// then every workspace with its state.
pub fn render(facts: &Facts, report: &Report, opts: &Opts) -> String {
    let mut out = String::from("ssf uninstall will:\n");
    let section = |out: &mut String, label: &str, lines: &[String]| {
        for (i, l) in lines.iter().enumerate() {
            if i == 0 {
                out.push_str(&format!("  {label:<8} {l}\n"));
            } else {
                out.push_str(&format!("           {l}\n"));
            }
        }
    };

    // stop
    let mut stop = Vec::new();
    if facts.service_active || facts.service_enabled {
        stop.push(format!(
            "{} ({}, {})",
            crate::platform::service_name(),
            if facts.service_active {
                "running"
            } else {
                "stopped"
            },
            if facts.service_enabled {
                "enabled"
            } else {
                "disabled"
            }
        ));
    }
    match facts.vm_running {
        Some(true) => stop.push(format!("VM {}", facts.vm_name)),
        // Not "no": the service stop shuts the guest down if it is up,
        // and saying nothing here would read as "there is nothing to
        // stop".
        None => stop.push(format!(
            "VM {} (if it is up: it could not be asked)",
            facts.vm_name
        )),
        Some(false) => {}
    }
    if !stop.is_empty() {
        section(&mut out, "stop:", &[stop.join(" and ")]);
    }

    // remove
    let mut remove = Vec::new();
    if report.daemon {
        remove.push("workspaces of closed items that are clean and pushed (ssf purge)".to_string());
    } else {
        let why = match (facts.vm_mode, facts.vm_running) {
            (true, Some(false)) => format!("VM {} is not running", facts.vm_name),
            (true, None) => format!(
                "VM {} could not be asked whether it is running",
                facts.vm_name
            ),
            _ => "the daemon is not running".to_string(),
        };
        remove.push(format!(
            "workspaces of closed items: not purged ({why}); left in place"
        ));
    }
    match facts.vm_present {
        Some(true) => remove.push(format!(
            "VM {} and {}{}",
            facts.vm_name,
            facts.vm_removed,
            // Only a data disk holds anyone's work; the rest of what
            // goes is ssf's own.
            match facts.vm_data {
                Some(true) => "; the clones and worktrees on its data disk go with it",
                None => "; the clones and worktrees on its data disk, if it is there, go with it",
                Some(false) => "",
            }
        )),
        Some(false) => remove.push("no VM".to_string()),
        None => remove.push(format!(
            "VM {}: could not be asked whether it is there; the destroy step tries anyway and says what happened",
            facts.vm_name
        )),
    }
    if opts.data {
        remove.push(format!(
            "{} and {} (--data)",
            facts.config_dir.display(),
            facts.state_dir.display()
        ));
    }
    section(&mut out, "remove:", &remove);

    // revoke
    let revoke = if facts.bot_signed_in() {
        let who = facts
            .bot
            .as_deref()
            .map(|b| format!("@{b}'s"))
            .unwrap_or_else(|| "the bot's".to_string());
        let mut parts = Vec::new();
        if facts.key_ids {
            parts.push("SSH and signing keys on GitHub".to_string());
        }
        if facts.has_token {
            parts.push("token here".to_string());
        }
        if parts.is_empty() {
            format!("{who} sign-in here (no keys or token to revoke)")
        } else {
            format!("{who} {}", parts.join("; its "))
        }
    } else {
        "bot not signed in; nothing to revoke".to_string()
    };
    section(&mut out, "revoke:", &[revoke]);

    // keep
    let mut keep = Vec::new();
    if facts.desktop_present {
        keep.push(
            "the installed Omarchy widget and menu entries (remove them through Omarchy)"
                .to_string(),
        );
    }
    for p in &facts.projects {
        keep.push(format!(
            "{} (clones and worktrees; may hold unpushed work)",
            p.display()
        ));
    }
    if facts.vm_base.exists() {
        keep.push(format!(
            "{} (VM image and downloads; retained -- inspect before removing)",
            facts.vm_base.display()
        ));
    }
    if !opts.data {
        keep.push(format!(
            "{} and {} (remove with --data)",
            facts.config_dir.display(),
            facts.state_dir.display()
        ));
    }
    keep.push(format!(
        "the package: {} (yours: ssf never runs sudo)",
        package_removal_command()
    ));
    section(&mut out, "keep:", &keep);

    // items
    out.push_str("items:\n");
    if let Some(e) = &opts.report_error {
        out.push_str(&format!("  {e}\n"));
    }
    if opts.vm_unchecked {
        let (what, remedy) = vm_uncheckable(facts);
        out.push_str(&format!(
            "  {what}: its workspaces (the clones on its data disk) cannot be checked; {remedy}, or --force destroys them unchecked\n"
        ));
    } else if report.items.is_empty() {
        out.push_str("  (none)\n");
    }
    let width = report
        .items
        .iter()
        .map(|i| i.session.chars().count())
        .max()
        .unwrap_or(0);
    for i in &report.items {
        out.push_str(&format!(
            "  {:<7} {:<width$} \"{}\"  [{}]  {}\n",
            if i.open { "open" } else { "closed" },
            i.session,
            status::one_line(&i.title, 40),
            i.state,
            i.path,
        ));
        for p in &i.problems {
            // git's multi-line explanations (a fetch failure) stay on
            // one line here; the listing is for scanning.
            out.push_str(&format!(
                "            - {}\n",
                p.split_whitespace().collect::<Vec<_>>().join(" ")
            ));
        }
    }
    out
}

/// Print the report as JSON: the guest's answer to the host, and what
/// `--report` shows anywhere.
pub async fn print_report() -> Result<()> {
    let r = report().await;
    println!("{}", serde_json::to_string(&r)?);
    Ok(())
}

/// Ask the guest for its report over ssh.
fn guest_report(vm: &vm::Vm) -> Result<Report> {
    let remote = vec![
        format!("{}=1", vm::GUEST_ENV),
        "ssf".to_string(),
        "uninstall".to_string(),
        "--report".to_string(),
    ];
    let out = vm
        .ssh(&remote, false)
        .stdin(Stdio::null())
        .output()
        .context("running ssh")?;
    if !out.status.success() {
        bail!(
            "`ssf uninstall --report` failed in the guest: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    serde_json::from_slice(&out.stdout).context("parsing the guest's report")
}

/// Remove a directory that may already be gone: `Ok(true)` when it was
/// there, `Ok(false)` when not.
pub fn remove_dir(path: &Path) -> Result<bool> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

/// `question [y/N]`: only a plain yes goes ahead.
fn confirm_default_no(question: &str) -> Result<bool> {
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let a = line.trim().to_lowercase();
    Ok(a == "y" || a == "yes")
}

/// Why the command refuses to go on, unless `force`: work that is not
/// on origin (which, in VM mode, goes with the VM's disks), or a VM whose
/// workspaces could not be looked at.
pub fn hard_stop(facts: &Facts, report: &Report, opts: &Opts, force: bool) -> Option<String> {
    if force {
        return None;
    }
    let unpushed = report.unpushed();
    if !unpushed.is_empty() {
        let n = unpushed.len();
        return Some(format!(
            "{n} workspace{} hold{} uncommitted or unpushed work, or could not be checked (listed above); push or discard it first, or pass --force to {}",
            if n == 1 { "" } else { "s" },
            if n == 1 { "s" } else { "" },
            if facts.vm_mode {
                "destroy it with the VM's disks"
            } else {
                "go ahead and leave it where it is"
            }
        ));
    }
    // Before the `vm_unchecked` clause, and not part of it: that flag
    // is set from the guest's report, and this is a disk the guest
    // cannot see. On a healthy lima VM -- instance up, ssh answering,
    // report parsed -- the flag stays false, and without this the
    // destroy took the directory and the Firecracker clones in it, with
    // no prompt at all under `--yes`.
    if let Some(disk) = &facts.vm_stranded_disk {
        return Some(format!(
            "{} is a data disk a change of [vm] backend left behind, and the lima VM cannot mount it to look inside; \
             put [vm] backend back to firecracker and run this again to reach the clones and worktrees on it, \
             or pass --force to destroy them unchecked",
            disk.display()
        ));
    }
    if opts.vm_unchecked {
        let (what, remedy) = vm_uncheckable(facts);
        return Some(format!(
            "{what}, so its workspaces cannot be checked; {remedy}, or pass --force to destroy them unchecked"
        ));
    }
    None
}

/// Would going ahead destroy work nobody has looked at? Only a data disk
/// holds clones and worktrees, so only a data disk can stop the command
/// -- and a disk that could not be asked about (`None`) stops it too,
/// because the wrong answer in that direction is silent data loss. What
/// `[vm] dir` holds without one is ssf's own: a template, an ssh key, a
/// share.
fn unchecked_workspaces(data: Option<bool>) -> bool {
    data != Some(false)
}

/// What `ssf vm destroy` takes with it under lima, in words: only the
/// parts that are there. The instance, the data disk and `[vm] dir` come
/// and go separately -- an instance whose directory was removed by hand,
/// a disk that outlived its instance -- and a report that promises to
/// remove what is not there is a report to trust less.
fn lima_removed(instance: &str, disk: &str, dir: Option<&Path>, survey: &vm::Survey) -> String {
    let mut parts = Vec::new();
    match (survey.startable, survey.running) {
        (true, _) => parts.push(format!("the lima instance {instance}")),
        (false, None) => parts.push(format!("the lima instance {instance} if it is there")),
        (false, Some(_)) => {}
    }
    match survey.data {
        Some(true) => parts.push(format!("its data disk {disk} in lima's home")),
        None => parts.push(format!(
            "its data disk {disk} in lima's home if it is there"
        )),
        Some(false) => {}
    }
    if let Some(d) = dir {
        parts.push(d.display().to_string());
    }
    // "a and b and c" is a hard sentence to read in the one line a
    // person scans before saying yes to destroying it all.
    match parts.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, [one])) => format!("{one} and {last}"),
        Some((last, rest)) => format!("{}, and {last}", rest.join(", ")),
    }
}

/// Why the VM's workspaces could not be looked at, and what to do about
/// it instead of `--force`: the same two halves in the report and in the
/// refusal, so they cannot drift apart.
fn vm_uncheckable(facts: &Facts) -> (String, String) {
    let name = &facts.vm_name;
    // Host mode is not a state of the VM but of the configuration: the
    // guest is never asked, whatever it would have answered, so none of
    // the remedies below would clear this one. Starting the VM does not
    // help while ssf is not pointed at it.
    if !facts.vm_mode {
        return (
            format!(
                "VM {name} still has a data disk, and `[vm] enabled = false` means ssf never asks its guest for anything"
            ),
            "point ssf back at it (`ssf config set vm.enabled true`) and run this again, so the clones and worktrees on that disk can be looked at".to_string(),
        );
    }
    match (facts.vm_running, facts.vm_startable, facts.vm_data) {
        (Some(true), _, _) => (
            format!("VM {name} gave no report"),
            "`ssf vm restart` first (the guest runs the ssf of its last start)".to_string(),
        ),
        (Some(false), true, _) => (
            format!("VM {name} is not running"),
            "`ssf vm start` first".to_string(),
        ),
        // Nothing to start, and a disk that lima says is there: it
        // outlived the instance that mounted it. `ssf vm start` refuses
        // an instance that is not there, so it is no remedy and must not
        // be offered as one. The disk is named because this is the one
        // refusal whose whole remedy is a command the person types, and
        // `limactl disk delete` needs an argument -- which is
        // `ssf-<name>`, not `[vm] name`.
        (Some(false), false, Some(true)) => (
            format!("VM {name}'s data disk outlived its instance"),
            format!(
                "nothing can mount it to look inside, so decide about the disk and remove it by hand (`limactl disk delete{}`)",
                match &facts.vm_disk {
                    Some(d) => format!(" {d}"),
                    None => String::new(),
                }
            ),
        ),
        // The disk question is the one that went unanswered, so nothing
        // may be asserted about a disk here.
        (Some(false), false, None) => (
            format!("VM {name} has no guest to start, and its data disk could not be asked about"),
            "find out why first (`limactl disk list` by hand says what lima answers)".to_string(),
        ),
        // Written out rather than folded into the arm above so that the
        // compiler, not a comment, is what keeps the two apart. Nothing
        // reaches it: `vm_unchecked` is false where there is no disk.
        (Some(false), false, Some(false)) => (
            format!("VM {name} has no data disk"),
            "there is no work on it to lose".to_string(),
        ),
        (None, _, _) => (
            format!("VM {name} could not be asked whether it is running"),
            "find out why first (`limactl list` by hand says what lima answers)".to_string(),
        ),
    }
}

/// The command: report, hard stops, one question, the steps, what is left.
pub async fn run(yes: bool, force: bool, data: bool) -> Result<()> {
    // Hold the marketplace lifecycle lock and validate its owned files before
    // any report, purge, service operation, or destructive teardown begins.
    let marketplace_lock = lock_marketplace_runtime()?;
    let cfg = Config::load()?;
    let vm = vm::Vm::new(&cfg);
    let mut facts = Facts::gather(&cfg, &vm);
    let mut opts = Opts {
        data,
        ..Opts::default()
    };
    let report = if facts.vm_mode {
        // A probe that could not be made is not "the guest is down", and
        // ssh is the better evidence either way: try it unless the
        // backend actually said no.
        if facts.vm_running != Some(false) && vm.ssh_ok() {
            facts.ssh_answered(&vm);
            match guest_report(&vm) {
                Ok(r) => r,
                Err(e) => {
                    opts.report_error = Some(format!(
                        "could not get the report from VM {}: {e:#}",
                        facts.vm_name
                    ));
                    opts.vm_unchecked = true;
                    Report::default()
                }
            }
        } else {
            // "There might be a data disk" is reason enough not to
            // destroy the work on it unasked; only a backend that said
            // "there is none" lets this through. Leftovers in `[vm] dir`
            // with no disk are ssf's own and stop nothing.
            opts.vm_unchecked = unchecked_workspaces(facts.vm_data);
            opts.report_error = match facts.vm_running {
                Some(true) => Some(format!(
                    "VM {} is running but does not answer on ssh",
                    facts.vm_name
                )),
                None => Some(format!(
                    "VM {} could not be asked whether it is running, and does not answer on ssh",
                    facts.vm_name
                )),
                Some(false) => None,
            };
            Report::default()
        }
    } else {
        // The host's own daemon answers for the workspaces on this
        // machine. It knows nothing of the sessions that ran in a VM,
        // and `[vm] enabled = false` does not remove one: the disk
        // survives the flag, and the destroy step below is not gated on
        // it. So an empty `items:` here means "not looked at", not
        // "nothing to lose", and a data disk that may hold clones stops
        // the command just as it does in VM mode.
        opts.vm_unchecked = unchecked_workspaces(facts.vm_data);
        report().await
    };
    print!("{}", render(&facts, &report, &opts));

    if let Some(why) = hard_stop(&facts, &report, &opts, force) {
        bail!("{why}");
    }
    if !yes {
        if !vm::stdin_is_tty() {
            bail!("not a terminal; pass --yes");
        }
        if !confirm_default_no("Continue?")? {
            println!("nothing changed");
            return Ok(());
        }
    }

    let mut failed = 0usize;
    let mut fail = |what: &str, e: anyhow::Error| {
        failed += 1;
        println!("{what} failed: {e:#}");
    };

    println!("==> purge closed workspaces");
    if !report.daemon {
        println!("skipped: the daemon is not running; workspaces left in place");
    } else if facts.vm_mode {
        match vm.exec_ssf(&["purge".to_string()]) {
            Ok(st) if st.success() => {}
            Ok(st) => println!(
                "purge failed in the guest (exit {}); workspaces left in place",
                st.code().unwrap_or(1)
            ),
            Err(e) => fail("purge", e),
        }
    } else if let Err(e) = crate::purge(false, None, false, false).await {
        fail("purge", e);
    }

    println!("==> stop and disable {}", crate::platform::service_name());
    let was_off = !facts.service_active && !facts.service_enabled;
    // The service command's own failure ends the uninstall here, and does
    // not join the tally of steps that failed: every step below destroys
    // something (the VM, the bot's login, and with --data the config and
    // state directories), and doing any of that while a daemon may still
    // be running is what this step exists to prevent. Counting it and
    // carrying on would have reported the failure at the very end, over a
    // factory that had been taken apart underneath it.
    match ui::set_service_enabled_on_error(false, ui::OnServiceError::Fail) {
        Ok(()) if was_off => println!("already stopped and disabled"),
        Ok(()) => println!("service stopped and disabled"),
        // Not "nothing has been destroyed": the purge above ran first,
        // and it removes the workspaces it found clean and pushed and
        // says `released` on their items. What this stop protects is
        // everything below it.
        Err(e) => bail!(
            "{e:#}\nthe steps after this one destroy the VM, sign the bot out{} and none of them may run while the daemon may still be working, so none of them has run.{} Stop the daemon by hand (`{}`), then run this again",
            if data {
                " and remove the config and state directories,"
            } else {
                ","
            },
            if report.daemon {
                " The purge above did run: workspaces that were clean and pushed are gone, and their items have a `released` comment."
            } else {
                ""
            },
            crate::platform::service_hint("stop"),
        ),
    }

    println!("==> sign the bot out");
    if !facts.bot_signed_in() {
        println!("bot not signed in; nothing to revoke");
    } else if let Err(e) = crate::auth_logout(false).await {
        fail("bot", e);
    }

    println!("==> destroy the VM");
    // The report the person said yes to, not a fresh probe: a backend
    // that answered differently in between would make the step and the
    // report disagree. Only a plain "there is none" skips it; an
    // unknown goes through `destroy`, which says what it found.
    let mut vm_gone = true;
    if facts.vm_present == Some(false) {
        println!("no VM");
    } else if let Err(e) = vm.destroy().await {
        fail("vm", e);
        vm_gone = false;
    }
    // With the VM gone the factory is nowhere; a config still saying
    // `vm.enabled` would send `ssf status` looking for it. Read the file
    // again: the sign-out step wrote it. With `--data` it goes anyway.
    //
    // Only with the VM actually gone. Written over a destroy that
    // failed, it turns the next run into a host-mode one: no guest
    // report, no refusal over the workspaces on the surviving data disk,
    // and a `Continue?` that destroys them unchecked.
    if facts.vm_mode && vm_gone && !data {
        match Config::load() {
            Ok(mut cfg) if cfg.vm.enabled => {
                cfg.vm.enabled = false;
                match cfg.save() {
                    Ok(()) => println!(
                        "[vm] enabled = false written to {}",
                        config::config_path().display()
                    ),
                    Err(e) => fail("vm", e),
                }
            }
            Ok(_) => {}
            Err(e) => fail("vm", e),
        }
    }

    if data {
        println!("==> remove the config and state directories");
        for (dir, note) in [
            (&facts.config_dir, ""),
            (&facts.state_dir, " (a reinstall starts the service again)"),
        ] {
            match remove_dir(dir) {
                Ok(true) => println!("removed {}{note}", dir.display()),
                Ok(false) => println!("{} was already gone", dir.display()),
                Err(e) => fail("data", e),
            }
        }
    }

    println!("left in place:");
    for p in &facts.projects {
        println!(
            "  {} (clones and worktrees; may hold unpushed work)",
            p.display()
        );
    }
    if facts.vm_base.exists() {
        println!(
            "  {} (VM image and downloads; retained -- inspect before removing)",
            facts.vm_base.display()
        );
    }
    if !data {
        println!(
            "  {} and {} (remove with `ssf uninstall --data`, or by hand)",
            facts.config_dir.display(),
            facts.state_dir.display()
        );
    }
    let marketplace_removed = if failed == 0 {
        match remove_marketplace_runtime(data, marketplace_lock.as_ref()) {
            Ok(removed) => removed,
            Err(e) => {
                failed += 1;
                println!("marketplace runtime failed: {e:#}");
                false
            }
        }
    } else {
        if marketplace_lock.is_some() {
            println!("marketplace runtime preserved so this uninstall can be retried");
        }
        false
    };
    if !marketplace_removed && marketplace_lock.is_none() {
        println!("the one step that is yours: {}", package_removal_command());
    }
    if failed > 0 {
        bail!(
            "{failed} step{} failed (listed above)",
            if failed == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
