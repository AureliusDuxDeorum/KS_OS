// SPDX-License-Identifier: Apache-2.0
//! Boot-time self-tests selected through the kernel command line
//! (`selftest=<name>`), used by the end-to-end test suite to prove the
//! exception machinery works on real (emulated) hardware. Production images
//! carry no command line, so none of this runs.

use crate::arch;
use crate::boot::BootInfo;
use crate::madt::{self, MadtError};
use crate::{apic, heap, irq, msi, pci, pmm, time, usb, vmm};
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
        "rng" => test_rng(),
        "usb" => test_usb(),
        "usb-hid" => test_usb_hid(),
        "usb-bot" => test_usb_bot(),
        "pci" => test_pci(),
        "pci-edu" => test_pci_edu(boot),
        "pci-msix" => test_pci_msix(boot),
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
    check(
        again <= run && again.is_multiple_of(2 << 20),
        "a freed run is reusable (first-fit returns the lowest suitable run)",
    );
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

fn test_pci() {
    let devs = pci::devices();
    check(!devs.is_empty(), "PCI enumeration found devices");
    check(
        devs.iter()
            .any(|d| d.header.class == 0x06 && d.header.subclass == 0x00),
        "a host bridge is present",
    );
    let mut addrs: Vec<_> = devs
        .iter()
        .map(|d| (d.addr.segment, d.addr.bus, d.addr.device, d.addr.function))
        .collect();
    addrs.sort_unstable();
    addrs.dedup();
    check(
        addrs.len() == devs.len(),
        "every function is enumerated exactly once",
    );
    check(
        devs.iter()
            .flat_map(|d| d.bars.iter().flatten())
            .all(|b| b.size.is_power_of_two()),
        "every BAR size is a power of two",
    );
    check(
        devs.iter()
            .filter(|d| d.header.kind == 1)
            .all(|d| d.header.secondary_bus > d.addr.bus),
        "bridge secondary buses are numbered above their primary",
    );
    kprintln!("selftest: pci ok ({} functions)", devs.len());
}

static EDU_BASE: AtomicU64 = AtomicU64::new(0);
static EDU_IRQS: AtomicU64 = AtomicU64::new(0);
static EDU_LAST_STATUS: AtomicU64 = AtomicU64::new(0);

fn edu_reg(off: u64) -> *mut u32 {
    (EDU_BASE.load(Ordering::Relaxed) + off) as *mut u32
}

fn edu_handler(_: &mut crate::idt::TrapFrame) {
    // SAFETY: the BAR is mapped uncached; reading status then acknowledging is the device protocol.
    unsafe {
        let status = edu_reg(0x24).read_volatile();
        EDU_LAST_STATUS.store(u64::from(status), Ordering::SeqCst);
        edu_reg(0x64).write_volatile(status);
    }
    EDU_IRQS.fetch_add(1, Ordering::SeqCst);
}

fn wait_edu_irqs(n: u64) -> bool {
    let t0 = time::now_ns();
    while EDU_IRQS.load(Ordering::SeqCst) < n && time::now_ns() - t0 < 2_000_000_000 {
        arch::enable_and_halt();
    }
    EDU_IRQS.load(Ordering::SeqCst) >= n
}

