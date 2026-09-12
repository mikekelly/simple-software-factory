use super::*;

impl Vm {
    /// Create the instance and boot it once so the guest scripts
    /// provision it; `--force` deletes an existing instance first. The
    /// data disk is kept, with one exception: a disk an earlier build
    /// created and no guest ever used is blank and unformattable, and
    /// `--force` re-creates that one (see [`Vm::unproven_disk`]).
    pub(in crate::vm) async fn lima_build(&self, host: &Config, force: bool) -> Result<()> {
        self.lima_preflight()?;
        let name = self.lima_name();
        if let Some(inst) = self.lima_instance()? {
            if !force {
                // A build that died between `limactl create` and the
                // guest answering ssh left `format: true` behind; this is
                // the run that would otherwise return without repairing
                // it, so it repairs it here -- and says so rather than
                // returning "instance exists" over a flag it could not
                // put right, which is how a later `ssf vm start` came to
                // boot a guest that was still allowed to reformat the
                // factory's disk.
                if let Some(yaml) = self.repair_stale_format(Some(&inst), Why::FoundStale) {
                    return Err(self.stale_format_error(&yaml));
                }
                println!(
                    "lima instance {name} exists ({}); `ssf vm build --force` makes a new one",
                    inst.dir
                );
                return Ok(());
            }
            if inst.is_running() {
                self.lima_stop().await?;
            }
            info!("deleting lima instance {name}");
            self.limactl_run(&["delete", "-f", &name])?;
        }
        self.ensure_key()?;
        let _ = std::fs::remove_file(self.known_hosts());
        let make_disk = self.take_data_disk(force)?;
        self.write_template(make_disk)?;
        self.write_share(host)?;
        if make_disk {
            self.create_data_disk()?;
        }
        self.lima_create()?;
        // From here the instance exists and boots with `format: true`
        // when this build made the disk. A failure that left it running
        // used to leave that flag behind with nothing able to change it
        // (`limactl edit` refuses a running instance), so the next
        // `ssf vm start` booted a guest that could still reformat a disk
        // by then holding the factory. The first boot is therefore
        // cleaned up after.
        if let Err(e) = self.lima_first_boot().await {
            self.after_failed_build().await;
            return Err(self.blank_disk_hint(e));
        }
        println!("built lima instance {name}; `ssf vm start` boots it");
        Ok(())
    }

    /// The data disk this build will run on, and whether the build has to
    /// make it -- which is the same as whether it may hand lima
    /// `format: true`, since only the build that creates a disk lets lima
    /// format it.
    ///
    /// A disk an earlier build created but never got a guest up on is
    /// marked ([`Vm::unproven_disk`]), and `--force` -- the flag that
    /// already means "make this instance again" -- deletes it and makes a
    /// fresh one. Without that, such a disk is unusable for good: lima's
    /// boot script is the only thing that formats it, no later build
    /// hands lima the flag, and every start waits its two minutes for a
    /// mount that will never come. A disk no marker points at is one some
    /// build saw a guest come up on, so nothing here deletes it.
    ///
    /// This is the one path in ssf that destroys the factory's data disk,
    /// which is why it is a step of its own rather than four lines in the
    /// middle of a build.
    pub(in crate::vm) fn take_data_disk(&self, force: bool) -> Result<bool> {
        if self.lima_disk()?.is_none() {
            return Ok(true);
        }
        if !force || !self.unproven_disk().exists() {
            return Ok(false);
        }
        let disk = self.lima_disk_name();
        info!(
            "the data disk {disk} was made by a build that never got a guest up on it; deleting it, and whatever it holds, and making a fresh one"
        );
        self.limactl_run(&["disk", "delete", &disk])?;
        let _ = std::fs::remove_file(self.unproven_disk());
        Ok(true)
    }

    /// Create the data disk, and mark it as one no guest has come up on
    /// yet ([`Vm::unproven_disk`]).
    pub(in crate::vm) fn create_data_disk(&self) -> Result<()> {
        let disk = self.lima_disk_name();
        let gib = self.sizes().data_gib;
        info!("creating lima disk {disk} ({gib} GiB)");
        self.limactl_run(&["disk", "create", &disk, "--size", &format!("{gib}GiB")])?;
        self.mark_disk_unproven();
        Ok(())
    }

