# ADR-0020: Physical memory manager, kernel-owned address space and heap

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Stage/Phase:** S2.P2

## Context
Drivers for xHCI, Bluetooth and the NIC need DMA-capable memory, device register
mappings, dynamic allocation and a trustworthy address-space layout. Until now the kernel
ran on the bootloader's page tables with a bump allocator.

## Decision
**Physical memory (`pmm.rs`).** A bitmap allocator, one bit per 4 KiB frame. Only `USABLE`
memory above 1 MiB is ever handed out; bootloader-reclaimable memory is *not* reclaimed yet
(we still hold pointers into it). Supports zeroed single frames and physically contiguous,
aligned runs below an address limit (the DMA allocator, e.g. below 4 GiB for 32-bit devices).
Double frees and frees of unmanaged frames panic. The bitmap itself is placed in the first
usable region that fits it.

**Kernel address space (`vmm.rs`, `paging.rs`).** The kernel builds and owns its page tables:
- kernel image: `.text` R-X, `.rodata` R--, `.data/.bss` RW- NX, exact 4 KiB mappings;
- HHDM of RAM-like regions RW- NX with 2 MiB pages where aligned, **excluding the kernel
  image's own physical pages** (no writable alias of kernel code);
- framebuffer RW- write-combining via a programmed PAT (entries WB/WT/UC-/UC/WB/WC/UC-/UC);
- stack guard pages simply never mapped;
- an MMIO window (PML4 slot 400) for uncached device registers, with its top-level slot
  pre-populated so every address space sharing the kernel half sees later mappings;
- a boot-time **W^X audit** walks every leaf and the kernel refuses to continue on a violation.

The EFI runtime window (ADR-0009) now joins this root permanently instead of creating a
parallel one; if firmware refuses relocation, its identity-mapped tables become the root.

**Heap (`heap.rs`).** `#[global_allocator]`: power-of-two size classes 16 B–2 KiB carved from
frames (naturally aligned blocks), larger requests as contiguous frame runs (honouring
alignment). All memory is reached through the HHDM, so no virtual heap region exists to
manage. Debug builds poison freed memory. The `alloc` crate is available kernel-wide.

**Verification.** `selftest=` cases: `pmm` (distinctness, accounting, 2 MiB-aligned DMA run
below 4 GiB, reuse), `pmm-double-free` (panics), `heap` (Box/Vec/String/BTreeMap, every size
class alignment and non-overlap, 3 MiB 64 KiB-aligned block, frame return), `mmio-map`
(alias semantics, uncached NX), `vmm-layout` (section permissions, no HHDM alias of kernel
text, W^X audit, guard page unmapped), and violations that must fault: `write-text`,
`exec-data`.

## Alternatives considered
- Buddy allocator: better worst-case contiguous allocation, more code; the bitmap's linear
  scan is adequate for the frame counts involved and trivially auditable. Revisit if
  fragmentation of large DMA runs becomes real.
- Separate virtual heap region: needless page-table work while the HHDM already maps RAM.
- Reclaiming bootloader memory now: unsafe until nothing references it (planned after the
  bootloader data is copied out).

## Consequences
- Slab frames are never returned to the PMM; large allocations are. Acceptable for a kernel
  that mostly allocates long-lived driver state; a slab reaper is a later optimisation.
- Allocation locks are not IRQ-safe yet; **no allocation from interrupt handlers** until the
  interrupt phase makes them so.
- The kernel half of the address space is shared by reference between roots; any new
  top-level kernel slot must be pre-populated before roots are duplicated.
