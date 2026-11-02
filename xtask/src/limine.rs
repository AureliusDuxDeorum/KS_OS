// SPDX-License-Identifier: Apache-2.0
//! Fetches the pinned, checksum-verified Limine UEFI bootloader.
//!
//! Supply-chain note: we pin an exact upstream commit of the signed-off
//! `v11.x-binary` branch *and* verify the SHA-256 of every artifact we embed.

use crate::{Ctx, Result};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Command;

const REPO: &str = "https://github.com/limine-bootloader/limine";
const COMMIT: &str = "5be26a73d7b7b4d4477d18be94e1d16e615adf56";
const BOOTX64_SHA256: &str = "333f7a69379b1f47e019be215885cdc078b9edd6e9255cbe2f706d6c5ec0ef06";
const LICENSE_SHA256: &str = "f174b8ff78f6f8982a1fc992668f8486eb8cf598f6938923c22e9bb50f8338a8";

pub struct Limine {
    pub bootx64: Vec<u8>,
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn verify(dir: &std::path::Path) -> Result<Limine> {
    let boot = std::fs::read(dir.join("BOOTX64.EFI"))?;
    let lic = std::fs::read(dir.join("LICENSE"))?;
    if sha256_hex(&boot) != BOOTX64_SHA256 {
        return Err("BOOTX64.EFI checksum mismatch — refusing to use it".into());
    }
    if sha256_hex(&lic) != LICENSE_SHA256 {
        return Err("Limine LICENSE checksum mismatch".into());
    }
    Ok(Limine { bootx64: boot })
}

pub fn fetch(ctx: &Ctx) -> Result<Limine> {
    let dir: PathBuf = ctx.target_dir().join("limine");
    if let Ok(l) = verify(&dir) {
        return Ok(l);
    }
    println!("fetching Limine {COMMIT}");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let git = |args: &[&str]| -> Result<()> {
        let st = Command::new("git").current_dir(&dir).args(args).status()?;
        if st.success() {
            Ok(())
        } else {
            Err(format!("git {args:?} failed").into())
        }
    };
    git(&["init", "-q"])?;
    git(&["fetch", "-q", "--depth", "1", REPO, COMMIT])?;
    git(&["checkout", "-q", "FETCH_HEAD"])?;
    verify(&dir)
}
