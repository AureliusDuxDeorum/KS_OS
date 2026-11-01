# ADR-0014: CPU exception handling, IST stacks and guard-paged kernel stacks

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Stage/Phase:** S2.P1 (resolves Gate G1 risks R2 and R3)

## Context
Until now any CPU fault triple-faulted and silently reset the machine, and the kernel ran
on the bootloader's stack with no overflow protection. Drivers for USB, Bluetooth and
networking will fault in ways that must be diagnosable on hardware we can only test remotely.

## Decision
- **Own GDT/TSS** (`gdt.rs`): flat 64-bit kernel segments and a TSS with three IST slots —
  double fault (IST1), NMI (IST2), machine check (IST3).
- **IDT** (`idt.rs`): assembly stubs normalise vectors 0–31 and the spurious vector into a
  `TrapFrame` and call one Rust dispatcher. `#BP` and NMI are logged and resumed; everything
  else is **fatal** and prints a full register dump, decoded cause (including page-fault
  flags and CR2) on the serial console and a monochrome fatal screen on the display, then halts.
- **Kernel-owned stacks** (`stacks.rs`): the kernel entry (`_start`, a naked function) switches
  to a 256 KiB static stack before any Rust runs. Each stack (kernel + three IST stacks) has a
  4 KiB **guard page** that is unmapped from the active page tables at boot. An overflow
  raises `#PF` on the guard page; because that fault cannot be pushed onto the exhausted
  stack it escalates to `#DF` on its IST stack, whose handler recognises the guard hit via
  CR2 and reports "stack overflow".
- **Self-tests** (`selftest.rs`): a kernel command line of `selftest=<name>` triggers each
  fault class deliberately; `cargo xtask test` asserts the expected diagnostics. Production
  images have no command line.
- Interrupts remain disabled (IF=0) until the APIC phase.

## Alternatives considered
- Leave the bootloader stack: no overflow detection, size not under our control.
- Make `#PF` use an IST stack too: would avoid the escalation but hides the real cause in
  the general case where the guard-page hit is the only evidence; the DF path is the
  standard, well-understood mechanism and is now tested.

## Consequences
- Guard pages need 4 KiB mappings: `unmap_active` refuses to split huge pages, which would
  surface as a boot-time message (not hit on Limine's 4 KiB kernel mapping).
- Real NMI delivery is masked at the LAPIC by Limine base revision 5; the NMI path is tested
  with a software `int 2` now and with a real NMI in the APIC phase (S2.P3).
- The fatal path is intentionally simple (no allocation, no locks beyond `try_lock`) so it
  works even when the rest of the kernel state is corrupt.
