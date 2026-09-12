use super::super::*;
use tracing::warn;

impl Engine {
    pub(in crate::engine) async fn check_conflicts(&mut self, repo: &RepoConfig) -> Result<()> {
        let interval = Duration::from_secs(self.cfg.conflict_check_interval_secs(repo));
        if interval.is_zero() {
            return Ok(());
        }
        if self
            .conflict_checks
            .get(&repo.name)
            .is_some_and(|last| last.elapsed() < interval)
        {
            return Ok(());
        }

        let candidates = self.conflict_candidates(repo).await;
        if candidates.is_empty() {
            return Ok(());
        }
        // Set this before Git work so a failed remote or a locked checkout is
        // retried at the configured cadence rather than on every ten-second
        // daemon pass.
        self.conflict_checks
            .insert(repo.name.clone(), Instant::now());

        let root = self.conflict_repo_root(repo, &candidates).await?;
        let (base_ref, base_sha) = self.conflict_base(&root, repo).await?;
        let base_name = base_ref
            .strip_prefix("refs/remotes/")
            .or_else(|| base_ref.strip_prefix("refs/heads/"))
            .unwrap_or(&base_ref);

        for st in candidates {
            let Some(branch) = st.branch.as_deref().map(normalize_branch) else {
                continue;
            };
            let Some(worktree) = st.worktree_path.as_deref() else {
                continue;
            };
            let Ok(actual) =
                conflict_git(worktree, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await
            else {
                warn!(
                    repo = repo.name,
                    issue = st.number,
                    "could not read the session worktree branch for conflict check"
                );
                continue;
            };
            if normalize_branch(&actual) != branch {
                warn!(
                    repo = repo.name,
                    issue = st.number,
                    expected = branch,
                    actual,
                    "session worktree branch differs from state; skipping conflict check"
                );
                continue;
            }
            let Ok(branch_sha) = conflict_git(worktree, &["rev-parse", "--verify", "HEAD"]).await
            else {
                warn!(
                    repo = repo.name,
                    issue = st.number,
                    "could not read the session branch commit for conflict check"
                );
                continue;
            };
            let key = (repo.name.clone(), branch.clone());
            let pair = match self.conflict_pairs.get(&key) {
                Some(pair) if pair.base_sha == base_sha && pair.branch_sha == branch_sha => {
                    let mut pair = pair.clone();
                    pair.base_ref = base_ref.clone();
                    pair
                }
                _ => match self.simulate_conflict(&root, &base_sha, &branch_sha).await {
                    Ok((conflict, files)) => {
                        let pair = ConflictPair {
                            base_ref: base_ref.clone(),
                            base_sha: base_sha.clone(),
                            branch_sha: branch_sha.clone(),
                            conflict,
                            files,
                        };
                        self.conflict_pairs.insert(key, pair.clone());
                        pair
                    }
                    Err(e) => {
                        warn!(
                            repo = repo.name,
                            issue = st.number,
                            branch,
                            "could not simulate merge for conflict check: {e:#}"
                        );
                        continue;
                    }
                },
            };
            let fingerprint = ConflictNotice {
                base_ref: pair.base_ref.clone(),
                base_sha: pair.base_sha.clone(),
                branch_sha: pair.branch_sha.clone(),
            };
            if !pair.conflict {
                self.entry(repo, st.number).conflict_notice = None;
                continue;
            }
            if self
                .peek(repo, st.number)
                .and_then(|s| s.conflict_notice.as_ref())
                == Some(&fingerprint)
            {
                continue;
            }
            let text = prompt::conflict_prompt(base_name, &base_sha, &pair.files);
            match self.deliver_to(repo, st.number, &text, None).await {
                Ok(d) => {
                    let e = self.entry(repo, st.number);
                    e.terminal_handle = Some(d.handle);
                    e.last_prompt_at = Some(now_iso());
                    e.prompts_sent += 1;
                    e.conflict_notice = Some(fingerprint);
                }
                Err(e) => warn!(
                    repo = repo.name,
                    issue = st.number,
                    "could not tell the session about its branch conflict: {e:#}"
                ),
            }
        }
        Ok(())
    }

    /// Owning sessions are selected before any Git work, so a repository with
    /// only retired, released, blocked, handed-over or bound-child records
    /// does not fetch merely because it remains configured.
    pub(in crate::engine) async fn conflict_candidates(
        &self,
        repo: &RepoConfig,
    ) -> Vec<IssueState> {
        let Some(rs) = self.state.repos.get(&repo.name) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for st in rs.issues.values() {
            if !(st.seeded
                && st.active
                && st.shares_workspace_of.is_none()
                && st.retired_at.is_none()
                && st.released_at.is_none()
                && !st.release_pending
                && !st.cleanup_pending
                && st.handover.is_none()
                && st.blocked.is_none()
                && st.worktree_id.is_some()
                && st.worktree_path.is_some()
                && st.branch.is_some())
            {
                continue;
            }
            let Some(id) = st.worktree_id.as_deref() else {
                continue;
            };
            if self.driver(repo).has_live_agent(id).await.unwrap_or(false) {
                out.push(st.clone());
            }
        }
        out
    }

    pub(in crate::engine) async fn conflict_repo_root(
        &self,
        repo: &RepoConfig,
        candidates: &[IssueState],
    ) -> Result<String> {
        if let Some(path) = repo.path.as_deref() {
            return Ok(path.to_string());
        }
        let st = candidates
            .first()
            .context("eligible conflict-check session has no workspace")?;
        let repo_id = st
            .repo_id
            .as_deref()
            .context("eligible conflict-check session has no repository")?;
        self.driver(repo).repo_path(repo_id).await
    }

    pub(in crate::engine) async fn conflict_base(
        &self,
        root: &str,
        repo: &RepoConfig,
    ) -> Result<(String, String)> {
        let configured = match repo.base_branch.as_deref().map(str::trim) {
            Some(b) if !b.is_empty() => b.to_string(),
            _ => conflict_default_base(root).await?,
        };
        let configured = match configured.as_str() {
            "origin/HEAD" | "refs/remotes/origin/HEAD" => conflict_git(
                root,
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    "refs/remotes/origin/HEAD",
                ],
            )
            .await
            .context("resolving origin/HEAD")?,
            _ => configured,
        };
        let remote_branch = base_remote_branch(&configured)?;
        let refspec = format!("+refs/heads/{remote_branch}:refs/remotes/origin/{remote_branch}");
        conflict_git(root, &["fetch", "--quiet", "origin", &refspec])
            .await
            .with_context(|| {
                format!("fetching origin/{remote_branch} for conflict checks in {root}")
            })?;
        let refs = base_ref_candidates(&configured)?;
        for reference in refs {
            if let Ok(sha) =
                conflict_git(root, &["rev-parse", "--verify", "--quiet", &reference]).await
            {
                return Ok((reference, sha));
            }
        }
        anyhow::bail!("base branch {configured} not found in {root}")
    }

