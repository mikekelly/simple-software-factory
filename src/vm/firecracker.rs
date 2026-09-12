use super::*;

impl Vm {
    pub fn new(cfg: &Config) -> Self {
        let base = expand_tilde(&cfg.vm.dir);
        let dir = base.join(&cfg.vm.name);
        Self {
            cfg: cfg.vm.clone(),
            base,
            dir,
            lima_home: lima::lima_home(),
            binary: None,
        }
    }

    /// The backend in effect: `[vm] backend`, else Firecracker on Linux
    /// and lima on macOS.
    pub fn backend(&self) -> BackendKind {
        self.cfg
            .backend
            .unwrap_or_else(BackendKind::platform_default)
    }

    /// The serial console log: Firecracker's `console.log` next to the
    /// disks, or lima's `serial.log` in the instance directory.
    pub fn console_path(&self) -> Result<PathBuf> {
        match self.backend() {
            BackendKind::Firecracker => Ok(self.console_log()),
            BackendKind::Lima => self.lima_console_log(),
        }
    }

    pub(in crate::vm) fn binary(&self) -> Result<PathBuf> {
        match &self.binary {
            Some(p) => Ok(p.clone()),
            None => crate::client_executable()
                .and_then(std::fs::canonicalize)
                .context("locating the ssf client binary"),
        }
    }

    // ---- files ----

    pub(in crate::vm) fn asset(&self, chosen: &Option<String>, name: &str) -> PathBuf {
        match chosen {
            Some(p) => expand_tilde(p),
            None => self.base.join(name),
        }
    }
    pub fn firecracker(&self) -> PathBuf {
        self.asset(&self.cfg.firecracker, "firecracker")
    }
    pub fn gvproxy(&self) -> PathBuf {
        self.asset(&self.cfg.gvproxy, "gvproxy")
    }
    pub fn gvforwarder(&self) -> PathBuf {
        self.base.join("gvforwarder")
    }
    pub fn kernel(&self) -> PathBuf {
        self.asset(&self.cfg.kernel, "vmlinux")
    }
    pub fn rootfs(&self) -> PathBuf {
        self.asset(&self.cfg.rootfs, "rootfs.ext4")
    }
    pub(in crate::vm) fn root_disk(&self) -> PathBuf {
        self.dir.join("root.ext4")
    }
    /// A Firecracker data disk left in the VM's own directory by a
    /// change of backend, if there could be one.
    ///
    /// `[vm] dir` is shared by the two backends and `data.ext4` is
    /// Firecracker's name for its disk, so switching `[vm] backend` to
    /// lima with `[vm] name` kept leaves the Firecracker VM's clones and
    /// worktrees in `<[vm] dir>/<name>` -- where lima knows nothing of
    /// them and [`Vm::destroy`] removes the directory with them in it.
    ///
    /// `Some` when the file is there *or* when nobody could tell, since
    /// both are reasons not to destroy the directory unasked. `None`
    /// under Firecracker, where the same file is simply this VM's own
    /// disk.
    pub fn stranded_data_disk(&self) -> Option<PathBuf> {
        (self.backend() == BackendKind::Lima && there(&self.data_disk()) != Some(false))
            .then(|| self.data_disk())
    }

    pub(in crate::vm) fn data_disk(&self) -> PathBuf {
        self.dir.join("data.ext4")
    }
    pub(in crate::vm) fn seed_disk(&self) -> PathBuf {
        self.dir.join("seed.ext4")
    }
    pub(in crate::vm) fn fc_pid(&self) -> PathBuf {
        self.dir.join("firecracker.pid")
    }
    pub(in crate::vm) fn gv_pid(&self) -> PathBuf {
        self.dir.join("gvproxy.pid")
    }
    pub fn console_log(&self) -> PathBuf {
        self.dir.join("console.log")
    }
    pub(in crate::vm) fn key(&self) -> PathBuf {
        self.dir.join("id_ed25519")
    }
    pub(in crate::vm) fn known_hosts(&self) -> PathBuf {
        self.dir.join("known_hosts")
    }

