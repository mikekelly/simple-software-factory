use super::super::*;
use tracing::{debug, info, warn};

impl Engine {
    /// Accept invitations from explicitly trusted inviters. This is deliberately
    /// account-wide and separate from repository enrollment: accepting access
    /// never adds a `[[repo]]` or selects/enables a server target.
    pub(in crate::engine) async fn accept_repository_invitations(&self) -> Result<()> {
        let allowed = &self.cfg.github.auto_accept_invitations_from;
        if allowed.is_empty() {
            return Ok(());
        }
        let mut failures = Vec::new();
        for invitation in self.gh.repository_invitations().await? {
            let Some(inviter) = invitation.inviter else {
                debug!(
                    repository = invitation.repository.full_name,
                    "leaving repository invitation pending: inviter is unavailable"
                );
                continue;
            };
            if !allowed.iter().any(|login| {
                login
                    .trim()
                    .trim_start_matches('@')
                    .eq_ignore_ascii_case(&inviter.login)
            }) {
                debug!(
                    repository = invitation.repository.full_name,
                    inviter = inviter.login,
                    "leaving repository invitation pending: inviter is not allowed"
                );
                continue;
            }
            if let Err(e) = self.gh.accept_repository_invitation(invitation.id).await {
                failures.push(format!(
                    "{} from @{}: {e:#}",
                    invitation.repository.full_name, inviter.login
                ));
                continue;
            }
            info!(
                repository = invitation.repository.full_name,
                inviter = inviter.login,
                "accepted repository invitation"
            );
        }
        if !failures.is_empty() {
            anyhow::bail!(
                "could not accept {} repository invitation(s): {}",
                failures.len(),
                failures.join("; ")
            );
        }
        Ok(())
    }

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

    /// What one item's next session is launched with: the repository's
    /// config with the item's own launch overrides applied (`ssf
    /// handover`). Every launch, resume, relaunch and re-creation of that
    /// item goes through this rather than through `repo` itself, or a
    /// handed-over session would be started with the old harness's flags.
    /// It says nothing about a session that is already running: a config
    /// edit does not change what a live pane is on, and the questions
    /// about that one go through `live_harness`.
    pub(in crate::engine) fn effective(&self, repo: &RepoConfig, number: u64) -> RepoConfig {
        repo.with_overrides(self.overrides_of(repo, number).as_ref())
    }

    /// The workspaces of a repository as the driver last reported them this
    /// pass (`Driver::ps`): the panes live in it, and the agent each one is
    /// running. Read once a pass, so the login check, the handover
    /// bookkeeping and the guidance a live session is given all see the
    /// same answer. False when the driver could not be asked, which nothing
    /// stands in for: a question about a pane is not answered by a guess,
    /// and a read that failed is remembered as that for the rest of the
    /// pass rather than retried by every caller.
    pub(in crate::engine) async fn learn_workspaces(&mut self, repo: &RepoConfig) -> bool {
        if self.workspaces_read.contains(&repo.name) {
            return self.workspaces.contains_key(&repo.name);
        }
        self.refresh_workspaces(repo).await
    }

    /// [`learn_workspaces`](Self::learn_workspaces) reading the driver
    /// again whatever this pass already saw. `ssf handover` decides on the
    /// answer, and the CLI answers it between passes; a pass that changes
    /// which harness is in a workspace has to read again too, since the
    /// panes it read at the start describe the session it has just ended.
    pub(in crate::engine) async fn refresh_workspaces(&mut self, repo: &RepoConfig) -> bool {
        self.workspaces_read.insert(repo.name.clone());
        match self.driver(repo).ps().await {
            Ok(list) => {
                self.workspaces.insert(repo.name.clone(), list);
                true
            }
            Err(e) => {
                // Nothing stands in for the answer: a pane whose harness is
                // not known is read as the stack its record would launch,
                // never as the one read in some earlier pass, and a caller
                // that has to know which panes are there is told to wait
                // for a pass that can ask.
                self.workspaces.remove(&repo.name);
                debug!(
                    repo = repo.name,
                    "the driver's workspaces could not be read, so what each pane runs is unknown: {e:#}"
                );
                false
            }
        }
    }

