// SPDX-License-Identifier: Apache-2.0
//! igc descriptor-ring and virtqueue logic against simulated devices that behave like the hardware.
mod common;

use common::Mem;
use ksnet::dma::DmaMem;
use ksnet::igc::{self, RxQueue, TxQueue, BUF_SIZE, DESC_SIZE};
use ksnet::virtio::{Layout, Virtqueue};

fn frame(tag: u8, len: usize) -> Vec<u8> {
    let mut f = vec![tag; len];
    f[..6].copy_from_slice(&[0xff; 6]);
    f
}

/// Plays the device side of an RX ring: fills the next armed descriptor.
fn hw_rx_deliver(desc: &Mem, bufs: &Mem, idx: usize, data: &[u8], status: u32) {
    bufs.write_bytes(idx * BUF_SIZE, data);
    let o = idx * DESC_SIZE;
    desc.write32(o + 8, status);
    desc.write16(o + 12, data.len() as u16);
}

#[test]
fn rx_ring_delivers_in_order_and_rearms() {
    let n = 8u16;
    let desc = Mem::new(RxQueue::ring_bytes(n));
    let bufs = Mem::new(RxQueue::buffer_bytes(n));
    let mut rx = RxQueue::new(n, 0x10_0000);
    let tail = rx.init(&desc);
    assert_eq!(tail, 7);
    assert_eq!(desc.read64(0), 0x10_0000);
    assert_eq!(desc.read64(3 * DESC_SIZE), 0x10_0000 + 3 * BUF_SIZE as u64);

    let mut out = [0u8; 2048];
    assert_eq!(rx.poll(&desc, &bufs, &mut out), (None, None));

    // Wrap around the ring several times, delivering frames of varying size.
    for round in 0..30usize {
        let idx = round % n as usize;
        let f = frame(round as u8, 60 + round * 40 % 1400);
        hw_rx_deliver(&desc, &bufs, idx, &f, 0b11);
        let (len, new_tail) = rx.poll(&desc, &bufs, &mut out);
        assert_eq!(len, Some(f.len()));
        assert_eq!(&out[..f.len()], &f[..]);
        assert_eq!(new_tail, Some(idx as u16));
        assert_eq!(
            desc.read32(idx * DESC_SIZE + 8),
            0,
            "status cleared on re-arm"
        );
    }
    assert_eq!(rx.dropped, 0);
}

#[test]
fn rx_ring_drops_errored_runt_and_fragmented_frames() {
    let n = 8u16;
    let desc = Mem::new(RxQueue::ring_bytes(n));
    let bufs = Mem::new(RxQueue::buffer_bytes(n));
    let mut rx = RxQueue::new(n, 0);
    rx.init(&desc);
    let mut out = [0u8; 1514];

    hw_rx_deliver(&desc, &bufs, 0, &frame(1, 100), 0b11 | (1 << 31)); // RXE
    assert_eq!(rx.poll(&desc, &bufs, &mut out).0, None);
    hw_rx_deliver(&desc, &bufs, 1, &frame(2, 100), 0b01); // no EOP
    assert_eq!(rx.poll(&desc, &bufs, &mut out).0, None);
    hw_rx_deliver(&desc, &bufs, 2, &frame(3, 10), 0b11); // runt
    assert_eq!(rx.poll(&desc, &bufs, &mut out).0, None);
    hw_rx_deliver(&desc, &bufs, 3, &frame(4, 1600), 0b11); // larger than the caller's buffer
    assert_eq!(rx.poll(&desc, &bufs, &mut out).0, None);
    assert_eq!(rx.dropped, 4);
    hw_rx_deliver(&desc, &bufs, 4, &frame(5, 100), 0b11);
    assert_eq!(rx.poll(&desc, &bufs, &mut out).0, Some(100));
}

/// Device side of TX: consumes descriptors up to `tdt`, marks them done.
fn hw_tx_consume(
    desc: &Mem,
    bufs: &Mem,
    from: usize,
    to: usize,
    n: usize,
    sent: &mut Vec<Vec<u8>>,
) {
    let mut i = from;
    while i != to {
        let o = i * DESC_SIZE;
        let cmd = desc.read32(o + 8);
        let len = (cmd & 0xFFFF) as usize;
        assert_eq!(cmd >> 20 & 0xF, 3, "data descriptor type");
        assert!(
            cmd & (1 << 24) != 0 && cmd & (1 << 25) != 0 && cmd & (1 << 27) != 0,
            "EOP|IFCS|RS"
        );
        assert!(cmd & (1 << 29) != 0, "DEXT");
        assert_eq!(desc.read32(o + 12) >> 14, len as u32, "PAYLEN");
        let mut f = vec![0; len];
        bufs.read_bytes(i * BUF_SIZE, &mut f);
        assert_eq!(desc.read64(o), 0x20_0000 + (i * BUF_SIZE) as u64);
        sent.push(f);
        desc.write32(o + 12, desc.read32(o + 12) | 1); // DD
        i = (i + 1) % n;
    }
}