    // ---- sizes ----

    /// The directory whose free space this VM is sized against, and how
    /// to name it: [`sizing_dir`] for this VM's backend.
    pub fn sizing_dir(&self) -> (PathBuf, &'static str) {
        sizing_dir(self.backend(), &self.base)
    }

    /// The sizes this VM runs at: `[vm]` where set, the rule for this
    /// machine where not (the minimums when the machine cannot be read).
    pub fn sizes(&self) -> Sizes {
        let c = &self.cfg;
        let rule = if c.vcpus.is_none() || c.mem_mib.is_none() || c.data_gib.is_none() {
            HostFacts::probe(&self.sizing_dir().0)
                .map(|f| sizes_for(&f))
                .unwrap_or(Sizes::MIN)
        } else {
            Sizes::MIN
        };
        Sizes {
            vcpus: c.vcpus.unwrap_or(rule.vcpus),
            mem_mib: c.mem_mib.unwrap_or(rule.mem_mib),
            data_gib: c.data_gib.unwrap_or(rule.data_gib),
        }
    }

    /// The data disk's cap in GiB: the disk's size once it exists, else
    /// what a start would make.
    pub fn data_cap_gib(&self) -> u32 {
        match self.backend() {
            BackendKind::Firecracker => match std::fs::metadata(self.data_disk()) {
                Ok(m) => u32::try_from(m.len().div_ceil(1 << 30)).unwrap_or(u32::MAX),
                Err(_) => self.sizes().data_gib,
            },
            BackendKind::Lima => self.lima_data_cap_gib(),
        }
    }

    /// Enlarge the data disk to `want` GiB, or to the rule for today's
    /// free space, keeping what is on it, with the VM stopped. Never
    /// shrinks. Returns the new size, or `None` when there was nothing to
    /// do. Firecracker: the filesystem checked, the file lengthened, the
    /// filesystem resized to fill it. lima: `limactl disk resize`; the
    /// guest's seed script resizes the filesystem on the next boot.
    pub fn grow(&self, want: Option<u32>) -> Result<Option<u32>> {
        if self.running() {
            bail!(
                "VM {} is running; stop it first (`{}` when the service owns it, else `ssf vm stop`), grow, then start it again",
                self.cfg.name,
                platform::service_hint("stop")
            );
        }
        match self.backend() {
            BackendKind::Firecracker => self.fc_grow(want),
            BackendKind::Lima => self.lima_grow(want),
        }
    }

    pub(in crate::vm) fn fc_grow(&self, want: Option<u32>) -> Result<Option<u32>> {
        let disk = self.data_disk();
        let meta = std::fs::metadata(&disk).with_context(|| {
            format!(
                "{} does not exist yet; `ssf vm start` makes the data disk at [vm] data_gib",
                disk.display()
            )
        })?;
        let current = u32::try_from(meta.len().div_ceil(1 << 30)).unwrap_or(u32::MAX);
        let facts = HostFacts::probe(&self.base)?;
        let rule = sizes_for(&facts).data_gib;
        let Some(target) = plan_grow(current, want, rule)? else {
            println!(
                "{} stays at {current} GiB{}",
                disk.display(),
                if want.is_none() {
                    format!(" (the rule for today's free space gives {rule} GiB)")
                } else {
                    String::new()
                }
            );
            return Ok(None);
        };
        // What the guest could still write, against what the host has.
        let allocated = std::os::unix::fs::MetadataExt::blocks(&meta) * 512;
        if (u64::from(target) << 30).saturating_sub(allocated) > facts.free_bytes {
            warn!(
                "{target} GiB is more than the host has free ({} GiB on {}); a guest that fills the disk would see I/O errors before \"disk full\"",
                facts.free_bytes >> 30,
                facts.mount
            );
        }
        info!("checking and resizing {}", disk.display());
        grow_image(&disk, u64::from(target) << 30)?;
        println!("{} grown from {current} to {target} GiB", disk.display());
        Ok(Some(target))
    }

