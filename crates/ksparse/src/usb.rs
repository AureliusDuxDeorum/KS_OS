// SPDX-License-Identifier: Apache-2.0
//! USB descriptors and control-request helpers.
//!
//! Descriptors come from devices (untrusted): every length field is validated, walks are
//! bounded, and malformed descriptors end parsing instead of panicking.

use crate::bytes::{u16_at, u8_at};
use alloc::vec::Vec;

pub const DT_DEVICE: u8 = 1;
pub const DT_CONFIG: u8 = 2;
pub const DT_STRING: u8 = 3;
pub const DT_INTERFACE: u8 = 4;
pub const DT_ENDPOINT: u8 = 5;
pub const DT_HID: u8 = 0x21;
pub const DT_REPORT: u8 = 0x22;
pub const DT_HUB: u8 = 0x29;
pub const DT_SS_ENDPOINT_COMPANION: u8 = 0x30;

pub const CLASS_HID: u8 = 3;
pub const CLASS_MASS_STORAGE: u8 = 8;
pub const CLASS_HUB: u8 = 9;
pub const CLASS_WIRELESS: u8 = 0xE0;

/// Standard request codes.
pub const REQ_GET_STATUS: u8 = 0;
pub const REQ_CLEAR_FEATURE: u8 = 1;
pub const REQ_SET_FEATURE: u8 = 3;
pub const REQ_SET_ADDRESS: u8 = 5;
pub const REQ_GET_DESCRIPTOR: u8 = 6;
pub const REQ_SET_CONFIGURATION: u8 = 9;
pub const REQ_SET_INTERFACE: u8 = 11;
/// HID class requests.
pub const REQ_HID_SET_IDLE: u8 = 0x0A;
pub const REQ_HID_SET_PROTOCOL: u8 = 0x0B;

