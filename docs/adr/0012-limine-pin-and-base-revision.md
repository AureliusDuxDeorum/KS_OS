# ADR-0012: Limine v11 pinned by commit and checksum; base revision 5

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Supplements:** [ADR-0006](0006-boot-and-test-tooling.md)
- **Stage/Phase:** S1

## Context
The bootloader is the first code to run and is part of our trusted base.

## Decision
- Use Limine from the upstream `v11.x-binary` branch, pinned to a specific commit
  (see `xtask/src/limine.rs`). `cargo xtask` fetches that exact commit and refuses
  to build if the SHA-256 of `BOOTX64.EFI` or `LICENSE` differs.
- Require **Limine base revision 5**: clean control-register state, IOMMUs
  quiesced, firmware tables and runtime services mapped in the HHDM.
- Boot config: `timeout: 0`, single entry, Limine protocol, kernel at `/boot/kernel`.
- Upgrades change the pin, hashes and (if needed) `limine.rs` in one reviewed commit.

## Consequences
Reproducible images; no unreviewed bootloader updates. Not Secure-Boot signed:
Secure Boot support is future work (shim/MOK or own signing).
