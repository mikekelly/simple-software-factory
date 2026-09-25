//! The Chrome extension's runtime files, zipped by `build.rs` from this
//! build's own `chrome-extension/`, so the extension a person loads matches
//! the factory that serves it.

use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::Path;

pub(crate) const ZIP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/chrome-extension.zip"));

/// The name a download is saved under.
pub(crate) const FILE_NAME: &str = "ssf-chrome-extension.zip";

/// `ssf chrome-extension`: write the zip, refusing to replace a file that is
/// already there unless asked to.
pub(crate) fn write(path: &Path, force: bool) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let mut file = match options.open(path) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!(
                "{} already exists; pass --force to replace it",
                path.display()
            )
        }
        other => other.with_context(|| format!("cannot create {}", path.display()))?,
    };
    file.write_all(ZIP)
        .with_context(|| format!("cannot write {}", path.display()))?;
    println!(
        "Wrote {}. Unzip it, then in chrome://extensions turn on Developer mode, \
         choose Load unpacked and select the unzipped directory.",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The entry names, read from the central directory.
    fn names() -> Vec<String> {
        let u16_at = |at: usize| usize::from(u16::from_le_bytes([ZIP[at], ZIP[at + 1]]));
        let u32_at = |at: usize| u32::from_le_bytes(ZIP[at..at + 4].try_into().unwrap()) as usize;
        let end = ZIP.len() - 22;
        assert_eq!(&ZIP[end..end + 4], b"PK\x05\x06");
        let mut at = u32_at(end + 16);
        (0..u16_at(end + 10))
            .map(|_| {
                assert_eq!(&ZIP[at..at + 4], b"PK\x01\x02");
                let length = u16_at(at + 28);
                let name = String::from_utf8(ZIP[at + 46..at + 46 + length].to_vec()).unwrap();
                at += 46 + length + u16_at(at + 30) + u16_at(at + 32);
                name
            })
            .collect()
    }

    #[test]
    fn zip_holds_the_manifest_at_its_root_and_no_checkout_extras() {
        let names = names();
        assert!(
            names.iter().any(|name| name == "manifest.json"),
            "{names:?}"
        );
        assert!(
            names.iter().any(|name| name.starts_with("vendor/")),
            "{names:?}"
        );
        assert!(
            !names.iter().any(|name| name == "README.md"
                || name.starts_with("test/")
                || name.starts_with("docs/")),
            "{names:?}"
        );
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "entries are in a fixed order");
    }

    #[test]
    fn write_refuses_to_replace_a_file_unless_forced() {
        let dir = std::env::temp_dir().join(format!("ssf-chrome-extension-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, "mine").unwrap();
        assert!(write(&path, false).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"mine");
        write(&path, true).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), ZIP);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
