//! Boot-time self-tests selected through the kernel command line
//! (`selftest=<name>`), used by the end-to-end test suite to prove the
//! exception machinery works on real (emulated) hardware. Production images
//! carry no command line, so none of this runs.

use crate::boot::BootInfo;
use crate::{heap, pmm, vmm};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::asm;

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
