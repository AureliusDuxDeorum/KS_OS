// SPDX-License-Identifier: Apache-2.0
//! ACPI tables that describe IOMMUs: Intel `DMAR` (VT-d) and AMD `IVRS` (AMD-Vi).
//!
//! The firmware-supplied structures are untrusted: every length is bounded and every walk is
//! bounds-checked (see `harness::exercise`, which fuzzes both parsers).

use crate::bytes::{slice_at, u16_at, u32_at, u64_at, u8_at};
use alloc::vec::Vec;

const MAX_UNITS: usize = 64;
const MAX_SCOPES: usize = 64;
const MAX_REGIONS: usize = 64;
const MAX_ENTRIES: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub enum IommuTableError {
    TooShort,
    BadSignature,
}

// ---------------------------------------------------------------- Intel DMAR

/// A PCI device or bridge named by a DMAR device scope (path of (device, function) pairs from `bus`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    /// 1 = PCI endpoint, 2 = PCI sub-hierarchy, 3 = IOAPIC, 4 = HPET, 5 = ACPI namespace device.
    pub kind: u8,
    pub enumeration_id: u8,
    pub start_bus: u8,
    pub path: Vec<(u8, u8)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drhd {
    pub segment: u16,
    pub register_base: u64,
    /// Covers every PCI device of the segment not claimed by another unit.
    pub include_all: bool,
    pub scopes: Vec<Scope>,
}

/// Reserved memory region that some device keeps using (legacy USB, integrated graphics): must stay mapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rmrr {
    pub segment: u16,
    pub base: u64,
    pub limit: u64,
    pub scopes: Vec<Scope>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Dmar {
    pub host_address_width: u8,
    pub flags: u8,
    pub units: Vec<Drhd>,
    pub rmrr: Vec<Rmrr>,
}

fn scopes(b: &[u8]) -> Vec<Scope> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while out.len() < MAX_SCOPES {
        let (Some(kind), Some(len)) = (u8_at(b, off), u8_at(b, off.saturating_add(1))) else {
            break;
        };
        let len = usize::from(len);
        let Some(s) = slice_at(b, off, len).filter(|_| len >= 8) else {
            break;
        };
        let mut path = Vec::new();
        let mut p = 6usize;
        while let (Some(d), Some(f)) = (u8_at(s, p), u8_at(s, p.saturating_add(1))) {
            if path.len() >= 8 {
                break;
            }
            path.push((d, f));
            p = p.saturating_add(2);
        }
        out.push(Scope {
            kind,
            enumeration_id: u8_at(s, 4).unwrap_or(0),
            start_bus: u8_at(s, 5).unwrap_or(0),
            path,
        });
        off = off.saturating_add(len);
    }
    out
}

pub fn parse_dmar(table: &[u8]) -> Result<Dmar, IommuTableError> {
    if table.len() < 48 {
        return Err(IommuTableError::TooShort);
    }
    if table.get(0..4) != Some(&b"DMAR"[..]) {
        return Err(IommuTableError::BadSignature);
    }
    let mut dmar = Dmar {
        host_address_width: u8_at(table, 36).unwrap_or(0).saturating_add(1),
        flags: u8_at(table, 37).unwrap_or(0),
        ..Dmar::default()
    };
    let mut off = 48usize;
    for _ in 0..MAX_ENTRIES {
        let (Some(kind), Some(len)) = (u16_at(table, off), u16_at(table, off.saturating_add(2)))
        else {
            break;
        };
        let len = usize::from(len);
        let Some(e) = slice_at(table, off, len).filter(|_| len >= 8) else {
            break;
        };
        match kind {
            0 if len >= 16 && dmar.units.len() < MAX_UNITS => dmar.units.push(Drhd {
                segment: u16_at(e, 6).unwrap_or(0),
                register_base: u64_at(e, 8).unwrap_or(0),
                include_all: u8_at(e, 4).unwrap_or(0) & 1 != 0,
                scopes: scopes(crate::bytes::tail(e, 16)),
            }),
            1 if len >= 24 && dmar.rmrr.len() < MAX_REGIONS => dmar.rmrr.push(Rmrr {
                segment: u16_at(e, 6).unwrap_or(0),
                base: u64_at(e, 8).unwrap_or(0),
                limit: u64_at(e, 16).unwrap_or(0),
                scopes: scopes(crate::bytes::tail(e, 24)),
            }),
            _ => {}
        }
        off = off.saturating_add(len);
    }
    Ok(dmar)
}

