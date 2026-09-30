//! Relative markdown links in `docs/` must name a file that exists and, when
//! they carry a `#fragment`, a heading in it (GitHub's anchor rules).
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Breakages that predate this check, left for their own fix.
const KNOWN_BROKEN: &[&str] = &["docs/ssf-md.md: prompts.md#how-to-work-on-this (no such heading)"];

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Lines outside fenced code blocks, with inline code spans removed.
fn prose_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let mut kept = String::new();
        let mut in_code = false;
        for c in line.chars() {
            if c == '`' {
                in_code = !in_code;
                kept.push('`');
            } else if !in_code {
                kept.push(c);
            }
        }
        out.push(kept);
    }
    out
}

/// GitHub's heading anchor: lowercase, drop punctuation other than `-` and
/// `_`, spaces to `-`; repeated headings get `-1`, `-2`, ...
fn anchors(text: &str) -> HashSet<String> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = HashSet::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced || !line.starts_with('#') {
            continue;
        }
        let title = line.trim_start_matches('#');
        if !title.starts_with(' ') {
            continue;
        }
        let slug: String = title
            .trim()
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == ' ')
            .map(|c| if c == ' ' { '-' } else { c })
            .collect();
        let n = seen.entry(slug.clone()).or_insert(0);
        out.insert(if *n == 0 {
            slug.clone()
        } else {
            format!("{slug}-{n}")
        });
        *n += 1;
    }
    out
}

fn link_targets(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(i) = rest.find("](") {
        rest = &rest[i + 2..];
        if let Some(end) = rest.find(')') {
            let target = rest[..end].split_whitespace().next().unwrap_or("");
            out.push(target.to_string());
            rest = &rest[end..];
        }
    }
    out
}

fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "history") {
                continue;
            }
            markdown_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

#[test]
fn relative_doc_links_resolve_to_files_and_headings() {
    let mut files = Vec::new();
    markdown_files(&repo().join("docs"), &mut files);
    files.sort();
    let mut broken = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).unwrap();
        for line in prose_lines(&text) {
            for target in link_targets(&line) {
                if target.is_empty() || target.contains("://") || target.starts_with("mailto:") {
                    continue;
                }
                let (path, fragment) = match target.split_once('#') {
                    Some((p, f)) => (p, Some(f)),
                    None => (target.as_str(), None),
                };
                let dest = if path.is_empty() {
                    file.clone()
                } else {
                    file.parent().unwrap().join(path)
                };
                let rel = file.strip_prefix(repo()).unwrap().display();
                if !dest.exists() {
                    broken.push(format!("{rel}: {target} (no such file)"));
                    continue;
                }
                let Some(fragment) = fragment else { continue };
                if dest.extension().is_none_or(|e| e != "md") {
                    continue;
                }
                let dest_text = fs::read_to_string(&dest).unwrap();
                if !anchors(&dest_text).contains(fragment) {
                    broken.push(format!("{rel}: {target} (no such heading)"));
                }
            }
        }
    }
    broken.retain(|b| !KNOWN_BROKEN.contains(&b.as_str()));
    assert!(broken.is_empty(), "broken links:\n{}", broken.join("\n"));
}

#[test]
fn github_anchor_rules() {
    let a = anchors("## 4. `ssf setup` and the service\n## A server\n## A server\n");
    assert!(a.contains("4-ssf-setup-and-the-service"));
    assert!(a.contains("a-server") && a.contains("a-server-1"));
    let b = anchors("### 5.1 A guest on a server: access for the resident agent and the laptop");
    assert!(b.contains("51-a-guest-on-a-server-access-for-the-resident-agent-and-the-laptop"));
}
