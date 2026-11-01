//! GATT client procedures over an ATT link: service, characteristic and descriptor discovery,
//! long reads and writes.
//!
//! Every loop is bounded and requires forward progress, so a hostile or buggy peer cannot make
//! discovery spin forever.

use crate::att::{self, Uuid};
use alloc::vec::Vec;

/// Failure of the underlying link or of an ATT procedure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Timeout,
    Disconnected,
    /// The peer answered with an ATT error (code).
    Att(u8),
    /// The response did not have the expected shape.
    Malformed,
}

/// A synchronous ATT request/response channel to one connected peer.
pub trait AttLink {
    /// Sends `pdu` and returns the matching response (notifications are queued elsewhere).
    fn request(&mut self, pdu: &[u8]) -> Result<Vec<u8>, Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Service {
    pub start: u16,
    pub end: u16,
    pub uuid: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Characteristic {
    pub decl_handle: u16,
    pub properties: u8,
    pub value_handle: u16,
    pub uuid: Uuid,
}

pub const PROP_NOTIFY: u8 = 0x10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    pub handle: u16,
    pub uuid: Uuid,
}

const MAX_ROUNDS: usize = 128;

fn check_response(rsp: &[u8], expect: u8) -> Result<(), Error> {
    match rsp.first() {
        Some(&op) if op == expect => Ok(()),
        Some(&att::OP_ERROR_RSP) => Err(Error::Att(att::parse_error(rsp).map_or(0, |e| e.2))),
        _ => Err(Error::Malformed),
    }
}

/// Negotiates the ATT MTU; returns the effective MTU (at least the default).
pub fn exchange_mtu(link: &mut impl AttLink, ours: u16) -> u16 {
    let Ok(rsp) = link.request(&att::mtu_request(ours)) else {
        return att::DEFAULT_MTU;
    };
    if check_response(&rsp, att::OP_MTU_RSP).is_err() || rsp.len() < 3 {
        return att::DEFAULT_MTU;
    }
    u16::from_le_bytes([rsp[1], rsp[2]]).clamp(att::DEFAULT_MTU, ours)
}

/// Discovers all primary services.
pub fn discover_services(link: &mut impl AttLink) -> Result<Vec<Service>, Error> {
    let mut out = Vec::new();
    let mut start = 1u16;
    for _ in 0..MAX_ROUNDS {
        let rsp = match link.request(&att::read_by_group_type(
            start,
            0xFFFF,
            att::uuid::PRIMARY_SERVICE,
        )) {
            Err(Error::Att(att::ERR_ATTRIBUTE_NOT_FOUND)) => break,
            other => other?,
        };
        if rsp.first() == Some(&att::OP_ERROR_RSP) {
            if att::parse_error(&rsp).map(|e| e.2) == Some(att::ERR_ATTRIBUTE_NOT_FOUND) {
                break;
            }
            return Err(Error::Att(att::parse_error(&rsp).map_or(0, |e| e.2)));
        }
        check_response(&rsp, att::OP_READ_BY_GROUP_TYPE_RSP)?;
        let item = usize::from(*rsp.get(1).ok_or(Error::Malformed)?);
        if item < 6 {
            return Err(Error::Malformed);
        }
        let mut last_end = 0;
        for e in rsp[2..].chunks_exact(item) {
            let (s, en) = (
                u16::from_le_bytes([e[0], e[1]]),
                u16::from_le_bytes([e[2], e[3]]),
            );
            if s == 0 || en < s {
                return Err(Error::Malformed);
            }
            out.push(Service {
                start: s,
                end: en,
                uuid: Uuid::from_le(&e[4..]).ok_or(Error::Malformed)?,
            });
            last_end = en;
        }
        if last_end < start || last_end == 0xFFFF {
            break; // no progress, or the last service ends at the top of the handle space
        }
        start = last_end + 1;
    }
    Ok(out)
}

/// Discovers characteristics of `service`.
pub fn discover_characteristics(
    link: &mut impl AttLink,
    service: &Service,
) -> Result<Vec<Characteristic>, Error> {
    let mut out = Vec::new();
    let mut start = service.start;
    for _ in 0..MAX_ROUNDS {
        if start > service.end {
            break;
        }
        let rsp = match link.request(&att::read_by_type(
            start,
            service.end,
            att::uuid::CHARACTERISTIC,
        )) {
            Err(Error::Att(att::ERR_ATTRIBUTE_NOT_FOUND)) => break,
            other => other?,
        };
        if rsp.first() == Some(&att::OP_ERROR_RSP) {
            if att::parse_error(&rsp).map(|e| e.2) == Some(att::ERR_ATTRIBUTE_NOT_FOUND) {
                break;
            }
            return Err(Error::Att(att::parse_error(&rsp).map_or(0, |e| e.2)));
        }
        check_response(&rsp, att::OP_READ_BY_TYPE_RSP)?;
        let item = usize::from(*rsp.get(1).ok_or(Error::Malformed)?);
        if item < 7 {
            return Err(Error::Malformed);
        }
        let mut last = 0;
        for e in rsp[2..].chunks_exact(item) {
            let decl = u16::from_le_bytes([e[0], e[1]]);
            out.push(Characteristic {
                decl_handle: decl,
                properties: e[2],
                value_handle: u16::from_le_bytes([e[3], e[4]]),
                uuid: Uuid::from_le(&e[5..]).ok_or(Error::Malformed)?,
            });
            last = decl;
        }
        if last < start {
            break;
        }
        start = last.saturating_add(1);
        if start == 0 {
            break;
        }
    }
    Ok(out)
}

/// Discovers descriptors in `start..=end`.
pub fn discover_descriptors(
    link: &mut impl AttLink,
    mut start: u16,
    end: u16,
) -> Result<Vec<Descriptor>, Error> {
    let mut out = Vec::new();
    for _ in 0..MAX_ROUNDS {
        if start > end || start == 0 {
            break;
        }
        let rsp = match link.request(&att::find_information(start, end)) {
            Err(Error::Att(att::ERR_ATTRIBUTE_NOT_FOUND)) => break,
            other => other?,
        };
        if rsp.first() == Some(&att::OP_ERROR_RSP) {
            if att::parse_error(&rsp).map(|e| e.2) == Some(att::ERR_ATTRIBUTE_NOT_FOUND) {
                break;
            }
            return Err(Error::Att(att::parse_error(&rsp).map_or(0, |e| e.2)));
        }
        check_response(&rsp, att::OP_FIND_INFO_RSP)?;
        let width = match rsp.get(1) {
            Some(1) => 2,
            Some(2) => 16,
            _ => return Err(Error::Malformed),
        };
        let mut last = 0;
        for e in rsp[2..].chunks_exact(2 + width) {
            let handle = u16::from_le_bytes([e[0], e[1]]);
            out.push(Descriptor {
                handle,
                uuid: Uuid::from_le(&e[2..]).ok_or(Error::Malformed)?,
            });
            last = handle;
        }
        if last < start {
            break;
        }
        start = last.saturating_add(1);
    }
    Ok(out)
}

/// Reads an attribute value of up to `limit` bytes, using Read Blob for long values.
pub fn read_long(
    link: &mut impl AttLink,
    handle: u16,
    mtu: u16,
    limit: usize,
) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let chunk = usize::from(mtu.max(att::DEFAULT_MTU)) - 1;
    for round in 0..MAX_ROUNDS {
        let req = if round == 0 {
            att::read(handle)
        } else {
            att::read_blob(handle, out.len() as u16)
        };
        let rsp = link.request(&req)?;
        check_response(
            &rsp,
            if round == 0 {
                att::OP_READ_RSP
            } else {
                att::OP_READ_BLOB_RSP
            },
        )?;
        let data = &rsp[1..];
        out.extend_from_slice(data);
        if data.len() < chunk || out.len() >= limit {
            break;
        }
    }
    out.truncate(limit);
    Ok(out)
}

/// Writes a value with a Write Request and waits for the response.
pub fn write(link: &mut impl AttLink, handle: u16, value: &[u8]) -> Result<(), Error> {
    let rsp = link.request(&att::write_request(handle, value))?;
    check_response(&rsp, att::OP_WRITE_RSP)
}
