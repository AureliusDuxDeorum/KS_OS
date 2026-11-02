// SPDX-License-Identifier: Apache-2.0
//! Intel VT-d DMA remapping unit (Intel VT-d specification, chapter 10 registers).
//!
//! One `Unit` per DRHD. All units share the root table built by `ksiommu::intel::Tables`; the unit only
//! needs its registers programmed, its caches flushed after table changes, and its fault log drained.

use crate::boot::BootInfo;
use crate::mmio::Mmio;
use crate::time;
use alloc::string::String;
use alloc::vec::Vec;
use ksparse::iommu::Drhd;

const VER: usize = 0x00;
const CAP: usize = 0x08;
const ECAP: usize = 0x10;
const GCMD: usize = 0x18;
const GSTS: usize = 0x1C;
const RTADDR: usize = 0x20;
const CCMD: usize = 0x28;
const FSTS: usize = 0x34;

const GCMD_TE: u32 = 1 << 31;
const GCMD_SRTP: u32 = 1 << 30;
const GCMD_WBF: u32 = 1 << 27;
const GSTS_TES: u32 = 1 << 31;
const GSTS_RTPS: u32 = 1 << 30;
const GSTS_WBFS: u32 = 1 << 27;
/// Bits of GSTS that mirror persistent enables and must be written back unchanged in GCMD.
const GSTS_PERSISTENT: u32 = 0x96FF_FFFF;

const CAP_RWBF: u64 = 1 << 4;
const MAP_LEN: u64 = 0x4000;

pub struct Fault {
    pub requester: u16,
    pub address: u64,
    pub reason: u8,
    pub write: bool,
}

pub struct Unit {
    regs: Mmio,
    cap: u64,
    ecap: u64,
    pub register_base: u64,
}

fn wait(timeout_ms: u64, mut cond: impl FnMut() -> bool) -> bool {
    let end = time::now_ns().saturating_add(timeout_ms.saturating_mul(1_000_000));
    loop {
        if cond() {
            return true;
        }
        if time::now_ns() > end {
            return false;
        }
        core::hint::spin_loop();
    }
}

impl Unit {
    pub fn new(boot: &BootInfo, d: &Drhd) -> Result<Self, String> {
        let virt = crate::vmm::map_mmio(boot, d.register_base, MAP_LEN)
            .map_err(|e| alloc::format!("map {:#x}: {e:?}", d.register_base))?;
        let regs = Mmio::new(virt, MAP_LEN);
        let cap = regs.r64(CAP);
        let ecap = regs.r64(ECAP);
        Ok(Self {
            regs,
            cap,
            ecap,
            register_base: d.register_base,
        })
    }

    pub fn version(&self) -> (u32, u32) {
        let v = self.regs.r32(VER);
        (v >> 4 & 0xF, v & 0xF)
    }

    /// Supported adapter-guest address widths: 4 levels (48-bit) preferred, else 3 levels (39-bit).
    pub fn levels(&self) -> Option<u8> {
        let sagaw = (self.cap >> 8) & 0x1F;
        if sagaw & 0b100 != 0 {
            Some(4)
        } else if sagaw & 0b010 != 0 {
            Some(3)
        } else {
            None
        }
    }

    /// Whether the unit's page walks snoop the CPU caches (otherwise table writes must be flushed).
    pub fn coherent(&self) -> bool {
        self.ecap & 1 != 0
    }

    pub fn interrupt_remapping(&self) -> bool {
        self.ecap & (1 << 3) != 0
    }

    fn command(&self, bit: u32, status_bit: u32) -> bool {
        let persistent = self.regs.r32(GSTS) & GSTS_PERSISTENT;
        self.regs.w32(GCMD, persistent | bit);
        wait(100, || self.regs.r32(GSTS) & status_bit != 0)
    }

    /// Points the unit at `root`, flushes its caches and turns translation on.
    pub fn enable(&self, root: u64) -> Result<(), String> {
        if self.regs.r32(GSTS) & GSTS_TES != 0 {
            // Firmware left translation on (Limine normally quiesces it): turn it off first.
            self.regs
                .w32(GCMD, self.regs.r32(GSTS) & GSTS_PERSISTENT & !GCMD_TE);
            wait(100, || self.regs.r32(GSTS) & GSTS_TES == 0);
        }
        self.regs.w64(RTADDR, root);
        if !self.command(GCMD_SRTP, GSTS_RTPS) {
            return Err("root table pointer was not latched".into());
        }
        self.flush();
        if !self.command(GCMD_TE, GSTS_TES) {
            return Err("translation did not enable".into());
        }
        Ok(())
    }

    /// Global context-cache and IOTLB invalidation (register-based; fine for the few updates we make).
    pub fn flush(&self) {
        if self.cap & CAP_RWBF != 0 {
            let persistent = self.regs.r32(GSTS) & GSTS_PERSISTENT;
            self.regs.w32(GCMD, persistent | GCMD_WBF);
            wait(100, || self.regs.r32(GSTS) & GSTS_WBFS == 0);
        }
        // Context cache: ICC (bit 63) with global invalidation granularity CIRG = 01.
        self.regs.w64(CCMD, (1 << 63) | (1 << 61));
        wait(100, || self.regs.r64(CCMD) & (1 << 63) == 0);
        // IOTLB: register block at ECAP.IRO * 16; IOTLB_REG at +8: IVT (63) with IIRG = 01 (global).
        let iotlb = (((self.ecap >> 8) & 0x3FF) as usize).saturating_mul(16);
        if iotlb != 0 && iotlb.saturating_add(16) <= MAP_LEN as usize {
            self.regs
                .w64(iotlb.saturating_add(8), (1 << 63) | (1 << 60));
            wait(100, || {
                self.regs.r64(iotlb.saturating_add(8)) & (1 << 63) == 0
            });
        }
    }

    /// Reads and clears pending fault records.
    pub fn drain_faults(&self, out: &mut Vec<Fault>) {
        let fsts = self.regs.r32(FSTS);
        if fsts & 0b11 == 0 {
            return;
        }
        let nfr = (((self.cap >> 40) & 0xFF) as usize).saturating_add(1);
        let fro = (((self.cap >> 24) & 0x3FF) as usize).saturating_mul(16);
        for i in 0..nfr.min(32) {
            let at = fro.saturating_add(i.saturating_mul(16));
            if at.saturating_add(16) > MAP_LEN as usize {
                break;
            }
            let hi = self.regs.r64(at.saturating_add(8));
            if hi & (1 << 63) == 0 {
                continue;
            }
            out.push(Fault {
                requester: hi as u16,
                address: self.regs.r64(at) & !0xFFF,
                reason: ((hi >> 32) & 0xFF) as u8,
                write: hi & (1 << 62) == 0, // type bit: 0 = write, 1 = read request
            });
            self.regs.w64(at.saturating_add(8), 1 << 63); // clear the fault
        }
        self.regs.w32(FSTS, fsts & 0b1_1111_1111); // clear overflow / pending bits
    }
}
