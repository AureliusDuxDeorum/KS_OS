// SPDX-License-Identifier: Apache-2.0
//! Access to device-shared memory (descriptor rings, packet buffers).
//!
//! All accesses are word/byte operations through `&self` so an implementation can use volatile
//! reads and writes (the device modifies this memory behind the CPU's back) and so the ring logic
//! stays free of `unsafe`. The kernel implements it for its `Dma` buffers; tests use a `Vec` model.

pub trait DmaMem {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn read32(&self, off: usize) -> u32;
    fn write32(&self, off: usize, v: u32);
    fn read_bytes(&self, off: usize, out: &mut [u8]);
    fn write_bytes(&self, off: usize, data: &[u8]);

    fn read16(&self, off: usize) -> u16 {
        let w = self.read32(off & !3);
        (w >> ((off & 2) * 8)) as u16
    }
    fn write16(&self, off: usize, v: u16) {
        let sh = (off & 2) * 8;
        let w = self.read32(off & !3);
        self.write32(off & !3, (w & !(0xFFFF << sh)) | (u32::from(v) << sh));
    }
    fn read64(&self, off: usize) -> u64 {
        u64::from(self.read32(off)) | (u64::from(self.read32(off + 4)) << 32)
    }
    fn write64(&self, off: usize, v: u64) {
        self.write32(off, v as u32);
        self.write32(off + 4, (v >> 32) as u32);
    }
}
