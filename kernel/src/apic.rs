//! Local APIC driver (xAPIC MMIO or x2APIC MSR mode) for the boot CPU.
//!
//! Also silences the legacy 8259 PICs, configures the NMI pin from the MADT so
//! real NMIs reach the NMI handler, and provides end-of-interrupt.

use crate::arch::{self, outb};
use crate::boot::BootInfo;
use crate::madt::{Madt, Polarity, Trigger};
use crate::{irq, vmm};
use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};

const MSR_APIC_BASE: u32 = 0x1B;
const APIC_ENABLE: u64 = 1 << 11;
const APIC_X2: u64 = 1 << 10;

// Register offsets (xAPIC MMIO); x2APIC MSR = 0x800 + (offset >> 4).
pub const REG_ID: u32 = 0x20;
pub const REG_TPR: u32 = 0x80;
pub const REG_EOI: u32 = 0xB0;
pub const REG_SVR: u32 = 0xF0;
pub const REG_ESR: u32 = 0x280;
pub const REG_LVT_TIMER: u32 = 0x320;
pub const REG_LVT_LINT0: u32 = 0x350;
pub const REG_LVT_LINT1: u32 = 0x360;
pub const REG_LVT_ERROR: u32 = 0x370;
pub const REG_TIMER_INIT: u32 = 0x380;
pub const REG_TIMER_CUR: u32 = 0x390;
pub const REG_TIMER_DIV: u32 = 0x3E0;

pub const LVT_MASKED: u32 = 1 << 16;
pub const LVT_PERIODIC: u32 = 1 << 17;
const LVT_NMI: u32 = 0b100 << 8;
const LVT_ACTIVE_LOW: u32 = 1 << 13;
const LVT_LEVEL: u32 = 1 << 15;

const MODE_NONE: u8 = 0;
const MODE_XAPIC: u8 = 1;
const MODE_X2APIC: u8 = 2;

static MODE: AtomicU8 = AtomicU8::new(MODE_NONE);
static MMIO: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
#[allow(dead_code)]
pub enum ApicError {
    NoApic,
    Map(vmm::VmmError),
}

impl From<vmm::VmmError> for ApicError {
    fn from(e: vmm::VmmError) -> Self {
        Self::Map(e)
    }
}

pub fn mode_name() -> &'static str {
    match MODE.load(Ordering::Relaxed) {
        MODE_X2APIC => "x2APIC",
        MODE_XAPIC => "xAPIC",
        _ => "none",
    }
}

pub fn read(reg: u32) -> u32 {
    if MODE.load(Ordering::Relaxed) == MODE_X2APIC {
        // SAFETY: x2APIC mode is active, so the APIC register MSRs exist.
        unsafe { arch::rdmsr(0x800 + (reg >> 4)) as u32 }
    } else {
        // SAFETY: the xAPIC page was mapped uncached by `init`; registers are 32-bit aligned.
        unsafe { ((MMIO.load(Ordering::Relaxed) + u64::from(reg)) as *const u32).read_volatile() }
    }
}

pub fn write(reg: u32, value: u32) {
    if MODE.load(Ordering::Relaxed) == MODE_X2APIC {
        // SAFETY: as in `read`.
        unsafe { arch::wrmsr(0x800 + (reg >> 4), u64::from(value)) };
    } else {
        // SAFETY: as in `read`.
        unsafe {
            ((MMIO.load(Ordering::Relaxed) + u64::from(reg)) as *mut u32).write_volatile(value)
        };
    }
}

/// This CPU's local APIC ID.
pub fn id() -> u32 {
    let raw = read(REG_ID);
    if MODE.load(Ordering::Relaxed) == MODE_X2APIC {
        raw
    } else {
        raw >> 24
    }
}

/// Signals end-of-interrupt.
pub fn eoi() {
    write(REG_EOI, 0);
}

/// Remaps and masks both legacy PICs so stray IRQs cannot masquerade as exceptions.
fn silence_pics() {
    // SAFETY: standard 8259 initialisation sequence on the legacy PIC ports.
    unsafe {
        outb(0x20, 0x11);
        outb(0xA0, 0x11);
        outb(0x21, 0x20); // master vectors 0x20..0x27
        outb(0xA1, 0x28); // slave vectors 0x28..0x2F
        outb(0x21, 4);
        outb(0xA1, 2);
        outb(0x21, 1);
        outb(0xA1, 1);
        outb(0x21, 0xFF); // mask everything
        outb(0xA1, 0xFF);
    }
}

