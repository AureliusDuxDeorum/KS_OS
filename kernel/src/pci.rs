//! PCI / PCI Express: configuration access, bus enumeration, BARs, MSI and MSI-X.
//!
//! Configuration space is reached through ECAM (from the ACPI MCFG table) when the
//! firmware provides it, otherwise through legacy port I/O mechanism #1. All decoding of
//! device-supplied data is done by the fuzzed `ksparse::pci` module.

use crate::arch::{inl, outl};
use crate::boot::BootInfo;
use crate::sync::IrqMutex;
use crate::{acpi, msi, vmm};
use alloc::vec::Vec;
use ksparse::mcfg::{self, EcamRegion};
use ksparse::pci::{self as parse, Bar, BarKind, Capability, Config, Header, Msi, Msix};
use spin::Once;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Address {
    pub segment: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl core::fmt::Display for Address {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:02x}:{:02x}.{}", self.bus, self.device, self.function)
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum PciError {
    NoCapability,
    NoBar,
    BarUnassigned,
    NoVector,
    Map(vmm::VmmError),
    BadMsix,
    ApicIdTooLarge,
}

/// An enumerated function.
pub struct Device {
    pub addr: Address,
    pub header: Header,
    pub bars: [Option<Bar>; 6],
    pub caps: Vec<Capability>,
}

struct Backend {
    ecam: Vec<(EcamRegion, u64)>, // region + mapped virtual base of its first bus
    legacy: IrqMutex<()>,
}

static BACKEND: Once<Backend> = Once::new();
static DEVICES: Once<Vec<Device>> = Once::new();

fn backend() -> &'static Backend {
    BACKEND.get().expect("pci not initialised")
}

fn ecam_ptr(a: Address, offset: u16) -> Option<*mut u32> {
    backend().ecam.iter().find_map(|(r, virt)| {
        r.function_address(a.bus, a.device, a.function)
            .filter(|_| r.segment == a.segment)
            .map(|p| (*virt + (p - r.base) + u64::from(offset & 0xFFC)) as *mut u32)
    })
}

/// Reads a 32-bit configuration register.
pub fn read32(a: Address, offset: u16) -> u32 {
    if let Some(p) = ecam_ptr(a, offset) {
        // SAFETY: the ECAM region was mapped uncached by `init`; the offset is masked to 4 KiB.
        return unsafe { p.read_volatile() };
    }
    if a.segment != 0 || offset >= 256 {
        return u32::MAX;
    }
    let _g = backend().legacy.lock();
    let addr = 0x8000_0000
        | (u32::from(a.bus) << 16)
        | (u32::from(a.device) << 11)
        | (u32::from(a.function) << 8)
        | u32::from(offset & 0xFC);
    // SAFETY: PCI configuration mechanism #1 ports, serialised by the lock.
    unsafe {
        outl(0xCF8, addr);
        inl(0xCFC)
    }
}

/// Writes a 32-bit configuration register.
pub fn write32(a: Address, offset: u16, value: u32) {
    if let Some(p) = ecam_ptr(a, offset) {
        // SAFETY: as in `read32`.
        unsafe { p.write_volatile(value) };
        return;
    }
    if a.segment != 0 || offset >= 256 {
        return;
    }
    let _g = backend().legacy.lock();
    let addr = 0x8000_0000
        | (u32::from(a.bus) << 16)
        | (u32::from(a.device) << 11)
        | (u32::from(a.function) << 8)
        | u32::from(offset & 0xFC);
    // SAFETY: as in `read32`.
    unsafe {
        outl(0xCF8, addr);
        outl(0xCFC, value);
    }
}

struct Func(Address);
impl Config for Func {
    fn read32(&self, offset: u16) -> u32 {
        read32(self.0, offset)
    }
}

fn read16(a: Address, off: u16) -> u16 {
    (read32(a, off & !3) >> (8 * u32::from(off & 2))) as u16
}

fn write16(a: Address, off: u16, v: u16) {
    let shift = 8 * u32::from(off & 2);
    let old = read32(a, off & !3);
    write32(
        a,
        off & !3,
        (old & !(0xFFFF << shift)) | (u32::from(v) << shift),
    );
}

