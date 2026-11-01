//! Fuzz/robustness entry point: drives every parser and both client state machines with
//! arbitrary bytes. The contract is simply "must return" — no panic, hang or out-of-bounds read.

use crate::bond::BondStore;
use crate::framing::{Assembler, Kind};
use crate::gatt::{self, AttLink, Error};
use crate::smp::{self, PairingConfig, PairingUi, SmpLink};
use crate::{hci, hogp, l2cap, mtk, Address, Rng};
use alloc::vec::Vec;

/// Serves slices of the fuzz input as protocol responses: the first byte of each response is
/// the number of bytes that follow it.
struct Replay<'a> {
    data: &'a [u8],
    pos: usize,
    served: usize,
}

impl Replay<'_> {
    fn next(&mut self) -> Option<Vec<u8>> {
        self.served += 1;
        if self.served > 4096 {
            return None; // a client that never terminates would be a bug; stop feeding it
        }
        let len = usize::from(*self.data.get(self.pos)?);
        let chunk = self.data.get(self.pos + 1..self.pos + 1 + len)?;
        self.pos += 1 + len;
        Some(chunk.to_vec())
    }
}

impl AttLink for Replay<'_> {
    fn request(&mut self, _pdu: &[u8]) -> Result<Vec<u8>, Error> {
        self.next().ok_or(Error::Timeout)
    }
}

impl SmpLink for Replay<'_> {
    fn send(&mut self, _pdu: &[u8]) -> Result<(), Error> {
        Ok(())
    }
    fn recv(&mut self, _timeout_ms: u64) -> Result<Vec<u8>, Error> {
        self.next().ok_or(Error::Timeout)
    }
}

struct Counter(u64);
impl Rng for Counter {
    fn fill(&mut self, buf: &mut [u8]) {
        for b in buf {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *b = (self.0 >> 33) as u8;
        }
    }
}

struct NoUi;
impl PairingUi for NoUi {
    fn display_passkey(&mut self, _: u32) {}
    fn confirm_numeric(&mut self, _: u32) -> bool {
        true
    }
    fn enter_passkey(&mut self) -> Option<u32> {
        Some(123_456)
    }
}

pub fn exercise(bytes: &[u8]) {
    // Pure parsers.
    if let Some(ev) = hci::parse_event(bytes) {
        let _ = (
            hci::parse_local_version(&bytes[bytes.len().min(2)..]),
            format_args!("{ev:?}"),
        );
    }
    let _ = hci::parse_acl(bytes);
    let _ = hci::parse_le_buffer_size(bytes);
    let adv = hci::parse_adv_data(bytes);
    let _ = (adv.is_hid(), adv.discoverable(), adv.name_lossy());
    let _ = l2cap::parse_signaling(bytes);
    let mut r = l2cap::Reassembler::default();
    let mut off = 0;
    while off + 4 <= bytes.len() {
        let len = usize::from(bytes[off + 2]) | (usize::from(bytes[off + 3]) << 8);
        let end = (off + 4 + len).min(bytes.len());
        if let Some((h, d)) = hci::parse_acl(&bytes[off..end]) {
            let _ = r.push(&h, d);
        }
        off = end.max(off + 1);
    }
    if let Ok(p) = mtk::parse_patch(bytes) {
        for s in &p.sections {
            let _ = mtk::chunks(&bytes[s.data_offset..s.data_offset + s.data_len]);
        }
    }
    let _ = mtk::parse_wmt_event(bytes);
    let _ = BondStore::from_bytes(bytes);
    for kind in [Kind::Event, Kind::Acl] {
        let mut a = Assembler::new(kind);
        for chunk in bytes.chunks(7) {
            let _ = a.push(chunk);
        }
    }
    // Client state machines against a hostile peer.
    let mut link = Replay {
        data: bytes,
        pos: 0,
        served: 0,
    };
    let _ = gatt::discover_services(&mut link);
    let svc = gatt::Service {
        start: 1,
        end: 0xFFFF,
        uuid: crate::att::Uuid::Short(0x1812),
    };
    let _ = gatt::discover_characteristics(&mut link, &svc);
    let _ = gatt::discover_descriptors(&mut link, 1, 0xFFFF);
    let _ = gatt::read_long(&mut link, 3, 23, 4096);
    let _ = hogp::HogpDevice::setup(&mut link, 23);
    let cfg = PairingConfig {
        io_capability: smp::io::KEYBOARD_DISPLAY,
        bonding: true,
        local: Address::new(0, [1; 6]),
        peer: Address::new(0, [2; 6]),
    };
    let mut link = Replay {
        data: bytes,
        pos: 0,
        served: 0,
    };
    let _ = smp::pair(&mut link, &mut NoUi, &mut Counter(7), &cfg);
}
