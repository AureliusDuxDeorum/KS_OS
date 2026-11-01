//! Interrupt Descriptor Table and CPU exception handling.
//!
//! Assembly stubs normalise every exception into a [`TrapFrame`] and call
//! [`exception_dispatch`]. Breakpoints and NMIs are logged and resumed; every
//! other exception is fatal and produces a full register dump on the serial
//! console and the screen instead of silently resetting the machine.

use crate::arch::{self, DescriptorPointer};
use crate::{fb, gdt, stacks};
use core::arch::global_asm;
use core::ptr::{addr_of, addr_of_mut};

/// Register state saved by the stubs (lowest address first).
#[repr(C)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

/// Vector used for spurious interrupts (set up with the APIC in a later phase).
pub const SPURIOUS_VECTOR: u8 = 0xFF;

global_asm!(
    r#"
    .macro isr_noerr n
    isr_\n:
        push 0
        push \n
        jmp isr_common
    .endm
    .macro isr_err n
    isr_\n:
        push \n
        jmp isr_common
    .endm

    .section .text
    .irp n, 0,1,2,3,4,5,6,7,9,15,16,18,19,20,22,23,24,25,26,27,28,31,255
        isr_noerr \n
    .endr
    .irp n, 8,10,11,12,13,14,17,21,29,30
        isr_err \n
    .endr

    isr_common:
        push rax
        push rcx
        push rdx
        push rbx
        push rbp
        push rsi
        push rdi
        push r8
        push r9
        push r10
        push r11
        push r12
        push r13
        push r14
        push r15
        mov rdi, rsp
        cld
        call {dispatch}
        pop r15
        pop r14
        pop r13
        pop r12
        pop r11
        pop r10
        pop r9
        pop r8
        pop rdi
        pop rsi
        pop rbp
        pop rbx
        pop rdx
        pop rcx
        pop rax
        add rsp, 16
        iretq

    .section .rodata
    .balign 8
    isr_table:
    .irp n, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
        .quad isr_\n
    .endr
    isr_spurious_addr:
        .quad isr_255
    .section .text
    "#,
    dispatch = sym exception_dispatch,
);

extern "C" {
    static isr_table: [u64; 32];
    static isr_spurious_addr: u64;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Gate {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    zero: u32,
}

impl Gate {
    const MISSING: Self = Self {
        offset_low: 0,
        selector: 0,
        ist: 0,
        type_attr: 0,
        offset_mid: 0,
        offset_high: 0,
        zero: 0,
    };

    fn interrupt(handler: u64, ist: u8) -> Self {
        Self {
            offset_low: handler as u16,
            selector: gdt::KERNEL_CODE,
            ist,
            type_attr: 0x8E, // present, DPL 0, 64-bit interrupt gate (IF cleared on entry)
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            zero: 0,
        }
    }
}

static mut IDT: [Gate; 256] = [Gate::MISSING; 256];

/// Builds and loads the IDT. Call once, after [`gdt::init`].
pub fn init() {
    // SAFETY: single-threaded early boot; the stub tables are provided by the assembly above.
    unsafe {
        let idt = addr_of_mut!(IDT).cast::<Gate>();
        for (v, &handler) in isr_table.iter().enumerate() {
            let ist = match v {
                8 => gdt::IST_DOUBLE_FAULT,
                2 => gdt::IST_NMI,
                18 => gdt::IST_MACHINE_CHECK,
                _ => 0,
            };
            idt.add(v).write(Gate::interrupt(handler, ist));
        }
        idt.add(usize::from(SPURIOUS_VECTOR))
            .write(Gate::interrupt(isr_spurious_addr, 0));
        let ptr = DescriptorPointer {
            limit: (core::mem::size_of::<[Gate; 256]>() - 1) as u16,
            base: addr_of!(IDT) as u64,
        };
        arch::load_idt(&ptr);
    }
}

const NAMES: [&str; 32] = [
    "#DE divide error",
    "#DB debug",
    "NMI",
    "#BP breakpoint",
    "#OF overflow",
    "#BR bound range exceeded",
    "#UD invalid opcode",
    "#NM device not available",
    "#DF double fault",
    "coprocessor segment overrun",
    "#TS invalid TSS",
    "#NP segment not present",
    "#SS stack-segment fault",
    "#GP general protection fault",
    "#PF page fault",
    "reserved",
    "#MF x87 floating-point",
    "#AC alignment check",
    "#MC machine check",
    "#XM SIMD floating-point",
    "#VE virtualization",
    "#CP control protection",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "#HV hypervisor injection",
    "#VC VMM communication",
    "#SX security exception",
    "reserved",
];

extern "C" fn exception_dispatch(frame: &mut TrapFrame) {
    match frame.vector {
        2 => kprintln!("exception: NMI at rip={:#x} (resuming)", frame.rip),
        3 => kprintln!(
            "exception: #BP breakpoint at rip={:#x} (resuming)",
            frame.rip
        ),
        v if v == u64::from(SPURIOUS_VECTOR) => {}
        _ => fatal(frame),
    }
}

fn page_fault_flags(e: u64) -> &'static str {
    match (e & 1 != 0, e & 2 != 0, e & 4 != 0, e & 16 != 0) {
        (false, false, _, false) => "read of non-present page",
        (false, true, _, false) => "write to non-present page",
        (false, _, _, true) => "instruction fetch from non-present page",
        (true, false, _, false) => "read protection violation",
        (true, true, _, false) => "write protection violation",
        (true, _, _, true) => "instruction fetch from non-executable page",
    }
}

