//! PCI configuration-space decoding: headers, capabilities, BARs, MSI/MSI-X.
//!
//! Devices are untrusted: every value read from configuration space is validated, capability
//! lists are loop- and length-bounded, and BAR decoding never overflows.

use alloc::vec::Vec;

/// Read access to one function's configuration space (32-bit, dword-aligned offsets).
pub trait Config {
    fn read32(&self, offset: u16) -> u32;
}

impl Config for [u8] {
    /// A raw byte image of configuration space (used by tests and fuzzers); out-of-range reads give all-ones.
    fn read32(&self, offset: u16) -> u32 {
        let o = usize::from(offset & !3);
        self.get(o..o + 4)
            .map_or(u32::MAX, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
}

fn read16(c: &(impl Config + ?Sized), off: u16) -> u16 {
    (c.read32(off & !3) >> (8 * u32::from(off & 2))) as u16
}

fn read8(c: &(impl Config + ?Sized), off: u16) -> u8 {
    (c.read32(off & !3) >> (8 * u32::from(off & 3))) as u8
}

pub const VENDOR_NONE: u16 = 0xFFFF;
pub const CMD_IO: u16 = 1 << 0;
pub const CMD_MEMORY: u16 = 1 << 1;
pub const CMD_BUS_MASTER: u16 = 1 << 2;
pub const CMD_INTX_DISABLE: u16 = 1 << 10;
pub const STATUS_CAP_LIST: u16 = 1 << 4;

/// Decoded type-0/type-1 header fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub vendor: u16,
    pub device: u16,
    pub command: u16,
    pub status: u16,
    pub revision: u8,
    pub prog_if: u8,
    pub subclass: u8,
    pub class: u8,
    /// Header layout: 0 = endpoint, 1 = PCI-PCI bridge, 2 = CardBus.
    pub kind: u8,
    pub multifunction: bool,
    pub subsystem_vendor: u16,
    pub subsystem: u16,
    pub interrupt_line: u8,
    pub interrupt_pin: u8,
    /// Bridge only: bus numbers behind this bridge.
    pub secondary_bus: u8,
    pub subordinate_bus: u8,
    pub cap_pointer: u8,
}

/// Decodes the header; `None` if no device responds.
pub fn header(c: &(impl Config + ?Sized)) -> Option<Header> {
    let id = c.read32(0);
    let vendor = id as u16;
    if vendor == VENDOR_NONE {
        return None;
    }
    let cmd_status = c.read32(4);
    let class_rev = c.read32(8);
    let htype = read8(c, 0x0E);
    let kind = htype & 0x7F;
    let (subsystem_vendor, subsystem) = if kind == 0 {
        let s = c.read32(0x2C);
        (s as u16, (s >> 16) as u16)
    } else {
        (0, 0)
    };
    let (secondary_bus, subordinate_bus) = if kind == 1 {
        (read8(c, 0x19), read8(c, 0x1A))
    } else {
        (0, 0)
    };
    Some(Header {
        vendor,
        device: (id >> 16) as u16,
        command: cmd_status as u16,
        status: (cmd_status >> 16) as u16,
        revision: class_rev as u8,
        prog_if: (class_rev >> 8) as u8,
        subclass: (class_rev >> 16) as u8,
        class: (class_rev >> 24) as u8,
        kind,
        multifunction: htype & 0x80 != 0,
        subsystem_vendor,
        subsystem,
        interrupt_line: read8(c, 0x3C),
        interrupt_pin: read8(c, 0x3D),
        secondary_bus,
        subordinate_bus,
        cap_pointer: if kind == 2 {
            read8(c, 0x14)
        } else {
            read8(c, 0x34)
        } & 0xFC,
    })
}

/// One entry of the capability list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    pub id: u8,
    pub offset: u8,
}

pub const CAP_MSI: u8 = 0x05;
pub const CAP_PCIE: u8 = 0x10;
pub const CAP_MSIX: u8 = 0x11;

/// Walks the capability list. Bounded: at most 48 entries, offsets must lie in 0x40..=0xFC,
/// and a pointer that revisits an offset ends the walk (hostile devices can form cycles).
pub fn capabilities(c: &(impl Config + ?Sized), h: &Header) -> Vec<Capability> {
    let mut out = Vec::new();
    if h.status & STATUS_CAP_LIST == 0 {
        return out;
    }
    let mut seen = [0u64; 4];
    let mut ptr = u16::from(h.cap_pointer);
    for _ in 0..48 {
        if !(0x40..=0xFC).contains(&ptr) {
            break;
        }
        let (word, bit) = (usize::from(ptr / 4 / 64), (ptr / 4) % 64);
        if seen[word] >> bit & 1 == 1 {
            break;
        }
        seen[word] |= 1 << bit;
        let hdr = c.read32(ptr);
        out.push(Capability {
            id: hdr as u8,
            offset: ptr as u8,
        });
        ptr = u16::from((hdr >> 8) as u8 & 0xFC);
    }
    out
}