#[test]
fn tx_ring_sends_wraps_and_reports_full() {
    let n = 8u16;
    let desc = Mem::new(RxQueue::ring_bytes(n));
    let bufs = Mem::new(RxQueue::buffer_bytes(n));
    let mut tx = TxQueue::new(n, 0x20_0000);
    let mut sent = vec![];
    let mut hw_head = 0usize;

    // Fill the ring completely without the device consuming: one slot stays unused.
    let mut tdt = 0;
    for i in 0..7 {
        tdt = tx
            .submit(&desc, &bufs, &frame(i, 64 + i as usize))
            .expect("room");
    }
    assert_eq!(tdt, 7);
    assert!(
        tx.submit(&desc, &bufs, &frame(9, 64)).is_none(),
        "full ring is reported"
    );
    hw_tx_consume(&desc, &bufs, hw_head, tdt as usize, n as usize, &mut sent);
    hw_head = tdt as usize;
    assert_eq!(sent.len(), 7);

    // Space is reclaimed from DD bits; run many more frames through, wrapping repeatedly.
    for i in 0..100u8 {
        let f = frame(i, 60 + usize::from(i));
        let tdt = tx.submit(&desc, &bufs, &f).expect("reclaimed room");
        hw_tx_consume(&desc, &bufs, hw_head, tdt as usize, n as usize, &mut sent);
        hw_head = tdt as usize;
        assert_eq!(sent.last().unwrap(), &f);
    }
    assert_eq!(sent.len(), 107);
}

#[test]
fn tx_rejects_bad_sizes() {
    let desc = Mem::new(TxQueue::new(8, 0).free() as usize * DESC_SIZE);
    let bufs = Mem::new(8 * BUF_SIZE);
    let mut tx = TxQueue::new(8, 0);
    assert!(tx.submit(&desc, &bufs, &[0; 5]).is_none());
    assert!(tx.submit(&desc, &bufs, &vec![0; BUF_SIZE + 1]).is_none());
}

#[test]
fn link_mac_and_family_decoding() {
    use igc::*;
    assert_eq!(
        link_from_status(STATUS_LU | STATUS_FD | STATUS_SPEED_2500),
        Link {
            up: true,
            mbps: 2500,
            full_duplex: true
        }
    );
    assert_eq!(link_from_status(STATUS_LU | STATUS_SPEED_1000).mbps, 1000);
    assert_eq!(link_from_status(STATUS_LU | STATUS_SPEED_100).mbps, 100);
    assert_eq!(link_from_status(STATUS_LU).mbps, 10);
    assert!(!link_from_status(0).up);
    assert_eq!(
        mac_from_ra(0x1211_0A02, RAH_AV | 0x4433),
        Some([0x02, 0x0A, 0x11, 0x12, 0x33, 0x44])
    );
    assert_eq!(
        mac_from_ra(0x1211_0A02, 0x4433),
        None,
        "address-valid bit required"
    );
    assert_eq!(mac_from_ra(0, RAH_AV), None);
    assert_eq!(mac_from_ra(!0, RAH_AV | 0xFFFF), None);
    assert_eq!(
        mac_from_ra(0x1211_0B01, RAH_AV | 0x4433),
        None,
        "multicast is not a station address"
    );
    assert_eq!(family(0x15F3), Some(Family::Igc)); // the owner's I225-V
    assert_eq!(family(0x10C9), Some(Family::Igb));
    assert_eq!(family(0x1234), None);
}

// ---- virtqueue ----

/// Device side: completes buffers in the order given.
fn hw_complete(mem: &Mem, l: &Layout, used_idx: &mut u16, head: u16, written: u32) {
    let slot = (*used_idx % l.size) as usize;
    mem.write32(l.used + 4 + 8 * slot, u32::from(head));
    mem.write32(l.used + 4 + 8 * slot + 4, written);
    *used_idx = used_idx.wrapping_add(1);
    mem.write16(l.used + 2, *used_idx);
}

#[test]
fn virtio_layout_is_spec_aligned() {
    for size in [8u16, 64, 256, 1024] {
        let l = Layout::new(size);
        assert_eq!(l.desc % 16, 0);
        assert_eq!(l.avail % 2, 0);
        assert_eq!(l.used % 4, 0);
        assert!(l.avail >= l.desc + 16 * size as usize);
        assert!(l.used >= l.avail + 6 + 2 * size as usize);
        assert_eq!(l.total, l.used + 6 + 8 * size as usize);
    }
}

