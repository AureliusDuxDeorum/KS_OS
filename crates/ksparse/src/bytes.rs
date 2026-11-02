// SPDX-License-Identifier: Apache-2.0
//! Bounds-checked little-endian readers.

/// Reads a `u8` at `off`.
pub fn u8_at(b: &[u8], off: usize) -> Option<u8> {
    b.get(off).copied()
}

/// Reads a little-endian `u16` at `off`.
pub fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    let s = b.get(off..off.checked_add(2)?)?;
    Some(u16::from_le_bytes(s.try_into().ok()?))
}

/// Reads a little-endian `u32` at `off`.
pub fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    let s = b.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes(s.try_into().ok()?))
}

/// Reads a little-endian `u64` at `off`.
pub fn u64_at(b: &[u8], off: usize) -> Option<u64> {
    let s = b.get(off..off.checked_add(8)?)?;
    Some(u64::from_le_bytes(s.try_into().ok()?))
}

/// Reads `N` bytes at `off` as an array.
pub fn array_at<const N: usize>(b: &[u8], off: usize) -> Option<[u8; N]> {
    b.get(off..off.checked_add(N)?)?.try_into().ok()
}

/// Reads `len` bytes at `off`.
pub fn slice_at(b: &[u8], off: usize, len: usize) -> Option<&[u8]> {
    b.get(off..off.checked_add(len)?)
}

/// Everything from `from` onwards (empty when `from` is past the end).
pub fn tail(b: &[u8], from: usize) -> &[u8] {
    b.get(from..).unwrap_or(&[])
}
