//! Pure, allocation-light parsers for untrusted firmware and hardware data.
//!
//! Everything here is a function of a byte slice (or a closure reading device
//! registers): no I/O, no globals, no `unsafe`. The kernel feeds these parsers data
//! from ACPI tables and PCI configuration space; the host test-suite and the
//! fuzz targets feed them arbitrary bytes. **Invariant: no input may cause a
//! panic, an out-of-bounds access or an unbounded loop.**

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod bytes;
pub mod efi;
pub mod harness;
pub mod hid;
pub mod madt;
pub mod mcfg;
pub mod pci;
pub mod sdt;
pub mod usb;
