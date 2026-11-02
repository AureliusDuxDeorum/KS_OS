// SPDX-License-Identifier: Apache-2.0
//! virtio-net (modern PCI transport, polled). Used on virtual machines and for CI under QEMU.
//!
//! Spec: OASIS Virtual I/O Device (VIRTIO) 1.2, sections 4.1 (PCI) and 5.1 (network device).

use super::{Mmio, NET_IRQS};
use crate::boot::BootInfo;
use crate::dma::Dma;
use crate::pci::{self, Device};
use crate::time;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{fence, Ordering};
use ksnet::virtio::{self as v, Layout, Virtqueue};
use ksnet::Mac;

const QUEUE: u16 = 256;
const BUF: usize = 2048;

pub fn supports(dev: &Device) -> bool {
    dev.header.vendor == v::VENDOR && matches!(dev.header.device, 0x1000 | v::DEVICE_NET_MODERN)
}

struct Queue {
    vq: Virtqueue,
    mem: Dma,
    bufs: Dma,
    slot_of: Vec<u16>,
    free_slots: Vec<u16>,
    notify: usize,
}

pub struct VirtioNet {
    common: Mmio,
    notify_base: Mmio,
    device_cfg: Mmio,
    mac: Mac,
    has_status: bool,
    rx: Queue,
    tx: Queue,
    irq: bool,
}

fn wait(timeout_ms: u64, mut cond: impl FnMut() -> bool) -> bool {
    let end = time::now_ns() + timeout_ms * 1_000_000;
    while !cond() {
        if time::now_ns() > end {
            return false;
        }
        time::sleep_ms(1);
    }
    true
}

struct Cap {
    cfg_type: u8,
    bar: u8,
    offset: u32,
    length: u32,
    multiplier: u32,
}

fn vendor_caps(dev: &Device) -> Vec<Cap> {
    dev.caps
        .iter()
        .filter(|c| c.id == 0x09)
        .map(|c| {
            let o = u16::from(c.offset);
            let w0 = pci::read32(dev.addr, o);
            let w1 = pci::read32(dev.addr, o + 4);
            Cap {
                cfg_type: (w0 >> 24) as u8,
                bar: w1 as u8,
                offset: pci::read32(dev.addr, o + 8),
                length: pci::read32(dev.addr, o + 12),
                multiplier: pci::read32(dev.addr, o + 16),
            }
        })
        .collect()
}

