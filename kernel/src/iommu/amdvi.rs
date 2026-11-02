// SPDX-License-Identifier: Apache-2.0
//! AMD-Vi (AMD I/O Virtualization Technology) IOMMU (AMD IOMMU specification, chapter 3).
//!
//! One device table shared by all functions, a command buffer for invalidations and an event log for
//! faults. The v1 page tables and the DTE layout come from `ksiommu`.

use crate::boot::BootInfo;
use crate::dma::Dma;
use crate::mmio::Mmio;
use crate::time;
use alloc::string::String;
use alloc::vec::Vec;
use ksiommu::amd::{self, DEVICE_TABLE_BYTES};
use ksnet::dma::DmaMem;
use ksparse::iommu::Ivhd;

const DEV_TAB_BASE: usize = 0x0000;
const CMD_BUF_BASE: usize = 0x0008;
const EVT_LOG_BASE: usize = 0x0010;
const CONTROL: usize = 0x0018;
const CMD_HEAD: usize = 0x2000;
const CMD_TAIL: usize = 0x2008;
const EVT_HEAD: usize = 0x2010;
const EVT_TAIL: usize = 0x2018;
const STATUS: usize = 0x2020;

const CTRL_IOMMU_EN: u64 = 1 << 0;
const CTRL_EVT_LOG_EN: u64 = 1 << 2;
const CTRL_CMD_BUF_EN: u64 = 1 << 12;
const STATUS_EVT_OVERFLOW: u32 = 1 << 0;

const QUEUE_ENTRIES: usize = 256;
const ENTRY: usize = 16;
const MAP_LEN: u64 = 0x4000;
const OP_COMPLETION_WAIT: u32 = 0x1;
const OP_INVALIDATE_DEVTAB: u32 = 0x2;
const OP_INVALIDATE_PAGES: u32 = 0x3;
const EVENT_IO_PAGE_FAULT: u32 = 0x2;

pub struct Fault {
    pub requester: u16,
    pub address: u64,
    pub code: u32,
}

pub struct AmdVi {
    regs: Mmio,
    dev_table: Dma,
    cmd: Dma,
    evt: Dma,
    sem: Dma,
    cmd_tail: usize,
    evt_head: usize,
    nonce: u64,
    pub ranges: Vec<(u16, u16)>,
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

impl AmdVi {
    pub fn new(boot: &BootInfo, h: &Ivhd) -> Result<Self, String> {
        let virt = crate::vmm::map_mmio(boot, h.register_base, MAP_LEN)
            .map_err(|e| alloc::format!("map {:#x}: {e:?}", h.register_base))?;
        let regs = Mmio::new(virt, MAP_LEN);
        let alloc = |len: usize| Dma::alloc(boot.hhdm, len, false).ok_or("out of DMA memory");
        let dev_table = alloc(DEVICE_TABLE_BYTES)?;
        let cmd = alloc(QUEUE_ENTRIES * ENTRY)?;
        let evt = alloc(QUEUE_ENTRIES * ENTRY)?;
        let sem = alloc(4096)?;
        Ok(Self {
            regs,
            dev_table,
            cmd,
            evt,
            sem,
            cmd_tail: 0,
            evt_head: 0,
            nonce: 0,
            ranges: h.ranges.clone(),
            register_base: h.register_base,
        })
    }

    pub fn covers(&self, device_id: u16) -> bool {
        self.ranges
            .iter()
            .any(|&(lo, hi)| (lo..=hi).contains(&device_id))
    }

    /// Programs the tables and turns the IOMMU on. Every DTE is invalid (all DMA blocked) until `set_dte`.
    pub fn enable(&mut self) -> Result<(), String> {
        let r = self.regs;
        r.w64(
            CONTROL,
            r.r64(CONTROL) & !(CTRL_IOMMU_EN | CTRL_CMD_BUF_EN | CTRL_EVT_LOG_EN),
        );
        let size_field = (DEVICE_TABLE_BYTES / 4096 - 1) as u64;
        r.w64(DEV_TAB_BASE, self.dev_table.phys | size_field);
        r.w64(CMD_BUF_BASE, self.cmd.phys | (8u64 << 56)); // 2^8 = 256 entries
        r.w64(EVT_LOG_BASE, self.evt.phys | (8u64 << 56));
        r.w64(CMD_HEAD, 0);
        r.w64(CMD_TAIL, 0);
        r.w64(EVT_HEAD, 0);
        r.w64(EVT_TAIL, 0);
        self.cmd_tail = 0;
        self.evt_head = 0;
        r.w64(CONTROL, CTRL_CMD_BUF_EN | CTRL_EVT_LOG_EN);
        r.w64(CONTROL, CTRL_CMD_BUF_EN | CTRL_EVT_LOG_EN | CTRL_IOMMU_EN);
        if r.r64(CONTROL) & CTRL_IOMMU_EN == 0 {
            return Err("the IOMMU did not turn on".into());
        }
        Ok(())
    }

