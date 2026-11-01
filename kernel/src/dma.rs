//! DMA-able memory for device rings, contexts and transfer buffers.
//!
//! Frames come from the PMM's contiguous allocator and are accessed through the HHDM.
//! Buffers are *not* freed automatically: hardware may still be reading or writing them,
//! so releasing one is an explicit decision by the owning driver.

use crate::pmm;

#[derive(Clone, Copy)]
pub struct Dma {
    pub phys: u64,
    pub virt: *mut u8,
    pub len: usize,
}

// SAFETY: a `Dma` is a plain descriptor of RAM that the owning driver serialises access to.
unsafe impl Send for Dma {}

impl Dma {
    /// Allocates `len` bytes (rounded up to whole pages), zeroed, 4 KiB aligned; below 4 GiB
    /// when `below_4g` is set (controllers without 64-bit addressing).
    pub fn alloc(hhdm: u64, len: usize, below_4g: bool) -> Option<Self> {
        let frames = len.div_ceil(4096).max(1) as u64;
        let limit = if below_4g { 1 << 32 } else { u64::MAX };
        let phys = pmm::alloc_contiguous(frames, 1, limit)?;
        Some(Self {
            phys,
            virt: (hhdm + phys) as *mut u8,
            len: frames as usize * 4096,
        })
    }

    /// Returns the memory to the PMM. The caller guarantees the device no longer uses it.
    pub fn free(self) {
        pmm::free_contiguous(self.phys, (self.len / 4096) as u64);
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `virt` points at `len` bytes of HHDM-mapped RAM owned by this buffer.
        unsafe { core::slice::from_raw_parts(self.virt, self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above, and `&mut self` gives exclusive access on the CPU side.
        unsafe { core::slice::from_raw_parts_mut(self.virt, self.len) }
    }

    /// Reads a little-endian `u32` at byte offset `off`.
    pub fn read32(&self, off: usize) -> u32 {
        assert!(off + 4 <= self.len);
        // SAFETY: bounds checked; volatile because the device may modify it.
        unsafe { (self.virt.add(off) as *const u32).read_volatile() }
    }

    pub fn write32(&self, off: usize, v: u32) {
        assert!(off + 4 <= self.len);
        // SAFETY: bounds checked; volatile because the device reads it.
        unsafe { (self.virt.add(off) as *mut u32).write_volatile(v) };
    }

    pub fn write64(&self, off: usize, v: u64) {
        self.write32(off, v as u32);
        self.write32(off + 4, (v >> 32) as u32);
    }
}
