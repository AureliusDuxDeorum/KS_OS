// SPDX-License-Identifier: Apache-2.0
//! Re-assembly of HCI packets from USB endpoint transfers.
//!
//! USB delivers HCI events over an interrupt endpoint and ACL data over bulk endpoints in
//! transfers no larger than the endpoint's max packet size, so one HCI packet may arrive split
//! across several transfers. These assemblers rebuild whole packets using the length field in
//! the HCI header, and recover from garbage by resynchronising instead of growing without bound.

use alloc::vec::Vec;

/// Which HCI packet header this stream carries.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Event: `code`, `length`, parameters (2-byte header, 1-byte length).
    Event,
    /// ACL: `handle|flags` (2), `length` (2), data (4-byte header, 2-byte length).
    Acl,
}

pub struct Assembler {
    kind: Kind,
    buf: Vec<u8>,
}

const MAX_ACL: usize = 4 + 1024;

impl Assembler {
    pub fn new(kind: Kind) -> Self {
        Self {
            kind,
            buf: Vec::new(),
        }
    }

    fn header_len(&self) -> usize {
        match self.kind {
            Kind::Event => 2,
            Kind::Acl => 4,
        }
    }

    /// Total length of the packet being assembled, once its header is known.
    fn total(&self) -> Option<usize> {
        let h = self.header_len();
        if self.buf.len() < h {
            return None;
        }
        Some(match self.kind {
            Kind::Event => 2 + usize::from(self.buf[1]),
            Kind::Acl => 4 + usize::from(u16::from_le_bytes([self.buf[2], self.buf[3]])),
        })
    }

    /// Feeds one USB transfer's bytes; returns every packet completed by them.
    pub fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        self.buf.extend_from_slice(data);
        while let Some(total) = self.total() {
            if total > MAX_ACL {
                // An implausible length means we lost sync: drop everything buffered.
                self.buf.clear();
                break;
            }
            if self.buf.len() < total {
                break;
            }
            let rest = self.buf.split_off(total);
            out.push(core::mem::replace(&mut self.buf, rest));
        }
        out
    }

    pub fn pending(&self) -> usize {
        self.buf.len()
    }
}