impl VirtioNet {
    pub fn new(boot: &BootInfo, dev: &Device) -> Result<Self, String> {
        pci::enable_bus_master(dev);
        pci::command_set(dev.addr, ksparse::pci::CMD_INTX_DISABLE);
        let caps = vendor_caps(dev);
        let find = |t: u8| caps.iter().find(|c| c.cfg_type == t);
        let (Some(cc), Some(nc), Some(dc)) = (
            find(v::CAP_COMMON),
            find(v::CAP_NOTIFY),
            find(v::CAP_DEVICE),
        ) else {
            return Err("no modern virtio capabilities (legacy-only device)".into());
        };
        let map = |c: &Cap| -> Result<Mmio, String> {
            let bar = dev
                .bars
                .get(usize::from(c.bar))
                .copied()
                .flatten()
                .ok_or("capability points at a missing BAR")?;
            if u64::from(c.offset) + u64::from(c.length) > bar.size {
                return Err("capability exceeds its BAR".into());
            }
            let base =
                pci::map_bar(boot, dev, usize::from(c.bar)).map_err(|e| format!("map: {e:?}"))?;
            Ok(Mmio::new(base + u64::from(c.offset), u64::from(c.length)))
        };
        // A malicious or buggy device could advertise structures too small for the accesses below
        // (every `Mmio` access is bounds-checked and would panic): validate instead (ADR-0030 S-05).
        if cc.length < 0x38 || dc.length < 8 || nc.length < 2 {
            return Err("virtio capability regions are too small".into());
        }
        let common = map(cc)?;
        let notify_base = map(nc)?;
        let device_cfg = map(dc)?;

        // Reset, then ACKNOWLEDGE | DRIVER.
        common.w8(v::CC_DEVICE_STATUS, 0);
        if !wait(100, || common.r8(v::CC_DEVICE_STATUS) == 0) {
            return Err("reset did not complete".into());
        }
        common.w8(v::CC_DEVICE_STATUS, v::STATUS_ACKNOWLEDGE);
        common.w8(
            v::CC_DEVICE_STATUS,
            v::STATUS_ACKNOWLEDGE | v::STATUS_DRIVER,
        );

        // Features: VERSION_1 is mandatory; MAC and STATUS are used if offered.
        common.w32(v::CC_DEVICE_FEATURE_SELECT, 0);
        let lo = u64::from(common.r32(v::CC_DEVICE_FEATURE));
        common.w32(v::CC_DEVICE_FEATURE_SELECT, 1);
        let hi = u64::from(common.r32(v::CC_DEVICE_FEATURE));
        let offered = lo | (hi << 32);
        if offered & v::F_VERSION_1 == 0 {
            return Err("device does not offer VIRTIO_F_VERSION_1".into());
        }
        let wanted = v::F_VERSION_1 | (offered & (v::F_NET_MAC | v::F_NET_STATUS));
        common.w32(v::CC_DRIVER_FEATURE_SELECT, 0);
        common.w32(v::CC_DRIVER_FEATURE, wanted as u32);
        common.w32(v::CC_DRIVER_FEATURE_SELECT, 1);
        common.w32(v::CC_DRIVER_FEATURE, (wanted >> 32) as u32);
        let st = v::STATUS_ACKNOWLEDGE | v::STATUS_DRIVER | v::STATUS_FEATURES_OK;
        common.w8(v::CC_DEVICE_STATUS, st);
        if common.r8(v::CC_DEVICE_STATUS) & v::STATUS_FEATURES_OK == 0 {
            return Err("device rejected the feature set".into());
        }

        let mac = if wanted & v::F_NET_MAC != 0 {
            let mut m = [0u8; 6];
            for (i, b) in m.iter_mut().enumerate() {
                *b = device_cfg.r8(v::NET_CFG_MAC + i);
            }
            m
        } else {
            return Err("device has no MAC address".into());
        };
        let has_status = wanted & v::F_NET_STATUS != 0;

        let mk = |index: u16| -> Result<Queue, String> {
            common.w16(v::CC_QUEUE_SELECT, index);
            let max = common.r16(v::CC_QUEUE_SIZE);
            if max == 0 {
                return Err(format!("queue {index} unavailable"));
            }
            // Largest power of two not above both our limit and what the device offers.
            let size = 1u16 << (15 - QUEUE.min(max).leading_zeros());
            let layout = Layout::new(size);
            let mem = Dma::alloc(boot.hhdm, layout.total, false).ok_or("out of DMA memory")?;
            let bufs =
                Dma::alloc(boot.hhdm, usize::from(size) * BUF, false).ok_or("out of DMA memory")?;
            let vq = Virtqueue::new(size, &mem);
            common.w16(v::CC_QUEUE_SIZE, size);
            common.w64(v::CC_QUEUE_DESC, mem.phys + layout.desc as u64);
            common.w64(v::CC_QUEUE_DRIVER, mem.phys + layout.avail as u64);
            common.w64(v::CC_QUEUE_DEVICE, mem.phys + layout.used as u64);
            let notify = common.r16(v::CC_QUEUE_NOTIFY_OFF);
            let notify_byte = usize::from(notify) * nc.multiplier as usize;
            if notify_byte
                .checked_add(2)
                .is_none_or(|e| e > nc.length as usize)
            {
                return Err(format!("queue {index}: notify offset outside its region"));
            }
            common.w16(v::CC_QUEUE_ENABLE, 1);
            Ok(Queue {
                vq,
                mem,
                bufs,
                slot_of: alloc::vec![0; usize::from(size)],
                free_slots: (0..size).collect(),
                notify: notify_byte,
            })
        };
        let rx = mk(0)?;
        let tx = mk(1)?;
        let mut this = Self {
            common,
            notify_base,
            device_cfg,
            mac,
            has_status,
            rx,
            tx,
            irq: false,
        };
        this.setup_interrupts(boot, dev);
        this.refill_rx();
        this.common
            .w8(v::CC_DEVICE_STATUS, st | v::STATUS_DRIVER_OK);
        Ok(this)
    }

