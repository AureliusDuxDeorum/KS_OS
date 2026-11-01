//! Kernel virtual memory manager.
//!
//! Replaces the bootloader's page tables with tables the kernel builds and
//! owns, with strict W^X:
//!
//! * kernel image: `.text` R-X, `.rodata` R--, `.data`/`.bss` RW- (NX), stack guard pages unmapped;
//! * higher-half direct map (HHDM) of RAM-like regions: RW-, NX, 2 MiB pages where possible,
//!   *excluding* the kernel image's own physical pages (no writable alias of kernel code);
//! * framebuffer: RW-, write-combining;
//! * an MMIO window for device registers: RW-, uncached.
//!
//! The PAT is programmed so cache attributes can be expressed per mapping.

use crate::arch;
use crate::boot::BootInfo;
use crate::frames::Frames;
use crate::limine;
use crate::paging::{MapError, Mapper, Perm, Span};
use crate::pmm;
use crate::stacks;
use core::sync::atomic::{AtomicU64, Ordering};

const IA32_PAT: u32 = 0x277;
/// PA0=WB PA1=WT PA2=UC- PA3=UC PA4=WB PA5=WC PA6=UC- PA7=UC (zero terms omitted).
const PAT_VALUE: u64 =
    0x06 | (0x04 << 8) | (0x07 << 16) | (0x06 << 32) | (0x01 << 40) | (0x07 << 48);

/// PML4 slot reserved for device MMIO mappings (pre-populated so every address
/// space that shares the kernel half sees later mappings).
const MMIO_PML4_INDEX: u64 = 400;
const MMIO_BASE: u64 = 0xFFFF_0000_0000_0000 | (MMIO_PML4_INDEX << 39);
const MMIO_LIMIT: u64 = MMIO_BASE + (1 << 39);

static KERNEL_ROOT: AtomicU64 = AtomicU64::new(0);
static NEXT_MMIO: AtomicU64 = AtomicU64::new(MMIO_BASE);
static NX: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
#[allow(dead_code)]
pub enum VmmError {
    Map(MapError),
    NoExecutableAddress,
    MmioExhausted,
}

impl From<MapError> for VmmError {
    fn from(e: MapError) -> Self {
        Self::Map(e)
    }
}

extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __data_end: u8;
}

/// Physical address of the root of the kernel address space.
pub fn kernel_root() -> u64 {
    KERNEL_ROOT.load(Ordering::Relaxed)
}

/// Records a new kernel root (used if the EFI runtime has to keep its own tables).
pub fn set_kernel_root(root: u64) {
    KERNEL_ROOT.store(root, Ordering::Relaxed);
}

pub fn nx_enabled() -> bool {
    NX.load(Ordering::Relaxed) != 0
}

fn sym(s: &u8) -> u64 {
    core::ptr::from_ref(s) as u64
}