// ---------------------------------------------------------------- AMD IVRS

/// One AMD IOMMU (an IVHD block).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ivhd {
    pub block_type: u8,
    pub flags: u8,
    pub segment: u16,
    /// PCI device id (bus/devfn) of the IOMMU itself.
    pub device_id: u16,
    pub register_base: u64,
    /// Device-id ranges `[start, end]` this IOMMU translates (inclusive).
    pub ranges: Vec<(u16, u16)>,
}

/// Unity-mapped region (IVMD): the device range must keep identity access to `[start, start+length)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ivmd {
    pub block_type: u8,
    pub device_start: u16,
    pub device_end: u16,
    pub start: u64,
    pub length: u64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Ivrs {
    pub iv_info: u32,
    pub units: Vec<Ivhd>,
    pub unity: Vec<Ivmd>,
}

fn ivhd_entries(e: &[u8], start: usize) -> Vec<(u16, u16)> {
    let mut ranges = Vec::new();
    let mut off = start;
    let mut open: Option<u16> = None;
    while let Some(kind) = u8_at(e, off) {
        let devid = u16_at(e, off.saturating_add(1)).unwrap_or(0);
        let size = match kind {
            0x00..=0x3F => 4usize,
            0x40..=0x7F => 8,
            _ => break, // variable-length ACPI HID entries: stop, what we have is enough
        };
        if ranges.len() >= MAX_ENTRIES {
            break;
        }
        match kind {
            0x01 => ranges.push((0, 0xFFFF)),           // all devices
            0x02 | 0x42 => ranges.push((devid, devid)), // select (and alias)
            0x03 | 0x43 => open = Some(devid),          // start of range
            0x04 => {
                if let Some(start) = open.take() {
                    if start <= devid {
                        ranges.push((start, devid));
                    }
                }
            }
            _ => {}
        }
        off = off.saturating_add(size);
    }
    ranges
}

pub fn parse_ivrs(table: &[u8]) -> Result<Ivrs, IommuTableError> {
    if table.len() < 48 {
        return Err(IommuTableError::TooShort);
    }
    if table.get(0..4) != Some(&b"IVRS"[..]) {
        return Err(IommuTableError::BadSignature);
    }
    let mut ivrs = Ivrs {
        iv_info: u32_at(table, 36).unwrap_or(0),
        ..Ivrs::default()
    };
    let mut off = 48usize;
    for _ in 0..MAX_ENTRIES {
        let (Some(kind), Some(flags), Some(len)) = (
            u8_at(table, off),
            u8_at(table, off.saturating_add(1)),
            u16_at(table, off.saturating_add(2)),
        ) else {
            break;
        };
        let len = usize::from(len);
        let Some(e) = slice_at(table, off, len).filter(|_| len >= 8) else {
            break;
        };
        match kind {
            0x10 | 0x11 | 0x40 if len >= 24 && ivrs.units.len() < MAX_UNITS => {
                let entries = if kind == 0x10 { 24 } else { 40 };
                ivrs.units.push(Ivhd {
                    block_type: kind,
                    flags,
                    device_id: u16_at(e, 4).unwrap_or(0),
                    segment: u16_at(e, 16).unwrap_or(0),
                    register_base: u64_at(e, 8).unwrap_or(0),
                    ranges: ivhd_entries(e, entries),
                });
            }
            0x20..=0x22 if len >= 32 && ivrs.unity.len() < MAX_REGIONS => {
                let devid = u16_at(e, 4).unwrap_or(0);
                let aux = u16_at(e, 6).unwrap_or(0);
                let (lo, hi) = match kind {
                    0x20 => (0, 0xFFFF),
                    0x21 => (devid, devid),
                    _ => (devid, aux),
                };
                ivrs.unity.push(Ivmd {
                    block_type: kind,
                    device_start: lo,
                    device_end: hi,
                    start: u64_at(e, 16).unwrap_or(0),
                    length: u64_at(e, 24).unwrap_or(0),
                });
            }
            _ => {}
        }
        off = off.saturating_add(len);
    }
    Ok(ivrs)
}
