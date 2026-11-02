// SPDX-License-Identifier: Apache-2.0
//! virtio-net over the modern (1.x) PCI transport: layout constants and split-virtqueue logic.
//!
//! Used for CI (QEMU `virtio-net-pci`) and virtual machines. Spec: OASIS Virtual I/O Device 1.2.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "ring indices are driver-private and always below the ring size (<= 32768) and offsets \
              stay inside the region; the release profile has overflow-checks, so a bug is fail-stop"
)]

use crate::dma::DmaMem;

pub const VENDOR: u16 = 0x1AF4;
/// Modern virtio-net PCI device id (0x1040 + device type 1).
pub const DEVICE_NET_MODERN: u16 = 0x1041;

// Vendor-specific PCI capability `cfg_type` values.
pub const CAP_COMMON: u8 = 1;
pub const CAP_NOTIFY: u8 = 2;
pub const CAP_ISR: u8 = 3;
pub const CAP_DEVICE: u8 = 4;

// Device status bits.
pub const STATUS_ACKNOWLEDGE: u8 = 1;
pub const STATUS_DRIVER: u8 = 2;
pub const STATUS_DRIVER_OK: u8 = 4;
pub const STATUS_FEATURES_OK: u8 = 8;
pub const STATUS_FAILED: u8 = 128;

// Feature bits (as bit numbers in the 64-bit feature word).
pub const F_NET_MAC: u64 = 1 << 5;
pub const F_NET_STATUS: u64 = 1 << 16;
pub const F_VERSION_1: u64 = 1 << 32;

// Common configuration structure offsets.
pub const CC_DEVICE_FEATURE_SELECT: usize = 0x00;
pub const CC_DEVICE_FEATURE: usize = 0x04;
pub const CC_DRIVER_FEATURE_SELECT: usize = 0x08;
pub const CC_DRIVER_FEATURE: usize = 0x0C;
pub const CC_DEVICE_STATUS: usize = 0x14;
pub const CC_QUEUE_SELECT: usize = 0x16;
pub const CC_QUEUE_SIZE: usize = 0x18;
pub const CC_QUEUE_ENABLE: usize = 0x1C;
pub const CC_QUEUE_NOTIFY_OFF: usize = 0x1E;
pub const CC_QUEUE_DESC: usize = 0x20;
pub const CC_QUEUE_DRIVER: usize = 0x28;
pub const CC_QUEUE_DEVICE: usize = 0x30;

/// `struct virtio_net_hdr` for VIRTIO_F_VERSION_1 (includes `num_buffers`).
pub const NET_HDR_LEN: usize = 12;
/// Net device config: MAC at 0, link status (u16) at 6.
pub const NET_CFG_MAC: usize = 0;
pub const NET_CFG_STATUS: usize = 6;
pub const NET_S_LINK_UP: u16 = 1;

const DESC_F_NEXT: u16 = 1;
const DESC_F_WRITE: u16 = 2;

/// Byte layout of a split virtqueue of `size` entries inside one contiguous DMA region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub size: u16,
    pub desc: usize,
    pub avail: usize,
    pub used: usize,
    pub total: usize,
}

impl Layout {
    pub fn new(size: u16) -> Self {
        assert!(size.is_power_of_two() && size <= 32768);
        let n = usize::from(size);
        let desc = 0;
        let avail = desc + 16 * n; // 2-aligned
        let used = (avail + 6 + 2 * n).next_multiple_of(4);
        let total = used + 6 + 8 * n;
        Self {
            size,
            desc,
            avail,
            used,
            total,
        }
    }
}

/// Driver-side state of one split virtqueue. Buffers are single descriptors (no chaining) except
/// for the header+payload pair built by [`Virtqueue::add_chain`].
pub struct Virtqueue {
    pub layout: Layout,
    free_head: u16,
    num_free: u16,
    avail_idx: u16,
    last_used: u16,
    /// Per-descriptor chain length, so freeing returns the whole chain.
    chain_len: alloc::vec::Vec<u16>,
    /// Driver-private copy of every descriptor's `next` link. Descriptor memory is shared with
    /// the device, so it can never be trusted to walk or recycle chains.
    next: alloc::vec::Vec<u16>,
}

