//! Boot-time self-tests selected through the kernel command line
//! (`selftest=<name>`), used by the end-to-end test suite to prove the
//! exception machinery works on real (emulated) hardware. Production images
//! carry no command line, so none of this runs.

use crate::arch;
use crate::boot::BootInfo;
use crate::madt::{self, MadtError};
use crate::{apic, heap, irq, msi, pmm, time, vmm};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

/// Returns the requested self-test name, if any.
pub fn requested(cmdline: &str) -> Option<&str> {
    cmdline
        .split_whitespace()
        .find_map(|t| t.strip_prefix("selftest="))
}

/// Runs one self-test. Most of them are *expected* to end in a fatal fault.
pub fn run(name: &str, boot: &BootInfo) {
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
        "irq" => test_irq(),
        "timer" => test_timer(),
        "msi" => test_msi(),
        "madt-parser" => test_madt_parser(),
        "ps2-irq" => {
            kprintln!("selftest: ps2 irq count {}", crate::ps2::irq_count());
            crate::time::sleep_ms(20);
        }
        "pmm" => test_pmm(),
        "pmm-double-free" => {
            let f = pmm::alloc().expect("frame");
            pmm::free(f);
            pmm::free(f);
        }
        "heap" => test_heap(),
        "mmio-map" => test_mmio(boot),
        "vmm-layout" => test_layout(boot),
        "write-text" => {
            let p = test_layout as *const u8 as *mut u8;
            // SAFETY: deliberately writes to read-only kernel code to prove W^X is enforced.
            unsafe { core::ptr::write_volatile(p, 0) };
        }
        "exec-data" => {
            static mut BUF: [u8; 16] = [0xC3; 16]; // `ret`
                                                   // SAFETY: deliberately executes from a non-executable data page.
            unsafe {
                let f: extern "C" fn() = core::mem::transmute(core::ptr::addr_of_mut!(BUF));
                f();
            }
        }
        other => kprintln!("selftest: unknown test `{other}`"),
    }
}

fn check(cond: bool, what: &str) {
    if cond {
        kprintln!("selftest:   ok  {what}");
    } else {
        panic!("selftest FAILED: {what}");
    }
}

fn test_pmm() {
    // Fixed-size arrays on purpose: a heap-backed Vec would itself consume frames.
    const N: usize = 200;
    let before = pmm::stats();
    let mut frames = [0u64; N];
    for f in frames.iter_mut() {
        *f = pmm::alloc_zeroed().expect("frame");
    }
    let mut sorted = frames;
    sorted.sort_unstable();
    check(
        sorted.windows(2).all(|w| w[0] != w[1]),
        "200 allocations are all distinct",
    );
    check(
        frames
            .iter()
            .all(|&f| f.is_multiple_of(4096) && f >= 0x10_0000),
        "frames are page-aligned and above 1 MiB",
    );
    check(
        pmm::stats().free_frames == before.free_frames - N as u64,
        "free count tracks allocations",
    );
    // 2 MiB-aligned, 2 MiB-long, DMA-safe (below 4 GiB) contiguous run.
    let run = pmm::alloc_contiguous(512, 512, 1 << 32).expect("contiguous run");
    check(
        run.is_multiple_of(2 << 20) && run + (2 << 20) <= 1 << 32,
        "512-frame run is 2 MiB-aligned and below 4 GiB",
    );
    for f in frames {
        pmm::free(f);
    }
    pmm::free_contiguous(run, 512);
    check(
        pmm::stats().free_frames == before.free_frames,
        "everything freed restores the free count",
    );
    let again = pmm::alloc_contiguous(512, 512, 1 << 32).expect("contiguous run after free");
    pmm::free_contiguous(again, 512);
    check(again == run, "freed run is reusable");
    kprintln!("selftest: pmm ok");
}

