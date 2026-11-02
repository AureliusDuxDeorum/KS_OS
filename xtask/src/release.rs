// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask release`: assembles a clearly named, self-contained release folder at the
//! repository root (`KS_OS-vX.Y.Z/`) with the boot image, checksums and instructions.
//!
//! The release image never contains firmware blobs (they cannot be redistributed); the shipped
//! script adds them on the user's own machine.

use crate::{build_kernel, write_image, Ctx, Result};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::Path;

const README: &str = include_str!("../release/README.txt.in");
const ADD_FIRMWARE: &str = include_str!("../release/add-bluetooth-firmware.sh");
const SECURE_BOOT: &str = include_str!("../release/secure-boot.sh");

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

pub fn run(ctx: &Ctx) -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    let name = format!("KS_OS-v{version}");
    let dir = ctx.root.join(&name);
    // A release is always an optimised build with no firmware and no kernel command line.
    let clean = Ctx {
        root: ctx.root.clone(),
        release: true,
        firmware: Vec::new(),
    };
    build_kernel(&clean)?;
    std::fs::create_dir_all(&dir)?;

    let img_name = format!("{name}.img");
    write_image(&clean, "", &dir.join(&img_name))?;

    std::fs::write(dir.join("README.txt"), README.replace("{version}", version))?;
    std::fs::write(dir.join("add-bluetooth-firmware.sh"), ADD_FIRMWARE)?;
    std::fs::write(dir.join("secure-boot.sh"), SECURE_BOOT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for script in ["add-bluetooth-firmware.sh", "secure-boot.sh"] {
            std::fs::set_permissions(dir.join(script), std::fs::Permissions::from_mode(0o755))?;
        }
    }
    // Licence texts travel with every binary release: ours, the notice, and every third-party component.
    std::fs::copy(ctx.root.join("LICENSE"), dir.join("LICENSE"))?;
    std::fs::copy(ctx.root.join("NOTICE"), dir.join("NOTICE"))?;
    std::fs::write(
        dir.join("THIRD-PARTY-LICENSES.txt"),
        crate::notices::generate(&clean)?,
    )?;
    let _ = std::fs::remove_file(dir.join("LICENSES.txt")); // superseded by the files above

    let mut sums = String::new();
    for f in [
        img_name.as_str(),
        "add-bluetooth-firmware.sh",
        "secure-boot.sh",
        "README.txt",
        "LICENSE",
        "NOTICE",
        "THIRD-PARTY-LICENSES.txt",
    ] {
        let _ = writeln!(
            sums,
            "{}  {f}",
            sha256_hex(&std::fs::read(Path::new(&dir).join(f))?)
        );
    }
    std::fs::write(dir.join("SHA256SUMS"), sums)?;
    println!("release: {}", dir.display());
    println!(
        "         {} ({} bytes)",
        img_name,
        std::fs::metadata(dir.join(&img_name))?.len()
    );
    Ok(())
}