#[test]
fn virtqueue_chains_recycle_without_leaking() {
    let size = 8u16;
    let l = Layout::new(size);
    let mem = Mem::new(l.total);
    let mut q = Virtqueue::new(size, &mem);
    let mut used_idx = 0u16;
    assert_eq!(q.free_descriptors(), 8);

    for round in 0..50u64 {
        // header + payload chain, device-readable
        let head = q
            .add_chain(
                &mem,
                &[(0x1000 + round, 12, false), (0x2000 + round, 100, false)],
            )
            .expect("room");
        assert_eq!(q.free_descriptors(), 6);
        // descriptor contents: first links to second, second terminates
        let d0 = l.desc + head as usize * 16;
        assert_eq!(mem.read64(d0), 0x1000 + round);
        assert_eq!(mem.read16(d0 + 12) & 1, 1, "NEXT");
        let next = mem.read16(d0 + 14);
        let d1 = l.desc + next as usize * 16;
        assert_eq!(mem.read64(d1), 0x2000 + round);
        assert_eq!(mem.read16(d1 + 12) & 1, 0);
        // avail ring published
        assert_eq!(mem.read16(l.avail + 2) as u64, round + 1);
        assert_eq!(mem.read16(l.avail + 4 + 2 * (round as usize % 8)), head);

        assert_eq!(q.pop_used(&mem), None);
        hw_complete(&mem, &l, &mut used_idx, head, 0);
        assert_eq!(q.pop_used(&mem), Some((head, 0)));
        assert_eq!(q.free_descriptors(), 8, "round {round} leaked descriptors");
    }
}

#[test]
fn virtqueue_exhaustion_and_malformed_completions() {
    let size = 8u16;
    let l = Layout::new(size);
    let mem = Mem::new(l.total);
    let mut q = Virtqueue::new(size, &mem);
    let mut heads = vec![];
    for i in 0..8 {
        heads.push(q.add_chain(&mem, &[(i, 1514, true)]).expect("slot"));
    }
    assert!(q.add_chain(&mem, &[(0, 1, true)]).is_none());
    assert!(q.add_chain(&mem, &[]).is_none());

    let mut used_idx = 0u16;
    // A device reporting an out-of-range id must not corrupt the free list.
    hw_complete(&mem, &l, &mut used_idx, 99, 5);
    assert_eq!(q.pop_used(&mem), None);
    assert_eq!(q.free_descriptors(), 0);
    // Reporting the same chain twice frees it only once.
    hw_complete(&mem, &l, &mut used_idx, heads[3], 60);
    hw_complete(&mem, &l, &mut used_idx, heads[3], 60);
    assert_eq!(q.pop_used(&mem), Some((heads[3], 60)));
    assert_eq!(q.pop_used(&mem), None);
    assert_eq!(q.free_descriptors(), 1);
}

#[test]
fn mdio_encoding_and_phy_fixup() {
    use igc::*;
    // Read BMSR (reg 1) of PHY 1: op=read(bit27), phy<<21, reg<<16.
    assert_eq!(
        mdic_read(Family::Igb, PHY_BMSR),
        (1 << 27) | (1 << 21) | (1 << 16)
    );
    assert_eq!(
        mdic_write(Family::Igc, PHY_BMCR, 0x1200),
        (1 << 26) | 0x1200
    );
    // A healthy auto-negotiating PHY is not disturbed.
    assert_eq!(
        bmcr_fixup(BMCR_ANENABLE, BMSR_ANCOMPLETE | BMSR_LSTATUS),
        None
    );
    // Power-down, autoneg off or autoneg never completed is repaired and restarted.
    let fixed = Some(BMCR_ANENABLE | BMCR_ANRESTART);
    assert_eq!(
        bmcr_fixup(BMCR_PDOWN | BMCR_ANENABLE, BMSR_ANCOMPLETE),
        fixed
    );
    assert_eq!(bmcr_fixup(0, BMSR_ANCOMPLETE), fixed);
    assert_eq!(bmcr_fixup(BMCR_ANENABLE, 0), fixed);
}

#[test]
fn harness_survives_pseudo_random_inputs() {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for round in 0..200 {
        let len = (round * 37) % 3000;
        let data: Vec<u8> = (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect();
        ksnet::harness::exercise(&data);
    }
}

#[test]
fn virtqueue_ignores_descriptor_next_scribbled_by_the_device() {
    // Regression for a fuzzer finding: chain links must come from driver memory, not the shared ring.
    let l = Layout::new(8);
    let mem = Mem::new(l.total);
    let mut q = Virtqueue::new(8, &mem);
    let head = q
        .add_chain(&mem, &[(0x1000, 12, false), (0x2000, 100, false)])
        .unwrap();
    for i in 0..8usize {
        mem.write16(l.desc + i * 16 + 14, 0xFFFF); // hostile `next`
    }
    let mut used_idx = 0;
    hw_complete(&mem, &l, &mut used_idx, head, 0);
    assert_eq!(q.pop_used(&mem), Some((head, 0)));
    assert_eq!(q.free_descriptors(), 8);
    // And the queue is still fully usable afterwards.
    for _ in 0..4 {
        assert!(q.add_chain(&mem, &[(1, 1, true), (2, 1, true)]).is_some());
    }
    assert_eq!(q.free_descriptors(), 0);
}
