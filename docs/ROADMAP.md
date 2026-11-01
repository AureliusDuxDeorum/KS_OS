# Roadmap

Legend: ☐ not started · ◐ in progress · ☑ done. IDs: `S<stage>.P<phase>.<step>`.
Each stage ends with a **Hardening gate** ([ADR-0002](adr/0002-staged-development-process.md)).

## Stage 0 — Project foundation
- ☑ S0.P1: Governance — ADRs, contributing rules, roadmap

## Stage 1 — Boot and bare-metal kernel (proposed, awaiting owner confirmation)
- ☐ S1.P1 Toolchain & build: pinned Rust toolchain, workspace, `x86_64-unknown-none` target, image build, QEMU runner
- ☐ S1.P2 CI: fmt/clippy/deny/audit + headless QEMU boot test
- ☐ S1.P3 Boot: Limine handoff, higher-half kernel, serial + framebuffer logging, panic handler
- ☐ S1.P4 CPU setup: GDT/TSS, IDT, exception handlers with IST stacks
- ☐ S1.P5 Memory: memory-map ingest, physical frame allocator, paging/VMM, kernel heap
- ☐ S1.P6 Interrupts & time: APIC/IOAPIC, timer, in-kernel test harness
- ☐ **Gate G1:** hardening review (`docs/hardening/stage-1.md`)

## Later stages (outline only)
- Stage 2: Threads, scheduler, user mode, syscalls
- Stage 3: Capabilities and IPC
- Stage 4: Userspace services and driver framework
- Stage 5: Storage, filesystem, networking
