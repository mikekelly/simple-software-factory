//! Which platform is this, and what differs between the host operating
//! systems ssf runs on? The one place that reads `/etc/os-release`, looks
//! for Omarchy and knows the daemon's service (a systemd user unit on
//! Linux, a Homebrew launchd service on macOS), so the rest of the code
//! asks a question instead of assuming an answer.

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)] // MacOs ends in Os, and that is its name
pub enum Os {
    Linux,
    MacOs,
    Other,
}

impl Os {
    fn current() -> Os {
        match std::env::consts::OS {
            "linux" => Os::Linux,
            "macos" => Os::MacOs,
            _ => Os::Other,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Platform {
    pub os: Os,
    /// `ID=` from `/etc/os-release` ("linux" when there is no file).
    pub id: String,
    /// `ID_LIKE=` from `/etc/os-release`, split on spaces.
    pub id_like: Vec<String>,
    /// Omarchy by id, or its shell commands are on PATH.
    pub omarchy: bool,
    pub arch: &'static str,
}

impl Platform {
    fn is_like(&self, ids: &[&str]) -> bool {
        ids.contains(&self.id.as_str()) || self.id_like.iter().any(|l| ids.contains(&l.as_str()))
    }

    pub fn is_arch_like(&self) -> bool {
        self.is_like(&["arch"])
    }

    pub fn is_debian_like(&self) -> bool {
        self.is_like(&["debian", "ubuntu"])
    }

    pub fn is_fedora_like(&self) -> bool {
        self.is_like(&["fedora", "rhel", "centos"])
    }

    /// What to run to install herdr here: the command only.
    pub fn herdr_install_hint(&self) -> String {
        let binary = format!(
            "sudo curl -fsSL -o /usr/local/bin/herdr https://github.com/herdrdev/herdr/releases/latest/download/herdr-linux-{} && sudo chmod +x /usr/local/bin/herdr",
            match self.arch {
                "aarch64" => "aarch64",
                _ => "x86_64",
            }
        );
        match self.os {
            Os::MacOs => "brew install herdr".to_string(),
            _ if self.omarchy => "sudo pacman -S herdr (Omarchy's package repository)".to_string(),
            _ if self.is_arch_like() => {
                format!("herdr from the AUR (herdr-bin), or the release binary: {binary}")
            }
            _ => binary,
        }
    }

    /// What removes the ssf package here: `sudo pacman -R ssf` on Arch
    /// (Omarchy included), apt on Debian and Ubuntu, dnf on Fedora, RHEL
    /// and CentOS, brew on macOS. Printed, never run: nothing in ssf runs
    /// sudo.
    pub fn package_removal_command(&self) -> String {
        match self.os {
            Os::MacOs => "brew uninstall ssf",
            _ if self.is_arch_like() => "sudo pacman -R ssf",
            _ if self.is_debian_like() => "sudo apt remove ssf",
            _ if self.is_fedora_like() => "sudo dnf remove ssf",
            _ => "remove the ssf package with your package manager",
        }
        .to_string()
    }
}

/// `ID` and `ID_LIKE` from an os-release file; quotes stripped, `ID_LIKE`
/// split on spaces. Missing keys give "linux" and nothing.
pub fn parse_os_release(text: &str) -> (String, Vec<String>) {
    let mut id = None;
    let mut id_like = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("ID=") {
            id = Some(unquote(v).to_string());
        } else if let Some(v) = line.strip_prefix("ID_LIKE=") {
            id_like = unquote(v).split_whitespace().map(str::to_string).collect();
        }
    }
    (
        id.filter(|s| !s.is_empty())
            .unwrap_or_else(|| "linux".to_string()),
        id_like,
    )
}

fn unquote(v: &str) -> &str {
    let v = v.trim();
    v.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(v)
}

/// The first file called `name` on PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

pub fn detect() -> &'static Platform {
    static PLATFORM: OnceLock<Platform> = OnceLock::new();
    PLATFORM.get_or_init(|| {
        let os = Os::current();
        let (id, id_like) = match os {
            Os::Linux => std::fs::read_to_string("/etc/os-release")
                .map(|t| parse_os_release(&t))
                .unwrap_or_else(|_| ("linux".to_string(), Vec::new())),
            Os::MacOs => ("macos".to_string(), Vec::new()),
            Os::Other => (std::env::consts::OS.to_string(), Vec::new()),
        };
        let omarchy = id == "omarchy" || which("omarchy-plugin-enable").is_some();
        Platform {
            os,
            id,
            id_like,
            omarchy,
            arch: std::env::consts::ARCH,
        }
    })
}

pub fn is_omarchy() -> bool {
    detect().omarchy
}

pub fn herdr_install_hint() -> String {
    detect().herdr_install_hint()
}

pub fn package_removal_command() -> String {
    detect().package_removal_command()
}

pub fn is_macos() -> bool {
    detect().os == Os::MacOs
}

// ---- the daemon's service ----

/// The systemd user unit (`systemctl --user ... ssf.service`); inside the
/// guest, the system unit of the same name.
pub const SERVICE: &str = "ssf.service";
/// The launchd label Homebrew gives `brew services start ssf`.
pub const LAUNCHD_LABEL: &str = "homebrew.mxcl.ssf";
/// The Homebrew formula (`brew services <action> ssf`).
pub const BREW_FORMULA: &str = "ssf";

/// `gui/<uid>/homebrew.mxcl.ssf`: the service in the user's login session.
fn launchd_target() -> String {
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    format!("gui/{uid}/{LAUNCHD_LABEL}")
}

/// The command a person types to `start`, `stop` or `restart` the daemon's
/// service on `os` (`linux`, `macos`).
pub fn service_hint_for(os: &str, action: &str) -> String {
    if os == "macos" {
        format!("brew services {action} {BREW_FORMULA}")
    } else {
        format!("systemctl --user {action} {SERVICE}")
    }
}

/// `service_hint_for` on this machine.
pub fn service_hint(action: &str) -> String {
    service_hint_for(std::env::consts::OS, action)
}

/// Is the daemon's service running? On Linux `systemctl --user is-active`
/// (the system unit inside the guest, where the daemon is a system
/// service); on macOS `launchctl print` and its `state = running` line.
pub fn service_active() -> bool {
    if is_macos() && !crate::vm::in_guest() {
        let out = Command::new("launchctl")
            .args(["print", &launchd_target()])
            .stdin(Stdio::null())
            .output();
        return match out {
            Ok(o) => {
                o.status.success() && launchctl_says_running(&String::from_utf8_lossy(&o.stdout))
            }
            Err(_) => false,
        };
    }
    let mut cmd = Command::new("systemctl");
    if !crate::vm::in_guest() {
        cmd.arg("--user");
    }
    cmd.args(["is-active", "--quiet", SERVICE])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `launchctl print` output for a service that runs has `state = running`.
pub fn launchctl_says_running(text: &str) -> bool {
    text.lines()
        .any(|l| l.trim().starts_with("state = ") && l.trim() == "state = running")
}

pub fn service_start() -> Result<()> {
    service_action("start")
}

pub fn service_stop() -> Result<()> {
    service_action("stop")
}

// The Omarchy menu restarts through `bin/ssf-ui` today; this is the
// cross-platform way for what comes next (the Homebrew service).
#[allow(dead_code)]
pub fn service_restart() -> Result<()> {
    service_action("restart")
}

/// Start, stop or restart the service: `systemctl --user <action>` on
/// Linux; `brew services <action> ssf` on macOS, which loads or unloads
/// the launchd agent (a `launchctl kill` alone would not hold: the agent
/// is `keep_alive`, so launchd would start `ssf run` again).
fn service_action(action: &str) -> Result<()> {
    let mut cmd = if is_macos() {
        let mut c = Command::new("brew");
        c.args(["services", action, BREW_FORMULA]);
        c
    } else {
        let mut c = Command::new("systemctl");
        c.args(["--user", action, SERVICE]);
        c
    };
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {:?}", cmd.get_program()))?;
    if !out.status.success() {
        bail!(
            "`{}` failed: {}",
            service_hint(action),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux(id: &str, id_like: &[&str], omarchy: bool) -> Platform {
        Platform {
            os: Os::Linux,
            id: id.into(),
            id_like: id_like.iter().map(|s| s.to_string()).collect(),
            omarchy,
            arch: "x86_64",
        }
    }

    fn mac() -> Platform {
        Platform {
            os: Os::MacOs,
            id: "macos".into(),
            id_like: vec![],
            omarchy: false,
            arch: "aarch64",
        }
    }

    #[test]
    fn parses_omarchy_ubuntu_and_fedora_files() {
        let omarchy = "NAME=\"Omarchy\"\nID=omarchy\nID_LIKE=arch\nBUILD_ID=rolling\n";
        assert_eq!(
            parse_os_release(omarchy),
            ("omarchy".to_string(), vec!["arch".to_string()])
        );
        let ubuntu =
            "PRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nNAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\n";
        assert_eq!(
            parse_os_release(ubuntu),
            ("ubuntu".to_string(), vec!["debian".to_string()])
        );
        let fedora = "NAME=\"Fedora Linux\"\nVERSION=\"41 (Workstation Edition)\"\nID=fedora\n";
        assert_eq!(parse_os_release(fedora), ("fedora".to_string(), vec![]));
    }

    #[test]
    fn parses_quoted_values_and_several_likes() {
        let text = "ID=\"linuxmint\"\nID_LIKE=\"ubuntu debian\"\n";
        assert_eq!(
            parse_os_release(text),
            (
                "linuxmint".to_string(),
                vec!["ubuntu".to_string(), "debian".to_string()]
            )
        );
        let single = "ID='rocky'\nID_LIKE='rhel centos fedora'\n";
        assert_eq!(parse_os_release(single).0, "rocky");
        assert_eq!(parse_os_release(single).1.len(), 3);
    }

    #[test]
    fn missing_keys_give_linux_and_nothing() {
        assert_eq!(parse_os_release(""), ("linux".to_string(), vec![]));
        assert_eq!(
            parse_os_release("NAME=Something\nVERSION_ID=1\n"),
            ("linux".to_string(), vec![])
        );
        assert_eq!(
            parse_os_release("ID=\nID_LIKE=\n"),
            ("linux".to_string(), vec![])
        );
    }

    #[test]
    fn arch_like_follows_id_and_id_like() {
        assert!(linux("omarchy", &["arch"], true).is_arch_like());
        assert!(linux("arch", &[], false).is_arch_like());
        assert!(linux("manjaro", &["arch"], false).is_arch_like());
        assert!(!linux("ubuntu", &["debian"], false).is_arch_like());
        assert!(!linux("fedora", &[], false).is_arch_like());
        assert!(!linux("linux", &[], false).is_arch_like());
    }

    #[test]
    fn herdr_hint_names_the_platform_route() {
        assert_eq!(
            linux("omarchy", &["arch"], true).herdr_install_hint(),
            "sudo pacman -S herdr (Omarchy's package repository)"
        );
        let arch = linux("arch", &[], false).herdr_install_hint();
        assert!(
            arch.starts_with("herdr from the AUR (herdr-bin), or the release binary: sudo curl")
        );
        assert!(arch.contains("herdr-linux-x86_64"));
        let ubuntu = linux("ubuntu", &["debian"], false).herdr_install_hint();
        assert!(ubuntu.starts_with("sudo curl -fsSL -o /usr/local/bin/herdr"));
        assert!(ubuntu.ends_with("sudo chmod +x /usr/local/bin/herdr"));
        let mut pi = linux("debian", &[], false);
        pi.arch = "aarch64";
        assert!(pi.herdr_install_hint().contains("herdr-linux-aarch64"));
        assert_eq!(mac().herdr_install_hint(), "brew install herdr");
    }

    #[test]
    fn package_removal_names_the_package_manager() {
        let pacman = "sudo pacman -R ssf";
        assert_eq!(
            linux("omarchy", &["arch"], true).package_removal_command(),
            pacman
        );
        assert_eq!(linux("arch", &[], false).package_removal_command(), pacman);
        assert_eq!(
            linux("manjaro", &["arch"], false).package_removal_command(),
            pacman
        );
        let apt = "sudo apt remove ssf";
        assert_eq!(linux("debian", &[], false).package_removal_command(), apt);
        assert_eq!(
            linux("ubuntu", &["debian"], false).package_removal_command(),
            apt
        );
        assert_eq!(
            linux("linuxmint", &["ubuntu", "debian"], false).package_removal_command(),
            apt
        );
        let dnf = "sudo dnf remove ssf";
        assert_eq!(linux("fedora", &[], false).package_removal_command(), dnf);
        assert_eq!(
            linux("rhel", &["fedora"], false).package_removal_command(),
            dnf
        );
        assert_eq!(
            linux("centos", &["rhel", "fedora"], false).package_removal_command(),
            dnf
        );
        assert_eq!(
            linux("rocky", &["rhel", "centos", "fedora"], false).package_removal_command(),
            dnf
        );
        assert_eq!(mac().package_removal_command(), "brew uninstall ssf");
        assert_eq!(
            linux("linux", &[], false).package_removal_command(),
            "remove the ssf package with your package manager"
        );
        assert_eq!(
            linux("alpine", &[], false).package_removal_command(),
            "remove the ssf package with your package manager"
        );
    }

    #[test]
    fn service_hints_name_the_command_for_the_os() {
        assert_eq!(
            service_hint_for("linux", "stop"),
            "systemctl --user stop ssf.service"
        );
        assert_eq!(
            service_hint_for("linux", "restart"),
            "systemctl --user restart ssf.service"
        );
        assert_eq!(service_hint_for("macos", "stop"), "brew services stop ssf");
        assert_eq!(
            service_hint_for("macos", "start"),
            "brew services start ssf"
        );
        // This machine gets one of the two.
        let here = service_hint("stop");
        assert!(
            here == service_hint_for("linux", "stop") || here == service_hint_for("macos", "stop")
        );
    }

    #[test]
    fn launchctl_print_is_read_for_its_state_line() {
        let running = "gui/501/homebrew.mxcl.ssf = {\n\tactive count = 1\n\tpath = /Users/me/Library/LaunchAgents/homebrew.mxcl.ssf.plist\n\tstate = running\n\n\tprogram = /opt/homebrew/opt/ssf/bin/ssf\n}\n";
        assert!(launchctl_says_running(running));
        assert!(!launchctl_says_running(
            &running.replace("state = running", "state = not running")
        ));
        assert!(!launchctl_says_running(""));
        assert!(launchd_target().ends_with("/homebrew.mxcl.ssf"));
        assert!(launchd_target().starts_with("gui/"));
    }
}
