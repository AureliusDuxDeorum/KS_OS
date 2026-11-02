# Changelog

Versions follow `X.Y.Z` as defined in [docs/VERSIONING.md](docs/VERSIONING.md): `X` = finished (0 = not),
`Y` = version, `Z` = bug patch.

## 0.4.0 — Gate G2 hardening (not finished)

Boot image: [`KS_OS-v0.4.0/`](KS_OS-v0.4.0/). Evidence and limits: [ADR-0031](docs/adr/0031-security-review-2.md).

- **Network:** default-deny ingress firewall, echo replies off by default, IPv6 (SLAAC), interrupt-driven receive (MSI-X).
- **DMA protection:** deny-by-default IOMMU for Intel VT-d and AMD-Vi (escape hatch: `iommu=off`).
- **Boot chain:** kernel, firmware and configuration are hash-pinned; optional Secure Boot with your own keys (`secure-boot.sh`);
  KASLR; `set-cmdline.sh` to change kernel options safely.
- **Secrets:** Bluetooth bonds are encrypted with a key sealed in the TPM (`bt.bonds=unencrypted` to opt out).
- **Not yet verified on real hardware** — see the Gate G2 checklist in [docs/hardware-testing.md](docs/hardware-testing.md).

## 0.3.0 — robust parsers, strict keyboard pairing (not finished)

Boot image: [`KS_OS-v0.3.0/`](KS_OS-v0.3.0/). Security work: [ADR-0030](docs/adr/0030-security-review-1.md) follow-ups.

- The parsing crates (`ksparse`, `ksbt`, `ksnet`) can no longer panic on malformed input: indexing, unchecked
  arithmetic, `unwrap`/`expect` are compile errors there (S-10, S-11).
- **Keyboards must pair with a passkey**; Just Works is refused unless you press **J** for the current pairing window (S-13).

## 0.2.1 — security review 1 (not finished)

Boot image: [`KS_OS-v0.2.1/`](KS_OS-v0.2.1/). Details: [ADR-0030](docs/adr/0030-security-review-1.md).

- **Bluetooth pairing is no longer open to anyone in range.** New devices pair only inside a pairing window:
  automatically for 120 s when the machine has no other keyboard/mouse (so a Bluetooth-only setup still
  works), otherwise after pressing **B**. Bonded devices reconnect as before. At most 3 attempts per round,
  60 s cooldown after a failure.
- CPU protections SMEP/SMAP/UMIP/WP enabled; integer overflow now stops the kernel instead of wrapping
  silently; hostile PCI/virtio capability values are rejected instead of panicking; the pairing passkey is
  no longer written to the log; Limine editor disabled; CI actions pinned to commit hashes.

## 0.2.0 — wired networking (not finished)

Boot image: [`KS_OS-v0.2.0/`](KS_OS-v0.2.0/).

- **Networking (S2.P7):** Intel igc (I225/I226, the owner's I225-V) and igb (QEMU) drivers plus virtio-net;
  smoltcp IPv4 stack with DHCP, DNS, ICMP echo and TCP; boot-time gateway ping; status in the UI and the `L`
  log ([ADR-0028](docs/adr/0028-networking-stack.md)). Verified in QEMU (DHCP, ICMP, TCP with a host
  server on virtio-net and igb) and on the host (loss, link flap, hostile rings/frames, fuzzing).
  **Not verified on a real I225-V yet.**
- **Security fix found by fuzzing:** virtqueue chain links no longer trusted from device-shared memory.
- `STRUCTURE.md`: generated platform/structure reference for coding agents ([ADR-0029](docs/adr/0029-structure-document-for-agents.md)).
- Everything from 0.1.0 (Bluetooth bring-up still awaiting the owner's hardware test).

## 0.1.0 — first release (not finished)

Boot image: [`KS_OS-v0.1.0/`](KS_OS-v0.1.0/).

**Works (verified in QEMU):**
- Boots on x86-64 UEFI (Limine), prints `OS operating`, monochrome UI with *Power off* and
  *Boot into UEFI* buttons; keyboard, PS/2 and USB mouse, on-screen kernel log (`L`).
- CPU exception handling with diagnosable fatal screens, guard-paged stacks, kernel-owned W^X page
  tables, heap, interrupts (APIC/IOAPIC/MSI), calibrated clock, PCI, DMA.
- USB (xHCI) with hubs and HID keyboards/mice; ACPI power-off; UEFI runtime services.
- Bluetooth LE host stack (pairing with LE Secure Connections, HID-over-GATT, bond storage) —
  verified against simulated devices and the Bluetooth specification test vectors.

**Licensing:** Apache-2.0 (ADR-0027); the release folder ships `LICENSE`, `NOTICE` and a generated
`THIRD-PARTY-LICENSES.txt`.

**Not verified on real hardware yet:** USB on real controllers, the MediaTek Bluetooth bring-up and
real Bluetooth peripherals. See [docs/hardware-testing.md](docs/hardware-testing.md).

**Not yet implemented:** networking (Intel I225-V driver, TCP/IP), Classic Bluetooth HID, USB hot-plug,
the real UI design (branding guidelines arrive with Phase 8), user mode and everything after Stage 2.
