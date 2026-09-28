use super::*;

/// Where `gh release download` finds the guest binary.
pub const RELEASE_REPO: &str = "mikekelly/simple-software-factory";

/// Where the guest's `ssf` binary comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestBinary {
    /// `[vm] guest_binary`.
    Configured(String),
    /// This binary: a Linux host of the guest's architecture.
    Own,
    /// The release asset for this version and the guest's architecture.
    Download { asset: String },
}

pub(crate) fn companion_server_path(client: &Path) -> PathBuf {
    let name = client.file_name().and_then(|n| n.to_str()).unwrap_or("ssf");
    let server = if name == "ssf-server" || name.starts_with("ssf-server-") {
        name.to_string()
    } else if name == "ssf" {
        "ssf-server".to_string()
    } else if let Some(suffix) = name.strip_prefix("ssf-") {
        format!("ssf-server-{suffix}")
    } else {
        "ssf-server".to_string()
    };
    client.with_file_name(server)
}

/// Which binary a host running `os` on `arch` (the guest's architecture
/// too) seeds into the guest: what `[vm] guest_binary` names, else its own
/// when it is Linux, else the release asset `ssf-<version>-linux-<arch>`.
pub fn guest_binary_source(os: &str, arch: &str, configured: Option<&str>) -> GuestBinary {
    match configured {
        Some(p) => GuestBinary::Configured(p.to_string()),
        None if os == "linux" => GuestBinary::Own,
        None => GuestBinary::Download {
            asset: format!("ssf-{}-linux-{arch}", env!("CARGO_PKG_VERSION")),
        },
    }
}

/// Settle `[vm] backend` for a build the way `choose_sizes` settles the
/// sizes: a value in the file stays; none gets `default` and is written.
/// Returns the backend, where it came from, and whether the file changed.
pub fn choose_backend(
    cfg: &mut VmConfig,
    default: BackendKind,
) -> (BackendKind, &'static str, bool) {
    match cfg.backend {
        Some(b) => (b, "set in config.toml", false),
        None => {
            cfg.backend = Some(default);
            (default, "for this machine", true)
        }
    }
}

/// Paths of one boot (the VM's, or the build's).
pub struct BootFiles {
    pub config: PathBuf,
    pub api: PathBuf,
    pub vsock: PathBuf,
    pub gv_api: PathBuf,
    pub gv_log: PathBuf,
}

pub(in crate::vm) fn clean_sockets(boot: &BootFiles) {
    for p in [
        boot.api.clone(),
        boot.vsock.clone(),
        boot.vsock.with_extension(format!("sock_{NET_PORT}")),
        boot.gv_api.clone(),
    ] {
        let _ = std::fs::remove_file(p);
    }
}

pub(in crate::vm) fn validate_migration_receipt(host: &Config, receipt: &str) -> Result<()> {
    let accepted: Config = toml::from_str(receipt)?;
    if toml::to_string(&accepted)? != toml::to_string(&guest_config(host))? {
        bail!(
            "host factory settings changed after the guest accepted migration; both are preserved. Reconcile explicitly, or back up and remove the host factory sections to keep the guest"
        );
    }
    Ok(())
}

pub(in crate::vm) fn validate_shared_destination(dest: &str) -> Result<()> {
    let path = Path::new(dest);
    if path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        bail!("[vm] files destination must not contain ..: {dest}");
    }
    let home = Path::new(GUEST_HOME);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        home.join(path)
    };
    if absolute.starts_with(home.join(".config/ssf"))
        || absolute == home.join(".gitconfig")
        || absolute.starts_with("/var/lib/ssf")
    {
        bail!(
            "[vm] files cannot overwrite guest-owned factory state: {dest}; use the guest CLI or ssf vm ssh"
        );
    }
    Ok(())
}

/// Whether the pre-single-owner host contains factory settings worth migrating.
pub(in crate::vm) fn has_legacy_factory(host: &Config) -> Result<bool> {
    Ok(
        toml::to_string(&guest_config(host))?
            != toml::to_string(&guest_config(&Config::default()))?,
    )
}

