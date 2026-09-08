//! `ssf uninstall`: take this machine back to just the package.
//!
//! Everything `ssf` set up outside its package is undone in one go: the
//! closed workspaces that are safe to remove are purged, the service is
//! stopped and disabled, the bar widget and menu entries go, the bot is
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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

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
    /// Instances and disks of ssf's that this configuration does not
    /// name -- what a changed `[vm] name` leaves behind. Named in the
    /// report as things `ssf uninstall` will not touch, never removed by
    /// it: ssf cannot tell a VM someone renamed away to keep from one
    /// they abandoned, and only one of those is safe to delete.
    pub vm_strays: Vec<vm::Stray>,
    /// Directories that are there and could not be read, so nobody
    /// knows what is in them -- including whether a VM a rename or a
    /// backend change orphaned is. Named, not counted: a `~/.lima` left
    /// root-owned by an earlier `sudo` sent people to fix permissions
    /// on `[vm] dir`, which was never the problem.
    pub vm_unread: Vec<PathBuf>,
    /// `[vm] dir` was there when the report was built. Snapshotted, so
    /// that the list printed before the question and the list printed
    /// after the last step are the same list in fact and not only in
    /// intent -- the steps in between can remove it.
    pub vm_base_exists: bool,
    /// What `ssf vm destroy` takes with it, in words: the VM's directory
    /// under Firecracker, where its disks are; the lima instance and its
    /// data disk (both in lima's own home, not under `[vm] dir`) as well
    /// under lima.
    pub vm_removed: String,
    /// `[vm] dir`: the image and downloads, left in place -- and, after
    /// a changed `[vm] name` under Firecracker, the old VM's directory
    /// with its data disk, which is why the line about it is not always
    /// "safe to remove".
    pub vm_base: PathBuf,
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    /// The projects directory of every driver in use (clones and
    /// worktrees), never removed.
    pub projects: Vec<PathBuf>,
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
            vm_strays: survey.strays.clone(),
            vm_unread: survey.unread.clone(),
            vm_base_exists: vm.base.exists(),
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
            // Absolute, for the same reason the `rm -rf` remedy is:
            // the report names this directory beside a command that
            // gives its full path, and a relative `[vm] dir` would print
            // two different-looking paths for one place.
            vm_base: std::path::absolute(&vm.base).unwrap_or_else(|_| vm.base.clone()),
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
            // ssh says nothing about what else lima holds.
            strays: self.vm_strays.clone(),
            unread: Vec::new(),
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
    if facts.desktop_present {
        remove.push("the Factory bar widget and menu entries".to_string());
    }
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
        // "no VM" on its own would be a lie while lima holds an
        // `ssf-*` instance: what there is none of is a VM for *this*
        // configuration, and the strays are named under `keep:` below.
        Some(false) => remove.push(no_vm_line(facts)),
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

    let mut keep = kept(facts, opts.data);
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
    if opts.vm_unchecked {
        let (what, remedy) = vm_uncheckable(facts);
        return Some(format!(
            "{what}, so its workspaces cannot be checked; {remedy}, or pass --force to destroy them unchecked"
        ));
    }
    None
}

/// What `ssf uninstall` leaves behind, in words.
///
/// One list, because there are two places that print it -- the report
/// before the question and the epilogue after the last step -- and the
/// second is the one the person is still looking at. Twice now a
/// sentence has been made honest in the report and left as it was a few
/// lines of output later, so the two no longer have the chance.
pub fn kept(facts: &Facts, data: bool) -> Vec<String> {
    let mut keep = Vec::new();
    for p in &facts.projects {
        keep.push(format!(
            "{} (clones and worktrees; may hold unpushed work)",
            p.display()
        ));
    }
    if facts.vm_base_exists {
        // "safe to remove" is true of the images and downloads. It is
        // not true of a VM directory a changed `[vm] name` orphaned,
        // which sits in here with its data disk -- and telling a person
        // their own clones are safe to delete is worse than deleting
        // them, because they run the command themselves and it works.
        //
        // Nor is it true of a directory nobody could read. "Could not
        // look" is not "nothing there", and safety nobody verified is
        // not safety.
        let holds_work = facts
            .vm_strays
            .iter()
            .any(|s| s.kind == vm::StrayKind::Directory);
        keep.push(format!(
            "{} (VM image and downloads{})",
            facts.vm_base.display(),
            match (facts.vm_unread.contains(&facts.vm_base), holds_work) {
                (true, _) => "; ssf could not read it, so what is in it is unknown",
                (false, true) => "; safe to remove except for what is listed below",
                (false, false) => "; safe to remove",
            }
        ));
    }
    if !data {
        keep.push(format!(
            "{} and {} (remove with `ssf uninstall --data`, or by hand)",
            facts.config_dir.display(),
            facts.state_dir.display()
        ));
    }
    for stray in &facts.vm_strays {
        keep.push(format!(
            "{} {}, which this configuration does not name{} -- untouched, `--force` included; `{}` removes it{}",
            stray.what(),
            stray.name,
            if stray.holds_work() {
                " (its clones and worktrees are in it)"
            } else {
                ""
            },
            stray.remove,
            stray.caveat()
        ));
    }
    keep
}

/// The epilogue after the last step, in full. A function rather than a
/// loop inside `run`, because `run` is not reachable from a test, and
/// this is the half of `kept` that has drifted from the other twice.
pub fn left_in_place(facts: &Facts, data: bool) -> String {
    let mut out = String::from("left in place:\n");
    for line in kept(facts, data) {
        out.push_str(&format!("  {line}\n"));
    }
    out
}

