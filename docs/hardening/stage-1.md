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
| R7 | `\_S5` found by DSDT byte-scan, not AML interpretation | **DECIDED 2026-11-01** (ADR-0023): no AML interpreter until a concrete need; scan stays, now in the fuzzed `ksparse` crate | no |
| R8 | CI workflow has not yet run on GitHub | Confirm first run is green; pin actions by SHA | **yes** (before gate closes) |
| R9 | No host-side fuzzing of ACPI/EFI table parsers | **RESOLVED 2026-11-01** (S2.P4, ADR-0023): ACPI, PCI and UEFI (memory map, config tables, MAT, W^X policy) parsing lives in `ksparse` with mutation/noise tests and a libFuzzer target (4.1 M executions / 91 s, no crash, run in CI). Moving the EFI code also fixed unchecked firmware-supplied counts and region sizes | no |

## 6. Decision
Waiver recorded in [ADR-0013](../adr/0013-stage-2-scope.md): Stage 2 may start; R3, R8, R9 are rescheduled into Stage 2 phases and Gate G2 cannot close while they are open.

Gate **not yet closed**. To close: resolve R8 — the first green GitHub Actions run (R3, R9 done) (or accept a waiver ADR),
then change **Status** to CLOSED and update `docs/ROADMAP.md`.

## 7. Addendum (2026-11-01, during S2.P3)
- **W^X in the EFI runtime window — fixed.** Re-auditing the kernel address space after
  firmware setup found 216 pages mapped writable+executable, caused by Memory Attributes Table
  entries lacking both the RO and XP flags (R1 in practice, even with a MAT present). Those
  entries now fall back to the region type's default; the audit reports zero violations and
  runs on every boot and in CI.
- **x2APIC is unverified** in this environment (see ADR-0021); xAPIC is the default.

## 8. Addendum (2026-11-01, S2.P4)
- Fuzzing: `cargo +nightly fuzz run parsers` — 4,103,168 executions in 91 s, no crashes, corpus grew
  to cover MADT/MCFG/FADT/DSDT/RSDP/EFI/PCI paths. CI now runs a 60 s fuzz job.
- Hardening found while moving the EFI parsing: firmware-supplied `number_of_table_entries` was
  multiplied unchecked (could wrap and lead to an out-of-bounds read) and descriptor page counts
  were not range-checked. Both are now rejected by `ksparse::efi`.
- **IOMMU (VT-d/AMD-Vi) is detected but not enforced**: DMA-capable devices are trusted. Enforcement
  is a Gate G2 item before any untrusted-device exposure.

## 9. Addendum (2026-11-01, S2.P5 USB)
- New untrusted-data surface: USB descriptors (device/configuration/hub) and HID report descriptors
  and reports. Parsed in `ksparse` with bounded walks (item, depth, usage and field limits), covered
  by the same mutation/noise/fuzz regime; hostile HID descriptors (huge sizes, deep push stacks,
  endless collections, long items) have dedicated tests.
- The xHCI controller is a DMA master and is **trusted** until IOMMU enforcement (G2 item).
- Not handled yet: USB hot-plug, SuperSpeed hubs, isochronous transfers.

## 10. Addendum (2026-11-02, S2.P6 Bluetooth)
- New untrusted-data surface: everything received over the radio (advertisements, ATT, SMP, L2CAP),
  the vendor firmware file and the stored bond record. Parsers are bounded and fuzzed
  (880 k executions, no crash), crypto is checked against the Core Spec vectors.
- **Open items for Gate G2** (details in ADR-0025): bonds stored unencrypted in UEFI NV storage;
  automatic pairing with any discoverable HID device (needs an authorisation policy); Just Works
  pairs are MITM-exposed; no Classic BR/EDR.
- MediaTek bring-up and the USB HCI transport are **unverified on hardware**; see `docs/hardware-testing.md`.

## 11. Addendum (2026-11-02, S2.P7 Networking)
- New untrusted-data surface: every Ethernet frame (ARP/IPv4/ICMP/UDP/TCP/DHCP/DNS — parsed by smoltcp,
  upstream-fuzzed) and everything the NIC writes into shared descriptor/used rings (parsed by `ksnet`,
  fuzz target `network`: 450 k executions, no crash after the fix below).
- **Fuzzer finding, fixed:** the virtqueue used the `next` field stored in device-shared descriptor memory
  to walk/recycle chains; a hostile device could force out-of-range indexing. Chain links are now kept in
  driver-private memory (regression test `virtqueue_ignores_descriptor_next_scribbled_by_the_device`).
- **Open items for Gate G2:** the NIC is a trusted DMA master until the IOMMU is enforced; the host answers
  ICMP echo and has no firewall; receive is polled (no interrupts); IPv4 only; DNS has no DNSSEC/DoT;
  weak TCP seed fallback if no hardware RNG (logged); I225-V specifics are unverified on hardware.

## 12. Addendum (2026-11-02, security review 1 — ADR-0030)
Scan found 18 items: 9 fixed (S-01…S-09 incl. the High-severity Bluetooth auto-pairing), 4 accepted with
reasons, 5 open for Gate G2/G3. **Gate G2 blockers that remain:** S-12 (unencrypted bonds), S-14 (no IOMMU),
S-15 (unauthenticated boot chain). See the ADR for the full table.

