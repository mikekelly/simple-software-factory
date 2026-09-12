use super::super::*;
use tracing::{debug, info, warn};

impl Engine {
    pub(in crate::engine) fn driver(&self, repo: &RepoConfig) -> &Driver {
        let kind = self.cfg.driver_for(repo);
        self.drivers
            .get(kind)
            .expect("sync_drivers keeps a driver for every kind the config uses")
    }

    /// Rebuild the driver set when the config's choice of drivers changed
    /// (a repo added with `--driver`, or the default switched).
    pub(in crate::engine) fn sync_drivers(&mut self) {
        if self.drivers.kinds() != self.cfg.drivers_in_use() {
            self.drivers = Drivers::from_config(&self.cfg);
        }
    }

    /// What one item's session runs with: the repository's config with
    /// the item's own launch overrides applied (`ssf handover`). Every
    /// launch, resume, relaunch, login check and event of that item goes
    /// through this rather than through `repo` itself, or a handed-over
    /// session would be started with the old harness's flags or probed as
    /// the wrong harness.
    pub(in crate::engine) fn effective(&self, repo: &RepoConfig, number: u64) -> RepoConfig {
        repo.with_overrides(self.overrides_of(repo, number).as_ref())
    }

    /// The overrides that govern an item: its own, or, for an item bound
    /// to another item's session, that session's (they share the
    /// workspace, so they share the harness in it).
    pub(in crate::engine) fn overrides_of(
        &self,
        repo: &RepoConfig,
        number: u64,
    ) -> Option<Overrides> {
        let owner = self.owner_of(repo, number);
        self.peek(repo, owner).and_then(|s| s.overrides.clone())
    }

    pub(in crate::engine) fn driver_down(&self, repo: &RepoConfig) -> bool {
        self.down.contains(&self.cfg.driver_for(repo))
    }

    /// Ask every driver in use whether it is ready, remembering the ones
    /// that are not. Returns what is wrong with the ones that are not; fails
    /// only when none is.
    pub(in crate::engine) async fn check_drivers(&mut self) -> Result<Vec<String>> {
        let mut down = Vec::new();
        let mut errors = Vec::new();
        let mut any_up = false;
        for d in self.drivers.iter() {
            match d.status().await {
                Ok(()) => any_up = true,
                Err(e) => {
                    down.push(d.kind());
                    errors.push(format!("{} unavailable: {e:#}", d.label()));
                }
            }
        }
        for e in &errors {
            if any_up {
                warn!("{e}; its repositories are skipped this pass");
            }
        }
        self.down = down;
        if any_up {
            Ok(errors)
        } else {
            anyhow::bail!("{}", errors.join("; "))
        }
    }

    pub async fn new(cfg: Config) -> Result<Self> {
        // Take the lock before looking up credentials or reading state: a
        // rejected `ssf-server --once` must not touch a live daemon's state.
        let state_lock = StateLock::acquire()?;
        refuse_live_daemon()?;
        let token = cfg.github_token()?;
        let gh = GitHub::new(&cfg.github.api_url, &token)?;
        let me = gh.whoami().await.context("verifying GitHub token")?;
        info!(login = me.login, kind = me.kind, "authenticated to GitHub");
        let drivers = Drivers::from_config(&cfg);
        let mut state = State::load()?;
        state.bot_login = Some(me.login.clone());
        state.save()?;
        let startup_pending = if cfg.daemon.resume_on_start {
            cfg.drivers_in_use()
        } else {
            Vec::new()
        };
        Ok(Self {
            cfg,
            gh,
            drivers,
            down: Vec::new(),
            login: me.login,
            state,
            failures: BTreeMap::new(),
            startup_pending,
            collaborators: BTreeMap::new(),
            dropped_logged: std::sync::Mutex::new(BTreeSet::new()),
            probe: std::sync::Arc::new(login::probe),
            installed: std::sync::Arc::new(crate::agents::installed),
            probes: BTreeMap::new(),
            refetch: BTreeSet::new(),
            startup_pass: false,
            onboarding: None,
            conflict_checks: BTreeMap::new(),
            conflict_pairs: BTreeMap::new(),
            _state_lock: Some(state_lock),
        })
    }