fn test_pci_edu(boot: &BootInfo) {
    let Some(dev) = pci::find(0x1234, 0x11e8) else {
        panic!("selftest FAILED: QEMU `edu` device not present (run with -device edu)");
    };
    pci::enable_bus_master(dev);
    let base = pci::map_bar(boot, dev, 0).expect("map BAR0");
    EDU_BASE.store(base, Ordering::Relaxed);
    // SAFETY: BAR0 is mapped uncached and 1 MiB long; all offsets below are within it.
    let rd = |off| unsafe { edu_reg(off).read_volatile() };
    // SAFETY: as above.
    let wr = |off, v| unsafe { edu_reg(off).write_volatile(v) };

    check(
        rd(0) & 0xFFFF == 0x00ED,
        "device identification register reads back",
    );
    wr(4, 0x1234_5678);
    check(
        rd(4) == !0x1234_5678,
        "liveness register inverts what was written (MMIO writes work)",
    );

    // MSI/MSI-X: raise an interrupt and see it arrive in our handler.
    let vector = irq::alloc_vector().expect("vector");
    irq::register(vector, edu_handler);
    let kind = pci::enable_msi(boot, dev, vector, apic::id()).expect("enable MSI");
    kprintln!("selftest:   using {kind}");
    wr(0x60, 0x1);
    check(wait_edu_irqs(1), "a device-raised MSI reaches the handler");
    check(
        EDU_LAST_STATUS.load(Ordering::SeqCst) == 1,
        "the handler sees the right device interrupt status",
    );

    // Factorial with completion interrupt.
    wr(0x20, 0x80);
    wr(0x08, 5);
    check(wait_edu_irqs(2), "factorial-complete MSI arrives");
    check(rd(0x08) == 120, "device computed 5! = 120");

    // DMA within the device's 28-bit address limit, using the PMM's DMA allocator.
    let buf = pmm::alloc_contiguous(1, 1, 1 << 28).expect("DMA frame below 256 MiB");
    let virt = (boot.hhdm + buf) as *mut u8;
    for i in 0..4096usize {
        // SAFETY: we own this frame.
        unsafe { virt.add(i).write((i * 7 + 3) as u8) };
    }
    let dma = |src: u64, dst: u64, cmd: u32| {
        wr(0x80, src as u32);
        wr(0x84, (src >> 32) as u32);
        wr(0x88, dst as u32);
        wr(0x8C, (dst >> 32) as u32);
        wr(0x90, 4096);
        wr(0x98, cmd);
        let t0 = time::now_ns();
        while rd(0x98) & 1 != 0 && time::now_ns() - t0 < 2_000_000_000 {
            core::hint::spin_loop();
        }
        rd(0x98) & 1 == 0
    };
    check(dma(buf, 0x40000, 1), "DMA RAM -> device completes");
    // SAFETY: we own this frame.
    unsafe { core::ptr::write_bytes(virt, 0, 4096) };
    let before = EDU_IRQS.load(Ordering::SeqCst);
    check(dma(0x40000, buf, 1 | 2 | 4), "DMA device -> RAM completes");
    check(wait_edu_irqs(before + 1), "DMA-complete MSI arrives");
    // SAFETY: we own this frame.
    let ok = (0..4096usize).all(|i| unsafe { virt.add(i).read() } == (i * 7 + 3) as u8);
    check(ok, "4 KiB survives the round trip through device memory");
    pmm::free(buf);
    kprintln!(
        "selftest: pci-edu ok ({} interrupts)",
        EDU_IRQS.load(Ordering::SeqCst)
    );
}

fn test_pci_msix(boot: &BootInfo) {
    // virtio-net (modern) exposes MSI-X with its table in a BAR.
    let Some(dev) = pci::find(0x1af4, 0x1041) else {
        panic!("selftest FAILED: no virtio-net-pci device (run with -device virtio-net-pci,disable-legacy=on)");
    };
    let cap = dev
        .caps
        .iter()
        .copied()
        .find(|c| c.id == ksparse::pci::CAP_MSIX)
        .expect("MSI-X capability");
    let x = ksparse::pci::msix(&PciCfg(dev.addr), cap).expect("decode MSI-X");
    let vector = 0x77u8;
    let kind = pci::enable_msi(boot, dev, vector, 0).expect("enable MSI-X");
    check(
        kind == "MSI-X",
        "MSI-X is preferred over MSI when both exist",
    );
    let table = pci::map_bar(boot, dev, usize::from(x.table_bar)).expect("map table BAR")
        + u64::from(x.table_offset);
    let e = table as *const u32;
    // SAFETY: the table lies inside the mapped BAR (validated by `enable_msi`).
    let (lo, hi, data, ctl) = unsafe {
        (
            e.read_volatile(),
            e.add(1).read_volatile(),
            e.add(2).read_volatile(),
            e.add(3).read_volatile(),
        )
    };
    check(
        lo == 0xFEE0_0000 && hi == 0,
        "table entry 0 holds the MSI address for APIC 0",
    );
    check(data == u32::from(vector), "table entry 0 holds the vector");
    check(ctl & 1 == 0, "table entry 0 is unmasked");
    let control = pci::read32(dev.addr, u16::from(cap.offset)) >> 16;
    check(
        control & (1 << 15) != 0 && control & (1 << 14) == 0,
        "MSI-X is enabled and the function is unmasked",
    );
    check(
        pci::read32(dev.addr, 4) & (1 << 10) != 0,
        "legacy INTx is disabled once MSI-X is on",
    );
    kprintln!(
        "selftest: pci-msix ok (table {} entries in BAR{})",
        x.table_size,
        x.table_bar
    );
}

struct PciCfg(pci::Address);
impl ksparse::pci::Config for PciCfg {
    fn read32(&self, offset: u16) -> u32 {
        pci::read32(self.0, offset)
    }
}

fn test_usb() {
    let devs = usb::devices();
    check(
        devs.len() >= 4,
        "hub, keyboard, mouse and a keyboard behind the hub were enumerated",
    );
    check(
        devs.iter().any(|d| d.parent.is_some()),
        "a device behind a hub was enumerated through its route string",
    );
    check(
        devs.iter()
            .all(|d| d.descriptor.num_configs >= 1 && !d.config.interfaces.is_empty()),
        "every device has a parsed configuration",
    );
    check(usb::hid::has_pointer(), "a relative pointer is bound");
    kprintln!("selftest: usb ok ({} devices)", devs.len());
}

