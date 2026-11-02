// SPDX-License-Identifier: Apache-2.0
//! Fixed-capacity, allocation-free string buffer implementing `fmt::Write`.

use core::fmt;

pub struct StrBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StrBuf<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    pub fn as_str(&self) -> &str {
        // SAFETY: only complete UTF-8 chunks are ever appended (see `write_str`).
        unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) }
    }
}

impl<const N: usize> fmt::Write for StrBuf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        // Truncate on a char boundary so the buffer always holds valid UTF-8.
        let mut end = s.len().min(N - self.len);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        self.buf[self.len..self.len + end].copy_from_slice(&s.as_bytes()[..end]);
        self.len += end;
        Ok(())
    }
}