fn test_heap() {
    let s0 = heap::stats();
    let b = Box::new(0xDEAD_BEEF_u64);
    check(*b == 0xDEAD_BEEF, "Box round-trips a value");
    let mut v: Vec<u32> = Vec::new();
    for i in 0..10_000 {
        v.push(i * 3);
    }
    check(
        v.iter().enumerate().all(|(i, &x)| x == i as u32 * 3),
        "Vec grows through many reallocations intact",
    );
    let mut s = String::new();
    for i in 0..500 {
        s.push_str(&alloc::format!("{i},"));
    }
    check(
        s.starts_with("0,1,2,") && s.ends_with("499,"),
        "String formatting works",
    );
    let mut m = BTreeMap::new();
    for i in 0..1000u64 {
        m.insert(i * 7919 % 1009, i);
    }
    check(
        m.len() == 1000 && m.get(&0) == Some(&0),
        "BTreeMap holds all inserted keys",
    );
    // Every size class returns blocks aligned to their size, and blocks never overlap.
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    for &size in &[16usize, 32, 64, 128, 256, 512, 1024, 2048] {
        for _ in 0..8 {
            let layout = core::alloc::Layout::from_size_align(size, size).expect("layout");
            // SAFETY: valid non-zero layout.
            let p = unsafe { alloc::alloc::alloc(layout) };
            check(
                !p.is_null() && (p as usize).is_multiple_of(size),
                "size-class block is non-null and naturally aligned",
            );
            // SAFETY: `p` points to `size` writable bytes.
            unsafe { core::ptr::write_bytes(p, size as u8, size) };
            blocks.push((p as usize, size));
        }
    }
    blocks.sort_unstable();
    check(
        blocks.windows(2).all(|w| w[0].0 + w[0].1 <= w[1].0),
        "no two live blocks overlap",
    );
    check(
        blocks.iter().all(|&(p, size)| {
            // SAFETY: blocks are live and `size` bytes long.
            (0..size).all(|i| unsafe { *(p as *const u8).add(i) } == size as u8)
        }),
        "block contents are intact",
    );
    for &(p, size) in &blocks {
        let layout = core::alloc::Layout::from_size_align(size, size).expect("layout");
        // SAFETY: freeing blocks allocated above with the same layout.
        unsafe { alloc::alloc::dealloc(p as *mut u8, layout) };
    }
    // Large, over-aligned allocation (contiguous frames, 64 KiB aligned).
    let big = core::alloc::Layout::from_size_align(3 << 20, 64 * 1024).expect("layout");
    // SAFETY: valid non-zero layout.
    let p = unsafe { alloc::alloc::alloc(big) };
    check(
        !p.is_null() && (p as usize).is_multiple_of(64 * 1024),
        "3 MiB allocation is 64 KiB-aligned",
    );
    // SAFETY: `p` points to 3 MiB of writable memory.
    unsafe { core::ptr::write_bytes(p, 0xA5, 3 << 20) };
    // SAFETY: freeing the block just allocated.
    unsafe { alloc::alloc::dealloc(p, big) };
    drop(b);
    drop(v);
    let s1 = heap::stats();
    check(
        s1.large_frames == s0.large_frames,
        "large frames are returned to the PMM",
    );
    check(
        s1.allocs > s0.allocs && s1.allocs >= s1.frees,
        "alloc/free accounting is consistent",
    );
    kprintln!(
        "selftest: heap ok ({} slab frames, {} allocs, {} frees)",
        s1.slab_frames,
        s1.allocs,
        s1.frees
    );
}

fn test_mmio(boot: &BootInfo) {
    let frame = pmm::alloc_zeroed().expect("frame");
    let hhdm = (boot.hhdm + frame) as *mut u32;
    // SAFETY: the frame is ours and HHDM-mapped.
    unsafe { hhdm.write_volatile(0x1234_5678) };
    let virt = vmm::map_mmio(boot, frame + 0x10, 8).expect("map_mmio");
    check(
        virt & 0xFFF == 0x10,
        "MMIO mapping preserves the in-page offset",
    );
    // SAFETY: `virt` maps the frame we own.
    let read = (virt - 0x10) as *const u32;
    // SAFETY: as above.
    check(
        unsafe { read.read_volatile() } == 0x1234_5678,
        "MMIO alias reads what RAM holds",
    );
    // SAFETY: as above.
    unsafe { read.cast_mut().write_volatile(0xCAFE_F00D) };
    // SAFETY: the frame is ours and HHDM-mapped.
    check(
        unsafe { hhdm.read_volatile() } == 0xCAFE_F00D,
        "MMIO writes reach the frame",
    );
    let leaf = vmm::lookup(boot, virt).expect("leaf");
    check(
        leaf.flags & (1 << 4) != 0 && !leaf.executable(),
        "MMIO mapping is uncached and non-executable",
    );
    pmm::free(frame);
    kprintln!("selftest: mmio-map ok");
}

