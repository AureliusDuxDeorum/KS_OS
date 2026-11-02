// SPDX-License-Identifier: Apache-2.0
//! I/O APIC driver: routes global system interrupts (GSIs) to vectors on the boot CPU.

use crate::boot::BootInfo;
use crate::madt::{Madt, Polarity, Trigger};
use crate::vmm;
use alloc::vec::Vec;
use spin::Mutex;

struct IoApic {
    virt: u64,
    gsi_base: u32,
    entries: u32,
}

static IOAPICS: Mutex<Vec<IoApic>> = Mutex::new(Vec::new());

const REG_VER: u32 = 1;
const ENTRY_MASKED: u64 = 1 << 16;
const ENTRY_ACTIVE_LOW: u64 = 1 << 13;
const ENTRY_LEVEL: u64 = 1 << 15;

impl IoApic {
    fn read(&self, reg: u32) -> u32 {
        // SAFETY: the I/O APIC page is mapped uncached; IOREGSEL at +0, IOWIN at +0x10.
        unsafe {
            (self.virt as *mut u32).write_volatile(reg);
            ((self.virt + 0x10) as *const u32).read_volatile()
        }
    }

    fn write(&self, reg: u32, value: u32) {
        // SAFETY: as in `read`.
        unsafe {
            (self.virt as *mut u32).write_volatile(reg);
            ((self.virt + 0x10) as *mut u32).write_volatile(value);
        }
    }

    fn write_entry(&self, index: u32, value: u64) {
        self.write(0x10 + 2 * index + 1, (value >> 32) as u32);
        self.write(0x10 + 2 * index, value as u32);
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum IoApicError {
    Map(vmm::VmmError),
    NoSuchGsi(u32),
}

/// Maps every I/O APIC and masks all of its inputs.
pub fn init(boot: &BootInfo, madt: &Madt) -> Result<(), IoApicError> {
    let mut list = IOAPICS.lock();
    for info in &madt.ioapics {
        let virt = vmm::map_mmio(boot, u64::from(info.address), 4096).map_err(IoApicError::Map)?;
        let io = IoApic {
            virt,
            gsi_base: info.gsi_base,
            entries: 0,
        };
        let entries = ((io.read(REG_VER) >> 16) & 0xFF) + 1;
        let io = IoApic { entries, ..io };
        for i in 0..entries {
            io.write_entry(i, ENTRY_MASKED);
        }
        kprintln!(
            "ioapic: id={} at {:#x}, GSIs {}..{}",
            info.id,
            info.address,
            info.gsi_base,
            info.gsi_base + entries
        );
        list.push(io);
    }
    Ok(())
}

/// Routes `gsi` to `vector` on the CPU with local APIC ID `dest` (fixed delivery, physical mode).
pub fn route(
    gsi: u32,
    vector: u8,
    dest: u32,
    polarity: Polarity,
    trigger: Trigger,
) -> Result<(), IoApicError> {
    let list = IOAPICS.lock();
    let io = list
        .iter()
        .find(|io| gsi >= io.gsi_base && gsi < io.gsi_base + io.entries)
        .ok_or(IoApicError::NoSuchGsi(gsi))?;
    let mut e = u64::from(vector) | (u64::from(dest & 0xFF) << 56);
    if polarity == Polarity::ActiveLow {
        e |= ENTRY_ACTIVE_LOW;
    }
    if trigger == Trigger::Level {
        e |= ENTRY_LEVEL;
    }
    io.write_entry(gsi - io.gsi_base, e);
    Ok(())
}
