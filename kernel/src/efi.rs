//! UEFI Runtime Services.
//!
//! The firmware stays resident after the bootloader exits boot services. We
//! use it for two things Stage 1 needs: persisting the "boot to firmware
//! setup" request (`OsIndications`) and resetting/shutting down the machine.
//!
//! Runtime services were written to execute at physical addresses, but our
//! kernel owns the address space, so (as Linux does) we:
//!   1. build page tables that map every `EFI_MEMORY_RUNTIME` region both at
//!      its physical address and inside a dedicated virtual window,
//!   2. call `SetVirtualAddressMap()` so the firmware relocates itself into
//!      that window, and
//!   3. switch to tables that contain only the window, keeping the lower half
//!      of the address space free for user mode later.
//!
//! Should a firmware reject the virtual map, we keep the identity mapping and
//! call it in physical mode.
//!
//! Mappings honour the UEFI Memory Attributes Table when present (W^X);
//! without it, runtime code must stay writable+executable and we report that.

use crate::arch;
use crate::boot::BootInfo;
use crate::frames::{Frames, PAGE_SIZE};
use crate::paging::{Cache, MapError, Mapper, Perm};
use alloc::vec::Vec;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct Guid {
    d1: u32,
    d2: u16,
    d3: u16,
    d4: [u8; 8],
}

const GLOBAL_VARIABLE: Guid = Guid {
    d1: 0x8BE4_DF61,
    d2: 0x93CA,
    d3: 0x11D2,
    d4: [0xAA, 0x0D, 0x00, 0xE0, 0x98, 0x03, 0x2B, 0x8C],
};

const fn utf16<const N: usize>(s: &str) -> [u16; N] {
    let b = s.as_bytes();
    assert!(b.len() + 1 == N);
    let mut out = [0u16; N];
    let mut i = 0;
    while i < b.len() {
        out[i] = b[i] as u16;
        i += 1;
    }
    out
}

const OS_INDICATIONS: [u16; 14] = utf16("OsIndications");
const OS_INDICATIONS_SUPPORTED: [u16; 23] = utf16("OsIndicationsSupported");

/// `EFI_OS_INDICATIONS_BOOT_TO_FW_UI`
const OS_INDICATIONS_BOOT_TO_FW_UI: u64 = 1;
/// `EFI_VARIABLE_NON_VOLATILE | BOOTSERVICE_ACCESS | RUNTIME_ACCESS`
const VAR_ATTRS_NV_BS_RT: u32 = 0x7;

const SYSTEM_TABLE_SIGNATURE: u64 = 0x5453_5953_2049_4249; // "IBI SYST"
const RUNTIME_SERVICES_SIGNATURE: u64 = 0x5652_4553_544E_5552; // "RUNTSERV"

type Status = usize;
const EFI_SUCCESS: Status = 0;
const EFI_NOT_FOUND: Status = (1 << 63) | 14;

const RESET_COLD: u32 = 0;
const RESET_SHUTDOWN: u32 = 2;

use ksparse::efi::{
    self as efi_fmt, TYPE_MMIO, TYPE_MMIO_PORT_SPACE, TYPE_RUNTIME_CODE, TYPE_RUNTIME_DATA,
};

#[repr(C)]
struct TableHeader {
    signature: u64,
    revision: u32,
    header_size: u32,
    crc32: u32,
    reserved: u32,
}

#[repr(C)]
struct SystemTable {
    hdr: TableHeader,
    firmware_vendor: u64,
    firmware_revision: u32,
    console_in_handle: usize,
    con_in: usize,
    console_out_handle: usize,
    con_out: usize,
    std_err_handle: usize,
    std_err: usize,
    runtime_services: u64,
    boot_services: usize,
    number_of_table_entries: usize,
    configuration_table: u64,
}

#[repr(C)]
struct RuntimeServices {
    hdr: TableHeader,
    get_time: usize,
    set_time: usize,
    get_wakeup_time: usize,
    set_wakeup_time: usize,
    set_virtual_address_map: unsafe extern "efiapi" fn(usize, usize, u32, *mut u8) -> Status,
    convert_pointer: usize,
    get_variable:
        unsafe extern "efiapi" fn(*const u16, *const Guid, *mut u32, *mut usize, *mut u8) -> Status,
    get_next_variable_name: usize,
    set_variable:
        unsafe extern "efiapi" fn(*const u16, *const Guid, u32, usize, *const u8) -> Status,
    get_next_high_monotonic_count: usize,
    reset_system: unsafe extern "efiapi" fn(u32, Status, usize, *const u8),
}