/// A decoded base address register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarKind {
    Io,
    Mem32,
    Mem64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    pub kind: BarKind,
    pub prefetchable: bool,
    pub base: u64,
    pub size: u64,
}

/// Decodes the BAR starting at slot `i` from the raw values read before (`orig`) and after
/// (`probed`) writing all-ones to the register(s). Returns the BAR and how many slots it uses.
pub fn decode_bar(orig: &[u32], probed: &[u32], i: usize) -> Option<(Bar, usize)> {
    let (&o, &p) = (orig.get(i)?, probed.get(i)?);
    if p == 0 || p == u32::MAX && o == u32::MAX {
        return None; // not implemented
    }
    if o & 1 == 1 {
        // I/O space: bits 31:2 are the address.
        let mask = p & !3;
        if mask == 0 {
            return None;
        }
        // Devices implementing only 16 address bits read the upper half back as zero.
        let size = if mask >> 16 == 0 {
            u64::from(!mask & 0xFFFF) + 1
        } else {
            u64::from((!mask).wrapping_add(1))
        };
        if size == 0 {
            return None;
        }
        return Some((
            Bar {
                kind: BarKind::Io,
                prefetchable: false,
                base: u64::from(o & !3),
                size,
            },
            1,
        ));
    }
    let prefetchable = o & 8 != 0;
    match (o >> 1) & 3 {
        0 => {
            let mask = p & !0xF;
            let size = u64::from((!mask).wrapping_add(1));
            if size == 0 || !size.is_power_of_two() {
                return None;
            }
            Some((
                Bar {
                    kind: BarKind::Mem32,
                    prefetchable,
                    base: u64::from(o & !0xF),
                    size,
                },
                1,
            ))
        }
        2 => {
            let (&o_hi, &p_hi) = (orig.get(i + 1)?, probed.get(i + 1)?);
            let mask = (u64::from(p_hi) << 32) | u64::from(p & !0xF);
            let size = (!mask).wrapping_add(1);
            if size == 0 || !size.is_power_of_two() {
                return None;
            }
            let base = (u64::from(o_hi) << 32) | u64::from(o & !0xF);
            Some((
                Bar {
                    kind: BarKind::Mem64,
                    prefetchable,
                    base,
                    size,
                },
                2,
            ))
        }
        _ => None, // reserved encoding
    }
}

/// Decoded MSI capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Msi {
    pub offset: u8,
    pub is_64bit: bool,
    pub per_vector_mask: bool,
    /// log2 of the number of vectors the function can use.
    pub max_vectors_log2: u8,
}

pub fn msi(c: &(impl Config + ?Sized), cap: Capability) -> Option<Msi> {
    if cap.id != CAP_MSI {
        return None;
    }
    let ctl = read16(c, u16::from(cap.offset) + 2);
    Some(Msi {
        offset: cap.offset,
        is_64bit: ctl & (1 << 7) != 0,
        per_vector_mask: ctl & (1 << 8) != 0,
        max_vectors_log2: ((ctl >> 1) & 7) as u8,
    })
}

/// Decoded MSI-X capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Msix {
    pub offset: u8,
    pub table_size: u16,
    pub table_bar: u8,
    pub table_offset: u32,
    pub pba_bar: u8,
    pub pba_offset: u32,
}

/// Size in bytes of one MSI-X table entry.
pub const MSIX_ENTRY_SIZE: u32 = 16;

pub fn msix(c: &(impl Config + ?Sized), cap: Capability) -> Option<Msix> {
    if cap.id != CAP_MSIX {
        return None;
    }
    let o = u16::from(cap.offset);
    let ctl = read16(c, o + 2);
    let (table, pba) = (c.read32(o + 4), c.read32(o + 8));
    Some(Msix {
        offset: cap.offset,
        table_size: (ctl & 0x7FF) + 1,
        table_bar: (table & 7) as u8,
        table_offset: table & !7,
        pba_bar: (pba & 7) as u8,
        pba_offset: pba & !7,
    })
}

impl Msix {
    /// Byte range `(offset, len)` of the vector table inside its BAR, or `None` if it would overflow.
    pub fn table_range(&self) -> Option<(u32, u32)> {
        let len = u32::from(self.table_size).checked_mul(MSIX_ENTRY_SIZE)?;
        self.table_offset.checked_add(len)?;
        Some((self.table_offset, len))
    }
}
