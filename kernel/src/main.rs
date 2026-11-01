//! KS_OS kernel — Stage 1: boot, announce "OS operating", power off / boot to UEFI setup.

#![no_std]
#![no_main]
#![deny(clippy::undocumented_unsafe_blocks)]

extern crate alloc;

#[macro_use]
mod log;

mod acpi;
mod apic;
mod arch;
mod boot;
mod efi;
mod fb;
mod frames;
mod gdt;
mod heap;
mod idt;
mod ioapic;
mod irq;
mod limine;
mod madt;
mod msi;
mod paging;
mod pmm;
mod ps2;
mod selftest;
mod serial;
mod stacks;
mod strbuf;
mod sync;
mod time;
mod ui;
mod vmm;

use core::fmt::Write;
use strbuf::StrBuf;
use ui::{Action, Level, Ui};

/// Kernel entry point, jumped to by Limine in 64-bit long mode.
///
/// Switches from the bootloader's stack to the kernel's own guard-paged stack
/// before running any Rust code.
#[unsafe(naked)]
#[no_mangle]
extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "cli",
        "lea rsp, [rip + {stack} + {size}]",
        "xor ebp, ebp",
        "call {main}",
        "ud2",
        stack = sym stacks::KERNEL_STACK,
        size = const stacks::KERNEL_STACK_SIZE + 4096,
        main = sym kmain,
    )
}

extern "C" fn kmain() -> ! {
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

    // CPU exception handling comes first so every later fault is diagnosable.
    gdt::init();
    idt::init();
    kprintln!("cpu: GDT/TSS/IDT loaded");

    // Memory: physical allocator, kernel-owned address space (W^X, guard pages), heap.
    pmm::init(&boot);
    let st = pmm::stats();
    kprintln!(
        "pmm: {} MiB managed, {} MiB free",
        st.total_frames * 4 / 1024,
        st.free_frames * 4 / 1024
    );
    if let Err(e) = vmm::init(&boot) {
        panic!("vmm: cannot build kernel address space: {e:?}");
    }
    kprintln!("vmm: kernel address space active, stack guard pages installed");
    heap::init(boot.hhdm);

    // The one line this stage exists to print.
    kprintln!("OS operating");

    let mut frames = frames::Frames;

    // Interrupt hardware: local APIC (with NMI), calibrated clock and tick, I/O APIC.
    let madt = match acpi::find_table(&boot, b"APIC")
        .map_err(|e| alloc::format!("{e:?}"))
        .and_then(|t| madt::parse(t).map_err(|e| alloc::format!("{e:?}")))
    {
        Ok(m) => m,
        Err(e) => panic!("acpi: no usable MADT: {e}"),
    };
    kprintln!(
        "madt: {} CPU(s), {} I/O APIC(s), {} source override(s), {} NMI entr(ies)",
        madt.processors,
        madt.ioapics.len(),
        madt.overrides.len(),
        madt.nmis.len()
    );
    if let Err(e) = apic::init(
        &boot,
        &madt,
        boot.cmdline.split_whitespace().any(|t| t == "apic=x2"),
    ) {
        panic!("apic: {e:?}");
    }
    match acpi::pm_timer(&boot) {
        Ok(pm) => time::init(&pm),
        Err(e) => panic!("time: no ACPI PM timer to calibrate against: {e:?}"),
    }
    if let Err(e) = ioapic::init(&boot, &madt) {
        panic!("ioapic: {e:?}");
    }

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

    let st = pmm::stats();
    kprintln!("pmm: {} MiB free", st.free_frames * 4 / 1024);
    let (leaves, violations) = vmm::audit(&boot);
    kprintln!(
        "vmm: W^X audit after firmware setup: {leaves} leaf mappings, {violations} violations"
    );
    for (virt, size) in vmm::find_wx(&boot, 8) {
        kprintln!("vmm:   writable+executable: {virt:#x} (+{size:#x})");
    }

    let mut ps2 = ps2::Ps2::init();
    kprintln!("ps2: keyboard={} mouse={}", ps2.has_keyboard, ps2.has_mouse);
    route_ps2_irqs(&madt, &mut ps2);

    // Everything an interrupt handler touches is ready: let interrupts in.
    arch::sti();
    kprintln!("irq: interrupts enabled");

    if let Some(test) = selftest::requested(boot.cmdline) {
        selftest::run(test, &boot);
    }

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
    fb::register_fatal_fb(fb.clone());
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

/// Routes the keyboard (ISA IRQ 1) and mouse (ISA IRQ 12) lines through the I/O APIC
/// and switches the controller to interrupt-driven mode; stays in polling mode on failure.
fn route_ps2_irqs(madt: &madt::Madt, ps2: &mut ps2::Ps2) {
    let dest = apic::id();
    for (irq_line, enabled) in [(1u8, ps2.has_keyboard), (12u8, ps2.has_mouse)] {
        if !enabled {
            continue;
        }
        let (gsi, polarity, trigger) = madt.isa_irq(irq_line);
        let Some(vector) = irq::alloc_vector() else {
            kprintln!("ps2: no free interrupt vector; staying in polling mode");
            return;
        };
        irq::register(vector, ps2::irq_handler);
        if let Err(e) = ioapic::route(gsi, vector, dest, polarity, trigger) {
            kprintln!(
                "ps2: cannot route IRQ {irq_line} (GSI {gsi}): {e:?}; staying in polling mode"
            );
            return;
        }
        kprintln!("ps2: IRQ {irq_line} -> GSI {gsi} -> vector {vector:#x}");
    }
    ps2.enable_irq();
    kprintln!("ps2: input is interrupt-driven");
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    serial::_print_panic(format_args!("\nKERNEL PANIC: {info}\n"));
    fb::show_fatal("KERNEL PANIC", "", 0, 0, 0);
    arch::halt_forever()
}