/// "There is nothing to destroy", said so that it stays true. A bare
/// "no VM" over an instance lima is holding, or a VM directory `[vm]
/// dir` still has, is the sentence this whole change exists to stop --
/// so both the report and the destroy step say it through here.
fn no_vm_line(facts: &Facts) -> String {
    match (facts.vm_strays.is_empty(), facts.vm_unread.is_empty()) {
        // A directory nobody could read may hold a VM a rename or a
        // backend change orphaned, so "no VM" is a claim about contents
        // nobody looked at -- and which directory is the whole of what
        // the person has to act on.
        (_, false) => format!(
            "no VM named {} to remove; {}",
            facts.vm_name,
            unread_note(&facts.vm_unread)
        ),
        (true, true) => "no VM".to_string(),
        (false, true) => format!("no VM named {} to remove", facts.vm_name),
    }
}

/// "I could not look, and here is where", in one sentence for every
/// command that has to say it. Hand-copied three ways once, and the
/// copy in `ssf vm status` named the wrong directory.
pub fn unread_note(unread: &[PathBuf]) -> String {
    let names: Vec<String> = unread.iter().map(|p| p.display().to_string()).collect();
    format!(
        "{} could not be read, so what else is in {} is unknown",
        names.join(" and "),
        if names.len() > 1 { "them" } else { "it" }
    )
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

    println!("==> remove the bar widget and menu entries");
    if !facts.desktop_present {
        println!("already gone");
    } else if let Err(e) = ui::uninstall_all() {
        fail("desktop", e);
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
        // The same sentence the report is careful about: there is no VM
        // *named this*, which is not the same as nothing being here.
        println!("{}", no_vm_line(&facts));
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

    print!("{}", left_in_place(&facts, data));
    println!("the one step that is yours: {}", package_removal_command());
    if failed > 0 {
        bail!(
            "{failed} step{} failed (listed above)",
            if failed == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::IssueState;

    fn issue(n: u64, path: Option<&str>) -> IssueState {
        IssueState {
            number: n,
            title: format!("item {n}"),
            worktree_path: path.map(str::to_string),
            ..Default::default()
        }
    }

    fn state_with(issues: Vec<IssueState>) -> State {
        let mut st = State::default();
        let rs = st.repo_mut("o/r");
        for i in issues {
            rs.issues.insert(i.number, i);
        }
        st
    }

    /// A fake look at a path: the state is the last path segment, so a
    /// test names the outcome it wants in the path itself.
    async fn by_name(path: String) -> Inspection {
        let state = path.rsplit('/').next().unwrap_or("").replace('_', " ");
        let problems = if state == "dirty" {
            vec!["2 uncommitted changes (modified or untracked files)".to_string()]
        } else {
            Vec::new()
        };
        Inspection { state, problems }
    }

    #[tokio::test]
    async fn report_picks_owned_workspaces_and_marks_open_and_closed() {
        let mut open = issue(1, Some("/w/dirty"));
        open.active = true;
        open.github_state = Some("open".into());
        let mut closed = issue(2, Some("/w/clean_and_pushed"));
        closed.github_state = Some("closed".into());
        let mut open_inactive = issue(3, Some("/w/unknown"));
        open_inactive.github_state = Some("open".into());
        let no_workspace = issue(4, None);
        let mut sub = issue(5, Some("/w/dirty"));
        sub.subscriber_only = true;
        let mut shared = issue(6, Some("/w/dirty"));
        shared.shares_workspace_of = Some(1);
        let mut gone = issue(7, Some("/w/already_gone"));
        gone.github_state = Some("merged".into());
        let st = state_with(vec![
            open,
            closed,
            open_inactive,
            no_workspace,
            sub,
            shared,
            gone,
        ]);
        let r = report_from(&st, true, by_name).await;
        assert!(r.daemon);
        let sessions: Vec<&str> = r.items.iter().map(|i| i.session.as_str()).collect();
        assert_eq!(sessions, ["o/r#1", "o/r#2", "o/r#3", "o/r#7"]);
        assert!(r.items[0].open);
        assert!(!r.items[1].open);
        assert!(
            r.items[2].open,
            "an open item with no session is still open"
        );
        assert!(!r.items[3].open);
        assert_eq!(r.items[0].state, "dirty");
        assert_eq!(r.items[0].problems.len(), 1);
        assert_eq!(r.items[1].state, "clean and pushed");
        assert_eq!(r.items[3].state, "already gone");
        let unpushed: Vec<&str> = r.unpushed().iter().map(|i| i.session.as_str()).collect();
        assert_eq!(
            unpushed,
            ["o/r#1", "o/r#3"],
            "dirty and unknown count, clean and gone do not"
        );
    }

    #[tokio::test]
    async fn report_covers_repos_no_longer_watched_and_round_trips_as_json() {
        let mut st = state_with(vec![issue(1, Some("/w/clean_and_pushed"))]);
        st.repo_mut("gone/repo")
            .issues
            .insert(9, issue(9, Some("/w/dirty")));
        let r = report_from(&st, false, by_name).await;
        assert_eq!(r.items.len(), 2);
        assert_eq!(r.items[0].session, "gone/repo#9");
        let json = serde_json::to_string(&r).unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        assert!(!back.daemon);
    }

    #[tokio::test]
    async fn a_missing_path_is_already_gone_and_a_plain_dir_is_unknown() {
        let dir = std::env::temp_dir().join(format!("ssf-uninstall-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("nope").to_string_lossy().to_string();
        assert_eq!(inspect_path(missing).await.state, "already gone");
        let found = inspect_path(dir.to_string_lossy().to_string()).await;
        assert_eq!(found.state, "unknown");
        assert_eq!(found.problems.len(), 1, "{:?}", found.problems);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn facts() -> Facts {
        Facts {
            service_active: true,
            service_enabled: true,
            desktop_present: true,
            bot: Some("bot".into()),
            has_token: true,
            key_ids: true,
            vm_mode: true,
            vm_name: "factory".into(),
            vm_present: Some(true),
            vm_running: Some(true),
            vm_startable: true,
            vm_data: Some(true),
            vm_disk: Some("ssf-factory".into()),
            vm_strays: Vec::new(),
            vm_unread: Vec::new(),
            vm_base_exists: false,
            vm_removed: "its disks in /vm/factory".into(),
            vm_base: PathBuf::from("/nonexistent/vm"),
            config_dir: PathBuf::from("/c"),
            state_dir: PathBuf::from("/s"),
            projects: vec![PathBuf::from("/p")],
        }
    }

    #[test]
    fn render_lists_what_goes_and_what_stays() {
        let r = Report {
            daemon: true,
            items: vec![Item {
                session: "o/r#9".into(),
                title: "a title".into(),
                open: false,
                path: "/p/w".into(),
                state: "dirty".into(),
                problems: vec!["1 uncommitted change".into()],
            }],
        };
        let text = render(&facts(), &r, &Opts::default());
        assert!(
            text.contains(&format!(
                "stop:    {} (running, enabled) and VM factory",
                crate::platform::service_name()
            )),
            "{text}"
        );
        assert!(
            text.contains("the Factory bar widget and menu entries"),
            "{text}"
        );
        assert!(text.contains("clean and pushed (ssf purge)"), "{text}");
        assert!(
            text.contains("VM factory and its disks in /vm/factory; the clones"),
            "{text}"
        );
        assert!(
            text.contains("revoke:  @bot's SSH and signing keys on GitHub; its token here"),
            "{text}"
        );
        assert!(
            text.contains("/p (clones and worktrees; may hold unpushed work)"),
            "{text}"
        );
        assert!(
            text.contains("/c and /s (remove with `ssf uninstall --data`, or by hand)"),
            "{text}"
        );
        assert!(!text.contains("(--data)"), "{text}");
        assert!(
            text.contains("closed  o/r#9 \"a title\"  [dirty]  /p/w"),
            "{text}"
        );
        assert!(text.contains("- 1 uncommitted change"), "{text}");
        assert!(!text.contains("(none)"), "{text}");
    }

    #[test]
    fn under_lima_the_report_names_the_instance_and_disk_not_the_vm_directory() {
        // The lima disks are not under `[vm] dir`: they are lima's, in
        // lima's own home, and `ssf vm destroy` names them.
        let mut f = facts();
        f.vm_removed =
            "the lima instance ssf-factory and its data disk ssf-factory in lima's home, and /vm/factory"
                .into();
        let text = render(&f, &Report::default(), &Opts::default());
        assert!(
            text.contains(
                "VM factory and the lima instance ssf-factory and its data disk ssf-factory in lima's home, and /vm/factory; the clones"
            ),
            "{text}"
        );
    }

    #[test]
    fn render_with_the_daemon_down_says_not_purged() {
        let mut f = facts();
        let text = render(&f, &Report::default(), &Opts::default());
        assert!(
            text.contains(
                "workspaces of closed items: not purged (the daemon is not running); left in place"
            ),
            "{text}"
        );
        assert!(text.contains("(none)"), "{text}");
        f.vm_running = Some(false);
        let text = render(&f, &Report::default(), &Opts::default());
        assert!(
            text.contains("not purged (VM factory is not running)"),
            "{text}"
        );
        // A probe that could not be made is a third answer, and saying
        // "is not running" over it points at `ssf vm start` for a guest
        // that may be working.
        f.vm_running = None;
        let text = render(&f, &Report::default(), &Opts::default());
        assert!(
            text.contains("not purged (VM factory could not be asked whether it is running)"),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "stop:    {} (running, enabled) and VM factory (if it is up",
                crate::platform::service_name()
            )),
            "{text}"
        );
    }

    #[test]
    fn render_with_data_moves_the_dirs_from_keep_to_remove() {
        let text = render(
            &facts(),
            &Report::default(),
            &Opts {
                data: true,
                ..Opts::default()
            },
        );
        assert!(text.contains("/c and /s (--data)"), "{text}");
        assert!(!text.contains("remove with --data"), "{text}");
    }

    #[test]
    fn render_says_what_is_already_gone() {
        let f = Facts {
            service_active: false,
            service_enabled: false,
            desktop_present: false,
            bot: None,
            has_token: false,
            key_ids: false,
            vm_present: Some(false),
            vm_running: Some(false),
            vm_startable: false,
            vm_data: Some(false),
            ..facts()
        };
        let text = render(&f, &Report::default(), &Opts::default());
        assert!(!text.contains("stop:"), "{text}");
        assert!(!text.contains("bar widget"), "{text}");
        assert!(
            text.contains("remove:  workspaces of closed items: not purged"),
            "{text}"
        );
        assert!(text.contains("no VM"), "{text}");
        assert!(
            text.contains("revoke:  bot not signed in; nothing to revoke"),
            "{text}"
        );
    }

    #[test]
    fn render_with_the_vm_unchecked_says_so_instead_of_none() {
        // The VM's disks are there but it is not running.
        let stopped = Facts {
            vm_running: Some(false),
            ..facts()
        };
        let text = render(
            &stopped,
            &Report::default(),
            &Opts {
                vm_unchecked: true,
                report_error: Some("could not get the report".into()),
                ..Opts::default()
            },
        );
        assert!(text.contains("could not get the report"), "{text}");
        assert!(
            text.contains("VM factory is not running: its workspaces")
                && text.contains("`ssf vm start` first"),
            "{text}"
        );
        assert!(!text.contains("(none)"), "{text}");
    }

    #[test]
    fn hard_stop_names_unpushed_work_and_what_force_does_to_it() {
        let dirty = Report {
            daemon: true,
            items: vec![Item {
                session: "o/r#1".into(),
                title: "t".into(),
                open: true,
                path: "/p/w".into(),
                state: "dirty".into(),
                problems: vec![],
            }],
        };
        let host = Facts::default();
        let why = hard_stop(&host, &dirty, &Opts::default(), false).unwrap();
        assert!(why.starts_with("1 workspace holds"), "{why}");
        assert!(why.ends_with("leave it where it is"), "{why}");
        let guest = Facts {
            vm_mode: true,
            vm_name: "factory".into(),
            ..Facts::default()
        };
        let why = hard_stop(&guest, &dirty, &Opts::default(), false).unwrap();
        assert!(why.ends_with("destroy it with the VM's disks"), "{why}");
        assert!(hard_stop(&guest, &dirty, &Opts::default(), true).is_none());
        // Clean and gone workspaces do not stop anything.
        let mut clean = dirty.clone();
        clean.items[0].state = "clean and pushed".into();
        clean.items.push(Item {
            state: "already gone".into(),
            ..clean.items[0].clone()
        });
        assert!(hard_stop(&host, &clean, &Opts::default(), false).is_none());
    }

    #[test]
    fn hard_stop_on_an_unchecked_vm_says_how_to_check_it() {
        let stopped = Facts {
            vm_mode: true,
            vm_name: "factory".into(),
            vm_present: Some(true),
            vm_running: Some(false),
            vm_startable: true,
            ..Facts::default()
        };
        let unchecked = Opts {
            vm_unchecked: true,
            ..Opts::default()
        };
        let why = hard_stop(&stopped, &Report::default(), &unchecked, false).unwrap();
        assert!(
            why.contains("is not running") && why.contains("`ssf vm start`"),
            "{why}"
        );
        let running = Facts {
            vm_running: Some(true),
            ..stopped.clone()
        };
        let why = hard_stop(&running, &Report::default(), &unchecked, false).unwrap();
        assert!(
            why.contains("gave no report") && why.contains("`ssf vm restart`"),
            "{why}"
        );
        assert!(hard_stop(&running, &Report::default(), &unchecked, true).is_none());
        assert!(hard_stop(&running, &Report::default(), &Opts::default(), false).is_none());
    }

    #[test]
    fn render_with_a_silent_running_vm_points_at_restart() {
        let facts = Facts {
            vm_mode: true,
            vm_name: "factory".into(),
            vm_present: Some(true),
            vm_running: Some(true),
            vm_startable: true,
            ..Facts::default()
        };
        let opts = Opts {
            vm_unchecked: true,
            report_error: Some("VM factory is running but does not answer on ssh".into()),
            ..Opts::default()
        };
        let text = render(&facts, &Report::default(), &opts);
        assert!(text.contains("does not answer on ssh"), "{text}");
        assert!(
            text.contains("VM factory gave no report: its workspaces")
                && text.contains("`ssf vm restart` first"),
            "{text}"
        );
        assert!(!text.contains("factory is not running"), "{text}");
    }

    #[test]
    fn an_unaskable_backend_is_neither_a_running_vm_nor_a_missing_one() {
        // The three answers the report and the refusal have to keep
        // apart. Reading "the probe could not be made" as "not running"
        // put `ssf vm start`, and a `--force` that destroys the clones
        // on the data disk, in front of a person whose VM was up.
        let base = Facts {
            vm_mode: true,
            vm_name: "factory".into(),
            vm_present: None,
            vm_running: None,
            vm_startable: false,
            ..Facts::default()
        };
        let unchecked = Opts {
            vm_unchecked: true,
            ..Opts::default()
        };
        let why = hard_stop(&base, &Report::default(), &unchecked, false).unwrap();
        assert!(
            why.contains("could not be asked whether it is running")
                && why.contains("limactl list"),
            "{why}"
        );
        assert!(!why.contains("ssf vm start"), "{why}");
        let text = render(&base, &Report::default(), &unchecked);
        // The report may not promise to destroy what it could not find.
        assert!(
            text.contains("could not be asked whether it is there"),
            "{text}"
        );
        assert!(!text.contains("go with it"), "{text}");
        assert!(!text.contains("no VM"), "{text}");
    }

    #[test]
    fn a_disk_that_outlived_its_instance_is_not_offered_a_start() {
        // `ssf vm start` refuses an instance that is not there, so
        // offering it as the way to check the workspaces on a disk that
        // outlived one leaves `--force` as the only real route.
        let orphan = Facts {
            vm_mode: true,
            vm_name: "factory".into(),
            vm_present: Some(true),
            vm_running: Some(false),
            vm_startable: false,
            // The state that reaches this message: a disk, and no
            // instance. Leftovers in `[vm] dir` alone hold no work, so
            // they never get here.
            vm_data: Some(true),
            ..Facts::default()
        };
        let unchecked = Opts {
            vm_unchecked: true,
            ..Opts::default()
        };
        let why = hard_stop(&orphan, &Report::default(), &unchecked, false).unwrap();
        assert!(why.contains("data disk outlived its instance"), "{why}");
        assert!(!why.contains("ssf vm start"), "{why}");
        assert!(why.contains("--force"), "{why}");
    }

    #[test]
    fn leftovers_in_the_vm_directory_are_removed_without_a_refusal() {
        // Under lima `[vm] dir`/<name> holds the generated template, the
        // ssh key and the share -- ssf's own, no one's work. A build
        // that died before `limactl create`, or a person who deleted the
        // instance and the disk by hand, leaves exactly that. There is
        // something to remove and nothing to check, so the command must
        // not refuse, and must not say a data disk outlived anything.
        let leftovers = Facts {
            vm_mode: true,
            vm_name: "factory".into(),
            vm_present: Some(true),
            vm_running: Some(false),
            vm_startable: false,
            vm_data: Some(false),
            vm_removed: "/v/factory".into(),
            ..Facts::default()
        };
        // The rule itself, not `Opts::default()`, which would make any
        // facts pass: a disk that is not there is the one answer that
        // lets the command go ahead.
        assert!(!unchecked_workspaces(leftovers.vm_data));
        assert!(unchecked_workspaces(Some(true)));
        assert!(unchecked_workspaces(None), "an unasked disk stops it too");
        let text = render(&leftovers, &Report::default(), &Opts::default());
        assert!(text.contains("VM factory and /v/factory"), "{text}");
        assert!(!text.contains("go with it"), "{text}");
        assert!(!text.contains("outlived"), "{text}");
    }

    #[test]
    fn the_report_names_only_the_parts_of_a_lima_vm_that_are_there() {
        let survey = |startable, running, data| vm::Survey {
            present: Some(true),
            running,
            startable,
            data,
            strays: Vec::new(),
            unread: Vec::new(),
        };
        assert_eq!(
            lima_removed(
                "ssf-f",
                "ssf-f",
                Some(Path::new("/v/f")),
                &survey(true, Some(false), Some(true))
            ),
            "the lima instance ssf-f, its data disk ssf-f in lima's home, and /v/f"
        );
        // An instance whose directory was removed by hand: the missing
        // directory is not named.
        assert_eq!(
            lima_removed(
                "ssf-f",
                "ssf-f",
                None,
                &survey(true, Some(true), Some(true))
            ),
            "the lima instance ssf-f and its data disk ssf-f in lima's home"
        );
        // A disk that outlived its instance.
        assert_eq!(
            lima_removed(
                "ssf-f",
                "ssf-f",
                None,
                &survey(false, Some(false), Some(true))
            ),
            "its data disk ssf-f in lima's home"
        );
        // Leftovers in `[vm] dir` and nothing of lima's: ssf's own
        // template, ssh key and share, and no promise about anyone else.
        assert_eq!(
            lima_removed(
                "ssf-f",
                "ssf-f",
                Some(Path::new("/v/f")),
                &survey(false, Some(false), Some(false))
            ),
            "/v/f"
        );
        // Nothing could be asked: everything is hedged, nothing asserted.
        assert_eq!(
            lima_removed("ssf-f", "ssf-f", None, &survey(false, None, None)),
            "the lima instance ssf-f if it is there and its data disk ssf-f in lima's home if it is there"
        );
    }

    #[test]
    fn the_refusal_names_the_thing_that_actually_could_not_be_checked() {
        // One source of wording for the report and the refusal, and no
        // arm may assert a fact its inputs do not carry.
        let f = |running, startable, data| Facts {
            vm_mode: true,
            vm_name: "factory".into(),
            vm_present: Some(true),
            vm_running: running,
            vm_startable: startable,
            vm_data: data,
            ..Facts::default()
        };
        // ssh answered but the guest's report did not: `run` settles
        // `vm_running` to `Some(true)` on the strength of that ssh, so
        // the refusal blames the guest and not `limactl`.
        let (what, remedy) = vm_uncheckable(&f(Some(true), false, None));
        assert!(what.contains("gave no report"), "{what}");
        assert!(remedy.contains("ssf vm restart"), "{remedy}");
        let (what, remedy) = vm_uncheckable(&f(Some(false), true, Some(true)));
        assert!(what.contains("is not running"), "{what}");
        assert!(remedy.contains("ssf vm start"), "{remedy}");
        // The one refusal whose whole remedy is a command the person
        // types, so it has to carry the disk's own name -- `ssf-factory`,
        // not `[vm] name`.
        let named = Facts {
            vm_disk: Some("ssf-factory".into()),
            ..f(Some(false), false, Some(true))
        };
        let (what, remedy) = vm_uncheckable(&named);
        assert!(what.contains("data disk outlived its instance"), "{what}");
        assert!(
            remedy.contains("limactl disk delete ssf-factory"),
            "{remedy}"
        );
        // The disk question is the one that failed: nothing may be said
        // about a disk outliving anything.
        let (what, remedy) = vm_uncheckable(&f(Some(false), false, None));
        assert!(what.contains("could not be asked about"), "{what}");
        assert!(!what.contains("outlived"), "{what}");
        assert!(remedy.contains("limactl disk list"), "{remedy}");
        let (what, remedy) = vm_uncheckable(&f(None, false, None));
        assert!(what.contains("whether it is running"), "{what}");
        assert!(remedy.contains("limactl list"), "{remedy}");
        // Only a guest that is up, or one that can be started, may be
        // pointed at a start or a restart.
        for (running, startable) in [(Some(false), false), (None, false), (None, true)] {
            let (_, remedy) = vm_uncheckable(&f(running, startable, None));
            assert!(!remedy.contains("ssf vm start"), "{remedy}");
            assert!(!remedy.contains("ssf vm restart"), "{remedy}");
        }
    }

    #[test]
    fn host_mode_does_not_destroy_a_data_disk_it_never_asked_about() {
        // `[vm] enabled = false` does not remove a VM: the instance and
        // the data disk survive the flag, and the destroy step is not
        // gated on it. The host's own daemon answers for this machine's
        // workspaces and knows nothing of the sessions that ran in the
        // guest, so `items: (none)` there is "not looked at" -- and
        // printing it under a line promising to delete a disk of clones,
        // with no refusal in between, is how `--yes` came to destroy
        // them.
        let host = Facts {
            vm_mode: false,
            vm_name: "factory".into(),
            vm_present: Some(true),
            vm_running: Some(false),
            vm_startable: true,
            vm_data: Some(true),
            vm_disk: Some("ssf-factory".into()),
            ..Facts::default()
        };
        let unchecked = Opts {
            vm_unchecked: unchecked_workspaces(host.vm_data),
            ..Opts::default()
        };
        assert!(
            unchecked.vm_unchecked,
            "a data disk stops it in host mode too"
        );
        let why = hard_stop(&host, &Report::default(), &unchecked, false).unwrap();
        assert!(why.contains("`[vm] enabled = false`"), "{why}");
        assert!(why.contains("vm.enabled true"), "{why}");
        // Starting the VM would not clear this one: nothing asks the
        // guest while the configuration does not point at it.
        assert!(!why.contains("`ssf vm start`"), "{why}");
        let text = render(&host, &Report::default(), &unchecked);
        assert!(!text.contains("(none)"), "{text}");
        // A host that never had a VM is untouched by any of it.
        let bare = Facts {
            vm_data: Some(false),
            ..host.clone()
        };
        assert!(!unchecked_workspaces(bare.vm_data));
        assert!(hard_stop(&bare, &Report::default(), &Opts::default(), false).is_none());
    }

    #[test]
    fn ssh_answering_proves_the_guest_is_there_and_not_that_it_has_a_disk() {
        // Under lima the sshd that answered is lima's own, and it is not
        // gated on the data disk being mounted -- so an instance whose
        // disk was deleted by hand answers ssh perfectly well. Reading a
        // disk out of that would promise to destroy clones that are not
        // there, and refuse over them.
        let mut cfg = Config::default();
        cfg.vm.name = "factory".into();
        cfg.vm.dir = "/nonexistent/ssf-vm".into();
        cfg.vm.backend = Some(vm::BackendKind::Lima);
        let vm = vm::Vm::new(&cfg);
        for (data, promises_clones) in [(Some(true), true), (Some(false), false), (None, true)] {
            let mut f = Facts {
                vm_mode: true,
                vm_name: "factory".into(),
                vm_present: None,
                vm_running: None,
                vm_startable: false,
                vm_data: data,
                ..Facts::default()
            };
            f.ssh_answered(&vm);
            assert_eq!(f.vm_present, Some(true));
            assert_eq!(f.vm_running, Some(true));
            assert!(f.vm_startable);
            assert_eq!(f.vm_data, data, "ssh says nothing about the disk");
            let text = render(&f, &Report::default(), &Opts::default());
            assert_eq!(
                text.contains("clones and worktrees on its data disk"),
                promises_clones,
                "{text}"
            );
            // A disk lima could not be asked about is hedged, never
            // promised as fact.
            assert_eq!(
                text.contains("if it is there, go with it"),
                data.is_none(),
                "{text}"
            );
            // Whatever the backend could not say, nothing is hedged
            // about the instance any more.
            assert!(!f.vm_removed.contains("instance ssf-factory if it is there"));
        }
    }

    #[test]
    fn a_stray_is_named_and_kept_and_no_flag_reaches_it() {
        // A VM someone renamed `[vm] name` away from. `ssf uninstall`
        // must stop saying a bare "no VM" over it -- that sentence is
        // the bug -- and must never remove it: ssf cannot tell one kept
        // on purpose from one abandoned, and only one of those is safe
        // to delete. So it is named under `keep:`, with the command,
        // and `--force` does not reach it.
        let stray = Facts {
            vm_name: "new".into(),
            vm_present: Some(false),
            vm_running: Some(false),
            vm_startable: false,
            vm_data: Some(false),
            vm_strays: vec![
                vm::Stray::lima_instance("ssf-old".into()),
                vm::Stray::lima_disk("ssf-old".into()),
            ],
            ..facts()
        };
        let text = render(&stray, &Report::default(), &Opts::default());
        assert!(
            !text.contains("no VM\n") && text.contains("no VM named new to remove"),
            "{text}"
        );
        assert!(text.contains("keep:"), "{text}");
        for cmd in ["limactl delete ssf-old", "limactl disk delete ssf-old"] {
            assert!(text.contains(cmd), "{cmd} missing from:\n{text}");
        }
        assert!(text.contains("`--force` included"), "{text}");
        // An observation, not a claim of ownership: ssf did not
        // necessarily create it and does not need to have, because
        // nothing here removes one.
        assert!(
            text.contains("which this configuration does not name"),
            "{text}"
        );
        assert!(!text.contains("not this configuration's"), "{text}");
        // The disk cannot go before its instance, and one line is all a
        // person reads.
        assert!(text.contains("after its instance"), "{text}");
        // The step that runs after the confirmation says it the same
        // way; a bare "no VM" there is the last word the person reads.
        assert_eq!(no_vm_line(&stray), "no VM named new to remove");
        // A stray is outside the thing being uninstalled, so nothing
        // stops the command over it and no flag turns it into a target.
        assert!(hard_stop(&stray, &Report::default(), &Opts::default(), false).is_none());
        assert!(hard_stop(&stray, &Report::default(), &Opts::default(), true).is_none());
        assert!(!unchecked_workspaces(stray.vm_data));
        // With nothing of ssf's elsewhere in lima, the plain sentence
        // comes back.
        let alone = Facts {
            vm_strays: Vec::new(),
            ..stray
        };
        let text = render(&alone, &Report::default(), &Opts::default());
        assert!(text.contains("no VM"), "{text}");
        assert!(!text.contains("limactl delete"), "{text}");
        assert_eq!(no_vm_line(&alone), "no VM");
    }

    #[test]
    fn the_directory_holding_an_orphaned_data_disk_is_not_called_safe_to_remove() {
        // `[vm] dir` is "the image and downloads, safe to remove" -- and
        // under Firecracker it is also where a VM directory a changed
        // `[vm] name` orphaned sits, with its clones. Saying "safe to
        // remove" over that is the report telling a person to delete
        // their own work, which is worse than deleting it: they run the
        // command themselves and it succeeds.
        let base = std::env::temp_dir().join(format!("ssf-keep-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let orphan = Facts {
            vm_base: base.clone(),
            vm_base_exists: true,
            vm_present: Some(false),
            vm_data: Some(false),
            vm_strays: vec![vm::Stray::directory(&base.join("old"))],
            ..facts()
        };
        let text = render(&orphan, &Report::default(), &Opts::default());
        let clean = Facts {
            vm_strays: Vec::new(),
            ..orphan.clone()
        };
        let clean_text = render(&clean, &Report::default(), &Opts::default());
        // A lima disk is not in `[vm] dir` at all, so it must not put a
        // caveat on that line. Rendered while the directory still
        // exists, since the line only appears when it does.
        let lima = kept(
            &Facts {
                vm_strays: vec![vm::Stray::lima_disk("ssf-old".into())],
                ..orphan.clone()
            },
            false,
        );
        std::fs::remove_dir_all(&base).unwrap();
        assert!(
            lima.iter()
                .any(|l| l.contains("downloads; safe to remove)")),
            "{lima:?}"
        );
        // ... but the disk itself is where the clones are, and its own
        // line has to say so: narrowing that to `[vm] dir` directories
        // silently dropped it from the one stray that holds work.
        assert!(
            lima.iter()
                .any(|l| l.contains("its clones and worktrees are in it")),
            "{lima:?}"
        );
        assert!(
            text.contains("safe to remove except for what is listed below"),
            "{text}"
        );
        assert!(
            text.contains("its clones and worktrees are in it"),
            "{text}"
        );
        assert!(text.contains("rm -rf"), "{text}");
        // Every line the epilogue prints after the last step is a line
        // the report printed before the question. That sentence has
        // drifted between the two twice, which is why they share a list
        // rather than a fix.
        let printed = left_in_place(&orphan, false);
        for line in kept(&orphan, false) {
            assert!(
                text.contains(&line),
                "epilogue line missing from report: {line}"
            );
            assert!(
                printed.contains(&line),
                "the epilogue does not print what kept() gives it: {line}"
            );
        }
        assert!(printed.starts_with("left in place:\n"), "{printed}");
        // `--data` removed the config and state directories, so the
        // epilogue must not go on offering them: passing the flag
        // through is what makes the two lists the same list.
        assert!(
            left_in_place(&orphan, true) != printed,
            "the epilogue ignores --data"
        );
        assert!(
            !left_in_place(&orphan, true).contains("remove with `ssf uninstall --data`"),
            "it lists what the run just removed"
        );
        // With nothing orphaned the old, true sentence stands.
        assert!(
            clean_text.contains("downloads; safe to remove)"),
            "{clean_text}"
        );
    }

    #[test]
    fn a_directory_nobody_could_read_is_never_called_safe_or_empty() {
        // The whole point of knowing it, at the layer a person reads.
        // Pinned here because the `Survey` flag being right buys nothing
        // if `Facts::gather` drops it or the two sentences ignore it --
        // all three of those reverted green before this test existed.
        let unread = Facts {
            vm_name: "new".into(),
            vm_base: PathBuf::from("/v"),
            vm_base_exists: true,
            vm_unread: vec![PathBuf::from("/v")],
            vm_present: Some(false),
            vm_data: Some(false),
            vm_strays: Vec::new(),
            ..facts()
        };
        let lines = kept(&unread, false);
        assert!(
            lines.iter().any(|l| l.contains("could not read it")),
            "{lines:?}"
        );
        // The sentence names the directory, because that is what the
        // person has to act on -- a bool left every printer to guess,
        // and they guessed `[vm] dir` for a fact about lima's home.
        let elsewhere = Facts {
            vm_unread: vec![PathBuf::from("/home/me/.lima")],
            ..unread.clone()
        };
        let line = no_vm_line(&elsewhere);
        assert!(line.contains("/home/me/.lima"), "{line}");
        assert!(!line.contains("/v could not"), "{line}");
        // ... and a `[vm] dir` that was readable keeps its own true
        // sentence even while somewhere else could not be read.
        assert!(
            kept(&elsewhere, false)
                .iter()
                .any(|l| l.contains("safe to remove)")),
            "{:?}",
            kept(&elsewhere, false)
        );
        assert!(
            !lines.iter().any(|l| l.contains("safe to remove")),
            "safety nobody verified: {lines:?}"
        );
        let line = no_vm_line(&unread);
        assert!(line.contains("could not be read"), "{line}");
        assert_ne!(line, "no VM");
        // ... and that `Facts::gather` actually carries it, which
        // building `Facts` by hand does not pin: dropping it there left
        // every assertion above green.
        #[cfg(unix)]
        {
            let _sandbox = crate::config::test_support::sandbox();
            let base = std::env::temp_dir().join(format!(
                "ssf-gather-unread-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&base).unwrap();
            let mut cfg = Config::default();
            cfg.vm.name = "new".into();
            cfg.vm.backend = Some(vm::BackendKind::Firecracker);
            cfg.vm.dir = base.to_string_lossy().into_owned();
            let vm = vm::Vm::new(&cfg);
            // The headline behaviour of the whole change, through the
            // one function that carries it from the backend to the
            // page: dropping `vm_strays` here left every hand-built
            // `Facts` test green.
            std::fs::create_dir_all(base.join("old")).unwrap();
            std::fs::write(base.join("old").join("data.ext4"), b"disk").unwrap();
            let gathered = Facts::gather(&cfg, &vm);
            assert_eq!(
                gathered
                    .vm_strays
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>(),
                ["old"],
                "the VM a rename left behind must reach the report"
            );
            assert!(gathered.vm_base_exists, "and so must the snapshot");
            // From a *relative* `[vm] dir`, since `temp_dir()` is
            // already absolute and asserting over it pins nothing.
            let mut rel = cfg.clone();
            rel.vm.dir = "target".into();
            let rel_vm = vm::Vm::new(&rel);
            let rel_base = Facts::gather(&rel, &rel_vm).vm_base;
            assert!(
                rel_base.is_absolute(),
                "the report names this beside an absolute `rm -rf`: {}",
                rel_base.display()
            );
            // ssh proves the guest is up; it says nothing about a
            // directory on the host nobody could read.
            let mut after_ssh = gathered.clone();
            after_ssh.vm_unread = vec![PathBuf::from("/home/me/.lima")];
            after_ssh.ssh_answered(&vm);
            assert_eq!(
                after_ssh.vm_unread,
                [PathBuf::from("/home/me/.lima")],
                "ssh does not make an unreadable directory readable"
            );
            // ... and the snapshot has to be a snapshot: a `[vm] dir`
            // that is not there must not be reported as one that is,
            // since the report's whole `keep:` line for it hangs on this.
            let mut gone = cfg.clone();
            gone.vm.dir = base.join("nowhere").to_string_lossy().into_owned();
            let gone_vm = vm::Vm::new(&gone);
            assert!(!Facts::gather(&gone, &gone_vm).vm_base_exists);
            assert!(gathered.vm_unread.is_empty(), "readable while readable");
            set_mode(&base, 0o000);
            let readable_anyway = std::fs::read_dir(&base).is_ok();
            let unread = Facts::gather(&cfg, &vm).vm_unread;
            set_mode(&base, 0o755);
            std::fs::remove_dir_all(&base).unwrap();
            // Root reads it regardless, and then there is nothing to
            // assert -- said by skipping rather than by wrapping the
            // assertion in a condition that makes it vacuous either way.
            if !readable_anyway {
                assert_eq!(
                    unread,
                    std::slice::from_ref(&base),
                    "the directory that could not be read, by name"
                );
            }
        }

        // And with the directory readable and empty, the plain sentences
        // come back.
        let known = Facts {
            vm_unread: Vec::new(),
            ..unread
        };
        assert_eq!(no_vm_line(&known), "no VM");
        assert!(
            kept(&known, false)
                .iter()
                .any(|l| l.contains("safe to remove)")),
            "{:?}",
            kept(&known, false)
        );
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn package_removal_is_pacman_on_omarchy() {
        let cmd = package_removal_command();
        if crate::platform::is_omarchy() && crate::platform::which("pacman").is_some() {
            assert_eq!(cmd, "sudo pacman -R ssf");
        } else {
            assert!(cmd.contains("ssf"), "{cmd}");
        }
    }

    #[test]
    fn data_removal_tolerates_missing_dirs() {
        let dir = std::env::temp_dir().join(format!(
            "ssf-uninstall-data-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(!remove_dir(&dir).unwrap());
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/f"), "x").unwrap();
        assert!(remove_dir(&dir).unwrap());
        assert!(!dir.exists());
        assert!(!remove_dir(&dir).unwrap());
    }
}
