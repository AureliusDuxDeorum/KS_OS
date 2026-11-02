// SPDX-License-Identifier: Apache-2.0
//! IOMMU (DMA remapping): Intel VT-d and AMD-Vi behind one interface (ADR-0030 S-14).
//!
//! Policy: **deny by default**. With translation on, a PCI function can only reach memory that a driver
//! explicitly mapped for it with [`map_identity`] (IOVA = physical address, so descriptors keep carrying
//! physical addresses). Firmware-declared reserved regions (RMRR / IVMD) stay mapped for the devices that need
//! them. Faults are logged and counted. Interrupt remapping is not implemented yet (ADR-0030 S-14 follow-up).
//!
//! The table logic lives in `ksiommu` and the ACPI parsing in `ksparse::iommu`, both host-tested and fuzzed.

pub mod amdvi;
pub mod vtd;

use crate::boot::BootInfo;
use crate::pci;
use crate::pmm;
use crate::sync::IrqMutex;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use ksiommu::{intel, Domain, Format, TableMem};
use ksparse::iommu::{self as tables, Scope};

/// Page-table pages come from the physical allocator and are reached through the HHDM.
pub struct FrameTables {
    hhdm: u64,
    /// The IOMMU does not snoop CPU caches: flush every table write.
    flush: bool,
}

impl FrameTables {
    fn ptr(&self, table: u64, index: usize) -> *mut u64 {
        (self
            .hhdm
            .saturating_add(table)
            .saturating_add((index as u64).saturating_mul(8))) as *mut u64
    }
}

impl TableMem for FrameTables {
    fn alloc_table(&mut self) -> Option<u64> {
        pmm::alloc_zeroed()
    }
    fn free_table(&mut self, phys: u64) {
        pmm::free(phys);
    }
    fn read(&self, table: u64, index: usize) -> u64 {
        assert!(index < 512);
        // SAFETY: `table` is a frame we allocated; the HHDM maps all RAM; the index is checked.
        unsafe { self.ptr(table, index).read_volatile() }
    }
    fn write(&mut self, table: u64, index: usize, value: u64) {
        assert!(index < 512);
        let p = self.ptr(table, index);
        // SAFETY: as in `read`.
        unsafe { p.write_volatile(value) };
        if self.flush {
            // SAFETY: flushing the cache line of a valid address has no other effect.
            unsafe { core::arch::x86_64::_mm_clflush(p.cast::<u8>()) };
        }
    }
}

enum Hw {
    Vtd {
        units: Vec<vtd::Unit>,
        tables: intel::Tables,
    },
    Amd {
        units: Vec<amdvi::AmdVi>,
    },
}

struct Dev {
    domain: Domain,
    did: u16,
    pages: u64,
}

struct State {
    mem: FrameTables,
    hw: Hw,
    levels: u8,
    devs: BTreeMap<u16, Dev>,
    next_did: u16,
    name: &'static str,
}

static STATE: IrqMutex<Option<State>> = IrqMutex::new(None);
static FAULTS: AtomicU32 = AtomicU32::new(0);

pub fn bdf(a: pci::Address) -> u16 {
    (u16::from(a.bus) << 8) | (u16::from(a.device & 0x1F) << 3) | u16::from(a.function & 7)
}

