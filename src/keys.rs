//! A dedicated SSH key for the bot account: used for SSH pushes and for
//! signing commits, so nothing the agent does is attributed to the human
//! whose desktop it runs on.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct KeyPair {
    pub private: PathBuf,
    pub public: PathBuf,
    pub public_key: String,
}

pub fn public_path(private: &Path) -> PathBuf {
    let mut p = private.as_os_str().to_owned();
    p.push(".pub");
    PathBuf::from(p)
}

/// Load the key at `private`, generating it when missing.
pub fn ensure(private: &Path, comment: &str) -> Result<KeyPair> {
    let public = public_path(private);
    if !private.exists() {
        if let Some(parent) = private.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
            let _ = std::fs::set_permissions(
                parent,
                std::os::unix::fs::PermissionsExt::from_mode(0o700),
            );
        }
        let status = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", comment, "-f"])
            .arg(private)
            .status()
            .context("running ssh-keygen (is openssh installed?)")?;
        if !status.success() {
            bail!("ssh-keygen failed");
        }
    }
    if !public.exists() {
        // Private key without its .pub (copied by hand?): derive it.
        let out = Command::new("ssh-keygen")
            .args(["-y", "-f"])
            .arg(private)
            .output()
            .context("running ssh-keygen -y")?;
        if !out.status.success() {
            bail!("could not derive public key from {}", private.display());
        }
        std::fs::write(&public, out.stdout)?;
    }
    let public_key = std::fs::read_to_string(&public)
        .with_context(|| format!("reading {}", public.display()))?
        .trim()
        .to_string();
    Ok(KeyPair {
        private: private.to_path_buf(),
        public,
        public_key,
    })
}

pub fn remove(private: &Path) {
    let _ = std::fs::remove_file(private);
    let _ = std::fs::remove_file(public_path(private));
}