/// Adopt the persistent guest configuration once. All comparisons precede writes;
/// a completion marker is the final write, so retrying interrupted copies is safe.
pub fn initialize_guest_factory(seed: &Path, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    set_mode(dir, 0o700)?;
    if dir.join("guest-owned").exists() {
        return Ok(());
    }
    let candidate = seed.join("config");
    let current_path = dir.join("config.toml");
    let candidate_path = candidate.join("config.toml");
    if candidate_path.exists() && current_path.exists() {
        let incoming: Config = toml::from_str(&std::fs::read_to_string(&candidate_path)?)?;
        let current: Config = toml::from_str(&std::fs::read_to_string(&current_path)?)?;
        if toml::to_string(&incoming)? != toml::to_string(&current)? {
            bail!(
                "host and guest factory configurations differ. Neither was overwritten. Compare the host config with ~/.config/ssf/config.toml using `ssf vm ssh`, reconcile the intended settings explicitly, then restart. To explicitly keep the guest, back up the host config and remove its factory sections (keep [vm]) before restarting"
            );
        }
    }
    // Existing token/key bytes win only when identical. Never silently replace
    // credentials or leave a config referencing a different imported key.
    let files = migration_files(&candidate)?;
    for relative in &files {
        let destination = dir.join(relative);
        if relative != Path::new("config.toml")
            && destination.exists()
            && std::fs::read(candidate.join(relative))? != std::fs::read(&destination)?
        {
            bail!(
                "legacy migration credential/file conflict at {}; both copies were preserved. Resolve the intended credential before restarting",
                destination.display()
            );
        }
    }
    for relative in &files {
        let destination = dir.join(relative);
        if !destination.exists() {
            std::fs::create_dir_all(destination.parent().context("migration file parent")?)?;
            crate::config::write_atomic(
                &destination,
                &std::fs::read(candidate.join(relative))?,
                0o600,
            )?;
        }
    }
    if !current_path.exists() {
        crate::config::write_atomic(
            &current_path,
            &std::fs::read(seed.join("defaults.toml"))?,
            0o600,
        )?;
    }
    if seed.join("migration-source.toml").exists() {
        crate::config::write_atomic(
            &dir.join("migration-source.toml"),
            &std::fs::read(seed.join("migration-source.toml"))?,
            0o600,
        )?;
    }
    crate::config::write_atomic(&dir.join("guest-owned"), b"1\n", 0o600)?;
    let _ = std::fs::remove_file(dir.join("migration-error"));
    Ok(())
}

