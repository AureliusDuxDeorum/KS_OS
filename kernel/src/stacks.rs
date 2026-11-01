//! Kernel-owned stacks, each with an unmapped guard page below it.
//!
//! A stack overflow therefore faults immediately instead of silently
//! corrupting adjacent memory. The overflow's page fault cannot be delivered on
//! the overflowed stack, so it escalates to a double fault that runs on a
//! dedicated IST stack and can name the culprit (see `idt.rs`).

use crate::paging::{MapError, Mapper};
use core::cell::UnsafeCell;
use core::ptr::addr_of;

const PAGE: usize = 4096;
pub const KERNEL_STACK_SIZE: usize = 256 * 1024;
pub const IST_STACK_SIZE: usize = 32 * 1024;

/// A stack whose lowest page is a guard page.
#[repr(C, align(4096))]
pub struct Stack<const N: usize> {
    guard: UnsafeCell<[u8; PAGE]>,
    data: UnsafeCell<[u8; N]>,
}

// SAFETY: the bytes are only ever touched by the CPU as a hardware stack.
unsafe impl<const N: usize> Sync for Stack<N> {}

impl<const N: usize> Stack<N> {
    pub const fn new() -> Self {
        Self {
            guard: UnsafeCell::new([0; PAGE]),
            data: UnsafeCell::new([0; N]),
        }
    }
}

pub static KERNEL_STACK: Stack<KERNEL_STACK_SIZE> = Stack::new();
pub static IST_DOUBLE_FAULT: Stack<IST_STACK_SIZE> = Stack::new();
pub static IST_NMI: Stack<IST_STACK_SIZE> = Stack::new();
pub static IST_MACHINE_CHECK: Stack<IST_STACK_SIZE> = Stack::new();

/// All stacks with a human-readable name, for diagnostics.
fn all() -> [(&'static str, u64, u64); 4] {
    fn span<const N: usize>(name: &'static str, s: &'static Stack<N>) -> (&'static str, u64, u64) {
        let base = addr_of!(*s) as u64;
        (name, base, base + (PAGE + N) as u64)
    }
    [
        span("kernel stack", &KERNEL_STACK),
        span("double-fault IST stack", &IST_DOUBLE_FAULT),
        span("NMI IST stack", &IST_NMI),
        span("machine-check IST stack", &IST_MACHINE_CHECK),
    ]
}

/// Virtual address of the kernel stack's guard page (for tests).
pub fn kernel_stack_guard() -> u64 {
    addr_of!(KERNEL_STACK) as u64
}

/// Top (initial stack pointer) of a stack.
pub fn top<const N: usize>(s: &'static Stack<N>) -> u64 {
    addr_of!(*s) as u64 + (PAGE + N) as u64
}

/// Unmaps every guard page in the address space rooted at `root`.
pub fn install_guards(mapper: &Mapper<'_>, root: u64) -> Result<(), MapError> {
    for (_, base, _) in all() {
        mapper.unmap(root, base)?;
    }
    Ok(())
}

/// If `addr` lies in a stack's guard page, returns that stack's name.
pub fn guard_owner(addr: u64) -> Option<&'static str> {
    all()
        .into_iter()
        .find(|&(_, base, _)| addr >= base && addr < base + PAGE as u64)
        .map(|(n, _, _)| n)
}

/// If `addr` lies anywhere inside a stack (guard or body), returns its name.
pub fn stack_containing(addr: u64) -> Option<&'static str> {
    all()
        .into_iter()
        .find(|&(_, base, end)| addr >= base && addr < end)
        .map(|(n, _, _)| n)
}
