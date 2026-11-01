//! MediaTek MT79xx Bluetooth bring-up: identify the chip, pick and download the patch firmware
//! through the WMT vendor protocol, then enable the Bluetooth function.
//!
//! **Experimental — unverified on hardware.** Every step is logged (and visible with the `L` key)
//! so the first real-device run can be diagnosed from a photo of the screen. Nothing is written to
//! the controller until the chip identity and the firmware file have both been validated.

use super::transport::UsbTransport;
use crate::boot::Module;
use ksbt::host::{Packet, Transport};
use ksbt::mtk::{self, wmt_op, PatchState, WmtEvent};

#[derive(Debug)]
#[allow(dead_code)]
pub enum MtkError {
    Usb(&'static str),
    UnknownChip(u32),
    FirmwareMissing(alloc::string::String),
    BadFirmware(mtk::PatchError),
    Wmt(&'static str),
}

fn read_reg(t: &UsbTransport, reg: u32) -> Result<u32, MtkError> {
    let mut b = [0u8; 4];
    t.vendor_read(
        mtk::UHW_READ_REQUEST,
        mtk::UHW_READ_TYPE,
        (reg >> 16) as u16,
        reg as u16,
        &mut b,
    )
    .map_err(|_| MtkError::Usb("register read"))?;
    Ok(u32::from_le_bytes(b))
}

fn write_reg(t: &UsbTransport, reg: u32, v: u32) -> Result<(), MtkError> {
    let mut b = v.to_le_bytes();
    t.vendor_write(
        mtk::UHW_WRITE_REQUEST,
        mtk::UHW_WRITE_TYPE,
        (reg >> 16) as u16,
        reg as u16,
        &mut b,
    )
    .map_err(|_| MtkError::Usb("register write"))?;
    Ok(())
}

/// Sends a WMT command and waits for its response event.
fn wmt(t: &mut UsbTransport, op: u8, flag: u8, data: &[u8]) -> Result<WmtEvent, MtkError> {
    let cmd = mtk::wmt_command(op, flag, data);
    t.send_command(&cmd.to_bytes())
        .map_err(|_| MtkError::Usb("WMT command"))?;
    let deadline = t.now_ms() + 2000;
    while t.now_ms() < deadline {
        if let Some(Packet::Event(e)) = t.recv(50) {
            if let Some(w) = mtk::parse_wmt_event(&e) {
                if w.op == op {
                    return Ok(w);
                }
            }
            // Other events (command status etc.) are expected noise before the WMT response.
        }
    }
    Err(MtkError::Wmt("no WMT response"))
}

/// Brings up the controller. `modules` are the firmware files the bootloader loaded.
pub fn setup(t: &mut UsbTransport, modules: &[Module]) -> Result<(), MtkError> {
    // 1. Identify the chip (read-only).
    let chip = read_reg(t, mtk::REG_CHIP_ID)?;
    let fw_version = read_reg(t, mtk::REG_FW_VERSION)?;
    let flavor_raw = read_reg(t, mtk::REG_FW_FLAVOR)?;
    let flavor = (flavor_raw & 0x80) >> 7 != 0;
    kprintln!("bt: MediaTek chip id {chip:#010x}, fw version reg {fw_version:#010x}, flavor reg {flavor_raw:#010x}");
    if chip & 0xFFFF != 0x7961 && chip & 0xFFFF != 0x7922 {
        return Err(MtkError::UnknownChip(chip));
    }
    // 2. Choose and validate the firmware file.
    let name = mtk::firmware_name(chip, fw_version, flavor);
    kprintln!("bt: firmware file wanted: {name}");
    let Some(file) = modules.iter().find(|m| m.name() == name) else {
        let have: alloc::vec::Vec<&str> = modules.iter().map(Module::name).collect();
        kprintln!("bt: firmware modules available: {have:?}");
        return Err(MtkError::FirmwareMissing(name));
    };
    let patch = mtk::parse_patch(file.data).map_err(MtkError::BadFirmware)?;
    kprintln!(
        "bt: firmware {} bytes, HW/SW Version: {:#010x}, Build Time: {}, {} section(s)",
        file.data.len(),
        patch.version_word(),
        patch.build_time,
        patch.sections.len()
    );
    // 3. Download every section the chip does not already have.
    for (i, s) in patch.sections.iter().enumerate() {
        let mut state = PatchState::Progress;
        for _ in 0..20 {
            let ev = wmt(t, wmt_op::PATCH_DWNLD, 0, &s.announce)?;
            state = mtk::patch_state(&ev);
            kprintln!("bt: section {i} announce -> {state:?} (flag {})", ev.flag);
            if state != PatchState::Progress {
                break;
            }
            crate::time::sleep_ms(100);
        }
        match state {
            PatchState::Done => continue,
            PatchState::Progress => return Err(MtkError::Wmt("section stuck in progress")),
            PatchState::Undone => {}
        }
        let data = &file.data[s.data_offset..s.data_offset + s.data_len];
        for (flag, chunk) in mtk::chunks(data) {
            wmt(t, wmt_op::PATCH_DWNLD, flag, chunk)?;
        }
        kprintln!("bt: section {i}: {} bytes downloaded", data.len());
    }
    crate::time::sleep_ms(120); // firmware activation
                                // 4. Endpoint reset option, then enable Bluetooth.
    write_reg(t, mtk::REG_EP_RST_OPT, mtk::EP_RST_IN_OUT_OPT)?;
    let ev = wmt(t, wmt_op::FUNC_CTRL, 0, &[1])?;
    kprintln!("bt: enable Bluetooth -> status {:?}", ev.status);
    Ok(())
}
