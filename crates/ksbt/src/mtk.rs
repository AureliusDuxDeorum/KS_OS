// SPDX-License-Identifier: Apache-2.0
//! MediaTek Bluetooth controller support: patch-file parsing and the WMT vendor protocol codec.
//!
//! **Hardware-unverified.** The layouts below follow the publicly documented behaviour of the
//! Linux `btmtk` driver for the MT79xx family. They are validated here only against synthetic
//! data; the first real-hardware run is expected to refine them (the kernel logs every step).
//! All parsing is bounds-checked because the patch file is an untrusted input.

use crate::hci::Command;
use alloc::string::String;
use alloc::vec::Vec;
use ksparse::bytes::{u16_at, u32_at, u8_at};

/// HCI opcode that carries WMT (wireless management task) requests.
pub const WMT_OPCODE: u16 = 0xFC6F;
/// Vendor event code that carries WMT responses.
pub const WMT_EVENT: u8 = 0xE4;

pub mod wmt_op {
    pub const PATCH_DWNLD: u8 = 0x01;
    pub const FUNC_CTRL: u8 = 0x06;
    pub const RST: u8 = 0x07;
    pub const SEMAPHORE: u8 = 0x17;
}

/// Vendor control-request encodings for 32-bit register access.
pub const UHW_READ_REQUEST: u8 = 0x01;
pub const UHW_READ_TYPE: u8 = 0xDE;
pub const UHW_WRITE_REQUEST: u8 = 0x02;
pub const UHW_WRITE_TYPE: u8 = 0x5E;

pub const REG_CHIP_ID: u32 = 0x7001_0200;
pub const REG_FW_VERSION: u32 = 0x8002_1004;
pub const REG_FW_FLAVOR: u32 = 0x7001_0020;
pub const REG_EP_RST_OPT: u32 = 0x7C00_0C14;
pub const EP_RST_IN_OUT_OPT: u32 = 0x0001_0001;

const HEADER_SIZE: usize = 32;
const GLOBAL_DESC_SIZE: usize = 64;
const SECTION_MAP_SIZE: usize = 64;
const SECTION_MAP_COMMON: usize = 12;
const SECTION_MAP_SEND: usize = 52;
const MAX_SECTIONS: usize = 16;
/// Largest payload of one patch-download command.
pub const PATCH_CHUNK: usize = 250;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchError {
    TooShort,
    BadSectionCount,
    SectionOutOfRange,
}

/// One firmware section to download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The 53 bytes sent with the section-announce command (`0` = legacy download mode + map data).
    pub announce: [u8; 1 + SECTION_MAP_SEND],
    pub data_offset: usize,
    pub data_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchFile {
    pub build_time: String,
    pub hw_version: u16,
    pub sw_version: u16,
    pub sections: Vec<Section>,
}

/// Parses a `BT_RAM_CODE_MT79xx_*_hdr.bin` patch file.
pub fn parse_patch(file: &[u8]) -> Result<PatchFile, PatchError> {
    if file.len() < HEADER_SIZE + GLOBAL_DESC_SIZE {
        return Err(PatchError::TooShort);
    }
    // Header: 16-byte build time, 4-byte platform, hw/sw version, magic.
    let build_time = String::from_utf8_lossy(&file[..16])
        .trim_end_matches('\0')
        .trim()
        .into();
    let (hw_version, sw_version) = (
        u16_at(file, 20).ok_or(PatchError::TooShort)?,
        u16_at(file, 22).ok_or(PatchError::TooShort)?,
    );
    // Global descriptor follows the header: patch version, subsystem, feature options, section count.
    let section_num = u32_at(file, HEADER_SIZE + 12).ok_or(PatchError::TooShort)? as usize;
    if section_num == 0 || section_num > MAX_SECTIONS {
        return Err(PatchError::BadSectionCount);
    }
    let mut sections = Vec::new();
    for i in 0..section_num {
        let map = HEADER_SIZE + GLOBAL_DESC_SIZE + SECTION_MAP_SIZE * i;
        let entry = file
            .get(map..map + SECTION_MAP_SIZE)
            .ok_or(PatchError::TooShort)?;
        let offset = u32_at(entry, 4).ok_or(PatchError::TooShort)? as usize;
        // bin_info_spec starts after the 12-byte common part: dlAddr, dlsize, ...
        let dl_size = u32_at(entry, SECTION_MAP_COMMON + 4).ok_or(PatchError::TooShort)? as usize;
        if dl_size == 0 {
            continue;
        }
        if offset
            .checked_add(dl_size)
            .is_none_or(|end| end > file.len())
        {
            return Err(PatchError::SectionOutOfRange);
        }
        let mut announce = [0u8; 1 + SECTION_MAP_SEND];
        announce[1..]
            .copy_from_slice(&entry[SECTION_MAP_COMMON..SECTION_MAP_COMMON + SECTION_MAP_SEND]);
        sections.push(Section {
            announce,
            data_offset: offset,
            data_len: dl_size,
        });
    }
    Ok(PatchFile {
        build_time,
        hw_version,
        sw_version,
        sections,
    })
}

