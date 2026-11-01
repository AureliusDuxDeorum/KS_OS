# ADR-0003: Target architecture is x86_64

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Stage/Phase:** S1

## Context
We need one primary target with strong emulation and debugging support.

## Decision
x86_64 (long mode, UEFI boot) is the primary and only initial target; QEMU/OVMF
is the reference platform. Architecture-specific code is isolated under an `arch`
module boundary so other ISAs remain possible.

## Alternatives considered
- AArch64: clean ISA, but less learning material and tooling for early bring-up.
- RISC-V 64: minimal, fewer real hardware targets.

## Consequences
Inherits x86 legacy complexity (APIC, ACPI, MSRs); mitigated by UEFI/Limine.
