// SPDX-License-Identifier: Apache-2.0
//! Message-signalled interrupt (MSI / MSI-X) message composition.
//!
//! PCI devices raise an interrupt by writing `data` to `address`; for x86 the
//! address selects the destination local APIC and the data selects the vector
//! (Intel SDM vol. 3, 10.11). The PCI drivers in later phases program these into
//! device capability structures.

/// Fixed base of the x86 MSI address window.
const MSI_BASE: u64 = 0xFEE0_0000;

/// A composed MSI message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    pub address: u64,
    pub data: u32,
}

/// Composes a fixed-delivery, edge-triggered, physical-destination message.
///
/// Returns `None` if the destination APIC ID does not fit the 8-bit compatibility
/// format (xAPIC IDs above 254 need interrupt remapping).
pub fn compose(apic_id: u32, vector: u8) -> Option<Message> {
    if apic_id > 0xFE || vector < 32 {
        return None;
    }
    Some(Message {
        address: MSI_BASE | (u64::from(apic_id) << 12),
        data: u32::from(vector),
    })
}
