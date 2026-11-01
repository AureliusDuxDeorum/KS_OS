# ADR-0009: UEFI Runtime Services via a virtual window and Memory Attributes Table

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Stage/Phase:** S1

## Context
"Boot into UEFI" requires `SetVariable(OsIndications)` and `ResetSystem` — both
runtime services that run firmware code in the kernel's address space. Firmware
expects either its original physical mappings or a one-shot relocation via
`SetVirtualAddressMap`. Limine (base revision 5) maps the system table and runtime
regions into the HHDM, but firmware-internal pointers are physical.

## Decision
`kernel/src/efi.rs` follows the model used by mainstream kernels:
- Build two page-table roots sharing the bootloader's higher half (PML4 256–511):
  a *boot* root that additionally identity-maps every `EFI_MEMORY_RUNTIME` region,
  and a *final* root containing only a dedicated virtual window (an unused PML4 slot).
- Call `SetVirtualAddressMap` with window addresses while the boot root is active,
  then switch to the final root so the lower half stays free for user mode.
- If the firmware rejects the map, stay in physical mode (identity map retained).
- Apply **W^X** from the UEFI Memory Attributes Table when present (code RX, data
  RW+NX). Without the table, runtime code is mapped RWX; this is reported at boot
  (`strict W^X=false`) and tracked in the hardening gate.
- Validate every firmware pointer against the bootloader memory map before the
  HHDM is dereferenced; enable EFER.NXE and SSE before calling firmware.
- Use `OsIndicationsSupported` to enable/disable the button honestly; never
  assume support.

## Alternatives considered
- Keep identity mapping permanently: simpler, but pollutes the lower half and
  blocks clean user-mode address spaces.
- Reboot via keyboard controller/ACPI reset: cannot request firmware setup.
- Write our own UEFI application stub: needless duplication of Limine's job.

## Consequences
Runtime firmware is a trusted, privileged component inside our address space; the
Stage 1 gate records this. `SetVirtualAddressMap` is one-shot, so this code must
run exactly once, with interrupts disabled, on the BSP.
