// SPDX-License-Identifier: Apache-2.0
//! Kernel heap: size-class slab allocator over the physical memory manager.
//!
//! * Requests up to 2 KiB are served from per-size-class free lists. Each class
//!   carves whole frames into equal blocks (power-of-two sizes, so every block
//!   is naturally aligned to its size).
//! * Larger requests take a physically contiguous run of frames.
//! * All memory is reached through the HHDM, so no separate virtual heap region
//!   (and no page-table updates) is needed.
//!
//! Slab frames are never returned to the PMM (blocks are recycled via the free
//! lists); large allocations are freed immediately. Freed memory is poisoned in
//! debug builds. The lock is interrupt-safe, so interrupt handlers may allocate.

use crate::pmm;
use crate::sync::IrqMutex;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;

const CLASSES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];
const MAX_SLAB: usize = 2048;
const POISON: u8 = 0xDD;

struct Heap {
    hhdm: u64,
    free: [*mut FreeBlock; CLASSES.len()],
    slab_frames: u64,
    large_frames: u64,
    allocs: u64,
    frees: u64,
}

struct FreeBlock {
    next: *mut FreeBlock,
}

// SAFETY: all access goes through the `HEAP` lock.
unsafe impl Send for Heap {}

static HEAP: IrqMutex<Heap> = IrqMutex::new(Heap {
    hhdm: 0,
    free: [null_mut(); CLASSES.len()],
    slab_frames: 0,
    large_frames: 0,
    allocs: 0,
    frees: 0,
});

/// Heap statistics for diagnostics.
#[derive(Clone, Copy, Debug)]
pub struct Stats {
    pub slab_frames: u64,
    pub large_frames: u64,
    pub allocs: u64,
    pub frees: u64,
}

pub struct KernelHeap;

#[global_allocator]
static ALLOCATOR: KernelHeap = KernelHeap;

/// Enables the heap. Requires the PMM to be initialised.
pub fn init(hhdm: u64) {
    HEAP.lock().hhdm = hhdm;
}

pub fn stats() -> Stats {
    let h = HEAP.lock();
    Stats {
        slab_frames: h.slab_frames,
        large_frames: h.large_frames,
        allocs: h.allocs,
        frees: h.frees,
    }
}

fn class_of(layout: Layout) -> Option<usize> {
    let need = layout.size().max(layout.align()).max(CLASSES[0]);
    (need <= MAX_SLAB).then(|| {
        CLASSES
            .iter()
            .position(|&c| c >= need)
            .expect("class exists")
    })
}

impl Heap {
    fn refill(&mut self, class: usize) -> bool {
        let Some(frame) = pmm::alloc_zeroed() else {
            return false;
        };
        self.slab_frames += 1;
        let size = CLASSES[class];
        let base = (self.hhdm + frame) as *mut u8;
        // Thread blocks onto the free list in address order.
        let mut head = self.free[class];
        for i in (0..4096 / size).rev() {
            // SAFETY: `i * size` is within the freshly allocated, HHDM-mapped frame.
            let block = unsafe { base.add(i * size) }.cast::<FreeBlock>();
            // SAFETY: the block is at least 16 bytes and suitably aligned for a pointer.
            unsafe { block.write(FreeBlock { next: head }) };
            head = block;
        }
        self.free[class] = head;
        true
    }

    fn alloc(&mut self, layout: Layout) -> *mut u8 {
        self.allocs += 1;
        if let Some(class) = class_of(layout) {
            if self.free[class].is_null() && !self.refill(class) {
                return null_mut();
            }
            let block = self.free[class];
            // SAFETY: non-null head of this class' free list; it points at a valid FreeBlock.
            self.free[class] = unsafe { (*block).next };
            return block.cast();
        }
        let frames = layout.size().div_ceil(4096) as u64;
        let align_frames = (layout.align() as u64).div_ceil(4096).max(1);
        match pmm::alloc_contiguous(frames, align_frames, u64::MAX) {
            Some(phys) => {
                self.large_frames += frames;
                (self.hhdm + phys) as *mut u8
            }
            None => null_mut(),
        }
    }

    fn dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        self.frees += 1;
        if let Some(class) = class_of(layout) {
            if cfg!(debug_assertions) {
                // SAFETY: the block belongs to us and is `CLASSES[class]` bytes long.
                unsafe { core::ptr::write_bytes(ptr, POISON, CLASSES[class]) };
            }
            let block = ptr.cast::<FreeBlock>();
            // SAFETY: the block is at least pointer-sized and aligned.
            unsafe {
                block.write(FreeBlock {
                    next: self.free[class],
                })
            };
            self.free[class] = block;
            return;
        }
        let frames = layout.size().div_ceil(4096) as u64;
        let phys = ptr as u64 - self.hhdm;
        if cfg!(debug_assertions) {
            // SAFETY: the run of `frames` frames belongs to us.
            unsafe { core::ptr::write_bytes(ptr, POISON, frames as usize * 4096) };
        }
        self.large_frames -= frames;
        pmm::free_contiguous(phys, frames);
    }
}

// SAFETY: `alloc` returns blocks that are unique, correctly sized and aligned (power-of-two
// classes in page-aligned frames, or page-aligned runs), and `dealloc` is only given blocks
// previously returned for the same layout.
unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        HEAP.lock().alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        HEAP.lock().dealloc(ptr, layout);
    }
}