    /// `df` of the data disk inside the guest.
    pub fn guest_disk_use(&self) -> Result<DiskUse> {
        let out = self.ssh_output(&["df", "-B1", "--output=used,avail,size", GUEST_DATA_DIR])?;
        parse_df(&out).context("unexpected df output")
    }

    // ---- processes ----

    pub(in crate::vm) fn pid_of(&self, file: &Path, program: &str) -> Option<u32> {
        let pid: u32 = std::fs::read_to_string(file).ok()?.trim().parse().ok()?;
        pid_runs(pid, program).then_some(pid)
    }

    pub fn firecracker_pid(&self) -> Option<u32> {
        self.pid_of(&self.fc_pid(), "firecracker")
    }

    pub fn gvproxy_pid(&self) -> Option<u32> {
        self.pid_of(&self.gv_pid(), "gvproxy")
    }

    /// Is the guest up? Firecracker: its PID file names a live
    /// firecracker; lima: `limactl list` says `Running`. A probe that
    /// could not be made counts as not running; use
    /// [`Vm::running_state`] where that difference matters.
    pub fn running(&self) -> bool {
        self.running_state().unwrap_or(false)
    }

    /// [`Vm::running`], with "the question could not be asked" kept apart
    /// from "no": `None` when the probe itself failed. Under Firecracker
    /// the probe is a PID file read, which answers either way; under lima
    /// it forks `limactl`, and one fork that fails is not the guest
    /// exiting. The callers that act on "the VM is gone" -- the
    /// supervisor and the ssh wait -- use this, so that a transient
    /// `limactl` failure cannot end them with "the VM exited".
    pub fn running_state(&self) -> Option<bool> {
        match self.backend() {
            BackendKind::Firecracker => Some(self.firecracker_pid().is_some()),
            BackendKind::Lima => self.lima_running_state(),
        }
    }

    /// The same question as [`Vm::running_state`], asked the way the gate
    /// in front of every forwarded command needs it: the reason comes
    /// back with a failure, so the command can say why it cannot tell
    /// rather than leaving it in the log, and the lima probe is held to
    /// [`lima::LIVENESS_LIMIT`] rather than the listing's own bound,
    /// because a person is waiting on this one and an answer it does not
    /// get is one it carries on without.
    pub fn running_now(&self) -> Result<bool> {
        match self.backend() {
            BackendKind::Firecracker => Ok(self.firecracker_pid().is_some()),
            BackendKind::Lima => self.lima_running_probe(lima::LIVENESS_LIMIT),
        }
    }

    /// What is here of this VM, asked of the backend in one pass:
    /// `ssf uninstall` needs every answer and each one costs a `limactl`
    /// fork under lima, so they are taken together.
    pub fn survey(&self) -> Survey {
        match self.backend() {
            BackendKind::Firecracker => {
                let dir = self.dir.exists();
                let running = self.firecracker_pid().is_some();
                Survey {
                    present: Some(dir || running),
                    running: Some(running),
                    startable: dir,
                    // `there`, not `exists()`: a `[vm] dir` that
                    // could not be read is not a VM without a data
                    // disk. `present` keeps `exists()` -- `Some(false)`
                    // there skips the destroy rather than running it.
                    data: there(&self.data_disk()),
                }
            }
            BackendKind::Lima => self.lima_survey(),
        }
    }

    // ---- build ----

    /// Make the guest: Firecracker downloads what is missing, makes the
    /// base image from the Ubuntu root tarball and boots it once to
    /// provision it; lima creates the instance from a cloud image and
    /// boots it once so the guest scripts provision it.
    pub async fn build(&self, host: &Config, force: bool) -> Result<()> {
        match self.backend() {
            BackendKind::Firecracker => self.fc_build(force).await,
            BackendKind::Lima => self.lima_build(host, force).await,
        }
    }

