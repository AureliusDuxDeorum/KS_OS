// SPDX-License-Identifier: Apache-2.0
//! Bounds-checked volatile access to a memory-mapped register window.

/// A memory-mapped register window (uncached mapping from `vmm::map_mmio`).
#[derive(Clone, Copy)]
pub struct Mmio {
    base: u64,
    len: u64,
}

impl Mmio {
    pub fn new(base: u64, len: u64) -> Self {
        Self { base, len }
    }
    pub fn r32(&self, off: usize) -> u32 {
        assert!((off as u64) + 4 <= self.len);
        // SAFETY: `base` is an uncached MMIO mapping of at least `len` bytes (checked above).
        unsafe { ((self.base + off as u64) as *const u32).read_volatile() }
    }
    pub fn w32(&self, off: usize, v: u32) {
        assert!((off as u64) + 4 <= self.len);
        // SAFETY: as in `r32`.
        unsafe { ((self.base + off as u64) as *mut u32).write_volatile(v) };
    }
    pub fn r16(&self, off: usize) -> u16 {
        assert!((off as u64) + 2 <= self.len);
        // SAFETY: as in `r32`.
        unsafe { ((self.base + off as u64) as *const u16).read_volatile() }
    }
    pub fn w16(&self, off: usize, v: u16) {
        assert!((off as u64) + 2 <= self.len);
        // SAFETY: as in `r32`.
        unsafe { ((self.base + off as u64) as *mut u16).write_volatile(v) };
    }
    pub fn r8(&self, off: usize) -> u8 {
        assert!((off as u64) < self.len);
        // SAFETY: as in `r32`.
        unsafe { ((self.base + off as u64) as *const u8).read_volatile() }
    }
    pub fn w8(&self, off: usize, v: u8) {
        assert!((off as u64) < self.len);
        // SAFETY: as in `r32`.
        unsafe { ((self.base + off as u64) as *mut u8).write_volatile(v) };
    }
    pub fn r64(&self, off: usize) -> u64 {
        u64::from(self.r32(off)) | (u64::from(self.r32(off.saturating_add(4))) << 32)
    }
    pub fn w64(&self, off: usize, v: u64) {
        self.w32(off, v as u32);
        self.w32(off + 4, (v >> 32) as u32);
    }
}
