// SPDX-License-Identifier: Apache-2.0
//! HID report-descriptor parsing and input-report interpretation (HID 1.11).
//!
//! The descriptor is device-supplied and may be arbitrarily hostile, so every walk is
//! bounded: item count, stack depth, collection depth, usage lists and field count all have
//! hard limits, and bit extraction is range-checked.

use crate::bytes::u8_at;
use alloc::vec::Vec;

pub const PAGE_GENERIC_DESKTOP: u16 = 0x01;
pub const PAGE_KEYBOARD: u16 = 0x07;
pub const PAGE_BUTTON: u16 = 0x09;
pub const PAGE_CONSUMER: u16 = 0x0C;

pub const USAGE_X: u16 = 0x30;
pub const USAGE_Y: u16 = 0x31;
pub const USAGE_WHEEL: u16 = 0x38;

const MAX_ITEMS: usize = 8192;
const MAX_FIELDS: usize = 512;
const MAX_STACK: usize = 8;
const MAX_COLLECTION_DEPTH: usize = 32;
const MAX_USAGES: usize = 256;
const MAX_REPORT_BITS: u32 = 8 * 1024;

/// One Input item: `count` values of `bit_size` bits each, starting at `bit_offset`
/// (after the report-ID byte, if any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub report_id: u8,
    pub bit_offset: u32,
    pub bit_size: u8,
    pub count: u16,
    pub usage_page: u16,
    /// Usages assigned to the values (explicit list, or expanded min..=max); the last one is
    /// reused for any remaining values.
    pub usages: Vec<u16>,
    pub logical_min: i32,
    pub logical_max: i32,
    pub constant: bool,
    pub variable: bool,
    pub relative: bool,
}