    pub(in crate::engine) async fn simulate_conflict(
        &self,
        root: &str,
        base_sha: &str,
        branch_sha: &str,
    ) -> Result<(bool, Vec<String>)> {
        let args = [
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--messages",
            "-z",
            base_sha,
            branch_sha,
        ];
        let out = conflict_git_status(root, &args).await?;
        if out.status.success() {
            return Ok((false, Vec::new()));
        }
        if out.status.code() != Some(1) {
            anyhow::bail!(
                "git merge-tree failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        if !out.stderr.is_empty() {
            anyhow::bail!(
                "git merge-tree failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let records: Vec<&[u8]> = out.stdout.split(|b| *b == 0).collect();
        let mut files = BTreeSet::new();
        // With --name-only -z, merge-tree puts the merged tree oid first,
        // then the affected paths, then an empty record before its message
        // records. Keeping the raw bytes preserves filenames containing
        // whitespace or newlines and also covers rename/delete conflicts
        // whose prose does not say "Merge conflict in".
        if let Some((_, paths)) = records.split_first() {
            for path in paths.iter().take_while(|p| !p.is_empty()) {
                let file = String::from_utf8_lossy(path);
                if !file.is_empty() {
                    files.insert(file.to_string());
                }
            }
        }
        Ok((true, files.into_iter().collect()))
    }
}
