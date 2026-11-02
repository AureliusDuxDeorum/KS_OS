// SPDX-License-Identifier: Apache-2.0
//! Fuzz/robustness entry point: a hostile network and a hostile device.
//!
//! The contract is "must return": no panic, hang or out-of-bounds access, whatever the device wrote
//! into its rings or whatever frames arrive from the wire.

#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    reason = "the harness is test scaffolding: it builds simulated devices from fuzz input"
)]

use crate::dma::DmaMem;
use crate::igc::{RxQueue, TxQueue, BUF_SIZE, DESC_SIZE};
use crate::nic::{Mac, Nic};
use crate::stack::Stack;
use crate::virtio::{Layout, Virtqueue};
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use smoltcp::wire::Ipv4Address;

struct Mem(RefCell<Vec<u8>>);

impl Mem {
    fn from_input(len: usize, data: &[u8]) -> Self {
        let mut v = vec![0u8; len];
        let n = data.len().min(len);
        v[..n].copy_from_slice(&data[..n]);
        Mem(RefCell::new(v))
    }
}

impl DmaMem for Mem {
    fn len(&self) -> usize {
        self.0.borrow().len()
    }
    fn read32(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.0.borrow()[off..off + 4].try_into().unwrap())
    }
    fn write32(&self, off: usize, v: u32) {
        self.0.borrow_mut()[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn read_bytes(&self, off: usize, out: &mut [u8]) {
        out.copy_from_slice(&self.0.borrow()[off..off + out.len()]);
    }
    fn write_bytes(&self, off: usize, data: &[u8]) {
        self.0.borrow_mut()[off..off + data.len()].copy_from_slice(data);
    }
}

/// A NIC whose received frames come from the fuzz input.
struct FuzzNic {
    frames: VecDeque<Vec<u8>>,
}

impl Nic for FuzzNic {
    fn mac(&self) -> Mac {
        [2, 0, 0, 0, 0, 1]
    }
    fn link_up(&mut self) -> bool {
        true
    }
    fn receive(&mut self, buf: &mut [u8]) -> Option<usize> {
        let f = self.frames.pop_front()?;
        let n = f.len().min(buf.len());
        buf[..n].copy_from_slice(&f[..n]);
        Some(n)
    }
    fn transmit(&mut self, _frame: &[u8]) -> bool {
        true
    }
}

pub fn exercise(data: &[u8]) {
    let split = data.len() / 2;
    let (dev, wire) = data.split_at(split);

    // 1. Hostile device: arbitrary descriptor-ring and used-ring contents.
    let n = 8u16;
    let desc = Mem::from_input(RxQueue::ring_bytes(n), dev);
    let bufs = Mem::from_input(RxQueue::buffer_bytes(n), dev.get(64..).unwrap_or(&[]));
    let mut rx = RxQueue::new(n, 0x1000);
    let mut out = [0u8; 1514];
    for _ in 0..64 {
        let _ = rx.poll(&desc, &bufs, &mut out);
    }
    let tdesc = Mem::from_input(RxQueue::ring_bytes(n), dev);
    let mut tx = TxQueue::new(n, 0x2000);
    for i in 0..32usize {
        let len = 14 + (usize::from(dev.get(i).copied().unwrap_or(0)) * 8) % (BUF_SIZE - 14);
        let _ = tx.submit(&tdesc, &bufs, &vec![0xAB; len]);
        let _ = DESC_SIZE;
    }

    let l = Layout::new(8);
    let vmem = Mem::from_input(l.total, dev);
    let mut vq = Virtqueue::new(8, &Mem::from_input(l.total, &[]));
    // The queue was initialised on separate memory; pointing it at hostile memory is the point.
    for _ in 0..32 {
        let _ = vq.pop_used(&vmem);
        let _ = vq.add_chain(&vmem, &[(0x1000, 100, true), (0x2000, 12, false)]);
    }

    // 2. Hostile wire: arbitrary Ethernet frames into the whole IP stack, including DHCP/DNS/TCP.
    let mut frames = VecDeque::new();
    let mut rest = wire;
    while let Some((&len, tail)) = rest.split_first() {
        let len = (usize::from(len) * 6).min(tail.len());
        frames.push_back(tail[..len].to_vec());
        rest = &tail[len..];
        if frames.len() >= 256 {
            break;
        }
    }
    let mut stack = Stack::new(FuzzNic { frames }, 0x1234, 0);
    let _ = stack.resolve("fuzz.example");
    let _ = stack.ping(Ipv4Address::new(10, 0, 0, 1), 1);
    for t in 0..300 {
        stack.poll(t * 10);
        let _ = stack.ping_result();
        let _ = stack.resolve_result();
    }
}
