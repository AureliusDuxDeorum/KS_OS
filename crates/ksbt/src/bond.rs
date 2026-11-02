// SPDX-License-Identifier: Apache-2.0
//! Bond storage format: what survives a reboot so that a paired device can reconnect and
//! re-encrypt without pairing again.
//!
//! The serialised form is versioned, length-prefixed and covered by a CRC-32; parsing is
//! bounded and rejects anything inconsistent. Where the bytes are stored (a UEFI variable) is
//! the kernel's concern.

use crate::{crypto, Address};
use alloc::vec::Vec;
use ksparse::bytes::{array_at, u32_at, u8_at};
use zeroize::Zeroize;

const MAGIC: &[u8; 4] = b"KSBB";
const VERSION: u8 = 1;
const RECORD_LEN: usize = 1 + 6 + 16 + 1 + 1 + 16 + 1;
const MAX_BONDS: usize = 16;

/// One bonded peripheral.
#[derive(Clone, PartialEq, Eq)]
pub struct Bond {
    /// Identity address (public or static random).
    pub address: Address,
    pub ltk: [u8; 16],
    pub authenticated: bool,
    pub irk: Option<[u8; 16]>,
    /// Appearance category hint for the UI (0 if unknown): 1 keyboard, 2 mouse.
    pub kind: u8,
}

impl Drop for Bond {
    fn drop(&mut self) {
        self.ltk.zeroize();
        if let Some(i) = self.irk.as_mut() {
            i.zeroize();
        }
    }
}

impl core::fmt::Debug for Bond {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never print key material.
        write!(
            f,
            "Bond({:?}, authenticated={}, irk={})",
            self.address,
            self.authenticated,
            self.irk.is_some()
        )
    }
}

#[derive(Default, Debug)]
pub struct BondStore {
    pub bonds: Vec<Bond>,
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (!(crc & 1)).wrapping_add(1));
        }
    }
    !crc
}

impl BondStore {
    /// Adds or replaces the bond for `bond.address`.
    pub fn upsert(&mut self, bond: Bond) {
        self.bonds.retain(|b| b.address != bond.address);
        if self.bonds.len() >= MAX_BONDS {
            self.bonds.remove(0);
        }
        self.bonds.push(bond);
    }

    pub fn find(&self, address: &Address) -> Option<&Bond> {
        self.bonds.iter().find(|b| b.address == *address)
    }

    /// Finds the bond for an advertising address, resolving private addresses with stored IRKs.
    pub fn find_for_advertiser(&self, address: &Address) -> Option<&Bond> {
        self.find(address).or_else(|| {
            (address.kind == 1)
                .then(|| {
                    self.bonds.iter().find(|b| {
                        b.irk
                            .as_ref()
                            .is_some_and(|irk| crypto::resolves(irk, &address.bytes))
                    })
                })
                .flatten()
        })
    }

    pub fn remove(&mut self, address: &Address) {
        self.bonds.retain(|b| b.address != *address);
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            self.bonds
                .len()
                .saturating_mul(RECORD_LEN)
                .saturating_add(12),
        );
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(self.bonds.len() as u8);
        for b in &self.bonds {
            out.push(b.address.kind);
            out.extend_from_slice(&b.address.bytes);
            out.extend_from_slice(&b.ltk);
            out.push(u8::from(b.authenticated));
            out.push(u8::from(b.irk.is_some()));
            out.extend_from_slice(&b.irk.unwrap_or([0; 16]));
            out.push(b.kind);
        }
        let crc = crc32(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    /// Parses stored bytes; returns an empty store for anything that does not validate.
    pub fn from_bytes(data: &[u8]) -> Self {
        Self::try_from_bytes(data).unwrap_or_default()
    }

    fn try_from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 10 || data.get(..4)? != MAGIC || u8_at(data, 4)? != VERSION {
            return None;
        }
        let n = usize::from(u8_at(data, 5)?);
        let body_len = n.checked_mul(RECORD_LEN)?.checked_add(6)?;
        if n > MAX_BONDS || data.len() != body_len.checked_add(4)? {
            return None;
        }
        let stored = u32_at(data, body_len)?;
        if crc32(data.get(..body_len)?) != stored {
            return None;
        }
        let mut bonds = Vec::new();
        for r in data.get(6..body_len)?.chunks_exact(RECORD_LEN) {
            let irk = if u8_at(r, 24)? != 0 {
                Some(array_at::<16>(r, 25)?)
            } else {
                None
            };
            bonds.push(Bond {
                address: Address::new(u8_at(r, 0)?, array_at::<6>(r, 1)?),
                ltk: array_at::<16>(r, 7)?,
                authenticated: u8_at(r, 23)? != 0,
                irk,
                kind: u8_at(r, 41)?,
            });
        }
        Some(Self { bonds })
    }
}