    /// The first boot of a freshly created instance: the guest provisions
    /// itself inside `limactl start`, the host waits for the marker and
    /// then for ssh, and the instance is stopped again.
    pub(in crate::vm) async fn lima_first_boot(&self) -> Result<()> {
        let name = self.lima_name();
        info!("starting {name} for its first boot: the guest provisions itself (a few minutes)");
        self.limactl_start(&["start", "--timeout", &start_timeout_arg(), &name])?;
        self.wait_for_provisioning().await?;
        if let Some(inst) = self.lima_instance()? {
            super::super::write_private(&Path::new(&inst.dir).join("ssf-safe-root-v2"), b"1\n")?;
            let _ = std::fs::remove_file(Path::new(&inst.dir).join("ssf-fresh-root-v2"));
        }
        if let Ok(log) =
            self.limactl_output_within(&["shell", &name, "sudo", "cat", PROVISION_LOG], PROBE_LIMIT)
        {
            for line in log.lines().filter(|l| l.starts_with("provision: ")) {
                eprintln!("  {line}");
            }
        }
        if let Err(e) = self.wait_for_ssh(SEED_TIMEOUT).await {
            bail!(
                "{e:#} (provisioned, but the guest does not answer as {}); `ssf vm console` has its console",
                super::super::GUEST_USER
            );
        }
        // The guest has provisioned and answered as `ssf`, which it can
        // only do once `seed-lima.sh` has mounted the data disk and
        // written `authorized_keys` onto it: the disk carries a
        // filesystem and the factory now, so it is no longer one a
        // `--force` build may throw away.
        let _ = std::fs::remove_file(self.unproven_disk());
        // Nothing from here on may let lima format it. The template goes
        // first and at once, because it is what
        // `ssf vm reset` builds the next instance from and because
        // everything below can still fail; the instance's own copy
        // follows the stop, since `limactl edit` is for a stopped
        // instance.
        self.write_template(false)?;
        self.limactl_run_within(&["stop", &name], STOP_LIMIT)?;
        let stopped = self.lima_instance().ok().flatten();
        if let Some(yaml) = self.repair_stale_format(stopped.as_ref(), Why::FinishingBuild) {
            return Err(self.stale_format_error(&yaml));
        }
        Ok(())
    }

    /// After a first boot that failed: stop the instance and put
    /// `format: false` back. Both are best effort and nothing here
    /// replaces the build's own error -- the point is only that the next
    /// command finds a stopped instance whose template cannot reformat
    /// the data disk.
    pub(in crate::vm) async fn after_failed_build(&self) {
        // The build has already failed; this is the tidying up, and it
        // runs `limactl` on a path where `limactl` may itself be the
        // thing that is stuck. Everything below is bounded, but a person
        // watching a build that failed minutes ago deserves to know why
        // the tool is still working.
        println!(
            "the build failed; putting {} back to a state the next command can use (stopping it, and turning `format` off for the data disk). This can take a few minutes when limactl is not answering; the build's own error follows it.",
            self.lima_name()
        );
        if let Err(e) = self.lima_stop().await {
            warn!(
                "could not stop {} after the build failed ({e:#}); `limactl stop -f {}` does it",
                self.lima_name(),
                self.lima_name()
            );
        }
        let inst = self.lima_instance().ok().flatten();
        if let Some(yaml) = self.repair_stale_format(inst.as_ref(), Why::FoundStale) {
            warn!("{}", self.stale_format_note(&yaml));
        }
        // A disk this build created but never got a filesystem onto is
        // said out loud in the build's own error instead of here, where
        // an `info!` line scrolls past above it: see
        // [`Vm::blank_disk_hint`].
    }

    /// Where a build records that it created the data disk and has not
    /// yet seen a guest use it. lima's boot script is the only thing that
    /// puts a filesystem on that disk and only the creating build lets it,
    /// so a disk left behind by a build that died before the guest came
    /// up can never be formatted by a later one: the seed waits its two
    /// minutes for a mount that will not come and the build fails eight
    /// minutes later pointing at ssh. The marker is what makes that
    /// recoverable, and it is a file of ssf's rather than a question put
    /// to lima because lima has nothing to say about a disk's contents.
    pub(in crate::vm) fn unproven_disk(&self) -> PathBuf {
        self.dir.join("disk-unproven")
    }

