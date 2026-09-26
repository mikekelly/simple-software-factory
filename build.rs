//! Packs the Chrome extension's runtime files into a zip the binary embeds
//! (`src/chrome_extension.rs`). The archive is stored (uncompressed) and
//! deterministic: entries are sorted and carry a fixed timestamp, so the
//! same tree always yields the same bytes.

use std::fs;
use std::path::Path;

const SOURCE: &str = "chrome-extension";
/// What a checkout carries that the loaded extension does not need.
const EXCLUDED: [&str; 3] = ["test", "docs", "README.md"];
/// 1980-01-01 00:00, the earliest time a zip entry can hold.
const DOS_TIME: u16 = 0;
const DOS_DATE: u16 = (1 << 5) | 1;

fn main() {
    println!("cargo:rerun-if-changed={SOURCE}");
    let mut files = Vec::new();
    collect(Path::new(SOURCE), "", &mut files);
    files.sort();
    let zip = archive(&files);
    let out = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    fs::write(Path::new(&out).join("chrome-extension.zip"), zip).expect("write the zip");
    commit();
}

/// The checkout's short commit, shown with the version on the dashboard
/// (`SSF_COMMIT`); "unknown" where the build is not from a git checkout (a
/// packaged tarball).
fn commit() {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    };
    if let Some(dir) = git(&["rev-parse", "--git-dir"]) {
        // A commit or checkout moves HEAD and appends to its log.
        println!("cargo:rerun-if-changed={dir}/HEAD");
        println!("cargo:rerun-if-changed={dir}/logs/HEAD");
    }
    let sha = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=SSF_COMMIT={sha}");
}

fn collect(dir: &Path, prefix: &str, files: &mut Vec<(String, Vec<u8>)>) {
    for entry in fs::read_dir(dir).expect("read chrome-extension") {
        let entry = entry.expect("read chrome-extension entry");
        let name = entry.file_name().into_string().expect("UTF-8 file name");
        if prefix.is_empty() && EXCLUDED.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        let relative = format!("{prefix}{name}");
        if path.is_dir() {
            collect(&path, &format!("{relative}/"), files);
        } else {
            files.push((relative, fs::read(&path).expect("read extension file")));
        }
    }
}

fn archive(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let size = u32::try_from(data.len()).expect("file under 4 GiB");
        // Local file header.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        common(&mut out, name, crc, size);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        // Central directory entry.
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        common(&mut central, name, crc, size);
        central.extend_from_slice(&0u16.to_le_bytes()); // comment length
        central.extend_from_slice(&0u16.to_le_bytes()); // disk number
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        central.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let start = out.len() as u32;
    let count = u16::try_from(files.len()).expect("under 65536 files");
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// The fields a local header and a central entry share, from "version
/// needed" through "extra field length".
fn common(out: &mut Vec<u8>, name: &str, crc: u32, size: u32) {
    out.extend_from_slice(&20u16.to_le_bytes()); // version needed
    out.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 names
    out.extend_from_slice(&0u16.to_le_bytes()); // stored
    out.extend_from_slice(&DOS_TIME.to_le_bytes());
    out.extend_from_slice(&DOS_DATE.to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // extra length
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}