fn test_usb_hid() {
    use crate::input::{self, Event, Key};
    kprintln!("selftest: usb-hid ready");
    let (mut key, mut mouse) = (false, false);
    let t0 = time::now_ns();
    while !(key && mouse) && time::now_ns() - t0 < 30_000_000_000 {
        match input::pop() {
            Some(Event::Key(Key::Char(b'p'))) => {
                kprintln!("selftest:   ok  USB keyboard 'p' arrived through interrupt endpoint + report decoding");
                key = true;
            }
            Some(Event::Mouse { dx, dy, .. }) if dx > 0 && dy > 0 => {
                kprintln!("selftest:   ok  USB mouse motion dx={dx} dy={dy} arrived");
                mouse = true;
            }
            Some(_) => {}
            None => arch::enable_and_halt(),
        }
    }
    check(key, "keyboard report received");
    check(mouse, "mouse report received");
    kprintln!(
        "selftest: usb-hid ok ({} reports)",
        usb::hid::reports_seen()
    );
}

fn test_usb_bot() {
    use crate::usb::xhci;
    use ksparse::usb::TransferKind;
    let (d, iface) =
        usb::find_interface(8, 6, 0x50).expect("USB mass-storage (bulk-only) interface");
    let ep = |inward: bool| {
        iface
            .endpoints
            .iter()
            .copied()
            .find(|e| e.kind() == TransferKind::Bulk && e.is_in() == inward)
            .expect("bulk endpoint")
    };
    let (out_ep, in_ep) = (ep(false), ep(true));
    let cfg = |e: ksparse::usb::Endpoint| xhci::EndpointConfig {
        dci: e.dci(),
        kind: e.kind(),
        is_in: e.is_in(),
        max_packet: e.packet_size(),
        max_burst: e.max_burst,
        interval: e.interval,
    };
    xhci::configure_endpoints(d.dev, &[cfg(out_ep), cfg(in_ep)], None)
        .expect("configure bulk endpoints");
    let mut buf = xhci::alloc_buffer(d.dev, 4096).expect("DMA buffer");
    // Command Block Wrapper: READ(10), LBA 0, one 512-byte block.
    let mut cbw = [0u8; 31];
    cbw[0..4].copy_from_slice(&0x4342_5355u32.to_le_bytes()); // 'USBC'
    cbw[4..8].copy_from_slice(&0xC0DE_0001u32.to_le_bytes()); // tag
    cbw[8..12].copy_from_slice(&512u32.to_le_bytes());
    cbw[12] = 0x80; // device-to-host
    cbw[14] = 10; // command length
    cbw[15] = 0x28; // READ(10)
    cbw[22] = 1; // transfer length (blocks), big-endian at bytes 22..24
    buf.as_mut_slice()[..31].copy_from_slice(&cbw);
    let sent = xhci::transfer(d.dev, out_ep.dci(), buf.phys, 31, 2000).expect("send CBW");
    check(sent == 31, "CBW goes out on the bulk OUT endpoint");
    let got = xhci::transfer(d.dev, in_ep.dci(), buf.phys, 512, 2000).expect("read data stage");
    check(got == 512, "512 bytes arrive on the bulk IN endpoint");
    let ok = buf.as_slice().starts_with(b"KSOS-USB-BOT-TEST")
        && (17..512).all(|i| buf.as_slice()[i] == (i as u8).wrapping_mul(3));
    check(ok, "sector 0 holds exactly what the test image contains");
    let n = xhci::transfer(d.dev, in_ep.dci(), buf.phys, 13, 2000).expect("read CSW");
    let csw = &buf.as_slice()[..13];
    check(
        n == 13 && csw[0..4] == 0x5342_5355u32.to_le_bytes(),
        "Command Status Wrapper arrives with its signature",
    );
    check(
        csw[4..8] == 0xC0DE_0001u32.to_le_bytes() && csw[12] == 0,
        "CSW echoes the tag and reports success",
    );
    buf.free();
    kprintln!("selftest: usb-bot ok");
}

fn test_rng() {
    use ksbt::Rng;
    check(
        crate::rng::available(),
        "the CPU provides a hardware RNG (run with -cpu max)",
    );
    let mut r = crate::rng::HwRng;
    let mut a = [0u8; 64];
    let mut b = [0u8; 64];
    r.fill(&mut a);
    r.fill(&mut b);
    check(a != b, "two draws differ");
    check(
        a.iter().any(|&x| x != 0) && b.iter().any(|&x| x != 0),
        "draws are not all zero",
    );
    let mut ones = 0u32;
    for _ in 0..64 {
        let mut x = [0u8; 32];
        r.fill(&mut x);
        ones += x.iter().map(|b| b.count_ones()).sum::<u32>();
    }
    let total = 64 * 32 * 8;
    check(
        (total * 45 / 100..total * 55 / 100).contains(&ones),
        "bit balance is within 45-55 %",
    );
    kprintln!("selftest: rng ok");
}
