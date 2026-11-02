// SPDX-License-Identifier: Apache-2.0
//! HCI (Core Spec Vol 4, Part E): command builders, event/ACL parsing, advertising data.
//!
//! All parsers take untrusted controller output and return `None` rather than panic.

use crate::Address;
use alloc::vec::Vec;
use ksparse::bytes::{u16_at, u32_at, u8_at};

pub const fn opcode(ogf: u16, ocf: u16) -> u16 {
    (ogf << 10) | ocf
}

pub mod op {
    use super::opcode;
    pub const DISCONNECT: u16 = opcode(0x01, 0x0006);
    pub const SET_EVENT_MASK: u16 = opcode(0x03, 0x0001);
    pub const RESET: u16 = opcode(0x03, 0x0003);
    pub const WRITE_LE_HOST_SUPPORTED: u16 = opcode(0x03, 0x006D);
    pub const READ_LOCAL_VERSION: u16 = opcode(0x04, 0x0001);
    pub const READ_LOCAL_COMMANDS: u16 = opcode(0x04, 0x0002);
    pub const READ_LOCAL_FEATURES: u16 = opcode(0x04, 0x0003);
    pub const READ_BD_ADDR: u16 = opcode(0x04, 0x0009);
    pub const LE_SET_EVENT_MASK: u16 = opcode(0x08, 0x0001);
    pub const LE_READ_BUFFER_SIZE: u16 = opcode(0x08, 0x0002);
    pub const LE_READ_LOCAL_FEATURES: u16 = opcode(0x08, 0x0003);
    pub const LE_SET_SCAN_PARAMETERS: u16 = opcode(0x08, 0x000B);
    pub const LE_SET_SCAN_ENABLE: u16 = opcode(0x08, 0x000C);
    pub const LE_CREATE_CONNECTION: u16 = opcode(0x08, 0x000D);
    pub const LE_CREATE_CONNECTION_CANCEL: u16 = opcode(0x08, 0x000E);
    pub const LE_CONNECTION_UPDATE: u16 = opcode(0x08, 0x0013);
    pub const LE_START_ENCRYPTION: u16 = opcode(0x08, 0x0019);
    pub const LE_LTK_REPLY: u16 = opcode(0x08, 0x001A);
    pub const LE_LTK_NEGATIVE_REPLY: u16 = opcode(0x08, 0x001B);
}

pub mod evt {
    pub const DISCONNECTION_COMPLETE: u8 = 0x05;
    pub const ENCRYPTION_CHANGE: u8 = 0x08;
    pub const COMMAND_COMPLETE: u8 = 0x0E;
    pub const COMMAND_STATUS: u8 = 0x0F;
    pub const NUMBER_OF_COMPLETED_PACKETS: u8 = 0x13;
    pub const LE_META: u8 = 0x3E;
}

pub mod le {
    pub const CONNECTION_COMPLETE: u8 = 0x01;
    pub const ADVERTISING_REPORT: u8 = 0x02;
    pub const CONNECTION_UPDATE_COMPLETE: u8 = 0x03;
    pub const LTK_REQUEST: u8 = 0x05;
    pub const ENHANCED_CONNECTION_COMPLETE: u8 = 0x0A;
}

pub const STATUS_SUCCESS: u8 = 0;

/// An HCI command: opcode plus parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub opcode: u16,
    pub params: Vec<u8>,
}

impl Command {
    pub fn new(opcode: u16, params: &[u8]) -> Self {
        Self {
            opcode,
            params: params.to_vec(),
        }
    }