/// Resolves a DMAR device scope into the concrete functions it names (bridges expand to everything behind them).
fn expand_scope(s: &Scope) -> Vec<(u8, u8)> {
    let mut bus = s.start_bus;
    let n = s.path.len();
    for (i, &(d, f)) in s.path.iter().enumerate() {
        let devfn = ((d & 0x1F) << 3) | (f & 7);
        if i + 1 == n {
            if s.kind == 2 {
                // PCI sub-hierarchy: the bridge and every function on the buses behind it.
                let a = pci::Address {
                    segment: 0,
                    bus,
                    device: d & 0x1F,
                    function: f & 7,
                };
                let sec = ((pci::read32(a, 0x18) >> 8) & 0xFF) as u8;
                let sub = ((pci::read32(a, 0x18) >> 16) & 0xFF) as u8;
                let mut v = alloc::vec![(bus, devfn)];
                v.extend(
                    pci::devices()
                        .iter()
                        .filter(|dev| (sec..=sub).contains(&dev.addr.bus))
                        .map(|dev| (dev.addr.bus, (dev.addr.device << 3) | dev.addr.function)),
                );
                return v;
            }
            return alloc::vec![(bus, devfn)];
        }
        let a = pci::Address {
            segment: 0,
            bus,
            device: d & 0x1F,
            function: f & 7,
        };
        bus = ((pci::read32(a, 0x18) >> 8) & 0xFF) as u8; // secondary bus of the bridge
    }
    Vec::new()
}

impl State {
    fn ensure(&mut self, device_id: u16) -> Result<(), String> {
        if !self.devs.contains_key(&device_id) {
            let format = match self.hw {
                Hw::Vtd { .. } => Format::IntelSecondLevel,
                Hw::Amd { .. } => Format::AmdV1,
            };
            let domain = Domain::new(&mut self.mem, format, self.levels)
                .map_err(|e| alloc::format!("domain: {e:?}"))?;
            let did = self.next_did;
            self.next_did = self.next_did.checked_add(1).ok_or("out of domain ids")?;
            self.attach_hw(device_id, &domain, did)?;
            self.devs.insert(
                device_id,
                Dev {
                    domain,
                    did,
                    pages: 0,
                },
            );
        }
        Ok(())
    }

    fn attach_hw(&mut self, device_id: u16, domain: &Domain, did: u16) -> Result<(), String> {
        match &mut self.hw {
            Hw::Vtd { units, tables } => {
                tables
                    .attach(
                        &mut self.mem,
                        (device_id >> 8) as u8,
                        device_id as u8,
                        domain,
                        did,
                    )
                    .map_err(|e| alloc::format!("context entry: {e:?}"))?;
                for u in units.iter() {
                    u.flush();
                }
                Ok(())
            }
            Hw::Amd { units } => {
                for u in units.iter_mut().filter(|u| u.covers(device_id)) {
                    u.set_dte(device_id, Some((domain.root, domain.levels, did)))?;
                }
                Ok(())
            }
        }
    }

    fn flush_hw(&mut self, device_id: u16, did: u16) {
        match &mut self.hw {
            Hw::Vtd { units, .. } => units.iter().for_each(vtd::Unit::flush),
            Hw::Amd { units } => {
                for u in units.iter_mut().filter(|u| u.covers(device_id)) {
                    let root = self
                        .devs
                        .get(&device_id)
                        .map(|d| (d.domain.root, d.domain.levels));
                    if let Some((r, l)) = root {
                        let _ = u.set_dte(device_id, Some((r, l, did)));
                    }
                }
            }
        }
    }

    fn map(&mut self, device_id: u16, phys: u64, len: u64) -> Result<(), String> {
        let start = phys & !0xFFF;
        let end = phys
            .checked_add(len)
            .and_then(|e| e.checked_add(0xFFF))
            .ok_or("range overflow")?
            & !0xFFF;
        self.ensure(device_id)?;
        let State { mem, devs, .. } = self;
        let dev = devs.get_mut(&device_id).ok_or("device state vanished")?;
        let Dev { domain, did, pages } = dev;
        let did = *did;
        // Pages that are already mapped for this device stay as they are (drivers may share a page).
        let mut p = start;
        while p < end {
            if domain.translate(&*mem, p).is_none() {
                domain
                    .map(mem, p, p, 4096, true)
                    .map_err(|e| alloc::format!("map {p:#x}: {e:?}"))?;
                *pages = pages.saturating_add(1);
            }
            p = p.saturating_add(4096);
        }
        self.flush_hw(device_id, did);
        Ok(())
    }
}

