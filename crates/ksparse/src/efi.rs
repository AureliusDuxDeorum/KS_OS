// SPDX-License-Identifier: Apache-2.0
//! UEFI data structures handed over by firmware: memory-map descriptors, configuration
//! tables and the Memory Attributes Table (MAT), plus the W^X policy derived from them.
//!
//! All counts and sizes come from firmware and are range-checked with checked arithmetic.

use crate::bytes::{u32_at, u64_at};
use alloc::vec::Vec;

pub const TYPE_RUNTIME_CODE: u32 = 5;
pub const TYPE_RUNTIME_DATA: u32 = 6;
pub const TYPE_MMIO: u32 = 11;
pub const TYPE_MMIO_PORT_SPACE: u32 = 12;
pub const ATTR_XP: u64 = 1 << 14;
pub const ATTR_RO: u64 = 1 << 17;
pub const ATTR_RUNTIME: u64 = 1 << 63;

/// Minimum size of an `EFI_MEMORY_DESCRIPTOR` (type, pad, phys, virt, pages, attribute).
pub const MIN_DESCRIPTOR_SIZE: usize = 40;
/// Largest descriptor size we accept (the spec allows growth; real firmware uses 48).
pub const MAX_DESCRIPTOR_SIZE: usize = 4096;
/// Largest region we accept: 2^28 pages = 1 TiB.
pub const MAX_REGION_PAGES: u64 = 1 << 28;
/// Size of one `EFI_CONFIGURATION_TABLE` entry (16-byte GUID + 8-byte pointer).
pub const CONFIG_ENTRY_SIZE: usize = 24;
/// Largest number of configuration tables we accept.
pub const MAX_CONFIG_TABLES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryDescriptor {
    pub kind: u32,
    pub phys: u64,
    pub pages: u64,
    pub attr: u64,
}

impl MemoryDescriptor {
    /// Parses one descriptor from at least [`MIN_DESCRIPTOR_SIZE`] bytes.
    pub fn parse(b: &[u8]) -> Option<Self> {
        Some(Self {
            kind: u32_at(b, 0)?,
            phys: u64_at(b, 8)?,
            pages: u64_at(b, 24)?,
            attr: u64_at(b, 32)?,
        })
    }

    pub fn is_runtime(&self) -> bool {
        self.attr & ATTR_RUNTIME != 0
    }

    /// Page-aligned, non-empty, bounded, and `phys + pages * 4096` does not overflow.
    pub fn is_sane(&self) -> bool {
        self.phys.is_multiple_of(4096)
            && (1..=MAX_REGION_PAGES).contains(&self.pages)
            && self
                .pages
                .checked_mul(4096)
                .and_then(|len| self.phys.checked_add(len))
                .is_some()
    }

    /// Exclusive end address of a sane descriptor.
    pub fn end(&self) -> u64 {
        self.phys + self.pages * 4096
    }
}

/// Iterates a firmware memory map as `(raw descriptor bytes, parsed descriptor)`.
/// Yields nothing if `desc_size` is outside the accepted range.
pub fn descriptors(
    map: &[u8],
    desc_size: usize,
) -> impl Iterator<Item = (&[u8], MemoryDescriptor)> + '_ {
    let valid = (MIN_DESCRIPTOR_SIZE..=MAX_DESCRIPTOR_SIZE).contains(&desc_size);
    map.chunks_exact(if valid { desc_size } else { usize::MAX })
        .filter_map(|raw| MemoryDescriptor::parse(raw).map(|d| (raw, d)))
}

/// Number of bytes `n` configuration-table entries occupy, or `None` if `n` is implausible.
pub fn config_tables_len(n: usize) -> Option<usize> {
    (n <= MAX_CONFIG_TABLES).then(|| n * CONFIG_ENTRY_SIZE)
}

/// Looks up `guid` (16 raw bytes) in a configuration-table array and returns its pointer.
pub fn find_config_table(tables: &[u8], guid: &[u8; 16]) -> Option<u64> {
    tables
        .chunks_exact(CONFIG_ENTRY_SIZE)
        .find(|e| &e[..16] == guid)
        .and_then(|e| u64_at(e, 16))
}

/// GUID of the UEFI Memory Attributes Table (`dcfa911d-26eb-469f-a220-38b7dc461220`), as stored.
pub const MAT_GUID: [u8; 16] = [
    0x1d, 0x91, 0xfa, 0xdc, 0xeb, 0x26, 0x9f, 0x46, 0xa2, 0x20, 0x38, 0xb7, 0xdc, 0x46, 0x12, 0x20,
];

/// Header size of the MAT.
pub const MAT_HEADER_LEN: usize = 16;

/// Total byte length of a MAT given just its 16-byte header, or `None` if implausible.
pub fn mat_total_len(header: &[u8]) -> Option<usize> {
    let (num, size) = (u32_at(header, 4)? as usize, u32_at(header, 8)? as usize);
    if !(MIN_DESCRIPTOR_SIZE..=MAX_DESCRIPTOR_SIZE).contains(&size) || num > 4096 {
        return None;
    }
    MAT_HEADER_LEN.checked_add(num.checked_mul(size)?)
}

/// Parses a complete MAT into sane descriptors (insane entries are skipped).
pub fn parse_mat(table: &[u8]) -> Option<Vec<MemoryDescriptor>> {
    let total = mat_total_len(table)?;
    let size = u32_at(table, 8)? as usize;
    let body = table.get(MAT_HEADER_LEN..total)?;
    Some(
        body.chunks_exact(size)
            .filter_map(MemoryDescriptor::parse)
            .filter(MemoryDescriptor::is_sane)
            .collect(),
    )
}

/// Page permissions `(writable, executable)` for a MAT section.
///
/// W^X is never relaxed: RO+XP sections are as stated; a section the firmware leaves
/// unrestricted falls back to its region type's default (code stays executable, everything
/// else stays non-executable). The result is never both writable and executable.
pub fn section_permissions(kind: u32, attr: u64) -> (bool, bool) {
    match (attr & ATTR_RO != 0, attr & ATTR_XP != 0) {
        (true, false) => (false, true),
        (false, true) => (true, false),
        (true, true) => (false, false),
        (false, false) => (kind != TYPE_RUNTIME_CODE, kind == TYPE_RUNTIME_CODE),
    }
}
