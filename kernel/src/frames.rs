// SPDX-License-Identifier: Apache-2.0
//! Frame source handle used by page-table code; backed by the global [`crate::pmm`].

use crate::pmm;

pub const PAGE_SIZE: u64 = pmm::FRAME_SIZE;

/// Zero-sized handle that allocates zeroed frames from the physical memory manager.
pub struct Frames;

impl Frames {
    /// Allocates one zeroed 4 KiB frame and returns its physical address.
    pub fn alloc(&mut self) -> Option<u64> {
        pmm::alloc_zeroed()
    }
}
