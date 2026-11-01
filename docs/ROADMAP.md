# Roadmap

Legend: ☐ not started · ◐ in progress · ☑ done. IDs: `S<stage>.P<phase>.<step>`.
Each stage ends with a **Hardening gate** ([ADR-0002](adr/0002-staged-development-process.md)).

## Stage 0 — Project foundation
- ☑ S0.P1: Governance — ADRs, contributing rules, roadmap

## Stage 1 — Boot, "OS operating", Power off / Boot into UEFI ([ADR-0008](adr/0008-stage-1-scope.md))
- ☑ S1.P1 Toolchain & build — pinned stable Rust, workspace, linker script, `cargo xtask` ([ADR-0007](adr/0007-stable-rust-toolchain.md))
- ☑ S1.P2 Image & boot — GPT + FAT ESP image, pinned/verified Limine ([ADR-0012](adr/0012-limine-pin-and-base-revision.md))
- ☑ S1.P3 Kernel entry — Limine bindings, serial console, panic handler, `OS operating` on serial and framebuffer
- ☑ S1.P4 Input & UI — PS/2 keyboard/mouse polling, framebuffer UI with two buttons ([ADR-0011](adr/0011-ps2-polling-input.md))
- ☑ S1.P5 Power off — ACPI S5 with UEFI and emulator fallbacks ([ADR-0010](adr/0010-power-off-path.md))
- ☑ S1.P6 Boot into UEFI — runtime services, `SetVirtualAddressMap`, `OsIndications` ([ADR-0009](adr/0009-uefi-runtime-services.md))
- ☑ S1.P7 Verification — QEMU/OVMF end-to-end tests, GitHub Actions CI
- ◐ **Gate G1:** hardening review — see [`hardening/stage-1.md`](hardening/stage-1.md) (open items listed there)

## Later stages (outline only; to be re-planned after Gate G1)
- Stage 2: Exceptions (GDT/TSS/IDT), physical + virtual memory manager, kernel heap, APIC/timer, interrupt-driven input
- Stage 3: Threads, scheduler, user mode, syscalls
- Stage 4: Capabilities and IPC
- Stage 5: Userspace services, driver framework (USB HID, AML interpreter), storage, networking
