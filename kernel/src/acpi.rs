//! Minimal ACPI support: locate the FADT and perform an S5 (soft-off) transition.
//!
//! Full AML interpretation is a later-stage concern. The `\_S5` object is a
//! constant package on essentially all platforms, so we extract the two
//! `SLP_TYP` values by scanning the DSDT for it — the same shortcut used by
//! early bring-up code in other kernels, guarded with strict bounds/shape checks.

use crate::arch::{inb, inw, outb, outw};
use crate::boot::BootInfo;

#[derive(Debug)]
#[allow(dead_code)]
pub enum AcpiError {
    NoRsdp,
    BadChecksum(&'static str),
    NotMapped(&'static str),
    NoFadt,
    NoS5,
    NoPm1Control,
    Hung,
}

#[derive(Clone, Copy)]
pub struct Acpi {
    pm1a_cnt: u16,
    pm1b_cnt: u16,
    slp_typ_a: u16,
    slp_typ_b: u16,
    smi_cmd: u16,
    acpi_enable: u8,
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

fn checksum_ok(b: &[u8]) -> bool {
    b.iter().fold(0u8, |a, &x| a.wrapping_add(x)) == 0
}

fn slice(
    boot: &BootInfo,
    phys: u64,
    len: u64,
    what: &'static str,
) -> Result<&'static [u8], AcpiError> {
    if !boot.hhdm_covers(phys, len) {
        return Err(AcpiError::NotMapped(what));
    }
    // SAFETY: HHDM-mapped range (checked above); ACPI tables are read-only to us.
    Ok(unsafe { core::slice::from_raw_parts(boot.phys_to_virt(phys) as *const u8, len as usize) })
}

/// Returns a whole SDT (header-validated, checksum-verified) at `phys`.
fn table(boot: &BootInfo, phys: u64, what: &'static str) -> Result<&'static [u8], AcpiError> {
    let hdr = slice(boot, phys, 36, what)?;
    let len = u64::from(rd32(hdr, 4));
    if !(36..=0x100_0000).contains(&len) {
        return Err(AcpiError::BadChecksum(what));
    }
    let t = slice(boot, phys, len, what)?;
    if !checksum_ok(t) {
        return Err(AcpiError::BadChecksum(what));
    }
    Ok(t)
}

/// Decodes one AML integer constant from a `_S5` package element.
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
            let v = rd16(b.get(*i + 1..*i + 3)?, 0);
            *i += 3;
            Some(v)
        }
        _ => None,
    }
}

/// Finds `Name(_S5_, Package(){ SLP_TYPa, SLP_TYPb, ... })` in the DSDT.
fn find_s5(dsdt: &[u8]) -> Option<(u16, u16)> {
    let body = dsdt.get(36..)?;
    let mut off = 0;
    while let Some(pos) = body[off..].windows(4).position(|w| w == b"_S5_") {
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
        let lead = *body.get(i)?;
        i += 1 + usize::from(lead >> 6);
        let _num_elements = *body.get(i)?;
        i += 1;
        let a = aml_int(body, &mut i)?;
        let b = aml_int(body, &mut i)?;
        if a < 8 && b < 8 {
            return Some((a, b));
        }
    }
    None
}

impl Acpi {
    /// Parses the ACPI tables and prepares an S5 transition.
    pub fn init(boot: &BootInfo) -> Result<Self, AcpiError> {
        let rsdp_virt = boot.rsdp.ok_or(AcpiError::NoRsdp)?;
        let rsdp_phys = rsdp_virt.checked_sub(boot.hhdm).ok_or(AcpiError::NoRsdp)?;
        let rsdp = slice(boot, rsdp_phys, 36, "RSDP")?;
        if &rsdp[0..8] != b"RSD PTR " || !checksum_ok(&rsdp[0..20]) {
            return Err(AcpiError::BadChecksum("RSDP"));
        }
        let revision = rsdp[15];
        let (root_phys, wide) = if revision >= 2 && rd64(rsdp, 24) != 0 {
            (rd64(rsdp, 24), true)
        } else {
            (u64::from(rd32(rsdp, 16)), false)
        };
        let root = table(boot, root_phys, if wide { "XSDT" } else { "RSDT" })?;
        let stride = if wide { 8 } else { 4 };

        let mut fadt = None;
        for entry in root[36..].chunks_exact(stride) {
            let phys = if wide {
                rd64(entry, 0)
            } else {
                u64::from(rd32(entry, 0))
            };
            if let Ok(hdr) = slice(boot, phys, 8, "SDT header") {
                if &hdr[0..4] == b"FACP" {
                    fadt = Some(table(boot, phys, "FADT")?);
                    break;
                }
            }
        }
        let fadt = fadt.ok_or(AcpiError::NoFadt)?;
        if fadt.len() < 116 {
            return Err(AcpiError::NoFadt);
        }

        let dsdt_phys = if fadt.len() >= 148 && rd64(fadt, 140) != 0 {
            rd64(fadt, 140)
        } else {
            u64::from(rd32(fadt, 40))
        };
        let dsdt = table(boot, dsdt_phys, "DSDT")?;
        let (slp_typ_a, slp_typ_b) = find_s5(dsdt).ok_or(AcpiError::NoS5)?;

        let gas_io = |off: usize| -> Option<u16> {
            // Generic Address Structure: space id (1 = system I/O), ..., 64-bit address at +4.
            (fadt.len() >= off + 12 && fadt[off] == 1)
                .then(|| rd64(fadt, off + 4) as u16)
                .filter(|&p| p != 0)
        };
        let pm1a = gas_io(172).unwrap_or(rd32(fadt, 64) as u16);
        let pm1b = gas_io(184).unwrap_or(rd32(fadt, 68) as u16);
        if pm1a == 0 {
            return Err(AcpiError::NoPm1Control);
        }
        kprintln!("acpi: PM1a_CNT={pm1a:#x} PM1b_CNT={pm1b:#x} SLP_TYPa={slp_typ_a} SLP_TYPb={slp_typ_b} SMI_CMD={:#x} ENABLE={:#x}", rd32(fadt, 48), fadt[52]);
        Ok(Self {
            pm1a_cnt: pm1a,
            pm1b_cnt: pm1b,
            slp_typ_a,
            slp_typ_b,
            smi_cmd: rd32(fadt, 48) as u16,
            acpi_enable: fadt[52],
        })
    }

    /// Enters S5. Returns only if the platform did not power off.
    pub fn power_off(&self) -> AcpiError {
        const SCI_EN: u16 = 1;
        const SLP_EN: u16 = 1 << 13;
        crate::arch::cli();
        // SAFETY: PM1 control block ports come from the validated FADT.
        unsafe {
            // Hand control of the power-management hardware to the OS if firmware still owns it.
            if inw(self.pm1a_cnt) & SCI_EN == 0 && self.smi_cmd != 0 && self.acpi_enable != 0 {
                outb(self.smi_cmd, self.acpi_enable);
                for _ in 0..3_000_000 {
                    if inw(self.pm1a_cnt) & SCI_EN != 0 {
                        break;
                    }
                    core::hint::spin_loop();
                }
            }
            outw(self.pm1a_cnt, (self.slp_typ_a << 10) | SLP_EN);
            if self.pm1b_cnt != 0 {
                outw(self.pm1b_cnt, (self.slp_typ_b << 10) | SLP_EN);
            }
            for _ in 0..200_000u32 {
                let _ = inb(0x80);
                core::hint::spin_loop();
            }
        }
        AcpiError::Hung
    }
}