impl Field {
    /// Usage of the `i`-th value.
    pub fn usage_at(&self, i: usize) -> Option<u16> {
        self.usages.get(i).or(self.usages.last()).copied()
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReportDescriptor {
    pub fields: Vec<Field>,
    pub uses_report_ids: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct Globals {
    usage_page: u16,
    logical_min: i32,
    logical_max: i32,
    report_size: u32,
    report_count: u32,
    report_id: u8,
}

fn item_value(b: &[u8], size: usize, signed: bool) -> i32 {
    let mut v: u32 = 0;
    for (i, &byte) in b.iter().take(size).enumerate() {
        v |= u32::from(byte) << (8 * i);
    }
    if signed && size > 0 && size < 4 && v >> (8 * size - 1) & 1 == 1 {
        v |= u32::MAX << (8 * size);
    }
    v as i32
}

/// Parses a report descriptor. Returns whatever fields could be extracted; malformed input
/// ends parsing early rather than failing.
pub fn parse(desc: &[u8]) -> ReportDescriptor {
    let mut out = ReportDescriptor::default();
    let mut g = Globals::default();
    let mut stack: Vec<Globals> = Vec::new();
    let mut usages: Vec<u16> = Vec::new();
    let (mut usage_min, mut usage_max) = (None::<u16>, None::<u16>);
    let mut depth = 0usize;
    // Running bit offset per report ID (index = report id).
    let mut offsets = alloc::vec![0u32; 256];
    let mut i = 0;
    for _ in 0..MAX_ITEMS {
        let Some(prefix) = u8_at(desc, i) else { break };
        if prefix == 0xFE {
            // Long item: skip its data.
            let len = usize::from(u8_at(desc, i + 1).unwrap_or(0));
            i += 3 + len;
            continue;
        }
        let size = match prefix & 3 {
            3 => 4,
            n => usize::from(n),
        };
        let (kind, tag) = ((prefix >> 2) & 3, prefix >> 4);
        let Some(data) = desc.get(i + 1..i + 1 + size) else {
            break;
        };
        i += 1 + size;
        let unsigned = item_value(data, size, false);
        let signed = item_value(data, size, true);
        match (kind, tag) {
            // Global items.
            (1, 0x0) => g.usage_page = unsigned as u16,
            (1, 0x1) => g.logical_min = signed,
            (1, 0x2) => {
                g.logical_max = if size == 4 {
                    signed
                } else {
                    item_value(data, size, g.logical_min < 0)
                }
            }
            (1, 0x7) => g.report_size = unsigned as u32,
            (1, 0x8) => {
                g.report_id = unsigned as u8;
                out.uses_report_ids = true;
            }
            (1, 0x9) => g.report_count = unsigned as u32,
            (1, 0xA) => {
                if stack.len() < MAX_STACK {
                    stack.push(g);
                }
            }
            (1, 0xB) => {
                if let Some(p) = stack.pop() {
                    g = p;
                }
            }
            // Local items.
            (2, 0x0) => {
                if usages.len() < MAX_USAGES {
                    usages.push(unsigned as u16);
                }
            }
            (2, 0x1) => usage_min = Some(unsigned as u16),
            (2, 0x2) => usage_max = Some(unsigned as u16),
            // Main items.
            (0, 0xA) => {
                depth = (depth + 1).min(MAX_COLLECTION_DEPTH);
                usages.clear();
                (usage_min, usage_max) = (None, None);
            }
            (0, 0xC) => {
                depth = depth.saturating_sub(1);
                usages.clear();
                (usage_min, usage_max) = (None, None);
            }
            (0, 0x8..=0xB) => {
                let bits = g.report_size.checked_mul(g.report_count);
                let slot = usize::from(g.report_id);
                if let Some(bits) = bits.filter(|&b| b <= MAX_REPORT_BITS && g.report_size <= 32) {
                    if tag == 0x8 && out.fields.len() < MAX_FIELDS {
                        // Input item.
                        if let (Some(lo), Some(hi)) = (usage_min, usage_max) {
                            if hi >= lo {
                                let n = (usize::from(hi - lo) + 1).min(MAX_USAGES);
                                usages.extend((0..n).map(|k| lo + k as u16));
                                usages.truncate(MAX_USAGES);
                            }
                        }
                        out.fields.push(Field {
                            report_id: g.report_id,
                            bit_offset: offsets[slot],
                            bit_size: g.report_size as u8,
                            count: g.report_count.min(u32::from(u16::MAX)) as u16,
                            usage_page: g.usage_page,
                            usages: usages.clone(),
                            logical_min: g.logical_min,
                            logical_max: g.logical_max,
                            constant: unsigned & 1 != 0,
                            variable: unsigned & 2 != 0,
                            relative: unsigned & 4 != 0,
                        });
                    }
                    if tag == 0x8 {
                        offsets[slot] = offsets[slot].saturating_add(bits).min(MAX_REPORT_BITS);
                    }
                }
                usages.clear();
                (usage_min, usage_max) = (None, None);
            }
            _ => {}
        }
    }
    out
}

/// Extracts a `size`-bit little-endian field at `bit_offset` from `report`.
pub fn extract(report: &[u8], bit_offset: u32, size: u8, signed: bool) -> Option<i32> {
    if size == 0 || size > 32 {
        return None;
    }
    let end = bit_offset.checked_add(u32::from(size))?;
    if end.div_ceil(8) as usize > report.len() {
        return None;
    }
    let mut v: u64 = 0;
    for k in 0..u32::from(size) {
        let bit = bit_offset + k;
        let byte = report[(bit / 8) as usize];
        v |= u64::from(byte >> (bit % 8) & 1) << k;
    }
    let mut v = v as u32;
    if signed && size < 32 && v >> (size - 1) & 1 == 1 {
        v |= u32::MAX << size;
    }
    Some(v as i32)
}

/// Decoded input from one report.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Input {
    /// Bitmask of pressed modifier keys (usages 0xE0..=0xE7: LCtrl, LShift, LAlt, LGui, RCtrl, ...).
    pub modifiers: u8,
    /// Pressed keyboard usages (up to 16 tracked).
    pub keys: Vec<u8>,
    pub dx: i32,
    pub dy: i32,
    pub wheel: i32,
    /// Bitmask of pressed buttons (button 1 = bit 0).
    pub buttons: u32,
}

/// Interprets one input report (with the leading report-ID byte if the descriptor uses IDs).
pub fn interpret(desc: &ReportDescriptor, report: &[u8]) -> Input {
    let (id, data) = if desc.uses_report_ids {
        match report.split_first() {
            Some((&id, rest)) => (id, rest),
            None => return Input::default(),
        }
    } else {
        (0, report)
    };
    let mut out = Input::default();
    for f in desc
        .fields
        .iter()
        .filter(|f| f.report_id == id && !f.constant)
    {
        for n in 0..usize::from(f.count) {
            let off = f
                .bit_offset
                .saturating_add((n as u32).saturating_mul(u32::from(f.bit_size)));
            let signed = f.logical_min < 0;
            let Some(v) = extract(data, off, f.bit_size, signed) else {
                break;
            };
            let Some(usage) = f.usage_at(n) else { continue };
            match (f.usage_page, f.variable) {
                (PAGE_KEYBOARD, true) => {
                    if v != 0 && (0xE0..=0xE7).contains(&usage) {
                        out.modifiers |= 1 << (usage - 0xE0);
                    } else if v != 0 && out.keys.len() < 16 {
                        out.keys.push(usage as u8);
                    }
                }
                (PAGE_KEYBOARD, false) => {
                    if v > 1 && out.keys.len() < 16 {
                        out.keys.push(v as u8); // array item: the value is the usage
                    }
                }
                (PAGE_BUTTON, true) => {
                    if v != 0 && (1..=32).contains(&usage) {
                        out.buttons |= 1 << (usage - 1);
                    }
                }
                (PAGE_GENERIC_DESKTOP, true) => match usage {
                    USAGE_X => out.dx = out.dx.saturating_add(v),
                    USAGE_Y => out.dy = out.dy.saturating_add(v),
                    USAGE_WHEEL => out.wheel = out.wheel.saturating_add(v),
                    _ => {}
                },
                _ => {}
            }
        }
    }
    out
}

/// Total length in bytes of the input report with `report_id` (excluding the ID byte).
pub fn input_report_len(desc: &ReportDescriptor, report_id: u8) -> usize {
    desc.fields
        .iter()
        .filter(|f| f.report_id == report_id)
        .map(|f| (f.bit_offset + u32::from(f.bit_size) * u32::from(f.count)).div_ceil(8) as usize)
        .max()
        .unwrap_or(0)
}

/// True if the descriptor describes a keyboard (keyboard-page input fields).
pub fn is_keyboard(desc: &ReportDescriptor) -> bool {
    desc.fields
        .iter()
        .any(|f| f.usage_page == PAGE_KEYBOARD && !f.constant)
}

/// True if the descriptor describes a pointer (relative or absolute X/Y).
pub fn is_pointer(desc: &ReportDescriptor) -> bool {
    desc.fields
        .iter()
        .any(|f| f.usage_page == PAGE_GENERIC_DESKTOP && f.variable && f.usages.contains(&USAGE_X))
}
