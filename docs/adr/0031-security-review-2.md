# ADR-0031: Security review 2 — closing ADR-0030, Gate G2 record

- **Status:** Accepted
- **Date:** 2026-11-02
- **Deciders:** owner (request: "fix all of them"), Claude (maintainer)
- **Stage/Phase:** S2 (Gate G2)
- **Supersedes the dispositions of:** [ADR-0030](0030-security-review-1.md)

## Context
ADR-0030 left S-10…S-18 accepted or open. The owner asked for all of them to be fixed, in phases A–H. This ADR records
the outcome with evidence, the honest limits, and what Gate G2 still depends on.

## Re-scan (same method as review 1)
`cargo audit` (1,402 advisories, 113 crates): no vulnerabilities. `cargo deny`: clean (new licences: 0BSD for smoltcp).
`unsafe` census: 200 occurrences in `kernel/`, 0 undocumented; the five library crates (`ksparse`, `ksbt`, `ksnet`,
`ksiommu`, `kstpm`) are `forbid(unsafe_code)`. The lint set `indexing_slicing`, `arithmetic_side_effects`, `unwrap_used`,
`expect_used`, `panic` is **0 hits and denied at compile time** in all five. W^X audit: 0 violations on every boot.
Fuzzing (45–60 s each, no crash): `parsers` 310 k (now incl. DMAR/IVRS), `bluetooth` 330 k, `network` 74 k, `tpm` 741 k.

## Outcome per finding

| ID | Finding | Result | Evidence |
|---|---|---|---|
| S-10 | `expect()` in crypto / harness | **Fixed** — removed; lint now denies it | clippy gate in all libs |
| S-11 | Panicking indexing/arithmetic in parsers | **Fixed** — ~320 sites converted to bounds-checked readers; enforced at compile time | lint gate; fuzzers |
| S-12 | Bonds stored unencrypted | **Fixed (hardware-unverified)** — AES-256-GCM record, key sealed to TPM PCR 7; plaintext migrated and erased; no TPM => not persisted unless `bt.bonds=unencrypted` | `kstpm` vs **swtpm**; QEMU `tpm` (CRB+TIS) |
| S-13 | Just Works pairing | **Fixed** — keyboards require passkey; mice may use Just Works; owner override `J` per pairing window | `ksbt` tests |
| S-14 | No IOMMU | **Fixed (hardware-unverified), partially** — VT-d and AMD-Vi DMA remapping, deny-by-default, RMRR/IVMD honoured, faults logged. **Not done:** interrupt remapping, ATS/PASID, queued invalidation | QEMU `iommu-vtd`, `iommu-amd` |
| S-15 | Unauthenticated boot chain | **Fixed (Secure Boot enforcement half untested here)** — BLAKE2b-pinned kernel/modules, enrolled config hash, owner-key Secure Boot tooling | QEMU `boot-chain`, `secure-boot` (signing), `fw-script`; enforcing-firmware test needs `virt-firmware` |
| S-16 | No ASLR / canaries | **Partially fixed** — KASLR (PIE kernel, Limine `kaslr`). **Stack canaries remain impossible on stable Rust** (accepted, compensated) | QEMU `kaslr` |
| S-17 | No firewall, IPv4 only | **Fixed (DoT deferred)** — default-deny ingress filter, echo replies off, IPv6 SLAAC. DNS-over-TLS not implemented (no suitable `no_std` TLS stack yet) | 15 filter tests; QEMU `net-*` |
| S-18 | Polled receive | **Fixed** — MSI-X receive, 64-frame budget per pass | QEMU `net-*` (`require_irq`) |

Earlier items S-01…S-09 stand as fixed in ADR-0030.

## Bugs the work itself uncovered (all fixed)
A smoltcp address table too small for dual stack silently dropped the IPv4 address; smoltcp panics when SLAAC is on but no
link-local address is configured; the IPv6 link-local range check wrongly classified `fec0::/10`; the `add-bluetooth-firmware.sh`
script would have broken the new hash-pinned chain (it now re-pins and re-enrols); a fuzz-found virtqueue trust issue (review 1).

## What this does **not** give you (read before trusting it)
1. **Nothing here has run on the owner's hardware.** igc, TPM (CRB/TIS), VT-d/AMD-Vi, KASLR/relocation under Limine on real
   firmware, MSI-X on the I225 and Secure Boot enrolment are verified only against QEMU/swtpm models. Each has a clear `iommu:`,
   `tpm:`, `net:`, `bt:` log line and an escape hatch (`set-cmdline.sh ... iommu=off`, `bt.bonds=unencrypted`).
2. Remaining open (Gate G3 / later stages): stack canaries, interrupt remapping, rollback protection of the bond record, DoT/DNSSEC,
   privacy extensions for IPv6 (SLAAC uses an EUI-64 address), TPM start methods needing AML, a userland-facing firewall policy
   (there is no userland yet), reproducible signed images.
3. With Secure Boot **off**, an attacker who replaces both `BOOTX64.EFI` and `limine.conf` consistently still wins; Secure Boot with
   the owner's keys closes this.

## Gate G2
G2's blockers named in `hardening/stage-1.md` (R3, R8, R9, S-12, S-14, S-15) are resolved **in code and in emulation**. The gate
closes when the owner's hardware test confirms the log lines in `docs/hardware-testing.md` section "Gate G2 checklist".
