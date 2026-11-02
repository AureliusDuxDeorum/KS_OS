// SPDX-License-Identifier: Apache-2.0
//! IOMMU building blocks as pure code (ADR-0030 S-14).
//!
//! * [`Domain`] — an I/O virtual address space: a 3- or 4-level page table in either the Intel VT-d
//!   second-level format or the AMD-Vi v1 format, with `map`, `unmap` and a software `translate`.
//! * [`intel`] — root/context entry encoding and the two-level table that selects a domain per PCI function.
//! * [`amd`] — device-table-entry encoding.
//!
//! Table memory is reached through [`TableMem`], so everything runs unchanged against a simulated
//! memory on the host; the kernel implements the trait over frames from its physical allocator.
#![no_std]
#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
#![cfg_attr(
    test,
    allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

extern crate alloc;

pub mod amd;
pub mod intel;

pub const PAGE: u64 = 4096;
const ENTRIES: usize = 512;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Zeroed, page-sized I/O page-table pages and access to their 64-bit entries.
pub trait TableMem {
    /// Allocates one zeroed 4 KiB table; returns its physical address.
    fn alloc_table(&mut self) -> Option<u64>;
    fn free_table(&mut self, phys: u64);
    fn read(&self, table: u64, index: usize) -> u64;
    fn write(&mut self, table: u64, index: usize, value: u64);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Intel VT-d second-level page table: bit0 read, bit1 write, address in bits 12..51.
    IntelSecondLevel,
    /// AMD-Vi v1 table: bit0 present, bits 9..11 next level, bit61 read, bit62 write.
    AmdV1,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    OutOfMemory,
    /// Address or length not page aligned, zero, or beyond the address width of the table.
    Range,
    /// An existing mapping would be replaced.
    Overlap,
}

#[derive(Debug)]
pub struct Domain {
    pub format: Format,
    pub levels: u8,
    pub root: u64,
}

fn index(iova: u64, level: u8) -> usize {
    // level 1 uses bits 12..20, level 2 bits 21..29, ...
    let shift = 12u32.saturating_add(9u32.saturating_mul(u32::from(level).saturating_sub(1)));
    ((iova.checked_shr(shift).unwrap_or(0)) & 0x1FF) as usize
}

impl Domain {
    pub fn new(mem: &mut impl TableMem, format: Format, levels: u8) -> Result<Self, Error> {
        if !(3..=4).contains(&levels) {
            return Err(Error::Range);
        }
        let root = mem.alloc_table().ok_or(Error::OutOfMemory)?;
        Ok(Self {
            format,
            levels,
            root,
        })
    }

    /// Exclusive upper bound of the IOVA space.
    pub fn limit(&self) -> u64 {
        1u64.checked_shl(12u32.saturating_add(9u32.saturating_mul(u32::from(self.levels))))
            .unwrap_or(u64::MAX)
    }

    fn table_entry(&self, next_table: u64, child_level: u8) -> u64 {
        match self.format {
            Format::IntelSecondLevel => (next_table & ADDR_MASK) | 0b11,
            Format::AmdV1 => {
                (next_table & ADDR_MASK) | 1 | (u64::from(child_level) << 9) | (1 << 61) | (1 << 62)
            }
        }
    }

    fn leaf_entry(&self, phys: u64, write: bool) -> u64 {
        match self.format {
            Format::IntelSecondLevel => (phys & ADDR_MASK) | 1 | (u64::from(write) << 1),
            Format::AmdV1 => (phys & ADDR_MASK) | 1 | (1 << 61) | (u64::from(write) << 62),
        }
    }

    fn present(&self, entry: u64) -> bool {
        match self.format {
            Format::IntelSecondLevel => entry & 0b11 != 0,
            Format::AmdV1 => entry & 1 != 0,
        }
    }

    /// Maps `len` bytes at `iova` to `phys` (all page aligned). Existing mappings are never replaced.
    pub fn map(
        &mut self,
        mem: &mut impl TableMem,
        iova: u64,
        phys: u64,
        len: u64,
        write: bool,
    ) -> Result<(), Error> {
        let end = iova.checked_add(len).ok_or(Error::Range)?;
        if len == 0
            || !iova.is_multiple_of(PAGE)
            || !phys.is_multiple_of(PAGE)
            || !len.is_multiple_of(PAGE)
            || end > self.limit()
        {
            return Err(Error::Range);
        }
        phys.checked_add(len).ok_or(Error::Range)?;
        let mut off = 0u64;
        while off < len {
            self.map_page(
                mem,
                iova.saturating_add(off),
                phys.saturating_add(off),
                write,
            )?;
            off = off.saturating_add(PAGE);
        }
        Ok(())
    }

    fn map_page(
        &mut self,
        mem: &mut impl TableMem,
        iova: u64,
        phys: u64,
        write: bool,
    ) -> Result<(), Error> {
        let mut table = self.root;
        let mut level = self.levels;
        while level > 1 {
            let i = index(iova, level);
            let e = mem.read(table, i);
            table = if self.present(e) {
                e & ADDR_MASK
            } else {
                let next = mem.alloc_table().ok_or(Error::OutOfMemory)?;
                mem.write(table, i, self.table_entry(next, level.saturating_sub(1)));
                next
            };
            level = level.saturating_sub(1);
        }
        let i = index(iova, 1);
        if self.present(mem.read(table, i)) {
            return Err(Error::Overlap);
        }
        mem.write(table, i, self.leaf_entry(phys, write));
        Ok(())
    }

    /// Software page walk: the physical address and write permission `iova` translates to.
    pub fn translate(&self, mem: &impl TableMem, iova: u64) -> Option<(u64, bool)> {
        if iova >= self.limit() {
            return None;
        }
        let mut table = self.root;
        let mut level = self.levels;
        while level > 1 {
            let e = mem.read(table, index(iova, level));
            if !self.present(e) {
                return None;
            }
            table = e & ADDR_MASK;
            level = level.saturating_sub(1);
        }
        let e = mem.read(table, index(iova, 1));
        if !self.present(e) {
            return None;
        }
        let writable = match self.format {
            Format::IntelSecondLevel => e & 2 != 0,
            Format::AmdV1 => e & (1 << 62) != 0,
        };
        Some(((e & ADDR_MASK) | (iova & (PAGE - 1)), writable))
    }

    /// Removes the leaf mappings covering `[iova, iova + len)`. Intermediate tables are kept until `destroy`.
    pub fn unmap(&mut self, mem: &mut impl TableMem, iova: u64, len: u64) {
        let mut off = 0u64;
        while off < len {
            let va = iova.saturating_add(off);
            let mut table = self.root;
            let mut level = self.levels;
            let mut ok = va < self.limit();
            while ok && level > 1 {
                let e = mem.read(table, index(va, level));
                if self.present(e) {
                    table = e & ADDR_MASK;
                    level = level.saturating_sub(1);
                } else {
                    ok = false;
                }
            }
            if ok {
                mem.write(table, index(va, 1), 0);
            }
            off = off.saturating_add(PAGE);
        }
    }

    /// Frees every table of the domain.
    pub fn destroy(self, mem: &mut impl TableMem) {
        fn free(d: &Domain, mem: &mut impl TableMem, table: u64, level: u8) {
            if level > 1 {
                for i in 0..ENTRIES {
                    let e = mem.read(table, i);
                    if d.present(e) {
                        free(d, mem, e & ADDR_MASK, level.saturating_sub(1));
                    }
                }
            }
            mem.free_table(table);
        }
        free(&self, mem, self.root, self.levels);
    }
}
