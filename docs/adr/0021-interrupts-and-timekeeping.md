# ADR-0021: Interrupt architecture and timekeeping

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Stage/Phase:** S2.P3

## Context
USB, Bluetooth and the NIC are interrupt-driven (MSI/MSI-X) and need timeouts. Stage 1 polled
PS/2 and ran with interrupts off. Limine base revision 5 leaves the APIC in a defined state with
NMI pins masked, so NMIs never arrived.

## Decision
**Controllers.** Boot CPU local APIC in **xAPIC mode by default** (uncached MMIO mapping), which is
the path verified under emulation. **x2APIC** (MSR access, 32-bit IDs) is used only if the firmware
already left it enabled (it cannot be switched back without disabling the APIC) or when requested
with `apic=x2` on the kernel command line. QEMU's software emulation has no x2APIC, so that path
is exercised only by a KVM-gated test and is **unverified in CI** until run on hardware or KVM. Legacy 8259 PICs are remapped to 0x20–0x2F and
fully masked. I/O APICs are discovered from the **MADT**, all inputs start masked, and ISA IRQs
are resolved through the MADT's source overrides (polarity/trigger). The MADT parser
(`madt.rs`) is a pure, bounds-checked function over bytes, with in-kernel hostile-input tests,
and moves unchanged into the fuzzed host crate in S2.P4 (gate R9).

**NMI.** The LVT NMI pin is programmed from the MADT (default LINT1) so **real NMIs are
delivered** to the IST-backed NMI handler.

**Vectors and dispatch.** All 256 vectors have assembly stubs. Vectors 0–31 are exceptions;
0x20–0x2F stray legacy IRQs (logged, rate-limited); 0x30–0xDF dynamically allocated
(`irq::alloc_vector`) for IOAPIC lines and MSI; fixed: timer 0xEC, APIC error 0xFE,
spurious 0xFF (never acknowledged). Handlers are `fn(&mut TrapFrame)` in a lock-free table;
the dispatcher acknowledges after the handler runs.

**Interrupt-safe locking.** `IrqMutex` disables interrupts while held and restores the previous
state. It protects the PMM, heap, serial port and fatal-screen framebuffer, so **interrupt
handlers may allocate and log**. Firmware (EFI runtime) calls always run with interrupts off.

**Time.** The ACPI PM timer (3.579545 MHz) is the calibration reference; the TSC and the local
APIC timer are each measured against it (median of three 40 ms windows). `now_ns()` is a
monotonic TSC clock; the LAPIC timer provides a 1 kHz periodic tick; `sleep_ms` halts between
interrupts (`sti; hlt`, no lost-wakeup window).

**MSI.** `msi::compose` builds the x86 MSI address/data pair (destination APIC ID in address
bits 19:12, vector in data), rejecting IDs that need interrupt remapping and exception vectors.
PCI drivers use it from S2.P4 onward.

**Input.** The i8042 keyboard (IRQ 1) and mouse (IRQ 12) are routed through the I/O APIC;
the handler only moves bytes into a lock-free ring, decoding happens in the consumer. Polling
remains as a fallback if routing fails.

## Alternatives considered
- HPET/PIT for calibration: the PM timer is mandatory with ACPI and trivially readable.
- TSC-deadline timer mode: faster, but the periodic LAPIC timer is universal; revisit with the scheduler.
- Per-vector registration with closures: needs allocation in the dispatch path.

## Consequences
- The EFI runtime window now shares the kernel root; the boot-time W^X audit is repeated after
  firmware setup. This found that treating Memory Attributes Table entries without RO/XP flags
  literally produced 216 writable+executable pages; such entries now fall back to their region
  type's default, so the kernel address space again has zero W^X violations.
- Single-CPU only: APs are not started yet (SMP is a later stage). Handlers target the BSP.
- On hardware where the TSC is not invariant (`invariant=false` in the log), clock drift under
  power management is possible; the log states it, and the scheduler phase must decide.
- The UI idles with `hlt`, so the machine no longer spins at 100 % while waiting for input.

## Verification
Self-tests: self-IPI delivery and EOI, allocation churn interleaved with interrupts that also
allocate (deadlock-freedom), timer accuracy and monotonicity, MSI composition, MADT parser against
crafted and truncated tables; end-to-end: a real NMI injected by QEMU, input via interrupts, and
the same IRQ test under x2APIC when `/dev/kvm` is available (skipped otherwise).
