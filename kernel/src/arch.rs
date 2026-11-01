//! Thin, audited wrappers around privileged x86_64 instructions.
//!
//! This is the only module (besides hardware-facing drivers) that contains
//! raw `asm!`; everything else goes through these functions.

use core::arch::asm;

/// Writes a byte to an I/O port.
///
/// # Safety
/// Port I/O can have arbitrary hardware side effects.
#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    // SAFETY: caller guarantees the port access is valid.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags))
    };
}

/// Writes a 16-bit word to an I/O port.
///
/// # Safety
/// See [`outb`].
#[inline]
pub unsafe fn outw(port: u16, value: u16) {
    // SAFETY: caller guarantees the port access is valid.
    unsafe {
        asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags))
    };
}

/// Reads a byte from an I/O port.
///
/// # Safety
/// See [`outb`].
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: caller guarantees the port access is valid.
    unsafe {
        asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags))
    };
    v
}

/// Reads a 16-bit word from an I/O port.
///
/// # Safety
/// See [`outb`].
#[inline]
pub unsafe fn inw(port: u16) -> u16 {
    let v: u16;
    // SAFETY: caller guarantees the port access is valid.
    unsafe {
        asm!("in ax, dx", out("ax") v, in("dx") port, options(nomem, nostack, preserves_flags))
    };
    v
}

/// Disables maskable interrupts.
#[inline]
pub fn cli() {
    // SAFETY: clearing IF is always sound in ring 0.
    unsafe { asm!("cli", options(nomem, nostack)) };
}

/// Halts the CPU forever with interrupts disabled.
pub fn halt_forever() -> ! {
    loop {
        cli();
        // SAFETY: `hlt` in ring 0 with IF clear only idles the core.
        unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}

/// Reads CR0.
#[inline]
pub fn read_cr0() -> u64 {
    let v: u64;
    // SAFETY: reading a control register has no side effects.
    unsafe { asm!("mov {}, cr0", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// Writes CR0.
///
/// # Safety
/// Changes fundamental CPU behaviour.
#[inline]
pub unsafe fn write_cr0(v: u64) {
    // SAFETY: caller upholds the CR0 invariants.
    unsafe { asm!("mov cr0, {}", in(reg) v, options(nostack, preserves_flags)) };
}

/// Reads CR3.
#[inline]
pub fn read_cr3() -> u64 {
    let v: u64;
    // SAFETY: reading a control register has no side effects.
    unsafe { asm!("mov {}, cr3", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// Loads a new top-level page table.
///
/// # Safety
/// The new tables must map the currently executing code, the stack, and every
/// address the kernel dereferences afterwards.
#[inline]
pub unsafe fn write_cr3(v: u64) {
    // SAFETY: caller guarantees the new address space keeps the kernel mapped.
    unsafe { asm!("mov cr3, {}", in(reg) v, options(nostack, preserves_flags)) };
}

/// Reads CR4.
#[inline]
pub fn read_cr4() -> u64 {
    let v: u64;
    // SAFETY: reading a control register has no side effects.
    unsafe { asm!("mov {}, cr4", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// Writes CR4.
///
/// # Safety
/// Changes fundamental CPU behaviour.
#[inline]
pub unsafe fn write_cr4(v: u64) {
    // SAFETY: caller upholds the CR4 invariants.
    unsafe { asm!("mov cr4, {}", in(reg) v, options(nostack, preserves_flags)) };
}

/// Reads a model-specific register.
///
/// # Safety
/// `msr` must exist on this CPU or a #GP is raised.
#[inline]
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: caller guarantees the MSR exists.
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags))
    };
    (u64::from(hi) << 32) | u64::from(lo)
}

/// Writes a model-specific register.
///
/// # Safety
/// `msr` must exist and `value` must be valid for it.
#[inline]
pub unsafe fn wrmsr(msr: u32, value: u64) {
    // SAFETY: caller guarantees the MSR write is valid.
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32,
             options(nomem, nostack, preserves_flags));
    }
}

/// `IA32_EFER` model-specific register.
pub const MSR_EFER: u32 = 0xC000_0080;
/// EFER.NXE: enable the no-execute page-table bit.
pub const EFER_NXE: u64 = 1 << 11;

/// Returns true if the CPU supports the NX (no-execute) page attribute.
pub fn cpu_has_nx() -> bool {
    let max = core::arch::x86_64::__cpuid(0x8000_0000).eax;
    if max < 0x8000_0001 {
        return false;
    }
    let r = core::arch::x86_64::__cpuid(0x8000_0001);
    r.edx & (1 << 20) != 0
}

/// Prepares the SSE/x87 unit so that firmware code (which freely uses SSE)
/// can run. The kernel itself is compiled soft-float and never touches it.
pub fn enable_sse() {
    const CR0_MP: u64 = 1 << 1;
    const CR0_EM: u64 = 1 << 2;
    const CR0_NE: u64 = 1 << 5;
    const CR4_OSFXSR: u64 = 1 << 9;
    const CR4_OSXMMEXCPT: u64 = 1 << 10;
    // SAFETY: standard SSE enablement sequence (Intel SDM vol. 3, 13.1.4).
    unsafe {
        write_cr0((read_cr0() & !CR0_EM) | CR0_MP | CR0_NE);
        write_cr4(read_cr4() | CR4_OSFXSR | CR4_OSXMMEXCPT);
        asm!("fninit", options(nomem, nostack));
        let mxcsr: u32 = 0x1F80;
        asm!("ldmxcsr [{}]", in(reg) &mxcsr, options(nostack, readonly, preserves_flags));
    }
}