impl PatchFile {
    /// The `HW/SW Version` pair as Linux logs it (`0x<hw><sw>`).
    pub fn version_word(&self) -> u32 {
        (u32::from(self.hw_version) << 16) | u32::from(self.sw_version)
    }
}

/// Splits a section into `(flag, data)` download chunks: first chunk flag 1, middle 2, last 3
/// (a section that fits in one chunk is sent with flag 1 only).
pub fn chunks(section_data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    let mut rest = section_data;
    let mut first = true;
    while !rest.is_empty() {
        let n = rest.len().min(PATCH_CHUNK);
        let flag = if first {
            1
        } else if rest.len() - n == 0 {
            3
        } else {
            2
        };
        first = false;
        out.push((flag, &rest[..n]));
        rest = &rest[n..];
    }
    out
}

/// Builds a WMT request as an HCI vendor command.
pub fn wmt_command(op: u8, flag: u8, data: &[u8]) -> Command {
    let mut p = Vec::with_capacity(5 + data.len());
    p.push(0x01); // direction: host to chip
    p.push(op);
    p.extend_from_slice(&((data.len() + 1) as u16).to_le_bytes()); // length counts the flag byte
    p.push(flag);
    p.extend_from_slice(data);
    Command::new(WMT_OPCODE, &p)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WmtEvent {
    pub op: u8,
    pub flag: u8,
    /// Big-endian status word (present for function-control responses).
    pub status: Option<u16>,
}

/// Parses a WMT response carried by vendor event 0xE4.
pub fn parse_wmt_event(event: &[u8]) -> Option<WmtEvent> {
    if u8_at(event, 0)? != WMT_EVENT {
        return None;
    }
    let len = usize::from(u8_at(event, 1)?);
    let p = event.get(2..2 + len)?;
    // direction, op, dlen (u16), flag, [status (u16, big-endian)]
    let (op, flag) = (u8_at(p, 1)?, u8_at(p, 4)?);
    let status = (p.len() >= 7).then(|| u16::from_be_bytes([p[5], p[6]]));
    Some(WmtEvent { op, flag, status })
}

/// Patch-download progress as reported by the chip in the section-announce response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchState {
    Undone,
    Progress,
    Done,
}

pub fn patch_state(e: &WmtEvent) -> PatchState {
    match e.flag {
        2 => PatchState::Done,
        1 => PatchState::Progress,
        _ => PatchState::Undone,
    }
}

/// The firmware file name Linux selects for this chip.
pub fn firmware_name(chip_id: u32, fw_version: u32, flavor_bit: bool) -> String {
    let id = chip_id & 0xFFFF;
    let a = if id == 0x7961 && flavor_bit { "a" } else { "" };
    alloc::format!(
        "BT_RAM_CODE_MT{id:04X}_1{a}_{:x}_hdr.bin",
        (fw_version & 0xFF) + 1
    )
}