/// Sets bits in the command register.
pub fn command_set(a: Address, bits: u16) {
    write16(a, 4, read16(a, 4) | bits);
}

/// Allows the function to master the bus (required for DMA).
pub fn enable_bus_master(dev: &Device) {
    command_set(dev.addr, parse::CMD_BUS_MASTER | parse::CMD_MEMORY);
}

/// Probes the BAR sizes of a type-0 function with decoding disabled, then restores everything.
fn probe_bars(a: Address, h: &Header) -> [Option<Bar>; 6] {
    let mut bars = [None; 6];
    if h.kind != 0 {
        return bars;
    }
    let (mut orig, mut probed) = ([0u32; 6], [0u32; 6]);
    let cmd = read16(a, 4);
    write16(a, 4, cmd & !(parse::CMD_IO | parse::CMD_MEMORY)); // never let a probe value decode
    for i in 0..6 {
        let off = 0x10 + 4 * i as u16;
        orig[i] = read32(a, off);
        write32(a, off, u32::MAX);
        probed[i] = read32(a, off);
        write32(a, off, orig[i]);
    }
    write16(a, 4, cmd);
    let mut i = 0;
    while i < 6 {
        match parse::decode_bar(&orig, &probed, i) {
            Some((bar, used)) => {
                bars[i] = Some(bar);
                i += used;
            }
            None => i += 1,
        }
    }
    bars
}

fn scan_bus(seg: u16, bus: u8, out: &mut Vec<Device>, visited: &mut [bool; 256]) {
    if core::mem::replace(&mut visited[usize::from(bus)], true) {
        return; // a bridge loop: never rescan a bus
    }
    for device in 0..32u8 {
        let a0 = Address {
            segment: seg,
            bus,
            device,
            function: 0,
        };
        let Some(h0) = parse::header(&Func(a0)) else {
            continue;
        };
        let functions = if h0.multifunction { 8 } else { 1 };
        for function in 0..functions {
            let addr = Address {
                segment: seg,
                bus,
                device,
                function,
            };
            let Some(header) = parse::header(&Func(addr)) else {
                continue;
            };
            let caps = parse::capabilities(&Func(addr), &header);
            let bars = probe_bars(addr, &header);
            if header.kind == 1 && header.secondary_bus != 0 && header.secondary_bus != bus {
                scan_bus(seg, header.secondary_bus, out, visited);
            }
            out.push(Device {
                addr,
                header,
                bars,
                caps,
            });
        }
    }
}

/// Initialises configuration access and enumerates every bus.
pub fn init(boot: &BootInfo) {
    let mut ecam = Vec::new();
    if let Ok(mcfg_tbl) = acpi::find_table(boot, b"MCFG") {
        for r in mcfg::parse(mcfg_tbl).unwrap_or_default() {
            let size = (u64::from(r.end_bus) - u64::from(r.start_bus) + 1) << 20;
            match vmm::map_mmio(boot, r.base, size) {
                Ok(virt) => ecam.push((r, virt)),
                Err(e) => kprintln!("pci: cannot map ECAM {:#x}: {e:?}", r.base),
            }
        }
    }
    kprintln!(
        "pci: configuration via {}",
        if ecam.is_empty() {
            "legacy port I/O"
        } else {
            "ECAM (MCFG)"
        }
    );
    let segments: Vec<u16> = if ecam.is_empty() {
        alloc::vec![0]
    } else {
        ecam.iter().map(|(r, _)| r.segment).collect()
    };
    BACKEND.call_once(|| Backend {
        ecam,
        legacy: IrqMutex::new(()),
    });

    let mut devices = Vec::new();
    for seg in segments {
        let mut visited = [false; 256];
        scan_bus(seg, 0, &mut devices, &mut visited);
    }
    for d in &devices {
        for (i, bar) in d.bars.iter().enumerate() {
            if let Some(b) = bar {
                kprintln!(
                    "pci:   {} bar{i} {:?} base={:#x} size={:#x}{}",
                    d.addr,
                    b.kind,
                    b.base,
                    b.size,
                    if b.prefetchable { " prefetchable" } else { "" }
                );
            }
        }
        kprintln!(
            "pci: {} {:04x}:{:04x} class {:02x}.{:02x}.{:02x}{}",
            d.addr,
            d.header.vendor,
            d.header.device,
            d.header.class,
            d.header.subclass,
            d.header.prog_if,
            if d.caps.iter().any(|c| c.id == parse::CAP_MSIX) {
                " msi-x"
            } else if d.caps.iter().any(|c| c.id == parse::CAP_MSI) {
                " msi"
            } else {
                ""
            }
        );
    }
    DEVICES.call_once(|| devices);
}

