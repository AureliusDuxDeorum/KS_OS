# ADR-0010: Power-off strategy

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Stage/Phase:** S1

## Context
Soft power-off must work on real hardware and common hypervisors.

## Decision
Attempt, in order, until the machine stops:
1. **ACPI S5**: parse RSDP→XSDT/RSDT→FADT (checksums verified), find
   `\_S5` in the DSDT, perform the `SCI_EN` handshake via `SMI_CMD` if needed,
   write `SLP_TYPx | SLP_EN` to `PM1a/PM1b_CNT`.
2. **UEFI** `ResetSystem(EfiResetShutdown)`.
3. **Emulator ports** (QEMU `0x604`, Bochs `0xB004`, VirtualBox `0x4004`).
4. Otherwise report failure on screen and keep running.

`\_S5` is extracted by a bounded scan of the DSDT rather than an AML interpreter.

## Consequences
Covers real firmware and all major emulators. Platforms defining `\_S5` outside the
DSDT (e.g. in an SSDT) fall through to the UEFI path. A real AML interpreter is
scheduled with the driver framework (Stage 4).
