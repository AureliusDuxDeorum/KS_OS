// SPDX-License-Identifier: Apache-2.0
//! Generates `THIRD-PARTY-LICENSES.txt`: the name, version, licence and full licence/notice texts of
//! every third-party component that is compiled into the kernel image, plus the bootloader.
//!
//! The permissive licences we depend on (MIT, BSD, Apache-2.0) require the copyright notice and licence
//! text to accompany binary redistributions; generating the file from `cargo metadata` keeps it exact.

use crate::{Ctx, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

fn license_files(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let up = name.to_ascii_uppercase();
        let is_notice = up.starts_with("LICENSE")
            || up.starts_with("LICENCE")
            || up.starts_with("COPYING")
            || up.starts_with("NOTICE");
        if is_notice && e.path().is_file() && e.metadata().is_ok_and(|m| m.len() < 200_000) {
            if let Ok(text) = std::fs::read_to_string(e.path()) {
                out.push((name, text));
            }
        }
    }
    out.sort();
    out
}

pub fn generate(ctx: &Ctx) -> Result<String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .current_dir(&ctx.root)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--filter-platform",
            "x86_64-unknown-none",
        ])
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    let meta: Value = serde_json::from_slice(&out.stdout)?;
    let packages = meta["packages"]
        .as_array()
        .ok_or("no packages in metadata")?;
    let by_id: HashMap<&str, &Value> = packages
        .iter()
        .filter_map(|p| Some((p["id"].as_str()?, p)))
        .collect();
    let nodes: HashMap<&str, &Value> = meta["resolve"]["nodes"]
        .as_array()
        .ok_or("no resolve graph")?
        .iter()
        .filter_map(|n| Some((n["id"].as_str()?, n)))
        .collect();

    // Everything reachable from the kernel through normal (non-build, non-dev) dependencies.
    let kernel = packages
        .iter()
        .find(|p| p["name"] == "kernel")
        .and_then(|p| p["id"].as_str())
        .ok_or("kernel package not found")?;
    let mut seen: HashSet<&str> = HashSet::new();
    let mut stack = vec![kernel];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        for d in nodes
            .get(id)
            .and_then(|n| n["deps"].as_array())
            .into_iter()
            .flatten()
        {
            let normal = d["dep_kinds"]
                .as_array()
                .is_some_and(|k| k.iter().any(|k| k["kind"].is_null()));
            if let (true, Some(pkg)) = (normal, d["pkg"].as_str()) {
                stack.push(pkg);
            }
        }
    }
    let mut deps: BTreeMap<String, &Value> = BTreeMap::new();
    for id in seen {
        let p = by_id[id];
        if !p["source"].is_null() {
            deps.insert(
                format!(
                    "{} {}",
                    p["name"].as_str().unwrap_or("?"),
                    p["version"].as_str().unwrap_or("?")
                ),
                p,
            );
        }
    }

    let mut text = String::new();
    let _ = writeln!(
        text,
        "THIRD-PARTY LICENCES\n====================\n\nKS_OS itself is licensed under the Apache License 2.0 (see LICENSE and NOTICE).\n\
         The boot image additionally contains the components listed below, each under its own licence.\n\
         {} Rust crates are compiled into the kernel; the Limine bootloader is included as a binary.\n",
        deps.len()
    );
    let mut seen_text: HashMap<[u8; 32], String> = HashMap::new();
    let mut emit = |text: &mut String, who: &str, files: Vec<(String, String)>| {
        for (name, body) in files {
            let hash: [u8; 32] = Sha256::digest(body.as_bytes()).into();
            if let Some(first) = seen_text.get(&hash) {
                let _ = writeln!(
                    text,
                    "--- {name}: identical to the text printed for {first} ---"
                );
            } else {
                let _ = writeln!(text, "--- {name} ---\n{}\n", body.trim_end());
                seen_text.insert(hash, who.to_string());
            }
        }
    };
    for (label, p) in &deps {
        let _ = writeln!(
            text,
            "\n==========================================================================\n{label}\nLicence: {}\nSource:  {}",
            p["license"].as_str().unwrap_or("(not declared)"),
            p["repository"].as_str().unwrap_or("crates.io")
        );
        let dir = Path::new(p["manifest_path"].as_str().unwrap_or(""))
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let files = license_files(&dir);
        if files.is_empty() {
            let _ = writeln!(
                text,
                "(this crate's package contains no licence file; see the licence expression above)"
            );
        }
        emit(&mut text, label, files);
    }
    let limine = std::fs::read_to_string(ctx.target_dir().join("limine").join("LICENSE"))
        .unwrap_or_default();
    let _ = writeln!(
        text,
        "\n==========================================================================\nLimine bootloader (BOOTX64.EFI)\nLicence: BSD-2-Clause\nSource:  https://github.com/limine-bootloader/limine (pinned commit: xtask/src/limine.rs)"
    );
    emit(&mut text, "Limine", vec![("LICENSE".into(), limine)]);
    Ok(text)
}