fn test_layout(boot: &BootInfo) {
    extern "C" {
        static __text_start: u8;
        static __rodata_start: u8;
        static __data_start: u8;
    }
    let (text, ro, data) = {
        (
            core::ptr::addr_of!(__text_start) as u64,
            core::ptr::addr_of!(__rodata_start) as u64,
            core::ptr::addr_of!(__data_start) as u64,
        )
    };
    let t = vmm::lookup(boot, text).expect("text mapped");
    check(t.executable() && !t.writable(), "kernel .text is R-X");
    let r = vmm::lookup(boot, ro).expect("rodata mapped");
    check(!r.executable() && !r.writable(), "kernel .rodata is R--");
    let d = vmm::lookup(boot, data).expect("data mapped");
    check(!d.executable() && d.writable(), "kernel .data is RW- (NX)");
    check(t.size == 4096, "kernel text uses exact 4 KiB mappings");
    let image_phys = t.phys;
    check(
        vmm::lookup(boot, boot.hhdm + image_phys).is_none(),
        "no writable HHDM alias of kernel text",
    );
    let (leaves, violations) = vmm::audit(boot);
    check(
        violations == 0 && leaves > 0,
        "W^X audit finds no writable+executable mapping",
    );
    check(
        vmm::lookup(boot, crate::stacks::kernel_stack_guard()).is_none(),
        "kernel stack guard page is unmapped",
    );
    kprintln!("selftest: vmm-layout ok ({leaves} leaf mappings)");
}

static IPI_COUNT: AtomicU64 = AtomicU64::new(0);

fn ipi_handler(_: &mut crate::idt::TrapFrame) {
    // Allocating (and freeing) inside an interrupt handler proves the allocator locks are IRQ-safe.
    let b = Box::new([0xABu8; 96]);
    let v: Vec<u8> = alloc::vec![1; 300];
    IPI_COUNT.fetch_add(u64::from(b[0] == 0xAB && v.len() == 300), Ordering::SeqCst);
}

fn test_irq() {
    let vector = irq::alloc_vector().expect("vector");
    irq::register(vector, ipi_handler);
    // 1. A self-IPI is delivered, dispatched and acknowledged.
    apic::send_self_ipi(vector);
    let t0 = time::now_ns();
    while IPI_COUNT.load(Ordering::SeqCst) < 1 && time::now_ns() - t0 < 1_000_000_000 {
        arch::enable_and_halt();
    }
    check(
        IPI_COUNT.load(Ordering::SeqCst) == 1,
        "self-IPI reaches its handler exactly once",
    );
    // 2. Allocation churn on the main path interleaved with interrupts that also allocate.
    let mut sent = 1u64;
    for i in 0..20_000u32 {
        let b = Box::new(i);
        let v: Vec<u32> = alloc::vec![i; 40];
        assert!(*b == i && v[39] == i);
        if i % 50 == 0 {
            apic::send_self_ipi(vector);
            sent += 1;
        }
    }
    let t0 = time::now_ns();
    while IPI_COUNT.load(Ordering::SeqCst) < sent && time::now_ns() - t0 < 1_000_000_000 {
        arch::enable_and_halt();
    }
    check(
        IPI_COUNT.load(Ordering::SeqCst) == sent,
        "every interrupt that allocated was handled without deadlock",
    );
    check(
        arch::interrupts_enabled(),
        "interrupts are re-enabled after IRQ-safe locks are released",
    );
    kprintln!("selftest: irq ok ({sent} interrupts)");
}

fn test_timer() {
    let (t0, k0) = (time::now_ns(), time::ticks());
    time::sleep_ms(100);
    let (dt, dk) = (time::now_ns() - t0, time::ticks() - k0);
    check(
        dt >= 100_000_000,
        "sleep_ms(100) sleeps at least 100 ms by the TSC clock",
    );
    check(dt < 400_000_000, "sleep_ms(100) does not oversleep wildly");
    check(
        (70..=140).contains(&dk),
        "the 1 kHz tick advances about 100 times in 100 ms",
    );
    let (a, b) = (time::now_ns(), time::now_ns());
    check(b >= a, "the clock is monotonic");
    kprintln!(
        "selftest: timer ok (slept {} ms, {} ticks)",
        dt / 1_000_000,
        dk
    );
}

