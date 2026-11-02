# ADR-0030: Security review 1 — scan results and dispositions

- **Status:** Accepted
- **Date:** 2026-11-02
- **Deciders:** owner (request), Claude (maintainer)
- **Stage/Phase:** S2 (pre-test review; input to Gate G2)

## Context
Before the first hardware test the owner asked for a three-step security rundown: **1. scan, 2. record,
3. fix as much as possible**. This ADR is steps 1 and 2; step 3 is the commits that reference the IDs below.

## Scan method
- Supply chain: `cargo audit` (1,402 advisories, 103 crates: no vulnerabilities), `cargo deny check`.
- Static: `unsafe` census (194 occurrences in `kernel/`, all with `// SAFETY:`; confirmed by
  `clippy::undocumented_unsafe_blocks` = 0), clippy with `indexing_slicing`, `arithmetic_side_effects`,
  `unwrap_used`, `expect_used`, `panic` on the library crates (156 / 162 / 2 / 4 / 0 hits).
- Dynamic: existing QEMU suite incl. W^X audit (0 violations), kernel-write/exec/double-free violation
  tests, three libFuzzer targets (`parsers`, `bluetooth`, `network`).
- Manual review of every path where outside data crosses a trust boundary: Bluetooth radio and pairing
  policy, USB descriptors, PCI capabilities/BARs read from devices, virtio/igc device-written rings and
  registers, network frames, firmware/ESP content, boot configuration, CPU protection bits, build profile, CI.

## Findings
Severity is the author's estimate for the *current* single-ring-0 system. "Fixed" references this ADR's ID in the commit message.

| ID | Sev | Finding | Disposition |
|---|---|---|---|
| S-01 | **High** | Bluetooth auto-pairs **any** discoverable HID device in range and trusts it as a keyboard: a nearby attacker advertising "keyboard" gets keystroke injection (the UI executes power-off / firmware-setup on keys). | **Fixed**: pairing only inside an explicit window — opened automatically for 120 s when the machine has *no other input device* (bootstrap, trust on first use), otherwise by the user pressing `B`; bonded devices reconnect without a window. The rules live in `ksbt::policy` (host-tested, 6 tests) and the `B` key in the UI. |
| S-02 | Med | Hostile or buggy BT devices could freeze the UI: unbounded candidates, each connect/pair blocks up to several seconds, repeated every rescan. | **Fixed**: at most 3 connection attempts per round, bonded devices first, 60 s cooldown per failing address (`ksbt::policy`). |
| S-03 | Med | SMEP/SMAP/UMIP not enabled although supported by the CPU (cheap defence in depth, essential once user mode exists). | **Fixed**: enabled at boot when CPUID reports them (plus CR0.WP asserted), logged as `cpu: protections …`, QEMU test `cpu-protections`. |
| S-04 | Med | Release builds wrap integers silently (`overflow-checks` off): arithmetic bugs on device/network-supplied lengths become silent corruption instead of a fail-stop. | **Fixed**: `overflow-checks = true` in the release profile. |
| S-05 | Med | Device-controlled values could trigger kernel `assert!` panics at driver init: virtio capability lengths and notify offsets, igc BAR size (denial of service by a malicious/buggy device). | **Fixed**: validated, init returns an error instead. |
| S-06 | Low | Bluetooth pairing passkey was also written to the serial log / log ring. | **Fixed**: shown on screen only; the log records that a passkey was displayed. |
| S-07 | Low | Limine's boot-time config editor not explicitly disabled. | **Fixed**: `editor_enabled: no` (menu already skipped with `timeout: 0`). |
| S-08 | Low | `// SAFETY:` discipline must be enforced, not conventional. | **Verified, no change needed**: `clippy::undocumented_unsafe_blocks` has been `deny` in the kernel since Stage 1 (0 hits); the other crates `forbid(unsafe_code)`. |
| S-09 | Low | CI actions pinned by mutable tag (`@v4`): supply-chain risk. | **Fixed**: pinned to commit SHAs (tag kept in a comment). |
| S-10 | Low | `expect()` in `ksbt::crypto` (3) and the fuzz harness (1). | **Accepted**: provably infallible (fixed-size key, uncompressed curve point); harness is test-only. |
| S-11 | Info | Library crates contain indexing/arithmetic that can panic on malformed input (156/162 lint hits). | **Accepted**: libraries are `forbid(unsafe)`, so the worst case is a fail-stop, not memory corruption; fuzzed (`parsers` 4.1 M, `bluetooth` 880 k, `network` 450 k execs, no crash). S-04 turns arithmetic wrap into fail-stop. Re-scan each stage. |
| S-12 | Med | Bluetooth bonds (LTKs) are stored **unencrypted** in a UEFI NV variable readable by any code with runtime-services access. | **Open (G2)**: no machine secret exists without a TPM/measured boot; seal to TPM2 in the storage/security stage. |
| S-13 | Med | Just Works pairing (all that most HID devices offer) has no MITM protection. | **Accepted**: protocol limit; mitigated by S-01 (short window) and by logging unauthenticated bonds. |
| S-14 | **High** | No IOMMU: xHCI, NICs and any PCIe device are trusted DMA masters. | **Open (G2)**: VT-d/AMD-Vi enforcement planned before untrusted devices/userland. |
| S-15 | Med | Boot chain is unauthenticated: no Secure Boot, ESP content (kernel, `limine.conf`, cmdline incl. `selftest=`, firmware blobs) can be altered by anyone with write access to the disk. | **Open (G2/G3)**: Limine config-hash enrolment + blob hashes + Secure Boot signing. |
| S-16 | Med | No kernel ASLR, no stack canaries (static non-PIE kernel). | **Open (G3)**: relocatable kernel; low value before user mode exists. |
| S-17 | Low | Network: no firewall (ICMP echo answered, no inbound policy), IPv4 only, DHCP/ARP/DNS trust the LAN, no DNSSEC/DoT. | **Open (G2)**. No listening TCP/UDP service exists, so inbound exposure is ICMP only. |
| S-18 | Info | Receive path is polled; a flood cannot starve interrupts but can starve the UI loop. | **Open (G2)**: interrupt-driven RX with budget. |

## Result of step 3
Released as **0.2.1** (bug-patch release: security fixes only). Full suite: 25 ok + 1 skip, with
overflow checks active in the release kernel. New tests: `ksbt/tests/policy.rs` (S-01/S-02) and the
`cpu-protections` QEMU test (S-03). S-05/S-06/S-07/S-09 are covered by review plus the existing
end-to-end tests (no behaviour change on valid hardware).

## Update (2026-11-02): dispositions after phases A–H
The open and accepted items S-10 to S-18 were worked through afterwards; the outcome with evidence is in
[ADR-0031](0031-security-review-2.md). In short: S-10, S-11, S-13, S-17 (DoT deferred), S-18 fixed; S-12, S-14, S-15, S-16
fixed in code and emulation (hardware-unverified; canaries and interrupt remapping still open).

## Consequences
- After the fixes, every High finding is either fixed (S-01) or tracked as a Gate-G2 blocker (S-14).
- `docs/hardening/stage-1.md` gets an addendum; `STRUCTURE.md` conventions list the new lint gate.
- Re-run this review at every stage gate; new findings get new IDs, never renumbered.
