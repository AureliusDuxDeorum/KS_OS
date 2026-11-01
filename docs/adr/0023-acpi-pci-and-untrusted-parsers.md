# ADR-0023: ACPI/PCI discovery, untrusted-data parsers and the AML decision

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Stage/Phase:** S2.P4 (resolves Gate G1 risks R9 and R7)

## Context
USB, Bluetooth and networking drivers all start from data produced by firmware or by devices
we do not control: ACPI tables, PCI configuration space, BAR registers, capability lists. A
parser bug here is a kernel vulnerability (ADR-0002 gate; risk R9). The kernel also needs
discovery of buses and interrupts (MSI/MSI-X) before any real driver can exist.

## Decision
**Parsers live in a separate `no_std`, `#![forbid(unsafe_code)]` crate, `ksparse`.** They are
pure functions over byte slices or a `Config` trait (32-bit config reads): RSDP/SDT headers and
checksums, root-table iteration, FADT, the `\_S5` scan, MADT, MCFG, PCI header, capability
list, BAR decoding, MSI and MSI-X capabilities. The kernel only maps physical memory and
passes bytes in. **Invariant: no input panics, reads out of bounds or loops unboundedly**:
capability lists are length-bounded (48) and cycle-detected, BAR decoding rejects reserved
encodings and non-power-of-two sizes, MSI-X table ranges are overflow-checked and validated
against the BAR size before use.

**Verification on every run:** unit tests for valid and invalid tables; 60 000 random
mutations of valid tables, every byte value at every offset, 20 000 pure-noise inputs,
adversarial all-0x00/0xFF/cyclic config spaces; and libFuzzer targets (`fuzz/`) driving the
same entry point for coverage-guided runs.

**PCI (`pci.rs`).** Configuration via ECAM from the MCFG table (each region mapped uncached),
falling back to port-I/O mechanism #1. Recursive bus enumeration through bridges with loop
protection; BARs are sized with decoding disabled and restored. API: `map_bar`,
`enable_bus_master`, and `enable_msi` which prefers MSI-X (table programmed with the entry
masked, then unmasked; INTx disabled) over MSI. IOMMU tables (DMAR/IVRS) are detected and
reported; **enforcement is deferred to a Gate G2 item** — until then DMA-capable devices are
trusted.

**Verification of the drivers' foundations** uses real device models in QEMU: the `edu` device
(MSI delivery, factorial interrupt, bidirectional DMA to/from a frame obtained from the PMM's
below-256 MiB DMA allocator), virtio-net (MSI-X table read-back), xHCI/e1000e/VGA enumeration
with expected class codes, BAR sizes and capabilities.

**AML decision (R7).** *No AML interpreter now.* Everything the next phases need is available
without one: MSI/MSI-X replaces PCI interrupt routing (`_PRT`), the PM timer and MADT/MCFG are
static tables, and `\_S5` is a constant. An interpreter (a vetted `no_std` crate or our own)
is introduced when a concrete need appears — battery/thermal/lid, hot-plug, power resources,
or hardware whose interrupts cannot be MSI. That later ADR must include fuzzing of the AML
loader, since AML is bytecode from firmware.

## Alternatives considered
- Keep parsers in the kernel crate: untestable on the host, no fuzzing, no `forbid(unsafe_code)`.
- Adopt the `acpi`/`aml` crates now: heavy, broad attack surface for features we do not use yet.

## Consequences
- Any new firmware/device-data parser goes into `ksparse` first, with tests and a fuzz entry.
- Legacy INTx-only devices are unsupported until the AML decision is revisited.
