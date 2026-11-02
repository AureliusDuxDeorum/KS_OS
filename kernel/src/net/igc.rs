// SPDX-License-Identifier: Apache-2.0
//! Intel I225/I226 (igc) driver; also drives the 82576 model that QEMU emulates as `igb` (CI).
//!
//! Polled operation with one RX and one TX queue. Register values and sequences follow Intel's
//! datasheets and the mainline Linux `igc` driver; descriptor/ring logic is in `ksnet::igc` and
//! unit-tested on the host.

use super::Mmio;
use crate::boot::BootInfo;
use crate::dma::Dma;
use crate::pci::{self, Device};
use crate::time;
use alloc::format;
use alloc::string::String;
use ksnet::igc::{self as hw, Family, RxQueue, TxQueue};
use ksnet::Mac;

const RING: u16 = 512;
const BAR0_LEN: u64 = 0x20000;

/// Hardware register window ends at 128 KiB for both families.
pub fn supports(dev: &Device) -> bool {
    dev.header.vendor == hw::VENDOR_INTEL && hw::family(dev.header.device).is_some()
}

pub struct Igc {
    regs: Mmio,
    family: Family,
    device_id: u16,
    mac: Mac,
    rx: RxQueue,
    tx: TxQueue,
    rx_desc: Dma,
    rx_bufs: Dma,
    tx_desc: Dma,
    tx_bufs: Dma,
    frames_rx: u64,
    frames_tx: u64,
}

fn wait(timeout_ms: u64, mut cond: impl FnMut() -> bool) -> bool {
    let end = time::now_ns() + timeout_ms * 1_000_000;
    loop {
        if cond() {
            return true;
        }
        if time::now_ns() > end {
            return false;
        }
        time::sleep_ms(1);
    }
}

/// Reads a standard MII register through MDIC; `None` on error or timeout.
fn phy_read(regs: Mmio, family: Family, reg: u8) -> Option<u16> {
    regs.w32(hw::MDIC, hw::mdic_read(family, reg));
    let mut v = 0;
    wait(20, || {
        v = regs.r32(hw::MDIC);
        v & (hw::MDIC_READY | hw::MDIC_ERROR) != 0
    });
    (v & hw::MDIC_READY != 0 && v & hw::MDIC_ERROR == 0).then_some(v as u16)
}

fn phy_write(regs: Mmio, family: Family, reg: u8, value: u16) -> bool {
    regs.w32(hw::MDIC, hw::mdic_write(family, reg, value));
    let mut v = 0;
    wait(20, || {
        v = regs.r32(hw::MDIC);
        v & (hw::MDIC_READY | hw::MDIC_ERROR) != 0
    });
    v & hw::MDIC_READY != 0 && v & hw::MDIC_ERROR == 0
}