## 13. Addendum (2026-11-02, Phase D — KASLR, closes R4 / ADR-0030 S-16 in part)
- The kernel is now a position-independent executable; Limine slides it by a random 2 MiB-aligned offset each boot
  (`kaslr: yes`). QEMU test `kaslr` boots three times and requires differing bases; the full suite is unchanged.
- Kernel addresses appear in the log only in fatal-fault reports (needed for diagnosis; the log is local-only).
- **Stack canaries remain unavailable**: they need `-Z stack-protector` (nightly), the project is pinned to stable
  (ADR-0007). Compensating controls: guard pages on every kernel stack, `forbid(unsafe)` libraries,
  `overflow-checks`, W^X, SMEP/SMAP/UMIP. Revisit when `-C stack-protector` stabilises.

## 14. Addendum (2026-11-02, Phase E — verified boot chain, ADR-0030 S-15)
- `limine.conf` pins the kernel and every firmware module by BLAKE2b-512 (`path: ...#hash`) and the hash of
  `limine.conf` is enrolled into `BOOTX64.EFI` (a Rust port of `limine enroll-config`, `xtask/src/bootchain.rs`).
  QEMU test `boot-chain` flips one byte in the kernel, the config and the enrolled hash: Limine panics each time
  ("Blake2b hash ... does not match", "CHECKSUM MISMATCH FOR CONFIG FILE") and the kernel never starts.
- `add-bluetooth-firmware.sh` keeps the chain valid (pins the new blobs, re-enrols the config hash); test `fw-script`.
- Secure Boot uses the **owner's own keys**: `secure-boot.sh keys|sign` ships in every release; the private keys are
  generated and stay on the owner's machine. Test `secure-boot` covers key generation, signing, `sbverify` and that a
  signed image still boots. **Not yet automated here:** the enforcing-firmware half (unsigned image refused) because it
  needs `virt-firmware` (`virt-fw-vars`) to enrol keys into an OVMF variable store; the test is written and runs as
  soon as `target/venv/bin/virt-fw-vars` exists.
- Residual risk: with Secure Boot **off**, an attacker who can replace `BOOTX64.EFI` *and* `limine.conf` consistently can
  still boot their own kernel; Secure Boot with owner keys closes that. Signed images are not byte-reproducible (PKCS#7 time).

## 15. Addendum (2026-11-02, Phase F — IOMMU, ADR-0030 S-14)
- **DMA remapping is on by default** when the firmware exposes `DMAR` (Intel VT-d) or `IVRS` (AMD-Vi) and enabled
  it: every PCI function is blocked until its driver maps memory for it (`Dma::alloc_for(owner, ..)` ->
  `iommu::map_identity`; xHCI, igc/igb and virtio-net are migrated). Firmware reserved regions (RMRR / IVMD) stay
  mapped for the devices that need them. Faults are logged (`iommu: BLOCKED DMA ...`) and counted.
- Pure parts are host-tested/fuzzed: `ksparse::iommu` (DMAR/IVRS parsers, in the `parsers` fuzz target) and
  `ksiommu` (page tables in VT-d and AMD-Vi formats, root/context/device-table entries, software walker).
- QEMU tests `iommu-vtd` and `iommu-amd`: edu DMA, USB (incl. bulk) and virtio-net work with translation on, and a DMA
  write to an unmapped frame leaves memory untouched (VT-d additionally reports it as a fault; QEMU's AMD-Vi model
  blocks silently).
- **Escape hatch**: `iommu=off` on the kernel command line, settable on a finished image with `set-cmdline.sh`.
- **Not done (stays open)**: interrupt remapping (MSI/IOAPIC sources are not authenticated), ATS/PASID, queued
  invalidation (register-based invalidation is used), per-buffer lifetime tracking beyond `Dma::free`, and everything is
  **hardware-unverified** (only QEMU's models). Expect real firmware quirks; the first hardware test must read the
  `iommu:` lines.

## 16. Addendum (2026-11-02, Phase G — TPM-protected Bluetooth bonds, ADR-0030 S-12)
- Bonds are stored in `KSOS-BT-BONDS2` as an AES-256-GCM record. The key is generated by the TPM and **sealed to PCR 7**
  (Secure Boot configuration) with a policy session (`kstpm`); the record header (sealed blob, nonce, label) is
  authenticated. Old plaintext bonds (`KSOS-BT-BONDS`) are imported, re-saved protected, and the plaintext copy is erased.
- **No TPM => bonds are not persisted** (unless the owner adds `bt.bonds=unencrypted` to the command line) — never silently
  in the clear. If PCR 7 changes (Secure Boot toggled / keys enrolled) the record cannot be opened: pair again.
- Verification: `kstpm` against real TPM 2.0 code (**swtpm**) in host tests — seal/unseal, PCR binding (an extended PCR 7
  locks the secret), tampered blob, restart persistence, every single bit flip of a vault record is rejected; fuzz target `tpm`
  (1.3 M execs). QEMU test `tpm` runs the vault round trip through the kernel's CRB and TIS drivers.
- **Limits**: TPM start methods that need ACPI AML (2, 8, 11, 12) are unsupported; the unsealed key crosses the TPM bus
  unencrypted (fine for PTT/fTPM, sniffable on a discrete LPC/SPI TPM with physical access); no rollback protection of the
  record (an old valid record can be restored by someone with write access to firmware variables); PCR 7 on a machine
  *without* Secure Boot protects against tampering with the platform configuration only weakly.
  **Hardware-unverified** (the owner's TPM is untested here).

