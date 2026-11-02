// SPDX-License-Identifier: Apache-2.0
//! ACPI MADT (multiple APIC description table) parser.
//!
//! Pure functions over a byte slice with strict bounds checks: malformed or
//! truncated tables yield an error or are cut short, never a panic or an
//! out-of-bounds read. Host-tested and fuzzed (gate item R9).

use crate::bytes::{u16_at, u32_at, u64_at, u8_at};
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoApicInfo {
    pub id: u8,
    pub address: u32,
    pub gsi_base: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    ActiveHigh,
    ActiveLow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Edge,
    Level,
}

/// Interrupt source override: ISA IRQ `source` is wired to global system interrupt `gsi`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceOverride {
    pub source: u8,
    pub gsi: u32,
    pub polarity: Polarity,
    pub trigger: Trigger,
}

/// A non-maskable interrupt wired to a local APIC LINT pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LapicNmi {
    /// ACPI processor UID this applies to; `0xFFFF_FFFF` means all processors.
    pub processor_uid: u32,
    pub lint: u8,
    pub polarity: Polarity,
    pub trigger: Trigger,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Madt {
    pub lapic_address: u64,
    pub ioapics: Vec<IoApicInfo>,
    pub overrides: Vec<SourceOverride>,
    pub nmis: Vec<LapicNmi>,
    pub processors: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MadtError {
    TooShort,
    BadSignature,
}

/// Decodes MPS INTI flags (polarity bits 0-1, trigger bits 2-3). `conform` gives the bus default.
fn decode_flags(flags: u16, conform_pol: Polarity, conform_trig: Trigger) -> (Polarity, Trigger) {
    let pol = match flags & 3 {
        1 => Polarity::ActiveHigh,
        3 => Polarity::ActiveLow,
        _ => conform_pol,
    };
    let trig = match (flags >> 2) & 3 {
        1 => Trigger::Edge,
        3 => Trigger::Level,
        _ => conform_trig,
    };
    (pol, trig)
}

pub fn parse(table: &[u8]) -> Result<Madt, MadtError> {
    if table.len() < 44 {
        return Err(MadtError::TooShort);
    }
    if table.get(0..4) != Some(&b"APIC"[..]) {
        return Err(MadtError::BadSignature);
    }
    let mut madt = Madt {
        lapic_address: u64::from(u32_at(table, 36).unwrap_or(0)),
        ..Madt::default()
    };
    let mut off = 44;
    while let (Some(kind), Some(len)) = (u8_at(table, off), u8_at(table, off.saturating_add(1))) {
        let len = usize::from(len);
        // Truncated or malformed entry: stop, keep what we have.
        let Some(e) = table.get(off..off.saturating_add(len)).filter(|_| len >= 2) else {
            break;
        };
        match kind {
            // Processor Local APIC: flags bit 0 = enabled, bit 1 = online-capable.
            0 if len >= 8 && u32_at(e, 4).unwrap_or(0) & 3 != 0 => {
                madt.processors = madt.processors.saturating_add(1)
            }
            // I/O APIC.
            1 if len >= 12 => madt.ioapics.push(IoApicInfo {
                id: u8_at(e, 2).unwrap_or(0),
                address: u32_at(e, 4).unwrap_or(0),
                gsi_base: u32_at(e, 8).unwrap_or(0),
            }),
            // Interrupt source override (ISA bus defaults: edge-triggered, active high).
            2 if len >= 10 => {
                let (polarity, trigger) = decode_flags(
                    u16_at(e, 8).unwrap_or(0),
                    Polarity::ActiveHigh,
                    Trigger::Edge,
                );
                madt.overrides.push(SourceOverride {
                    source: u8_at(e, 3).unwrap_or(0),
                    gsi: u32_at(e, 4).unwrap_or(0),
                    polarity,
                    trigger,
                });
            }
            // Local APIC NMI (NMI pins default to edge, active high).
            4 if len >= 6 => {
                let (polarity, trigger) = decode_flags(
                    u16_at(e, 3).unwrap_or(0),
                    Polarity::ActiveHigh,
                    Trigger::Edge,
                );
                let uid = match u8_at(e, 2).unwrap_or(0) {
                    0xFF => u32::MAX,
                    n => u32::from(n),
                };
                madt.nmis.push(LapicNmi {
                    processor_uid: uid,
                    lint: u8_at(e, 5).unwrap_or(0),
                    polarity,
                    trigger,
                });
            }
            // Local APIC address override.
            5 if len >= 12 => madt.lapic_address = u64_at(e, 4).unwrap_or(0),
            // Processor Local x2APIC.
            9 if len >= 16 && u32_at(e, 8).unwrap_or(0) & 3 != 0 => {
                madt.processors = madt.processors.saturating_add(1)
            }
            // Local x2APIC NMI.
            0xA if len >= 12 => {
                let (polarity, trigger) = decode_flags(
                    u16_at(e, 4).unwrap_or(0),
                    Polarity::ActiveHigh,
                    Trigger::Edge,
                );
                madt.nmis.push(LapicNmi {
                    processor_uid: u32_at(e, 6).unwrap_or(0),
                    lint: u8_at(e, 10).unwrap_or(0),
                    polarity,
                    trigger,
                });
            }
            _ => {}
        }
        off = off.saturating_add(len);
    }
    Ok(madt)
}

impl Madt {
    /// Resolves an ISA IRQ to its GSI, polarity and trigger mode.
    pub fn isa_irq(&self, irq: u8) -> (u32, Polarity, Trigger) {
        self.overrides
            .iter()
            .find(|o| o.source == irq)
            .map_or((u32::from(irq), Polarity::ActiveHigh, Trigger::Edge), |o| {
                (o.gsi, o.polarity, o.trigger)
            })
    }
}
