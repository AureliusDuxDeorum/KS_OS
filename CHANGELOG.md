# Changelog

Versions follow `X.Y.Z` as defined in [docs/VERSIONING.md](docs/VERSIONING.md): `X` = finished (0 = not),
`Y` = version, `Z` = bug patch.

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
