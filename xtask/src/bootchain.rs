// SPDX-License-Identifier: Apache-2.0
//! Verified boot chain (ADR-0030 S-15): hash-pinning of everything Limine loads.
//!
//! * Every file Limine reads from the ESP (kernel, firmware modules) is referenced in `limine.conf`
//!   with its BLAKE2b-512 hash (`path: boot():/boot/kernel#<hash>`), so Limine refuses a modified file.
//! * The hash of `limine.conf` itself is *enrolled* into `BOOTX64.EFI` (Limine's `enroll-config`),
//!   so the config cannot be modified either.
//! * Signing `BOOTX64.EFI` for UEFI Secure Boot is done by the owner with their own keys
//!   (`secure-boot.sh`, shipped in every release), because the private keys must never leave their machine.
//!   The enrolled config hash is part of the signed binary, closing the chain.

use crate::Result;
use blake2::{Blake2b512, Digest};

/// Marker inside Limine's EFI binary that is followed by 128 ASCII characters (the enrolled hash).
const SIGNATURE: &[u8] = b"++CONFIG_B2SUM_SIGNATURE++";

pub fn blake2b_hex(data: &[u8]) -> String {
    Blake2b512::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Returns a copy of Limine's `BOOTX64.EFI` with the BLAKE2b of `conf` enrolled
/// (a port of `limine enroll-config`, which only overwrites those 128 bytes).
pub fn enroll_config(efi: &[u8], conf: &[u8]) -> Result<Vec<u8>> {
    let pos = efi
        .windows(SIGNATURE.len())
        .position(|w| w == SIGNATURE)
        .ok_or("Limine config-hash marker not found in BOOTX64.EFI")?;
    let at = pos + SIGNATURE.len();
    let hash = blake2b_hex(conf);
    debug_assert_eq!(hash.len(), 128);
    let mut out = efi.to_vec();
    out.get_mut(at..at + 128)
        .ok_or("BOOTX64.EFI is truncated after the config-hash marker")?
        .copy_from_slice(hash.as_bytes());
    Ok(out)
}
