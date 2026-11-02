// SPDX-License-Identifier: Apache-2.0
//! Intel igc (I225/I226 2.5GbE) and igb (82576, the QEMU test model) definitions and ring logic.
//!
//! Both families share the queue register block at 0xC000/0xE000 and the *advanced* descriptor
//! formats used here. Register layout follows the public Intel datasheets (I225, 82576) and the
//! mainline Linux `igc`/`igb` drivers; the ring logic is generic over [`DmaMem`] so it is tested
//! on the host against a simulated device that writes descriptors back like the hardware does.

use crate::dma::DmaMem;

pub const VENDOR_INTEL: u16 = 0x8086;

/// Which register dialect a device speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// I225 / I226.
    Igc,
    /// 82576 (emulated by QEMU's `igb`, used for CI).
    Igb,
}

pub fn family(device_id: u16) -> Option<Family> {
    match device_id {
        // I225-LM, I225-V, I225-IT, I226-LM, I226-V, I226-IT, I225-K, I225-K2, I226-K, I225-LMvP, I226-LMvP.
        0x15F2 | 0x15F3 | 0x0D9F | 0x125B | 0x125C | 0x125D | 0x3100 | 0x3101 | 0x5502 | 0x5503
        | 0x125E | 0x125F => Some(Family::Igc),
        0x10C9 => Some(Family::Igb),
        _ => None,
    }
}

// ---- registers (byte offsets in BAR0) ----
pub const CTRL: usize = 0x0000;
pub const STATUS: usize = 0x0008;
pub const RCTL: usize = 0x0100;
pub const TCTL: usize = 0x0400;
pub const TIPG: usize = 0x0410;
pub const MDIC: usize = 0x0020;
pub const ICR: usize = 0x1500;
pub const IMS: usize = 0x1508;
pub const IMC: usize = 0x150C;
pub const RXCSUM: usize = 0x5000;
pub const MTA: usize = 0x5200;
pub const RAL0: usize = 0x5400;
pub const RAH0: usize = 0x5404;
pub const RDBAL0: usize = 0xC000;
pub const RDBAH0: usize = 0xC004;
pub const RDLEN0: usize = 0xC008;
pub const SRRCTL0: usize = 0xC00C;
pub const RDH0: usize = 0xC010;
pub const RDT0: usize = 0xC018;
pub const RXDCTL0: usize = 0xC028;
pub const TDBAL0: usize = 0xE000;
pub const TDBAH0: usize = 0xE004;
pub const TDLEN0: usize = 0xE008;
pub const TDH0: usize = 0xE010;
pub const TDT0: usize = 0xE018;
pub const TXDCTL0: usize = 0xE028;

// ---- bits ----
pub const CTRL_FD: u32 = 1 << 0;
pub const CTRL_ASDE: u32 = 1 << 5;
pub const CTRL_SLU: u32 = 1 << 6;
pub const CTRL_RST: u32 = 1 << 26;
pub const CTRL_DEV_RST: u32 = 1 << 29;
pub const CTRL_PHY_RST: u32 = 1 << 31;
pub const STATUS_FD: u32 = 1 << 0;
pub const STATUS_LU: u32 = 1 << 1;
pub const STATUS_SPEED_100: u32 = 1 << 6;
pub const STATUS_SPEED_1000: u32 = 1 << 7;
pub const STATUS_SPEED_2500: u32 = 1 << 22;
pub const RCTL_EN: u32 = 1 << 1;
pub const RCTL_UPE: u32 = 1 << 3;
pub const RCTL_MPE: u32 = 1 << 4;
pub const RCTL_BAM: u32 = 1 << 15;
pub const RCTL_SECRC: u32 = 1 << 26;
pub const TCTL_EN: u32 = 1 << 1;
pub const TCTL_PSP: u32 = 1 << 3;
pub const SRRCTL_BSIZE_2K: u32 = 2; // units of 1 KiB
pub const SRRCTL_DESCTYPE_ADV_ONEBUF: u32 = 1 << 25;
pub const SRRCTL_DROP_EN: u32 = 1 << 31;
pub const XDCTL_ENABLE: u32 = 1 << 25;
pub const RAH_AV: u32 = 1 << 31;

