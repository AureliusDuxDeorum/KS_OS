//! Boot-time self-tests selected through the kernel command line
//! (`selftest=<name>`), used by the end-to-end test suite to prove the
//! exception machinery works on real (emulated) hardware. Production images
//! carry no command line, so none of this runs.

use core::arch::asm;

/// Returns the requested self-test name, if any.
pub fn requested(cmdline: &str) -> Option<&str> {
    cmdline
        .split_whitespace()
        .find_map(|t| t.strip_prefix("selftest="))
}

/// Runs one self-test. Most of them are *expected* to end in a fatal fault.
pub fn run(name: &str) {
    kprintln!("selftest: {name}");
    match name {
        "breakpoint" => {
            // SAFETY: #BP is handled by the kernel and execution resumes.
            unsafe { asm!("int3") };
            kprintln!("selftest: breakpoint returned");
        }
        "divide" => {
            // SAFETY: deliberately raises #DE; the handler reports it fatally.
            unsafe {
                asm!("xor edx, edx", "xor ecx, ecx", "div ecx", out("eax") _, out("edx") _, out("ecx") _)
            };
        }
        "invalid-opcode" => {
            // SAFETY: deliberately raises #UD.
            unsafe { asm!("ud2") };
        }
        "general-protection" => {
            // SAFETY: loading a non-existent selector deliberately raises #GP.
            unsafe { asm!("mov ds, {0:x}", in(reg) 0x1000u16) };
        }
        "page-fault" => {
            // SAFETY: deliberately dereferences the null page to raise #PF.
            let _ = unsafe { core::ptr::read_volatile(0x10 as *const u8) };
        }
        "stack-overflow" => {
            // SAFETY: deliberately exhausts the kernel stack; the guard page turns this
            // into a diagnosable fault. (A recursive function would be folded into a loop.)
            unsafe { asm!("2:", "push rax", "jmp 2b", options(noreturn)) };
        }
        "nmi" => {
            // SAFETY: software `int 2` runs the NMI gate (and its IST stack) like a real NMI.
            unsafe { asm!("int 2") };
            kprintln!("selftest: nmi returned");
        }
        other => kprintln!("selftest: unknown test `{other}`"),
    }
}
