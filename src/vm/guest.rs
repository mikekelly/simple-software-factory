use super::*;

/// The host config sections that stay on a VM host once the guest owns the
/// factory: the VM itself, and the web dashboard the host serves.
const HOST_SECTIONS: &[&str] = &["vm", "dashboard"];

/// What the host config becomes once the guest owns the factory: its
/// factory sections go (they live in the guest now), and [vm] and
/// [dashboard] stay. [dashboard] is the supervising host's listener, not a
/// factory setting (see `guest_config`); dropping it turned the dashboard
/// off at the next host command. `None` when there is nothing to remove.
pub(in crate::vm) fn host_config_after_adoption(
    existing: &toml::Table,
    vm: &VmConfig,
) -> Result<Option<toml::Table>> {
    if existing
        .keys()
        .all(|key| HOST_SECTIONS.contains(&key.as_str()))
    {
        return Ok(None);
    }
    let mut table = toml::Table::new();
    table.insert("vm".into(), toml::Value::try_from(vm)?);
    if let Some(dashboard) = existing.get("dashboard") {
        table.insert("dashboard".into(), dashboard.clone());
    }
    Ok(Some(table))
}

impl Vm {
    // ---- seed ----

    /// Firecracker: the seed tree as an ext4 disk (`seed.ext4`), made with
    /// `mkfs.ext4 -d` at every start.
    pub(in crate::vm) fn write_seed(&self, host: &Config) -> Result<()> {
        let tree = self.dir.join("seed");
        self.seed_tree(host, &tree)?;
        // The disk: what the tree takes plus room, at least 16 MiB.
        let bytes = dir_size(&tree)?;
        let size = (bytes + bytes / 4 + (8 << 20)).max(16 << 20);
        let size = size.div_ceil(1 << 20) << 20;
        let disk = self.seed_disk();
        let _ = std::fs::remove_file(&disk);
        let f = std::fs::File::create(&disk)?;
        f.set_len(size)?;
        drop(f);
        set_mode(&disk, 0o600)?;
        run_ok(
            Command::new("mkfs.ext4")
                .args(["-q", "-L", "ssf-seed", "-d"])
                .arg(&tree)
                .arg(&disk),
            "mkfs.ext4 -d",
        )?;
        let _ = std::fs::remove_dir_all(&tree);
        Ok(())
    }

