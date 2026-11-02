// SPDX-License-Identifier: Apache-2.0
//! ACPI MCFG table: where PCI Express enhanced configuration (ECAM) space lives.

use crate::bytes::{u16_at, u64_at, u8_at};
use alloc::vec::Vec;

/// One ECAM region: configuration space for buses `start_bus..=end_bus` of `segment`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcamRegion {
    pub base: u64,
    pub segment: u16,
    pub start_bus: u8,
    pub end_bus: u8,
}

#[derive(Debug, PartialEq, Eq)]
pub enum McfgError {
    TooShort,
    BadSignature,
}

/// Parses an MCFG table; malformed entries (inverted bus range, unaligned base) are skipped.
pub fn parse(table: &[u8]) -> Result<Vec<EcamRegion>, McfgError> {
    if table.len() < 44 {
        return Err(McfgError::TooShort);
    }
    if table.get(0..4) != Some(&b"MCFG"[..]) {
        return Err(McfgError::BadSignature);
    }
    let mut out = Vec::new();
    for e in table.get(44..).unwrap_or(&[]).chunks_exact(16) {
        let (Some(base), Some(segment), Some(start_bus), Some(end_bus)) =
            (u64_at(e, 0), u16_at(e, 8), u8_at(e, 10), u8_at(e, 11))
        else {
            continue;
        };
        // ECAM bases are 1 MiB aligned (one bus = 1 MiB).
        if start_bus <= end_bus && base % (1 << 20) == 0 {
            out.push(EcamRegion {
                base,
                segment,
                start_bus,
                end_bus,
            });
        }
    }
    Ok(out)
}

impl EcamRegion {
    /// Physical address of a function's 4 KiB configuration space, if it falls in this region.
    pub fn function_address(&self, bus: u8, device: u8, function: u8) -> Option<u64> {
        if bus < self.start_bus || bus > self.end_bus || device > 31 || function > 7 {
            return None;
        }
        let offset = (u64::from(bus.checked_sub(self.start_bus)?) << 20)
            | (u64::from(device) << 15)
            | (u64::from(function) << 12);
        self.base.checked_add(offset)
    }
}
