// SPDX-License-Identifier: Apache-2.0
//! L2CAP basic frames over LE ACL links: reassembly, fragmentation and LE signalling.

use crate::hci::{build_acl, AclHeader};
use alloc::vec::Vec;
use ksparse::bytes::{u16_at, u8_at};

pub const CID_ATT: u16 = 0x0004;
pub const CID_LE_SIGNALING: u16 = 0x0005;
pub const CID_SMP: u16 = 0x0006;

/// Largest L2CAP payload we accept (guards against a peer announcing a huge length).
pub const MAX_SDU: usize = 1024;

/// A complete L2CAP frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub cid: u16,
    pub payload: Vec<u8>,
}

/// Reassembles ACL fragments of one connection into L2CAP frames.
#[derive(Default)]
pub struct Reassembler {
    buf: Vec<u8>,
    expected: usize,
}

impl Reassembler {
    /// Feeds one ACL packet; returns a frame when one completes.
    pub fn push(&mut self, h: &AclHeader, data: &[u8]) -> Option<Frame> {
        if h.pb == 0b01 {
            // Continuation: only valid while a frame is in flight.
            if self.expected == 0 {
                return None;
            }
            self.buf.extend_from_slice(data);
        } else {
            // Start of a new frame: any partial one is dropped.
            let len = usize::from(u16_at(data, 0)?);
            if len > MAX_SDU {
                self.reset();
                return None;
            }
            self.expected = len + 4;
            self.buf.clear();
            self.buf.extend_from_slice(data);
        }
        if self.buf.len() > self.expected {
            self.reset(); // inconsistent fragment lengths
            return None;
        }
        if self.buf.len() == self.expected {
            let cid = u16_at(&self.buf, 2)?;
            let payload = self.buf[4..].to_vec();
            self.reset();
            return Some(Frame { cid, payload });
        }
        None
    }

    fn reset(&mut self) {
        self.buf.clear();
        self.expected = 0;
    }
}

/// Splits a frame into ACL packets no larger than `max_acl` bytes of data.
pub fn fragment(handle: u16, cid: u16, payload: &[u8], max_acl: usize) -> Vec<Vec<u8>> {
    let mut sdu = Vec::with_capacity(4 + payload.len());
    sdu.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    sdu.extend_from_slice(&cid.to_le_bytes());
    sdu.extend_from_slice(payload);
    sdu.chunks(max_acl.max(1))
        .enumerate()
        .map(|(i, chunk)| build_acl(handle, if i == 0 { 0b00 } else { 0b01 }, chunk))
        .collect()
}

/// LE signalling commands we handle.
pub mod sig {
    pub const COMMAND_REJECT: u8 = 0x01;
    pub const CONNECTION_PARAMETER_UPDATE_REQUEST: u8 = 0x12;
    pub const CONNECTION_PARAMETER_UPDATE_RESPONSE: u8 = 0x13;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionParameterUpdate {
    pub identifier: u8,
    pub interval_min: u16,
    pub interval_max: u16,
    pub latency: u16,
    pub timeout: u16,
}

/// Parses an LE signalling payload; only parameter-update requests are of interest.
pub fn parse_signaling(p: &[u8]) -> Option<ConnectionParameterUpdate> {
    if u8_at(p, 0)? != sig::CONNECTION_PARAMETER_UPDATE_REQUEST || u16_at(p, 2)? != 8 {
        return None;
    }
    Some(ConnectionParameterUpdate {
        identifier: u8_at(p, 1)?,
        interval_min: u16_at(p, 4)?,
        interval_max: u16_at(p, 6)?,
        latency: u16_at(p, 8)?,
        timeout: u16_at(p, 10)?,
    })
}

/// Builds the response (0 = accepted, 1 = rejected).
pub fn signaling_update_response(identifier: u8, accepted: bool) -> Vec<u8> {
    alloc::vec![
        sig::CONNECTION_PARAMETER_UPDATE_RESPONSE,
        identifier,
        2,
        0,
        u8::from(!accepted),
        0
    ]
}

/// Valid LE connection parameters (Core Spec Vol 3, Part A, 4.20 / Vol 6 Part B 4.5.1).
pub fn valid_connection_parameters(p: &ConnectionParameterUpdate) -> bool {
    (6..=3200).contains(&p.interval_min)
        && (p.interval_min..=3200).contains(&p.interval_max)
        && p.latency <= 499
        && (10..=3200).contains(&p.timeout)
        && u32::from(p.timeout) * 4
            > (1 + u32::from(p.latency)) * u32::from(p.interval_max) * 2 / 10
}
