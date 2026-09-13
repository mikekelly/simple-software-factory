use super::super::*;
use tracing::{info, warn};

impl Engine {
    /// Resolve immutable GitHub repository ids to their current canonical
    /// names before a normal pass. Missing ids are enrolled once; established
    /// ids are checked on a separate, bounded cadence.
    pub(in crate::engine) async fn reconcile_repo_identities(&mut self, force: bool) -> bool {
        if !force
            && self
                .identity_checked_at
                .is_some_and(|at| at.elapsed() < IDENTITY_CHECK_INTERVAL)
        {
            return true;
        }
        self.identity_checked_at = Some(Instant::now());

        let snapshot = IdentitySnapshot::capture(self);
        let configured = self.cfg.repos.clone();
        let mut resolved = Vec::new();
        let mut config_changed = false;
        let mut state_changed = false;
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
            match self.apply_repository_identity(&repo, &identity) {
                Ok(changes) => {
                    config_changed |= changes.config;
                    state_changed |= changes.state;
                    resolved.push((repo, identity, changes));
                }
                Err(e) => {
                    warn!(repo = repo.name, "repository identity repair failed: {e:#}");
                    snapshot.restore(self);
                    return true;
                }
            }
        }

        if let Err(e) = self.cfg.validate() {
            warn!("repository identity repair produced invalid configuration: {e:#}");
            snapshot.restore(self);
            return true;
        }
        // Config is the recoverable transaction marker: once it contains the
        // canonical name and former-name alias, startup can migrate an older
        // state file. Never publish migrated state before that marker.
        if config_changed && let Err(e) = self.cfg.save() {
            warn!("saving repaired repository configuration: {e:#}");
            snapshot.restore(self);
            self.identity_checked_at = None;
            return false;
        }
        if state_changed && let Err(e) = self.state.save() {
            warn!("saving repository identity state: {e:#}");
            snapshot.restore(self);
            self.identity_checked_at = None;
            return false;
        }

        for (old, identity, changes) in resolved {
            if changes.renamed || !old.aliases.is_empty() {
                self.update_checkout_origins(&old, &identity).await;
            }
            if changes.renamed {
                info!(
                    old = old.name,
                    new = identity.full_name,
                    id = identity.id,
                    "repository identity repaired"
                );
            } else if changes.enrolled {
                info!(
                    repo = identity.full_name,
                    id = identity.id,
                    "repository identity enrolled"
                );
            }
        }
        true
    }

    fn apply_repository_identity(
        &mut self,
        old: &RepoConfig,
        identity: &RepositoryIdentity,
    ) -> Result<IdentityChanges> {
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
        let mut state_changed = false;
        if renamed {
            state_changed |= self.state.rename_repo(&old.name, canonical)?;
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
                points_to_repository(url, &old.name, identity)
                    || old
                        .aliases
                        .iter()
                        .any(|a| points_to_repository(url, a, identity))
            }) {
                repo.clone_url = Some(url_for_style(
                    repo.clone_url.as_deref().unwrap_or_default(),
                    identity,
                ));
            }
        }
        repo.aliases.retain(|a| !a.eq_ignore_ascii_case(canonical));
        let aliases = repo.aliases.clone();
        let clone_url = repo.clone_url.clone();
        for alias in &aliases {
            state_changed |= self.state.rename_repo(alias, canonical)?;
            self.rename_runtime_keys(alias, canonical);
        }
        if state_changed {
            self.refetch.insert(canonical.to_string());
        }
        Ok(IdentityChanges {
            config: old.github_id != Some(identity.id)
                || renamed
                || old.aliases != aliases
                || old.clone_url != clone_url,
            state: state_changed,
            renamed,
            enrolled: old.github_id.is_none(),
        })
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
        self.collaborators.remove(new);
        self.refetch.remove(old);
        self.refetch.insert(new.to_string());
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

#[derive(Clone, Copy)]
struct IdentityChanges {
    config: bool,
    state: bool,
    renamed: bool,
    enrolled: bool,
}

struct IdentitySnapshot {
    cfg: Config,
    state: State,
    failures: BTreeMap<(String, u64), u32>,
    collaborators: BTreeMap<String, Collaborators>,
    refetch: BTreeSet<String>,
    conflict_checks: BTreeMap<String, Instant>,
    conflict_pairs: BTreeMap<(String, String), ConflictPair>,
}

impl IdentitySnapshot {
    fn capture(engine: &Engine) -> Self {
        Self {
            cfg: engine.cfg.clone(),
            state: engine.state.clone(),
            failures: engine.failures.clone(),
            collaborators: engine.collaborators.clone(),
            refetch: engine.refetch.clone(),
            conflict_checks: engine.conflict_checks.clone(),
            conflict_pairs: engine.conflict_pairs.clone(),
        }
    }

    fn restore(self, engine: &mut Engine) {
        engine.cfg = self.cfg;
        engine.state = self.state;
        engine.failures = self.failures;
        engine.collaborators = self.collaborators;
        engine.refetch = self.refetch;
        engine.conflict_checks = self.conflict_checks;
        engine.conflict_pairs = self.conflict_pairs;
    }
}

async fn update_origin(
    root: &str,
    repo: &RepoConfig,
    identity: &RepositoryIdentity,
) -> Result<bool> {
    let current = conflict_git(root, &["remote", "get-url", "origin"]).await?;
    if !points_to_repository(&current, &repo.name, identity)
        && !repo
            .aliases
            .iter()
            .any(|a| points_to_repository(&current, a, identity))
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
    let clean = clean.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    clean.ends_with(&format!("/{name}")) || clean.ends_with(&format!(":{name}"))
}

fn points_to_repository(url: &str, name: &str, identity: &RepositoryIdentity) -> bool {
    points_to_name(url, name)
        && remote_host(url).is_some_and(|host| {
            [
                remote_host(&identity.clone_url),
                remote_host(&identity.ssh_url),
            ]
            .into_iter()
            .flatten()
            .any(|expected| host.eq_ignore_ascii_case(&expected))
        })
}

fn remote_host(remote: &str) -> Option<String> {
    let remote = remote.trim();
    if let Ok(url) = reqwest::Url::parse(remote) {
        return url.host_str().map(str::to_string);
    }
    let (user_host, _) = remote.split_once(':')?;
    let (_, host) = user_host.rsplit_once('@')?;
    (!host.is_empty()).then(|| host.to_string())
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
        assert!(points_to_repository(
            "https://github.com/old/place.git",
            "old/place",
            &id
        ));
        assert!(!points_to_repository(
            "https://gitlab.com/old/place.git",
            "old/place",
            &id
        ));
        assert!(!points_to_repository(
            "/srv/mirror/old/place.git",
            "old/place",
            &id
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

        sh(
            &checkout.work,
            &[
                "remote",
                "set-url",
                "origin",
                "https://gitlab.com/old/place.git",
            ],
        )
        .await;
        assert!(
            !update_origin(&checkout.work, &repo, &identity)
                .await
                .unwrap()
        );
        assert_eq!(
            conflict_git(&checkout.work, &["remote", "get-url", "origin"])
                .await
                .unwrap(),
            "https://gitlab.com/old/place.git"
        );
    }
}