impl Igc {
    pub fn new(boot: &BootInfo, dev: &Device) -> Result<Self, String> {
        let family = hw::family(dev.header.device).ok_or("unsupported device")?;
        pci::enable_bus_master(dev);
        // No interrupts are used: keep legacy INTx quiet.
        pci::command_set(dev.addr, ksparse::pci::CMD_INTX_DISABLE);
        let base = pci::map_bar(boot, dev, 0).map_err(|e| format!("BAR0: {e:?}"))?;
        let bar_len = dev.bars[0].map_or(BAR0_LEN, |b| b.size).min(BAR0_LEN);
        let regs = Mmio::new(base, bar_len);

        // 1. Quiesce and reset.
        regs.w32(hw::IMC, !0);
        regs.w32(hw::RCTL, 0);
        regs.w32(hw::TCTL, hw::TCTL_PSP);
        let _ = regs.r32(hw::STATUS); // flush posted writes
        time::sleep_ms(10);
        let ctrl = regs.r32(hw::CTRL);
        regs.w32(hw::CTRL, ctrl | hw::reset_bit(family));
        if !wait(1000, || regs.r32(hw::CTRL) & hw::reset_bit(family) == 0) {
            return Err("device reset did not complete".into());
        }
        time::sleep_ms(20);
        regs.w32(hw::IMC, !0);
        let _ = regs.r32(hw::ICR);

        // 2. Station address, loaded from NVM into RAL0/RAH0 by the hardware after reset.
        let mut mac = None;
        wait(500, || {
            mac = hw::mac_from_ra(regs.r32(hw::RAL0), regs.r32(hw::RAH0));
            mac.is_some()
        });
        let mac = mac.ok_or("no valid MAC address in NVM")?;

        // 3. Link: set link up, let auto-negotiation pick speed and duplex.
        let ctrl = regs.r32(hw::CTRL);
        let force_bits = (1 << 11) | (1 << 12); // FRCSPD | FRCDPX
        regs.w32(
            hw::CTRL,
            (ctrl & !force_bits) | hw::CTRL_SLU | hw::CTRL_ASDE,
        );

        // PHY: make sure it is powered and auto-negotiating; an already-working link is left alone.
        match phy_read(regs, family, hw::PHY_BMCR) {
            Some(bmcr) => {
                let bmsr = phy_read(regs, family, hw::PHY_BMSR).unwrap_or(0);
                kprintln!("net: phy BMCR={bmcr:#06x} BMSR={bmsr:#06x}");
                if let Some(fix) = hw::bmcr_fixup(bmcr, bmsr) {
                    kprintln!("net: phy not auto-negotiated yet; (re)starting auto-negotiation");
                    phy_write(regs, family, hw::PHY_BMCR, fix);
                }
            }
            None => {
                kprintln!("net: phy not reachable over MDIO (continuing; link may still come up)")
            }
        }

        // 4. Rings.
        let alloc = |len: usize| Dma::alloc(boot.hhdm, len, false).ok_or("out of DMA memory");
        let rx_desc = alloc(RxQueue::ring_bytes(RING))?;
        let rx_bufs = alloc(RxQueue::buffer_bytes(RING))?;
        let tx_desc = alloc(RxQueue::ring_bytes(RING))?;
        let tx_bufs = alloc(RxQueue::buffer_bytes(RING))?;
        let mut rx = RxQueue::new(RING, rx_bufs.phys);
        let tx = TxQueue::new(RING, tx_bufs.phys);
        let rx_tail = rx.init(&rx_desc);

        // Multicast table cleared; only unicast to our address and broadcast are accepted.
        for i in 0..128 {
            regs.w32(hw::MTA + 4 * i, 0);
        }

        regs.w64(hw::RDBAL0, rx_desc.phys);
        regs.w32(hw::RDLEN0, RxQueue::ring_bytes(RING) as u32);
        regs.w32(
            hw::SRRCTL0,
            hw::SRRCTL_BSIZE_2K | hw::SRRCTL_DESCTYPE_ADV_ONEBUF | hw::SRRCTL_DROP_EN,
        );
        regs.w32(hw::RDH0, 0);
        regs.w32(hw::RDT0, 0);
        regs.w32(hw::RXDCTL0, hw::XDCTL_ENABLE | 8 | (8 << 8) | (1 << 16));
        if !wait(100, || regs.r32(hw::RXDCTL0) & hw::XDCTL_ENABLE != 0) {
            return Err("RX queue did not enable".into());
        }
        regs.w32(hw::RDT0, u32::from(rx_tail));
        regs.w32(hw::RCTL, hw::RCTL_EN | hw::RCTL_BAM | hw::RCTL_SECRC);

        regs.w64(hw::TDBAL0, tx_desc.phys);
        regs.w32(hw::TDLEN0, RxQueue::ring_bytes(RING) as u32);
        regs.w32(hw::TDH0, 0);
        regs.w32(hw::TDT0, 0);
        regs.w32(hw::TXDCTL0, hw::XDCTL_ENABLE | 8 | (1 << 8) | (1 << 16));
        if !wait(100, || regs.r32(hw::TXDCTL0) & hw::XDCTL_ENABLE != 0) {
            return Err("TX queue did not enable".into());
        }
        regs.w32(
            hw::TCTL,
            hw::TCTL_EN | hw::TCTL_PSP | (0x0F << 4) | (0x3F << 12),
        );

        Ok(Self {
            regs,
            family,
            device_id: dev.header.device,
            mac,
            rx,
            tx,
            rx_desc,
            rx_bufs,
            tx_desc,
            tx_bufs,
            frames_rx: 0,
            frames_tx: 0,
        })
    }

    pub fn name(&self) -> &'static str {
        match (self.family, self.device_id) {
            (Family::Igb, _) => "Intel 82576 (igb)",
            (_, 0x15F3 | 0x125C) => "Intel I225-V/I226-V (igc)",
            (_, 0x15F2 | 0x125B) => "Intel I225-LM/I226-LM (igc)",
            _ => "Intel I225/I226 (igc)",
        }
    }

    pub fn describe_link(&mut self) -> String {
        let l = hw::link_from_status(self.regs.r32(hw::STATUS));
        if l.up {
            format!(
                "link {} Mb/s {}",
                l.mbps,
                if l.full_duplex {
                    "full duplex"
                } else {
                    "half duplex"
                }
            )
        } else {
            "no link".into()
        }
    }

    pub fn mac(&self) -> Mac {
        self.mac
    }

    pub fn link_up(&mut self) -> bool {
        self.regs.r32(hw::STATUS) & hw::STATUS_LU != 0
    }

    pub fn receive(&mut self, buf: &mut [u8]) -> Option<usize> {
        let (len, tail) = self.rx.poll(&self.rx_desc, &self.rx_bufs, buf);
        if let Some(t) = tail {
            self.regs.w32(hw::RDT0, u32::from(t));
        }
        if len.is_some() {
            self.frames_rx += 1;
        }
        len
    }

    pub fn transmit(&mut self, frame: &[u8]) -> bool {
        match self.tx.submit(&self.tx_desc, &self.tx_bufs, frame) {
            Some(tail) => {
                core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
                self.regs.w32(hw::TDT0, u32::from(tail));
                self.frames_tx += 1;
                true
            }
            None => false,
        }
    }
}