/// All enumerated functions.
pub fn devices() -> &'static [Device] {
    DEVICES.get().map_or(&[], Vec::as_slice)
}

/// Finds the first function matching a vendor/device pair.
pub fn find(vendor: u16, device: u16) -> Option<&'static Device> {
    devices()
        .iter()
        .find(|d| d.header.vendor == vendor && d.header.device == device)
}

/// Maps BAR `index` of `dev` (memory BARs only) into the MMIO window and returns its virtual base.
pub fn map_bar(boot: &BootInfo, dev: &Device, index: usize) -> Result<u64, PciError> {
    let bar = dev
        .bars
        .get(index)
        .copied()
        .flatten()
        .ok_or(PciError::NoBar)?;
    if bar.kind == BarKind::Io || bar.base == 0 {
        return Err(PciError::BarUnassigned);
    }
    vmm::map_mmio(boot, bar.base, bar.size).map_err(PciError::Map)
}

/// Programs and enables MSI-X if available, otherwise MSI, delivering `vector` to `apic_id`.
/// Returns which mechanism was used.
pub fn enable_msi(
    boot: &BootInfo,
    dev: &Device,
    vector: u8,
    apic_id: u32,
) -> Result<&'static str, PciError> {
    let msg = msi::compose(apic_id, vector).ok_or(PciError::ApicIdTooLarge)?;
    let a = dev.addr;
    let f = Func(a);
    if let Some(cap) = dev.caps.iter().copied().find(|c| c.id == parse::CAP_MSIX) {
        let x: Msix = parse::msix(&f, cap).ok_or(PciError::NoCapability)?;
        let (table_off, table_len) = x.table_range().ok_or(PciError::BadMsix)?;
        let bar = dev
            .bars
            .get(usize::from(x.table_bar))
            .copied()
            .flatten()
            .ok_or(PciError::NoBar)?;
        if u64::from(table_off) + u64::from(table_len) > bar.size {
            return Err(PciError::BadMsix);
        }
        let base = map_bar(boot, dev, usize::from(x.table_bar))?;
        let entry = (base + u64::from(table_off)) as *mut u32;
        let ctl = u16::from(cap.offset) + 2;
        // Function-mask while programming, entry 0 masked until fully written.
        write16(a, ctl, read16(a, ctl) | (1 << 15) | (1 << 14));
        // SAFETY: the table lies inside the BAR (checked) which is mapped uncached.
        unsafe {
            entry.add(3).write_volatile(1); // vector control: masked
            entry.write_volatile(msg.address as u32);
            entry.add(1).write_volatile((msg.address >> 32) as u32);
            entry.add(2).write_volatile(msg.data);
            entry.add(3).write_volatile(0); // unmask
        }
        write16(a, ctl, (read16(a, ctl) | (1 << 15)) & !(1 << 14)); // enable, unmask function
        command_set(a, parse::CMD_INTX_DISABLE);
        return Ok("MSI-X");
    }
    if let Some(cap) = dev.caps.iter().copied().find(|c| c.id == parse::CAP_MSI) {
        let m: Msi = parse::msi(&f, cap).ok_or(PciError::NoCapability)?;
        let o = u16::from(m.offset);
        write32(a, o + 4, msg.address as u32);
        let data_off = if m.is_64bit {
            write32(a, o + 8, (msg.address >> 32) as u32);
            o + 12
        } else {
            o + 8
        };
        write16(a, data_off, msg.data as u16);
        if m.per_vector_mask {
            write32(a, data_off + 4, 0); // unmask vector 0
        }
        let ctl = read16(a, o + 2);
        write16(a, o + 2, (ctl & !(0b111 << 4)) | 1); // one vector (MME = 0), enable
        command_set(a, parse::CMD_INTX_DISABLE);
        return Ok("MSI");
    }
    Err(PciError::NoCapability)
}