    /// Who may drive a repository: its configured list, else the instance
    /// list, else the collaborators fetched this pass (an empty list until
    /// they have been, so nothing slips through on a guess).
    pub(in crate::engine) fn allow_list(&self, repo: &RepoConfig) -> AllowList {
        match self.cfg.allowed_users(repo) {
            Some((list, source)) => {
                AllowList::new(&self.login, list.iter().map(String::as_str), source)
            }
            None => AllowList::new(
                &self.login,
                self.collaborators
                    .get(&repo.name)
                    .map(|c| c.logins.iter().map(String::as_str))
                    .into_iter()
                    .flatten(),
                Source::Collaborators,
            ),
        }
    }

    /// Bring the collaborator list of a repository up to date, when the
    /// config leaves the allow-list to it. A fetch that fails keeps the
    /// last good list with a warning; with none cached the pass fails for
    /// this repository rather than running open or shut on a guess.
    pub(in crate::engine) async fn refresh_collaborators(
        &mut self,
        repo: &RepoConfig,
        owner: &str,
        name: &str,
    ) -> Result<()> {
        if self.cfg.allowed_users(repo).is_some() {
            return Ok(());
        }
        let etag = self
            .collaborators
            .get(&repo.name)
            .and_then(|c| c.etag.clone());
        match self.gh.collaborators(owner, name, etag.as_deref()).await {
            Ok(Conditional::NotModified) => Ok(()),
            Ok(Conditional::Modified { value, etag }) => {
                let logins = allow::pushers(&value);
                let before = self.collaborators.get(&repo.name).map(|c| &c.logins);
                if before != Some(&logins) {
                    info!(
                        repo = repo.name,
                        logins = logins.join(", "),
                        "allowed users are the collaborators with push access"
                    );
                }
                self.collaborators
                    .insert(repo.name.clone(), Collaborators { logins, etag });
                Ok(())
            }
            Err(e) if self.collaborators.contains_key(&repo.name) => {
                warn!(
                    repo = repo.name,
                    "collaborators could not be refreshed; keeping the last list: {e:#}"
                );
                Ok(())
            }
            Err(e) => Err(e.context(format!(
                "collaborators of {} could not be fetched and no allowed_users is configured; \
                 nothing is acted on until one of the two works (see `ssf doctor`)",
                repo.name
            ))),
        }
    }

    /// Whether whoever asked the bot onto an item (see `allow::askers`) is
    /// allowed to; `Err` says who was not, for the log.
    pub(in crate::engine) fn gate(
        &self,
        repo: &RepoConfig,
        issue: &Issue,
        timeline: &[Value],
        triggers: &[String],
    ) -> std::result::Result<(), String> {
        let asks = allow::askers(issue, timeline, triggers, &self.login);
        allow::check(&self.allow_list(repo), &asks)
    }

    /// Log an event left out of a delivery because of the allow-list: an
    /// info line the first time, debug after (`diff` sees the same events
    /// again whenever it builds a relaunch text or a story). Project
    /// automation and other `[bot]` accounts fire on every card move, so
    /// they are debug from the start.
    pub(in crate::engine) fn dropped(&self, repo: &RepoConfig, key: &str, who: &str) {
        let first = self
            .dropped_logged
            .lock()
            .map(|mut set| set.insert(format!("{}:{key}", repo.name)))
            .unwrap_or(false);
        if first && !allow::is_bot_account(who) {
            info!(
                repo = repo.name,
                key,
                actor = who,
                "dropping event: @{who} is not an allowed user"
            );
        } else {
            debug!(
                repo = repo.name,
                key,
                actor = who,
                "dropping event by @{who}, not an allowed user"
            );
        }
    }