    /// The command packet as sent over USB (no H4 indicator byte).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(3 + self.params.len());
        v.extend_from_slice(&self.opcode.to_le_bytes());
        v.push(self.params.len() as u8);
        v.extend_from_slice(&self.params);
        v
    }

    pub fn reset() -> Self {
        Self::new(op::RESET, &[])
    }
    pub fn read_local_version() -> Self {
        Self::new(op::READ_LOCAL_VERSION, &[])
    }
    pub fn read_bd_addr() -> Self {
        Self::new(op::READ_BD_ADDR, &[])
    }
    pub fn read_local_commands() -> Self {
        Self::new(op::READ_LOCAL_COMMANDS, &[])
    }
    pub fn le_read_buffer_size() -> Self {
        Self::new(op::LE_READ_BUFFER_SIZE, &[])
    }
    pub fn le_read_local_features() -> Self {
        Self::new(op::LE_READ_LOCAL_FEATURES, &[])
    }
    pub fn set_event_mask(mask: u64) -> Self {
        Self::new(op::SET_EVENT_MASK, &mask.to_le_bytes())
    }
    pub fn le_set_event_mask(mask: u64) -> Self {
        Self::new(op::LE_SET_EVENT_MASK, &mask.to_le_bytes())
    }
    pub fn write_le_host_supported(enabled: bool) -> Self {
        Self::new(op::WRITE_LE_HOST_SUPPORTED, &[u8::from(enabled), 0])
    }
    /// Active scanning, `interval`/`window` in 0.625 ms units, public own address.
    pub fn le_set_scan_parameters(active: bool, interval: u16, window: u16) -> Self {
        let mut p = [0u8; 7];
        p[0] = u8::from(active);
        p[1..3].copy_from_slice(&interval.to_le_bytes());
        p[3..5].copy_from_slice(&window.to_le_bytes());
        // own address type = public (0), filter policy = accept all (0)
        Self::new(op::LE_SET_SCAN_PARAMETERS, &p)
    }
    pub fn le_set_scan_enable(enable: bool, filter_duplicates: bool) -> Self {
        Self::new(
            op::LE_SET_SCAN_ENABLE,
            &[u8::from(enable), u8::from(filter_duplicates)],
        )
    }
    /// Initiates a connection with the given parameters (units: interval 1.25 ms, timeout 10 ms).
    pub fn le_create_connection(
        peer: Address,
        interval: u16,
        latency: u16,
        supervision_timeout: u16,
    ) -> Self {
        let mut p = Vec::with_capacity(25);
        p.extend_from_slice(&0x0060u16.to_le_bytes()); // scan interval
        p.extend_from_slice(&0x0030u16.to_le_bytes()); // scan window
        p.push(0); // initiator filter policy: use peer address
        p.push(peer.kind);
        p.extend_from_slice(&peer.bytes);
        p.push(0); // own address type: public
        p.extend_from_slice(&interval.to_le_bytes()); // min
        p.extend_from_slice(&interval.to_le_bytes()); // max
        p.extend_from_slice(&latency.to_le_bytes());
        p.extend_from_slice(&supervision_timeout.to_le_bytes());
        p.extend_from_slice(&0u16.to_le_bytes()); // min CE length
        p.extend_from_slice(&0u16.to_le_bytes()); // max CE length
        Self::new(op::LE_CREATE_CONNECTION, &p)
    }
    pub fn le_create_connection_cancel() -> Self {
        Self::new(op::LE_CREATE_CONNECTION_CANCEL, &[])
    }
    pub fn disconnect(handle: u16, reason: u8) -> Self {
        let mut p = [0u8; 3];
        p[..2].copy_from_slice(&handle.to_le_bytes());
        p[2] = reason;
        Self::new(op::DISCONNECT, &p)
    }
    pub fn le_start_encryption(handle: u16, rand: [u8; 8], ediv: u16, ltk: [u8; 16]) -> Self {
        let mut p = Vec::with_capacity(28);
        p.extend_from_slice(&handle.to_le_bytes());
        p.extend_from_slice(&rand);
        p.extend_from_slice(&ediv.to_le_bytes());
        p.extend_from_slice(&ltk);
        Self::new(op::LE_START_ENCRYPTION, &p)
    }
    pub fn le_ltk_reply(handle: u16, ltk: [u8; 16]) -> Self {
        let mut p = Vec::with_capacity(18);
        p.extend_from_slice(&handle.to_le_bytes());
        p.extend_from_slice(&ltk);
        Self::new(op::LE_LTK_REPLY, &p)
    }
    pub fn le_ltk_negative_reply(handle: u16) -> Self {
        Self::new(op::LE_LTK_NEGATIVE_REPLY, &handle.to_le_bytes())
    }
    /// Replies to the peripheral's connection parameter update request.
    pub fn le_connection_update(handle: u16, interval: u16, latency: u16, timeout: u16) -> Self {
        let mut p = Vec::with_capacity(14);
        p.extend_from_slice(&handle.to_le_bytes());
        p.extend_from_slice(&interval.to_le_bytes());
        p.extend_from_slice(&interval.to_le_bytes());
        p.extend_from_slice(&latency.to_le_bytes());
        p.extend_from_slice(&timeout.to_le_bytes());
        p.extend_from_slice(&[0; 4]);
        Self::new(op::LE_CONNECTION_UPDATE, &p)
    }
}

/// One LE advertising report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvReport {
    pub event_type: u8,
    pub address: Address,
    pub data: Vec<u8>,
    pub rssi: i8,
}

