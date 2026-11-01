//! Physical memory manager: a bitmap frame allocator.
//!
//! One bit per 4 KiB frame (1 = in use). Only memory the bootloader reports as
//! `USABLE` and that lies above 1 MiB is ever handed out; everything else
//! (firmware, ACPI, bootloader data still referenced by pointers we hold, the
//! kernel image) stays marked in-use. Supports single frames and physically
//! contiguous, aligned runs below an address limit (for DMA).
//!
//! Locking: a spin lock. Allocation from interrupt context must not happen
//! until the lock becomes IRQ-safe (tracked in the interrupts phase).

use crate::boot::BootInfo;
use crate::limine;
use spin::Mutex;

pub const FRAME_SIZE: u64 = 4096;
const FIRST_MANAGED: u64 = 0x10_0000; // never hand out the first MiB

struct Pmm {
    bitmap: *mut u64,
    words: usize,
    frames: u64,
    free: u64,
    hint: usize,
    hhdm: u64,
}

// SAFETY: the bitmap is only touched under the `PMM` lock.
unsafe impl Send for Pmm {}

static PMM: Mutex<Option<Pmm>> = Mutex::new(None);

/// Allocator statistics.
#[derive(Clone, Copy, Debug)]
pub struct Stats {
    pub total_frames: u64,
    pub free_frames: u64,
}

impl Pmm {
    #[inline]
    fn test(&self, f: u64) -> bool {
        // SAFETY: `f < self.frames` is guaranteed by callers; the word index is in range.
        unsafe { self.bitmap.add((f / 64) as usize).read() >> (f % 64) & 1 == 1 }
    }

    #[inline]
    fn set(&mut self, f: u64, used: bool) {
        // SAFETY: as in `test`.
        unsafe {
            let w = self.bitmap.add((f / 64) as usize);
            let mask = 1u64 << (f % 64);
            w.write(if used {
                w.read() | mask
            } else {
                w.read() & !mask
            });
        }
    }

    fn alloc_one(&mut self) -> Option<u64> {
        for i in 0..self.words {
            let idx = (self.hint + i) % self.words;
            // SAFETY: idx < words.
            let w = unsafe { self.bitmap.add(idx).read() };
            if w != u64::MAX {
                let bit = (!w).trailing_zeros() as u64;
                let f = idx as u64 * 64 + bit;
                if f >= self.frames {
                    continue;
                }
                self.set(f, true);
                self.free -= 1;
                self.hint = idx;
                return Some(f);
            }
        }
        None
    }

    /// Finds `count` contiguous free frames, first frame aligned to `align` frames,
    /// all below `limit_frame` (exclusive).
    fn alloc_run(&mut self, count: u64, align: u64, limit_frame: u64) -> Option<u64> {
        let limit = limit_frame.min(self.frames);
        let align = align.max(1);
        let mut start = 0u64;
        'search: while start + count <= limit {
            let aligned = start.next_multiple_of(align);
            if aligned != start {
                start = aligned;
                continue;
            }
            // Fast skip over fully-used words.
            if start.is_multiple_of(64) {
                // SAFETY: start/64 < words because start < frames.
                if unsafe { self.bitmap.add((start / 64) as usize).read() } == u64::MAX {
                    start += 64;
                    continue;
                }
            }
            for k in 0..count {
                if self.test(start + k) {
                    start += k + 1;
                    continue 'search;
                }
            }
            for k in 0..count {
                self.set(start + k, true);
            }
            self.free -= count;
            return Some(start);
        }
        None
    }
}

