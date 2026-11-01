//! Firmware blobs (Bluetooth patch files and the like) supplied by the user at image-build time.
//!
//! They are never committed. `--firmware-dir <dir>` (or `KS_FIRMWARE_DIR`) is scanned (up to
//! three directory levels) for `*.bin` and `*.bin.zst` files; zstd files are decompressed here,
//! and every file's SHA-256 is printed so the user can check what ships in the image. The files
//! become Limine modules the kernel reads at boot.

use crate::Result;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// One firmware file, ready to be placed on the ESP.
#[derive(Clone)]
pub struct Blob {
    pub name: String,
    pub data: Vec<u8>,
}

const MAX_BLOB: usize = 8 * 1024 * 1024;

fn walk(dir: &Path, depth: usize, out: &mut Vec<Blob>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            if depth > 0 {
                walk(&path, depth - 1, out)?;
            }
            continue;
        }
        let Some(file) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let (name, compressed) = match file.strip_suffix(".zst") {
            Some(n) if n.ends_with(".bin") => (n.to_string(), true),
            None if file.ends_with(".bin") => (file.to_string(), false),
            _ => continue,
        };
        let raw = std::fs::read(&path)?;
        let data = if compressed {
            let mut decoder = ruzstd::decoding::StreamingDecoder::new(&raw[..])
                .map_err(|e| format!("{}: not valid zstd: {e}", path.display()))?;
            let mut buf = Vec::new();
            decoder
                .by_ref()
                .take(MAX_BLOB as u64 + 1)
                .read_to_end(&mut buf)?;
            buf
        } else {
            raw
        };
        if data.is_empty() || data.len() > MAX_BLOB {
            return Err(
                format!("{}: unreasonable size {} bytes", path.display(), data.len()).into(),
            );
        }
        if out.iter().any(|b| b.name == name) {
            continue; // first one found wins; duplicates are harmless
        }
        out.push(Blob { name, data });
    }
    Ok(())
}

/// Loads every firmware blob from `dir`.
pub fn load(dir: &Path) -> Result<Vec<Blob>> {
    let mut out = Vec::new();
    walk(dir, 3, &mut out)?;
    out.sort_by(|a, b| a.name.cmp(&b.name));
    for b in &out {
        let hash: String = Sha256::digest(&b.data)
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect();
        println!(
            "firmware: {} ({} bytes, sha256 {hash})",
            b.name,
            b.data.len()
        );
    }
    if out.is_empty() {
        println!(
            "firmware: no *.bin / *.bin.zst files found in {}",
            dir.display()
        );
    }
    Ok(out)
}
