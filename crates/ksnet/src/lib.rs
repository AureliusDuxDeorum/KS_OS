// SPDX-License-Identifier: Apache-2.0
//! KS_OS networking as pure, host-testable code.
//!
//! * [`dma`] — the volatile DMA-memory access trait the ring logic is written against.
//! * [`nic`] — the `Nic` trait every driver implements and the adapter into smoltcp.
//! * [`stack`] — the IP stack: DHCP client, DNS, ICMP echo and TCP client sockets on top of smoltcp.
//! * [`igc`] — Intel I225/I226 (igc) and 82576 (igb) register, descriptor and ring definitions.
//! * [`virtio`] — virtio-net (modern PCI transport) layout definitions and split-virtqueue maths.
//!
//! No `unsafe` and no hardware access live here; the kernel's `net/` module supplies MMIO and DMA.
#![no_std]
#![forbid(unsafe_code)]
// Device- and wire-supplied data must never be able to panic the stack (ADR-0030 S-10/S-11).
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
#![cfg_attr(
    test,
    allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

extern crate alloc;

pub mod dma;
pub mod harness;
pub mod igc;
pub mod nic;
pub mod stack;
pub mod virtio;

pub use nic::{Mac, Nic, NicDevice};
pub use stack::{Lease, PingResult, Stack, TcpId, TcpState};
