//! ACPI MADT (multiple APIC description table) parser.
//!
//! Pure functions over a byte slice with strict bounds checks: malformed or
//! truncated tables yield an error or are cut short, never a panic or an
//! out-of-bounds read. Written to move unchanged into a host-testable, fuzzed
//! crate (gate item R9, phase S2.P4).

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

fn rd16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn rd64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
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
    if &table[0..4] != b"APIC" {
        return Err(MadtError::BadSignature);
    }
    let mut madt = Madt {
        lapic_address: u64::from(rd32(table, 36)),
        ..Madt::default()
    };
    let mut off = 44;
    while off + 2 <= table.len() {
        let (kind, len) = (table[off], usize::from(table[off + 1]));
        if len < 2 || off + len > table.len() {
            break; // truncated or malformed entry: stop, keep what we have
        }
        let e = &table[off..off + len];
        match kind {
            // Processor Local APIC: flags bit 0 = enabled, bit 1 = online-capable.
            0 if len >= 8 && rd32(e, 4) & 3 != 0 => madt.processors += 1,
            // I/O APIC.
            1 if len >= 12 => madt.ioapics.push(IoApicInfo {
                id: e[2],
                address: rd32(e, 4),
                gsi_base: rd32(e, 8),
            }),
            // Interrupt source override (ISA bus defaults: edge-triggered, active high).
            2 if len >= 10 => {
                let (polarity, trigger) =
                    decode_flags(rd16(e, 8), Polarity::ActiveHigh, Trigger::Edge);
                madt.overrides.push(SourceOverride {
                    source: e[3],
                    gsi: rd32(e, 4),
                    polarity,
                    trigger,
                });
            }
            // Local APIC NMI (NMI pins default to edge, active high).
            4 if len >= 6 => {
                let (polarity, trigger) =
                    decode_flags(rd16(e, 3), Polarity::ActiveHigh, Trigger::Edge);
                let uid = if e[2] == 0xFF {
                    u32::MAX
                } else {
                    u32::from(e[2])
                };
                madt.nmis.push(LapicNmi {
                    processor_uid: uid,
                    lint: e[5],
                    polarity,
                    trigger,
                });
            }
            // Local APIC address override.
            5 if len >= 12 => madt.lapic_address = rd64(e, 4),
            // Processor Local x2APIC.
            9 if len >= 16 && rd32(e, 8) & 3 != 0 => madt.processors += 1,
            // Local x2APIC NMI.
            0xA if len >= 12 => {
                let (polarity, trigger) =
                    decode_flags(rd16(e, 4), Polarity::ActiveHigh, Trigger::Edge);
                madt.nmis.push(LapicNmi {
                    processor_uid: rd32(e, 6),
                    lint: e[10],
                    polarity,
                    trigger,
                });
            }
            _ => {}
        }
        off += len;
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