fn fatal(f: &TrapFrame) -> ! {
    let name = NAMES.get(f.vector as usize).copied().unwrap_or("unknown");
    let cr2 = arch::read_cr2();
    let cr3 = arch::read_cr3();

    let mut diagnosis = "";
    if f.vector == 14 {
        diagnosis = page_fault_flags(f.error);
        if let Some(s) = stacks::guard_owner(cr2) {
            diagnosis = match s {
                "kernel stack" => "KERNEL STACK OVERFLOW (guard page hit)",
                _ => "IST STACK OVERFLOW (guard page hit)",
            };
        }
    } else if f.vector == 8 {
        // The first fault's address is still in CR2; a guard-page hit means the
        // stack overflowed and the #PF could not be delivered on it.
        if let Some(s) = stacks::guard_owner(cr2) {
            diagnosis = "stack overflow: #PF on guard page could not be delivered";
            kprintln!("  overflowed: {s}");
        } else if let Some(s) = stacks::stack_containing(f.rsp) {
            kprintln!("  rsp inside {s}");
        }
    }

    serial_dump(f, name, diagnosis, cr2, cr3);
    fb::show_fatal(name, diagnosis, f.rip, cr2, f.error);
    arch::halt_forever()
}

fn serial_dump(f: &TrapFrame, name: &str, diagnosis: &str, cr2: u64, cr3: u64) {
    crate::serial::_print_panic(format_args!(
        "\nKERNEL FAULT: {name} (vector {})\n",
        f.vector
    ));
    if !diagnosis.is_empty() {
        kprintln!("  cause: {diagnosis}");
    }
    kprintln!(
        "  error={:#x} rip={:#018x} cs={:#x} rflags={:#x}",
        f.error,
        f.rip,
        f.cs,
        f.rflags
    );
    kprintln!(
        "  rsp={:#018x} ss={:#x} cr2={:#x} cr3={:#x}",
        f.rsp,
        f.ss,
        cr2,
        cr3
    );
    kprintln!(
        "  rax={:#018x} rbx={:#018x} rcx={:#018x} rdx={:#018x}",
        f.rax,
        f.rbx,
        f.rcx,
        f.rdx
    );
    kprintln!(
        "  rsi={:#018x} rdi={:#018x} rbp={:#018x} r8 ={:#018x}",
        f.rsi,
        f.rdi,
        f.rbp,
        f.r8
    );
    kprintln!(
        "  r9 ={:#018x} r10={:#018x} r11={:#018x} r12={:#018x}",
        f.r9,
        f.r10,
        f.r11,
        f.r12
    );
    kprintln!(
        "  r13={:#018x} r14={:#018x} r15={:#018x}",
        f.r13,
        f.r14,
        f.r15
    );
}
