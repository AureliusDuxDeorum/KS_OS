//! 4-level page-table construction for address spaces the kernel builds itself.
//!
//! Stage 1 uses this for the EFI runtime-services address space. The
//! bootloader's higher-half mappings (kernel image, HHDM, framebuffer) are
//! shared by copying PML4 entries 256..512 from the active tables.

use crate::boot::BootInfo;
use crate::frames::BumpFrames;

pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const WRITE_THROUGH: u64 = 1 << 3;
pub const CACHE_DISABLE: u64 = 1 << 4;
pub const NO_EXECUTE: u64 = 1 << 63;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Page permissions for a leaf mapping.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Perm {
    pub write: bool,
    pub exec: bool,
    pub uncached: bool,
}

impl Perm {
    pub const RX: Self = Self {
        write: false,
        exec: true,
        uncached: false,
    };
    pub const RWX: Self = Self {
        write: true,
        exec: true,
        uncached: false,
    };
    pub const RW: Self = Self {
        write: true,
        exec: false,
        uncached: false,
    };
    pub const MMIO: Self = Self {
        write: true,
        exec: false,
        uncached: true,
    };

    fn bits(self, nx_supported: bool) -> u64 {
        let mut f = PRESENT;
        if self.write {
            f |= WRITABLE;
        }
        if !self.exec && nx_supported {
            f |= NO_EXECUTE;
        }
        if self.uncached {
            f |= CACHE_DISABLE | WRITE_THROUGH;
        }
        f
    }
}

#[derive(Debug)]
pub enum MapError {
    OutOfFrames,
    NotCanonical,
    Conflict,
}

pub struct Mapper<'a> {
    boot: &'a BootInfo,
    nx: bool,
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

    /// Allocates an empty top-level table.
    pub fn new_root(&self, frames: &mut BumpFrames<'_>) -> Result<u64, MapError> {
        frames.alloc().ok_or(MapError::OutOfFrames)
    }

    /// Copies the kernel half (PML4 entries 256..512) of `src_root` into `dst_root`.
    pub fn share_kernel_half(&self, dst_root: u64, src_root: u64) {
        for i in 256..512 {
            // SAFETY: both roots are 4 KiB tables reachable through the HHDM.
            unsafe {
                self.table(dst_root)
                    .add(i)
                    .write(self.table(src_root & ADDR_MASK).add(i).read())
            };
        }
    }

    /// Copies a single top-level entry from `src_root` to `dst_root`.
    pub fn share_entry(&self, dst_root: u64, src_root: u64, index: usize) {
        // SAFETY: indices are < 512 and both roots are valid tables.
        unsafe {
            self.table(dst_root)
                .add(index)
                .write(self.table(src_root).add(index).read())
        };
    }

    /// Returns whether top-level entry `index` of `root` is empty.
    pub fn root_entry_empty(&self, root: u64, index: usize) -> bool {
        // SAFETY: index < 512; root is a valid table.
        unsafe { self.table(root).add(index).read() == 0 }
    }

    /// Maps (or re-maps) one 4 KiB page `virt -> phys` in `root`.
    pub fn map_page(
        &self,
        frames: &mut BumpFrames<'_>,
        root: u64,
        virt: u64,
        phys: u64,
        perm: Perm,
    ) -> Result<(), MapError> {
        // Canonical 48-bit address check.
        let top = virt >> 47;
        if top != 0 && top != 0x1_FFFF {
            return Err(MapError::NotCanonical);
        }
        let mut table = root;
        for level in (1..=3).rev() {
            let idx = ((virt >> (12 + 9 * level)) & 0x1FF) as usize;
            // SAFETY: idx < 512 and `table` is a page-table frame.
            let entry = unsafe { self.table(table).add(idx).read() };
            table = if entry & PRESENT != 0 {
                if entry & (1 << 7) != 0 {
                    return Err(MapError::Conflict); // huge page in the way
                }
                entry & ADDR_MASK
            } else {
                let f = frames.alloc().ok_or(MapError::OutOfFrames)?;
                // SAFETY: as above; intermediate entries are permissive, leaves restrict.
                unsafe { self.table(table).add(idx).write(f | PRESENT | WRITABLE) };
                f
            };
        }
        let idx = ((virt >> 12) & 0x1FF) as usize;
        // SAFETY: `table` is the leaf (PT) frame.
        unsafe {
            self.table(table)
                .add(idx)
                .write((phys & ADDR_MASK) | perm.bits(self.nx))
        };
        Ok(())
    }
}

/// Unmaps one 4 KiB page in the *currently active* page tables.
///
/// Fails if the page is covered by a huge page (splitting is not supported yet).
/// Tables shared with other roots (the kernel half) change for all of them.
pub fn unmap_active(boot: &BootInfo, virt: u64) -> Result<(), MapError> {
    let mut table = crate::arch::read_cr3() & ADDR_MASK;
    for level in (1..=3).rev() {
        let idx = ((virt >> (12 + 9 * level)) & 0x1FF) as usize;
        // SAFETY: `table` is a page-table frame reachable through the HHDM.
        let entry = unsafe { (boot.phys_to_virt(table) as *const u64).add(idx).read() };
        if entry & PRESENT == 0 {
            return Ok(()); // nothing mapped here already
        }
        if entry & (1 << 7) != 0 {
            return Err(MapError::Conflict);
        }
        table = entry & ADDR_MASK;
    }
    let idx = ((virt >> 12) & 0x1FF) as usize;
    // SAFETY: `table` is the leaf (PT) frame; clearing the entry unmaps the page.
    unsafe { (boot.phys_to_virt(table) as *mut u64).add(idx).write(0) };
    crate::arch::invlpg(virt);
    Ok(())
}
