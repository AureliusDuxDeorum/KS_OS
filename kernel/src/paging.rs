// SPDX-License-Identifier: Apache-2.0
//! 4-level page-table construction and inspection.
//!
//! Used for the kernel's own address space ([`crate::vmm`]) and the EFI
//! runtime-services address space ([`crate::efi`]). Supports 4 KiB and 2 MiB
//! leaves, per-mapping permissions and cache attributes (via the PAT, see
//! [`crate::vmm`]), unmapping, lookup, and a W^X audit.

use crate::boot::BootInfo;
use crate::frames::Frames;

pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const WRITE_THROUGH: u64 = 1 << 3;
pub const CACHE_DISABLE: u64 = 1 << 4;
pub const HUGE: u64 = 1 << 7;
pub const NO_EXECUTE: u64 = 1 << 63;
const PAT_4K: u64 = 1 << 7;
const PAT_HUGE: u64 = 1 << 12;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const ADDR_MASK_2M: u64 = 0x000F_FFFF_FFE0_0000;
const SIZE_2M: u64 = 2 * 1024 * 1024;

/// Memory type of a mapping. The PAT is programmed so that these map to
/// entries 0 (WB), 5 (WC) and 3 (UC).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cache {
    WriteBack,
    WriteCombining,
    Uncached,
}

/// Page permissions for a leaf mapping.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Perm {
    pub write: bool,
    pub exec: bool,
    pub cache: Cache,
}

impl Perm {
    pub const RX: Self = Self {
        write: false,
        exec: true,
        cache: Cache::WriteBack,
    };
    pub const RWX: Self = Self {
        write: true,
        exec: true,
        cache: Cache::WriteBack,
    };
    pub const RW: Self = Self {
        write: true,
        exec: false,
        cache: Cache::WriteBack,
    };
    pub const R: Self = Self {
        write: false,
        exec: false,
        cache: Cache::WriteBack,
    };
    pub const MMIO: Self = Self {
        write: true,
        exec: false,
        cache: Cache::Uncached,
    };
    pub const FRAMEBUFFER: Self = Self {
        write: true,
        exec: false,
        cache: Cache::WriteCombining,
    };

    fn bits(self, nx_supported: bool, huge: bool) -> u64 {
        let mut f = PRESENT;
        if self.write {
            f |= WRITABLE;
        }
        if !self.exec && nx_supported {
            f |= NO_EXECUTE;
        }
        match self.cache {
            Cache::WriteBack => {}
            Cache::Uncached => f |= CACHE_DISABLE | WRITE_THROUGH,
            Cache::WriteCombining => f |= WRITE_THROUGH | if huge { PAT_HUGE } else { PAT_4K },
        }
        if huge {
            f |= HUGE;
        }
        f
    }
}

#[derive(Debug)]
pub enum MapError {
    OutOfFrames,
    NotCanonical,
    Conflict,
    Unaligned,
}

/// A virtual range and the physical range it maps.
#[derive(Clone, Copy)]
pub struct Span {
    pub virt: u64,
    pub phys: u64,
    pub len: u64,
}

/// A resolved leaf mapping.
#[derive(Clone, Copy, Debug)]
pub struct Leaf {
    pub phys: u64,
    pub flags: u64,
    pub size: u64,
}

impl Leaf {
    pub fn writable(&self) -> bool {
        self.flags & WRITABLE != 0
    }
    pub fn executable(&self) -> bool {
        self.flags & NO_EXECUTE == 0
    }
}

pub struct Mapper<'a> {
    boot: &'a BootInfo,
    nx: bool,
}

fn canonical(virt: u64) -> bool {
    let top = virt >> 47;
    top == 0 || top == 0x1_FFFF
}

impl<'a> Mapper<'a> {
    pub fn new(boot: &'a BootInfo, nx_supported: bool) -> Self {
        Self {
            boot,
            nx: nx_supported,
        }
    }

    fn table(&self, phys: u64) -> *mut u64 {
        self.boot.phys_to_virt(phys) as *mut u64
    }

    fn read(&self, table: u64, idx: usize) -> u64 {
        // SAFETY: `table` is a page-table frame reachable through the HHDM and idx < 512.
        unsafe { self.table(table).add(idx).read() }
    }

