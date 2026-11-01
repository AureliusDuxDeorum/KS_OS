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

/// Invalidates the TLB entry for one page.
#[inline]
pub fn invlpg(addr: u64) {
    // SAFETY: INVLPG only drops a cached translation.
    unsafe { asm!("invlpg [{}]", in(reg) addr, options(nostack, preserves_flags)) };
}

/// Reads CR2 (the faulting address of the last page fault).
#[inline]
pub fn read_cr2() -> u64 {
    let v: u64;
    // SAFETY: reading a control register has no side effects.
    unsafe { asm!("mov {}, cr2", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// Pseudo-descriptor for `lgdt` / `lidt`.
#[repr(C, packed)]
pub struct DescriptorPointer {
    pub limit: u16,
    pub base: u64,
}

/// Loads the GDT and reloads every segment register.
///
/// # Safety
/// `gdt` must describe a valid GDT that stays alive and unmodified; `code` and
/// `data` must be a 64-bit code and a data selector within it.
pub unsafe fn load_gdt(gdt: &DescriptorPointer, code: u16, data: u16) {
    // SAFETY: caller guarantees the GDT is valid; the far return reloads CS.
    unsafe {
        asm!(
            "lgdt [{gdt}]",
            "push {code}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov ds, {data:x}",
            "mov es, {data:x}",
            "mov ss, {data:x}",
            "xor {tmp:e}, {tmp:e}",
            "mov fs, {tmp:x}",
            "mov gs, {tmp:x}",
            gdt = in(reg) gdt,
            code = in(reg) u64::from(code),
            data = in(reg) u64::from(data),
            tmp = out(reg) _,
            options(preserves_flags),
        );
    }
}

/// Loads the task register.
///
/// # Safety
/// `selector` must reference an available 64-bit TSS descriptor in the loaded GDT.
pub unsafe fn load_tss(selector: u16) {
    // SAFETY: caller guarantees the selector is valid.
    unsafe { asm!("ltr {:x}", in(reg) selector, options(nomem, nostack, preserves_flags)) };
}

/// Loads the IDT.
///
/// # Safety
/// `idt` must describe a valid IDT that stays alive for as long as it is loaded.
pub unsafe fn load_idt(idt: &DescriptorPointer) {
    // SAFETY: caller guarantees the IDT is valid.
    unsafe { asm!("lidt [{}]", in(reg) idt, options(readonly, nostack, preserves_flags)) };
}

/// Reads a 32-bit double word from an I/O port.
///
/// # Safety
/// See [`outb`].
#[inline]
pub unsafe fn inl(port: u16) -> u32 {
    let v: u32;
    // SAFETY: caller guarantees the port access is valid.
    unsafe {
        asm!("in eax, dx", out("eax") v, in("dx") port, options(nomem, nostack, preserves_flags))
    };
    v
}

/// Writes a 32-bit double word to an I/O port (PCI configuration space, later phases).
///
/// # Safety
/// See [`outb`].
#[inline]
#[allow(dead_code)]
pub unsafe fn outl(port: u16, value: u32) {
    // SAFETY: caller guarantees the port access is valid.
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags))
    };
}

/// Enables maskable interrupts.
#[inline]
pub fn sti() {
    // SAFETY: setting IF is sound once the IDT and handlers are installed.
    unsafe { asm!("sti", options(nomem, nostack)) };
}

/// Atomically enables interrupts and halts until the next one (no lost-wakeup window).
#[inline]
pub fn enable_and_halt() {
    // SAFETY: `sti; hlt` executes the halt before any interrupt can be taken in between.
    unsafe { asm!("sti", "hlt", options(nomem, nostack)) };
}

/// Returns RFLAGS.
#[inline]
pub fn read_rflags() -> u64 {
    let v: u64;
    // SAFETY: pushes and pops RFLAGS; no side effects.
    unsafe { asm!("pushfq", "pop {}", out(reg) v, options(nomem, preserves_flags)) };
    v
}

/// Returns true if maskable interrupts are enabled.
#[inline]
pub fn interrupts_enabled() -> bool {
    read_rflags() & (1 << 9) != 0
}

/// Reads the time-stamp counter.
#[inline]
pub fn rdtsc() -> u64 {
    // SAFETY: RDTSC is available on every x86_64 CPU and has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// CPUID leaf/sub-leaf query.
pub fn cpuid(leaf: u32, sub: u32) -> core::arch::x86_64::CpuidResult {
    core::arch::x86_64::__cpuid_count(leaf, sub)
}

/// Runs `f` with maskable interrupts disabled, restoring the previous state afterwards.
pub fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    let were_enabled = interrupts_enabled();
    cli();
    let r = f();
    if were_enabled {
        sti();
    }
    r
}