/// Decoded HCI events (the ones this stack acts on).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    CommandComplete {
        opcode: u16,
        status: u8,
        ret: Vec<u8>,
    },
    CommandStatus {
        opcode: u16,
        status: u8,
    },
    Disconnection {
        status: u8,
        handle: u16,
        reason: u8,
    },
    EncryptionChange {
        status: u8,
        handle: u16,
        enabled: bool,
    },
    NumCompletedPackets(Vec<(u16, u16)>),
    LeConnectionComplete {
        status: u8,
        handle: u16,
        role: u8,
        peer: Address,
        interval: u16,
        latency: u16,
        timeout: u16,
    },
    LeAdvertisingReports(Vec<AdvReport>),
    LeLtkRequest {
        handle: u16,
        rand: [u8; 8],
        ediv: u16,
    },
    LeConnectionUpdate {
        status: u8,
        handle: u16,
    },
    Other {
        code: u8,
    },
}

fn address_at(b: &[u8], kind_off: usize) -> Option<Address> {
    let kind = u8_at(b, kind_off)?;
    let mut bytes = [0u8; 6];
    bytes.copy_from_slice(b.get(kind_off + 1..kind_off + 7)?);
    Some(Address::new(kind, bytes))
}

/// Parses an event packet (`code`, `length`, parameters).
pub fn parse_event(pkt: &[u8]) -> Option<Event> {
    let code = u8_at(pkt, 0)?;
    let len = usize::from(u8_at(pkt, 1)?);
    let p = pkt.get(2..2 + len)?;
    Some(match code {
        evt::COMMAND_COMPLETE => {
            // num_commands, opcode, return parameters (first byte is usually the status)
            let opcode = u16_at(p, 1)?;
            Event::CommandComplete {
                opcode,
                status: u8_at(p, 3).unwrap_or(0),
                ret: p.get(4..).unwrap_or(&[]).to_vec(),
            }
        }
        evt::COMMAND_STATUS => Event::CommandStatus {
            status: u8_at(p, 0)?,
            opcode: u16_at(p, 2)?,
        },
        evt::DISCONNECTION_COMPLETE => Event::Disconnection {
            status: u8_at(p, 0)?,
            handle: u16_at(p, 1)? & 0x0FFF,
            reason: u8_at(p, 3)?,
        },
        evt::ENCRYPTION_CHANGE => Event::EncryptionChange {
            status: u8_at(p, 0)?,
            handle: u16_at(p, 1)? & 0x0FFF,
            enabled: u8_at(p, 3)? != 0,
        },
        evt::NUMBER_OF_COMPLETED_PACKETS => {
            let n = usize::from(u8_at(p, 0)?);
            let mut v = Vec::new();
            for i in 0..n {
                v.push((u16_at(p, 1 + 4 * i)? & 0x0FFF, u16_at(p, 3 + 4 * i)?));
            }
            Event::NumCompletedPackets(v)
        }
        evt::LE_META => {
            let sub = u8_at(p, 0)?;
            let p = p.get(1..)?;
            match sub {
                le::CONNECTION_COMPLETE => Event::LeConnectionComplete {
                    status: u8_at(p, 0)?,
                    handle: u16_at(p, 1)? & 0x0FFF,
                    role: u8_at(p, 3)?,
                    peer: address_at(p, 4)?,
                    interval: u16_at(p, 11)?,
                    latency: u16_at(p, 13)?,
                    timeout: u16_at(p, 15)?,
                },
                le::ENHANCED_CONNECTION_COMPLETE => Event::LeConnectionComplete {
                    // status, handle, role, peer type, peer addr, local RPA, peer RPA, interval, latency, timeout
                    status: u8_at(p, 0)?,
                    handle: u16_at(p, 1)? & 0x0FFF,
                    role: u8_at(p, 3)?,
                    peer: address_at(p, 4)?,
                    interval: u16_at(p, 17)?,
                    latency: u16_at(p, 19)?,
                    timeout: u16_at(p, 21)?,
                },
                le::ADVERTISING_REPORT => {
                    let n = usize::from(u8_at(p, 0)?);
                    let mut reports = Vec::new();
                    let mut off = 1;
                    for _ in 0..n.min(16) {
                        let event_type = u8_at(p, off)?;
                        let address = address_at(p, off + 1)?;
                        let dlen = usize::from(u8_at(p, off + 8)?);
                        let data = p.get(off + 9..off + 9 + dlen)?.to_vec();
                        let rssi = u8_at(p, off + 9 + dlen)? as i8;
                        reports.push(AdvReport {
                            event_type,
                            address,
                            data,
                            rssi,
                        });
                        off += 10 + dlen;
                    }
                    Event::LeAdvertisingReports(reports)
                }
                le::LTK_REQUEST => {
                    let mut rand = [0u8; 8];
                    rand.copy_from_slice(p.get(2..10)?);
                    Event::LeLtkRequest {
                        handle: u16_at(p, 0)? & 0x0FFF,
                        rand,
                        ediv: u16_at(p, 10)?,
                    }
                }
                le::CONNECTION_UPDATE_COMPLETE => Event::LeConnectionUpdate {
                    status: u8_at(p, 0)?,
                    handle: u16_at(p, 1)? & 0x0FFF,
                },
                _ => Event::Other { code },
            }
        }
        _ => Event::Other { code },
    })
}