/// How firmware code is being executed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Relocated into the kernel's EFI virtual window via `SetVirtualAddressMap`.
    Virtual,
    /// Firmware rejected relocation; called at physical addresses via an identity map.
    Physical,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum EfiError {
    Missing(&'static str),
    Invalid(&'static str),
    NotMapped(&'static str),
    NoWindow,
    OutOfWindow,
    Map(MapError),
    Firmware(&'static str, usize),
    Unsupported(&'static str),
}

impl From<MapError> for EfiError {
    fn from(e: MapError) -> Self {
        Self::Map(e)
    }
}

pub struct Efi {
    rt: *const RuntimeServices,
    pub mode: Mode,
    pub os_indications_supported: u64,
    pub strict_wx: bool,
}

/// Reads a `T` from firmware memory reachable through the HHDM.
fn read_fw<T>(boot: &BootInfo, phys: u64, what: &'static str) -> Result<&'static T, EfiError> {
    if !boot.hhdm_covers(phys, core::mem::size_of::<T>() as u64) {
        return Err(EfiError::NotMapped(what));
    }
    // SAFETY: the range is HHDM-mapped (checked) and firmware tables are immutable enough
    // for reading while only the BSP runs with interrupts off.
    Ok(unsafe { &*(boot.phys_to_virt(phys) as *const T) })
}

fn fw_bytes(
    boot: &BootInfo,
    phys: u64,
    len: u64,
    what: &'static str,
) -> Result<&'static [u8], EfiError> {
    if !boot.hhdm_covers(phys, len) {
        return Err(EfiError::NotMapped(what));
    }
    // SAFETY: HHDM-mapped range (checked above).
    Ok(unsafe { core::slice::from_raw_parts(boot.phys_to_virt(phys) as *const u8, len as usize) })
}

impl Efi {
    /// Brings up runtime services. Interrupts must be disabled.
    pub fn init(boot: &BootInfo, frames: &mut Frames) -> Result<Self, EfiError> {
        let st_virt = boot
            .efi_system_table
            .ok_or(EfiError::Missing("EFI system table"))?;
        let st_phys = st_virt
            .checked_sub(boot.hhdm)
            .ok_or(EfiError::Invalid("system table address"))?;
        let st: &SystemTable = read_fw(boot, st_phys, "system table")?;
        if st.hdr.signature != SYSTEM_TABLE_SIGNATURE {
            return Err(EfiError::Invalid("system table signature"));
        }
        let rt_phys = st.runtime_services;
        let rt_hdr: &RuntimeServices = read_fw(boot, rt_phys, "runtime services")?;
        if rt_hdr.hdr.signature != RUNTIME_SERVICES_SIGNATURE {
            return Err(EfiError::Invalid("runtime services signature"));
        }

        let (map_bytes, desc_size) = boot.efi_memmap.ok_or(EfiError::Missing("EFI memory map"))?;
        if !(efi_fmt::MIN_DESCRIPTOR_SIZE..=efi_fmt::MAX_DESCRIPTOR_SIZE).contains(&desc_size) {
            return Err(EfiError::Invalid("EFI descriptor size"));
        }
        // Runtime regions as `(raw descriptor, parsed)`; insane descriptors are ignored everywhere.
        let runtime = || {
            efi_fmt::descriptors(map_bytes, desc_size)
                .filter(|(_, d)| d.is_runtime() && d.is_sane())
        };
        let count = runtime().count();
        if count == 0 {
            return Err(EfiError::Missing("runtime memory regions"));
        }
        let rt_in_runtime = runtime().any(|(_, d)| rt_phys >= d.phys && rt_phys < d.end());
        if !rt_in_runtime {
            return Err(EfiError::Invalid("runtime table outside runtime regions"));
        }

        // Memory Attributes Table (optional): enables W^X for runtime images.
        let mat = find_mat(boot, st)?;

        // CPU prerequisites for running firmware code.
        let nx = crate::vmm::nx_enabled();
        arch::enable_sse();

        let mapper = Mapper::new(boot, nx);
        let cur_root = arch::read_cr3() & 0x000F_FFFF_FFFF_F000;
        let boot_root = mapper.new_root(frames)?;
        mapper.share_kernel_half(boot_root, cur_root);

        let window_index = (257..500)
            .rev()
            .find(|&i| mapper.root_entry_empty(boot_root, i))
            .ok_or(EfiError::NoWindow)?;
        let window_base = 0xFFFF_0000_0000_0000u64 | ((window_index as u64) << 39);
        let window_limit = 1u64 << 39;

        let code_perm = if mat.is_some() { Perm::RX } else { Perm::RWX };
        for (_, d) in runtime() {
            let perm = match d.kind {
                TYPE_RUNTIME_CODE => code_perm,
                TYPE_MMIO => Perm::MMIO,
                TYPE_MMIO_PORT_SPACE => continue,
                TYPE_RUNTIME_DATA => Perm::RW,
                _ => Perm::RW,
            };
            for i in 0..d.pages {
                let p = d.phys + i * PAGE_SIZE;
                if p >= window_limit {
                    return Err(EfiError::OutOfWindow);
                }
                mapper.map_page(frames, boot_root, window_base + p, p, perm)?;
                mapper.map_page(frames, boot_root, p, p, perm)?;
            }
        }

        // Tighten permissions per the Memory Attributes Table (W^X policy: see ksparse::efi).
        if let Some(sections) = &mat {
            for d in sections {
                if !d.is_runtime() && d.attr & (efi_fmt::ATTR_RO | efi_fmt::ATTR_XP) == 0 {
                    continue;
                }
                let (write, exec) = efi_fmt::section_permissions(d.kind, d.attr);
                let perm = Perm {
                    write,
                    exec,
                    cache: if d.kind == TYPE_MMIO {
                        Cache::Uncached
                    } else {
                        Cache::WriteBack
                    },
                };
                for i in 0..d.pages {
                    let p = d.phys + i * PAGE_SIZE;
                    if p >= window_limit {
                        continue;
                    }
                    mapper.map_page(frames, boot_root, window_base + p, p, perm)?;
                    mapper.map_page(frames, boot_root, p, p, perm)?;
                }
            }
        }

        // Compact virtual map handed to SetVirtualAddressMap (runtime regions only).
        let map_len = count * desc_size;
        let map_pages = map_len.div_ceil(PAGE_SIZE as usize);
        let map_phys = frames.alloc().ok_or(MapError::OutOfFrames)?;
        for n in 1..map_pages {
            let f = frames.alloc().ok_or(MapError::OutOfFrames)?;
            if f != map_phys + n as u64 * PAGE_SIZE {
                return Err(EfiError::Invalid("non-contiguous scratch frames"));
            }
        }
        for n in 0..map_pages as u64 {
            mapper.map_page(
                frames,
                boot_root,
                map_phys + n * PAGE_SIZE,
                map_phys + n * PAGE_SIZE,
                Perm::RW,
            )?;
        }
        // SAFETY: freshly allocated frames, HHDM-mapped, exclusively ours.
        let out = unsafe {
            core::slice::from_raw_parts_mut(boot.phys_to_virt(map_phys) as *mut u8, map_len)
        };
        let mut o = 0;
        for (raw, d) in runtime() {
            out[o..o + desc_size].copy_from_slice(raw);
            out[o + 16..o + 24].copy_from_slice(&(window_base + d.phys).to_le_bytes());
            o += desc_size;
        }

        // ---- Point of no return: relocate the firmware. ----
        arch::cli();
        // SAFETY: boot_root maps the kernel half identically to the active tables and
        // additionally identity-maps the firmware's runtime regions.
        unsafe { arch::write_cr3(boot_root) };
        let svam = rt_hdr.set_virtual_address_map; // physical address, identity-mapped now
                                                   // SAFETY: arguments describe a valid map of runtime descriptors with identity-mapped storage.
        let status = unsafe {
            svam(
                map_len,
                desc_size,
                map_bytes_version(boot),
                map_phys as *mut u8,
            )
        };

        let (rt, mode) = if status == EFI_SUCCESS {
            // The window joins the kernel address space permanently; the identity map is dropped.
            mapper.share_entry(cur_root, boot_root, window_index);
            // SAFETY: the kernel root maps the kernel plus the firmware window; firmware now
            // runs at window addresses.
            unsafe { arch::write_cr3(cur_root) };
            (
                (window_base + rt_phys) as *const RuntimeServices,
                Mode::Virtual,
            )
        } else {
            kprintln!("efi: SetVirtualAddressMap failed ({status:#x}); staying in physical mode");
            // The identity-mapped tables become the kernel's permanent root.
            crate::vmm::set_kernel_root(boot_root);
            (rt_phys as *const RuntimeServices, Mode::Physical)
        };

        let mut efi = Self {
            rt,
            mode,
            os_indications_supported: 0,
            strict_wx: mat.is_some(),
        };
        efi.os_indications_supported = efi.read_u64(&OS_INDICATIONS_SUPPORTED).unwrap_or(0);
        Ok(efi)
    }

    fn read_u64(&self, name: &[u16]) -> Result<u64, Status> {
        let mut value: u64 = 0;
        let mut size = 8usize;
        let mut attrs = 0u32;
        // SAFETY: runtime services are mapped per `init`; pointers are to live locals.
        // Interrupts stay off: firmware is not reentrant against our handlers.
        let s = arch::without_interrupts(|| unsafe {
            ((*self.rt).get_variable)(
                name.as_ptr(),
                &GLOBAL_VARIABLE,
                &mut attrs,
                &mut size,
                (&mut value as *mut u64).cast(),
            )
        });
        if s == EFI_SUCCESS {
            Ok(value)
        } else {
            Err(s)
        }
    }

    /// Whether the firmware advertises "reboot into firmware setup".
    pub fn firmware_setup_supported(&self) -> bool {
        self.os_indications_supported & OS_INDICATIONS_BOOT_TO_FW_UI != 0
    }

    /// Sets `OsIndications.BOOT_TO_FW_UI` and cold-resets. Returns only on failure.
    pub fn reboot_into_firmware_setup(&self) -> EfiError {
        if !self.firmware_setup_supported() {
            return EfiError::Unsupported("firmware does not advertise boot-to-setup");
        }
        let current = match self.read_u64(&OS_INDICATIONS) {
            Ok(v) => v,
            Err(EFI_NOT_FOUND) => 0,
            Err(s) => return EfiError::Firmware("GetVariable(OsIndications)", s),
        };
        let new = current | OS_INDICATIONS_BOOT_TO_FW_UI;
        // SAFETY: valid name/GUID/data pointers to live memory.
        let s = arch::without_interrupts(|| unsafe {
            ((*self.rt).set_variable)(
                OS_INDICATIONS.as_ptr(),
                &GLOBAL_VARIABLE,
                VAR_ATTRS_NV_BS_RT,
                8,
                (&new as *const u64).cast(),
            )
        });
        if s != EFI_SUCCESS {
            return EfiError::Firmware("SetVariable(OsIndications)", s);
        }
        self.reset(RESET_COLD);
        EfiError::Firmware("ResetSystem(cold) returned", 0)
    }

    /// Asks the firmware to power the machine off. Returns only on failure.
    pub fn shutdown(&self) {
        self.reset(RESET_SHUTDOWN);
    }

    fn reset(&self, kind: u32) {
        arch::cli();
        // SAFETY: ResetSystem takes no pointers we must keep alive; it should not return.
        unsafe { ((*self.rt).reset_system)(kind, EFI_SUCCESS, 0, core::ptr::null()) };
    }
}

/// Descriptor version to pass back to SetVirtualAddressMap (taken from the bootloader).
fn map_bytes_version(_boot: &BootInfo) -> u32 {
    crate::limine::EFI_MEMMAP
        .response()
        .map_or(1, |r| r.desc_version as u32)
}

/// Finds and parses the UEFI Memory Attributes Table through the configuration-table array.
fn find_mat(
    boot: &BootInfo,
    st: &SystemTable,
) -> Result<Option<Vec<efi_fmt::MemoryDescriptor>>, EfiError> {
    let len = efi_fmt::config_tables_len(st.number_of_table_entries)
        .ok_or(EfiError::Invalid("configuration table count"))?;
    let tables = fw_bytes(
        boot,
        st.configuration_table,
        len as u64,
        "configuration tables",
    )?;
    let Some(ptr) = efi_fmt::find_config_table(tables, &efi_fmt::MAT_GUID) else {
        return Ok(None);
    };
    let header = fw_bytes(
        boot,
        ptr,
        efi_fmt::MAT_HEADER_LEN as u64,
        "memory attributes table",
    )?;
    let Some(total) = efi_fmt::mat_total_len(header) else {
        kprintln!("efi: ignoring a malformed Memory Attributes Table");
        return Ok(None);
    };
    let bytes = fw_bytes(boot, ptr, total as u64, "memory attributes table entries")?;
    Ok(efi_fmt::parse_mat(bytes))
}