/// Builds the kernel address space and switches to it.
pub fn init(boot: &BootInfo) -> Result<(), VmmError> {
    let nx = arch::cpu_has_nx();
    if nx {
        // SAFETY: EFER exists on every x86_64 CPU; NXE only enables the XD page bit.
        unsafe { arch::wrmsr(arch::MSR_EFER, arch::rdmsr(arch::MSR_EFER) | arch::EFER_NXE) };
    }
    NX.store(u64::from(nx), Ordering::Relaxed);
    // SAFETY: IA32_PAT exists on every CPU we support (PAT is mandatory for x86_64).
    unsafe { arch::wrmsr(IA32_PAT, PAT_VALUE) };

    let exec = limine::EXECUTABLE_ADDRESS
        .response()
        .ok_or(VmmError::NoExecutableAddress)?;
    let (phys_base, virt_base) = (exec.physical_base, exec.virtual_base);
    // SAFETY: only the addresses of linker-provided symbols are taken.
    let (text, text_end, ro, ro_end, data, data_end) = unsafe {
        (
            sym(&__text_start),
            sym(&__text_end),
            sym(&__rodata_start),
            sym(&__rodata_end),
            sym(&__data_start),
            sym(&__data_end),
        )
    };
    let to_phys = |v: u64| phys_base + (v - virt_base);

    let mut frames = Frames;
    let mapper = Mapper::new(boot, nx);
    let root = mapper.new_root(&mut frames)?;

    // Kernel image.
    let seg = |m: &Mapper<'_>, f: &mut Frames, s: u64, e: u64, perm: Perm| {
        let (s, e) = (s & !0xFFF, e.next_multiple_of(4096));
        m.map_range(
            f,
            root,
            Span {
                virt: s,
                phys: to_phys(s),
                len: e - s,
            },
            perm,
            false,
        )
    };
    seg(&mapper, &mut frames, text, text_end, Perm::RX)?;
    seg(&mapper, &mut frames, ro, ro_end, Perm::R)?;
    seg(&mapper, &mut frames, data, data_end, Perm::RW)?;
    let image_phys = (
        to_phys(text) & !0xFFF,
        to_phys(data_end.next_multiple_of(4096)),
    );

    // Direct map of RAM-like regions and the framebuffer.
    for e in boot.memmap() {
        let (start, end) = (e.base & !0xFFF, (e.base + e.length).next_multiple_of(4096));
        match e.kind {
            limine::MEMMAP_FRAMEBUFFER => {
                mapper.map_range(
                    &mut frames,
                    root,
                    Span {
                        virt: boot.hhdm + start,
                        phys: start,
                        len: end - start,
                    },
                    Perm::FRAMEBUFFER,
                    false,
                )?;
            }
            limine::MEMMAP_EXECUTABLE_AND_MODULES => {
                // Map modules but never a writable alias of the kernel image itself.
                let (is, ie) = image_phys;
                for (s, t) in [(start, end.min(is)), (start.max(ie), end)] {
                    if s < t {
                        mapper.map_range(
                            &mut frames,
                            root,
                            Span {
                                virt: boot.hhdm + s,
                                phys: s,
                                len: t - s,
                            },
                            Perm::RW,
                            true,
                        )?;
                    }
                }
            }
            limine::MEMMAP_USABLE
            | limine::MEMMAP_ACPI_RECLAIMABLE
            | limine::MEMMAP_ACPI_NVS
            | limine::MEMMAP_BOOTLOADER_RECLAIMABLE
            | limine::MEMMAP_RESERVED_MAPPED => {
                mapper.map_range(
                    &mut frames,
                    root,
                    Span {
                        virt: boot.hhdm + start,
                        phys: start,
                        len: end - start,
                    },
                    Perm::RW,
                    true,
                )?;
            }
            _ => {}
        }
    }

    // Pre-populate the MMIO window's top-level slot so later mappings stay visible
    // in every address space that shares the kernel half.
    mapper.map_page(
        &mut frames,
        root,
        MMIO_BASE,
        pmm::alloc_zeroed().ok_or(MapError::OutOfFrames)?,
        Perm::MMIO,
    )?;
    mapper.unmap(root, MMIO_BASE)?;

    // Guard pages below every kernel/IST stack.
    stacks::install_guards(&mapper, root)?;

    let (leaves, violations) = mapper.audit_wx(root);
    kprintln!(
        "vmm: kernel address space built: {leaves} leaf mappings, {violations} W^X violations"
    );
    assert!(
        violations == 0,
        "vmm: W^X violation in kernel address space"
    );

    // SAFETY: the new tables map the running kernel image, its stacks, the HHDM
    // (bootloader data, page tables, framebuffer) and nothing the kernel uses is left out.
    unsafe { arch::write_cr3(root) };
    set_kernel_root(root);
    Ok(())
}

/// Maps a device MMIO range uncached into the MMIO window and returns its virtual address.
pub fn map_mmio(boot: &BootInfo, phys: u64, len: u64) -> Result<u64, VmmError> {
    let offset = phys & 0xFFF;
    let base = phys & !0xFFF;
    let size = (len + offset).next_multiple_of(4096);
    let virt = NEXT_MMIO.fetch_add(size, Ordering::Relaxed);
    if virt + size > MMIO_LIMIT {
        return Err(VmmError::MmioExhausted);
    }
    let mapper = Mapper::new(boot, nx_enabled());
    let mut frames = Frames;
    mapper.map_range(
        &mut frames,
        kernel_root(),
        Span {
            virt,
            phys: base,
            len: size,
        },
        Perm::MMIO,
        false,
    )?;
    Ok(virt + offset)
}

/// Whether `virt` is mapped in the kernel address space, with its leaf details.
pub fn lookup(boot: &BootInfo, virt: u64) -> Option<crate::paging::Leaf> {
    Mapper::new(boot, nx_enabled()).lookup(kernel_root(), virt)
}

/// Re-runs the W^X audit on the current kernel root.
pub fn audit(boot: &BootInfo) -> (u64, u64) {
    Mapper::new(boot, nx_enabled()).audit_wx(kernel_root())
}

/// Lists up to `limit` writable+executable mappings in the kernel root (diagnostics).
pub fn find_wx(boot: &BootInfo, limit: usize) -> alloc::vec::Vec<(u64, u64)> {
    Mapper::new(boot, nx_enabled()).find_wx(kernel_root(), limit)
}
