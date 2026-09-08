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
    /// The VM's directory exists or it is running.
    pub vm_present: bool,
    pub vm_running: bool,
    /// `<vm.dir>/<name>`: the disks and sockets, removed by destroy.
    pub vm_dir: PathBuf,
    /// `[vm] dir`: the image and downloads, left in place.
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
            .collect();
        projects.dedup();
        let vm_running = vm.running();
        Facts {
            service_active: ui::service_active(),
            service_enabled: ui::service_enabled(),
            desktop_present: ui::desktop_present(),
            bot: cfg.github.login.clone(),
            has_token: config::token_path().exists(),
            key_ids: cfg.github.ssh_key_id.is_some() || cfg.github.signing_key_id.is_some(),
            vm_mode: cfg.vm.enabled,
            vm_name: cfg.vm.name.clone(),
            vm_present: vm.dir.exists() || vm_running,
            vm_running,
            vm_dir: vm.dir.clone(),
            vm_base: vm.base.clone(),
            config_dir: config::config_dir(),
            state_dir: config::state_dir(),
            projects,
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

/// What `sudo pacman -R ssf` is on this machine. Only pacman is known
/// today; #127 (other distributions) extends this with their package
/// managers. Printed, never run: nothing in ssf runs sudo.
pub fn package_removal_command() -> String {
    if ui::which("pacman").is_some() {
        "sudo pacman -R ssf".into()
    } else {
        "remove the ssf package with your package manager".into()
    }
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
            ui::SERVICE,
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
    if facts.vm_running {
        stop.push(format!("VM {}", facts.vm_name));
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
        let why = if facts.vm_mode && !facts.vm_running {
            format!("VM {} is not running", facts.vm_name)
        } else {
            "the daemon is not running".to_string()
        };
        remove.push(format!(
            "workspaces of closed items: not purged ({why}); left in place"
        ));
    }
    if facts.vm_present {
        remove.push(format!(
            "VM {} and its disks ({})",
            facts.vm_name,
            facts.vm_dir.display()
        ));
    } else {
        remove.push("no VM".to_string());
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
    for p in &facts.projects {
        keep.push(format!(
            "{} (clones and worktrees; may hold unpushed work)",
            p.display()
        ));
    }
    if facts.vm_base.exists() {
        keep.push(format!(
            "{} (VM image and downloads; safe to remove)",
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
        out.push_str(&format!(
            "  VM {} is not running: its workspaces (the clones on its data disk) cannot be checked; `ssf vm start` first, or --force destroys them unchecked\n",
            facts.vm_name
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

/// The command: report, hard stops, one question, the steps, what is left.
pub async fn run(yes: bool, force: bool, data: bool) -> Result<()> {
    let cfg = Config::load()?;
    let vm = vm::Vm::new(&cfg);
    let facts = Facts::gather(&cfg, &vm);
    let mut opts = Opts {
        data,
        ..Opts::default()
    };
    let report = if facts.vm_mode {
        if facts.vm_running && vm.ssh_ok() {
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
            opts.vm_unchecked = facts.vm_present;
            Report::default()
        }
    } else {
        report().await
    };
    print!("{}", render(&facts, &report, &opts));

    let unpushed = report.unpushed();
    if !force {
        if !unpushed.is_empty() {
            bail!(
                "{} workspace{} hold{} uncommitted or unpushed work (listed above); push or discard it first, or pass --force to go ahead and leave it where it is",
                unpushed.len(),
                if unpushed.len() == 1 { "" } else { "s" },
                if unpushed.len() == 1 { "s" } else { "" }
            );
        }
        if opts.vm_unchecked {
            bail!(
                "VM {} {}, so its workspaces cannot be checked; `ssf vm start` first, or pass --force to destroy them unchecked",
                facts.vm_name,
                if facts.vm_running {
                    "gave no report"
                } else {
                    "is not running"
                }
            );
        }
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

    println!("==> stop and disable {}", ui::SERVICE);
    let was_off = !facts.service_active && !facts.service_enabled;
    match ui::set_service_enabled(false) {
        Ok(()) if was_off => println!("already stopped and disabled"),
        Ok(()) => println!("service stopped and disabled"),
        Err(e) => fail("service", e),
    }

    println!("==> remove the bar widget and menu entries");
    if let Err(e) = ui::uninstall_all() {
        fail("desktop", e);
    }

    println!("==> sign the bot out");
    if !facts.bot_signed_in() {
        println!("bot not signed in; nothing to revoke");
    } else if let Err(e) = crate::auth_logout(false).await {
        fail("bot", e);
    }

    println!("==> destroy the VM");
    if vm.dir.exists() || vm.running() {
        if let Err(e) = vm.destroy().await {
            fail("vm", e);
        }
    } else {
        println!("no VM");
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
            "  {} (VM image and downloads; safe to remove)",
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
            vm_present: true,
            vm_running: true,
            vm_dir: PathBuf::from("/vm/factory"),
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
            text.contains("stop:    ssf.service (running, enabled) and VM factory"),
            "{text}"
        );
        assert!(
            text.contains("the Factory bar widget and menu entries"),
            "{text}"
        );
        assert!(text.contains("clean and pushed (ssf purge)"), "{text}");
        assert!(
            text.contains("VM factory and its disks (/vm/factory)"),
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
        assert!(text.contains("/c and /s (remove with --data)"), "{text}");
        assert!(!text.contains("(--data)"), "{text}");
        assert!(
            text.contains("closed  o/r#9 \"a title\"  [dirty]  /p/w"),
            "{text}"
        );
        assert!(text.contains("- 1 uncommitted change"), "{text}");
        assert!(!text.contains("(none)"), "{text}");
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
        f.vm_running = false;
        let text = render(&f, &Report::default(), &Opts::default());
        assert!(
            text.contains("not purged (VM factory is not running)"),
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
            vm_present: false,
            vm_running: false,
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
        let text = render(
            &facts(),
            &Report::default(),
            &Opts {
                vm_unchecked: true,
                report_error: Some("could not get the report".into()),
                ..Opts::default()
            },
        );
        assert!(text.contains("could not get the report"), "{text}");
        assert!(
            text.contains("VM factory is not running: its workspaces"),
            "{text}"
        );
        assert!(!text.contains("(none)"), "{text}");
    }

    #[test]
    fn package_removal_is_pacman_where_pacman_is() {
        let cmd = package_removal_command();
        if ui::which("pacman").is_some() {
            assert_eq!(cmd, "sudo pacman -R ssf");
        } else {
            assert!(cmd.contains("package manager"), "{cmd}");
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
