//! The driver boundary: everything the engine asks of whatever runs the
//! agents. Orca was the only such thing; now it is one of two, chosen per
//! repository (`driver = "orca" | "herdr"`).
//!
//! A driver owns three things. A *project*: a checkout of the repository on
//! this machine (Orca keeps its own registry of those; the others clone
//! into `projects_dir`). A *workspace* per item: a git worktree on the
//! item's branch, with an id the engine stores and hands back. And the
//! *agent* in it: started with a command, given prompts, asked whether it
//! is alive or busy. The engine never looks behind the ids.
//!
//! The git side that the non-Orca drivers share (clone, worktree add and
//! remove, branch lookup) lives here too.

use anyhow::{Context, Result, anyhow, bail};
use std::path::{Path, PathBuf};
use tracing::info;

use crate::config::{Config, DriverKind};
use crate::herdr::Herdr;
use crate::orca::{Delivery, Orca, ProjectSetup, WorkspaceInfo, Worktree};
use crate::release::git;

/// One configured driver.
#[derive(Clone)]
pub enum Driver {
    Orca(Orca),
    Herdr(Herdr),
}

/// How a prompt is delivered when the agent has to be started again: the
/// commands to start fresh or to resume, and what to send in each case.
pub struct Relaunch<'a> {
    /// Command that starts the harness from scratch.
    pub command: &'a str,
    /// Command that resumes the harness's earlier conversation, if one is
    /// known and the harness can.
    pub resume_command: Option<&'a str>,
    pub harness: &'a str,
    pub title: &'a str,
    /// What a fresh harness gets instead of the prompt (the whole story).
    pub text: Option<&'a str>,
}

impl Driver {
    pub fn new(kind: DriverKind, cfg: &Config) -> Self {
        match kind {
            DriverKind::Orca => Driver::Orca(Orca::new(cfg.orca.clone())),
            DriverKind::Herdr => Driver::Herdr(Herdr::new(cfg.herdr.clone())),
        }
    }

    pub fn kind(&self) -> DriverKind {
        match self {
            Driver::Orca(_) => DriverKind::Orca,
            Driver::Herdr(_) => DriverKind::Herdr,
        }
    }

