// SPDX-License-Identifier: Apache-2.0
//! ATT (Core Spec Vol 3, Part F): PDU opcodes, error codes, UUIDs and PDU builders.

use alloc::vec::Vec;
use ksparse::bytes::{u16_at, u8_at};

pub const OP_ERROR_RSP: u8 = 0x01;
pub const OP_MTU_REQ: u8 = 0x02;
pub const OP_MTU_RSP: u8 = 0x03;
pub const OP_FIND_INFO_REQ: u8 = 0x04;
pub const OP_FIND_INFO_RSP: u8 = 0x05;
pub const OP_READ_BY_TYPE_REQ: u8 = 0x08;
pub const OP_READ_BY_TYPE_RSP: u8 = 0x09;
pub const OP_READ_REQ: u8 = 0x0A;
pub const OP_READ_RSP: u8 = 0x0B;
pub const OP_READ_BLOB_REQ: u8 = 0x0C;
pub const OP_READ_BLOB_RSP: u8 = 0x0D;
pub const OP_READ_BY_GROUP_TYPE_REQ: u8 = 0x10;
pub const OP_READ_BY_GROUP_TYPE_RSP: u8 = 0x11;
pub const OP_WRITE_REQ: u8 = 0x12;
pub const OP_WRITE_RSP: u8 = 0x13;
pub const OP_HANDLE_VALUE_NTF: u8 = 0x1B;
pub const OP_HANDLE_VALUE_IND: u8 = 0x1D;
pub const OP_HANDLE_VALUE_CONF: u8 = 0x1E;
pub const OP_WRITE_CMD: u8 = 0x52;

pub const ERR_INSUFFICIENT_AUTHENTICATION: u8 = 0x05;
pub const ERR_ATTRIBUTE_NOT_FOUND: u8 = 0x0A;
pub const ERR_INSUFFICIENT_ENCRYPTION: u8 = 0x0F;

/// The default ATT MTU on LE.
pub const DEFAULT_MTU: u16 = 23;

/// A 16- or 128-bit attribute type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uuid {
    Short(u16),
    Long([u8; 16]),
}

impl Uuid {
    /// Decodes a 2- or 16-byte little-endian UUID.
    pub fn from_le(b: &[u8]) -> Option<Self> {
        match b.len() {
            2 => Some(Self::Short(u16::from_le_bytes(b.try_into().ok()?))),
            16 => {
                let a: [u8; 16] = b.try_into().ok()?;
                Some(Self::Long(a))
            }
            _ => None,
        }
    }

    pub fn short(&self) -> Option<u16> {
        match self {
            Self::Short(s) => Some(*s),
            Self::Long(_) => None,
        }
    }
}

/// Well-known 16-bit UUIDs.
pub mod uuid {
    pub const PRIMARY_SERVICE: u16 = 0x2800;
    pub const CHARACTERISTIC: u16 = 0x2803;
    pub const CLIENT_CHARACTERISTIC_CONFIG: u16 = 0x2902;
    pub const HID_SERVICE: u16 = 0x1812;
    pub const HID_INFORMATION: u16 = 0x2A4A;
    pub const REPORT_MAP: u16 = 0x2A4B;
    pub const HID_CONTROL_POINT: u16 = 0x2A4C;
    pub const REPORT: u16 = 0x2A4D;
    pub const PROTOCOL_MODE: u16 = 0x2A4E;
    pub const REPORT_REFERENCE: u16 = 0x2908;
}

pub fn mtu_request(mtu: u16) -> Vec<u8> {
    let mut v = alloc::vec![OP_MTU_REQ];
    v.extend_from_slice(&mtu.to_le_bytes());
    v
}

pub fn read_by_group_type(start: u16, end: u16, group: u16) -> Vec<u8> {
    let mut v = alloc::vec![OP_READ_BY_GROUP_TYPE_REQ];
    v.extend_from_slice(&start.to_le_bytes());
    v.extend_from_slice(&end.to_le_bytes());
    v.extend_from_slice(&group.to_le_bytes());
    v
}

pub fn read_by_type(start: u16, end: u16, kind: u16) -> Vec<u8> {
    let mut v = alloc::vec![OP_READ_BY_TYPE_REQ];
    v.extend_from_slice(&start.to_le_bytes());
    v.extend_from_slice(&end.to_le_bytes());
    v.extend_from_slice(&kind.to_le_bytes());
    v
}

pub fn find_information(start: u16, end: u16) -> Vec<u8> {
    let mut v = alloc::vec![OP_FIND_INFO_REQ];
    v.extend_from_slice(&start.to_le_bytes());
    v.extend_from_slice(&end.to_le_bytes());
    v
}

pub fn read(handle: u16) -> Vec<u8> {
    let mut v = alloc::vec![OP_READ_REQ];
    v.extend_from_slice(&handle.to_le_bytes());
    v
}

pub fn read_blob(handle: u16, offset: u16) -> Vec<u8> {
    let mut v = alloc::vec![OP_READ_BLOB_REQ];
    v.extend_from_slice(&handle.to_le_bytes());
    v.extend_from_slice(&offset.to_le_bytes());
    v
}

pub fn write_request(handle: u16, value: &[u8]) -> Vec<u8> {
    let mut v = alloc::vec![OP_WRITE_REQ];
    v.extend_from_slice(&handle.to_le_bytes());
    v.extend_from_slice(value);
    v
}

/// Decodes an ATT Error Response into `(request opcode, handle, error code)`.
pub fn parse_error(p: &[u8]) -> Option<(u8, u16, u8)> {
    if u8_at(p, 0)? != OP_ERROR_RSP {
        return None;
    }
    Some((u8_at(p, 1)?, u16_at(p, 2)?, u8_at(p, 4)?))
}

/// True for PDUs the server sends unprompted (not responses to our requests).
pub fn is_server_initiated(op: u8) -> bool {
    matches!(op, OP_HANDLE_VALUE_NTF | OP_HANDLE_VALUE_IND)
}