impl Virtqueue {
    /// Initialises the free list inside `mem` (which must be zeroed and at least `layout.total` long).
    pub fn new(size: u16, mem: &impl DmaMem) -> Self {
        let layout = Layout::new(size);
        assert!(mem.len() >= layout.total);
        for i in 0..size {
            let o = layout.desc + usize::from(i) * 16;
            mem.write16(o + 14, (i + 1) % size); // next = following descriptor
        }
        Self {
            layout,
            free_head: 0,
            num_free: size,
            avail_idx: 0,
            last_used: 0,
            chain_len: alloc::vec![0; usize::from(size)],
            next: (0..size).map(|i| (i + 1) % size).collect(),
        }
    }

    pub fn free_descriptors(&self) -> u16 {
        self.num_free
    }

    /// Adds a chain of buffers `(phys_addr, len, device_writes)` and publishes it.
    /// Returns the head index (the token reported back in the used ring).
    pub fn add_chain(&mut self, mem: &impl DmaMem, bufs: &[(u64, u32, bool)]) -> Option<u16> {
        let count = bufs.len() as u16;
        if count == 0 || count > self.num_free {
            return None;
        }
        let l = self.layout;
        let head = self.free_head;
        let mut idx = head;
        for (k, &(addr, len, write)) in bufs.iter().enumerate() {
            let o = l.desc + usize::from(idx) * 16;
            let next = self.next.get(usize::from(idx)).copied().unwrap_or(0);
            let last = k + 1 == bufs.len();
            mem.write64(o, addr);
            mem.write32(o + 8, len);
            mem.write16(o + 14, next);
            let mut flags = if write { DESC_F_WRITE } else { 0 };
            if !last {
                flags |= DESC_F_NEXT;
            }
            mem.write16(o + 12, flags);
            if last {
                self.free_head = next;
            } else {
                // `next` field is already the chain link.
            }
            idx = next;
        }
        self.num_free -= count;
        if let Some(c) = self.chain_len.get_mut(usize::from(head)) {
            *c = count;
        }
        // avail ring: flags(2) idx(2) ring[size] ; publish the head then bump idx.
        let slot = usize::from(self.avail_idx % l.size);
        mem.write16(l.avail + 4 + 2 * slot, head);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        mem.write16(l.avail + 2, self.avail_idx);
        Some(head)
    }

    /// Takes the next completed chain: `(head token, bytes written by the device)`.
    pub fn pop_used(&mut self, mem: &impl DmaMem) -> Option<(u16, u32)> {
        let l = self.layout;
        if mem.read16(l.used + 2) == self.last_used {
            return None;
        }
        let slot = usize::from(self.last_used % l.size);
        let e = l.used + 4 + 8 * slot;
        let id = mem.read32(e) as u16;
        let len = mem.read32(e + 4);
        self.last_used = self.last_used.wrapping_add(1);
        // A malformed completion from the device (bad id, or a chain we did not submit) is ignored.
        let count = match self.chain_len.get(usize::from(id)).copied() {
            Some(c) if c != 0 && id < l.size => c,
            _ => return None,
        };
        // Return the chain to the free list.
        if let Some(c) = self.chain_len.get_mut(usize::from(id)) {
            *c = 0;
        }
        let mut tail = id;
        for _ in 1..count {
            tail = self.next.get(usize::from(tail)).copied()?;
        }
        *self.next.get_mut(usize::from(tail))? = self.free_head;
        mem.write16(l.desc + usize::from(tail) * 16 + 14, self.free_head);
        self.free_head = id;
        self.num_free += count;
        Some((id, len))
    }
}
