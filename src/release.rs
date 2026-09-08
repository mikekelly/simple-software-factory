//! Whether a workspace can go without losing work: the checks behind
//! `ssf release` and `ssf purge`, run with git inside the worktree.
//!
//! A workspace is safe to remove when its tree is clean (no modified or
//! untracked files; ignored build artefacts do not count), its branch is on
//! origin with nothing unpushed, and no stash entry was made on it. Anything
//! git cannot answer (a detached head, an unreachable origin, a missing
//! directory) counts as unknown, which is unsafe.

use anyhow::{Context, Result};
use serde_json::{Value, json};

/// Run git in `path` and return its trimmed stdout.
pub async fn git(path: &str, args: &[&str]) -> Result<String> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .await
        .context("running git")?;
    if !out.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// What the worktree looks like.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Check {
    /// Checked-out branch; `None` on a detached head.
    pub branch: Option<String>,
    /// `git status --porcelain` lines: modified, staged and untracked files.
    pub dirty: Vec<String>,
    /// The branch exists on origin.
    pub on_origin: bool,
    /// Commits on the branch that origin does not have.
    pub unpushed: u64,
    /// Stash entries made on the branch.
    pub stashes: Vec<String>,
    /// Origin could not be fetched, so `on_origin`/`unpushed` may be stale.
    pub fetch_error: Option<String>,
}

impl Check {
    /// Nothing would be lost by removing the worktree.
    pub fn safe(&self) -> bool {
        self.problems().is_empty()
    }

    /// One line per reason the worktree cannot go, empty when it can.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.dirty.is_empty() {
            out.push(format!(
                "{} uncommitted change{} (modified or untracked files)",
                self.dirty.len(),
                if self.dirty.len() == 1 { "" } else { "s" }
            ));
        }
        match &self.branch {
            None => out.push("detached HEAD: commits here are on no branch".into()),
            Some(b) => {
                if let Some(e) = &self.fetch_error {
                    out.push(format!("could not fetch origin to compare {b}: {e}"));
                } else if !self.on_origin {
                    out.push(format!("branch {b} is not on origin"));
                } else if self.unpushed > 0 {
                    out.push(format!(
                        "{} commit{} on {b} not pushed to origin",
                        self.unpushed,
                        if self.unpushed == 1 { "" } else { "s" }
                    ));
                }
            }
        }
        if !self.stashes.is_empty() {
            out.push(format!(
                "{} stash entr{} made on this branch",
                self.stashes.len(),
                if self.stashes.len() == 1 { "y" } else { "ies" }
            ));
        }
        out
    }

    /// Short state for listings: `clean and pushed`, `dirty`, `unpushed
    /// commits`, `dirty, unpushed commits`, or `unknown`.
    pub fn state(&self) -> String {
        if self.branch.is_none() || self.fetch_error.is_some() {
            return "unknown".into();
        }
        let mut parts = Vec::new();
        if !self.dirty.is_empty() || !self.stashes.is_empty() {
            parts.push("dirty");
        }
        if !self.on_origin || self.unpushed > 0 {
            parts.push("unpushed commits");
        }
        if parts.is_empty() {
            "clean and pushed".into()
        } else {
            parts.join(", ")
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "state": self.state(),
            "safe": self.safe(),
            "branch": self.branch,
            "dirty": self.dirty,
            "on_origin": self.on_origin,
            "unpushed": self.unpushed,
            "stashes": self.stashes,
            "fetch_error": self.fetch_error,
            "problems": self.problems(),
        })
    }
}

