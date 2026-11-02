// SPDX-License-Identifier: Apache-2.0
//! Bluetooth host stack.
//!
//! Everything here is a pure function of bytes plus small traits for the pieces that touch the
//! outside world (the radio link, randomness, the user). That is what lets the whole stack —
//! HCI, L2CAP, ATT/GATT, SMP pairing with LE Secure Connections, and HID-over-GATT — be tested
//! on the host against simulated controllers and peripherals, and keeps device-supplied data
//! (advertising reports, ATT responses, SMP PDUs) behind bounds-checked parsers.

#![no_std]
#![forbid(unsafe_code)]
// Radio input must never be able to panic the host stack (ADR-0030 S-10/S-11, ADR-0031).
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
#[cfg(test)]
extern crate std;

pub mod att;
pub mod bond;
pub mod crypto;
pub mod framing;
pub mod gatt;
pub mod harness;
pub mod hci;
pub mod hogp;
pub mod host;
pub mod l2cap;
pub mod mtk;
pub mod policy;
pub mod smp;

/// A 48-bit Bluetooth device address with its type (public = 0, random = 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address {
    pub kind: u8,
    /// Little-endian, as on the wire.
    pub bytes: [u8; 6],
}

impl Address {
    pub const fn new(kind: u8, bytes: [u8; 6]) -> Self {
        Self { kind, bytes }
    }
}

/// Source of cryptographically secure random bytes.
pub trait Rng {
    fn fill(&mut self, buf: &mut [u8]);
}