    /// Build the seed tree at `tree` (replacing what was there): the guest
    /// client and server binaries, bootstrap defaults, our public key and the `[vm] files`.
    /// Legacy host factory state is included only before initial adoption;
    /// an established guest always keeps its own settings and credentials.
    pub(in crate::vm) fn seed_tree(&self, host: &Config, tree: &Path) -> Result<()> {
        let _ = std::fs::remove_dir_all(tree);
        std::fs::create_dir_all(tree.join("config"))?;
        set_mode(tree, 0o700)?;
        let binary = self.guest_binary()?;
        std::fs::copy(&binary, tree.join("ssf"))
            .with_context(|| format!("copying {}", binary.display()))?;
        make_executable(&tree.join("ssf"))?;
        let server = self.guest_server_binary()?;
        std::fs::copy(&server, tree.join("ssf-server"))
            .with_context(|| format!("copying {}", server.display()))?;
        make_executable(&tree.join("ssf-server"))?;
        // `[vm] guest_binary` wins over the guest's own package (seed_from).
        if self.cfg.guest_binary.is_some() {
            std::fs::write(tree.join("guest-binary"), "")?;
        }
        let delivery_bridge = crate::delivery_channel::bridge();
        std::fs::copy(&delivery_bridge, tree.join("ssf-delivery.ts"))
            .with_context(|| format!("copying {}", delivery_bridge.display()))?;
        let delivery_launcher = crate::delivery_channel::launcher();
        std::fs::copy(&delivery_launcher, tree.join("ssf-pi-launch"))
            .with_context(|| format!("copying {}", delivery_launcher.display()))?;
        let opencode_bridge = crate::delivery_channel::opencode_bridge();
        std::fs::copy(&opencode_bridge, tree.join("ssf-opencode.ts"))
            .with_context(|| format!("copying {}", opencode_bridge.display()))?;
        let grok_bridge = crate::delivery_channel::grok_bridge();
        std::fs::copy(&grok_bridge, tree.join("ssf-grok.mjs"))
            .with_context(|| format!("copying {}", grok_bridge.display()))?;
        // Credentials are imported only during adoption of legacy host state.
        let defaults = guest_config(&Config::default());
        write_private(
            &tree.join("defaults.toml"),
            toml::to_string_pretty(&defaults)?.as_bytes(),
        )?;
        if host.vm.enabled && !self.dir.join("guest-owned").exists() && has_legacy_factory(host)? {
            write_private(
                &tree.join("migration-source.toml"),
                toml::to_string(&guest_config(host))?.as_bytes(),
            )?;
            let mut guest = guest_config(host);
            if let Some(key) = host.github.ssh_key_path.as_deref()
                && !expand_tilde(key).is_file()
            {
                bail!(
                    "legacy bot signing key {key} is missing; restore it or explicitly choose the existing guest by backing up and removing host factory sections before restarting"
                );
            }
            if let Some(key) = host
                .github
                .ssh_key_path
                .as_deref()
                .map(expand_tilde)
                .filter(|p| p.exists())
            {
                let name = key
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "bot_ed25519".into());
                let keys = tree.join("config/keys");
                std::fs::create_dir_all(&keys)?;
                std::fs::copy(&key, keys.join(&name))?;
                let pubkey = crate::keys::public_path(&key);
                if pubkey.exists() {
                    std::fs::copy(&pubkey, keys.join(format!("{name}.pub")))?;
                }
                guest.github.ssh_key_path = Some(format!("{GUEST_HOME}/.config/ssf/keys/{name}"));
            } else {
                guest.github.ssh_key_path = None;
                guest.github.ssh_key_id = None;
                guest.github.signing_key_id = None;
            }
            let host_name = host.github.git_host();
            guest_git(
                host,
                &mut guest,
                &tree.join("config/keys"),
                &tree.join("config/git-tokens"),
                &|login| crate::ghcli::token_for(&host_name, login),
            )?;
            let toml = toml::to_string_pretty(&guest).context("serialising the guest config")?;
            write_private(&tree.join("config/config.toml"), toml.as_bytes())?;
            match host.github_token() {
                Ok(t) => write_private(&tree.join("config/token"), t.as_bytes())?,
                Err(e) if host.github.login.is_some() => bail!(
                    "legacy bot token unavailable ({e:#}); restore it or explicitly keep the guest factory by backing up and removing host factory sections"
                ),
                Err(_) => {}
            }
        }
        std::fs::copy(
            self.key().with_extension("pub"),
            tree.join("authorized_keys"),
        )?;
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let mut list = String::new();
        let files = tree.join("files");
        std::fs::create_dir_all(&files)?;
        for (i, spec) in self.cfg.files.iter().enumerate() {
            let (src, dest) = parse_file_spec(spec, &home);
            validate_shared_destination(&dest)?;
            if !src.exists() {
                warn!(
                    spec,
                    "[vm] files: {} does not exist; skipped",
                    src.display()
                );
                list.push('\n');
                continue;
            }
            std::fs::copy(&src, files.join((i + 1).to_string()))
                .with_context(|| format!("copying {}", src.display()))?;
            list.push_str(&dest);
            list.push('\n');
        }
        std::fs::write(tree.join("files.list"), list)?;
        Ok(())
    }

    /// Replay the stopped root's journal before inspecting its OS identity.
    /// Never patch with debugfs: journal replay at boot can undo such
    /// writes. A root with an older seed script boots as it is (the seed
    /// disk's layout is unchanged since 0.16); legacy and non-Ubuntu roots must be
    /// rebuilt/reset before they can see the data disk.
    pub(in crate::vm) fn require_compatible_root(&self) -> Result<()> {
        let disk = self.root_disk();
        let checked = Command::new("e2fsck")
            .args(["-f", "-p"])
            .arg(&disk)
            .output()
            .context("checking the stopped root filesystem (is e2fsprogs installed?)")?;
        // Corrected filesystems and the offline equivalent of reboot-required
        // are safe to inspect; every unresolved error refuses the boot.
        if !matches!(checked.status.code(), Some(0..=2)) {
            bail!(
                "cannot verify the stopped root filesystem {} ({}): {}{}; no guest boot was attempted. Repair the disposable root or rebuild/reset it; the data disk is untouched",
                disk.display(),
                checked.status,
                String::from_utf8_lossy(&checked.stdout),
                String::from_utf8_lossy(&checked.stderr)
            );
        }
        // Any seed script that holds the daemon until guest ownership is
        // established keeps the guest's factory state; one older than that
        // (before 0.3) could overwrite it.
        let output = Command::new("debugfs")
            .args(["-R", "cat /usr/local/lib/ssf/seed-common.sh"])
            .arg(&disk)
            .output()
            .context("reading the stopped root seed script (is e2fsprogs installed?)")?;
        if !output.status.success()
            || !String::from_utf8_lossy(&output.stdout)
                .contains("ConditionPathExists=/home/ssf/.config/ssf/guest-owned")
        {
            bail!(
                "the Firecracker root {} has a legacy seed script; refusing to boot it with the persistent data disk. Run `ssf vm build --force`, then `ssf vm reset` and `ssf vm start`. The data disk is untouched",
                disk.display()
            );
        }
        // Ubuntu ships /etc/os-release as a relative symlink. debugfs does not
        // follow symlinks for `cat`, so inspect the canonical file directly.
        let os_release = Command::new("debugfs")
            .args(["-R", "cat /usr/lib/os-release"])
            .arg(&disk)
            .output()
            .context("reading the stopped root OS identity (is e2fsprogs installed?)")?;
        let identity_read = os_release.status.success();
        let os_release = String::from_utf8_lossy(&os_release.stdout);
        if !identity_read
            || !os_release.lines().any(|line| line == "ID=ubuntu")
            || !os_release
                .lines()
                .any(|line| matches!(line, "VERSION_ID=24.04" | "VERSION_ID=\"24.04\""))
        {
            bail!(
                "the Firecracker root {} is not the supported Ubuntu 24.04 LTS image; refusing to boot it with the persistent data disk. Run `ssf vm build --force`, then `ssf vm reset` and `ssf vm start`. The data disk is untouched",
                disk.display()
            );
        }
        Ok(())
    }

    /// The Linux `ssf` binary the guest runs: `[vm] guest_binary`, this
    /// binary on a Linux host of the guest's architecture, else the
    /// release asset for this version, downloaded once with `gh`.
    pub(in crate::vm) fn guest_binary(&self) -> Result<PathBuf> {
        match guest_binary_source(
            std::env::consts::OS,
            std::env::consts::ARCH,
            self.cfg.guest_binary.as_deref(),
        ) {
            GuestBinary::Configured(p) => {
                let p = expand_tilde(&p);
                if !p.is_file() {
                    bail!("[vm] guest_binary {} does not exist", p.display());
                }
                Ok(p)
            }
            GuestBinary::Own => self.binary(),
            GuestBinary::Download { asset } => {
                let dir = self.dir.join("guest-bin");
                let path = dir.join(&asset);
                if path.is_file() {
                    return Ok(path);
                }
                std::fs::create_dir_all(&dir)?;
                info!(asset, "downloading the guest ssf binary with gh");
                download_release_asset(env!("CARGO_PKG_VERSION"), &asset, &dir).with_context(
                    || "no guest binary; build one for Linux and set [vm] guest_binary to it",
                )?;
                make_executable(&path)?;
                Ok(path)
            }
        }
    }

    /// The daemon paired with [`Self::guest_binary`]. Release assets use the
    /// `ssf-server-<version>-linux-<arch>` name; local and configured builds
    /// keep `ssf-server` beside `ssf`.
    pub(in crate::vm) fn guest_server_binary(&self) -> Result<PathBuf> {
        let source = guest_binary_source(
            std::env::consts::OS,
            std::env::consts::ARCH,
            self.cfg.guest_binary.as_deref(),
        );
        let client = self.guest_binary()?;
        let server = match source {
            GuestBinary::Download { .. } => self.dir.join("guest-bin").join(format!(
                "ssf-server-{}-linux-{}",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::ARCH
            )),
            _ => companion_server_path(&client),
        };
        if server.is_file() {
            return Ok(server);
        }
        if matches!(source, GuestBinary::Download { .. }) {
            let asset = server
                .file_name()
                .expect("server asset has a filename")
                .to_string_lossy()
                .to_string();
            download_release_asset(
                env!("CARGO_PKG_VERSION"),
                &asset,
                server.parent().expect("server asset has a directory"),
            )
            .context("no guest server binary")?;
            make_executable(&server)?;
            return Ok(server);
        }
        bail!(
            "no guest server binary at {}; build ssf and ssf-server together (or place the matching release asset beside [vm] guest_binary)",
            server.display()
        )
    }

    // ---- the guest's ssf package ----

    /// The release `.deb` of `version` for the guest's Debian architecture
    /// (`amd64` or `arm64`): the host's apt cache when it holds it, else the
    /// release asset, downloaded once with `gh`.
    pub(in crate::vm) fn guest_deb(&self, version: &str, arch: &str) -> Result<PathBuf> {
        if !matches!(arch, "amd64" | "arm64") {
            bail!(
                "no release ssf package for a {arch} guest (only amd64 and arm64); install a .deb built for it with `ssf vm upgrade --deb PATH`"
            );
        }
        let asset = format!("ssf_{version}-1_{arch}.deb");
        let cached = Path::new("/var/cache/apt/archives").join(&asset);
        if cached.is_file() {
            return Ok(cached);
        }
        let dir = self.dir.join("guest-bin");
        let path = dir.join(&asset);
        if path.is_file() {
            return Ok(path);
        }
        std::fs::create_dir_all(&dir)?;
        info!(asset, "downloading the guest ssf package with gh");
        download_release_asset(version, &asset, &dir).context("no guest package")?;
        Ok(path)
    }

    /// The version of the guest's installed ssf package; None when the guest
    /// has none (it runs the binary the host copies in at each boot).
    pub fn guest_package_version(&self) -> Result<Option<String>> {
        let out = self.ssh_output(&[
            "sh",
            "-c",
            "dpkg-query -W -f='${Status} ${Version}' ssf 2>/dev/null || true",
        ])?;
        Ok(out
            .strip_prefix("install ok installed ")
            .map(|v| v.split('-').next().unwrap_or(v).to_string()))
    }

    /// Install `deb` in the guest with apt (its postinst points the guest's
    /// boot script and `/usr/local/bin` at the package), then restart the
    /// guest daemon. Nothing else on the root changes.
    pub(in crate::vm) fn install_guest_deb(&self, deb: &Path) -> Result<()> {
        let remote = "/tmp/ssf-guest.deb";
        let file =
            std::fs::File::open(deb).with_context(|| format!("opening {}", deb.display()))?;
        let st = self
            .ssh(
                &["sh".into(), "-c".into(), format!("cat > {remote}")],
                false,
            )
            .stdin(file)
            .status()
            .context("running ssh")?;
        if !st.success() {
            bail!("copying {} into the guest failed ({st})", deb.display());
        }
        self.ssh_output(&[
            "sudo",
            "env",
            "DEBIAN_FRONTEND=noninteractive",
            "apt-get",
            "install",
            "-y",
            "--no-install-recommends",
            "--allow-downgrades",
            remote,
        ])?;
        let _ = self.ssh_output(&["rm", "-f", remote]);
        self.ssh_output(&["sudo", "systemctl", "restart", "ssf"])?;
        Ok(())
    }

    /// First start of a root without the guest package (a new one, or one
    /// from 0.18 and earlier): install this client's release package in the
    /// guest once, so from then on the guest keeps its own version. Skipped
    /// with `[vm] guest_binary` (a development build is copied in instead)
    /// and on a guest without dpkg (an Arch lima guest), which keep the
    /// copied-in binary. A guest that has the package gets
    /// [`Vm::repair_package_boot`] instead.
    pub(in crate::vm) fn adopt_guest_package(&self) -> Result<()> {
        if self.cfg.guest_binary.is_some() {
            return Ok(());
        }
        let Some(arch) = self.guest_deb_arch()? else {
            tracing::debug!("the guest has no dpkg; it keeps the copied-in ssf binary");
            return Ok(());
        };
        // The boot's seed unit installs the copied-in binary; let it finish
        // before the package's links replace it.
        let deadline = Instant::now() + Duration::from_secs(120);
        while self
            .ssh_output(&["systemctl", "is-active", "--quiet", "ssf-seed"])
            .is_err()
        {
            if Instant::now() >= deadline {
                bail!("ssf-seed.service did not finish in the guest");
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        if self.guest_package_version()?.is_some() {
            return self.repair_package_boot();
        }
        let deb = self.guest_deb(env!("CARGO_PKG_VERSION"), &arch)?;
        println!(
            "installing the ssf {} package in the guest",
            env!("CARGO_PKG_VERSION")
        );
        self.install_guest_deb(&deb)
    }

    /// A package-managed guest whose package predates the boot script that
    /// leaves the package's ssf alone (a release before 0.19) still copies
    /// the host's binary over `/usr/local/bin/ssf` at every boot, shadowing
    /// the package. Give it this ssf's boot script and point the links back
    /// at the package, restarting the guest daemon only if they were wrong.
    fn repair_package_boot(&self) -> Result<()> {
        const SCRIPT: &str = r#"set -e
tmp=$(mktemp)
cat > "$tmp"
# Only a script without the package branch; a newer package's is kept.
if ! grep -q 'dpkg-query -W' /usr/local/lib/ssf/seed-common.sh; then
    sudo install -m644 "$tmp" /usr/local/lib/ssf/seed-common.sh
fi
rm -f "$tmp"
changed=
for b in ssf ssf-server; do
    if [ "$(readlink "/usr/local/bin/$b")" != "/usr/bin/$b" ]; then
        sudo ln -sfn "/usr/bin/$b" "/usr/local/bin/$b"
        changed=1
    fi
done
if [ -n "$changed" ]; then
    echo relinked
    sudo systemctl restart ssf
fi
"#;
        let mut child = self
            .ssh(&["sh".into(), "-c".into(), SCRIPT.into()], false)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .context("running ssh")?;
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().context("ssh stdin")?;
            stdin.write_all(include_bytes!("../../vm/guest/seed-common.sh"))?;
        }
        let out = child.wait_with_output().context("running ssh")?;
        if !out.status.success() {
            bail!("repairing the guest's boot script failed ({})", out.status);
        }
        if String::from_utf8_lossy(&out.stdout).contains("relinked") {
            println!(
                "the guest's ssf now runs from its package again (its boot script copied the host's binary over it)"
            );
        }
        Ok(())
    }

    /// The guest's Debian architecture (`dpkg --print-architecture`); None
    /// on a guest without dpkg (an Arch lima guest).
    fn guest_deb_arch(&self) -> Result<Option<String>> {
        let arch = self.ssh_output(&[
            "sh",
            "-c",
            "! command -v dpkg >/dev/null || dpkg --print-architecture",
        ])?;
        Ok((!arch.is_empty()).then_some(arch))
    }

    /// `ssf vm upgrade [VERSION]`: install that release's package in the
    /// running guest (or `deb`, a local package) and restart its daemon,
    /// not the VM. A guest without the package adopts it this way too.
    pub fn upgrade(&self, version: Option<&str>, deb: Option<&Path>) -> Result<()> {
        if !self.ssh_ok() {
            bail!("the VM is not reachable; `ssf vm start` first");
        }
        let Some(arch) = self.guest_deb_arch()? else {
            bail!(
                "the guest has no dpkg (an Arch lima guest): it runs the ssf the host copies in at each start (`ssf vm restart`). `ssf vm build --force` and `ssf vm start` move it to Ubuntu, discarding what was installed on its root"
            );
        };
        let deb = match deb {
            Some(deb) => deb.to_path_buf(),
            None => {
                let version = version.unwrap_or(env!("CARGO_PKG_VERSION"));
                self.guest_deb(version.strip_prefix('v').unwrap_or(version), &arch)?
            }
        };
        self.install_guest_deb(&deb)?;
        println!(
            "the guest runs ssf {}",
            self.guest_package_version()?
                .unwrap_or_else(|| "unknown".into())
        );
        Ok(())
    }

    // ---- ssh ----

    /// The ssh options that reach the guest.
    /// ssh's own options: the VM's key, port and hosts file, and the
    /// limits that keep one attempt from lasting for ever. `batch` (every
    /// probe and forwarded command; not an interactive session) adds the
    /// keepalives, so an ssh whose connection has gone silent gives up
    /// after a minute instead of holding the waits below open.
    pub fn ssh_args(&self, batch: bool) -> Vec<String> {
        let mut v = vec![
            "-i".to_string(),
            self.key().to_string_lossy().to_string(),
            "-p".to_string(),
            self.cfg.ssh_port.to_string(),
            "-o".to_string(),
            "StrictHostKeyChecking=no".to_string(),
            "-o".to_string(),
            format!("UserKnownHostsFile={}", self.known_hosts().display()),
            "-o".to_string(),
            "LogLevel=ERROR".to_string(),
            "-o".to_string(),
            "ConnectTimeout=5".to_string(),
        ];
        if batch {
            v.push("-o".into());
            v.push("BatchMode=yes".into());
            v.push("-o".into());
            v.push("ServerAliveInterval=15".into());
            v.push("-o".into());
            v.push("ServerAliveCountMax=4".into());
        }
        v
    }

    pub(in crate::vm) fn target(&self) -> String {
        format!("{GUEST_USER}@127.0.0.1")
    }

    /// An ssh command running `remote` in the guest (`tty` for interactive use).
    pub fn ssh(&self, remote: &[String], tty: bool) -> Command {
        let mut cmd = Command::new("ssh");
        cmd.args(self.ssh_args(!tty));
        if tty {
            cmd.arg("-t");
        }
        cmd.arg(self.target());
        if !remote.is_empty() {
            cmd.arg("--");
            cmd.arg(shell_join(remote));
        }
        cmd
    }

    /// Run a command in the guest and return its stdout.
    pub fn ssh_output(&self, remote: &[&str]) -> Result<String> {
        let remote: Vec<String> = remote.iter().map(|s| s.to_string()).collect();
        let out = self
            .ssh(&remote, false)
            .stdin(Stdio::null())
            .output()
            .context("running ssh")?;
        if !out.status.success() {
            bail!(
                "ssh {} failed: {}",
                remote.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn ssh_ok(&self) -> bool {
        self.ssh_output(&["true"]).is_ok()
    }

    /// Wait for the guest to answer as `ssf`, `timeout` at the most.
    ///
    /// The loop below watches its own deadline; this wraps the whole wait
    /// in one too, because a loop can stop reaching its deadline check at
    /// all: an `ssf vm build` was once found parked at an `.await` for
    /// fifteen minutes with no child process, no output and no CPU. The
    /// inner check is what a person normally sees, the outer one is the
    /// backstop, and each attempt is bounded in turn by ssh's own
    /// `ConnectTimeout` and keepalives ([`Vm::ssh_args`]).
    pub(in crate::vm) async fn wait_for_ssh(&self, timeout: Duration) -> Result<()> {
        let waited = tokio::time::timeout(timeout + WAIT_BACKSTOP_MARGIN, self.ssh_loop(timeout));
        match waited.await {
            Ok(r) => r,
            Err(_) => bail!(
                "the guest did not answer on ssh in {}s, and the wait itself stopped making progress; `ssf vm console` has its console",
                timeout.as_secs()
            ),
        }
    }

    pub(in crate::vm) async fn ssh_loop(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            // Only a definite "not running" ends the wait: a probe that
            // could not be made (under lima, a `limactl` that did not
            // run) is not the guest exiting.
            if self.running_state() == Some(false) {
                bail!("the VM exited");
            }
            if self.ssh_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        bail!("the guest did not answer on ssh in {}s", timeout.as_secs())
    }

    /// `systemctl is-active ssf` in the guest once it stops `activating`,
    /// with the same backstop around the whole wait as [`Vm::wait_for_ssh`].
    pub(in crate::vm) async fn wait_for_daemon(&self, timeout: Duration) -> Option<String> {
        let waited =
            tokio::time::timeout(timeout + WAIT_BACKSTOP_MARGIN, self.daemon_loop(timeout));
        waited.await.unwrap_or_else(|_| {
            warn!(
                "waiting for the daemon in the guest stopped making progress after {}s; `ssf vm ssh -- systemctl status ssf` says where it is",
                (timeout + WAIT_BACKSTOP_MARGIN).as_secs()
            );
            None
        })
    }

    pub(in crate::vm) async fn daemon_loop(&self, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let state = self.daemon_state();
            if state.as_deref() != Some("activating") || Instant::now() >= deadline {
                return state;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    pub(in crate::vm) fn daemon_state(&self) -> Option<String> {
        let remote = ["systemctl", "is-active", "ssf"];
        let remote: Vec<String> = remote.iter().map(|s| s.to_string()).collect();
        let out = self
            .ssh(&remote, false)
            .stdin(Stdio::null())
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    /// `ssf <args>` as the guest runs it: the guest marker, so the
    /// command in there knows it is the factory and does not forward
    /// again, and the arguments as given.
    pub(in crate::vm) fn ssf_remote(&self, args: &[String]) -> Vec<String> {
        self.ssf_remote_with_client_context(
            args,
            std::env::var_os(crate::cli::CLIENT_VERSION_ENV).as_deref(),
            std::env::var_os(crate::cli::VERSION_REPORTED_ENV).is_some(),
        )
    }

    pub(in crate::vm) fn ssf_remote_with_client_context(
        &self,
        args: &[String],
        client_version: Option<&std::ffi::OsStr>,
        version_reported: bool,
    ) -> Vec<String> {
        let mut remote = vec![format!("{GUEST_ENV}=1"), "ssf".to_string()];
        if let Some(version) = client_version {
            remote.insert(
                1,
                format!(
                    "{}={}",
                    crate::cli::CLIENT_VERSION_ENV,
                    version.to_string_lossy()
                ),
            );
        }
        if version_reported {
            remote.insert(1, format!("{}=1", crate::cli::VERSION_REPORTED_ENV));
        }
        remote.extend(args.iter().cloned());
        remote
    }

    /// Run an `ssf` command inside the guest with this terminal, and exit
    /// with its status.
    pub fn exec_ssf(&self, args: &[String]) -> Result<ExitStatus> {
        let remote = self.ssf_remote(args);
        let tty = stdin_is_tty();
        let st = self
            .ssh(&remote, tty)
            .status()
            .context("running ssh (is the VM up? `ssf vm status`)")?;
        Ok(st)
    }

    /// [`Vm::exec_ssf`] with the guest's stdout captured, its stderr left
    /// on this terminal. For the caller that has to answer even when the
    /// guest does not: `status --json`, which the dashboards parse.
    pub fn capture_ssf(&self, args: &[String]) -> Result<std::process::Output> {
        self.capture_ssf_command(args, Path::new("/tmp"))?
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .output()
            .context("running ssh (is the VM up? `ssf vm status`)")
    }

    /// Reuse only captured status requests, including when this host itself
    /// is reached over SSH. Guest ownership and VM routing stay on the server.
    pub(in crate::vm) fn capture_ssf_command(
        &self,
        args: &[String],
        socket_root: &Path,
    ) -> Result<Command> {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let directory = status_control_directory(socket_root)?;
        let mut identity = DefaultHasher::new();
        // The key and trust store distinguish factories sharing an SSH port.
        // Hashing keeps the Unix socket path short enough for macOS.
        (
            self.key(),
            self.known_hosts(),
            self.cfg.ssh_port,
            self.target(),
        )
            .hash(&mut identity);
        let socket = directory.join(format!("{:016x}", identity.finish()));
        let mut command = Command::new("ssh");
        command.args(self.ssh_args(true));
        command.args(["-o", "ControlMaster=auto", "-o", "ControlPersist=60"]);
        command
            .arg("-o")
            .arg(format!("ControlPath={}", socket.display()));
        command
            .arg(self.target())
            .arg("--")
            .arg(shell_join(&self.ssf_remote(args)));
        Ok(command)
    }

    /// Run one daemon pass inside the guest.
    pub fn exec_server_once(&self) -> Result<ExitStatus> {
        let remote = vec![
            format!("{GUEST_ENV}=1"),
            "ssf-server".to_string(),
            "--once".to_string(),
        ];
        self.ssh(&remote, false)
            .status()
            .context("running ssf-server in the VM over ssh")
    }

    /// A shell, or a command line passed to the guest's shell as given
    /// (`ssf vm ssh -- ls -la /var/lib/ssf`).
    pub fn shell(&self, args: &[String]) -> Result<ExitStatus> {
        let tty = stdin_is_tty();
        let mut cmd = Command::new("ssh");
        cmd.args(self.ssh_args(!tty));
        if tty {
            cmd.arg("-t");
        }
        cmd.arg(self.target());
        if !args.is_empty() {
            cmd.arg("--").arg(args.join(" "));
        }
        Ok(cmd.status()?)
    }

    /// Install Tailscale on demand and enrol the guest. The script is sent
    /// over ssh rather than baked into the image, so Tailscale remains absent
    /// until this explicit command and the flow also works after an upgrade.
    pub fn tailscale(&self) -> Result<ExitStatus> {
        let remote = vec![
            "bash".to_string(),
            "-s".to_string(),
            "--".to_string(),
            "ssf-vm".to_string(),
        ];
        let mut child = self
            .ssh(&remote, false)
            .stdin(Stdio::piped())
            .spawn()
            .context("running ssh (is the VM up? `ssf vm status`)")?;
        child
            .stdin
            .take()
            .context("opening ssh input")?
            .write_all(include_bytes!("../../vm/guest/tailscale.sh"))
            .context("sending the Tailscale enrolment script to the guest")?;
        child.wait().context("waiting for Tailscale enrolment")
    }

    /// Attach to herdr's persistent session in the guest, in this terminal.
    pub fn attach(&self) -> Result<ExitStatus> {
        self.ssh(&["herdr".to_string()], true)
            .status()
            .context("running ssh")
    }

    /// An `~/.ssh/config` entry for the guest, for `herdr --remote <name>`
    /// and plain `ssh <name>`.
    pub fn ssh_config(&self) -> String {
        format!(
            "Host ssf-{name}\n  HostName 127.0.0.1\n  Port {port}\n  User {GUEST_USER}\n  IdentityFile {key}\n  IdentitiesOnly yes\n  StrictHostKeyChecking no\n  UserKnownHostsFile {kh}\n  LogLevel ERROR\n",
            name = self.cfg.name,
            port = self.cfg.ssh_port,
            key = self.key().display(),
            kh = self.known_hosts().display(),
        )
    }

    /// Explicit migration repair; ordinary factory commands never sync settings.
    pub fn sync(&self, host: &Config) -> Result<()> {
        self.ensure_factory_ownership(host)?;
        println!("guest owns factory configuration; no synchronization is needed");
        Ok(())
    }

    pub fn factory_owned(&self) -> bool {
        self.dir.join("guest-owned").exists()
    }

    pub fn ensure_factory_ownership(&self, host: &Config) -> Result<()> {
        // A supervisor can retain its startup snapshot for many VM boots.
        // Adoption must compare and archive what is currently on disk, not
        // discard edits made while booting or re-import its old snapshot.
        let current;
        let host = if crate::config::config_path().exists() {
            current = Config::load()?;
            &current
        } else {
            host
        };
        if !host.vm.enabled {
            return Ok(());
        }
        let selected = Vm::new(host);
        if selected.dir != self.dir || selected.backend() != self.backend() {
            bail!(
                "the host VM selection changed during startup; restart the supervisor before completing migration. No factory settings were changed"
            );
        }
        if !self.ssh_ok() {
            bail!(
                "the VM is stopped or unreachable; run `ssf vm start`. Factory changes belong to the guest and no host configuration was changed"
            );
        }
        if self
            .ssh_output(&["test", "-f", "/home/ssf/.config/ssf/guest-owned"])
            .is_err()
        {
            let detail = self
                .ssh_output(&["cat", "/home/ssf/.config/ssf/migration-error"])
                .unwrap_or_default();
            bail!(
                "guest ownership migration is incomplete. {detail} Restart the VM to run migration; existing guest configuration and credentials are preserved"
            );
        }
        if self.factory_owned() && has_legacy_factory(host)? {
            bail!(
                "the guest already owns this factory, but the host has new factory settings (for example from host mode). Both are preserved. Back up the host config and remove its factory sections, keeping [vm], to use the guest; reconcile any wanted changes explicitly in the guest"
            );
        }
        if !self.factory_owned() && has_legacy_factory(host)? {
            let receipt = self.ssh_output(&["cat", "/home/ssf/.config/ssf/migration-source.toml"])
                .context("guest migration receipt is missing; preserve the host config and explicitly choose guest ownership by removing host factory sections")?;
            validate_migration_receipt(host, &receipt)?;
        }
        let path = crate::config::config_path();
        let backup = path.with_extension("toml.pre-guest-ownership");
        if !self.factory_owned() && path.exists() && !backup.exists() {
            crate::config::write_atomic(&backup, &std::fs::read(&path)?, 0o600)?;
        }
        if path.exists() {
            let existing: toml::Table = toml::from_str(&std::fs::read_to_string(&path)?)?;
            if let Some(table) = host_config_after_adoption(&existing, &host.vm)? {
                crate::config::write_atomic(
                    &path,
                    toml::to_string_pretty(&table)?.as_bytes(),
                    0o600,
                )?;
            }
        }
        crate::config::write_atomic(&self.dir.join("guest-owned"), b"1\n", 0o600)?;
        // Remove transient migration copies after the guest acknowledges them.
        let _ = std::fs::remove_dir_all(self.dir.join("share/seed/config"));
        let _ = std::fs::remove_file(self.dir.join("share/seed/migration-source.toml"));
        let _ = std::fs::remove_file(self.seed_disk());
        Ok(())
    }

    /// The guest daemon's journal.
    pub fn logs(&self, follow: bool, lines: u32) -> Result<ExitStatus> {
        let mut remote = vec![
            "sudo".to_string(),
            "journalctl".to_string(),
            "-u".to_string(),
            "ssf".to_string(),
            "-n".to_string(),
            lines.to_string(),
            "--no-pager".to_string(),
        ];
        if follow {
            remote.push("-f".into());
        }
        Ok(self.ssh(&remote, follow).status()?)
    }

    pub async fn status(&self) -> VmStatus {
        let backend = self.backend();
        // One `limactl list` for everything lima knows -- and, when it
        // did not answer, that fact rather than `.ok().flatten()`. Read
        // as "no such instance" it printed `instance: ssf-<name> missing
        // (ssf vm build)` over a VM that exists, which is the same
        // conflation `lima_stop` was fixed for.
        let (inst, probe_error) = match backend {
            BackendKind::Lima => match self.lima_instance() {
                Ok(i) => (i, None),
                Err(e) => (None, Some(format!("{e:#}"))),
            },
            BackendKind::Firecracker | BackendKind::Incus => (None, None),
        };
        // Incus: one `incus list` for the same two answers.
        let (incus, probe_error) = match backend {
            BackendKind::Incus => match self.incus_instance() {
                Ok(i) => (i, None),
                Err(e) => (None, Some(format!("{e:#}"))),
            },
            _ => (None, probe_error),
        };
        let running = match backend {
            BackendKind::Firecracker => Some(self.running()),
            BackendKind::Lima => probe_error
                .is_none()
                .then(|| inst.as_ref().is_some_and(|i| i.is_running())),
            BackendKind::Incus => probe_error
                .is_none()
                .then(|| incus.as_ref().is_some_and(|i| i.is_running())),
        };
        let ssh = running == Some(true) && self.ssh_ok();
        let sizes = self.sizes();
        VmStatus {
            vcpus: sizes.vcpus,
            mem_mib: sizes.mem_mib,
            data_gib: self.data_cap_gib(),
            data: if ssh {
                self.guest_disk_use().ok()
            } else {
                None
            },
            enabled: self.cfg.enabled,
            name: self.cfg.name.clone(),
            dir: self.dir.to_string_lossy().to_string(),
            backend: backend.to_string(),
            instance: match backend {
                BackendKind::Lima => Some(self.lima_name()),
                BackendKind::Incus => Some(self.incus_name()),
                BackendKind::Firecracker => None,
            },
            shares_host_kernel: backend.shares_host_kernel(),
            lima_dir: inst.as_ref().map(|i| i.dir.clone()),
            image: match backend {
                BackendKind::Firecracker => self.rootfs().exists(),
                BackendKind::Lima => inst.is_some(),
                BackendKind::Incus => incus.is_some(),
            },
            running,
            firecracker_pid: match backend {
                BackendKind::Firecracker => self.firecracker_pid(),
                BackendKind::Lima | BackendKind::Incus => None,
            },
            gvproxy_pid: match backend {
                BackendKind::Firecracker => self.gvproxy_pid(),
                BackendKind::Lima | BackendKind::Incus => None,
            },
            ssh_port: self.cfg.ssh_port,
            ssh,
            daemon: if ssh { self.daemon_state() } else { None },
            logins: if ssh {
                self.logins().unwrap_or_default()
            } else {
                Vec::new()
            },
            tooling: (!in_guest()).then(|| self.tooling()),
            probe_error,
        }
    }

    /// Look for the tooling this VM's backend needs on the machine this
    /// runs on: what `ssf doctor` and `ssf vm status` report.
    pub fn tooling(&self) -> Tooling {
        let backend = self.backend();
        let tools = backend_tools(
            backend,
            std::env::consts::OS,
            std::env::consts::ARCH,
            self.cfg.limactl.as_deref(),
            self.cfg.vm_type.as_deref(),
        );
        let (ok, detail) = backend_tooling_line(&tools, &probe_tools(&tools));
        Tooling { ok, detail }
    }

    // ---- harness logins ----

    /// Every harness's login state in the guest, in one ssh round trip.
    ///
    /// What the script found is in its output, not in its status: every
    /// test in it fails for a harness that is not installed or not logged
    /// in, which is the ordinary case. [`Vm::ssh_output`] throws stdout
    /// away when the status is not zero, so a script ending in a failed
    /// test would come back as an error with the answer discarded. The
    /// closing `exit 0` is what keeps that from happening -- nothing may
    /// be appended after it, and a test appended before it must not be
    /// the last command of the script.
    pub fn logins(&self) -> Result<Vec<LoginState>> {
        let script: String = LOGINS
            .iter()
            .map(|l| {
                format!(
                    "i=0; s=0; command -v {h} >/dev/null 2>&1 && i=1; {check} && s=1; echo {h} $i $s; ",
                    h = l.harness,
                    check = l.check()
                )
            })
            .chain(std::iter::once("exit 0".to_string()))
            .collect();
        let out = self.ssh_output(&["sh", "-c", &script])?;
        Ok(parse_login_states(&out))
    }

    /// A fresh root at the next start (Firecracker: the root disk remade
    /// from the image; lima: the instance re-created from its template);
    /// the data disk (state, clones, worktrees) stays.
    pub async fn reset(&self) -> Result<()> {
        if self.running_or_refuse("reset")? {
            self.stop().await?;
        }
        let _ = std::fs::remove_file(self.known_hosts());
        match self.backend() {
            BackendKind::Firecracker => {
                let _ = std::fs::remove_file(self.root_disk());
                println!("root disk removed; `ssf vm start` makes a fresh one from the image");
                Ok(())
            }
            BackendKind::Lima => self.lima_reset(),
            BackendKind::Incus => self.incus_reset(),
        }
    }

    /// Remove the VM and everything in it. Every part of it may be gone
    /// already -- a second `ssf uninstall`, a half-uninstalled machine
    /// -- so each is removed only if it is there, and the command says
    /// so rather than claiming a removal it did not make.
    pub async fn destroy(&self) -> Result<()> {
        // Firecracker must have its process gone before its disks:
        // removing them under a live firecracker leaves it running on
        // unlinked inodes, so a stop that fails ends the destroy. Lima's
        // `limactl delete -f` force-stops the instance itself, so
        // neither a stop that fails nor a running state that could not
        // be read is a reason to leave a VM standing for ever -- which
        // is what a guest that would not shut down did, on every run.
        match (self.backend(), self.running_state()) {
            (_, Some(false)) => {}
            (BackendKind::Firecracker, _) => self.stop().await?,
            (BackendKind::Lima | BackendKind::Incus, Some(true)) => {
                if let Err(e) = self.stop().await {
                    warn!(
                        "could not stop {} before destroying it: {e:#}; deleting it anyway",
                        self.cfg.name
                    );
                }
            }
            // A lima that could not be asked is not asked twice more for
            // nothing: `lima_stop` re-runs the same listing and
            // `lima_destroy` runs it again after that, so a limactl that
            // hangs rather than fails bought this step minutes of
            // silence and a warning that explains none of it. The
            // graceful shutdown is worth its wait where the guest is
            // known to be up; where it is not, `limactl delete -f` stops
            // whatever is there.
            // `incus delete -f` likewise stops whatever is there.
            (BackendKind::Lima | BackendKind::Incus, None) => {}
        }
        let lima = match self.backend() {
            BackendKind::Lima => self.lima_destroy(),
            BackendKind::Incus => self.incus_destroy(),
            BackendKind::Firecracker => Ok(false),
        };
        let mut removed = lima.as_ref().copied().unwrap_or(false);
        // Under lima this directory is often the only thing that was
        // ever here, and under either backend it may be gone already. It
        // is ssf's own either way, so it goes even when lima's half
        // failed: leaving it behind only makes the next run harder to
        // read, and the failure is still the step's failure below.
        let dir = if self.dir.exists() {
            match std::fs::remove_dir_all(&self.dir) {
                Ok(()) => {
                    println!("removed {}", self.dir.display());
                    removed = true;
                    Ok(())
                }
                Err(e) => {
                    Err(anyhow::Error::new(e).context(format!("removing {}", self.dir.display())))
                }
            }
        } else {
            Ok(())
        };
        // Both halves are reported. A directory that would not go is no
        // reason to lose the news that lima still holds an instance and
        // a data disk.
        match (lima, dir) {
            (Ok(_), Ok(())) => {}
            (Err(l), Err(d)) => bail!("{l:#}; and {d:#}"),
            (Err(e), Ok(())) | (Ok(_), Err(e)) => return Err(e),
        }
        if !removed {
            // Not "the directory is not there": under lima that
            // directory is precisely what the question did not turn on.
            println!("nothing to remove");
        }
        Ok(())
    }
}

/// How fetching one release file went.
enum Fetched {
    Done,
    /// The release has no file of that name.
    Missing,
    Failed(String),
}

/// Fetch release file `name` of `v{version}` into `dir`: with `gh`, then,
/// when that fails for any other reason than the file being absent, with a
/// plain HTTPS download. The releases are public, and on a server the host's
/// `gh` is usually not signed in (the bot's token lives in the guest), where
/// `gh release download` refuses to run at all.
fn fetch_release_file(version: &str, name: &str, dir: &Path) -> Fetched {
    let path = dir.join(name);
    let gh = Command::new("gh")
        .args(["release", "download", &format!("v{version}")])
        .args(["-R", RELEASE_REPO, "--clobber", "--pattern", name, "-D"])
        .arg(dir)
        .stdin(Stdio::null())
        .output();
    let gh_err = match gh {
        Ok(o) if o.status.success() && path.is_file() => return Fetched::Done,
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            if err.contains("no assets match") {
                return Fetched::Missing;
            }
            format!("`gh release download` failed ({err})")
        }
        Err(e) => format!("running gh failed ({e})"),
    };
    let url = format!("https://github.com/{RELEASE_REPO}/releases/download/v{version}/{name}");
    let curl = Command::new("curl")
        .args(["-sSL", "--retry", "2", "-w", "%{http_code}", "-o"])
        .arg(&path)
        .arg(&url)
        .stdin(Stdio::null())
        .output();
    match curl {
        Ok(o) => {
            let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if o.status.success() && code == "200" && path.is_file() {
                return Fetched::Done;
            }
            let _ = std::fs::remove_file(&path);
            if code == "404" {
                return Fetched::Missing;
            }
            Fetched::Failed(format!(
                "{gh_err}; and downloading {url} failed (HTTP {code}: {})",
                String::from_utf8_lossy(&o.stderr).trim()
            ))
        }
        Err(e) => Fetched::Failed(format!("{gh_err}; and running curl failed ({e})")),
    }
}

/// Download release asset `asset` of `v{version}` into `dir` (see
/// [`fetch_release_file`]), and check it against the release's
/// `SHA256SUMS` (#617). A release that publishes `SHA256SUMS` must list the
/// asset with a matching digest, or the download is removed and this fails.
/// Releases made before `SHA256SUMS` existed have none; their asset is
/// accepted with a warning, since failing would strand every guest of those
/// versions.
fn download_release_asset(version: &str, asset: &str, dir: &Path) -> Result<PathBuf> {
    let path = dir.join(asset);
    match fetch_release_file(version, asset, dir) {
        Fetched::Done => {}
        Fetched::Missing => bail!("release v{version} of {RELEASE_REPO} has no asset {asset}"),
        Fetched::Failed(e) => bail!("could not download {asset} of v{version}: {e}"),
    }
    let sums = dir.join("SHA256SUMS");
    let _ = std::fs::remove_file(&sums);
    match fetch_release_file(version, "SHA256SUMS", dir) {
        Fetched::Done => {}
        Fetched::Missing => {
            warn!(
                asset,
                "release v{version} publishes no SHA256SUMS (made before #617?); the asset is unverified"
            );
            return Ok(path);
        }
        // Only a release without the file is "old"; any other failure
        // (network, auth, rate limit) must not skip verification.
        Fetched::Failed(e) => {
            let _ = std::fs::remove_file(&path);
            bail!("could not fetch SHA256SUMS for v{version} to verify {asset} ({e})");
        }
    }
    let listed = std::fs::read_to_string(&sums)?;
    let _ = std::fs::remove_file(&sums);
    match sha256_listed(&listed, asset) {
        Some(sum) => verify_sha256(&path, sum)?,
        None => {
            let _ = std::fs::remove_file(&path);
            bail!("release v{version}'s SHA256SUMS does not list {asset}");
        }
    }
    Ok(path)
}

/// The digest `sha256sum` output gives for `name` (text or `*`binary mode).
fn sha256_listed<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (sum, file) = line.split_once(char::is_whitespace)?;
        let file = file.trim_start();
        (file.strip_prefix('*').unwrap_or(file) == name).then_some(sum)
    })
}

#[cfg(test)]
mod sha256sums_tests {
    use super::sha256_listed;

    #[test]
    fn sha256sums_lines_are_matched_by_exact_name() {
        let sums = "aaa  ssf_1.0.0-1_amd64.deb\nbbb *ssf-1.0.0-linux-x86_64\n";
        assert_eq!(sha256_listed(sums, "ssf_1.0.0-1_amd64.deb"), Some("aaa"));
        assert_eq!(sha256_listed(sums, "ssf-1.0.0-linux-x86_64"), Some("bbb"));
        assert_eq!(sha256_listed(sums, "ssf-1.0.0-linux"), None);
    }
}

#[cfg(test)]
mod release_download_tests {
    /// With `gh` signed out, the public release still downloads and verifies
    /// over plain HTTPS. Needs the network: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn downloads_without_a_signed_in_gh() {
        let sandbox = crate::config::test_support::sandbox();
        let dir = sandbox.root().join("dl");
        let empty = sandbox.root().join("gh");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&empty).unwrap();
        // SAFETY: an ignored test run on its own.
        unsafe { std::env::set_var("GH_CONFIG_DIR", &empty) };
        unsafe { std::env::remove_var("GH_TOKEN") };
        let path = super::download_release_asset("0.21.0", "ssf_0.21.0-1_amd64.deb", &dir).unwrap();
        assert!(path.is_file());
        assert!(super::download_release_asset("0.21.0", "no-such-asset", &dir).is_err());
    }
}