    fn write(&self, table: u64, idx: usize, v: u64) {
        // SAFETY: as in `read`.
        unsafe { self.table(table).add(idx).write(v) };
    }

    /// Allocates an empty top-level table.
    pub fn new_root(&self, frames: &mut Frames) -> Result<u64, MapError> {
        frames.alloc().ok_or(MapError::OutOfFrames)
    }

    /// Copies the kernel half (PML4 entries 256..512) of `src_root` into `dst_root`.
    pub fn share_kernel_half(&self, dst_root: u64, src_root: u64) {
        for i in 256..512 {
            self.write(dst_root, i, self.read(src_root & ADDR_MASK, i));
        }
    }

    /// Copies a single top-level entry from `src_root` to `dst_root`.
    pub fn share_entry(&self, dst_root: u64, src_root: u64, index: usize) {
        self.write(dst_root, index, self.read(src_root, index));
    }

    /// Returns whether top-level entry `index` of `root` is empty.
    pub fn root_entry_empty(&self, root: u64, index: usize) -> bool {
        self.read(root, index) == 0
    }

    /// Walks to the table that holds the leaf for `virt` at `leaf_level`
    /// (1 = 4 KiB PT, 2 = 2 MiB PD), allocating intermediate tables.
    fn descend(
        &self,
        frames: &mut Frames,
        root: u64,
        virt: u64,
        leaf_level: usize,
    ) -> Result<u64, MapError> {
        let mut table = root;
        for level in (leaf_level + 1..=4).rev() {
            let idx = ((virt >> (12 + 9 * (level - 1))) & 0x1FF) as usize;
            let entry = self.read(table, idx);
            table = if entry & PRESENT != 0 {
                if entry & HUGE != 0 {
                    return Err(MapError::Conflict);
                }
                entry & ADDR_MASK
            } else {
                let f = frames.alloc().ok_or(MapError::OutOfFrames)?;
                // Intermediate entries are permissive; leaves carry the real restrictions.
                self.write(table, idx, f | PRESENT | WRITABLE);
                f
            };
        }
        Ok(table)
    }

    /// Maps (or re-maps) one 4 KiB page `virt -> phys` in `root`.
    pub fn map_page(
        &self,
        frames: &mut Frames,
        root: u64,
        virt: u64,
        phys: u64,
        perm: Perm,
    ) -> Result<(), MapError> {
        if !canonical(virt) {
            return Err(MapError::NotCanonical);
        }
        let pt = self.descend(frames, root, virt, 1)?;
        self.write(
            pt,
            ((virt >> 12) & 0x1FF) as usize,
            (phys & ADDR_MASK) | perm.bits(self.nx, false),
        );
        Ok(())
    }

    /// Maps one 2 MiB page.
    pub fn map_huge(
        &self,
        frames: &mut Frames,
        root: u64,
        virt: u64,
        phys: u64,
        perm: Perm,
    ) -> Result<(), MapError> {
        if !canonical(virt) {
            return Err(MapError::NotCanonical);
        }
        if !virt.is_multiple_of(SIZE_2M) || !phys.is_multiple_of(SIZE_2M) {
            return Err(MapError::Unaligned);
        }
        let pd = self.descend(frames, root, virt, 2)?;
        let idx = ((virt >> 21) & 0x1FF) as usize;
        let existing = self.read(pd, idx);
        if existing & PRESENT != 0 && existing & HUGE == 0 {
            return Err(MapError::Conflict); // a page table already lives here
        }
        self.write(pd, idx, (phys & ADDR_MASK_2M) | perm.bits(self.nx, true));
        Ok(())
    }

    /// Maps `[phys, phys+len)` at `virt`, using 2 MiB pages where alignment allows
    /// and `allow_huge` is set.
    pub fn map_range(
        &self,
        frames: &mut Frames,
        root: u64,
        span: Span,
        perm: Perm,
        allow_huge: bool,
    ) -> Result<(), MapError> {
        let Span { virt, phys, len } = span;
        if !virt.is_multiple_of(4096) || !phys.is_multiple_of(4096) {
            return Err(MapError::Unaligned);
        }
        let len = len.next_multiple_of(4096);
        let mut off = 0;
        while off < len {
            let (v, p) = (virt + off, phys + off);
            if allow_huge
                && v.is_multiple_of(SIZE_2M)
                && p.is_multiple_of(SIZE_2M)
                && len - off >= SIZE_2M
            {
                self.map_huge(frames, root, v, p, perm)?;
                off += SIZE_2M;
            } else {
                self.map_page(frames, root, v, p, perm)?;
                off += 4096;
            }
        }
        Ok(())
    }