    /// An item nobody allowed asked for: said once, and not looked at again
    /// until it changes (an allowed user assigning or mentioning the bot
    /// later brings it in).
    pub(in crate::engine) fn refuse(
        &mut self,
        repo: &RepoConfig,
        issue: &Issue,
        triggers: &[String],
        why: &str,
    ) {
        info!(
            repo = repo.name,
            issue = issue.number,
            "ignoring {}: {why}",
            issue.html_url
        );
        self.state
            .repo_mut(&repo.name)
            .ignored
            .insert(issue.number, Ignored::new(issue, triggers));
    }

    pub async fn run_forever(mut self) -> Result<()> {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .context("installing SIGTERM handler")?;
        let listener = bind_socket()?;
        info!(
            repos = self.cfg.repos.len(),
            poll_secs = self.cfg.daemon.poll_interval_secs,
            socket = %crate::ipc::socket_path().display(),
            "ssf daemon started"
        );
        // Orca may still be coming up in the same login (the unit starts with
        // the graphical session), and the startup pass needs it: wait a
        // bounded while before the first poll rather than skipping passes.
        let mut stop = false;
        if !self.startup_pending.is_empty() {
            let wait = Duration::from_secs(self.cfg.daemon.startup_driver_wait_secs);
            let started = tokio::time::Instant::now();
            loop {
                let err = match self.check_drivers().await {
                    Ok(_) => break,
                    Err(e) => e,
                };
                let elapsed = started.elapsed();
                if elapsed >= wait {
                    if !wait.is_zero() {
                        warn!(
                            waited_secs = elapsed.as_secs(),
                            "the driver is still not ready; polling starts now and interrupted sessions \
are resumed on the first pass that finds it: {err:#}"
                        );
                    }
                    break;
                }
                info!("waiting for the driver before the first pass: {err:#}");
                let deadline =
                    tokio::time::Instant::now() + Duration::from_secs(10).min(wait - elapsed);
                if self.idle_until(deadline, &listener, &mut sigterm).await {
                    stop = true;
                    break;
                }
            }
        }
        while !stop {
            self.tick().await;
            let interval = Duration::from_secs(self.cfg.daemon.poll_interval_secs.max(5));
            let deadline = tokio::time::Instant::now() + interval;
            stop = self.idle_until(deadline, &listener, &mut sigterm).await;
        }
        self.state.save()?;
        let _ = std::fs::remove_file(crate::ipc::socket_path());
        Ok(())
    }

    /// Answer the CLI (`ssf sub|unsub|tell`) until `deadline`. True when a
    /// signal asked the daemon to exit.
    pub(in crate::engine) async fn idle_until(
        &mut self,
        deadline: tokio::time::Instant,
        listener: &tokio::net::UnixListener,
        sigterm: &mut tokio::signal::unix::Signal,
    ) -> bool {
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => return false,
                _ = tokio::signal::ctrl_c() => { info!("interrupted; exiting"); return true; }
                _ = sigterm.recv() => { info!("SIGTERM; exiting"); return true; }
                conn = listener.accept() => match conn {
                    Ok((stream, _)) => self.serve(stream).await,
                    Err(e) => warn!("accepting a CLI connection failed: {e}"),
                }
            }
        }
    }

    /// The record of an item, if there is one.
    pub(in crate::engine) fn peek(&self, repo: &RepoConfig, number: u64) -> Option<&IssueState> {
        self.state.repos.get(&repo.name)?.issues.get(&number)
    }

    /// Pick up edits to the config file between passes (repos, harnesses,
    /// intervals) without a restart. The token is fixed for the process.
    pub(in crate::engine) fn reload_config(&mut self) {
        match Config::load() {
            Ok(cfg) => {
                self.cfg = cfg;
                self.sync_drivers();
            }
            Err(e) => warn!("config reload failed, keeping previous: {e:#}"),
        }
    }
}