pub(in crate::vm) fn migration_files(root: &Path) -> Result<Vec<PathBuf>> {
    fn visit(root: &Path, here: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        if !here.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(here)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                visit(root, &entry.path(), files)?;
            } else if kind.is_file() {
                files.push(entry.path().strip_prefix(root)?.to_path_buf());
            } else {
                bail!("unsupported migration file {}", entry.path().display());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

/// The host config as the guest runs it: paths on the data disk, no host
/// checkouts, and the VM section off so nothing forwards again.
pub fn guest_config(host: &Config) -> Config {
    let mut g = host.clone();
    g.driver = Some(DriverKind::Herdr);
    g.herdr.command = GUEST_HERDR.to_string();
    g.herdr.projects_dir = GUEST_PROJECTS_DIR.to_string();
    for r in &mut g.repos {
        r.driver = None;
        r.path = None;
    }
    g.vm = VmConfig::default();
    // The guest serves the web dashboard with its own [dashboard] (#653);
    // a host's is never carried into it.
    g.dashboard = crate::config::DashboardConfig::default();
    g.daemon.startup_driver_wait_secs = 0;
    g
}

/// Where the seed puts a person's signing key and token in the guest.
pub const GUEST_KEYS_DIR: &str = "/home/ssf/.config/ssf/keys";
pub const GUEST_TOKENS_DIR: &str = "/home/ssf/.config/ssf/git-tokens";

/// Rewrite the `[git]` tables (instance and per repository) for the guest,
/// which has none of the host's files: a signing key the host names is
/// copied into `keys_dir` and the guest path put in its place; a
/// `token:<login>` is resolved here with `resolve_token` (gh's keyring)
/// and written to `tokens_dir` as a file the guest reads (`file:...`), and
/// a `file:<path>` is copied the same way. A helper string passes through
/// as it is. Missing credentials stop migration without changing intent.
pub fn guest_git(
    host: &Config,
    guest: &mut Config,
    keys_dir: &Path,
    tokens_dir: &Path,
    resolve_token: &dyn Fn(&str) -> Result<String>,
) -> Result<()> {
    let mut copied: std::collections::BTreeMap<PathBuf, String> = Default::default();
    let mut tables: Vec<(String, &GitConfig, &mut GitConfig)> =
        vec![("[git]".to_string(), &host.git, &mut guest.git)];
    for (h, g) in host.repos.iter().zip(guest.repos.iter_mut()) {
        tables.push((format!("repo {}", h.name), &h.git, &mut g.git));
    }
    for (where_, from, to) in tables {
        if let Some(SigningKey::Path(p)) = &from.signing_key {
            let key = expand_tilde(p);
            if key.exists() {
                let name = place(&key, keys_dir, &mut copied)?;
                let pubkey = crate::keys::public_path(&key);
                if pubkey.exists() {
                    std::fs::copy(&pubkey, keys_dir.join(format!("{name}.pub")))?;
                }
                to.signing_key = Some(SigningKey::Path(format!("{GUEST_KEYS_DIR}/{name}")));
            } else {
                bail!(
                    "{where_}: signing key {} does not exist; restore it before migration",
                    key.display()
                );
            }
        }
        match from.credential.as_deref().map(Credential::parse) {
            Some(Ok(Credential::Token(login))) => match resolve_token(&login) {
                Ok(t) => {
                    std::fs::create_dir_all(tokens_dir)?;
                    write_private(
                        &tokens_dir.join(&login),
                        format!("{}\n", t.trim()).as_bytes(),
                    )?;
                    to.credential = Some(format!("file:{GUEST_TOKENS_DIR}/{login}"));
                }
                Err(e) => bail!(
                    "{where_}: no token for @{login} here ({e:#}); pushes in the VM as @{login} will fail until it is signed in to gh on the host and the VM restarted"
                ),
            },
            Some(Ok(Credential::File(path))) => {
                if path.exists() {
                    let name = place(&path, tokens_dir, &mut copied)?;
                    to.credential = Some(format!("file:{GUEST_TOKENS_DIR}/{name}"));
                } else {
                    bail!(
                        "{where_}: token file {} does not exist; pushes in the VM with it will fail",
                        path.display()
                    );
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Copy `src` into `dir` under its own file name (a numbered one when two
/// different files share a name), once per host path.
pub(in crate::vm) fn place(
    src: &Path,
    dir: &Path,
    copied: &mut std::collections::BTreeMap<PathBuf, String>,
) -> Result<String> {
    if let Some(name) = copied.get(src) {
        return Ok(name.clone());
    }
    std::fs::create_dir_all(dir)?;
    let base = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    let mut name = base.clone();
    let mut n = 1;
    while copied.values().any(|v| v == &name) {
        n += 1;
        name = format!("{base}.{n}");
    }
    std::fs::copy(src, dir.join(&name)).with_context(|| format!("copying {}", src.display()))?;
    set_mode(&dir.join(&name), 0o600)?;
    copied.insert(src.to_path_buf(), name.clone());
    Ok(name)
}

/// `src` or `src:dest` from `[vm] files`: the host file and where it goes
/// in the guest (relative to the guest user's home unless absolute; a bare
/// `src` under the host home keeps its place there).
pub fn parse_file_spec(spec: &str, home: &Path) -> (PathBuf, String) {
    let (src, dest) = match spec.split_once(':') {
        Some((s, d)) if !d.is_empty() => (s, Some(d)),
        _ => (spec, None),
    };
    let src = expand_tilde(src);
    let dest = match dest {
        Some(d) if d.starts_with('/') => d.to_string(),
        Some(d) => d.trim_start_matches("~/").to_string(),
        None => match src.strip_prefix(home) {
            Ok(rel) => rel.to_string_lossy().to_string(),
            Err(_) => src.to_string_lossy().to_string(),
        },
    };
    (src, dest)
}

/// Where the guest scripts are: `SSF_VM_DIR`, the package's
/// `/usr/share/ssf/vm`, `share/ssf/vm` under the prefix this binary is
/// installed in (Homebrew), or `vm/` next to a source build.
pub(in crate::vm) fn scripts_dir() -> Result<PathBuf> {
    if let Ok(d) = std::env::var("SSF_VM_DIR") {
        return Ok(PathBuf::from(d));
    }
    let mut candidates = vec![PathBuf::from("/usr/share/ssf/vm")];
    if let Ok(exe) = std::env::current_exe()
        && let Some(d) = exe.parent()
    {
        candidates.push(d.join("../share/ssf/vm"));
        candidates.push(d.join("../../vm"));
    }
    candidates.push(PathBuf::from("vm"));
    candidates
        .into_iter()
        .find(|p| p.join("guest/provision.sh").exists())
        .map(|p| p.canonicalize().unwrap_or(p))
        .context("the VM scripts (vm/guest/provision.sh) are not installed; set SSF_VM_DIR")
}

/// Is `pid` alive and running `program`?
pub(in crate::vm) fn pid_runs(pid: u32, program: &str) -> bool {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|c| {
            c.split(|b| *b == 0)
                .next()
                .map(|a| String::from_utf8_lossy(a).contains(program))
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

pub(in crate::vm) fn kill(pid: u32, sig: i32) {
    // SAFETY: kill(2) with a pid we started; a wrong pid only fails.
    unsafe {
        libc::kill(pid as i32, sig);
    }
}

/// End `pid` (running `program`): SIGTERM, up to five seconds for it to
/// go, then SIGKILL, and a moment for that to land.
pub(in crate::vm) fn end_process(pid: u32, program: &str) {
    kill(pid, libc::SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(5);
    while pid_runs(pid, program) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if pid_runs(pid, program) {
        kill(pid, libc::SIGKILL);
        let deadline = Instant::now() + Duration::from_secs(2);
        while pid_runs(pid, program) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Start `cmd` in a session of its own so it outlives us, with stdin
/// closed and its output appended to `log` (or dropped). With `scope`, and
/// a systemd user manager to ask ([`scopes_available`]), it runs in the
/// transient `<scope>.scope` rather than in this process's cgroup, so that
/// stopping or restarting the service this process belongs to (the
/// `ssf@NAME` supervisor) leaves it running. A scope that could not be made
/// falls back to the plain start, with a warning.
pub(crate) fn spawn_detached(
    cmd: &mut Command,
    log: Option<&Path>,
    scope: Option<&str>,
) -> Result<u32> {
    if let Some(unit) = scope.filter(|_| scopes_available()) {
        let program = cmd.get_program().to_string_lossy().into_owned();
        let name = Path::new(&program)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(program);
        let mut child = spawn_session(&mut in_own_scope(cmd, unit), log)?;
        // `systemd-run --scope` becomes the program once the scope exists.
        // Until then the PID reads as systemd-run, which a caller checking
        // it at once would take for a program that already exited.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if pid_runs(child.id(), &name) {
                return Ok(child.id());
            }
            if let Ok(Some(st)) = child.try_wait() {
                warn!(
                    "could not start {name} in {unit}.scope ({st}); starting it in this service's own cgroup, where a restart of the service takes it down"
                );
                break;
            }
            if Instant::now() >= deadline {
                return Ok(child.id());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    Ok(spawn_session(cmd, log)?.id())
}

fn spawn_session(cmd: &mut Command, log: Option<&Path>) -> Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    cmd.stdin(Stdio::null());
    match log {
        Some(p) => {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)?;
            cmd.stdout(f.try_clone()?).stderr(f);
        }
        None => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    // SAFETY: setsid is async-signal-safe and touches no shared state.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()
        .with_context(|| format!("starting {:?}", cmd.get_program()))
}

/// Can VM processes be given a systemd scope of their own? Only with a
/// systemd user manager to ask (its private socket under
/// `$XDG_RUNTIME_DIR`, which every user unit, the `ssf@NAME` supervisor
/// included, has) and `systemd-run` that can make one; asked once. Not on
/// macOS, and not in tests, which must not start units on the machine
/// running them.
pub(in crate::vm) fn scopes_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        if cfg!(test) || !cfg!(target_os = "linux") {
            return false;
        }
        let manager = std::env::var_os("XDG_RUNTIME_DIR")
            .map(|d| Path::new(&d).join("systemd/private"))
            .is_some_and(|p| p.exists());
        manager
            && Command::new("systemd-run")
                .args(["--user", "--scope", "--quiet", "--collect", "--", "true"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|st| st.success())
    })
}

/// `cmd` as `systemd-run --user --scope --unit=<unit>` runs it: the same
/// program, arguments, environment and directory, moved into
/// `<unit>.scope` of the user manager before it starts. The scope ends
/// when its last process does.
pub(crate) fn in_own_scope(cmd: &Command, unit: &str) -> Command {
    let mut scoped = Command::new("systemd-run");
    scoped
        .stdin(Stdio::null())
        .args(["--user", "--scope", "--quiet", "--collect"])
        .arg(format!("--unit={}", scope_unit_name(unit)))
        .arg("--")
        .arg(cmd.get_program())
        .args(cmd.get_args());
    for (k, v) in cmd.get_envs() {
        match v {
            Some(v) => scoped.env(k, v),
            None => scoped.env_remove(k),
        };
    }
    if let Some(dir) = cmd.get_current_dir() {
        scoped.current_dir(dir);
    }
    scoped
}

/// A unit name systemd accepts, from a VM's name.
pub(in crate::vm) fn scope_unit_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Does `pid` share this process's cgroup? A VM process that does was
/// started inside this service (by an older ssf, or where no scope could
/// be made), and dies with it; one that does not has its own lifetime.
pub(in crate::vm) fn shares_our_cgroup(pid: u32) -> bool {
    let read = |p: String| std::fs::read_to_string(p).ok().filter(|s| !s.is_empty());
    match (
        read(format!("/proc/{pid}/cgroup")),
        read("/proc/self/cgroup".into()),
    ) {
        (Some(theirs), Some(ours)) => theirs == ours,
        _ => false,
    }
}

/// One request to Firecracker's API socket.
pub(in crate::vm) async fn fc_api(
    sock: &Path,
    method: &str,
    path: &str,
    body: Value,
) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::UnixStream::connect(sock)
        .await
        .with_context(|| format!("connecting to {}", sock.display()))?;
    let body = body.to_string();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await?;
    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf)).await??;
    let reply = String::from_utf8_lossy(&buf[..n]);
    let status = reply.lines().next().unwrap_or("");
    if !status.contains(" 204") && !status.contains(" 200") {
        bail!("firecracker answered {status}");
    }
    Ok(())
}

pub(in crate::vm) async fn download(url: &str, to: &Path) -> Result<()> {
    info!(url, "downloading");
    if let Some(p) = to.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = to.with_extension("part");
    let st = tokio::process::Command::new("curl")
        .args(["-fSL", "--retry", "3", "-o"])
        .arg(&tmp)
        .arg(url)
        .status()
        .await
        .context("running curl")?;
    if !st.success() {
        let _ = std::fs::remove_file(&tmp);
        bail!("downloading {url} failed ({st})");
    }
    std::fs::rename(&tmp, to)?;
    Ok(())
}

pub(in crate::vm) fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .context("running sha256sum")?;
    let actual = String::from_utf8_lossy(&output.stdout);
    let matches = output.status.success() && actual.split_whitespace().next() == Some(expected);
    if !matches {
        let _ = std::fs::remove_file(path);
        bail!(
            "the SHA-256 checksum for {} did not match; the cached download was removed, so rerun `ssf vm build`",
            path.display()
        );
    }
    Ok(())
}

pub(in crate::vm) fn make_executable(p: &Path) -> Result<()> {
    set_mode(p, 0o755)
}

pub(in crate::vm) fn set_mode(p: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("chmod {}", p.display()))
}

pub(in crate::vm) fn write_private(p: &Path, data: &[u8]) -> Result<()> {
    std::fs::write(p, data).with_context(|| format!("writing {}", p.display()))?;
    set_mode(p, 0o600)
}

pub(in crate::vm) fn dir_size(p: &Path) -> Result<u64> {
    let mut total = 0;
    for e in std::fs::read_dir(p)? {
        let e = e?;
        let m = e.metadata()?;
        total += if m.is_dir() {
            dir_size(&e.path())?
        } else {
            m.len()
        };
    }
    Ok(total)
}

pub(in crate::vm) fn run_ok(cmd: &mut Command, what: &str) -> Result<()> {
    let out = cmd
        .output()
        .with_context(|| format!("running {what} (is it installed?)"))?;
    if !out.status.success() {
        bail!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

pub fn stdin_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

pub(in crate::vm) fn which(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    // Anything with a separator in it is a path, not a name to look up:
    // `PATH` is searched for `limactl`, never for `~/bin/limactl` (which
    // reaches here expanded) or `./limactl`.
    if name.contains('/') {
        return p.exists().then(|| p.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|c| c.is_file())
}

/// `<harness> <installed> <logged in>` lines from `Vm::logins`' script.
pub fn parse_login_states(out: &str) -> Vec<LoginState> {
    out.lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            Some(LoginState {
                harness: f.next()?.to_string(),
                installed: f.next()? == "1",
                logged_in: f.next()? == "1",
            })
        })
        .collect()
}

/// A command line for the remote shell.
pub fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c))
            {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A predictable parent permits short-lived `ssf-server` status processes to
/// share the same master. Never follow or repair a directory another user made.
pub(in crate::vm) fn status_control_directory(root: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let uid = unsafe { libc::geteuid() };
    let directory = root.join(format!("ssf-status-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("creating private VM status SSH directory"),
    }
    let metadata = std::fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
        bail!(
            "unsafe VM status SSH directory: {} (expected owned directory with mode 0700)",
            directory.display()
        );
    }
    Ok(directory)
}
