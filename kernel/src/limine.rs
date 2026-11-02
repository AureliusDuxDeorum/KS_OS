// SPDX-License-Identifier: Apache-2.0
//! Minimal Limine boot protocol bindings (base revision 5).
//!
//! Hand-written against `PROTOCOL.md` of the Limine boot protocol rather than
//! pulled from a crate, so the kernel builds on stable Rust and the exact
//! wire layout is reviewable in one place. See ADR-0006 / ADR-0007.
//!
//! Constants mirror the specification; not all are consumed yet.
#![allow(dead_code)]

use core::cell::UnsafeCell;
use core::ffi::c_char;

const COMMON_MAGIC: [u64; 2] = [0xc7b1dd30df4c8b88, 0x0a82e883a194f07b];

/// Base revision this kernel requires.
pub const BASE_REVISION: u64 = 5;

/// The base-revision tag. The bootloader zeroes word 2 when it supports the
/// requested revision and stores the revision it actually used in word 1.
#[repr(C)]
pub struct BaseRevision(UnsafeCell<[u64; 3]>);

// SAFETY: only accessed through volatile reads; written solely by the bootloader before entry.
unsafe impl Sync for BaseRevision {}

impl BaseRevision {
    pub const fn new(revision: u64) -> Self {
        Self(UnsafeCell::new([
            0xf9562b2d5c95a6c8,
            0x6a7b384944536bdc,
            revision,
        ]))
    }

    pub fn is_supported(&self) -> bool {
        // SAFETY: pointer is to our own static; volatile because the bootloader mutated it.
        unsafe { self.0.get().cast::<u64>().add(2).read_volatile() == 0 }
    }

    pub fn loaded_revision(&self) -> u64 {
        // SAFETY: as above.
        unsafe { self.0.get().cast::<u64>().add(1).read_volatile() }
    }
}

/// Request-block start marker (must precede all requests).
#[repr(C)]
pub struct StartMarker([u64; 4]);
/// Request-block end marker (must follow all requests).
#[repr(C)]
pub struct EndMarker([u64; 2]);

pub const START_MARKER: StartMarker = StartMarker([
    0xf6b8f4b39de7d1ae,
    0xfab91a6940fcb9cf,
    0x785c6ed015d3e316,
    0x181e920a7852b9d9,
]);
pub const END_MARKER: EndMarker = EndMarker([0xadc0e0531bb10d03, 0x9572709f31764c62]);

/// A request with a bootloader-filled response pointer. `X` carries
/// request-specific trailing fields.
#[repr(C)]
pub struct Request<R, X = ()> {
    id: [u64; 4],
    revision: u64,
    response: UnsafeCell<*const R>,
    extra: X,
}

// SAFETY: the response pointer is written once by the bootloader before the kernel runs.
unsafe impl<R, X> Sync for Request<R, X> {}

impl<R, X> Request<R, X> {
    pub const fn with_extra(id: [u64; 2], revision: u64, extra: X) -> Self {
        Self {
            id: [COMMON_MAGIC[0], COMMON_MAGIC[1], id[0], id[1]],
            revision,
            response: UnsafeCell::new(core::ptr::null()),
            extra,
        }
    }

    /// Returns the bootloader's response, if it provided one.
    pub fn response(&self) -> Option<&'static R> {
        // SAFETY: volatile read of the pointer the bootloader wrote; Limine
        // guarantees responses live in memory mapped for the kernel's lifetime.
        let p = unsafe { self.response.get().read_volatile() };
        // SAFETY: non-null response pointers are valid, aligned and immutable.
        unsafe { p.as_ref() }
    }
}

impl<R> Request<R, ()> {
    pub const fn new(id: [u64; 2]) -> Self {
        Self::with_extra(id, 0, ())
    }
}

pub const BOOTLOADER_INFO_ID: [u64; 2] = [0xf55038d8e2a1202f, 0x279426fcf5f59740];
pub const FIRMWARE_TYPE_ID: [u64; 2] = [0x8c2f75d90bef28a8, 0x7045a4688eac00c3];
pub const EXECUTABLE_CMDLINE_ID: [u64; 2] = [0x4b161536e598651e, 0xb390ad4a2f1f303a];
pub const EXECUTABLE_ADDRESS_ID: [u64; 2] = [0x71ba76863cc55f63, 0xb2644a48c516a487];
pub const MODULE_ID: [u64; 2] = [0x3e7e279702be32af, 0xca1c4f3bd1280cee];
pub const HHDM_ID: [u64; 2] = [0x48dcf1cb8ad2b852, 0x63984e959a98244b];
pub const FRAMEBUFFER_ID: [u64; 2] = [0x9d5827dcd881dd75, 0xa3148604f6fab11b];
pub const MEMMAP_ID: [u64; 2] = [0x67cf3d9d378a806f, 0xe304acdfc50c3c62];
pub const RSDP_ID: [u64; 2] = [0xc5e77b6b397e7b43, 0x27637845accdcf3c];
pub const EFI_SYSTEM_TABLE_ID: [u64; 2] = [0x5ceba5163eaaf6d6, 0x0a6981610cf65fcc];
pub const EFI_MEMMAP_ID: [u64; 2] = [0x7df62a431d6872d5, 0xa4fcdfb3e57306c8];

#[repr(C)]
pub struct BootloaderInfoResponse {
    pub revision: u64,
    pub name: *const c_char,
    pub version: *const c_char,
}

#[repr(C)]
pub struct FirmwareTypeResponse {
    pub revision: u64,
    pub firmware_type: u64,
}

