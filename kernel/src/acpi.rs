// SPDX-License-Identifier: Apache-2.0
//! ACPI: table discovery, PM timer and the S5 (soft-off) transition.
//!
//! The kernel only does the physical-memory access here; all parsing of the
//! untrusted tables lives in the host-tested, fuzzed `ksparse` crate. Full AML
//! interpretation is deliberately absent (ADR-0023): the `\_S5` object is
//! extracted by a bounded scan and nothing else in the kernel needs AML yet.

use crate::arch::{inb, inl, inw, outb, outw};
use crate::boot::BootInfo;
use ksparse::sdt::{self, Fadt, Rsdp, SdtError};

#[derive(Debug)]
#[allow(dead_code)]
pub enum AcpiError {
    NoRsdp,
    Invalid(&'static str, SdtError),
    NotMapped(&'static str),
    NoFadt,
    NoTable,
    NoS5,
    NoPm1Control,
    NoPmTimer,
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

/// Maps and validates one whole table at `phys`.
fn table(boot: &BootInfo, phys: u64, what: &'static str) -> Result<&'static [u8], AcpiError> {
    let header = slice(boot, phys, sdt::HEADER_LEN as u64, what)?;
    let len = sdt::declared_length(header).ok_or(AcpiError::Invalid(what, SdtError::BadLength))?;
    sdt::validate_table(slice(boot, phys, len as u64, what)?)
        .map_err(|e| AcpiError::Invalid(what, e))
}

/// Locates an ACPI system description table by signature (checksum-verified).
pub fn find_table(boot: &BootInfo, sig: &[u8; 4]) -> Result<&'static [u8], AcpiError> {
    let rsdp_virt = boot.rsdp.ok_or(AcpiError::NoRsdp)?;
    let rsdp_phys = rsdp_virt.checked_sub(boot.hhdm).ok_or(AcpiError::NoRsdp)?;
    let rsdp = Rsdp::parse(slice(boot, rsdp_phys, 36, "RSDP")?)
        .map_err(|e| AcpiError::Invalid("RSDP", e))?;
    let (root_phys, wide) = rsdp.root();
    let root = table(boot, root_phys, if wide { "XSDT" } else { "RSDT" })?;
    for phys in sdt::root_entries(root, wide) {
        if let Ok(hdr) = slice(boot, phys, 8, "SDT header") {
            if &hdr[0..4] == sig {
                return table(boot, phys, "SDT");
            }
        }
    }
    Err(AcpiError::NoTable)
}

fn fadt(boot: &BootInfo) -> Result<Fadt, AcpiError> {
    let t = find_table(boot, b"FACP").map_err(|e| match e {
        AcpiError::NoTable => AcpiError::NoFadt,
        other => other,
    })?;
    Fadt::parse(t).map_err(|e| AcpiError::Invalid("FADT", e))
}

/// The ACPI power-management timer: a fixed-frequency (3.579545 MHz) free-running counter.
#[derive(Clone, Copy)]
pub struct PmTimer {
    port: u16,
    mask: u32,
}

/// PM timer frequency in Hz.
pub const PM_TIMER_HZ: u64 = 3_579_545;

impl PmTimer {
    /// Reads the (24- or 32-bit) counter.
    pub fn read(&self) -> u32 {
        // SAFETY: the port comes from the validated FADT.
        (unsafe { inl(self.port) }) & self.mask
    }

    /// Ticks elapsed from `start` to `now`, accounting for wrap-around.
    pub fn elapsed(&self, start: u32, now: u32) -> u32 {
        now.wrapping_sub(start) & self.mask
    }
}

/// Locates the PM timer through the FADT.
pub fn pm_timer(boot: &BootInfo) -> Result<PmTimer, AcpiError> {
    let f = fadt(boot)?;
    if f.pm_tmr_blk == 0 {
        return Err(AcpiError::NoPmTimer);
    }
    Ok(PmTimer {
        port: f.pm_tmr_blk,
        mask: if f.tmr_32bit { u32::MAX } else { 0x00FF_FFFF },
    })
}

impl Acpi {
    /// Parses the ACPI tables and prepares an S5 transition.
    pub fn init(boot: &BootInfo) -> Result<Self, AcpiError> {
        let f = fadt(boot)?;
        let dsdt = table(boot, f.dsdt, "DSDT")?;
        let (a, b) = sdt::find_s5(dsdt).ok_or(AcpiError::NoS5)?;
        if f.pm1a_cnt == 0 {
            return Err(AcpiError::NoPm1Control);
        }
        kprintln!(
            "acpi: PM1a_CNT={:#x} PM1b_CNT={:#x} SLP_TYPa={a} SLP_TYPb={b} SMI_CMD={:#x} ENABLE={:#x}",
            f.pm1a_cnt,
            f.pm1b_cnt,
            f.smi_cmd,
            f.acpi_enable
        );
        Ok(Self {
            pm1a_cnt: f.pm1a_cnt,
            pm1b_cnt: f.pm1b_cnt,
            slp_typ_a: u16::from(a),
            slp_typ_b: u16::from(b),
            smi_cmd: f.smi_cmd as u16,
            acpi_enable: f.acpi_enable,
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
