//! Which platform is this? The one place that reads `/etc/os-release` and
//! looks for Omarchy, so the rest of the code asks a question instead of
//! assuming an answer. Small on purpose: a later macOS port builds on it.

use std::path::PathBuf;
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

#[allow(dead_code)]
pub fn is_arch_like() -> bool {
    detect().is_arch_like()
}

pub fn herdr_install_hint() -> String {
    detect().herdr_install_hint()
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
        let mac = Platform {
            os: Os::MacOs,
            id: "macos".into(),
            id_like: vec![],
            omarchy: false,
            arch: "aarch64",
        };
        assert_eq!(mac.herdr_install_hint(), "brew install herdr");
    }
}
