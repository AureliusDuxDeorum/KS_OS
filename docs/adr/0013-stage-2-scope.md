# ADR-0013: Stage 2 scope — Bluetooth input and wired LAN internet

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Stage/Phase:** S2

## Context
The project owner's keyboard and mouse are Bluetooth-only, so the Stage 1 buttons
(PS/2 only, ADR-0011, risk R6) cannot be operated on the target machine. Stage 2 is
defined as **Bluetooth support and LAN internet support**. The target hardware:

- Ethernet: Intel I225-V (`8086:15f3`, `igc` family).
- Bluetooth controller: a USB-attached Realtek / MediaTek / Broadcom / Qualcomm part
  (exact USB ID still to be supplied).
- Peripherals: both **Classic Bluetooth HID** and **BLE HID (HOGP)** are in scope.

Neither feature can be built on the Stage 1 kernel, which has no IDT, interrupts,
memory manager, PCI or USB. These foundations become the first phases of Stage 2.

## Decision
Stage 2 delivers, in this order (details in `docs/ROADMAP.md`):
1. **Exceptions and CPU state** (GDT/TSS/IST, IDT, guard-paged stacks) — resolves G1 risks R2, R3.
2. **Memory management** (frame allocator, kernel-owned page tables, heap, DMA allocator).
3. **Interrupts and time** (LAPIC/IOAPIC, timer, MSI/MSI-X, interrupt-driven input).
4. **ACPI and PCI** — table parsers in a host-testable, fuzzed crate (resolves R9),
   PCIe ECAM enumeration, AML interpreter decision (resolves R7).
5. **USB xHCI** with HID boot-protocol keyboards and mice (also closes R6 for wired devices).
6. **Bluetooth**: USB HCI transport, vendor firmware loading, L2CAP/SDP, Secure
   Simple Pairing, Classic HID (HIDP), then LE: SMP, ATT/GATT, HOGP; bonds, then a
   unified input-event layer feeding the UI.
7. **Networking**: Intel `igc` driver (I225-V), Ethernet, `smoltcp`-based IP stack,
   DHCP, DNS, ICMP and TCP; virtio-net and e1000e for CI.
8. **Integration**: UI shows Bluetooth and network status and a pairing screen; tests and a
   hardware-in-the-loop procedure.

Rules specific to this stage:
- **Vendor Bluetooth firmware is never committed.** It is supplied at image-build time
  from a user-provided directory (`cargo xtask image --firmware-dir …`) and loaded as a
  Limine module; licences are recorded per blob. Without firmware the controller is
  reported as unsupported rather than guessed at.
- **Drivers do not trust devices.** All descriptors, HCI events, L2CAP/ATT lengths and
  NIC descriptors are parsed with explicit bounds checks; parsers live in `no_std`
  crates that are unit-tested and fuzzed on the host.
- **DMA is isolated where hardware allows**: bus-master capable devices are enabled only
  after their buffers exist; VT-d/IOMMU enforcement is a Stage 2 gate item.
- **Test strategy:** QEMU covers xHCI, virtio-net, e1000e and (via a virtual HCI
  controller) Bluetooth host logic; `igc` and the real Bluetooth chip can only be verified
  on the owner's hardware, so each driver phase ships a serial-logged self-test the owner
  runs from the produced USB image and reports back.

## Gate G1 waiver
ADR-0002 forbids starting a stage before the previous gate closes or is waived. The
project owner has directed Stage 2 to start now. This ADR records a **waiver of the
remaining G1 items**, each rescheduled into Stage 2: R3 → phase 1, R8 → first push of the
workflow, R9 → phase 4. G1 stays **OPEN** in `docs/hardening/stage-1.md` until those are
done, and **Gate G2 cannot close while any of them is open.**

## Alternatives considered
- USB legacy keyboard emulation / UEFI input protocols to bridge Stage 1: unavailable
  once the bootloader exits boot services, and does not help Bluetooth.
- Wi-Fi instead of LAN: rejected by the owner; Ethernet only.
- Writing a TCP/IP stack from scratch: large, security-sensitive and not the project's
  differentiator; `smoltcp` is audited by use and `no_std`.

## Consequences
A very large stage; the foundational phases dominate the schedule. The Bluetooth stack
(HCI → pairing → HID) is the main risk because it depends on vendor firmware and on
hardware we can only test remotely. Phase boundaries are chosen so each one is
independently demonstrable and testable.
