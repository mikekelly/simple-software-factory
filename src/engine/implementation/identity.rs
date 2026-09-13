use super::super::*;
use tracing::{info, warn};

impl Engine {
    /// Resolve immutable GitHub repository ids to their current canonical
    /// names before a normal pass. Missing ids are enrolled once; established
    /// ids are checked on a separate, bounded cadence.
    pub(in crate::engine) async fn reconcile_repo_identities(&mut self, force: bool) {
        let needs_enrolment = self.cfg.repos.iter().any(|r| r.github_id.is_none());
        if !force
            && !needs_enrolment
            && self
                .identity_checked_at
                .is_some_and(|at| at.elapsed() < IDENTITY_CHECK_INTERVAL)
        {
            return;
        }
        self.identity_checked_at = Some(Instant::now());

        let configured = self.cfg.repos.clone();
        let mut changed = false;
        for repo in configured {
            let identity = match repo.github_id {
                Some(id) => self.gh.repository_by_id(id).await,
                None => match repo.split() {
                    Ok((owner, name)) => self.gh.repository(owner, name).await,
                    Err(e) => Err(e),
                },
            };
            let identity = match identity {
                Ok(identity) => identity,
                Err(e) => {
                    warn!(repo = repo.name, "repository identity check failed: {e:#}");
                    continue;
                }
            };
            if let Err(e) = self.apply_repository_identity(&repo, &identity).await {
                warn!(repo = repo.name, "repository identity repair failed: {e:#}");
            } else if repo.github_id != Some(identity.id)
                || !repo.name.eq_ignore_ascii_case(&identity.full_name)
            {
                changed = true;
            }
        }
        if changed {
            if let Err(e) = self.state.save() {
                warn!("saving repository identity state: {e:#}");
                return;
            }
            if let Err(e) = self.cfg.save() {
                warn!("saving repaired repository configuration: {e:#}");
            }
        }
    }

    async fn apply_repository_identity(
        &mut self,
        old: &RepoConfig,
        identity: &RepositoryIdentity,
    ) -> Result<()> {
        let canonical = identity.full_name.trim();
        crate::config::split_repo_name(canonical)?;
        if let Some(other) = self
            .cfg
            .repos
            .iter()
            .find(|r| !r.name.eq_ignore_ascii_case(&old.name) && r.matches_name(canonical))
        {
            anyhow::bail!(
                "GitHub says {} is now {canonical}, which is already configured as {}",
                old.name,
                other.name
            );
        }

        let renamed = !old.name.eq_ignore_ascii_case(canonical);
        if renamed || !old.aliases.is_empty() {
            self.update_checkout_origins(old, identity).await;
        }
        if renamed {
            self.state.rename_repo(&old.name, canonical)?;
            self.rename_runtime_keys(&old.name, canonical);
        }

        let Some(repo) = self
            .cfg
            .repos
            .iter_mut()
            .find(|r| r.name.eq_ignore_ascii_case(&old.name))
        else {
            anyhow::bail!(
                "{} disappeared from the configuration during repair",
                old.name
            );
        };
        repo.github_id = Some(identity.id);
        if renamed {
            if !repo
                .aliases
                .iter()
                .any(|a| a.eq_ignore_ascii_case(&old.name))
            {
                repo.aliases.push(old.name.clone());
            }
            repo.name = canonical.to_string();
            if repo.clone_url.as_deref().is_some_and(|url| {
                points_to_name(url, &old.name) || old.aliases.iter().any(|a| points_to_name(url, a))
            }) {
                repo.clone_url = Some(url_for_style(
                    repo.clone_url.as_deref().unwrap_or_default(),
                    identity,
                ));
            }
            info!(
                old = old.name,
                new = canonical,
                id = identity.id,
                "repository identity repaired"
            );
        } else if old.github_id.is_none() {
            info!(
                repo = canonical,
                id = identity.id,
                "repository identity enrolled"
            );
        }
        Ok(())
    }