    pub(in crate::vm) fn mark_disk_unproven(&self) {
        let path = self.unproven_disk();
        if let Err(e) = std::fs::write(
            &path,
            format!(
                "This build created the lima disk {}, and ssf never saw a guest come\nup on it. `ssf vm build --force` deletes that disk, and whatever is on it,\nand makes a fresh one while this file is here; ssf removes this file as soon\nas a guest has answered on the disk, and from then on no build deletes it.\n",
                self.lima_disk_name()
            ),
        ) {
            // Not fatal: without it a `--force` build simply keeps the
            // disk, which is what every build did before this marker.
            warn!("could not write {}: {e:#}", path.display());
        }
    }

    /// The build's own error, with what to do about a disk this build
    /// created and no guest ever used.
    pub(in crate::vm) fn blank_disk_hint(&self, e: anyhow::Error) -> anyhow::Error {
        if !self.unproven_disk().exists() {
            return e;
        }
        anyhow::anyhow!(
            "{e:#}\n\nthis build created the data disk {disk} and never saw a guest come up on it. ssf removes this mark the moment one does, so as far as ssf knows nothing has been put on that disk -- but it cannot see inside it, and a build interrupted after lima's boot script ran may have left a filesystem and a seeded factory there. No later build, `ssf vm start` or `ssf vm reset` will format it (only the build that creates a disk lets lima do that), so a disk that never got a filesystem stays unusable. `ssf vm build --force` deletes {disk} and everything on it and makes a fresh one; it deletes no disk a build ever saw a guest use.",
            disk = self.lima_disk_name()
        )
    }

    /// `limactl start`, bounded by the allowance lima is given for the
    /// same work plus a margin (see [`OWN_TIMEOUT_MARGIN`]): lima times
    /// the boot out first and says so; ssf's bound is only for a
    /// `limactl` that never returns at all.
    pub(in crate::vm) fn limactl_start(&self, args: &[&str]) -> Result<()> {
        self.limactl_run_within(args, PROVISION_TIMEOUT + OWN_TIMEOUT_MARGIN)
    }

