# ADR-0028: Networking: smoltcp stack, igc/igb and virtio-net drivers

- **Status:** Accepted
- **Date:** 2026-11-02
- **Deciders:** Claude (maintainer), owner
- **Stage/Phase:** S2.P7

## Context
Stage 2 requires wired internet on the owner's Intel I225-V (`8086:15f3`). The long-term goal (ADR-0022)
needs a real TCP/IP stack. QEMU cannot emulate an I225, so the driver must be verifiable some other way,
and a NIC is a DMA master that parses attacker-controlled frames and shares descriptor memory with hardware.

## Decision
- **Stack:** `smoltcp` 0.14 (0BSD, no_std, widely deployed, upstream-fuzzed) for Ethernet/ARP/IPv4/ICMP/
  UDP/TCP/DHCPv4/DNS. We do not write our own TCP. Wrapped by `ksnet::Stack` (DHCP lease handling, ping,
  one-shot DNS A queries, TCP connect/listen/send/recv/close, link-loss handling), IPv4 only.
  The stack answers ICMP echo (`auto-icmp-echo-reply`), like most OSes.
- **Crate split:** all logic is in `crates/ksnet` (`forbid(unsafe)`, host-tested): the `Nic` trait, the
  smoltcp adapter, the igc/igb register and descriptor definitions with RX/TX ring logic, and the
  virtio-net layout with a split-virtqueue implementation. Rings are written against a `DmaMem` trait so
  they run unchanged on the host against simulated devices; the kernel only supplies MMIO/DMA/PCI glue
  (`kernel/src/net/`).
- **Drivers:** `igc` (I225/I226, owner's hardware) which also drives the 82576 as emulated by QEMU's `igb`
  (same queue block and advanced descriptors) — this gives an end-to-end test of the real driver code
  path in CI; `virtio-net` (modern transport) for CI/VMs. Polled from the UI idle loop (1 kHz); 512-entry
  rings. PHY handled over MDIO only to power up / restart auto-negotiation when it has not completed.
- **Hardening applied now:** frames and descriptors from the device are length/flag checked and dropped,
  never trusted; virtqueue chain links are kept in driver-private memory (a fuzzer finding: the shared
  `next` field let a hostile device cause out-of-range indexing); a fuzz target `network` covers hostile
  rings and hostile frames. TCP/port seeds come from the hardware RNG (TSC mixing only if absent, logged).
- **Boot behaviour:** DHCP, then a ping of the gateway only (no traffic to the internet at boot). Status
  lines reach the UI status bar and the `L` log (`net:` prefix).

## Alternatives considered
- *Own TCP/IP stack*: large attack surface and years of bug history to repeat; rejected.
- *Linux-style NAPI/interrupt-driven RX now*: more complex and needs MSI-X plumbing per queue; deferred to
  Gate G2 as a performance/latency item — polling at 1 kHz with 512 descriptors is sufficient for 2.5 GbE bursts.
- *Only virtio-net for tests*: would leave the igc code path completely unexercised; the QEMU `igb` model
  exercises ring, queue enable, MDIO and link logic of the real driver.
- *e1000e driver as the test device*: different register/descriptor layout from igc; no benefit.

## Consequences
- Verified in CI: DHCP, ICMP, TCP round trip (host server) on virtio-net and on igb; DNS logic on the host
  (QEMU's resolver is unavailable offline, so the guest DNS check is informational).
- **Unverified on real I225-V:** the I225-specific bits (device-reset bit, MDIO addressing of the internal
  PHY, 2.5G status bit, NVM-loaded MAC timing). `docs/hardware-testing.md` lists the `net:` lines to send back.
- Gate G2 follow-ups: IOMMU (NIC is a trusted DMA master), interrupt-driven RX, IPv6, a userland socket API,
  DNS hardening (DNSSEC/DoT) and a firewall policy.