// MDIO (PHY management) interface.
pub const MDIC_READY: u32 = 1 << 28;
pub const MDIC_ERROR: u32 = 1 << 30;
const MDIC_OP_WRITE: u32 = 1 << 26;
const MDIC_OP_READ: u32 = 1 << 27;
pub const PHY_BMCR: u8 = 0;
pub const PHY_BMSR: u8 = 1;
pub const BMCR_ANRESTART: u16 = 1 << 9;
pub const BMCR_PDOWN: u16 = 1 << 11;
pub const BMCR_ANENABLE: u16 = 1 << 12;
pub const BMSR_LSTATUS: u16 = 1 << 2;
pub const BMSR_ANCOMPLETE: u16 = 1 << 5;

/// PHY address on the MDIO bus: the I225/I226 internal PHY is 0, the 82576 copper PHY is 1.
pub const fn phy_address(family: Family) -> u32 {
    match family {
        Family::Igc => 0,
        Family::Igb => 1,
    }
}

pub const fn mdic_read(family: Family, reg: u8) -> u32 {
    MDIC_OP_READ | (phy_address(family) << 21) | ((reg as u32) << 16)
}

pub const fn mdic_write(family: Family, reg: u8, value: u16) -> u32 {
    MDIC_OP_WRITE | (phy_address(family) << 21) | ((reg as u32) << 16) | value as u32
}

/// The BMCR value to write so the PHY is powered and auto-negotiating, or `None` if it already has
/// completed auto-negotiation (an established link is then left untouched).
pub const fn bmcr_fixup(bmcr: u16, bmsr: u16) -> Option<u16> {
    if bmcr & BMCR_PDOWN != 0 || bmcr & BMCR_ANENABLE == 0 || bmsr & BMSR_ANCOMPLETE == 0 {
        Some((bmcr & !BMCR_PDOWN) | BMCR_ANENABLE | BMCR_ANRESTART)
    } else {
        None
    }
}