/// Initialises the allocator from the bootloader memory map.
pub fn init(boot: &BootInfo) {
    let max_end = boot
        .memmap()
        .filter(|e| e.kind == limine::MEMMAP_USABLE)
        .map(|e| e.base + e.length)
        .max()
        .expect("no usable memory");
    let frames = max_end / FRAME_SIZE;
    let words = frames.div_ceil(64) as usize;
    let bitmap_bytes = (words * 8) as u64;
    let bitmap_frames = bitmap_bytes.div_ceil(FRAME_SIZE);

    // Place the bitmap at the start of the first usable region (above 1 MiB) that fits it.
    let place = boot
        .memmap()
        .filter(|e| e.kind == limine::MEMMAP_USABLE)
        .filter_map(|e| {
            let start = e.base.max(FIRST_MANAGED).next_multiple_of(FRAME_SIZE);
            let end = (e.base + e.length) & !(FRAME_SIZE - 1);
            (start < end && end - start >= bitmap_frames * FRAME_SIZE).then_some(start)
        })
        .next()
        .expect("no region large enough for the frame bitmap");

    let bitmap = boot.phys_to_virt(place) as *mut u64;
    // SAFETY: the region is usable RAM mapped in the HHDM; we mark everything used first.
    unsafe { core::ptr::write_bytes(bitmap, 0xFF, words) };
    let mut pmm = Pmm {
        bitmap,
        words,
        frames,
        free: 0,
        hint: 0,
        hhdm: boot.hhdm,
    };

    for e in boot.memmap().filter(|e| e.kind == limine::MEMMAP_USABLE) {
        let start = e.base.max(FIRST_MANAGED).next_multiple_of(FRAME_SIZE);
        let end = (e.base + e.length) & !(FRAME_SIZE - 1);
        let mut a = start;
        while a < end {
            pmm.set(a / FRAME_SIZE, false);
            pmm.free += 1;
            a += FRAME_SIZE;
        }
    }
    for f in 0..bitmap_frames {
        pmm.set(place / FRAME_SIZE + f, true);
        pmm.free -= 1;
    }
    *PMM.lock() = Some(pmm);
}

fn with<R>(f: impl FnOnce(&mut Pmm) -> R) -> R {
    f(PMM.lock().as_mut().expect("pmm not initialised"))
}

/// Allocates one frame (contents unspecified).
pub fn alloc() -> Option<u64> {
    with(|p| p.alloc_one()).map(|f| f * FRAME_SIZE)
}

/// Allocates one zero-filled frame.
pub fn alloc_zeroed() -> Option<u64> {
    let phys = alloc()?;
    zero(phys, 1);
    Some(phys)
}

/// Allocates `count` physically contiguous frames whose start is aligned to
/// `align_frames` and whose end lies at or below `max_phys` (exclusive); zero-filled.
pub fn alloc_contiguous(count: u64, align_frames: u64, max_phys: u64) -> Option<u64> {
    let limit = max_phys / FRAME_SIZE;
    let f = with(|p| p.alloc_run(count, align_frames, limit))?;
    let phys = f * FRAME_SIZE;
    zero(phys, count);
    Some(phys)
}

fn zero(phys: u64, count: u64) {
    let hhdm = with(|p| p.hhdm);
    // SAFETY: the frames were just allocated to us and are HHDM-mapped RAM.
    unsafe { core::ptr::write_bytes((hhdm + phys) as *mut u8, 0, (count * FRAME_SIZE) as usize) };
}

/// Frees one frame. Panics on a double free or an unmanaged address.
pub fn free(phys: u64) {
    free_contiguous(phys, 1);
}

/// Frees `count` frames starting at `phys`.
pub fn free_contiguous(phys: u64, count: u64) {
    with(|p| {
        assert!(
            phys.is_multiple_of(FRAME_SIZE),
            "pmm: unaligned free {phys:#x}"
        );
        for k in 0..count {
            let f = phys / FRAME_SIZE + k;
            assert!(
                f < p.frames,
                "pmm: free of unmanaged frame {:#x}",
                f * FRAME_SIZE
            );
            assert!(p.test(f), "pmm: double free of frame {:#x}", f * FRAME_SIZE);
            p.set(f, false);
        }
        p.free += count;
    });
}

pub fn stats() -> Stats {
    with(|p| Stats {
        total_frames: p.frames,
        free_frames: p.free,
    })
}
