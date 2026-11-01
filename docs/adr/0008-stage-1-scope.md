# ADR-0008: Stage 1 scope — boot, announce, power off / boot to UEFI setup

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Stage/Phase:** S1

## Context
The project owner defined Stage 1 as: boot the kernel, print "OS operating", and
provide a **power off** button and a **boot into UEFI** button. This replaces the
broader bring-up outline originally proposed in `ROADMAP.md`.

## Decision
Stage 1 delivers, on x86_64/UEFI under Limine:
1. A kernel that boots and prints `OS operating` to the serial console and to the
   framebuffer (large banner).
2. Two on-screen buttons — **Power off** and **Boot into UEFI** — operable by
   keyboard (Tab/arrows/Enter/Space, accelerators `P`/`U`) and PS/2 mouse (ADR-0011).
3. **Power off** via ACPI S5, with UEFI `ResetSystem(Shutdown)` and emulator
   port writes as fallbacks (ADR-0010).
4. **Boot into UEFI** by setting `OsIndications.BOOT_TO_FW_UI` through UEFI Runtime
   Services and cold-resetting, so the firmware stops in its setup UI (ADR-0009).
5. A reproducible disk image (`cargo xtask image`), and automated end-to-end QEMU
   tests for all of the above (`cargo xtask test`) run in CI.

Out of scope (later stages): interrupts/IDT, memory management beyond a bump
frame allocator, scheduling, user mode, USB input, Secure Boot.

## Consequences
The first stage is deliberately user-visible and end-to-end; foundational kernel
work (exceptions, paging, heap) follows in Stage 2 with its own hardening gate.

Testing note: the end-to-end tests observe the kernel through the serial console and
drive it through the QEMU monitor (key and mouse injection, screendump). The
`isa-debug-exit` device named in ADR-0006 is reserved for in-kernel unit tests later.
