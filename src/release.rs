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

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch repository: a bare origin and a clone of it on `main`
    /// with one pushed commit. Removed when dropped.
    struct Scratch {
        dir: std::path::PathBuf,
        work: String,
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn sh(path: &str, args: &[&str]) -> String {
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

    async fn scratch(name: &str) -> Scratch {
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