pub const FIRMWARE_EFI64: u64 = 2;

#[repr(C)]
pub struct ExecutableCmdlineResponse {
    pub revision: u64,
    pub cmdline: *const c_char,
}

#[repr(C)]
pub struct ExecutableAddressResponse {
    pub revision: u64,
    pub physical_base: u64,
    pub virtual_base: u64,
}

/// Prefix of `struct limine_file` (the fields this kernel reads).
#[repr(C)]
pub struct File {
    pub revision: u64,
    pub address: *mut u8,
    pub size: u64,
    pub path: *const c_char,
    pub string: *const c_char,
}

#[repr(C)]
pub struct ModuleResponse {
    pub revision: u64,
    pub module_count: u64,
    pub modules: *const *const File,
}

#[repr(C)]
pub struct HhdmResponse {
    pub revision: u64,
    pub offset: u64,
}

#[repr(C)]
pub struct Framebuffer {
    pub address: *mut u8,
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
    pub bpp: u16,
    pub memory_model: u8,
    pub red_mask_size: u8,
    pub red_mask_shift: u8,
    pub green_mask_size: u8,
    pub green_mask_shift: u8,
    pub blue_mask_size: u8,
    pub blue_mask_shift: u8,
    pub unused: [u8; 7],
    pub edid_size: u64,
    pub edid: *const u8,
}

pub const FRAMEBUFFER_RGB: u8 = 1;

#[repr(C)]
pub struct FramebufferResponse {
    pub revision: u64,
    pub framebuffer_count: u64,
    pub framebuffers: *const *const Framebuffer,
}

pub const MEMMAP_USABLE: u64 = 0;
pub const MEMMAP_RESERVED: u64 = 1;
pub const MEMMAP_ACPI_RECLAIMABLE: u64 = 2;
pub const MEMMAP_ACPI_NVS: u64 = 3;
pub const MEMMAP_BAD_MEMORY: u64 = 4;
pub const MEMMAP_BOOTLOADER_RECLAIMABLE: u64 = 5;
pub const MEMMAP_EXECUTABLE_AND_MODULES: u64 = 6;
pub const MEMMAP_FRAMEBUFFER: u64 = 7;
pub const MEMMAP_RESERVED_MAPPED: u64 = 8;

#[repr(C)]
pub struct MemmapEntry {
    pub base: u64,
    pub length: u64,
    pub kind: u64,
}

#[repr(C)]
pub struct MemmapResponse {
    pub revision: u64,
    pub entry_count: u64,
    pub entries: *const *const MemmapEntry,
}

/// Response for the RSDP request (virtual/HHDM address from base revision 5 on).
#[repr(C)]
pub struct RsdpResponse {
    pub revision: u64,
    pub address: u64,
}

/// Response for the EFI system-table request (virtual/HHDM address from base revision 5 on).
#[repr(C)]
pub struct EfiSystemTableResponse {
    pub revision: u64,
    pub address: u64,
}

#[repr(C)]
pub struct EfiMemmapResponse {
    pub revision: u64,
    pub memmap: *const u8,
    pub memmap_size: u64,
    pub desc_size: u64,
    pub desc_version: u64,
}

// ---- The request block -------------------------------------------------
//
// Order is fixed by the linker script: start marker, requests, end marker.

#[used]
#[link_section = ".limine_requests_start"]
static REQUESTS_START: StartMarker = START_MARKER;

#[used]
#[link_section = ".limine_requests"]
pub static BASE: BaseRevision = BaseRevision::new(BASE_REVISION);

#[used]
#[link_section = ".limine_requests"]
pub static EXECUTABLE_CMDLINE: Request<ExecutableCmdlineResponse> =
    Request::new(EXECUTABLE_CMDLINE_ID);

#[used]
#[link_section = ".limine_requests"]
pub static BOOTLOADER_INFO: Request<BootloaderInfoResponse> = Request::new(BOOTLOADER_INFO_ID);

#[used]
#[link_section = ".limine_requests"]
pub static FIRMWARE_TYPE: Request<FirmwareTypeResponse> = Request::new(FIRMWARE_TYPE_ID);

#[used]
#[link_section = ".limine_requests"]
pub static EXECUTABLE_ADDRESS: Request<ExecutableAddressResponse> =
    Request::new(EXECUTABLE_ADDRESS_ID);

// Request revision 0 has no internal-module fields; modules come from limine.conf.
#[used]
#[link_section = ".limine_requests"]
pub static MODULES: Request<ModuleResponse> = Request::new(MODULE_ID);

#[used]
#[link_section = ".limine_requests"]
pub static HHDM: Request<HhdmResponse> = Request::new(HHDM_ID);

#[used]
#[link_section = ".limine_requests"]
pub static FRAMEBUFFER: Request<FramebufferResponse> = Request::new(FRAMEBUFFER_ID);

#[used]
#[link_section = ".limine_requests"]
pub static MEMMAP: Request<MemmapResponse> = Request::new(MEMMAP_ID);

#[used]
#[link_section = ".limine_requests"]
pub static RSDP: Request<RsdpResponse> = Request::new(RSDP_ID);

#[used]
#[link_section = ".limine_requests"]
pub static EFI_SYSTEM_TABLE: Request<EfiSystemTableResponse> = Request::new(EFI_SYSTEM_TABLE_ID);

#[used]
#[link_section = ".limine_requests"]
pub static EFI_MEMMAP: Request<EfiMemmapResponse> = Request::new(EFI_MEMMAP_ID);

#[used]
#[link_section = ".limine_requests_end"]
static REQUESTS_END: EndMarker = END_MARKER;