/// Look at the worktree at `path`.
pub async fn inspect(path: &str) -> Result<Check> {
    if !std::path::Path::new(path).is_dir() {
        anyhow::bail!("{path} does not exist");
    }
    git(path, &["rev-parse", "--is-inside-work-tree"])
        .await
        .with_context(|| format!("{path} is not a git worktree"))?;
    let mut check = Check::default();
    let status = git(path, &["status", "--porcelain", "--untracked-files=all"]).await?;
    check.dirty = status
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    check.branch = git(path, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .await
        .ok()
        .filter(|b| !b.is_empty());
    if let Some(branch) = check.branch.clone() {
        if let Err(e) = git(path, &["fetch", "--quiet", "origin", &branch]).await {
            // A branch that was never pushed makes the fetch fail too; that
            // is "not on origin", not an unreachable origin. `ls-remote
            // --exit-code` exits 2 and says nothing when the ref is missing,
            // and complains when origin cannot be reached.
            match git(
                path,
                &["ls-remote", "--exit-code", "--heads", "origin", &branch],
            )
            .await
            {
                Ok(_) => check.fetch_error = Some(e.to_string()),
                Err(le) if !le.to_string().trim_end().ends_with("failed:") => {
                    check.fetch_error = Some(le.to_string())
                }
                Err(_) => {}
            }
        }
        let remote = format!("refs/remotes/origin/{branch}");
        check.on_origin = git(path, &["rev-parse", "--verify", "--quiet", &remote])
            .await
            .is_ok();
        if check.on_origin {
            let n = git(path, &["rev-list", "--count", &format!("{remote}..HEAD")]).await?;
            check.unpushed = n.parse().unwrap_or(0);
        }
        let needle = format!("on {}:", branch.to_lowercase());
        check.stashes = git(path, &["stash", "list", "--format=%gd %gs"])
            .await
            .unwrap_or_default()
            .lines()
            .filter(|l| l.to_lowercase().contains(&needle))
            .map(str::to_string)
            .collect();
    }
    Ok(check)
}

/// A worktree ssf made under `<checkout>.worktrees/`, and what it holds
/// that exists nowhere else. `ssf doctor` reports the ones no agent is on:
/// a workspace closed by hand leaves the checkout behind, and removing
/// that directory (or a purge of it) would lose what is only there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Held {
    pub path: String,
    /// The directory name (`issue-12-...`).
    pub name: String,
    /// Checked-out branch, short; `None` on a detached head.
    pub branch: Option<String>,
    /// Commits not reachable from the base branch.
    pub ahead: u64,
    /// Commits reachable from neither the base branch nor any
    /// remote-tracking branch: this checkout is the only place they are.
    pub lost: u64,
    /// Modified, staged and untracked files.
    pub dirty: usize,
    /// Stash entries made on the branch.
    pub stashes: usize,
}

impl Held {
    /// Removing the checkout would lose something.
    pub fn at_risk(&self) -> bool {
        self.lost > 0 || self.dirty > 0 || self.stashes > 0
    }

    /// `6 commits ahead of master, not on origin; 2 uncommitted changes`.
    pub fn describe(&self, base: &str) -> String {
        let mut parts = Vec::new();
        if self.branch.is_none() {
            parts.push("detached HEAD".to_string());
        }
        if self.ahead > 0 {
            let commits = format!(
                "{} commit{} ahead of {base}",
                self.ahead,
                if self.ahead == 1 { "" } else { "s" }
            );
            parts.push(if self.lost == self.ahead {
                format!("{commits}, not on origin")
            } else if self.lost > 0 {
                format!("{commits}, {} of them not on origin", self.lost)
            } else {
                format!("{commits}, all on origin")
            });
        }
        if self.dirty > 0 {
            parts.push(format!(
                "{} uncommitted change{}",
                self.dirty,
                if self.dirty == 1 { "" } else { "s" }
            ));
        }
        if self.stashes > 0 {
            parts.push(format!(
                "{} stash entr{}",
                self.stashes,
                if self.stashes == 1 { "y" } else { "ies" }
            ));
        }
        if parts.is_empty() {
            format!("nothing beyond {base}")
        } else {
            parts.join("; ")
        }
    }
}

/// What [`held_work`] found under one checkout.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeldReport {
    /// The base branch compared against, short (`master`).
    pub base: String,
    /// Origin could not be fetched first, so the counts may be stale.
    pub fetch_error: Option<String>,
    /// The directory the worktrees were looked for in.
    pub dir: String,
    pub worktrees: Vec<Held>,
}