    /// MSI-X (or MSI) with a single vector for configuration changes and both queues; polled otherwise.
    fn setup_interrupts(&mut self, boot: &BootInfo, dev: &Device) {
        let Some(vector) = crate::irq::alloc_vector() else {
            return;
        };
        match pci::enable_msi(boot, dev, vector, crate::apic::id()) {
            Ok(kind) => {
                crate::irq::register(vector, irq_handler);
                let c = self.common;
                c.w16(v::CC_MSIX_CONFIG, 0);
                let mut ok = c.r16(v::CC_MSIX_CONFIG) != v::NO_VECTOR;
                for q in 0..2u16 {
                    c.w16(v::CC_QUEUE_SELECT, q);
                    c.w16(v::CC_QUEUE_MSIX_VECTOR, 0);
                    ok &= c.r16(v::CC_QUEUE_MSIX_VECTOR) != v::NO_VECTOR;
                }
                self.irq = ok;
                if ok {
                    kprintln!("net: virtio-net interrupts via {kind}, vector {vector:#x}");
                } else {
                    kprintln!("net: virtio-net: device refused the MSI-X vectors; polled mode");
                }
            }
            Err(e) => kprintln!("net: virtio-net: no MSI/MSI-X ({e:?}); polled mode"),
        }
    }

    pub fn rearm_irq(&mut self) {}

    pub fn irq_enabled(&self) -> bool {
        self.irq
    }

    fn notify(&self, queue_index: u16, offset: usize) {
        fence(Ordering::SeqCst);
        self.notify_base.w16(offset, queue_index);
    }

    fn refill_rx(&mut self) {
        let mut added = false;
        while let Some(&slot) = self.rx.free_slots.last() {
            let phys = self.rx.bufs.phys + u64::from(slot) * BUF as u64;
            match self
                .rx
                .vq
                .add_chain(&self.rx.mem, &[(phys, BUF as u32, true)])
            {
                Some(head) => {
                    self.rx.free_slots.pop();
                    self.rx.slot_of[usize::from(head)] = slot;
                    added = true;
                }
                None => break,
            }
        }
        if added {
            self.notify(0, self.rx.notify);
        }
    }

    pub fn mac(&self) -> Mac {
        self.mac
    }

    pub fn describe_link(&mut self) -> String {
        if self.link_up() {
            "link up (virtual)".into()
        } else {
            "no link".into()
        }
    }

    pub fn link_up(&mut self) -> bool {
        if self.has_status {
            self.device_cfg.r16(v::NET_CFG_STATUS) & v::NET_S_LINK_UP != 0
        } else {
            true
        }
    }

    pub fn receive(&mut self, buf: &mut [u8]) -> Option<usize> {
        let (head, written) = self.rx.vq.pop_used(&self.rx.mem)?;
        let slot = self.rx.slot_of[usize::from(head)];
        let total = (written as usize).min(BUF);
        let mut out = None;
        if total > v::NET_HDR_LEN {
            let len = total - v::NET_HDR_LEN;
            if len >= 14 && len <= buf.len() {
                use ksnet::dma::DmaMem;
                self.rx
                    .bufs
                    .read_bytes(usize::from(slot) * BUF + v::NET_HDR_LEN, &mut buf[..len]);
                out = Some(len);
            }
        }
        self.rx.free_slots.push(slot);
        self.refill_rx();
        out
    }

    pub fn transmit(&mut self, frame: &[u8]) -> bool {
        use ksnet::dma::DmaMem;
        // Reclaim completed transmissions first.
        while let Some((head, _)) = self.tx.vq.pop_used(&self.tx.mem) {
            let slot = self.tx.slot_of[usize::from(head)];
            self.tx.free_slots.push(slot);
        }
        if frame.len() < 14 || frame.len() + v::NET_HDR_LEN > BUF {
            return false;
        }
        let Some(&slot) = self.tx.free_slots.last() else {
            return false;
        };
        let off = usize::from(slot) * BUF;
        self.tx.bufs.write_bytes(off, &[0u8; v::NET_HDR_LEN]);
        self.tx.bufs.write_bytes(off + v::NET_HDR_LEN, frame);
        let phys = self.tx.bufs.phys + off as u64;
        let len = (v::NET_HDR_LEN + frame.len()) as u32;
        match self.tx.vq.add_chain(&self.tx.mem, &[(phys, len, false)]) {
            Some(head) => {
                self.tx.free_slots.pop();
                self.tx.slot_of[usize::from(head)] = slot;
                self.notify(1, self.tx.notify);
                true
            }
            None => false,
        }
    }
}

fn irq_handler(_frame: &mut crate::idt::TrapFrame) {
    NET_IRQS.fetch_add(1, core::sync::atomic::Ordering::Release);
}
