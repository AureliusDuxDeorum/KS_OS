// SPDX-License-Identifier: Apache-2.0
//! Generates `STRUCTURE.md`: the platform/structure reference that is handed to coding agents.
//!
//! The hand-written text lives in `xtask/structure/STRUCTURE.md.in`; this module fills in the
//! version, the crate table, the annotated file tree and the ADR index from the repository itself,
//! so the document cannot drift. `--check` (used by CI) fails if the committed file is stale.

use crate::{Ctx, Result};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

const MAX_DESC: usize = 110;

fn git_files(root: &Path) -> Result<Vec<String>> {
    let out = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .current_dir(root)
        .output()?;
    if !out.status.success() {
        return Err("git ls-files failed".into());
    }
    let mut files: Vec<String> = String::from_utf8(out.stdout)?
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    // A file that is tracked but deleted in the working tree should not appear.
    files.retain(|f| root.join(f).is_file());
    files.sort();
    files.dedup();
    Ok(files)
}

fn shorten(s: &str) -> String {
    let s = s.trim().trim_end_matches(['.', ':']).trim();
    let s = s.split(". ").next().unwrap_or(s);
    if s.chars().count() <= MAX_DESC {
        return s.to_owned();
    }
    let cut: String = s.chars().take(MAX_DESC - 1).collect();
    let cut = cut
        .rsplit_once(' ')
        .map_or(cut.as_str(), |(a, _)| a)
        .to_owned();
    format!("{cut}…")
}

fn describe(root: &Path, path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let fixed = match name {
        "LICENSE" => "Apache License 2.0",
        "NOTICE" => "Copyright and third-party notice",
        "Cargo.lock" => "Pinned dependency versions",
        "rust-toolchain.toml" => "Pinned Rust toolchain",
        "deny.toml" => "cargo-deny policy (licences, advisories, sources)",
        "config.toml" => "Cargo target flags (static, non-PIE)",
        "linker.ld" => "Kernel linker script (higher half, W^X segments)",
        ".gitignore" => "Ignored files",
        "ci.yml" => "GitHub Actions: build, lint, test, deny, structure check",
        "SHA256SUMS" => "Checksums of the release files",
        "STRUCTURE.md" => {
            "This file: platform and structure reference for coding agents (generated)"
        }
        _ => "",
    };
    if !fixed.is_empty() {
        return fixed.to_owned();
    }
    let Ok(text) = std::fs::read_to_string(root.join(path)) else {
        return String::new();
    };
    if name.ends_with(".rs") {
        let mut doc = String::new();
        for l in text.lines().take(30) {
            if let Some(d) = l.trim().strip_prefix("//!") {
                let d = d.trim();
                if d.is_empty() {
                    if doc.is_empty() {
                        continue;
                    }
                    break;
                }
                doc.push_str(d);
                doc.push(' ');
                if d.ends_with('.') {
                    break;
                }
            } else if !doc.is_empty() || !l.trim_start().starts_with("//") && !l.trim().is_empty() {
                break;
            }
        }
        return shorten(&doc);
    }
    if name.ends_with(".md") || name.ends_with(".in") {
        if let Some(h) = text.lines().find_map(|l| l.strip_prefix("# ")) {
            return shorten(h);
        }
    }
    if path == "Cargo.toml" {
        return "Workspace manifest: members and the single release version".to_owned();
    }
    if name == "Cargo.toml" {
        if let Some(l) = text.lines().find(|l| l.starts_with("description")) {
            if let Some((_, v)) = l.split_once('=') {
                return shorten(v.trim().trim_matches('"'));
            }
        }
    }
    if name.ends_with(".sh") || name.ends_with(".txt") {
        if let Some(l) = text
            .lines()
            .find(|l| l.starts_with("# ") && !l.contains("SPDX"))
        {
            return shorten(l.trim_start_matches("# "));
        }
    }
    String::new()
}

#[derive(Default)]
struct Node {
    children: BTreeMap<String, Node>,
    file: Option<String>, // full path if this node is a file
    count: usize,         // files below (for collapsed directories)
}

fn render(node: &Node, root: &Path, prefix: &str, out: &mut String) {
    let n = node.children.len();
    for (i, (name, child)) in node.children.iter().enumerate() {
        let last = i + 1 == n;
        let branch = if last { "└── " } else { "├── " };
        let next_prefix = format!("{prefix}{}", if last { "    " } else { "│   " });
        if let Some(path) = &child.file {
            let d = describe(root, path);
            let line = if d.is_empty() {
                format!("{prefix}{branch}{name}\n")
            } else {
                format!("{prefix}{branch}{name} — {d}\n")
            };
            out.push_str(&line);
        } else if name.starts_with("KS_OS-v") {
            out.push_str(&format!(
                "{prefix}{branch}{name}/ — release: boot image, SHA256SUMS, licences ({} files)\n",
                child.count
            ));
        } else {
            out.push_str(&format!("{prefix}{branch}{name}/\n"));
            render(child, root, &next_prefix, out);
        }
    }
}

