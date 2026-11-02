// SPDX-License-Identifier: Apache-2.0
//! Big-endian TPM wire encoding helpers.

use alloc::vec::Vec;

#[derive(Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }
    /// TPM2B: 16-bit length followed by the data.
    pub fn tpm2b(&mut self, b: &[u8]) {
        self.u16(u16::try_from(b.len()).unwrap_or(u16::MAX));
        self.bytes(b);
    }
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
    /// Patches the command size field (bytes 2..6) with the final length.
    pub fn finish_with_size(mut self) -> Vec<u8> {
        let n = u32::try_from(self.buf.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes();
        if let Some(dst) = self.buf.get_mut(2..6) {
            dst.copy_from_slice(&n);
        }
        self.buf
    }
}
