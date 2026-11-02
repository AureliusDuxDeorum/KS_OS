// SPDX-License-Identifier: Apache-2.0
//! Safe view over the information the bootloader hands us.

use crate::limine::{self, Framebuffer, MemmapEntry};

/// Everything we learned from the bootloader, gathered once at entry.
pub struct BootInfo {
    pub hhdm: u64,
    pub framebuffer: Option<&'static Framebuffer>,
    memmap: &'static [*const MemmapEntry],
    pub rsdp: Option<u64>,
    pub efi_system_table: Option<u64>,
    pub efi_memmap: Option<(&'static [u8], usize)>,
    pub bootloader: (&'static str, &'static str),
    pub cmdline: &'static str,
    modules: &'static [*const limine::File],
}

/// A file loaded by the bootloader as a module (e.g. Bluetooth firmware).
pub struct Module {
    pub path: &'static str,
    pub data: &'static [u8],
}

impl Module {
    /// File name without directories.
    pub fn name(&self) -> &'static str {
        self.path.rsplit('/').next().unwrap_or(self.path)
    }
}

/// Converts a NUL-terminated C string from the bootloader into a `&str`.
///
/// # Safety
/// `p` must be null or point to a NUL-terminated string valid for `'static`.
unsafe fn cstr(p: *const core::ffi::c_char, max: usize) -> &'static str {
    if p.is_null() {
        return "?";
    }
    let mut len = 0;
    // SAFETY: caller guarantees NUL termination.
    while unsafe { *p.add(len) } != 0 && len < max {
        len += 1;
    }
    // SAFETY: `len` bytes were just read from the string.
    let bytes = unsafe { core::slice::from_raw_parts(p.cast::<u8>(), len) };
    core::str::from_utf8(bytes).unwrap_or("?")
}

impl BootInfo {
    /// Collects boot information. Returns `Err` if a mandatory item is missing.
    pub fn collect() -> Result<Self, &'static str> {
        if !limine::BASE.is_supported() {
            return Err("bootloader does not support the requested Limine base revision");
        }
        let hhdm = limine::HHDM.response().ok_or("no HHDM response")?.offset;
        let mm = limine::MEMMAP.response().ok_or("no memory map response")?;
        let memmap = if mm.entry_count == 0 {
            &[][..]
        } else {
            // SAFETY: Limine provides `entry_count` valid entry pointers.
            unsafe { core::slice::from_raw_parts(mm.entries, mm.entry_count as usize) }
        };
        let framebuffer = limine::FRAMEBUFFER.response().and_then(|r| {
            if r.framebuffer_count == 0 {
                None
            } else {
                // SAFETY: at least one framebuffer pointer is present.
                unsafe { (*r.framebuffers).as_ref() }
            }
        });
        let efi_memmap = limine::EFI_MEMMAP.response().map(|r| {
            // SAFETY: the EFI memory map lives in bootloader-reclaimable (mapped) memory.
            let bytes = unsafe { core::slice::from_raw_parts(r.memmap, r.memmap_size as usize) };
            (bytes, r.desc_size as usize)
        });
        let bootloader = limine::BOOTLOADER_INFO.response().map_or(("?", "?"), |r| {
            // SAFETY: Limine supplies NUL-terminated strings.
            unsafe { (cstr(r.name, 64), cstr(r.version, 64)) }
        });
        let cmdline = limine::EXECUTABLE_CMDLINE
            .response()
            // SAFETY: Limine supplies a NUL-terminated command line.
            .map_or("", |r| unsafe { cstr(r.cmdline, 128) });
        let modules = limine::MODULES.response().map_or(&[][..], |r| {
            if r.module_count == 0 {
                &[]
            } else {
                // SAFETY: Limine provides `module_count` valid file pointers.
                unsafe { core::slice::from_raw_parts(r.modules, r.module_count as usize) }
            }
        });
        Ok(Self {
            cmdline,
            modules,
            hhdm,
            framebuffer,
            memmap,
            rsdp: limine::RSDP.response().map(|r| r.address),
            efi_system_table: limine::EFI_SYSTEM_TABLE.response().map(|r| r.address),
            efi_memmap,
            bootloader,
        })
    }

    /// Files the bootloader loaded as modules.
    pub fn modules(&self) -> impl Iterator<Item = Module> + '_ {
        self.modules.iter().filter_map(|&f| {
            // SAFETY: Limine guarantees each file pointer, its path string and its data range are valid
            // (module memory is mapped in the HHDM for the kernel's lifetime).
            unsafe {
                let f = f.as_ref()?;
                Some(Module {
                    path: cstr(f.path, 128),
                    data: core::slice::from_raw_parts(f.address, f.size as usize),
                })
            }
        })
    }

    /// Iterates over the bootloader memory map.
    pub fn memmap(&self) -> impl Iterator<Item = &'static MemmapEntry> + '_ {
        // SAFETY: Limine guarantees every entry pointer is valid.
        self.memmap.iter().map(|p| unsafe { &**p })
    }

    /// Virtual address of a physical address through the higher-half direct map.
    ///
    /// Only valid for memory the bootloader mapped (see [`Self::hhdm_covers`]).
    pub fn phys_to_virt(&self, phys: u64) -> u64 {
        self.hhdm + phys
    }

    /// Whether `[phys, phys+len)` is entirely backed by memory-map regions that
    /// base revision 5 maps into the HHDM. Guards every firmware-pointer dereference.
    pub fn hhdm_covers(&self, phys: u64, len: u64) -> bool {
        let Some(end) = phys.checked_add(len) else {
            return false;
        };
        let mut cursor = phys;
        'outer: while cursor < end {
            for e in self.memmap() {
                let mapped = matches!(
                    e.kind,
                    limine::MEMMAP_USABLE
                        | limine::MEMMAP_ACPI_RECLAIMABLE
                        | limine::MEMMAP_ACPI_NVS
                        | limine::MEMMAP_BOOTLOADER_RECLAIMABLE
                        | limine::MEMMAP_EXECUTABLE_AND_MODULES
                        | limine::MEMMAP_FRAMEBUFFER
                        | limine::MEMMAP_RESERVED_MAPPED
                );
                if mapped && cursor >= e.base && cursor < e.base + e.length {
                    cursor = e.base + e.length;
                    continue 'outer;
                }
            }
            return false;
        }
        true
    }
}