    pub(in crate::vm) async fn fc_build(&self, force: bool) -> Result<()> {
        if std::env::consts::OS != "linux" || std::env::consts::ARCH != "x86_64" {
            bail!(
                "the Firecracker backend is Linux x86_64 only (Firecracker, gvproxy and the guest kernel are downloaded for it); this machine is {} {}; use `ssf config set vm.backend lima`",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
        }
        let scripts = scripts_dir()?;
        std::fs::create_dir_all(self.base.join("dl"))
            .with_context(|| format!("creating {}", self.base.display()))?;
        self.fetch_assets().await?;
        if self.rootfs().exists() && !force {
            println!(
                "{} exists; `ssf vm build --force` makes a new one",
                self.rootfs().display()
            );
            return Ok(());
        }
        let herdr = which(
            &crate::config::herdr_command_path(&crate::config::HerdrConfig::default().command)
                .to_string_lossy(),
        )
        .or_else(|| which(&crate::config::herdr_command_path("herdr").to_string_lossy()))
        .context("herdr is not installed on this machine; the image takes its binary from here")?;
        let build = self.base.join("build");
        std::fs::create_dir_all(&build)?;
        let base = build.join("base.ext4");
        let tarball = self.base.join("dl/ubuntu-24.04-root.tar.xz");
        info!("making the base image from {}", tarball.display());
        let st = Command::new(scripts.join("make-base.sh"))
            .arg(&tarball)
            .arg(&base)
            .arg(self.cfg.root_gib.to_string())
            .arg(scripts.join("guest"))
            .arg(&herdr)
            .arg(self.gvforwarder())
            .status()
            .context("running make-base.sh (are fakeroot, bsdtar and mkfs.ext4 installed?)")?;
        if !st.success() {
            bail!("make-base.sh failed ({st})");
        }
        // The provisioning boot: root writable, the provisioning init as
        // PID 1, network through a gvproxy of its own.
        let console = build.join("console.log");
        let _ = std::fs::remove_file(&console);
        let boot = self.boot_files(&build);
        std::fs::write(
            &boot.config,
            serde_json::to_string_pretty(&self.fc_config_json(
                &[(&base, false)],
                &boot,
                Some("/usr/local/lib/ssf/provision-init.sh"),
            ))?,
        )?;
        clean_sockets(&boot);
        // The build's own gvproxy, next to a VM that may be running.
        let port = self
            .cfg
            .ssh_port
            .checked_add(1)
            .unwrap_or_else(|| self.cfg.ssh_port.saturating_sub(1));
        let gv = self.spawn_gvproxy(&boot, port)?;
        let result = self.provision(&boot, &console).await;
        kill(gv, libc::SIGTERM);
        result?;
        std::fs::rename(&base, self.rootfs())
            .with_context(|| format!("moving the image to {}", self.rootfs().display()))?;
        println!("built {}", self.rootfs().display());
        Ok(())
    }

    pub(in crate::vm) async fn provision(&self, boot: &BootFiles, console: &Path) -> Result<()> {
        info!("booting the image once to provision it (a few minutes)");
        let out = std::fs::File::create(console)?;
        let mut child = Command::new(self.firecracker())
            .arg("--api-sock")
            .arg(&boot.api)
            .arg("--config-file")
            .arg(&boot.config)
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .spawn()
            .with_context(|| format!("starting {}", self.firecracker().display()))?;
        // Show the guest's progress (not the kernel's boot messages).
        let path = console.to_path_buf();
        let tail = tokio::spawn(async move {
            let mut seen = 0usize;
            loop {
                if let Ok(text) = tokio::fs::read_to_string(&path).await {
                    for line in text[seen.min(text.len())..].lines() {
                        if !line.starts_with('[') && !line.trim().is_empty() {
                            eprintln!("  {line}");
                        }
                    }
                    seen = text.len();
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
        let status = loop {
            if let Some(st) = child.try_wait()? {
                break st;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        tail.abort();
        let log = std::fs::read_to_string(console).unwrap_or_default();
        if !log.contains("ssf-provision: DONE") {
            let tail: Vec<&str> = log.lines().rev().take(30).collect::<Vec<_>>();
            bail!(
                "provisioning failed (firecracker {status}); the end of {}:\n{}",
                console.display(),
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            );
        }
        Ok(())
    }

    pub(in crate::vm) async fn fetch_assets(&self) -> Result<()> {
        let dl = self.base.join("dl");
        if !self.firecracker().exists() {
            let tgz = dl.join("firecracker.tgz");
            download(&firecracker_url(), &tgz).await?;
            let out = Command::new("tar")
                .arg("-xzf")
                .arg(&tgz)
                .arg("-C")
                .arg(&dl)
                .status()?;
            if !out.success() {
                bail!("unpacking {} failed", tgz.display());
            }
            let bin = dl
                .join(format!("release-{FIRECRACKER_VERSION}-x86_64"))
                .join(format!("firecracker-{FIRECRACKER_VERSION}-x86_64"));
            std::fs::copy(&bin, self.firecracker())
                .with_context(|| format!("no {} in the release tarball", bin.display()))?;
            make_executable(&self.firecracker())?;
        }
        if !self.gvproxy().exists() {
            download(&gvproxy_url("gvproxy-linux-amd64"), &self.gvproxy()).await?;
            make_executable(&self.gvproxy())?;
        }
        if !self.gvforwarder().exists() {
            download(&gvproxy_url("gvforwarder"), &self.gvforwarder()).await?;
            make_executable(&self.gvforwarder())?;
        }
        if !self.kernel().exists() {
            download(KERNEL_URL, &self.kernel()).await?;
        }
        let tarball = dl.join("ubuntu-24.04-root.tar.xz");
        if !tarball.exists() {
            download(UBUNTU_ROOT_URL, &tarball).await?;
        }
        verify_sha256(&tarball, UBUNTU_ROOT_SHA256)?;
        Ok(())
    }

    // ---- start / stop ----

    pub(in crate::vm) fn boot_files(&self, dir: &Path) -> BootFiles {
        BootFiles {
            config: dir.join("firecracker.json"),
            api: dir.join("firecracker.sock"),
            vsock: dir.join("v.sock"),
            gv_api: dir.join("gvproxy.sock"),
            gv_log: dir.join("gvproxy.log"),
        }
    }

    /// Firecracker's config: the kernel, the drives in order (`/dev/vda`
    /// first), the machine size and the vsock device.
    pub fn fc_config_json(
        &self,
        drives: &[(&Path, bool)],
        boot: &BootFiles,
        init: Option<&str>,
    ) -> Value {
        let mut args = String::from(
            "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw random.trust_cpu=on",
        );
        if let Some(i) = init {
            args.push_str(&format!(" init={i}"));
        }
        let sizes = self.sizes();
        let drives: Vec<Value> = drives
            .iter()
            .enumerate()
            .map(|(i, (path, ro))| {
                json!({
                    "drive_id": format!("drive{i}"),
                    "path_on_host": path.to_string_lossy(),
                    "is_root_device": i == 0,
                    "is_read_only": ro,
                })
            })
            .collect();
        json!({
            "boot-source": {
                "kernel_image_path": self.kernel().to_string_lossy(),
                "boot_args": args,
            },
            "drives": drives,
            "machine-config": {
                "vcpu_count": sizes.vcpus,
                "mem_size_mib": sizes.mem_mib,
            },
            "vsock": {
                "guest_cid": GUEST_CID,
                "uds_path": boot.vsock.to_string_lossy(),
            },
        })
    }

    pub(in crate::vm) fn spawn_gvproxy(&self, boot: &BootFiles, ssh_port: u16) -> Result<u32> {
        let net = format!("unix://{}_{NET_PORT}", boot.vsock.display());
        let api = format!("unix://{}", boot.gv_api.display());
        let mut cmd = Command::new(self.gvproxy());
        cmd.args(["-listen", &net, "-listen", &api, "-mtu", "1500"])
            .args(["-ssh-port", &ssh_port.to_string()])
            .arg("-log-file")
            .arg(&boot.gv_log);
        let pid = spawn_detached(&mut cmd, None)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !boot
            .vsock
            .with_extension(format!("sock_{NET_PORT}"))
            .exists()
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(100));
        }
        if !pid_runs(pid, "gvproxy") {
            bail!(
                "gvproxy exited at once (port {ssh_port} taken?); see {}",
                boot.gv_log.display()
            );
        }
        Ok(pid)
    }

    /// Start the VM with a fresh seed and wait for ssh and the daemon.
    pub async fn start(&self, host: &Config) -> Result<()> {
        if self.running() {
            println!("VM {} is already running", self.cfg.name);
            return self.ensure_factory_ownership(host);
        }
        match self.backend() {
            BackendKind::Firecracker => self.fc_start(host).await,
            BackendKind::Lima => self.lima_start(host).await,
        }?;
        self.ensure_factory_ownership(host)
    }

    /// Stop the VM cleanly.
    pub async fn stop(&self) -> Result<()> {
        match self.backend() {
            BackendKind::Firecracker => self.fc_stop().await,
            BackendKind::Lima => self.lima_stop().await,
        }
    }

    /// The per-VM ssh key, made on first use.
    pub(in crate::vm) fn ensure_key(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        set_mode(&self.dir, 0o700)?;
        if !self.key().exists() {
            run_ok(
                Command::new("ssh-keygen")
                    .args(["-q", "-t", "ed25519", "-N", "", "-C", "ssf-vm", "-f"])
                    .arg(self.key()),
                "ssh-keygen",
            )?;
        }
        Ok(())
    }

    /// Print how the guest came up, after `wait_for_daemon`.
    pub(in crate::vm) fn report_up(&self, daemon: Option<&str>) {
        println!(
            "VM {} is up: ssh -p {} (ssf vm ssh), daemon {}",
            self.cfg.name,
            self.cfg.ssh_port,
            daemon.unwrap_or("unknown")
        );
        if daemon != Some("active") {
            eprintln!("the guest daemon is not active; `ssf vm logs` has its journal");
        }
    }

    /// Firecracker: make the disks if missing, write the seed disk, start
    /// gvproxy and Firecracker detached.
    pub(in crate::vm) async fn fc_start(&self, host: &Config) -> Result<()> {
        for (what, path) in [
            ("Firecracker", self.firecracker()),
            ("gvproxy", self.gvproxy()),
            ("the guest kernel", self.kernel()),
            ("the root image", self.rootfs()),
        ] {
            if !path.exists() {
                bail!("{what} is missing ({}); run `ssf vm build`", path.display());
            }
        }
        self.ensure_key()?;
        if !self.root_disk().exists() {
            info!("making {} from the image", self.root_disk().display());
            let st = Command::new("cp")
                .args(["--reflink=auto", "--sparse=always"])
                .arg(self.rootfs())
                .arg(self.root_disk())
                .status()?;
            if !st.success() {
                bail!("copying the image failed");
            }
        }
        self.require_compatible_root()?;
        if !self.data_disk().exists() {
            let gib = self.sizes().data_gib;
            info!("making {} ({gib} GiB, sparse)", self.data_disk().display());
            let f = std::fs::File::create(self.data_disk())?;
            f.set_len(u64::from(gib) << 30)?;
            drop(f);
            run_ok(
                Command::new("mkfs.ext4")
                    .args(["-q", "-L", "ssf-data"])
                    .arg(self.data_disk()),
                "mkfs.ext4",
            )?;
        }
        self.write_seed(host)?;
        let boot = self.boot_files(&self.dir);
        std::fs::write(
            &boot.config,
            serde_json::to_string_pretty(&self.fc_config_json(
                &[
                    (&self.root_disk(), false),
                    (&self.data_disk(), false),
                    (&self.seed_disk(), true),
                ],
                &boot,
                None,
            ))?,
        )?;
        clean_sockets(&boot);
        let _ = std::fs::remove_file(self.console_log());
        let gv = self.spawn_gvproxy(&boot, self.cfg.ssh_port)?;
        std::fs::write(self.gv_pid(), gv.to_string())?;
        let mut cmd = Command::new(self.firecracker());
        cmd.arg("--api-sock")
            .arg(&boot.api)
            .arg("--config-file")
            .arg(&boot.config);
        let fc = match spawn_detached(&mut cmd, Some(&self.console_log())) {
            Ok(pid) => pid,
            Err(e) => {
                kill(gv, libc::SIGTERM);
                return Err(e);
            }
        };
        std::fs::write(self.fc_pid(), fc.to_string())?;
        info!(pid = fc, "firecracker started");
        if let Err(e) = self.wait_for_ssh(Duration::from_secs(90)).await {
            bail!("{e:#}; the console is in {}", self.console_log().display());
        }
        let daemon = self.wait_for_daemon(Duration::from_secs(60)).await;
        self.report_up(daemon.as_deref());
        Ok(())
    }

    /// Shut the guest down (Ctrl-Alt-Del through Firecracker's API, which
    /// systemd turns into a reboot that ends the VM), then gvproxy.
    pub(in crate::vm) async fn fc_stop(&self) -> Result<()> {
        let Some(fc) = self.firecracker_pid() else {
            if let Some(gv) = self.gvproxy_pid() {
                kill(gv, libc::SIGTERM);
            }
            println!("VM {} is not running", self.cfg.name);
            return Ok(());
        };
        let boot = self.boot_files(&self.dir);
        match fc_api(
            &boot.api,
            "PUT",
            "/actions",
            json!({"action_type": "SendCtrlAltDel"}),
        )
        .await
        {
            Ok(()) => {}
            Err(e) => warn!("could not ask the guest to shut down ({e:#}); killing it"),
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        while pid_runs(fc, "firecracker") && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if pid_runs(fc, "firecracker") {
            warn!("the guest did not shut down in time; killing it");
            kill(fc, libc::SIGKILL);
        }
        if let Some(gv) = self.gvproxy_pid() {
            kill(gv, libc::SIGTERM);
        }
        let _ = std::fs::remove_file(self.fc_pid());
        let _ = std::fs::remove_file(self.gv_pid());
        clean_sockets(&boot);
        println!("VM {} stopped", self.cfg.name);
        Ok(())
    }

    /// `ssf-server` on the host with `[vm] enabled`: start the VM and stay
    /// until it ends or we are told to stop, shutting it down cleanly then.
    pub async fn supervise(&self, host: &Config) -> Result<()> {
        self.start(host).await?;
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut int = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let every = supervise_interval(self.backend());
        let mut unanswered = 0u32;
        loop {
            tokio::select! {
                _ = term.recv() => break,
                _ = int.recv() => break,
                _ = tokio::time::sleep(every) => {
                    // Only a definite "not running" ends the supervision.
                    // A probe that could not be made says nothing (it has
                    // logged itself); ending on one killed the daemon
                    // with "the VM exited" over a live VM.
                    match self.running_state() {
                        Some(true) => unanswered = 0,
                        None => {
                            unanswered += 1;
                            if unanswered >= MAX_UNANSWERED_PROBES {
                                return Err(cannot_tell_error(
                                    self.backend(),
                                    unanswered,
                                    every,
                                ));
                            }
                            warn!(
                                "could not tell whether the VM is running ({unanswered} probe(s) in a row); still supervising it"
                            );
                        }
                        Some(false) => {
                            warn!("the VM exited");
                            if let Some(gv) = self.gvproxy_pid() {
                                kill(gv, libc::SIGTERM);
                            }
                            bail!(
                                "the VM exited; see {}",
                                self.console_path()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_else(|_| "`ssf vm console`".into())
                            );
                        }
                    }
                }
            }
        }
        info!("stopping the VM");
        self.stop().await
    }
}