fn lvt_nmi_bits(polarity: Polarity, trigger: Trigger) -> u32 {
    LVT_NMI
        | if polarity == Polarity::ActiveLow {
            LVT_ACTIVE_LOW
        } else {
            0
        }
        | if trigger == Trigger::Level {
            LVT_LEVEL
        } else {
            0
        }
}

/// Enables the boot CPU's local APIC.
pub fn init(boot: &BootInfo, madt: &Madt, prefer_x2apic: bool) -> Result<(), ApicError> {
    let features = arch::cpuid(1, 0);
    if features.edx & (1 << 9) == 0 {
        return Err(ApicError::NoApic);
    }
    silence_pics();

    // SAFETY: IA32_APIC_BASE exists whenever CPUID reports an APIC.
    let mut base = unsafe { arch::rdmsr(MSR_APIC_BASE) };
    let x2_supported = features.ecx & (1 << 21) != 0;
    // xAPIC is the default (verified under emulation). x2APIC is used only if the firmware
    // already switched to it (it cannot be switched back without disabling the APIC) or when
    // explicitly requested with `apic=x2` on the kernel command line.
    let firmware_left_x2 = base & APIC_X2 != 0;
    let x2 = firmware_left_x2 || (x2_supported && prefer_x2apic);
    if x2 {
        if !firmware_left_x2 {
            // Enable xAPIC first, then x2APIC (the only legal transition order).
            // SAFETY: standard x2APIC enable sequence (Intel SDM vol. 3, 10.12.5).
            unsafe {
                arch::wrmsr(MSR_APIC_BASE, base | APIC_ENABLE);
                arch::wrmsr(MSR_APIC_BASE, base | APIC_ENABLE | APIC_X2);
            }
        }
        MODE.store(MODE_X2APIC, Ordering::Relaxed);
    } else {
        // SAFETY: as above.
        unsafe { arch::wrmsr(MSR_APIC_BASE, base | APIC_ENABLE) };
        // SAFETY: re-read what the CPU actually latched.
        base = unsafe { arch::rdmsr(MSR_APIC_BASE) };
        let phys = if madt.lapic_address != 0 {
            madt.lapic_address
        } else {
            base & 0x000F_FFFF_FFFF_F000
        };
        MMIO.store(vmm::map_mmio(boot, phys, 4096)?, Ordering::Relaxed);
        MODE.store(MODE_XAPIC, Ordering::Relaxed);
    }

    // Quiesce all local vectors, then bring up what we use.
    write(REG_TPR, 0);
    write(REG_LVT_TIMER, LVT_MASKED);
    write(REG_LVT_LINT0, LVT_MASKED); // ExtINT: the PICs are silenced
    write(REG_LVT_LINT1, LVT_MASKED);
    write(REG_LVT_ERROR, u32::from(irq::ERROR_VECTOR));
    write(REG_SVR, 0x100 | u32::from(irq::SPURIOUS_VECTOR));
    write(REG_ESR, 0);
    write(REG_ESR, 0);

    // Wire the NMI pin as the firmware describes (default: LINT1), so real NMIs arrive.
    let me = id();
    let nmi = madt
        .nmis
        .iter()
        .find(|n| n.processor_uid == u32::MAX || n.processor_uid == me)
        .copied();
    let (lint, bits) = nmi.map_or(
        (1, lvt_nmi_bits(Polarity::ActiveHigh, Trigger::Edge)),
        |n| (n.lint, lvt_nmi_bits(n.polarity, n.trigger)),
    );
    write(
        if lint == 0 {
            REG_LVT_LINT0
        } else {
            REG_LVT_LINT1
        },
        bits,
    );
    kprintln!("apic: {} enabled, id={me}, NMI on LINT{lint}", mode_name());
    Ok(())
}

/// Sends a fixed interrupt with `vector` to this CPU (used by self-tests and, later, IPIs to self).
pub fn send_self_ipi(vector: u8) {
    if MODE.load(Ordering::Relaxed) == MODE_X2APIC {
        // SAFETY: the x2APIC SELF_IPI register (MSR 0x83F) exists in x2APIC mode.
        unsafe { arch::wrmsr(0x83F, u64::from(vector)) };
    } else {
        // Destination shorthand "self" (0b01 << 18), fixed delivery; wait for delivery to complete.
        write(0x300, (0b01 << 18) | u32::from(vector));
        while read(0x300) & (1 << 12) != 0 {
            core::hint::spin_loop();
        }
    }
}