    async fn update_checkout_origins(&self, repo: &RepoConfig, identity: &RepositoryIdentity) {
        if repo.path.is_some() {
            return;
        }
        let mut ids = BTreeSet::new();
        if let Some(state) = self.state.repos.get(&repo.name) {
            ids.extend(state.issues.values().filter_map(|i| i.repo_id.clone()));
        }
        for id in ids {
            let root = match self.driver(repo).repo_path(&id).await {
                Ok(root) => root,
                Err(e) => {
                    warn!(
                        repo = repo.name,
                        repo_id = id,
                        "cannot locate checkout while repairing rename: {e:#}"
                    );
                    continue;
                }
            };
            if let Err(e) = update_origin(&root, repo, identity).await {
                warn!(
                    repo = repo.name,
                    path = root,
                    "cannot repair checkout origin after repository rename: {e:#}"
                );
            }
        }
    }

    fn rename_runtime_keys(&mut self, old: &str, new: &str) {
        self.collaborators.remove(old);
        if self.refetch.remove(old) {
            self.refetch.insert(new.to_string());
        }
        self.failures = std::mem::take(&mut self.failures)
            .into_iter()
            .map(|((repo, n), count)| ((renamed_key(repo, old, new), n), count))
            .collect();
        self.conflict_checks = std::mem::take(&mut self.conflict_checks)
            .into_iter()
            .map(|(repo, at)| (renamed_key(repo, old, new), at))
            .collect();
        self.conflict_pairs = std::mem::take(&mut self.conflict_pairs)
            .into_iter()
            .map(|((repo, branch), pair)| ((renamed_key(repo, old, new), branch), pair))
            .collect();
    }
}

async fn update_origin(
    root: &str,
    repo: &RepoConfig,
    identity: &RepositoryIdentity,
) -> Result<bool> {
    let current = conflict_git(root, &["remote", "get-url", "origin"]).await?;
    if !points_to_name(&current, &repo.name)
        && !repo.aliases.iter().any(|a| points_to_name(&current, a))
    {
        return Ok(false);
    }
    let next = url_for_style(&current, identity);
    conflict_git(root, &["remote", "set-url", "origin", &next]).await?;
    Ok(true)
}

fn renamed_key(value: String, old: &str, new: &str) -> String {
    if value.eq_ignore_ascii_case(old) {
        new.to_string()
    } else {
        value
    }
}

fn points_to_name(url: &str, name: &str) -> bool {
    let clean = url.trim().trim_end_matches('/').trim_end_matches(".git");
    clean.ends_with(&format!("/{name}")) || clean.ends_with(&format!(":{name}"))
}

fn url_for_style(current: &str, identity: &RepositoryIdentity) -> String {
    if current.starts_with("git@") || current.starts_with("ssh://") {
        identity.ssh_url.clone()
    } else {
        identity.clone_url.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_urls_match_only_the_whole_old_name_and_keep_transport() {
        let id = RepositoryIdentity {
            id: 7,
            full_name: "new/place".into(),
            clone_url: "https://github.com/new/place.git".into(),
            ssh_url: "git@github.com:new/place.git".into(),
        };
        assert!(points_to_name(
            "https://github.com/old/place.git",
            "old/place"
        ));
        assert!(!points_to_name(
            "https://github.com/not-old/place.git",
            "old/place"
        ));
        assert_eq!(
            url_for_style("git@github.com:old/place.git", &id),
            id.ssh_url
        );
        assert_eq!(
            url_for_style("https://github.com/old/place.git", &id),
            id.clone_url
        );
    }

    #[tokio::test]
    async fn managed_checkout_origin_moves_to_the_canonical_name() {
        use crate::release::testkit::{scratch, sh};

        let checkout = scratch("identity-origin").await;
        sh(
            &checkout.work,
            &[
                "remote",
                "set-url",
                "origin",
                "git@github.com:old/place.git",
            ],
        )
        .await;
        let repo = RepoConfig {
            name: "new/place".into(),
            aliases: vec!["old/place".into()],
            ..Default::default()
        };
        let identity = RepositoryIdentity {
            id: 7,
            full_name: "new/place".into(),
            clone_url: "https://github.com/new/place.git".into(),
            ssh_url: "git@github.com:new/place.git".into(),
        };

        assert!(
            update_origin(&checkout.work, &repo, &identity)
                .await
                .unwrap()
        );
        assert_eq!(
            conflict_git(&checkout.work, &["remote", "get-url", "origin"])
                .await
                .unwrap(),
            identity.ssh_url
        );
    }
}
