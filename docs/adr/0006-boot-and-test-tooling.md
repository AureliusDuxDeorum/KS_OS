# ADR-0006: Limine boot, QEMU testing, GitHub Actions CI

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Stage/Phase:** S1

## Context
We want to reach kernel code quickly and verify every commit automatically.

## Decision
- Bootloader: Limine (Limine boot protocol, UEFI) rather than a custom loader.
- Run/test: QEMU with OVMF; kernel tests report via `isa-debug-exit` and serial.
- CI: GitHub Actions builds, lints (`fmt`, `clippy`), audits dependencies
  (`cargo-deny`, `cargo-audit`), and boots the image headless in QEMU.
- Commits follow Conventional Commits (see CONTRIBUTING).

## Alternatives considered
- Custom bootloader: large detour before a kernel exists (revisit in a later ADR).
- GRUB/Multiboot2: less suited to 64-bit UEFI-first design.

## Consequences
Dependence on Limine's protocol; boot-chain trust is revisited at the Stage 1 gate.