    /// The workspaces of a repository as last read this pass; empty when
    /// the driver could not be asked (`learn_workspaces` says which).
    pub(in crate::engine) fn workspaces_of(&self, repo: &RepoConfig) -> &[WorkspaceInfo] {
        self.workspaces
            .get(&repo.name)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Forget this pass's read of a repository's panes: the driver changed
    /// what is in a workspace since (a handover ended one harness and
    /// started another), and what the pass asks next has to see the
    /// harness that is there now.
    pub(in crate::engine) fn forget_workspaces(&mut self, repo: &RepoConfig) {
        self.workspaces_read.remove(&repo.name);
    }

    /// The harness the pane of an item's session is running, from the
    /// driver's own report (`AgentInfo.agent_type`). `None` when nothing
    /// live reports one: an item that has never been launched, a workspace
    /// the driver does not list, a driver that could not be asked, or a
    /// pane whose agent the driver does not recognise.
    pub(in crate::engine) fn running_harness(
        &self,
        repo: &RepoConfig,
        number: u64,
    ) -> Option<String> {
        let owner = self.owner_of(repo, number);
        let id = self.peek(repo, owner)?.worktree_id.as_deref()?;
        self.workspaces_of(repo)
            .iter()
            .find(|w| w.worktree_id == id)
            .and_then(WorkspaceInfo::primary_agent)
            .and_then(|a| a.agent_type.clone())
    }

    /// The harness a question about the session that is *live now* is asked
    /// with: what its pane is running when the driver reports one, and the
    /// harness its record would launch otherwise (nothing started yet, or
    /// a driver that cannot say). The screen it is showing, the login it
    /// needs and the guidance it reads all follow this; what a launch,
    /// resume or relaunch starts is `effective`, never this.
    pub(in crate::engine) fn live_harness(&self, repo: &RepoConfig, number: u64) -> String {
        self.running_harness(repo, number)
            .unwrap_or_else(|| self.effective(repo, number).harness)
    }

    /// What the item is on now, for the commands and posts that have to name
    /// it: the harness its pane is running when the driver reports one, with
    /// the record's model, effort and command. A session left on another
    /// harness by a config edit (`ssf repo set`) keeps that harness, and
    /// there its model, effort and command stay unset: only the harness is
    /// the driver's to report, and what that session was launched with is
    /// not on the record, so nothing is claimed about it.
    pub(in crate::engine) fn current_stack(&self, repo: &RepoConfig, number: u64) -> Current {
        let mut eff = self.effective(repo, number);
        let mut unknown_stack = false;
        if let Some(running) = self
            .running_harness(repo, number)
            .filter(|running| *running != eff.harness)
        {
            eff.harness = running;
            eff.model = None;
            eff.effort = None;
            eff.command = None;
            unknown_stack = true;
        }
        Current {
            harness: eff.harness,
            model: eff.model,
            effort: eff.effort,
            command: eff.command,
            unknown_stack,
        }
    }

    /// The stack a command is naming, checked the way both commands that
    /// write one need it: a harness ssf knows, a model and effort that
    /// harness takes, installed where the daemon runs, and signed in as of
    /// now. The login is asked afresh rather than off the pass's memo: an
    /// operator who signs the harness in and runs the command again must
    /// get the new answer, not the one from up to a poll interval ago.
    pub(in crate::engine) async fn check_stack(
        &mut self,
        harness: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<()> {
        if !crate::agents::is_known(harness) {
            anyhow::bail!("{harness} is not a harness ssf knows (see `ssf agents`)");
        }
        crate::models::validate(harness, model, effort)?;
        let name = login::display_name(harness);
        if !(self.installed)(harness) {
            anyhow::bail!("{name} is not installed where the daemon runs (see `ssf agents`)");
        }
        self.probes.remove(harness);
        let probe = self.probe_harness(harness).await;
        if probe.state == LoginState::SignedOut {
            anyhow::bail!(
                "{name} is not signed in here; {}",
                login::how_to_sign_in(harness)
            );
        }
        Ok(())
    }

    /// The context-compaction threshold `eff` will be launched with, checked
    /// against the harness that will launch: `ssf handover` and `ssf assign`
    /// may name a harness the repository's or the instance's value does not
    /// fit, and a launch handed a count its harness refuses never comes up.
    pub(in crate::engine) fn check_auto_compaction(&self, eff: &RepoConfig) -> Result<()> {
        crate::models::validate_auto_compaction(
            &eff.harness,
            self.cfg.auto_compaction_tokens_for(eff),
        )
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
        let mut engine = Self {
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
            workspaces: BTreeMap::new(),
            workspaces_read: BTreeSet::new(),
            refetch: BTreeSet::new(),
            channel_lost: BTreeSet::new(),
            startup_pass: false,
            onboarding: None,
            adopting: None,
            conflict_checks: BTreeMap::new(),
            conflict_pairs: BTreeMap::new(),
            identity_checked_at: None,
            _state_lock: Some(state_lock),
        };
        if !engine.reconcile_repo_identities(true).await {
            anyhow::bail!("repository identity repair could not be saved");
        }
        Ok(engine)
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
        // Herdr may still be coming up in the same login, and the startup pass
        // needs it: wait a
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

    /// Answer the CLI (`ssf sub|unsub`, `ssf release|purge`) until
    /// `deadline`. True when a signal asked the daemon to exit.
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