/// AD structure types used by this stack.
pub mod ad {
    pub const FLAGS: u8 = 0x01;
    pub const INCOMPLETE_UUID16: u8 = 0x02;
    pub const COMPLETE_UUID16: u8 = 0x03;
    pub const SHORT_NAME: u8 = 0x08;
    pub const COMPLETE_NAME: u8 = 0x09;
    pub const APPEARANCE: u8 = 0x19;
}

/// Flags: LE Limited / General Discoverable Mode (the device is in pairing mode).
pub const AD_FLAG_DISCOVERABLE: u8 = 0b11;

/// What an advertisement tells us about a device.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AdvInfo {
    pub flags: Option<u8>,
    pub name: Option<Vec<u8>>,
    pub appearance: Option<u16>,
    pub services16: Vec<u16>,
}

impl AdvInfo {
    pub fn discoverable(&self) -> bool {
        self.flags.is_some_and(|f| f & AD_FLAG_DISCOVERABLE != 0)
    }

    /// Advertises the HID service (0x1812) or a keyboard/mouse/other HID appearance (0x03C0..=0x03C4).
    pub fn is_hid(&self) -> bool {
        self.services16.contains(&0x1812)
            || self
                .appearance
                .is_some_and(|a| (0x03C0..=0x03C4).contains(&a))
    }

    pub fn name_lossy(&self) -> alloc::string::String {
        self.name
            .as_deref()
            .map(|n| alloc::string::String::from_utf8_lossy(n).into_owned())
            .unwrap_or_default()
    }
}

/// Walks the AD structures of advertising data, stopping at the first malformed one.
pub fn parse_adv_data(data: &[u8]) -> AdvInfo {
    let mut info = AdvInfo::default();
    let mut off = 0;
    while let Some(&len) = data.get(off) {
        let len = usize::from(len);
        if len == 0 {
            break;
        }
        let Some(s) = data.get(off + 1..off + 1 + len) else {
            break;
        };
        let (kind, body) = (s[0], &s[1..]);
        match kind {
            ad::FLAGS => info.flags = body.first().copied(),
            ad::SHORT_NAME | ad::COMPLETE_NAME => info.name = Some(body.to_vec()),
            ad::APPEARANCE => info.appearance = u16_at(body, 0),
            ad::INCOMPLETE_UUID16 | ad::COMPLETE_UUID16 => info.services16.extend(
                body.chunks_exact(2)
                    .take(16)
                    .map(|c| u16::from_le_bytes([c[0], c[1]])),
            ),
            _ => {}
        }
        off += 1 + len;
    }
    info
}

/// ACL data packet header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AclHeader {
    pub handle: u16,
    /// Packet boundary flag: 0b00 / 0b10 = start, 0b01 = continuation.
    pub pb: u8,
    pub len: u16,
}

pub fn parse_acl(pkt: &[u8]) -> Option<(AclHeader, &[u8])> {
    let hw = u16_at(pkt, 0)?;
    let len = u16_at(pkt, 2)?;
    let data = pkt.get(4..4 + usize::from(len))?;
    Some((
        AclHeader {
            handle: hw & 0x0FFF,
            pb: ((hw >> 12) & 3) as u8,
            len,
        },
        data,
    ))
}

pub fn build_acl(handle: u16, pb: u8, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + data.len());
    v.extend_from_slice(&((handle & 0x0FFF) | (u16::from(pb & 3) << 12)).to_le_bytes());
    v.extend_from_slice(&(data.len() as u16).to_le_bytes());
    v.extend_from_slice(data);
    v
}

/// Result of Read Local Version Information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalVersion {
    pub hci_version: u8,
    pub hci_revision: u16,
    pub lmp_version: u8,
    pub manufacturer: u16,
    pub lmp_subversion: u16,
}

pub fn parse_local_version(ret: &[u8]) -> Option<LocalVersion> {
    Some(LocalVersion {
        hci_version: u8_at(ret, 0)?,
        hci_revision: u16_at(ret, 1)?,
        lmp_version: u8_at(ret, 3)?,
        manufacturer: u16_at(ret, 4)?,
        lmp_subversion: u16_at(ret, 6)?,
    })
}

/// LE buffer size: `(max ACL data length, number of ACL buffers)`.
pub fn parse_le_buffer_size(ret: &[u8]) -> Option<(u16, u8)> {
    Some((u16_at(ret, 0)?, u8_at(ret, 2)?))
}

/// Reads a `u32` return parameter (used by vendor commands).
pub fn ret_u32(ret: &[u8], off: usize) -> Option<u32> {
    u32_at(ret, off)
}
