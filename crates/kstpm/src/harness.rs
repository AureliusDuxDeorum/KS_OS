// SPDX-License-Identifier: Apache-2.0
//! Fuzz/robustness entry point: a TPM that answers with arbitrary bytes, and arbitrary stored records.
//! The contract is "must return": no panic, hang or out-of-bounds access.

use crate::vault::Vault;
use crate::{Error, PcrSelection, SealedBlob, Tpm, Transport};
use alloc::vec::Vec;

struct Hostile<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Transport for Hostile<'_> {
    fn transmit(&mut self, _command: &[u8], response: &mut [u8]) -> Result<usize, Error> {
        // Each response is the next slice of the input: first byte = length.
        let len = usize::from(*self.data.get(self.pos).ok_or(Error::Transport)?);
        let chunk = self
            .data
            .get(self.pos.saturating_add(1)..self.pos.saturating_add(1).saturating_add(len))
            .ok_or(Error::Transport)?;
        self.pos = self.pos.saturating_add(1).saturating_add(len);
        let n = chunk.len().min(response.len());
        response
            .get_mut(..n)
            .ok_or(Error::Malformed)?
            .copy_from_slice(chunk.get(..n).ok_or(Error::Malformed)?);
        Ok(n)
    }
}

pub fn exercise(bytes: &[u8]) {
    let _ = SealedBlob::from_bytes(bytes);
    let mut tpm = Tpm::new(Hostile {
        data: bytes,
        pos: 0,
    });
    let _ = tpm.startup();
    let _ = tpm.random(40);
    let _ = tpm.pcr_read(7);
    let blob = SealedBlob {
        private: Vec::from(bytes.get(..8).unwrap_or(&[])),
        public: Vec::from(bytes.get(8..16).unwrap_or(&[])),
        pcrs: PcrSelection::pcr(7),
    };
    let _ = tpm.unseal(&blob, b"x");
    let _ = tpm.seal(bytes.get(..16).unwrap_or(&[1]), PcrSelection::pcr(7), b"x");
    let _ = Vault::open(&mut tpm, b"fuzz", bytes);
}