    /// Unmaps one 4 KiB page in `root`. Fails if it is covered by a huge page.
    pub fn unmap(&self, root: u64, virt: u64) -> Result<(), MapError> {
        let mut table = root;
        for level in (2..=4).rev() {
            let entry = self.read(table, ((virt >> (12 + 9 * (level - 1))) & 0x1FF) as usize);
            if entry & PRESENT == 0 {
                return Ok(());
            }
            if entry & HUGE != 0 {
                return Err(MapError::Conflict);
            }
            table = entry & ADDR_MASK;
        }
        self.write(table, ((virt >> 12) & 0x1FF) as usize, 0);
        crate::arch::invlpg(virt);
        Ok(())
    }

    /// Resolves `virt` in `root` to its leaf mapping, if any.
    pub fn lookup(&self, root: u64, virt: u64) -> Option<Leaf> {
        let mut table = root;
        for level in (1..=4).rev() {
            let entry = self.read(table, ((virt >> (12 + 9 * (level - 1))) & 0x1FF) as usize);
            if entry & PRESENT == 0 {
                return None;
            }
            if level == 1 {
                return Some(Leaf {
                    phys: entry & ADDR_MASK,
                    flags: entry,
                    size: 4096,
                });
            }
            if entry & HUGE != 0 && level <= 3 {
                let size = if level == 2 { SIZE_2M } else { 1 << 30 };
                return Some(Leaf {
                    phys: entry & ADDR_MASK & !(size - 1),
                    flags: entry,
                    size,
                });
            }
            table = entry & ADDR_MASK;
        }
        None
    }

    /// Lists up to `limit` leaf mappings that are both writable and executable, as
    /// `(virtual address, size)`.
    pub fn find_wx(&self, root: u64, limit: usize) -> alloc::vec::Vec<(u64, u64)> {
        fn walk(
            m: &Mapper<'_>,
            table: u64,
            level: usize,
            base: u64,
            limit: usize,
            out: &mut alloc::vec::Vec<(u64, u64)>,
        ) {
            for idx in 0..512u64 {
                let e = m.read(table, idx as usize);
                if e & PRESENT == 0 || out.len() >= limit {
                    continue;
                }
                let mut virt = base | (idx << (12 + 9 * (level as u64 - 1)));
                if level == 4 && idx >= 256 {
                    virt |= 0xFFFF_0000_0000_0000; // sign-extend into the kernel half
                }
                if level == 1 || (e & HUGE != 0 && level <= 3) {
                    if e & WRITABLE != 0 && e & NO_EXECUTE == 0 {
                        out.push((virt, 4096u64 << (9 * (level as u64 - 1))));
                    }
                } else {
                    walk(m, e & ADDR_MASK, level - 1, virt, limit, out);
                }
            }
        }
        let mut out = alloc::vec::Vec::new();
        walk(self, root, 4, 0, limit, &mut out);
        out
    }

    /// Counts leaf mappings reachable from `root` and how many are both writable and executable.
    pub fn audit_wx(&self, root: u64) -> (u64, u64) {
        fn walk(m: &Mapper<'_>, table: u64, level: usize, counts: &mut (u64, u64)) {
            for idx in 0..512 {
                let e = m.read(table, idx);
                if e & PRESENT == 0 {
                    continue;
                }
                if level == 1 || (e & HUGE != 0 && level <= 3) {
                    counts.0 += 1;
                    if e & WRITABLE != 0 && e & NO_EXECUTE == 0 {
                        counts.1 += 1;
                    }
                } else {
                    walk(m, e & ADDR_MASK, level - 1, counts);
                }
            }
        }
        let mut counts = (0, 0);
        walk(self, root, 4, &mut counts);
        counts
    }
}
