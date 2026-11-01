//! KS_OS kernel — Stage 1: boot, announce "OS operating", power off / boot to UEFI setup.

#![no_std]
#![no_main]
#![deny(clippy::undocumented_unsafe_blocks)]

#[macro_use]
mod log;

mod acpi;
mod arch;
mod boot;
mod efi;
mod fb;
mod frames;
mod limine;
mod paging;
mod ps2;
mod serial;
mod strbuf;
mod ui;

use core::fmt::Write;
use strbuf::StrBuf;
use ui::{Action, Level, Ui};

/// Kernel entry point, jumped to by Limine in 64-bit long mode.
#[no_mangle]
extern "C" fn _start() -> ! {
    arch::cli();
    serial::init();
    kprintln!("KS_OS 0.1.0 booting");

    let boot = match boot::BootInfo::collect() {
        Ok(b) => b,
        Err(e) => panic!("boot protocol error: {e}"),
    };
    kprintln!(
        "boot: {} {} (base revision {}), hhdm={:#x}",
        boot.bootloader.0,
        boot.bootloader.1,
        limine::BASE.loaded_revision(),
        boot.hhdm
    );
    match limine::FIRMWARE_TYPE.response() {
        Some(r) if r.firmware_type == limine::FIRMWARE_EFI64 => {
            kprintln!("firmware: UEFI (64-bit)")
        }
        Some(r) => kprintln!(
            "firmware: type {} (UEFI-only features disabled)",
            r.firmware_type
        ),
        None => kprintln!("firmware: unknown"),
    }
    let usable: u64 = boot
        .memmap()
        .filter(|e| e.kind == limine::MEMMAP_USABLE)
        .map(|e| e.length)
        .sum();
    kprintln!("memory: {} MiB usable", usable >> 20);

    // The one line this stage exists to print.
    kprintln!("OS operating");

    let mut frames = frames::BumpFrames::new(&boot);

    // Firmware services needed by the buttons.
    let acpi = match acpi::Acpi::init(&boot) {
        Ok(a) => {
            kprintln!("acpi: S5 power-off ready");
            Some(a)
        }
        Err(e) => {
            kprintln!("acpi: unavailable ({e:?})");
            None
        }
    };
    let efi = match efi::Efi::init(&boot, &mut frames) {
        Ok(e) => {
            kprintln!(
                "efi: runtime services up ({:?} mode, OsIndicationsSupported={:#x}, strict W^X={})",
                e.mode,
                e.os_indications_supported,
                e.strict_wx
            );
            Some(e)
        }
        Err(e) => {
            kprintln!("efi: runtime services unavailable ({e:?})");
            None
        }
    };

    kprintln!("frames: {} early page(s) allocated", frames.allocated());

    let mut ps2 = ps2::Ps2::init();
    kprintln!("ps2: keyboard={} mouse={}", ps2.has_keyboard, ps2.has_mouse);

    let Some(fb_info) = boot.framebuffer else {
        kprintln!("no framebuffer; serial-only mode");
        arch::halt_forever();
    };
    // SAFETY: Limine hands us exclusive access to a mapped framebuffer.
    let Some(fb) = (unsafe { fb::Fb::new(fb_info) }) else {
        panic!("unsupported framebuffer format: {} bpp", fb_info.bpp);
    };

    let mut info = StrBuf::<96>::new();
    ui::info_line(&mut info, &fb, "UEFI");
    let setup_ok = efi.as_ref().is_some_and(|e| e.firmware_setup_supported());
    let mut ui = Ui::new(fb, setup_ok, ps2.has_mouse);
    ui.draw_all(info.as_str());
    ui.describe();
    kprintln!("ui: ready");

    loop {
        match ui.run(&mut ps2) {
            Action::PowerOff => {
                kprintln!("action: power off");
                ui.set_status("Powering off...", Level::Info);
                if let Some(a) = &acpi {
                    let e = a.power_off();
                    kprintln!("acpi: power off did not take effect ({e:?})");
                }
                if let Some(e) = &efi {
                    e.shutdown();
                    kprintln!("efi: shutdown returned");
                }
                // Emulator-specific last resorts (QEMU/Bochs/VirtualBox ACPI ports).
                // SAFETY: writes to ports that are no-ops on hardware lacking them.
                unsafe {
                    arch::outw(0x604, 0x2000);
                    arch::outw(0xB004, 0x2000);
                    arch::outw(0x4004, 0x3400);
                }
                ui.set_status("Power off failed: no working method", Level::Error);
            }
            Action::BootToFirmware => {
                kprintln!("action: reboot into UEFI firmware setup");
                let Some(e) = &efi else {
                    ui.set_status("UEFI runtime services unavailable", Level::Error);
                    continue;
                };
                if !e.firmware_setup_supported() {
                    ui.set_status("Firmware does not support boot-to-setup", Level::Warn);
                    continue;
                }
                ui.set_status("Rebooting into UEFI setup...", Level::Info);
                let err = e.reboot_into_firmware_setup();
                kprintln!("efi: reboot into setup failed: {err:?}");
                let mut msg = StrBuf::<64>::new();
                let _ = write!(msg, "Reboot into UEFI failed: {err:?}");
                ui.set_status(msg.as_str(), Level::Error);
            }
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    serial::_print_panic(format_args!("\nKERNEL PANIC: {info}\n"));
    arch::halt_forever()
}
