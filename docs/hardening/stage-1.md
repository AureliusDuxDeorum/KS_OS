# Hardening gate G1 — end of Stage 1

- **Stage reviewed:** Stage 1 (boot, "OS operating", Power off / Boot into UEFI)
- **Reviewer:** Claude (primary maintainer)
- **Date:** 2026-11-01
- **Status:** **OPEN** — automated checks pass; Stage 2 must not start until the
  items marked **blocking** are resolved or waived by ADR (see ADR-0002).

## 1. Threat model delta
New attack surface introduced by Stage 1:

| Surface | Trust assumption | Notes |
| --- | --- | --- |
| Limine bootloader | Trusted, pinned by commit + SHA-256 (ADR-0012) | Not Secure-Boot signed |
| UEFI runtime services | Trusted, **runs in ring 0 inside our address space** (ADR-0009) | Largest new trust expansion |
| ACPI tables / DSDT scan | Firmware-provided, parsed with bounds + checksum checks | Malformed tables fail closed (power-off falls back) |
| i8042 (PS/2) input | Hardware; polled | No injection path from user mode exists yet |
| Serial console | Output only | No input is accepted |

No user mode exists yet, so there is no privilege boundary to defend in Stage 1.

## 2. Automated checks (run 2026-11-01)

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy -p kernel --target x86_64-unknown-none -- -D warnings` (incl. `undocumented_unsafe_blocks` = deny) | pass |
| `cargo clippy -p xtask -- -D warnings` | pass |
| `cargo deny check` (advisories, bans, licenses, sources) | pass |
| `cargo audit` | runs in CI (RustSec data is also covered by `cargo deny` advisories) |
| `cargo xtask test` — boot banner, power off (key), power off (mouse), boot into UEFI setup | pass (4/4) on QEMU q35 + OVMF 2024.02 |
| Fuzzing / Miri / sanitizers | not applicable yet: parsers (ACPI, EFI tables) are small; host-side fuzz targets for them are **planned for Stage 2** |

## 3. `unsafe` inventory
All `unsafe` blocks carry a `// SAFETY:` justification; this is enforced by
`#![deny(clippy::undocumented_unsafe_blocks)]`.

| Module | Purpose |
| --- | --- |
| `arch.rs` | Port I/O, control registers, MSRs, SSE enablement — the single home of raw CPU `asm!` |
| `serial.rs`, `ps2.rs` | Port I/O to legacy devices (bounded waits, no unbounded spin on absent hardware) |
| `limine.rs`, `boot.rs` | Volatile reads of bootloader-written requests/responses; slices over bootloader-provided arrays |
| `fb.rs` | Volatile writes to the framebuffer (every access bounds-checked) |
| `frames.rs`, `paging.rs` | Page-table frames through the HHDM |
| `efi.rs` | Firmware calls (`extern "efiapi"`), firmware table reads through the HHDM — each preceded by `BootInfo::hhdm_covers` |
| `acpi.rs` | Port writes to PM1 control |
| `strbuf.rs` | `from_utf8_unchecked` over bytes only appended on char boundaries |

## 4. Mitigation review
*(State at the time of review; R2 and R3 have since been resolved, see §5.)*

| Mitigation | State |
| --- | --- |
| W^X for kernel image | **Yes** — separate R-X / R-- / RW- `PT_LOAD` segments, 4 KiB aligned |
| NX on kernel-built mappings | **Yes** (EFER.NXE enabled when supported) |
| W^X for UEFI runtime code | **Yes when the Memory Attributes Table exists** (true on OVMF); otherwise RWX — tracked as R1 |
| Stack guard pages | **No** — kernel runs on the bootloader stack (256 KiB requested) → R2 |
| IDT / exception handling | **No** — any fault triple-faults and resets; IF is never enabled → R3, **blocking for Stage 2** (first item) |
| KASLR | No (fixed link address `0xffffffff80000000`) → R4 |
| SMEP/SMAP/UMIP | Not enabled — nothing runs in user mode yet → enable with user mode (Stage 3) |
| Stack protector / CFI | Not available on stable for bare-metal targets; revisit → R5 |
| Secrets / credentials in repo | None; `run_secret_scanning` to be run before first PR |
| Supply chain | Limine pinned + hashed; Rust deps: `font8x8`, `spin` (kernel); `sha2`, `crc32fast`, `fatfs` (host tooling) — all vetted by `cargo deny` |

## 5. Risks and follow-ups

| ID | Risk | Action | Blocking Stage 2? |
| --- | --- | --- | --- |
| R1 | Runtime code RWX on firmware without a Memory Attributes Table | Keep reporting at boot; consider refusing runtime services unless `strict_wx` | no |
| R2 | No stack guard pages | **RESOLVED 2026-11-01** (S2.P1, ADR-0014): guard-paged kernel and IST stacks, overflow test in CI | no |
| R3 | No IDT: faults reset the machine silently | **RESOLVED 2026-11-01** (S2.P1, ADR-0014): IDT, IST stacks, fatal diagnostics, fault self-tests | ~~yes~~ no |
| R4 | No KASLR | Evaluate with relocatable kernel in Stage 2/3 | no |
| R5 | No compiler-level CFI/stack-protector | Re-evaluate toolchain options at gate G2 | no |
| R6 | PS/2-only input (USB-only machines cannot press the buttons) | USB HID driver in Stage 5; interrupt-driven PS/2 in Stage 2 | no |
| R7 | `\_S5` found by DSDT byte-scan, not AML interpretation | AML interpreter with the driver framework | no |
| R8 | CI workflow has not yet run on GitHub | Confirm first run is green; pin actions by SHA | **yes** (before gate closes) |
| R9 | No host-side fuzzing of ACPI/EFI table parsers | Factor parsers into a host-testable crate and fuzz | **yes** (before gate closes) |

## 6. Decision
Waiver recorded in [ADR-0013](../adr/0013-stage-2-scope.md): Stage 2 may start; R3, R8, R9 are rescheduled into Stage 2 phases and Gate G2 cannot close while they are open.

Gate **not yet closed**. To close: resolve R8 and R9 (R3 done) (or accept a waiver ADR),
then change **Status** to CLOSED and update `docs/ROADMAP.md`.

## 7. Addendum (2026-11-01, during S2.P3)
- **W^X in the EFI runtime window — fixed.** Re-auditing the kernel address space after
  firmware setup found 216 pages mapped writable+executable, caused by Memory Attributes Table
  entries lacking both the RO and XP flags (R1 in practice, even with a MAT present). Those
  entries now fall back to the region type's default; the audit reports zero violations and
  runs on every boot and in CI.
- **x2APIC is unverified** in this environment (see ADR-0021); xAPIC is the default.
