//! Early physical frame allocator: a bump allocator over usable memory.
//!
//! Frames are never freed. This is sufficient for the handful of page-table
//! and scratch pages Stage 1 needs; a real frame allocator replaces it in the
//! memory-management phase (see ROADMAP).

use crate::boot::BootInfo;
use crate::limine;

pub const PAGE_SIZE: u64 = 4096;

pub struct BumpFrames<'a> {
    boot: &'a BootInfo,
    next: u64,
    allocated: usize,
}

impl<'a> BumpFrames<'a> {
    pub fn new(boot: &'a BootInfo) -> Self {
        Self {
            boot,
            next: 0x10_0000,
            allocated: 0,
        } // never hand out the first MiB
    }

    pub fn allocated(&self) -> usize {
        self.allocated
    }

    /// Allocates one zeroed 4 KiB frame and returns its physical address.
    pub fn alloc(&mut self) -> Option<u64> {
        // Lowest usable page at or after `next`.
        let mut best: Option<u64> = None;
        for e in self.boot.memmap() {
            if e.kind != limine::MEMMAP_USABLE {
                continue;
            }
            let start = e.base.max(self.next).next_multiple_of(PAGE_SIZE);
            let end = (e.base + e.length) & !(PAGE_SIZE - 1);
            if start + PAGE_SIZE <= end && best.is_none_or(|s| start < s) {
                best = Some(start);
            }
        }
        let start = best?;
        self.next = start + PAGE_SIZE;
        self.allocated += 1;
        // SAFETY: usable RAM is mapped in the HHDM and exclusively ours.
        unsafe {
            core::ptr::write_bytes(
                self.boot.phys_to_virt(start) as *mut u8,
                0,
                PAGE_SIZE as usize,
            )
        };
        Some(start)
    }
}