/// An 8-byte SETUP packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl Setup {
    pub const DIR_IN: u8 = 0x80;
    pub const TYPE_CLASS: u8 = 0x20;
    pub const RECIP_INTERFACE: u8 = 0x01;
    pub const RECIP_OTHER: u8 = 0x03;

    pub fn is_in(&self) -> bool {
        self.request_type & Self::DIR_IN != 0
    }

    /// The packet as the little-endian 64-bit value placed in a setup TRB.
    pub fn to_u64(&self) -> u64 {
        u64::from(self.request_type)
            | (u64::from(self.request) << 8)
            | (u64::from(self.value) << 16)
            | (u64::from(self.index) << 32)
            | (u64::from(self.length) << 48)
    }

    pub fn get_descriptor(kind: u8, index: u8, length: u16) -> Self {
        Self {
            request_type: 0x80,
            request: REQ_GET_DESCRIPTOR,
            value: (u16::from(kind) << 8) | u16::from(index),
            index: 0,
            length,
        }
    }

    pub fn set_configuration(value: u8) -> Self {
        Self {
            request_type: 0x00,
            request: REQ_SET_CONFIGURATION,
            value: u16::from(value),
            index: 0,
            length: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceDescriptor {
    pub usb_bcd: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub max_packet0: u8,
    pub vendor: u16,
    pub product: u16,
    pub device_bcd: u16,
    pub num_configs: u8,
}

pub fn parse_device(b: &[u8]) -> Option<DeviceDescriptor> {
    if u8_at(b, 0)? < 18 || u8_at(b, 1)? != DT_DEVICE {
        return None;
    }
    Some(DeviceDescriptor {
        usb_bcd: u16_at(b, 2)?,
        class: u8_at(b, 4)?,
        subclass: u8_at(b, 5)?,
        protocol: u8_at(b, 6)?,
        max_packet0: u8_at(b, 7)?,
        vendor: u16_at(b, 8)?,
        product: u16_at(b, 10)?,
        device_bcd: u16_at(b, 12)?,
        num_configs: u8_at(b, 17)?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    Control,
    Isochronous,
    Bulk,
    Interrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub address: u8,
    pub attributes: u8,
    pub max_packet: u16,
    pub interval: u8,
    /// SuperSpeed companion: additional packets per burst (0 if absent).
    pub max_burst: u8,
}

impl Endpoint {
    pub fn number(&self) -> u8 {
        self.address & 0x0F
    }
    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }
    pub fn kind(&self) -> TransferKind {
        match self.attributes & 3 {
            0 => TransferKind::Control,
            1 => TransferKind::Isochronous,
            2 => TransferKind::Bulk,
            _ => TransferKind::Interrupt,
        }
    }
    /// Packet size in bytes (bits 10:0 of `wMaxPacketSize`).
    pub fn packet_size(&self) -> u16 {
        self.max_packet & 0x7FF
    }
    /// Additional transactions per microframe for high-bandwidth endpoints (bits 12:11).
    pub fn extra_transactions(&self) -> u16 {
        (self.max_packet >> 11) & 3
    }
    /// xHCI device-context index: `2 * number + (IN ? 1 : 0)`.
    pub fn dci(&self) -> u8 {
        self.number()
            .saturating_mul(2)
            .saturating_add(u8::from(self.is_in()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub number: u8,
    pub alt: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoints: Vec<Endpoint>,
    /// HID: length of the report descriptor announced by the HID descriptor.
    pub hid_report_len: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDescriptor {
    pub value: u8,
    pub attributes: u8,
    pub max_power: u8,
    pub interfaces: Vec<Interface>,
}

/// Maximum descriptors walked in one configuration (guards against crafted lengths).
const MAX_DESCRIPTORS: usize = 512;

pub fn parse_config(b: &[u8]) -> Option<ConfigDescriptor> {
    if u8_at(b, 0)? < 9 || u8_at(b, 1)? != DT_CONFIG {
        return None;
    }
    let total = usize::from(u16_at(b, 2)?).min(b.len());
    let b = b.get(..total)?;
    let mut cfg = ConfigDescriptor {
        value: u8_at(b, 5)?,
        attributes: u8_at(b, 7)?,
        max_power: u8_at(b, 8)?,
        interfaces: Vec::new(),
    };
    let mut off = usize::from(u8_at(b, 0)?);
    for _ in 0..MAX_DESCRIPTORS {
        let (Some(len), Some(kind)) = (u8_at(b, off), u8_at(b, off.saturating_add(1))) else {
            break;
        };
        let len = usize::from(len);
        let Some(d) = b.get(off..off.saturating_add(len)).filter(|_| len >= 2) else {
            break;
        };
        let at = |i: usize| u8_at(d, i).unwrap_or(0);
        match kind {
            DT_INTERFACE if len >= 9 => cfg.interfaces.push(Interface {
                number: at(2),
                alt: at(3),
                class: at(5),
                subclass: at(6),
                protocol: at(7),
                endpoints: Vec::new(),
                hid_report_len: None,
            }),
            DT_ENDPOINT if len >= 7 => {
                if let Some(i) = cfg.interfaces.last_mut() {
                    i.endpoints.push(Endpoint {
                        address: at(2),
                        attributes: at(3),
                        max_packet: u16_at(d, 4).unwrap_or(0),
                        interval: at(6),
                        max_burst: 0,
                    });
                }
            }
            DT_SS_ENDPOINT_COMPANION if len >= 6 => {
                if let Some(e) = cfg
                    .interfaces
                    .last_mut()
                    .and_then(|i| i.endpoints.last_mut())
                {
                    e.max_burst = at(2);
                }
            }
            DT_HID if len >= 9 => {
                // bNumDescriptors at 5, then (type, length) pairs from 6.
                let n = usize::from(at(5));
                for k in 0..n {
                    let base = k.saturating_mul(3);
                    let (t, l) = (
                        u8_at(d, base.saturating_add(6)),
                        u16_at(d, base.saturating_add(7)),
                    );
                    if let (Some(DT_REPORT), Some(l), Some(i)) = (t, l, cfg.interfaces.last_mut()) {
                        i.hid_report_len = Some(l);
                    }
                }
            }
            _ => {}
        }
        off = off.saturating_add(len);
    }
    Some(cfg)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubDescriptor {
    pub ports: u8,
    pub characteristics: u16,
    /// Time from power-on to power-good, in 2 ms units.
    pub power_on_good: u8,
}

pub fn parse_hub(b: &[u8]) -> Option<HubDescriptor> {
    if u8_at(b, 0)? < 7 || u8_at(b, 1)? != DT_HUB {
        return None;
    }
    let ports = u8_at(b, 2)?;
    (1..=127).contains(&ports).then_some(())?;
    Some(HubDescriptor {
        ports,
        characteristics: u16_at(b, 3)?,
        power_on_good: u8_at(b, 5)?,
    })
}

/// Decoded hub port status (`wPortStatus` | `wPortChange << 16`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubPortStatus {
    pub connected: bool,
    pub enabled: bool,
    pub reset: bool,
    pub power: bool,
    pub low_speed: bool,
    pub high_speed: bool,
    pub connect_changed: bool,
    pub reset_changed: bool,
}

impl HubPortStatus {
    pub fn from_raw(raw: u32) -> Self {
        let bit = |n: u32| raw >> n & 1 != 0;
        Self {
            connected: bit(0),
            enabled: bit(1),
            reset: bit(4),
            power: bit(8),
            low_speed: bit(9),
            high_speed: bit(10),
            connect_changed: bit(16),
            reset_changed: bit(20),
        }
    }
}

pub const FEATURE_PORT_RESET: u16 = 4;
pub const FEATURE_PORT_POWER: u16 = 8;
pub const FEATURE_C_PORT_CONNECTION: u16 = 16;
pub const FEATURE_C_PORT_RESET: u16 = 20;

/// Port speed IDs as used by xHCI (PORTSC speed field, slot context speed).
pub const SPEED_FULL: u8 = 1;
pub const SPEED_LOW: u8 = 2;
pub const SPEED_HIGH: u8 = 3;
pub const SPEED_SUPER: u8 = 4;

/// Initial EP0 max packet size for a device of the given speed.
pub fn default_ep0_packet_size(speed: u8) -> u16 {
    match speed {
        SPEED_LOW | SPEED_FULL => 8,
        SPEED_HIGH => 64,
        _ => 512,
    }
}

/// Converts the `bMaxPacketSize0` field to bytes (SuperSpeed encodes it as a power of two).
pub fn ep0_packet_size(speed: u8, field: u8) -> Option<u16> {
    if speed >= SPEED_SUPER {
        (field == 9).then_some(512)
    } else {
        matches!(field, 8 | 16 | 32 | 64).then_some(u16::from(field))
    }
}

/// xHCI endpoint-context `Interval` (period = 2^n * 125 us) for a descriptor `bInterval`.
pub fn interval_exponent(speed: u8, b_interval: u8) -> u8 {
    match speed {
        SPEED_LOW | SPEED_FULL => {
            // bInterval counts 1 ms frames = 8 microframes; round down to a power of two.
            let ms = u32::from(b_interval.max(1));
            let units = ms.saturating_mul(8);
            31u32.saturating_sub(units.leading_zeros()).min(10) as u8
        }
        _ => b_interval.clamp(1, 16).saturating_sub(1),
    }
}