    pub fn label(&self) -> &'static str {
        self.kind().label()
    }

    /// The executable the driver runs.
    pub fn command(&self) -> &str {
        match self {
            Driver::Orca(d) => d.command(),
            Driver::Herdr(d) => d.command(),
        }
    }

    /// Is the driver there and ready to take commands?
    pub async fn status(&self) -> Result<()> {
        match self {
            Driver::Orca(d) => d.status().await.map(|_| ()),
            Driver::Herdr(d) => d.status().await,
        }
    }

    /// Make sure the repository has a checkout to make workspaces from,
    /// cloning it or importing `existing_path` when it has none.
    pub async fn ensure_project(
        &self,
        owner: &str,
        repo: &str,
        clone_url: &str,
        existing_path: Option<&str>,
        projects_dir: &Path,
    ) -> Result<ProjectSetup> {
        match self {
            Driver::Orca(d) => {
                d.ensure_project(owner, repo, clone_url, existing_path, projects_dir)
                    .await
            }
            Driver::Herdr(_) => {
                ensure_local_checkout(repo, clone_url, existing_path, projects_dir).await
            }
        }
    }

    /// The workspace already bound to item `number`, if the driver can tell.
    pub async fn find_worktree_for_issue(
        &self,
        repo_id: &str,
        number: u64,
    ) -> Result<Option<Worktree>> {
        match self {
            Driver::Orca(d) => d.find_worktree_for_issue(repo_id, number).await,
            Driver::Herdr(d) => d.find_worktree_for_issue(repo_id, number).await,
        }
    }

    /// Create the workspace for an item: a checkout named `name` on a branch
    /// of its own, from `base_branch` (the driver's default base without).
    pub async fn create_worktree(
        &self,
        repo_id: &str,
        name: &str,
        number: u64,
        comment: &str,
        base_branch: Option<&str>,
    ) -> Result<Worktree> {
        match self {
            Driver::Orca(d) => {
                d.create_worktree(repo_id, name, number, None, comment, base_branch)
                    .await
            }
            Driver::Herdr(d) => d.create_worktree(repo_id, name, comment, base_branch).await,
        }
    }

    /// Filesystem path of the repository's main checkout.
    pub async fn repo_path(&self, repo_id: &str) -> Result<String> {
        match self {
            Driver::Orca(d) => d.repo_path(repo_id).await,
            Driver::Herdr(_) => Ok(repo_root(repo_id).to_string()),
        }
    }

    /// A ref to base a re-created workspace on: the local branch if it still
    /// exists, else its remote-tracking copy.
    pub async fn existing_branch_ref(&self, repo_id: &str, branch: &str) -> Result<Option<String>> {
        let path = self.repo_path(repo_id).await?;
        existing_branch_ref_at(&path, branch).await
    }

    /// Start the harness in a workspace that has no agent yet and give it
    /// its first prompt. Returns the handle later prompts go to.
    pub async fn start(
        &self,
        worktree_id: &str,
        command: &str,
        title: &str,
        harness: &str,
        text: &str,
    ) -> Result<String> {
        match self {
            Driver::Orca(d) => {
                let handle = d
                    .launch_in_worktree(worktree_id, command, title, harness)
                    .await?;
                d.send_prompt(&handle, text).await?;
                Ok(handle)
            }
            Driver::Herdr(d) => {
                let handle = d.launch(worktree_id, command, title, harness).await?;
                d.send_prompt(&handle, text).await?;
                Ok(handle)
            }
        }
    }

    /// Whether the workspace still exists.
    pub async fn worktree_exists(&self, worktree_id: &str) -> Result<bool> {
        match self {
            Driver::Orca(d) => d.worktree_exists(worktree_id).await,
            Driver::Herdr(d) => d.worktree_exists(worktree_id).await,
        }
    }

    /// Every workspace the driver knows about, with the agents in it.
    pub async fn ps(&self) -> Result<Vec<WorkspaceInfo>> {
        match self {
            Driver::Orca(d) => d.ps().await,
            Driver::Herdr(d) => d.ps().await,
        }
    }

    /// Is the agent in this workspace still busy? (Unknown counts as not.)
    pub async fn agent_busy(&self, worktree_id: &str) -> Result<bool> {
        match self {
            Driver::Orca(d) => d.agent_busy(worktree_id).await,
            Driver::Herdr(d) => d.agent_busy(worktree_id).await,
        }
    }

    /// Stop the workspace's agent and remove the workspace.
    pub async fn remove_worktree(&self, worktree_id: &str) -> Result<()> {
        match self {
            Driver::Orca(d) => d.remove_worktree(worktree_id).await,
            Driver::Herdr(d) => d.remove_worktree(worktree_id).await,
        }
    }

    /// A note on the workspace for people looking at the driver's UI.
    pub async fn set_comment(&self, worktree_id: &str, comment: &str) -> Result<()> {
        match self {
            Driver::Orca(d) => d.set_comment(worktree_id, comment).await,
            Driver::Herdr(d) => d.set_comment(worktree_id, comment).await,
        }
    }

    /// Board column (`in-progress`, `completed`) where the driver has one.
    pub async fn set_status(&self, worktree_id: &str, status: &str) -> Result<()> {
        match self {
            Driver::Orca(d) => d.set_status(worktree_id, status).await,
            Driver::Herdr(d) => d.set_status(worktree_id, status).await,
        }
    }

    /// Is there an agent in the workspace that a prompt would reach without
    /// starting one?
    pub async fn has_live_agent(&self, worktree_id: &str) -> Result<bool> {
        match self {
            Driver::Orca(d) => d.has_live_agent(worktree_id).await,
            Driver::Herdr(d) => d.has_live_agent(worktree_id).await,
        }
    }

    /// Deliver a prompt to the workspace's agent, starting the harness again
    /// (resuming its conversation when it can) if it is gone.
    pub async fn deliver(
        &self,
        worktree_id: &str,
        preferred_handle: Option<&str>,
        relaunch: Relaunch<'_>,
        text: &str,
    ) -> Result<Delivery> {
        match self {
            Driver::Orca(d) => {
                d.deliver(
                    worktree_id,
                    preferred_handle,
                    relaunch.command,
                    relaunch.resume_command,
                    relaunch.harness,
                    relaunch.title,
                    text,
                    relaunch.text,
                )
                .await
            }
            Driver::Herdr(d) => {
                d.deliver(worktree_id, preferred_handle, &relaunch, text)
                    .await
            }
        }
    }
}

/// The drivers a daemon runs, one per kind in use.
#[derive(Clone)]
pub struct Drivers {
    list: Vec<Driver>,
}

impl Drivers {
    pub fn from_config(cfg: &Config) -> Self {
        let list = cfg
            .drivers_in_use()
            .into_iter()
            .map(|k| Driver::new(k, cfg))
            .collect();
        Self { list }
    }

    /// For tests: a set with the given drivers.
    #[cfg(test)]
    pub fn from_list(list: Vec<Driver>) -> Self {
        Self { list }
    }