/// Look at every worktree under `<root>.worktrees/`: what each holds
/// beyond the base branch (`base`, else the checkout's default base) and
/// beyond origin. Origin is fetched once first, so a branch merged since
/// the last fetch does not count as unmerged; a failed fetch is reported,
/// not fatal.
pub async fn held_work(root: &str, base: Option<&str>) -> Result<HeldReport> {
    let root = std::path::Path::new(root)
        .canonicalize()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| root.to_string());
    let fetch_error = git(&root, &["fetch", "--quiet", "origin"])
        .await
        .err()
        .map(|e| e.to_string());
    let base = match base {
        Some(b) => b.to_string(),
        None => crate::driver::default_base(&root).await?,
    };
    let short = base.strip_prefix("origin/").unwrap_or(&base).to_string();
    // Origin's copy of the base is where merges land; the local one only
    // when there is no such copy.
    let remote = format!("refs/remotes/origin/{short}");
    let base_ref = if git(&root, &["rev-parse", "--verify", "--quiet", &remote])
        .await
        .is_ok()
    {
        remote
    } else {
        format!("refs/heads/{short}")
    };
    git(&root, &["rev-parse", "--verify", "--quiet", &base_ref])
        .await
        .with_context(|| format!("base branch {short} not found in {root}"))?;
    let dir = crate::driver::worktrees_dir(&root);
    let mut worktrees = Vec::new();
    for w in crate::driver::local_worktrees(&root).await? {
        let p = std::path::Path::new(&w.path);
        if p.parent() != Some(dir.as_path()) || !p.is_dir() {
            continue;
        }
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let branch = w
            .branch
            .as_deref()
            .map(|b| b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
        let dirty = git(&w.path, &["status", "--porcelain", "--untracked-files=all"])
            .await?
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();
        let ahead = count(
            &w.path,
            &["rev-list", "--count", "HEAD", "--not", &base_ref],
        )
        .await?;
        let lost = count(
            &w.path,
            &[
                "rev-list",
                "--count",
                "HEAD",
                "--not",
                &base_ref,
                "--remotes",
            ],
        )
        .await?;
        let stashes = match &branch {
            Some(b) => {
                let needle = format!("on {}:", b.to_lowercase());
                git(&w.path, &["stash", "list", "--format=%gs"])
                    .await
                    .unwrap_or_default()
                    .lines()
                    .filter(|l| l.to_lowercase().contains(&needle))
                    .count()
            }
            None => 0,
        };
        worktrees.push(Held {
            path: w.path.clone(),
            name,
            branch,
            ahead,
            lost,
            dirty,
            stashes,
        });
    }
    Ok(HeldReport {
        base: short,
        fetch_error,
        dir: dir.to_string_lossy().to_string(),
        worktrees,
    })
}

async fn count(path: &str, args: &[&str]) -> Result<u64> {
    let n = git(path, args).await?;
    n.trim()
        .parse()
        .with_context(|| format!("git {}: not a count: {n:?}", args.join(" ")))
}

/// A scratch repository for tests: a bare origin and a clone of it on
/// `main` with one pushed commit. Removed when dropped.
#[cfg(test)]
pub(crate) mod testkit {
    use super::git;

    pub struct Scratch {
        pub dir: std::path::PathBuf,
        pub work: String,
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Run git in `path` with a fixed identity and no signing.
    pub async fn sh(path: &str, args: &[&str]) -> String {
        let mut full = vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ];
        full.extend_from_slice(args);
        git(path, &full)
            .await
            .unwrap_or_else(|e| panic!("git {args:?}: {e:#}"))
    }