/// The register value that resets the device for `family`.
pub const fn reset_bit(family: Family) -> u32 {
    match family {
        Family::Igc => CTRL_DEV_RST,
        Family::Igb => CTRL_RST,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Link {
    pub up: bool,
    pub mbps: u32,
    pub full_duplex: bool,
}

pub fn link_from_status(status: u32) -> Link {
    let mbps = if status & STATUS_SPEED_2500 != 0 {
        2500
    } else if status & STATUS_SPEED_1000 != 0 {
        1000
    } else if status & STATUS_SPEED_100 != 0 {
        100
    } else {
        10
    };
    Link {
        up: status & STATUS_LU != 0,
        mbps,
        full_duplex: status & STATUS_FD != 0,
    }
}

/// The station address the hardware loaded from NVM into receive-address register 0.
pub fn mac_from_ra(ral: u32, rah: u32) -> Option<[u8; 6]> {
    if rah & RAH_AV == 0 {
        return None;
    }
    let l = ral.to_le_bytes();
    let h = rah.to_le_bytes();
    let mac = [l[0], l[1], l[2], l[3], h[0], h[1]];
    let valid = mac != [0; 6] && mac != [0xFF; 6] && mac[0] & 1 == 0;
    valid.then_some(mac)
}

// ---- descriptor rings ----
pub const DESC_SIZE: usize = 16;
/// Per-slot packet buffer size (BSIZEPKT = 2 KiB; a standard 1518-byte frame fits).
pub const BUF_SIZE: usize = 2048;

const RX_DD: u32 = 1 << 0;
const RX_EOP: u32 = 1 << 1;
/// CE, SE, SEQ, CXE and RXE: frame-level errors (checksum-offload bits are not used).
const RX_ERR_MASK: u32 = (1 << 24) | (1 << 25) | (1 << 26) | (1 << 28) | (1 << 31);

const TX_DTYP_DATA: u32 = 0x3 << 20;
const TX_EOP: u32 = 1 << 24;
const TX_IFCS: u32 = 1 << 25;
const TX_RS: u32 = 1 << 27;
const TX_DEXT: u32 = 1 << 29;
const TX_DD: u32 = 1;

/// Receive queue: descriptors are re-armed in place; the driver writes RDT after each frame.
pub struct RxQueue {
    n: u16,
    next: u16,
    bufs_phys: u64,
    pub dropped: u32,
}

impl RxQueue {
    pub fn new(n: u16, bufs_phys: u64) -> Self {
        assert!(n.is_power_of_two() && n >= 8);
        Self {
            n,
            next: 0,
            bufs_phys,
            dropped: 0,
        }
    }

    fn arm(&self, desc: &impl DmaMem, i: u16) {
        let o = usize::from(i) * DESC_SIZE;
        desc.write64(o, self.bufs_phys + u64::from(i) * BUF_SIZE as u64);
        desc.write64(o + 8, 0); // clears the write-back status
    }

    /// Arms every descriptor and returns the initial tail (RDT) value.
    pub fn init(&mut self, desc: &impl DmaMem) -> u16 {
        for i in 0..self.n {
            self.arm(desc, i);
        }
        self.next = 0;
        self.n - 1
    }

    pub const fn ring_bytes(n: u16) -> usize {
        n as usize * DESC_SIZE
    }

    pub const fn buffer_bytes(n: u16) -> usize {
        n as usize * BUF_SIZE
    }

    /// Takes the next completed frame, copying it to `out`. Returns the frame length (if one was
    /// delivered) and the new RDT value to write whenever a descriptor was consumed.
    pub fn poll(
        &mut self,
        desc: &impl DmaMem,
        bufs: &impl DmaMem,
        out: &mut [u8],
    ) -> (Option<usize>, Option<u16>) {
        let i = self.next;
        let o = usize::from(i) * DESC_SIZE;
        let status = desc.read32(o + 8);
        if status & RX_DD == 0 {
            return (None, None);
        }
        let len = usize::from(desc.read16(o + 12));
        let good =
            status & RX_EOP != 0 && status & RX_ERR_MASK == 0 && len >= 14 && len <= out.len();
        let mut delivered = None;
        if good {
            bufs.read_bytes(usize::from(i) * BUF_SIZE, &mut out[..len]);
            delivered = Some(len);
        } else {
            self.dropped = self.dropped.wrapping_add(1);
        }
        self.arm(desc, i);
        self.next = (i + 1) % self.n;
        (delivered, Some(i)) // the descriptor just re-armed is the new tail
    }
}

/// Transmit queue: one data descriptor per frame, completion reported via DD write-back (RS).
pub struct TxQueue {
    n: u16,
    tail: u16,
    clean: u16,
    used: u16,
    bufs_phys: u64,
}

impl TxQueue {
    pub fn new(n: u16, bufs_phys: u64) -> Self {
        assert!(n.is_power_of_two() && n >= 8);
        Self {
            n,
            tail: 0,
            clean: 0,
            used: 0,
            bufs_phys,
        }
    }

    pub fn free(&self) -> u16 {
        self.n - self.used
    }

    fn reclaim(&mut self, desc: &impl DmaMem) {
        while self.used > 0 {
            let o = usize::from(self.clean) * DESC_SIZE;
            if desc.read32(o + 12) & TX_DD == 0 {
                break;
            }
            desc.write32(o + 12, 0);
            self.clean = (self.clean + 1) % self.n;
            self.used -= 1;
        }
    }

    /// Queues one frame; returns the new TDT value to write, or `None` if the ring is full
    /// or the frame size is invalid.
    pub fn submit(&mut self, desc: &impl DmaMem, bufs: &impl DmaMem, frame: &[u8]) -> Option<u16> {
        if frame.len() < 14 || frame.len() > BUF_SIZE {
            return None;
        }
        self.reclaim(desc);
        if self.used == self.n - 1 {
            return None;
        }
        let i = self.tail;
        let o = usize::from(i) * DESC_SIZE;
        bufs.write_bytes(usize::from(i) * BUF_SIZE, frame);
        let len = frame.len() as u32;
        desc.write64(o, self.bufs_phys + u64::from(i) * BUF_SIZE as u64);
        desc.write32(o + 12, len << 14);
        // Command word last: the device may fetch as soon as TDT moves, and the barrier is the
        // caller's MMIO write.
        desc.write32(
            o + 8,
            len | TX_DTYP_DATA | TX_EOP | TX_IFCS | TX_RS | TX_DEXT,
        );
        self.tail = (i + 1) % self.n;
        self.used += 1;
        Some(self.tail)
    }
}
