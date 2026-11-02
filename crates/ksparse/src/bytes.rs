// SPDX-License-Identifier: Apache-2.0
//! Bounds-checked little-endian readers.

/// Reads a `u8` at `off`.
pub fn u8_at(b: &[u8], off: usize) -> Option<u8> {
    b.get(off).copied()
}

/// Reads a little-endian `u16` at `off`.
pub fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    let s = b.get(off..off.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

/// Reads a little-endian `u32` at `off`.
pub fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    let s = b.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Reads a little-endian `u64` at `off`.
pub fn u64_at(b: &[u8], off: usize) -> Option<u64> {
    let s = b.get(off..off.checked_add(8)?)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Some(u64::from_le_bytes(a))
}
