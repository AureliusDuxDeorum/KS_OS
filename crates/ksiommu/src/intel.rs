// SPDX-License-Identifier: Apache-2.0
//! Intel VT-d translation structures (Intel VT-d spec, section 9): root entries, context entries and
//! the table that selects a domain per PCI bus/device/function. Entries are 128 bits (two `u64`).

use crate::{Domain, Error, Format, TableMem, ADDR_MASK};

/// Translation type 00b: translate with the second-level page table.
const T_TRANSLATE: u64 = 0;

/// Root entry: present bit + pointer to the context table of one bus.
pub fn root_entry(context_table: u64) -> [u64; 2] {
    [(context_table & ADDR_MASK) | 1, 0]
}

/// Address-width field for a table with `levels` levels (3 -> 39-bit, 4 -> 48-bit).
pub fn address_width_field(levels: u8) -> Option<u64> {
    match levels {
        3 => Some(1),
        4 => Some(2),
        _ => None,
    }
}

/// Context entry binding a function to a second-level page table and a domain id.
pub fn context_entry(slpt_root: u64, levels: u8, domain_id: u16) -> Option<[u64; 2]> {
    let aw = address_width_field(levels)?;
    Some([
        (slpt_root & ADDR_MASK) | (T_TRANSLATE << 2) | 1,
        aw | (u64::from(domain_id) << 8),
    ])
}

/// Root table (256 entries, one per bus) and lazily created context tables (256 entries per bus,
/// indexed by `devfn`). Absent entries mean "all DMA from this function is blocked".
pub struct Tables {
    pub root: u64,
}

impl Tables {
    pub fn new(mem: &mut impl TableMem) -> Result<Self, Error> {
        Ok(Self {
            root: mem.alloc_table().ok_or(Error::OutOfMemory)?,
        })
    }

    fn context_table(&self, mem: &mut impl TableMem, bus: u8, create: bool) -> Option<u64> {
        let lo = mem.read(self.root, usize::from(bus).saturating_mul(2));
        if lo & 1 != 0 {
            return Some(lo & ADDR_MASK);
        }
        if !create {
            return None;
        }
        let ctx = mem.alloc_table()?;
        let e = root_entry(ctx);
        mem.write(
            self.root,
            usize::from(bus).saturating_mul(2).saturating_add(1),
            e[1],
        );
        mem.write(self.root, usize::from(bus).saturating_mul(2), e[0]); // present bit last
        Some(ctx)
    }

    /// Lets `bus:devfn` DMA through `domain`.
    pub fn attach(
        &self,
        mem: &mut impl TableMem,
        bus: u8,
        devfn: u8,
        domain: &Domain,
        domain_id: u16,
    ) -> Result<(), Error> {
        if domain.format != Format::IntelSecondLevel {
            return Err(Error::Range);
        }
        let ctx = self
            .context_table(mem, bus, true)
            .ok_or(Error::OutOfMemory)?;
        let e = context_entry(domain.root, domain.levels, domain_id).ok_or(Error::Range)?;
        let i = usize::from(devfn).saturating_mul(2);
        mem.write(ctx, i.saturating_add(1), e[1]);
        mem.write(ctx, i, e[0]); // present bit last
        Ok(())
    }

    /// Blocks all DMA from `bus:devfn` again.
    pub fn detach(&self, mem: &mut impl TableMem, bus: u8, devfn: u8) {
        if let Some(ctx) = self.context_table(mem, bus, false) {
            let i = usize::from(devfn).saturating_mul(2);
            mem.write(ctx, i, 0);
            mem.write(ctx, i.saturating_add(1), 0);
        }
    }

    /// Software model of the hardware lookup: the domain root/levels/id a function is bound to.
    pub fn lookup(&self, mem: &mut impl TableMem, bus: u8, devfn: u8) -> Option<(u64, u8, u16)> {
        let ctx = self.context_table(mem, bus, false)?;
        let i = usize::from(devfn).saturating_mul(2);
        let (lo, hi) = (mem.read(ctx, i), mem.read(ctx, i.saturating_add(1)));
        if lo & 1 == 0 {
            return None;
        }
        let levels = match hi & 7 {
            1 => 3,
            2 => 4,
            _ => return None,
        };
        Some((lo & ADDR_MASK, levels, ((hi >> 8) & 0xFFFF) as u16))
    }
}
