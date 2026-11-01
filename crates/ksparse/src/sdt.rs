//! ACPI system description tables: RSDP, table headers, FADT and the `\_S5` object.

use crate::bytes::{u16_at, u32_at, u64_at, u8_at};

/// Size of the common SDT header.
pub const HEADER_LEN: usize = 36;
/// Sanity bound on any table's declared length.
pub const MAX_TABLE_LEN: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdtError {
    TooShort,
    BadSignature,
    BadChecksum,
    BadLength,
}

/// True if the bytes sum to zero modulo 256 (the ACPI checksum rule).
pub fn checksum_ok(b: &[u8]) -> bool {
    b.iter().fold(0u8, |a, &x| a.wrapping_add(x)) == 0
}

/// Returns the length a table declares in its header, if it is plausible.
pub fn declared_length(header: &[u8]) -> Option<usize> {
    let len = u32_at(header, 4)? as usize;
    (HEADER_LEN..=MAX_TABLE_LEN).contains(&len).then_some(len)
}

/// Validates a complete table: plausible length that fits `bytes`, matching checksum.
/// Returns the table truncated to its declared length.
pub fn validate_table(bytes: &[u8]) -> Result<&[u8], SdtError> {
    if bytes.len() < HEADER_LEN {
        return Err(SdtError::TooShort);
    }
    let len = declared_length(bytes).ok_or(SdtError::BadLength)?;
    let table = bytes.get(..len).ok_or(SdtError::BadLength)?;
    if checksum_ok(table) {
        Ok(table)
    } else {
        Err(SdtError::BadChecksum)
    }
}

/// The root pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rsdp {
    pub revision: u8,
    pub rsdt: u32,
    pub xsdt: u64,
}

impl Rsdp {
    /// Parses and checksum-verifies an RSDP (ACPI 1.0 20-byte or 2.0+ 36-byte form).
    pub fn parse(b: &[u8]) -> Result<Self, SdtError> {
        if b.len() < 20 {
            return Err(SdtError::TooShort);
        }
        if &b[0..8] != b"RSD PTR " {
            return Err(SdtError::BadSignature);
        }
        if !checksum_ok(&b[0..20]) {
            return Err(SdtError::BadChecksum);
        }
        let revision = b[15];
        let rsdt = u32_at(b, 16).ok_or(SdtError::TooShort)?;
        let mut xsdt = 0;
        if revision >= 2 {
            let len = u32_at(b, 20).ok_or(SdtError::TooShort)? as usize;
            if !(36..=b.len()).contains(&len) {
                return Err(SdtError::BadLength);
            }
            if !checksum_ok(&b[..len]) {
                return Err(SdtError::BadChecksum);
            }
            xsdt = u64_at(b, 24).ok_or(SdtError::TooShort)?;
        }
        Ok(Self {
            revision,
            rsdt,
            xsdt,
        })
    }

    /// Physical address of the root table and whether its entries are 64-bit.
    pub fn root(&self) -> (u64, bool) {
        if self.revision >= 2 && self.xsdt != 0 {
            (self.xsdt, true)
        } else {
            (u64::from(self.rsdt), false)
        }
    }
}

/// Iterates the physical addresses listed by an RSDT (`wide = false`) or XSDT (`wide = true`).
pub fn root_entries(root: &[u8], wide: bool) -> impl Iterator<Item = u64> + '_ {
    let stride = if wide { 8 } else { 4 };
    root.get(HEADER_LEN..)
        .unwrap_or(&[])
        .chunks_exact(stride)
        .map(move |c| {
            if wide {
                u64_at(c, 0).unwrap_or(0)
            } else {
                u64::from(u32_at(c, 0).unwrap_or(0))
            }
        })
}

/// The parts of the FADT the kernel uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fadt {
    pub smi_cmd: u32,
    pub acpi_enable: u8,
    pub pm1a_cnt: u16,
    pub pm1b_cnt: u16,
    pub pm_tmr_blk: u16,
    /// True if the PM timer counter is 32 bits wide (otherwise 24).
    pub tmr_32bit: bool,
    pub dsdt: u64,
}

impl Fadt {
    /// Parses a validated FADT.
    pub fn parse(t: &[u8]) -> Result<Self, SdtError> {
        if t.len() < 116 {
            return Err(SdtError::TooShort);
        }
        if &t[0..4] != b"FACP" {
            return Err(SdtError::BadSignature);
        }
        // Generic Address Structure in system I/O space: address at +4.
        let gas_io = |off: usize| -> Option<u16> {
            (u8_at(t, off)? == 1)
                .then(|| u64_at(t, off + 4).unwrap_or(0) as u16)
                .filter(|&p| p != 0)
        };
        let x_dsdt = u64_at(t, 140).unwrap_or(0);
        Ok(Self {
            smi_cmd: u32_at(t, 48).ok_or(SdtError::TooShort)?,
            acpi_enable: u8_at(t, 52).ok_or(SdtError::TooShort)?,
            pm1a_cnt: gas_io(172).unwrap_or(u32_at(t, 64).unwrap_or(0) as u16),
            pm1b_cnt: gas_io(184).unwrap_or(u32_at(t, 68).unwrap_or(0) as u16),
            pm_tmr_blk: u32_at(t, 76).unwrap_or(0) as u16,
            tmr_32bit: u32_at(t, 112).unwrap_or(0) & (1 << 8) != 0,
            dsdt: if x_dsdt != 0 {
                x_dsdt
            } else {
                u64::from(u32_at(t, 40).ok_or(SdtError::TooShort)?)
            },
        })
    }
}

/// Decodes one AML integer constant (`Zero`, `One`, `BytePrefix`, `WordPrefix`) at `*i`.
fn aml_int(b: &[u8], i: &mut usize) -> Option<u16> {
    match *b.get(*i)? {
        0x00 => {
            *i += 1;
            Some(0)
        }
        0x01 => {
            *i += 1;
            Some(1)
        }
        0x0A => {
            let v = *b.get(*i + 1)?;
            *i += 2;
            Some(u16::from(v))
        }
        0x0B => {
            let v = u16_at(b, *i + 1)?;
            *i += 3;
            Some(v)
        }
        _ => None,
    }
}

/// Finds `Name(_S5_, Package(){ SLP_TYPa, SLP_TYPb, ... })` in a DSDT and returns the two values.
///
/// This is a bounded byte scan, not an AML interpreter (see ADR-0010/ADR-0023): the `\_S5`
/// object is a constant package on essentially all platforms.
pub fn find_s5(dsdt: &[u8]) -> Option<(u8, u8)> {
    let body = dsdt.get(HEADER_LEN..)?;
    let mut off = 0;
    while let Some(pos) = body.get(off..)?.windows(4).position(|w| w == b"_S5_") {
        let at = off + pos;
        off = at + 1;
        // Preceded by NameOp (0x08), optionally with a root prefix '\' (0x5C).
        let prev_ok = (at >= 1 && body[at - 1] == 0x08)
            || (at >= 2 && body[at - 1] == 0x5C && body[at - 2] == 0x08);
        if !prev_ok {
            continue;
        }
        let mut i = at + 4;
        if body.get(i) != Some(&0x12) {
            continue; // PackageOp
        }
        i += 1;
        // PkgLength: top two bits of the lead byte = number of extra length bytes.
        let Some(&lead) = body.get(i) else { continue };
        i += 1 + usize::from(lead >> 6);
        i += 1; // NumElements
        let (Some(a), Some(b)) = (aml_int(body, &mut i), aml_int(body, &mut i)) else {
            continue;
        };
        if a < 8 && b < 8 {
            return Some((a as u8, b as u8));
        }
    }
    None
}
