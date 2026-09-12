use super::*;

impl Vm {
    /// The lima instance: `ssf-<name>`.
    pub fn lima_name(&self) -> String {
        instance_name(&self.cfg.name)
    }

    /// The lima data disk: `ssf-<name>`.
    pub fn lima_disk_name(&self) -> String {
        disk_name(&self.cfg.name)
    }

    /// The host directory the guest mounts at `/mnt/ssf`.
    pub fn share_dir(&self) -> PathBuf {
        self.dir.join("share")
    }

    pub fn template_path(&self) -> PathBuf {
        self.dir.join("lima.yaml")
    }

    /// The guest's architecture: this machine's.
    pub(in crate::vm) fn lima_arch(&self) -> Result<&'static str> {
        lima_arch(std::env::consts::ARCH)
    }

    /// The template for this VM from `[vm]`. `format_disk` says whether
    /// lima may format the data disk: only the build that creates the
    /// disk passes true. lima's guest boot script formats a disk whose
    /// `lima-<disk>` label it cannot find and `format` is true, and then
    /// mounts the partition by device either way, so with it false a
    /// healthy disk still mounts and a damaged or mislabelled one fails
    /// loudly instead of being wiped (a label too long for ext4 once cost
    /// this VM its data disk). `vm/guest/seed.sh` states the same policy
    /// for Firecracker: checked and mounted, never formatted.
    pub fn lima_template(&self, format_disk: bool) -> Result<String> {
        Ok(render_template(&Template {
            disk: &self.lima_disk_name(),
            share: &self.share_dir(),
            ssh_port: self.cfg.ssh_port,
            sizes: self.sizes(),
            root_gib: self.cfg.root_gib,
            arch: self.lima_arch()?,
            image: self.cfg.image.as_deref(),
            vm_type: self.cfg.vm_type.as_deref(),
            format_disk,
        }))
    }

    // ---- limactl ----

    /// `limactl --tty=false ...`, from `[vm] limactl` or PATH, with this
    /// environment (`LIMA_HOME` and the rest are the person's).
    pub(in crate::vm) fn limactl(&self) -> Command {
        let program = match &self.cfg.limactl {
            Some(p) => expand_tilde(p),
            None => PathBuf::from("limactl"),
        };
        let mut cmd = Command::new(program);
        cmd.arg("--tty=false").stdin(Stdio::null());
        cmd
    }

    /// Run limactl for its output; stderr goes into the error. Every
    /// caller names its own bound: a local lookup, a create that
    /// downloads an image and a probe with a person waiting are not the
    /// same wait.
    pub(in crate::vm) fn limactl_output_within(
        &self,
        args: &[&str],
        limit: Duration,
    ) -> Result<String> {
        let mut cmd = self.limactl();
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        let label = limactl_label(args);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("running {}", self.limactl_hint()))?;
        // Drain both pipes while it runs: a child blocked writing into a
        // full pipe would look exactly like a hang, and be killed for it.
        let out = drain(child.stdout.take().expect("piped"));
        let err = drain(child.stderr.take().expect("piped"));
        let started = Instant::now();
        debug!(command = %label, "running limactl");
        let status = wait_within(&mut child, &label, limit)?;
        debug!(command = %label, ?status, elapsed = ?started.elapsed(), "limactl exited; draining its output");
        let stdout = out.join().unwrap_or_default();
        let stderr = err.join().unwrap_or_default();
        debug!(command = %label, elapsed = ?started.elapsed(), "limactl output drained");
        if !status.success() {
            bail!(
                "`{label}` failed ({status}): {}",
                String::from_utf8_lossy(&stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&stdout).to_string())
    }

    /// Run limactl with this terminal (its progress lines are worth
    /// seeing), within [`QUICK_LIMIT`].
    pub(in crate::vm) fn limactl_run(&self, args: &[&str]) -> Result<()> {
        self.limactl_run_within(args, QUICK_LIMIT)
    }

    /// [`Vm::limactl_run`] with a bound of its own: `create` downloads an
    /// image, `start` provisions a guest, `stop` waits for one.
    pub(in crate::vm) fn limactl_run_within(&self, args: &[&str], limit: Duration) -> Result<()> {
        let mut cmd = self.limactl();
        cmd.args(args);
        let label = limactl_label(args);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("running {}", self.limactl_hint()))?;
        let started = Instant::now();
        debug!(command = %label, "running limactl");
        let st = wait_within(&mut child, &label, limit)?;
        debug!(command = %label, status = ?st, elapsed = ?started.elapsed(), "limactl exited");
        if !st.success() {
            bail!("`{label}` failed ({st})");
        }
        Ok(())
    }

    pub(in crate::vm) fn limactl_hint(&self) -> String {
        match &self.cfg.limactl {
            Some(p) => format!("[vm] limactl = {p}"),
            None => "limactl (is lima installed and on PATH?)".to_string(),
        }
    }

    /// Which limactl ran, for a message about a limactl that *did* run:
    /// [`Vm::limactl_hint`] is written for one that could not be found,
    /// and reads as a question rather than an answer next to a version.
    pub(in crate::vm) fn limactl_where(&self) -> String {
        match &self.cfg.limactl {
            Some(p) => format!("[vm] limactl = {p}"),
            None => which("limactl").map_or_else(
                || "limactl on PATH".to_string(),
                |p| p.display().to_string(),
            ),
        }
    }

    /// The instance, when lima has it.
    pub(in crate::vm) fn lima_instance(&self) -> Result<Option<Instance>> {
        self.lima_instance_within(QUICK_LIMIT)
    }

    /// [`Vm::lima_instance`] with a bound of its own, for the liveness
    /// question that is asked on a waiting path.
    pub(in crate::vm) fn lima_instance_within(&self, limit: Duration) -> Result<Option<Instance>> {
        let name = self.lima_name();
        let out = self.limactl_output_within(&["list", "--json"], limit)?;
        Ok(parse_instances(&out).into_iter().find(|i| i.name == name))
    }

    /// The data disk, when lima has it.
    pub(in crate::vm) fn lima_disk(&self) -> Result<Option<Disk>> {
        self.lima_disk_within(QUICK_LIMIT)
    }

    /// [`Vm::lima_disk`] with a bound of its own, for the survey that
    /// asks it on a path with a person waiting.
    pub(in crate::vm) fn lima_disk_within(&self, limit: Duration) -> Result<Option<Disk>> {
        let name = self.lima_disk_name();
        let out = self.limactl_output_within(&["disk", "list", "--json"], limit)?;
        Ok(parse_disks(&out).into_iter().find(|d| d.name == name))
    }

    /// Is the instance running, or why could that not be asked? The
    /// probe forks a ~60 MB Go binary, so a fired resource limit or a
    /// fork that failed under load is a plausible answer, and it is not
    /// the same answer as "stopped".
    pub(in crate::vm) fn lima_running_probe(&self, limit: Duration) -> Result<bool> {
        self.lima_instance_within(limit)
            .map(|inst| inst.is_some_and(|i| i.is_running()))
            .with_context(|| format!("asking lima whether {} is running", self.lima_name()))
    }

    /// [`Vm::lima_running_probe`] for the callers that only want the
    /// answer, with "could not ask" as `None` and a warning in the log.
    /// `Vm::supervise` polls this, and a probe failure read as "stopped"
    /// once ended the supervisor with "the VM exited" over a VM that was
    /// running.
    pub(in crate::vm) fn lima_running_state(&self) -> Option<bool> {
        match self.lima_running_probe(QUICK_LIMIT) {
            Ok(running) => Some(running),
            Err(e) => {
                warn!("{e:#}");
                None
            }
        }
    }

    /// Where lima keeps this instance: `<lima home>/<name>`.
    pub(in crate::vm) fn lima_instance_dir(&self) -> Option<PathBuf> {
        self.lima_home.as_ref().map(|h| h.join(self.lima_name()))
    }

    /// Where lima keeps this VM's external data disk:
    /// `<lima home>/_disks/<disk>`.
    pub(in crate::vm) fn lima_disk_dir(&self) -> Option<PathBuf> {
        self.lima_home
            .as_ref()
            .map(|h| h.join("_disks").join(self.lima_disk_name()))
    }

    /// Is either of them on disk? What is left to go on when `limactl`
    /// will not answer. `None` when nobody could tell -- no lima home to
    /// look in, or a `stat` that was refused -- which is not the same as
    /// "nothing there", and `lima_destroy` reports `Some(false)` here as
    /// a destroy that finished.
    pub(in crate::vm) fn lima_leftovers(&self) -> Option<bool> {
        let (instance, disk) = (self.lima_instance_dir()?, self.lima_disk_dir()?);
        match (super::super::there(&instance), super::super::there(&disk)) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        }
    }

    /// What lima holds of this VM. The data disk is asked about as well
    /// as the instance: `limactl delete` of an instance leaves an
    /// external disk where it is, so a disk full of clones and worktrees
    /// can outlive the instance that mounted it -- and nothing can then
    /// be started to look inside it, which is why that case is not
    /// `startable`.
    pub(in crate::vm) fn lima_survey(&self) -> Survey {
        let dir = self.dir.exists();
        let instance = match self.lima_instance_within(SURVEY_LIMIT) {
            Ok(i) => i,
            Err(e) => return self.lima_unanswered(dir, "the instance", &self.lima_name(), &e),
        };
        let disk = match self.lima_disk_within(SURVEY_LIMIT) {
            Ok(d) => Some(d.is_some()),
            Err(e) => {
                warn!(
                    "could not ask lima about the data disk {}: {e:#}",
                    self.lima_disk_name()
                );
                // Lima's own filesystem, for the one answer whose loss
                // cannot be undone.
                self.lima_disk_dir().and_then(|p| super::super::there(&p))
            }
        };
        Survey {
            present: match (instance.is_some(), disk) {
                (true, _) | (false, Some(true)) => Some(true),
                (false, Some(false)) => Some(dir),
                (false, None) => dir.then_some(true),
            },
            running: Some(instance.as_ref().is_some_and(Instance::is_running)),
            startable: instance.is_some(),
            data: disk,
        }
    }

    /// A `limactl` question that came back an error.
    ///
    /// An unrunnable or silent `limactl` -- moved by an upgrade, off the
    /// PATH the service runs under, a stale `[vm] limactl`, a locked
    /// lima home -- is evidence about the tool, not about the machine.
    /// So lima's own filesystem answers instead: `<lima home>/<name>`
    /// for the instance, `<lima home>/_disks/<disk>` for the data disk.
    /// Nothing there is a real "no VM", and a Mac that never built one
    /// gets its clean `ssf uninstall`. Something there is a VM that
    /// cannot be asked about -- never a missing binary's licence to
    /// treat a disk full of workspaces as absent.
    pub(in crate::vm) fn lima_unanswered(
        &self,
        dir: bool,
        what: &str,
        name: &str,
        e: &anyhow::Error,
    ) -> Survey {
        warn!("could not ask lima about {what} {name}: {e:#}");
        let instance = self
            .lima_instance_dir()
            .and_then(|p| super::super::there(&p));
        let disk = self.lima_disk_dir().and_then(|p| super::super::there(&p));
        let here = instance == Some(true) || disk == Some(true);
        let nothing = instance == Some(false) && disk == Some(false);
        Survey {
            present: if here || dir {
                Some(true)
            } else if nothing {
                Some(false)
            } else {
                None
            },
            // Nothing of it anywhere is not running; anything else is a
            // question that was never answered, and `ssf vm start` is no
            // cure for it.
            running: nothing.then_some(false),
            startable: false,
            data: disk,
        }
    }

    /// The data disk's size as lima has it, else what a build would make.
    pub(in crate::vm) fn lima_data_cap_gib(&self) -> u32 {
        match self.lima_disk() {
            Ok(Some(d)) => gib_ceil(d.size),
            _ => self.sizes().data_gib,
        }
    }

    /// The instance's serial console (`serial.log`; `serialv.log` where
    /// lima writes the virtio console instead).
    pub(in crate::vm) fn lima_console_log(&self) -> Result<PathBuf> {
        let inst = self.lima_instance()?.with_context(|| {
            format!(
                "lima instance {} does not exist; run `ssf vm build`",
                self.lima_name()
            )
        })?;
        let dir = PathBuf::from(inst.dir);
        let name = self.lima_name();
        // Whichever lima wrote, and only if it is there: returning a path
        // to a file that does not exist left `ssf vm console` showing
        // `tail: cannot open ...serial.log`, which reads as a broken
        // command rather than as "this instance has never been booted".
        for log in ["serial.log", "serialv.log"] {
            let p = dir.join(log);
            if p.exists() {
                return Ok(p);
            }
        }
        bail!(
            "lima instance {name} has no console log yet ({}); lima writes serial.log (serialv.log with a virtio console) at the first `ssf vm start`",
            dir.display()
        )
    }

    /// What a build needs: limactl that runs, and qemu for the
    /// architecture wherever lima will drive the VM with it -- always on
    /// Linux (its only driver there), and on macOS when `[vm] vm_type`
    /// asks for qemu. Keying that on the operating system alone let a Mac
    /// with `vm_type = "qemu"` and no qemu through preflight, to fail
    /// inside `limactl create` instead.
    pub(in crate::vm) fn lima_preflight(&self) -> Result<()> {
        check_name(&self.cfg.name)?;
        let arch = self.lima_arch()?;
        let os = std::env::consts::OS;
        let vm_type = self.cfg.vm_type.as_deref();
        let version = match self.limactl_output_within(&["--version"], PROBE_LIMIT) {
            Ok(out) => out,
            Err(e) => bail!(
                "limactl does not run ({}): {e:#}; install lima {MIN_LIMA} or newer (`brew install lima` on macOS, the `lima` package on Linux) or set [vm] limactl to it",
                self.limactl_hint()
            ),
        };
        // The version was fetched anyway to see that limactl runs, so it
        // is read: an older lima does not fail here, it fails inside
        // `limactl create` on a base image it could not resolve, which
        // does not name the reason.
        match parse_lima_version(&version) {
            Some(v) if v < MIN_LIMA => bail!(
                "this is lima {v} ({}), and ssf needs {MIN_LIMA} or newer: the VM template names its base image the way lima 2.0 spells a template locator, and leaves the share's mount type to lima, whose default for qemu is 9p -- mounted before the guest provisions itself -- only from lima 1.0. Upgrade lima (`brew upgrade lima` on macOS, your distribution's package or lima's release tarball on Linux), or point [vm] limactl at a newer one",
                self.limactl_where()
            ),
            Some(_) => {}
            None => warn!(
                "no version in `limactl --version` ({}); ssf needs lima {MIN_LIMA} or newer",
                version.trim()
            ),
        }
        if !platform::is_macos() && vm_type == Some("vz") {
            bail!("[vm] vm_type = \"vz\" is macOS only; unset it or use \"qemu\" here");
        }
        if super::super::lima_uses_qemu(os, vm_type) {
            let qemu = format!("qemu-system-{arch}");
            if which(&qemu).is_none() {
                bail!(
                    "{qemu} is not on PATH; {}",
                    super::super::qemu_install_hint(os, arch)
                );
            }
        }
        Ok(())
    }

    // ---- share ----

    /// Write `share/` fresh: the guest scripts, the seed tree with
    /// `lima.env`, and a herdr binary for the guest when the host has one.
    pub(in crate::vm) fn write_share(&self, host: &Config) -> Result<()> {
        let share = self.share_dir();
        std::fs::create_dir_all(&share)?;
        let guest = share.join("guest");
        let _ = std::fs::remove_dir_all(&guest);
        copy_dir(&scripts_dir()?.join("guest"), &guest)?;
        let seed = share.join("seed");
        self.seed_tree(host, &seed)?;
        std::fs::write(seed.join("lima.env"), lima_env(&self.cfg.name))?;
        let herdr = share.join("herdr");
        let _ = std::fs::remove_file(&herdr);
        if let Some(src) = self.guest_herdr()? {
            std::fs::copy(&src, &herdr).with_context(|| format!("copying {}", src.display()))?;
            make_executable(&herdr)?;
        }
        Ok(())
    }

    /// A herdr binary for the guest: `[vm] herdr`, or the host's own on a
    /// Linux host (the guest's architecture is the host's); `None` lets
    /// `provision.sh` download herdr's release.
    pub(in crate::vm) fn guest_herdr(&self) -> Result<Option<PathBuf>> {
        if let Some(p) = &self.cfg.herdr {
            let p = expand_tilde(p);
            if !p.is_file() {
                bail!("[vm] herdr {} does not exist", p.display());
            }
            return Ok(Some(p));
        }
        if std::env::consts::OS != "linux" {
            return Ok(None);
        }
        Ok(which(&HerdrConfig::default().command).or_else(|| which("herdr")))
    }

    // ---- build / start / stop ----
}
