// SPDX-License-Identifier: Apache-2.0
//! AMD-Vi device table entries (AMD IOMMU spec, section 3.2.2). A DTE is 256 bits (four `u64`) and the
//! table has one entry per 16-bit PCI device id (`bus << 8 | devfn`).

use crate::ADDR_MASK;

pub const DTE_WORDS: usize = 4;
/// 65 536 entries x 32 bytes.
pub const DEVICE_TABLE_BYTES: usize = 65_536 * 32;

const V: u64 = 1 << 0;
const TV: u64 = 1 << 1;
const IR: u64 = 1 << 61;
const IW: u64 = 1 << 62;

/// DTE that translates the function's DMA through the v1 page table at `root` (`levels` deep).
pub fn dte(root: u64, levels: u8, domain_id: u16) -> [u64; DTE_WORDS] {
    [
        V | TV | (u64::from(levels & 7) << 9) | (root & ADDR_MASK) | IR | IW,
        u64::from(domain_id),
        0,
        0,
    ]
}

/// Byte offset of the DTE of `device_id` in the device table.
pub fn dte_offset(device_id: u16) -> usize {
    usize::from(device_id).saturating_mul(32)
}