    /// Put `format: false` back where a build that did not reach the end
    /// left `format: true`: the template ssf writes (what `ssf vm reset`
    /// creates the next instance from) and, when one is given, the
    /// instance's own copy of it (the only copy lima reads at boot).
    /// Only when the data disk exists -- with no disk there is nothing to
    /// protect, and the build that makes it is the one build that may
    /// hand lima a `format: true`. See [`plan_repair`] for the rules.
    ///
    /// Returns the copy that still says `format: true` afterwards --
    /// lima's, on a running instance (which `limactl edit` refuses) or
    /// after an edit that did not take, and ssf's own when that could not
    /// be rewritten. A caller that is about to boot the instance must not
    /// go on when it does: that boot is exactly what would let lima
    /// reformat a disk the factory is already living on.
    ///
    /// Every probe here is read fail-closed, because on this path the
    /// unknown answer is the dangerous one: "I could not find out" and
    /// "there is nothing to protect" look the same to a caller, and only
    /// one of them is safe to boot on. So a `limactl disk list` that
    /// failed counts as "the disk is there", a template that cannot be
    /// read counts as "it still says `format: true`", a rewrite that
    /// failed leaves that file unrepaired, and an edit lima reported as
    /// successful is believed only after the file it edited has been read
    /// back. The cost of being wrong is a refused boot and a message; the
    /// cost of the other way round is the factory's disk.
    #[must_use = "a stale `format: true` must stop the boot, not be dropped"]
    pub(super) fn repair_stale_format(&self, inst: Option<&Instance>, why: Why) -> Option<PathBuf> {
        let path = self.template_path();
        let instance_yaml = inst.map(|i| Path::new(&i.dir).join("lima.yaml"));
        let stale = |p: &Path| match std::fs::read_to_string(p) {
            Ok(t) => says_format_true(&t),
            // A file that is not there tells lima nothing, so it is not a
            // failed probe: `ssf vm reset` repairs a template ssf has not
            // written yet, and an instance without its own copy does not
            // boot at all. Any other error is a probe that did not run.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                warn!(
                    "could not read {} ({e}); treating it as still saying `format: true`",
                    p.display()
                );
                true
            }
        };
        let disk_exists = match self.lima_disk() {
            Ok(d) => d.is_some(),
            Err(e) => {
                warn!(
                    "could not ask lima about the data disk {} ({e:#}); treating it as present, so a stale `format: true` still stops the boot",
                    self.lima_disk_name()
                );
                true
            }
        };
        let plan = plan_repair(
            disk_exists,
            stale(&path),
            instance_yaml.as_deref().is_some_and(stale),
            inst.is_some_and(|i| i.is_running()),
        );
        if plan == Repair::default() {
            return None;
        }
        let which = if plan.template {
            path.display().to_string()
        } else {
            format!("lima's copy of {}", path.display())
        };
        let note = format_off_note(why, &self.lima_disk_name(), &which);
        match why {
            Why::FinishingBuild => info!("{note}"),
            Why::FoundStale => warn!("{note}"),
        }
        // What is still stale when this returns. ssf's own template is
        // the lesser of the two (lima boots from its own copy), so lima's
        // copy overwrites it below when both are unrepaired.
        let mut unrepaired = None;
        if plan.template
            && let Err(e) = self.write_template(false)
        {
            warn!(
                "could not rewrite {}: {e:#}; that file is what `ssf vm reset` creates the next instance from, so it is not left behind quietly",
                path.display()
            );
            unrepaired = Some(path.clone());
        }
        let name = self.lima_name();
        if plan.blocked {
            warn!(
                "lima's copy of the template for {name} still says `format: true` and {name} is running, which `limactl edit` refuses; stop it (`ssf vm stop`) and ssf repairs it at the next `ssf vm build` or `ssf vm start`"
            );
            unrepaired = instance_yaml;
        } else if plan.instance {
            match self.stop_formatting_data_disk() {
                Err(e) => {
                    warn!(
                        "could not turn `format` off for {name}'s data disk ({e:#}); `limactl edit {name} --set '{FORMAT_OFF}'` does it with the instance stopped, and until then a boot that cannot find the disk's label would reformat it"
                    );
                    unrepaired = instance_yaml;
                }
                // The edit is believed only once the file says so. An
                // exit status of 0 is limactl's word that its own `--set`
                // ran, not that `.additionalDisks[0].format` is now
                // false: a differently shaped `additionalDisks`, a schema
                // that moved the key, a future lima whose restricted yq
                // silently matches nothing -- each of them would leave
                // ssf reporting a repair it did not make, and the boot
                // that followed is the one this whole path exists to stop.
                Ok(()) if instance_yaml.as_deref().is_some_and(stale) => {
                    warn!(
                        "`limactl edit {name} --set '{FORMAT_OFF}'` reported success, but lima's copy of the template still says `format: true`; ssf does not boot {name} over that -- put `format: false` in it by hand (the `additionalDisks` entry for {}) before starting the VM again",
                        self.lima_disk_name()
                    );
                    unrepaired = instance_yaml;
                }
                Ok(()) => {}
            }
        }
        unrepaired
    }

    /// What to tell a person whose boot has just been refused because a
    /// copy of the template would let lima reformat the data disk.
    /// `yaml` is the copy [`Vm::repair_stale_format`] could not put
    /// right: lima's own (the one it reads at boot) or, when the rewrite
    /// failed, ssf's.
    pub(in crate::vm) fn stale_format_error(&self, yaml: &Path) -> anyhow::Error {
        let name = self.lima_name();
        let head = format!(
            "{} still lets lima format the data disk {}, and that disk holds the factory's state; refusing to boot {name}.",
            yaml.display(),
            self.lima_disk_name(),
        );
        if yaml == self.template_path() {
            // ssf's own template: nothing here is lima's to fix, and the
            // warning above says why the rewrite failed.
            return anyhow::anyhow!(
                "{head} ssf could not rewrite that file (the warning above says why); make it writable, or put `format: false` in its `additionalDisks` entry by hand, and run this again"
            );
        }
        anyhow::anyhow!(
            "{head} Stop the instance (`ssf vm stop`, or `limactl stop {name}`) and run this again -- ssf turns the flag off while the instance is stopped -- or do it by hand with `limactl edit {name} --set '{FORMAT_OFF}'`"
        )
    }

    /// The same fact where nothing is being booted: the cleanup after a
    /// failed build, and the look `ssf vm start` takes once the guest is
    /// already up. "Refusing to boot" would be wrong in both -- no boot
    /// is being refused here; what matters is that the next one will be.
    pub(in crate::vm) fn stale_format_note(&self, yaml: &Path) -> String {
        let name = self.lima_name();
        format!(
            "{} still lets lima format the data disk {}, and ssf could not turn that off; the next boot of {name} is refused until it is. Stop the instance (`ssf vm stop`, or `limactl stop {name}`) and run `ssf vm start` again -- ssf turns the flag off while the instance is stopped -- or do it by hand with `limactl edit {name} --set '{FORMAT_OFF}'`",
            yaml.display(),
            self.lima_disk_name(),
        )
    }

    /// Write `lima.yaml` for this VM.
    pub(in crate::vm) fn write_template(&self, format_disk: bool) -> Result<()> {
        std::fs::write(self.template_path(), self.lima_template(format_disk)?)
            .with_context(|| format!("writing {}", self.template_path().display()))
    }

    /// Turn `format` off for the data disk in the instance's own copy of
    /// the template (`limactl create` took a copy, and only lima's copy
    /// is read at boot). A build made the disk with `format: true`
    /// because there was nothing to lose yet; from here on a boot that
    /// cannot find the disk's label must fail rather than reformat it.
    /// The instance must be stopped: `limactl edit` refuses a running one
    /// ("cannot edit a running instance"), so callers go through
    /// [`Vm::repair_stale_format`], which knows the instance's state,
    /// rather than calling this on an instance they have not checked.
    pub(in crate::vm) fn stop_formatting_data_disk(&self) -> Result<()> {
        let name = self.lima_name();
        self.limactl_run(&["edit", &name, "--set", FORMAT_OFF])
    }

    /// `limactl create` from the template written by a build.
    pub(in crate::vm) fn lima_create(&self) -> Result<()> {
        let template = self.template_path();
        if !template.exists() {
            bail!("{} does not exist; run `ssf vm build`", template.display());
        }
        let name = self.lima_name();
        info!("creating lima instance {name} from {}", template.display());
        self.limactl_run_within(
            &["create", "--name", &name, &template.to_string_lossy()],
            CREATE_LIMIT,
        )?;
        let inst = self
            .lima_instance()?
            .context("new Lima instance is missing after create")?;
        super::super::write_private(&Path::new(&inst.dir).join("ssf-fresh-root-v2"), b"1\n")
    }

    /// Wait for `/etc/ssf-image-built` over `limactl shell`: present at
    /// once on a provisioned instance; on a first boot, until
    /// `provision.sh` has written it, or has ended without it (then the
    /// end of its log is the error).
    ///
    /// The loop keeps [`PROVISION_TIMEOUT`] itself, and this wraps the
    /// whole wait in a deadline as well: an `ssf vm build` was once found
    /// parked at an `.await` for fifteen minutes -- past `limactl start`,
    /// no child process, no output, no CPU -- and a limit that lives
    /// inside a loop cannot end a loop that is no longer running. The
    /// deadline starts here, after `limactl start` has returned, so the
    /// minutes the guest spends provisioning inside that call are not
    /// counted twice.
    pub(in crate::vm) async fn wait_for_provisioning(&self) -> Result<()> {
        let name = self.lima_name();
        let backstop = PROVISION_TIMEOUT + OWN_TIMEOUT_MARGIN;
        match tokio::time::timeout(backstop, self.provisioning_loop()).await {
            Ok(r) => r,
            Err(_) => bail!(
                "waiting for {name} to provision itself stopped making progress after {}; `limactl shell {name} sudo tail {PROVISION_LOG}` shows where it got to, and `ssf vm console` has its console",
                human_duration(backstop)
            ),
        }
    }

    pub(in crate::vm) async fn provisioning_loop(&self) -> Result<()> {
        let name = self.lima_name();
        let probe = provision_probe();
        let deadline = Instant::now() + PROVISION_TIMEOUT;
        let mut idle = 0;
        loop {
            // A probe that cannot run at all is usually transient: ssh in
            // the guest is not up yet, or one `limactl shell` timed out.
            // But `limactl shell` against a stopped or deleted instance
            // ("instance %q is stopped, run `limactl start %s`") is the
            // end, and reading that as "nothing seen" is what parked a
            // wait for half an hour in silence. So a failed probe asks
            // lima what the instance is doing, and only a definite answer
            // other than Running ends the wait.
            let out = match self
                .limactl_output_within(&["shell", &name, "sh", "-c", &probe], PROBE_LIMIT)
            {
                Ok(out) => out,
                Err(e) => {
                    match self.lima_instance() {
                        Ok(state) => {
                            if let Some(why) =
                                terminal_state(state.as_ref().map(|i| i.status.as_str()))
                            {
                                bail!(
                                    "{name} {why} while waiting for it to provision itself ({e:#}); `ssf vm console` has its console, and `limactl shell {name} sudo tail {PROVISION_LOG}` its log once it runs again"
                                );
                            }
                        }
                        // Two failures in a row say nothing definite:
                        // keep waiting, but not silently.
                        Err(list) => warn!(
                            "the provisioning probe did not run ({e:#}) and asking lima about {name} failed too ({list:#}); still waiting"
                        ),
                    }
                    debug!(error = format!("{e:#}"), "provisioning probe did not run");
                    String::new()
                }
            };
            let probed = parse_probe(&out);
            debug!(?probed, idle, "provisioning probe");
            match provision_step(probed, idle) {
                Step::Provisioned => return Ok(()),
                Step::Failed => {
                    let tail = self
                        .limactl_output_within(
                            &["shell", &name, "sudo", "tail", "-50", PROVISION_LOG],
                            PROBE_LIMIT,
                        )
                        .unwrap_or_default();
                    bail!(
                        "provisioning failed in {name} (no {PROVISION_MARKER}); the end of {PROVISION_LOG}:\n{}",
                        tail.trim_end()
                    );
                }
                Step::Wait(n) => idle = n,
            }
            if Instant::now() >= deadline {
                bail!(
                    "provisioning did not finish in {} minutes; `limactl shell {name} sudo tail {PROVISION_LOG}` shows where it is",
                    PROVISION_TIMEOUT.as_secs() / 60
                );
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    /// Write `share/` fresh, start the instance, wait for the guest.
    pub(in crate::vm) async fn lima_start(&self, host: &Config) -> Result<()> {
        let name = self.lima_name();
        let Some(inst) = self.lima_instance()? else {
            bail!("lima instance {name} does not exist; run `ssf vm build`");
        };
        // The boot below is what a `format: true` left by a build that
        // did not finish would reach, and by now the data disk holds the
        // factory. This is also the one point where the flag can still be
        // put right: the instance is stopped here, which is the only
        // state `limactl edit` accepts. A flag that survives the repair
        // (an instance already running) stops the start rather than
        // booting into it.
        if let Some(yaml) = self.repair_stale_format(Some(&inst), Why::FoundStale) {
            return Err(self.stale_format_error(&yaml));
        }
        require_safe_root(Path::new(&inst.dir))?;
        self.ensure_key()?;
        self.write_share(host)?;
        self.apply_sizes()?;
        self.limactl_start(&["start", "--timeout", &start_timeout_arg(), &name])?;
        // The template pins the port; an instance made from an older
        // template (or edited by hand) may listen elsewhere.
        if let Ok(Some(inst)) = self.lima_instance()
            && inst.ssh_local_port != 0
            && inst.ssh_local_port != self.cfg.ssh_port
        {
            warn!(
                "lima forwards {name}'s ssh to port {} but [vm] ssh_port is {}; `ssf vm build --force` remakes the instance to match",
                inst.ssh_local_port, self.cfg.ssh_port
            );
        }
        self.wait_for_provisioning().await?;
        if let Some(inst) = self.lima_instance()? {
            super::super::write_private(&Path::new(&inst.dir).join("ssf-safe-root-v2"), b"1\n")?;
            let _ = std::fs::remove_file(Path::new(&inst.dir).join("ssf-fresh-root-v2"));
        }
        if let Err(e) = self.wait_for_ssh(SEED_TIMEOUT).await {
            bail!(
                "{e:#}; the console is in {}",
                self.lima_console_log()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "lima's instance directory".into())
            );
        }
        // The guest has provisioned and answered as `ssf`: the data disk
        // is the factory's. The repair above ran before this boot, which
        // is where it can work; this is the same point `ssf vm build`
        // checks, and it is here so that a flag that came back (an
        // instance edited by hand, a template restored from elsewhere)
        // is said out loud rather than waiting for the next boot to be
        // discovered.
        if let Some(yaml) = self.repair_stale_format(
            self.lima_instance().ok().flatten().as_ref(),
            Why::FoundStale,
        ) {
            warn!("{}", self.stale_format_note(&yaml));
        }
        let daemon = self.wait_for_daemon(Duration::from_secs(60)).await;
        self.report_up(daemon.as_deref());
        Ok(())
    }

    /// `vcpus` and `mem_mib` from `config.toml` reach a stopped instance
    /// through `limactl edit`, so a change and `ssf vm restart` apply them
    /// as they do under Firecracker (the template itself is only rendered
    /// by `ssf vm build`).
    pub(in crate::vm) fn apply_sizes(&self) -> Result<()> {
        let Some(inst) = self.lima_instance()? else {
            return Ok(());
        };
        if inst.is_running() {
            return Ok(());
        }
        let sizes = self.sizes();
        let want_mem = u64::from(sizes.mem_mib) << 20;
        let mut args = vec!["edit".to_string(), inst.name.clone()];
        if inst.cpus != 0 && inst.cpus != sizes.vcpus {
            args.push(format!("--cpus={}", sizes.vcpus));
        }
        if inst.memory != 0 && inst.memory != want_mem {
            args.push(format!("--memory={}", sizes.mem_mib as f64 / 1024.0));
        }
        if args.len() == 2 {
            return Ok(());
        }
        println!(
            "applying [vm] vcpus = {} and mem_mib = {} to lima instance {}",
            sizes.vcpus, sizes.mem_mib, inst.name
        );
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.limactl_run(&args)
    }

    /// `limactl stop`, and `-f` when that fails (lima gives the guest a
    /// few minutes to shut down first).
    pub(in crate::vm) async fn lima_stop(&self) -> Result<()> {
        let name = self.lima_name();
        // "Not running" has to come from lima, not from a probe that
        // failed: `.ok().flatten()` printed "VM is not running" over a
        // running VM whenever `limactl list` had a bad moment, and the
        // VM was then left up.
        match self
            .lima_instance()
            .with_context(|| format!("asking lima whether {name} is running"))?
        {
            None => {
                println!("there is no lima instance {name}");
                return Ok(());
            }
            Some(inst) if !inst.is_running() => {
                println!(
                    "VM {} is not running{}",
                    self.cfg.name,
                    if inst.status.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", inst.status)
                    }
                );
                return Ok(());
            }
            Some(_) => {}
        }
        if let Err(e) = self.limactl_run_within(&["stop", &name], STOP_LIMIT) {
            warn!("{e:#}; forcing it");
            self.limactl_run_within(&["stop", "-f", &name], STOP_LIMIT)?;
        }
        println!("VM {} stopped", self.cfg.name);
        Ok(())
    }

    /// `limactl disk resize` (the VM stopped); the guest's seed script
    /// grows the filesystem on the next boot.
    pub(in crate::vm) fn lima_grow(&self, want: Option<u32>) -> Result<Option<u32>> {
        let disk = self.lima_disk_name();
        let d = self.lima_disk()?.with_context(|| {
            format!("lima disk {disk} does not exist yet; `ssf vm build` makes it at [vm] data_gib")
        })?;
        let current = gib_ceil(d.size);
        // The lima disks are lima's, not under `[vm] dir`: the warning
        // below has to be about the filesystem they are on.
        let (dir, _) = self.sizing_dir();
        let facts = HostFacts::probe(&dir)?;
        let rule = sizes_for(&facts).data_gib;
        let Some(target) = plan_grow(current, want, rule)? else {
            println!(
                "{disk} stays at {current} GiB{}",
                if want.is_none() {
                    format!(" (the rule for today's free space gives {rule} GiB)")
                } else {
                    String::new()
                }
            );
            return Ok(None);
        };
        if (u64::from(target) << 30).saturating_sub(d.size) > facts.free_bytes {
            warn!(
                "{target} GiB is more than the host has free ({} GiB on {}); a guest that fills the disk would see I/O errors before \"disk full\"",
                facts.free_bytes >> 30,
                facts.mount
            );
        }
        info!("resizing lima disk {disk}");
        self.limactl_run(&["disk", "resize", &disk, "--size", &format!("{target}GiB")])?;
        println!(
            "{disk} ({}) grown from {current} to {target} GiB; the guest grows its filesystem ({}) to match at the next boot (`ssf vm start`)",
            d.dir, d.mount_point
        );
        Ok(Some(target))
    }

    /// Delete the instance and create it again from its template; the
    /// data disk stays, and the next start provisions the fresh root.
    pub(in crate::vm) fn lima_reset(&self) -> Result<()> {
        let name = self.lima_name();
        // The template is what the new instance inherits, so a stale
        // `format: true` in it has to go before the create, not after:
        // the instance that is about to be deleted is not worth fixing,
        // which is why no instance is passed. What can still come back is
        // ssf's own template, when the rewrite of it failed -- and
        // creating an instance from a template that says `format: true`
        // is exactly the boot this whole path exists to stop, over a disk
        // that by now holds the factory. So the reset refuses, as every
        // other caller does.
        if let Some(yaml) = self.repair_stale_format(None, Why::FoundStale) {
            return Err(self.stale_format_error(&yaml));
        }
        if self.lima_instance()?.is_some() {
            self.limactl_run(&["delete", "-f", &name])?;
        }
        self.lima_create()?;
        println!(
            "lima instance {name} re-created from {}; the data disk {} stays; `ssf vm start` provisions it again (a few minutes)",
            self.template_path().display(),
            self.lima_disk_name()
        );
        Ok(())
    }

    /// Delete the instance and the data disk (the VM's directory goes
    /// after this). `Ok(false)`: lima had neither.
    ///
    /// A `limactl` that will not answer fails the step only when lima's
    /// home still holds something: there is then a VM here that ssf
    /// cannot delete, and saying so is the point. With nothing there,
    /// the same failure is just a tool that is not around any more, and
    /// `ssf uninstall` carries on.
    pub(in crate::vm) fn lima_destroy(&self) -> Result<bool> {
        let mut removed = false;
        let name = self.lima_name();
        let instance = match self.lima_instance() {
            Ok(i) => i,
            Err(e) => match self.lima_leftovers() {
                Some(false) => {
                    warn!("could not ask lima about {name} ({e:#}); its home holds nothing of it");
                    return Ok(false);
                }
                Some(true) => {
                    return Err(e).with_context(|| {
                        format!(
                            "lima's home still holds something of {name}, and lima cannot be asked about it"
                        )
                    });
                }
                None => {
                    return Err(e).with_context(|| {
                        format!(
                            "lima cannot be asked about {name}, and its own home could not be read or is not there"
                        )
                    });
                }
            },
        };
        if instance.is_some() {
            self.limactl_run(&["delete", "-f", &name])?;
            println!("deleted lima instance {name}");
            removed = true;
        }
        let disk = self.lima_disk_name();
        // The same rule as the instance above: a `limactl` that will not
        // answer is not the last word when lima's own filesystem can be
        // looked at. The survey already trusts `<lima home>/_disks/` for
        // the decision that *permits* the destroying, so refusing to
        // trust it for the one that declares the destroying finished
        // failed the step over a disk directory demonstrably not there.
        let held = match self.lima_disk() {
            Ok(d) => d.is_some(),
            Err(e) => match self.lima_disk_dir().and_then(|p| super::super::there(&p)) {
                Some(false) => {
                    warn!(
                        "could not ask lima about disk {disk} ({e:#}); its home holds no such disk"
                    );
                    false
                }
                _ => {
                    return Err(e).with_context(|| {
                        format!("lima cannot be asked about disk {disk}, and its home may hold it")
                    });
                }
            },
        };
        if held {
            self.limactl_run(&["disk", "delete", &disk])?;
            println!("deleted lima disk {disk}");
            removed = true;
        }
        let _ = std::fs::remove_file(self.unproven_disk());
        Ok(removed)
    }
}