    fn push(&mut self, c: [u32; 4]) -> Result<(), String> {
        let r = self.regs;
        let next = (self.cmd_tail + ENTRY) % (QUEUE_ENTRIES * ENTRY);
        if next as u64 == r.r64(CMD_HEAD) & 0x7FFF0 {
            return Err("command buffer full".into());
        }
        for (i, w) in c.iter().enumerate() {
            self.cmd.write32(self.cmd_tail + i * 4, *w);
        }
        self.cmd_tail = next;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        r.w64(CMD_TAIL, self.cmd_tail as u64);
        Ok(())
    }

    /// Runs the queued commands to completion (COMPLETION_WAIT with a memory semaphore).
    fn sync(&mut self) -> Result<(), String> {
        self.nonce = self.nonce.wrapping_add(1);
        let addr = self.sem.phys;
        self.sem.write64(0, 0);
        self.push([
            (addr as u32 & !7) | 1, // store the data below at `addr`
            ((addr >> 32) as u32 & 0xF_FFFF) | (OP_COMPLETION_WAIT << 28),
            self.nonce as u32,
            (self.nonce >> 32) as u32,
        ])?;
        let nonce = self.nonce;
        if wait(100, || self.sem.read64(0) == nonce) {
            Ok(())
        } else {
            Err("IOMMU command did not complete".into())
        }
    }

    /// Installs (or, with `None`, clears) the DTE of `device_id` and invalidates cached copies.
    pub fn set_dte(&mut self, device_id: u16, entry: Option<(u64, u8, u16)>) -> Result<(), String> {
        let base = amd::dte_offset(device_id);
        let words = entry.map_or([0; amd::DTE_WORDS], |(root, levels, did)| {
            amd::dte(root, levels, did)
        });
        // Write the data words first and word 0 (which holds the valid bit) last.
        for i in (0..amd::DTE_WORDS).rev() {
            self.dev_table.write64(base + i * 8, words[i]);
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.push([u32::from(device_id), OP_INVALIDATE_DEVTAB << 28, 0, 0])?;
        if let Some((_, _, did)) = entry {
            // Invalidate every cached translation of the domain: S = 1, address bits all ones.
            self.push([
                0,
                (OP_INVALIDATE_PAGES << 28) | u32::from(did),
                0xFFFF_F001,
                0x7FFF_FFFF,
            ])?;
        }
        self.sync()
    }

    pub fn drain_faults(&mut self, out: &mut Vec<Fault>) {
        let r = self.regs;
        let tail = (r.r64(EVT_TAIL) & 0x7FFF0) as usize;
        let mut n = 0;
        while self.evt_head != tail && n < QUEUE_ENTRIES {
            let at = self.evt_head;
            let (w0, w1) = (self.evt.read32(at), self.evt.read32(at + 4));
            let code = w1 >> 28;
            out.push(Fault {
                requester: w0 as u16,
                address: self.evt.read64(at + 8),
                code,
            });
            let _ = EVENT_IO_PAGE_FAULT;
            self.evt_head = (at + ENTRY) % (QUEUE_ENTRIES * ENTRY);
            n += 1;
        }
        r.w64(EVT_HEAD, self.evt_head as u64);
        if r.r32(STATUS) & STATUS_EVT_OVERFLOW != 0 {
            // Overflow: the log was full; restart it.
            r.w64(CONTROL, r.r64(CONTROL) & !CTRL_EVT_LOG_EN);
            r.w64(EVT_HEAD, 0);
            r.w64(EVT_TAIL, 0);
            self.evt_head = 0;
            r.w64(CONTROL, r.r64(CONTROL) | CTRL_EVT_LOG_EN);
        }
    }
}