fn count(node: &mut Node) -> usize {
    if node.file.is_some() {
        node.count = 1;
        return 1;
    }
    let mut c = 0;
    for ch in node.children.values_mut() {
        c += count(ch);
    }
    node.count = c;
    c
}

fn tree(root: &Path, files: &[String]) -> String {
    let mut top = Node::default();
    for f in files {
        let mut cur = &mut top;
        for part in f.split('/') {
            cur = cur.children.entry(part.to_owned()).or_default();
        }
        cur.file = Some(f.clone());
    }
    count(&mut top);
    let mut s = String::from("KS_OS/\n");
    render(&top, root, "", &mut s);
    s
}

fn crates(root: &Path, files: &[String]) -> String {
    let mut rows = vec![
        "| Crate | Path | Role |".to_owned(),
        "|---|---|---|".to_owned(),
    ];
    for f in files {
        let is_member = (f.starts_with("crates/") && f.matches('/').count() == 2)
            || f == "kernel/Cargo.toml"
            || f == "xtask/Cargo.toml"
            || f == "fuzz/Cargo.toml";
        if !f.ends_with("Cargo.toml") || !is_member {
            continue;
        }
        let text = std::fs::read_to_string(root.join(f)).unwrap_or_default();
        let name = text
            .lines()
            .find_map(|l| l.strip_prefix("name = "))
            .map_or(String::new(), |n| n.trim_matches('"').to_owned());
        let role = match name.as_str() {
            "kernel" => {
                "The kernel binary (`no_std`, ring 0). All `unsafe` hardware glue lives here."
                    .to_owned()
            }
            "xtask" => {
                "Host tooling: build, image, QEMU tests, release, this document (`cargo xtask …`)."
                    .to_owned()
            }
            "ksparse-fuzz" => "libFuzzer targets (nightly; standalone workspace).".to_owned(),
            _ => text
                .lines()
                .find_map(|l| l.strip_prefix("description = "))
                .map_or(String::new(), |d| d.trim_matches('"').to_owned()),
        };
        let dir = f.trim_end_matches("/Cargo.toml");
        rows.push(format!("| `{name}` | `{dir}/` | {role} |"));
    }
    rows.join("\n")
}

fn adrs(root: &Path, files: &[String]) -> String {
    let mut out = String::new();
    for f in files {
        let Some(n) = f.strip_prefix("docs/adr/") else {
            continue;
        };
        if !n.ends_with(".md") || !n.as_bytes()[0].is_ascii_digit() {
            continue;
        }
        let text = std::fs::read_to_string(root.join(f)).unwrap_or_default();
        let title = text
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .unwrap_or(n)
            .to_owned();
        out.push_str(&format!("- [`{n}`](docs/adr/{n}) — {}\n", shorten(&title)));
    }
    out
}

pub fn generate(ctx: &Ctx) -> Result<String> {
    let root = &ctx.root;
    let mut files = git_files(root)?;
    if !files.iter().any(|f| f == "STRUCTURE.md") {
        files.push("STRUCTURE.md".to_owned()); // always listed, even on the very first run
        files.sort();
    }
    let template = std::fs::read_to_string(root.join("xtask/structure/STRUCTURE.md.in"))?;
    let manifest = std::fs::read_to_string(root.join("Cargo.toml"))?;
    let version = manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = "))
        .map_or("unknown".to_owned(), |v| v.trim_matches('"').to_owned());
    let mut doc = template
        .replace("{{VERSION}}", &format!("`{version}`"))
        .replace("{{CRATES}}", &crates(root, &files))
        .replace("{{TREE}}", tree(root, &files).trim_end())
        .replace("{{ADRS}}", adrs(root, &files).trim_end());
    if !doc.ends_with('\n') {
        doc.push('\n');
    }
    Ok(doc)
}

/// `cargo xtask structure [--check]`.
pub fn run(ctx: &Ctx, check: bool) -> Result<()> {
    let doc = generate(ctx)?;
    let path = ctx.root.join("STRUCTURE.md");
    if check {
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if current != doc {
            return Err(
                "STRUCTURE.md is stale: run `cargo xtask structure` and commit the result".into(),
            );
        }
        println!("STRUCTURE.md is up to date");
    } else {
        std::fs::write(&path, doc)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}