    pub async fn scratch(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("ssf-release-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let origin = dir.join("origin.git");
        let work = dir.join("work");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let origin = origin.to_string_lossy().to_string();
        let work_s = work.to_string_lossy().to_string();
        sh(&origin, &["init", "--bare", "-b", "main", "-q"]).await;
        sh(&work_s, &["init", "-b", "main", "-q"]).await;
        sh(&work_s, &["remote", "add", "origin", &origin]).await;
        std::fs::write(work.join("a.txt"), "a\n").unwrap();
        std::fs::write(work.join(".gitignore"), "target/\n").unwrap();
        sh(&work_s, &["add", "."]).await;
        sh(&work_s, &["commit", "-q", "-m", "one"]).await;
        sh(&work_s, &["push", "-q", "-u", "origin", "main"]).await;
        Scratch { dir, work: work_s }
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;

    /// A worktree of the scratch checkout under `work.worktrees/<name>`,
    /// on branch `bot/<name>` from `main`.
    async fn worktree(s: &Scratch, name: &str) -> String {
        let (path, _) = crate::driver::add_local_worktree(&s.work, name, None)
            .await
            .unwrap();
        path
    }

    #[tokio::test]
    async fn held_work_counts_what_only_the_worktree_has() {
        let s = scratch("held").await;
        // Nothing beyond main yet: nothing at risk.
        let w = worktree(&s, "issue-5-fix").await;
        let r = held_work(&s.work, None).await.unwrap();
        assert_eq!(r.base, "main");
        assert!(r.fetch_error.is_none(), "{:?}", r.fetch_error);
        assert_eq!(r.worktrees.len(), 1);
        let h = &r.worktrees[0];
        assert_eq!(h.name, "issue-5-fix");
        assert_eq!(h.branch.as_deref(), Some("bot/issue-5-fix"));
        assert_eq!((h.ahead, h.lost, h.dirty, h.stashes), (0, 0, 0, 0));
        assert!(!h.at_risk());
        assert_eq!(h.describe(&r.base), "nothing beyond main");
        // Two commits nobody else has.
        std::fs::write(std::path::Path::new(&w).join("b.txt"), "b\n").unwrap();
        sh(&w, &["add", "."]).await;
        sh(&w, &["commit", "-q", "-m", "two"]).await;
        std::fs::write(std::path::Path::new(&w).join("c.txt"), "c\n").unwrap();
        sh(&w, &["add", "."]).await;
        sh(&w, &["commit", "-q", "-m", "three"]).await;
        let h = held_work(&s.work, None).await.unwrap().worktrees.remove(0);
        assert_eq!((h.ahead, h.lost), (2, 2));
        assert!(h.at_risk());
        assert_eq!(h.describe("main"), "2 commits ahead of main, not on origin");
        // Pushed: still ahead of main, but origin has them.
        sh(&w, &["push", "-q", "-u", "origin", "bot/issue-5-fix"]).await;
        let h = held_work(&s.work, None).await.unwrap().worktrees.remove(0);
        assert_eq!((h.ahead, h.lost), (2, 0));
        assert!(!h.at_risk());
        assert_eq!(h.describe("main"), "2 commits ahead of main, all on origin");
        // One more commit on top, plus a dirty file and a stash.
        std::fs::write(std::path::Path::new(&w).join("d.txt"), "d\n").unwrap();
        sh(&w, &["add", "."]).await;
        sh(&w, &["commit", "-q", "-m", "four"]).await;
        std::fs::write(std::path::Path::new(&w).join("a.txt"), "stashed\n").unwrap();
        sh(&w, &["stash", "push", "-q", "-m", "keep"]).await;
        std::fs::write(std::path::Path::new(&w).join("e.txt"), "e\n").unwrap();
        let h = held_work(&s.work, None).await.unwrap().worktrees.remove(0);
        assert_eq!((h.ahead, h.lost, h.dirty, h.stashes), (3, 1, 1, 1));
        assert_eq!(
            h.describe("main"),
            "3 commits ahead of main, 1 of them not on origin; 1 uncommitted change; 1 stash entry"
        );
    }

    #[tokio::test]
    async fn held_work_follows_origin_for_merges_and_skips_other_worktrees() {
        let s = scratch("held-merge").await;
        let w = worktree(&s, "issue-6-x").await;
        std::fs::write(std::path::Path::new(&w).join("b.txt"), "b\n").unwrap();
        sh(&w, &["add", "."]).await;
        sh(&w, &["commit", "-q", "-m", "two"]).await;
        assert_eq!(held_work(&s.work, None).await.unwrap().worktrees[0].lost, 1);
        // Merged into main on origin by someone else (a second clone):
        // the fetch sees it and the commit is no longer only here.
        let other = s.dir.join("other");
        let other_s = other.to_string_lossy().to_string();
        let origin = s.dir.join("origin.git").to_string_lossy().to_string();
        sh(
            &s.dir.to_string_lossy(),
            &["clone", "-q", &origin, &other_s],
        )
        .await;
        sh(&w, &["push", "-q", "origin", "bot/issue-6-x"]).await;
        sh(&other_s, &["fetch", "-q", "origin"]).await;
        sh(
            &other_s,
            &["merge", "-q", "--ff-only", "origin/bot/issue-6-x"],
        )
        .await;
        sh(&other_s, &["push", "-q", "origin", "main"]).await;
        sh(&origin, &["branch", "-D", "bot/issue-6-x"]).await;
        let r = held_work(&s.work, None).await.unwrap();
        let h = &r.worktrees[0];
        assert_eq!((h.ahead, h.lost), (0, 0), "{h:?}");
        assert_eq!(h.describe(&r.base), "nothing beyond main");
        // A worktree somewhere else is not ssf's to report; a detached
        // one under the directory is, with its commits on no branch.
        let elsewhere = s.dir.join("elsewhere").to_string_lossy().to_string();
        sh(&s.work, &["worktree", "add", "-q", "--detach", &elsewhere]).await;
        let detached = std::path::Path::new(&r.dir).join("pr-9");
        let detached_s = detached.to_string_lossy().to_string();
        sh(&s.work, &["worktree", "add", "-q", "--detach", &detached_s]).await;
        std::fs::write(detached.join("z.txt"), "z\n").unwrap();
        sh(&detached_s, &["add", "."]).await;
        sh(&detached_s, &["commit", "-q", "-m", "loose"]).await;
        let r = held_work(&s.work, Some("main")).await.unwrap();
        let names: Vec<&str> = r.worktrees.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["issue-6-x", "pr-9"]);
        let d = &r.worktrees[1];
        assert!(d.branch.is_none());
        assert!(d.at_risk());
        assert_eq!(
            d.describe("main"),
            "detached HEAD; 1 commit ahead of main, not on origin"
        );
        // An unknown base is an error, not a silent zero.
        assert!(held_work(&s.work, Some("nope")).await.is_err());
    }

    #[tokio::test]
    async fn a_clean_pushed_tree_is_safe_and_ignored_files_do_not_count() {
        let s = scratch("clean").await;
        std::fs::create_dir_all(std::path::Path::new(&s.work).join("target")).unwrap();
        std::fs::write(std::path::Path::new(&s.work).join("target/out.o"), "x").unwrap();
        let c = inspect(&s.work).await.unwrap();
        assert_eq!(c.branch.as_deref(), Some("main"));
        assert!(c.dirty.is_empty(), "{:?}", c.dirty);
        assert!(c.on_origin);
        assert_eq!(c.unpushed, 0);
        assert!(c.stashes.is_empty());
        assert!(c.fetch_error.is_none());
        assert!(c.safe());
        assert_eq!(c.state(), "clean and pushed");
        assert!(c.problems().is_empty());
    }

    #[tokio::test]
    async fn modified_and_untracked_files_make_it_dirty() {
        let s = scratch("dirty").await;
        std::fs::write(std::path::Path::new(&s.work).join("a.txt"), "changed\n").unwrap();
        std::fs::write(std::path::Path::new(&s.work).join("new.txt"), "n\n").unwrap();
        let c = inspect(&s.work).await.unwrap();
        assert_eq!(c.dirty.len(), 2);
        assert!(!c.safe());
        assert_eq!(c.state(), "dirty");
        assert_eq!(c.problems().len(), 1);
        assert!(c.problems()[0].contains("2 uncommitted changes"));
    }

    #[tokio::test]
    async fn unpushed_commits_and_unpushed_branches_are_reported() {
        let s = scratch("unpushed").await;
        std::fs::write(std::path::Path::new(&s.work).join("a.txt"), "two\n").unwrap();
        sh(&s.work, &["commit", "-q", "-am", "two"]).await;
        let c = inspect(&s.work).await.unwrap();
        assert!(c.dirty.is_empty());
        assert!(c.on_origin);
        assert_eq!(c.unpushed, 1);
        assert!(!c.safe());
        assert_eq!(c.state(), "unpushed commits");
        assert!(c.problems()[0].contains("1 commit on main not pushed"));
        // Pushing settles it.
        sh(&s.work, &["push", "-q", "origin", "main"]).await;
        assert!(inspect(&s.work).await.unwrap().safe());
        // A branch origin has never seen.
        sh(&s.work, &["checkout", "-q", "-b", "feature"]).await;
        let c = inspect(&s.work).await.unwrap();
        assert_eq!(c.branch.as_deref(), Some("feature"));
        assert!(!c.on_origin);
        assert!(c.fetch_error.is_none(), "{:?}", c.fetch_error);
        assert!(!c.safe());
        assert_eq!(c.state(), "unpushed commits");
        assert!(c.problems()[0].contains("feature is not on origin"));
        // Dirty on top of that.
        std::fs::write(std::path::Path::new(&s.work).join("x.txt"), "x\n").unwrap();
        assert_eq!(
            inspect(&s.work).await.unwrap().state(),
            "dirty, unpushed commits"
        );
    }

    #[tokio::test]
    async fn stashes_on_the_branch_count_and_others_do_not() {
        let s = scratch("stash").await;
        std::fs::write(std::path::Path::new(&s.work).join("a.txt"), "stashed\n").unwrap();
        sh(&s.work, &["stash", "push", "-q", "-m", "keep this"]).await;
        let c = inspect(&s.work).await.unwrap();
        assert!(c.dirty.is_empty());
        assert_eq!(c.stashes.len(), 1, "{:?}", c.stashes);
        assert!(!c.safe());
        assert_eq!(c.state(), "dirty");
        assert!(c.problems()[0].contains("1 stash entry"));
        // The same stash seen from another branch (the stash list is
        // shared) is not this branch's problem.
        sh(&s.work, &["checkout", "-q", "-b", "other"]).await;
        sh(&s.work, &["push", "-q", "-u", "origin", "other"]).await;
        let c = inspect(&s.work).await.unwrap();
        assert!(c.stashes.is_empty());
        assert!(c.safe());
    }

    #[tokio::test]
    async fn detached_head_missing_dir_and_unreachable_origin_are_unknown() {
        let s = scratch("unknown").await;
        sh(&s.work, &["checkout", "-q", "--detach"]).await;
        let c = inspect(&s.work).await.unwrap();
        assert!(c.branch.is_none());
        assert_eq!(c.state(), "unknown");
        assert!(c.problems()[0].contains("detached HEAD"));
        sh(&s.work, &["checkout", "-q", "main"]).await;
        // Origin gone: the comparison cannot be trusted.
        std::fs::remove_dir_all(s.dir.join("origin.git")).unwrap();
        let c = inspect(&s.work).await.unwrap();
        assert!(c.fetch_error.is_some());
        assert_eq!(c.state(), "unknown");
        assert!(!c.safe());
        assert!(c.problems()[0].contains("could not fetch origin"));
        let missing = s.dir.join("nope").to_string_lossy().to_string();
        assert!(inspect(&missing).await.is_err());
        let plain = s.dir.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(inspect(&plain.to_string_lossy()).await.is_err());
    }
}
