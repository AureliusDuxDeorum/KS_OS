// SPDX-License-Identifier: Apache-2.0
//! In-memory kernel log: a ring buffer that mirrors everything written to the serial console,
//! so the log can be read on screen (key `L`) on machines without a serial port.

use crate::sync::IrqMutex;
use alloc::string::String;
use alloc::vec::Vec;

const SIZE: usize = 64 * 1024;

struct Ring {
    buf: [u8; SIZE],
    head: usize, // next write position
    filled: bool,
}

static RING: IrqMutex<Ring> = IrqMutex::new(Ring {
    buf: [0; SIZE],
    head: 0,
    filled: false,
});

/// Appends text (drops the oldest data when full).
pub fn append(s: &str) {
    let mut r = RING.lock();
    for &b in s.as_bytes() {
        let h = r.head;
        r.buf[h] = b;
        r.head = (h + 1) % SIZE;
        if r.head == 0 {
            r.filled = true;
        }
    }
}

/// The last `rows` display lines, wrapped to `cols` columns (oldest first).
pub fn tail(rows: usize, cols: usize) -> Vec<String> {
    let bytes: Vec<u8> = {
        let r = RING.lock();
        if r.filled {
            r.buf[r.head..]
                .iter()
                .chain(r.buf[..r.head].iter())
                .copied()
                .collect()
        } else {
            r.buf[..r.head].to_vec()
        }
    };
    // Only the tail matters; avoid wrapping the whole 64 KiB.
    let start = bytes.len().saturating_sub(rows * cols * 2 + 4096);
    let text = String::from_utf8_lossy(&bytes[start..]);
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.trim_end_matches('\r');
        if raw.is_empty() {
            lines.push(String::new());
            continue;
        }
        let chars: Vec<char> = raw.chars().filter(|c| !c.is_control()).collect();
        for chunk in chars.chunks(cols.max(1)) {
            lines.push(chunk.iter().collect());
        }
    }
    let skip = lines.len().saturating_sub(rows);
    lines.split_off(skip)
}