/// Probes ACPI for an IOMMU and, if one is found, turns DMA remapping on with everything denied.
pub fn init(boot: &BootInfo, enabled: bool) {
    if !enabled {
        kprintln!("iommu: disabled on the kernel command line (iommu=off); DMA is unrestricted");
        return;
    }
    let res = if let Ok(t) = crate::acpi::find_table(boot, b"DMAR") {
        init_vtd(boot, t)
    } else if let Ok(t) = crate::acpi::find_table(boot, b"IVRS") {
        init_amd(boot, t)
    } else {
        kprintln!(
            "iommu: no DMAR/IVRS table: no IOMMU, DMA is unrestricted (trusted devices only)"
        );
        return;
    };
    if let Err(e) = res {
        kprintln!("iommu: NOT enabled: {e}; DMA is unrestricted");
    }
}

fn init_vtd(boot: &BootInfo, table: &[u8]) -> Result<(), String> {
    let dmar = tables::parse_dmar(table).map_err(|e| alloc::format!("DMAR: {e:?}"))?;
    if dmar.units.is_empty() {
        return Err("DMAR lists no remapping units".into());
    }
    let mut units = Vec::new();
    for d in &dmar.units {
        units.push(vtd::Unit::new(boot, d)?);
    }
    let levels = units
        .iter()
        .map(|u| u.levels())
        .collect::<Option<Vec<u8>>>()
        .ok_or("a remapping unit supports neither 39- nor 48-bit addressing")?
        .into_iter()
        .min()
        .unwrap_or(3);
    let coherent = units.iter().all(vtd::Unit::coherent);
    let mut mem = FrameTables {
        hhdm: boot.hhdm,
        flush: !coherent,
    };
    let tables = intel::Tables::new(&mut mem).map_err(|e| alloc::format!("root table: {e:?}"))?;
    for u in &units {
        let (maj, min) = u.version();
        kprintln!(
            "iommu: VT-d {maj}.{min} unit at {:#x}, {levels}-level tables, {}coherent, interrupt remapping {}",
            u.register_base,
            if u.coherent() { "" } else { "non-" },
            if u.interrupt_remapping() { "available (not used yet)" } else { "unavailable" }
        );
    }
    let mut st = State {
        mem,
        hw: Hw::Vtd { units, tables },
        levels,
        devs: BTreeMap::new(),
        next_did: 1,
        name: "Intel VT-d",
    };
    // Reserved regions that devices keep using (legacy USB, integrated graphics) stay mapped.
    for r in &dmar.rmrr {
        let (base, limit) = (r.base & !0xFFF, r.limit);
        if limit < base {
            continue;
        }
        for s in &r.scopes {
            for (bus, devfn) in expand_scope(s) {
                let id = (u16::from(bus) << 8) | u16::from(devfn);
                st.map(id, base, limit - base + 1)?;
                kprintln!("iommu: reserved region {base:#x}-{limit:#x} stays mapped for {bus:02x}:{:02x}.{}", devfn >> 3, devfn & 7);
            }
        }
    }
    if let Hw::Vtd { units, tables } = &st.hw {
        for u in units {
            u.enable(tables.root)?;
        }
    }
    kprintln!("iommu: Intel VT-d DMA remapping ENABLED; every device is blocked until a driver maps memory for it");
    *STATE.lock() = Some(st);
    Ok(())
}