    /// The kinds in the set, in `DriverKind` order.
    pub fn kinds(&self) -> Vec<DriverKind> {
        let mut out: Vec<DriverKind> = self.list.iter().map(Driver::kind).collect();
        out.sort();
        out.dedup();
        out
    }

    pub fn get(&self, kind: DriverKind) -> Option<&Driver> {
        self.list.iter().find(|d| d.kind() == kind)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Driver> {
        self.list.iter()
    }
}

// ---- the git side shared by the drivers that keep checkouts themselves ----

/// The checkout a repo id names (a plain path for the non-Orca drivers;
/// tolerant of Orca's `<repo>::<path>` form).
pub fn repo_root(id: &str) -> &str {
    id.split_once("::").map(|(r, _)| r).unwrap_or(id)
}

/// Directory a checkout's worktrees go in: next to it, as `<name>.worktrees`.
pub fn worktrees_dir(repo_root: &str) -> PathBuf {
    let root = Path::new(repo_root);
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".into());
    root.parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(format!("{name}.worktrees"))
}

/// Branch a workspace named `name` works on.
pub fn branch_for(name: &str) -> String {
    format!("bot/{name}")
}

/// The item number a workspace name (`issue-12-...`, `pr-12`, `review-12-...`)
/// was made for, and whether it is a reviewer's.
pub fn number_of_name(name: &str) -> Option<(u64, bool)> {
    let (prefix, rest) = name.split_once('-')?;
    let reviewer = match prefix {
        "issue" | "pr" => false,
        "review" => true,
        _ => return None,
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let after = &rest[digits.len()..];
    if digits.is_empty() || !(after.is_empty() || after.starts_with('-')) {
        return None;
    }
    Some((digits.parse().ok()?, reviewer))
}

/// A clone of the repository under `projects_dir` (or `existing_path`),
/// made if missing. The repo id is the checkout path.
pub async fn ensure_local_checkout(
    repo: &str,
    clone_url: &str,
    existing_path: Option<&str>,
    projects_dir: &Path,
) -> Result<ProjectSetup> {
    let path = match existing_path {
        Some(p) => PathBuf::from(p),
        None => projects_dir.join(repo),
    };
    if !path.join(".git").exists() {
        if existing_path.is_some() {
            bail!("{} is not a git checkout", path.display());
        }
        std::fs::create_dir_all(projects_dir)
            .with_context(|| format!("creating {}", projects_dir.display()))?;
        info!(clone_url, dest = %path.display(), "cloning repository");
        let out = tokio::process::Command::new("git")
            .args(["clone", clone_url])
            .arg(&path)
            .output()
            .await
            .context("running git clone")?;
        if !out.status.success() {
            bail!(
                "git clone {clone_url} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    let path = path
        .canonicalize()
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    Ok(ProjectSetup {
        repo_id: path.clone(),
        path,
    })
}

/// The local branch if it exists, else its remote-tracking copy.
pub async fn existing_branch_ref_at(path: &str, branch: &str) -> Result<Option<String>> {
    for (full, short) in [
        (format!("refs/heads/{branch}"), branch.to_string()),
        (
            format!("refs/remotes/origin/{branch}"),
            format!("origin/{branch}"),
        ),
    ] {
        if git(path, &["rev-parse", "--verify", "--quiet", &full])
            .await
            .is_ok()
        {
            return Ok(Some(short));
        }
    }
    Ok(None)
}

/// The repository's default base ref: `origin/HEAD` if known, else the
/// checked-out branch of the main checkout.
pub async fn default_base(repo_root: &str) -> Result<String> {
    if let Ok(r) = git(
        repo_root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .await
        && !r.is_empty()
    {
        return Ok(r);
    }
    let b = git(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    if b.is_empty() || b == "HEAD" {
        bail!("cannot tell the base branch of {repo_root}");
    }
    Ok(b)
}

/// A local worktree named `name` of the checkout at `repo_root`, on branch
/// `bot/<name>`: the branch is made from `base` when it does not exist yet,
/// and checked out as it is when it does (a re-created workspace keeps its
/// history). Returns the worktree's path and full branch ref.
pub async fn add_local_worktree(
    repo_root: &str,
    name: &str,
    base: Option<&str>,
) -> Result<(String, String)> {
    let dir = worktrees_dir(repo_root);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(name);
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    let branch = branch_for(name);
    let path_s = path.to_string_lossy().to_string();
    let _ = git(repo_root, &["worktree", "prune"]).await;
    let have_branch = git(
        repo_root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .await
    .is_ok();
    if have_branch && base.is_none_or(|b| b == branch) {
        git(repo_root, &["worktree", "add", &path_s, &branch]).await?;
    } else {
        let base = match base {
            Some(b) => b.to_string(),
            None => default_base(repo_root).await?,
        };
        if let Some(remote) = base.strip_prefix("origin/") {
            let _ = git(repo_root, &["fetch", "origin", remote]).await;
        }
        let flag = if have_branch { "-B" } else { "-b" };
        git(
            repo_root,
            &["worktree", "add", flag, &branch, &path_s, &base],
        )
        .await?;
    }
    Ok((path_s, format!("refs/heads/{branch}")))
}

/// Remove a local worktree (its branch stays).
pub async fn remove_local_worktree(repo_root: &str, path: &str) -> Result<()> {
    if Path::new(path).exists() {
        git(repo_root, &["worktree", "remove", "--force", path]).await?;
    }
    let _ = git(repo_root, &["worktree", "prune"]).await;
    Ok(())
}

/// A worktree of the checkout, as `git worktree list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorktree {
    pub path: String,
    /// Full ref (`refs/heads/...`), or `None` when detached.
    pub branch: Option<String>,
}

/// Parse `git worktree list --porcelain`.
pub fn parse_worktree_list(text: &str) -> Vec<LocalWorktree> {
    let mut out = Vec::new();
    let mut cur: Option<LocalWorktree> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(c) = cur.take() {
                out.push(c);
            }
            cur = Some(LocalWorktree {
                path: p.to_string(),
                branch: None,
            });
        } else if let Some(b) = line.strip_prefix("branch ")
            && let Some(c) = cur.as_mut()
        {
            c.branch = Some(b.to_string());
        }
    }
    if let Some(c) = cur {
        out.push(c);
    }
    out
}

/// The linked worktrees of the checkout (not the checkout itself).
pub async fn local_worktrees(repo_root: &str) -> Result<Vec<LocalWorktree>> {
    let text = git(repo_root, &["worktree", "list", "--porcelain"]).await?;
    let root = Path::new(repo_root)
        .canonicalize()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| repo_root.to_string());
    Ok(parse_worktree_list(&text)
        .into_iter()
        .filter(|w| w.path != root && w.path != repo_root)
        .collect())
}

/// The worktree made for item `number` under `repo_root`, by its name.
pub async fn find_local_worktree(
    repo_root: &str,
    number: u64,
    reviewer: bool,
) -> Result<Option<LocalWorktree>> {
    let dir = worktrees_dir(repo_root);
    for w in local_worktrees(repo_root).await? {
        let p = Path::new(&w.path);
        if p.parent() != Some(dir.as_path()) {
            continue;
        }
        let name = p.file_name().map(|n| n.to_string_lossy().to_string());
        if let Some((n, r)) = name.as_deref().and_then(number_of_name)
            && n == number
            && r == reviewer
        {
            return Ok(Some(w));
        }
    }
    Ok(None)
}

pub fn err_no_field(what: &str, v: &serde_json::Value) -> anyhow::Error {
    anyhow!("{what}: {v}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_root_tolerates_orca_ids() {
        assert_eq!(repo_root("/p/widgets::w7"), "/p/widgets");
        assert_eq!(repo_root("/p/widgets"), "/p/widgets");
    }

    #[test]
    fn worktrees_go_next_to_the_checkout() {
        assert_eq!(
            worktrees_dir("/p/widgets"),
            PathBuf::from("/p/widgets.worktrees")
        );
        assert_eq!(branch_for("issue-3-x"), "bot/issue-3-x");
    }

    #[test]
    fn names_tell_their_item() {
        assert_eq!(number_of_name("issue-12"), Some((12, false)));
        assert_eq!(number_of_name("issue-12-fix-it"), Some((12, false)));
        assert_eq!(number_of_name("pr-7-x"), Some((7, false)));
        assert_eq!(number_of_name("review-7-x"), Some((7, true)));
        assert_eq!(number_of_name("issue-12x"), None);
        assert_eq!(number_of_name("scratch"), None);
        assert_eq!(number_of_name("issue-"), None);
    }

    #[test]
    fn parses_git_worktree_list() {
        let text = "worktree /p/widgets\nHEAD abc\nbranch refs/heads/master\n\n\
worktree /p/widgets.worktrees/issue-3\nHEAD def\nbranch refs/heads/bot/issue-3\n\n\
worktree /p/widgets.worktrees/tmp\nHEAD 123\ndetached\n";
        let list = parse_worktree_list(text);
        assert_eq!(list.len(), 3);
        assert_eq!(list[1].path, "/p/widgets.worktrees/issue-3");
        assert_eq!(list[1].branch.as_deref(), Some("refs/heads/bot/issue-3"));
        assert_eq!(list[2].branch, None);
    }
}