fn test_msi() {
    let m = msi::compose(0, 0x41).expect("compose");
    check(
        m.address == 0xFEE0_0000 && m.data == 0x41,
        "MSI message for APIC 0, vector 0x41",
    );
    let m = msi::compose(3, 0x50).expect("compose");
    check(
        m.address == 0xFEE0_3000 && m.data == 0x50,
        "MSI destination ID lands in address bits 19:12",
    );
    check(
        msi::compose(255, 0x50).is_none(),
        "APIC IDs beyond 8 bits are rejected",
    );
    check(
        msi::compose(0, 0x10).is_none(),
        "exception vectors are rejected",
    );
    kprintln!("selftest: msi ok");
}

fn madt_bytes(entries: &[&[u8]]) -> Vec<u8> {
    let mut t = alloc::vec![0u8; 44];
    t[0..4].copy_from_slice(b"APIC");
    t[36..40].copy_from_slice(&0xFEE0_0000u32.to_le_bytes());
    for e in entries {
        t.extend_from_slice(e);
    }
    let len = t.len() as u32;
    t[4..8].copy_from_slice(&len.to_le_bytes());
    t
}

fn test_madt_parser() {
    use madt::{Polarity, Trigger};
    let cpu: &[u8] = &[0, 8, 0, 0, 1, 0, 0, 0];
    let cpu_disabled: &[u8] = &[0, 8, 1, 1, 0, 0, 0, 0];
    let ioapic: &[u8] = &[1, 12, 2, 0, 0x00, 0x00, 0xC0, 0xFE, 0, 0, 0, 0];
    let iso_timer: &[u8] = &[2, 10, 0, 0, 2, 0, 0, 0, 0, 0];
    let iso_level_low: &[u8] = &[2, 10, 0, 9, 9, 0, 0, 0, 0x0F, 0];
    let nmi: &[u8] = &[4, 6, 0xFF, 0x00, 0x00, 1];
    let m = madt::parse(&madt_bytes(&[
        cpu,
        cpu_disabled,
        ioapic,
        iso_timer,
        iso_level_low,
        nmi,
    ]))
    .expect("valid MADT");
    check(m.processors == 1, "only enabled processors are counted");
    check(
        m.ioapics.len() == 1 && m.ioapics[0].address == 0xFEC0_0000,
        "I/O APIC address parsed",
    );
    check(m.isa_irq(0).0 == 2, "ISA IRQ 0 is overridden to GSI 2");
    let (gsi, pol, trig) = m.isa_irq(9);
    check(
        gsi == 9 && pol == Polarity::ActiveLow && trig == Trigger::Level,
        "level/low override decoded",
    );
    check(
        m.isa_irq(4) == (4, Polarity::ActiveHigh, Trigger::Edge),
        "un-overridden IRQs default to ISA edge/high",
    );
    check(
        m.nmis.len() == 1 && m.nmis[0].lint == 1 && m.nmis[0].processor_uid == u32::MAX,
        "LAPIC NMI parsed (all CPUs, LINT1)",
    );
    // Hostile input must never panic, loop forever or read out of bounds.
    check(
        madt::parse(&[]) == Err(MadtError::TooShort),
        "empty table is rejected",
    );
    let mut bad = madt_bytes(&[cpu]);
    bad[0] = b'X';
    check(
        madt::parse(&bad) == Err(MadtError::BadSignature),
        "wrong signature is rejected",
    );
    let zero_len: &[u8] = &[0, 0, 0, 0];
    check(
        madt::parse(&madt_bytes(&[zero_len, cpu])).is_ok(),
        "a zero-length entry stops parsing without looping",
    );
    let mut truncated = madt_bytes(&[ioapic]);
    truncated.truncate(truncated.len() - 5);
    check(
        madt::parse(&truncated).is_ok_and(|m| m.ioapics.is_empty()),
        "a truncated entry is ignored",
    );
    let overlong: &[u8] = &[2, 250, 0, 0];
    check(
        madt::parse(&madt_bytes(&[overlong])).is_ok_and(|m| m.overrides.is_empty()),
        "an entry longer than the table is ignored",
    );
    // Every prefix of a valid table parses without panicking.
    let full = madt_bytes(&[cpu, ioapic, iso_timer, nmi]);
    for n in 0..=full.len() {
        let _ = madt::parse(&full[..n]);
    }
    check(true, "all truncations of a valid table parse safely");
    kprintln!("selftest: madt-parser ok");
}