fn init_amd(boot: &BootInfo, table: &[u8]) -> Result<(), String> {
    let ivrs = tables::parse_ivrs(table).map_err(|e| alloc::format!("IVRS: {e:?}"))?;
    if ivrs.units.is_empty() {
        return Err("IVRS lists no IOMMUs".into());
    }
    let mut units = Vec::new();
    for h in &ivrs.units {
        let mut u = amdvi::AmdVi::new(boot, h)?;
        u.enable()?;
        kprintln!("iommu: AMD-Vi unit at {:#x} enabled", u.register_base);
        units.push(u);
    }
    let mut st = State {
        mem: FrameTables {
            hhdm: boot.hhdm,
            flush: false,
        },
        hw: Hw::Amd { units },
        levels: 4,
        devs: BTreeMap::new(),
        next_did: 1,
        name: "AMD-Vi",
    };
    // IVMD unity maps: identity-map the declared region for the existing functions in the declared range.
    for m in &ivrs.unity {
        for d in pci::devices() {
            let id = bdf(d.addr);
            if (m.device_start..=m.device_end).contains(&id) && m.length != 0 {
                st.map(id, m.start, m.length)?;
                kprintln!(
                    "iommu: unity map {:#x}+{:#x} for {} stays mapped",
                    m.start,
                    m.length,
                    d.addr
                );
            }
        }
    }
    kprintln!("iommu: AMD-Vi DMA remapping ENABLED; every device is blocked until a driver maps memory for it");
    *STATE.lock() = Some(st);
    Ok(())
}

/// Lets `dev` DMA to `[phys, phys + len)`. Returns `true` when the range is reachable afterwards
/// (always `true` if no IOMMU is active).
pub fn map_identity(dev: pci::Address, phys: u64, len: u64) -> bool {
    let mut g = STATE.lock();
    let Some(st) = g.as_mut() else { return true };
    match st.map(bdf(dev), phys, len) {
        Ok(()) => true,
        Err(e) => {
            kprintln!("iommu: cannot map {phys:#x}+{len:#x} for {dev}: {e}");
            false
        }
    }
}

/// Removes the mapping again (used when a driver frees a buffer the device must no longer touch).
pub fn unmap(dev: pci::Address, phys: u64, len: u64) {
    let mut g = STATE.lock();
    let Some(st) = g.as_mut() else { return };
    let id = bdf(dev);
    let mut did = 0;
    if let Some(d) = st.devs.get_mut(&id) {
        did = d.did;
        let start = phys & !0xFFF;
        let end = phys.saturating_add(len).saturating_add(0xFFF) & !0xFFF;
        d.domain.unmap(&mut st.mem, start, end - start);
        d.pages = d.pages.saturating_sub((end - start) / 4096);
    }
    st.flush_hw(id, did);
}

/// Name and mapping statistics of the active IOMMU, if any.
pub fn status() -> Option<(&'static str, usize, u64)> {
    let g = STATE.lock();
    g.as_ref()
        .map(|s| (s.name, s.devs.len(), s.devs.values().map(|d| d.pages).sum()))
}

pub fn fault_count() -> u32 {
    FAULTS.load(Ordering::Relaxed)
}

/// Drains and logs DMA faults. Call from the idle loop.
pub fn service() {
    let Some(mut g) = STATE.try_lock() else {
        return;
    };
    let Some(st) = g.as_mut() else { return };
    let mut n = 0u32;
    match &mut st.hw {
        Hw::Vtd { units, .. } => {
            let mut faults = Vec::new();
            for u in units.iter() {
                u.drain_faults(&mut faults);
            }
            for f in &faults {
                n += 1;
                if FAULTS.load(Ordering::Relaxed) + n <= 16 {
                    kprintln!(
                        "iommu: BLOCKED DMA {} from {:02x}:{:02x}.{} at {:#x} (reason {})",
                        if f.write { "write" } else { "read" },
                        f.requester >> 8,
                        (f.requester >> 3) & 0x1F,
                        f.requester & 7,
                        f.address,
                        f.reason
                    );
                }
            }
        }
        Hw::Amd { units } => {
            let mut faults = Vec::new();
            for u in units.iter_mut() {
                u.drain_faults(&mut faults);
            }
            for f in &faults {
                n += 1;
                if FAULTS.load(Ordering::Relaxed) + n <= 16 {
                    kprintln!(
                        "iommu: BLOCKED DMA (event {:#x}) from {:02x}:{:02x}.{} at {:#x}",
                        f.code,
                        f.requester >> 8,
                        (f.requester >> 3) & 0x1F,
                        f.requester & 7,
                        f.address
                    );
                }
            }
        }
    }
    FAULTS.fetch_add(n, Ordering::Relaxed);
}
