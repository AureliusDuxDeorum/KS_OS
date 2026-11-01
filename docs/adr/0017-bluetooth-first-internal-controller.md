# ADR-0017: Bluetooth is the primary input path; internal MediaTek controller first

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Supersedes in part:** [ADR-0016](0016-target-hardware-inventory.md) (decisions 1 and 2 ordering)
- **Stage/Phase:** S2.P5–S2.P6

## Context
The owner reports physical damage affecting USB ports and prefers to rely on Bluetooth
for input. The internal MediaTek Wi-Fi/Bluetooth card (`0e8d:0608`) is wired to the
motherboard's internal USB link, so it does not depend on the external ports; the ASUS
USB-BT500 dongle (`0b05:190e`) does.

## Decision
1. **Primary target: the internal MediaTek controller.** The ASUS dongle becomes a
   secondary, optional target (useful as a second controller and for Realtek coverage).
2. **xHCI remains mandatory** (S2.P5): Bluetooth controllers are USB devices. The port-facing
   USB HID path is *not* a goal for the owner's machine, but the HID report-descriptor
   parser is still built once and shared with Bluetooth HID (Classic HIDP and BLE HOGP),
   so USB keyboards/mice come essentially for free and are kept.
3. **Bluetooth critical path:** xHCI → USB HCI transport → MediaTek firmware download
   (vendor WMT commands) → HCI init → L2CAP/SDP → pairing (SSP, SMP) → HID (Classic, then LE)
   → unified input events → UI. Until this path is complete the Stage 1 buttons cannot be
   operated on the owner's machine; this is accepted.
4. **Pairing UX** must work with only the keyboard (passkey typed on the keyboard being
   paired), because there may be no other working input device.
5. **Firmware:** the exact MediaTek firmware file is taken from what the owner's Linux
   logs for `0e8d:0608` and from the Wi-Fi chip identified by `lspci`; it is supplied at
   image-build time and never committed (ADR-0013).

## Consequences
The MediaTek firmware/WMT protocol (more involved than Realtek's) moves onto the critical
path, and no real-hardware input is possible before S2.P6 completes; phases P1–P5 are
verified on QEMU plus owner-run serial self-tests on the target machine.
