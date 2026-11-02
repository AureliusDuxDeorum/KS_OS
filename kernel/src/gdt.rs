// SPDX-License-Identifier: Apache-2.0
//! Global Descriptor Table and Task State Segment.
//!
//! Flat 64-bit segments plus a TSS whose IST slots give the double-fault,
//! NMI and machine-check handlers known-good stacks regardless of the state of
//! the interrupted code's stack.

use crate::arch::{self, DescriptorPointer};
use crate::stacks;
use core::ptr::{addr_of, addr_of_mut};

pub const KERNEL_CODE: u16 = 0x08;
pub const KERNEL_DATA: u16 = 0x10;
const TSS_SELECTOR: u16 = 0x18;

/// IST slot numbers (1-based as used in IDT gates).
pub const IST_DOUBLE_FAULT: u8 = 1;
pub const IST_NMI: u8 = 2;
pub const IST_MACHINE_CHECK: u8 = 3;

#[repr(C, packed)]
struct Tss {
    _reserved0: u32,
    rsp: [u64; 3],
    _reserved1: u64,
    ist: [u64; 7],
    _reserved2: u64,
    _reserved3: u16,
    iomap_base: u16,
}

// 0: null, 1: kernel code, 2: kernel data, 3-4: TSS (16-byte descriptor).
static mut GDT: [u64; 5] = [0; 5];
static mut TSS: Tss = Tss {
    _reserved0: 0,
    rsp: [0; 3],
    _reserved1: 0,
    ist: [0; 7],
    _reserved2: 0,
    _reserved3: 0,
    iomap_base: core::mem::size_of::<Tss>() as u16,
};

/// Builds and loads the GDT and TSS. Call once on the boot CPU.
pub fn init() {
    // SAFETY: single-threaded early boot; nothing else references GDT/TSS yet.
    unsafe {
        let tss = addr_of_mut!(TSS);
        (*tss).ist[usize::from(IST_DOUBLE_FAULT) - 1] = stacks::top(&stacks::IST_DOUBLE_FAULT);
        (*tss).ist[usize::from(IST_NMI) - 1] = stacks::top(&stacks::IST_NMI);
        (*tss).ist[usize::from(IST_MACHINE_CHECK) - 1] = stacks::top(&stacks::IST_MACHINE_CHECK);

        let base = tss as u64;
        let limit = (core::mem::size_of::<Tss>() - 1) as u64;
        let gdt = addr_of_mut!(GDT).cast::<u64>();
        gdt.add(1).write(0x00AF_9A00_0000_FFFF); // 64-bit code, DPL 0
        gdt.add(2).write(0x00CF_9200_0000_FFFF); // data, DPL 0
        let low = (limit & 0xFFFF)
            | ((base & 0xFF_FFFF) << 16)
            | (0x89 << 40) // present, available 64-bit TSS
            | (((limit >> 16) & 0xF) << 48)
            | (((base >> 24) & 0xFF) << 56);
        gdt.add(3).write(low);
        gdt.add(4).write(base >> 32);

        let ptr = DescriptorPointer {
            limit: (core::mem::size_of::<[u64; 5]>() - 1) as u16,
            base: addr_of!(GDT) as u64,
        };
        arch::load_gdt(&ptr, KERNEL_CODE, KERNEL_DATA);
        arch::load_tss(TSS_SELECTOR);
    }
}
