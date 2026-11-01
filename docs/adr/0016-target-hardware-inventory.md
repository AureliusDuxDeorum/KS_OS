# ADR-0016: Target hardware inventory and input/Bluetooth priorities

- **Status:** Accepted; ordering of decisions 1–2 superseded in part by [ADR-0017](0017-bluetooth-first-internal-controller.md)
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Supplements:** [ADR-0013](0013-stage-2-scope.md)
- **Stage/Phase:** S2.P5–S2.P7

## Context
The owner supplied `lsusb` output for the target machine. Relevant devices:

| USB ID | Device | Role for KS_OS |
| --- | --- | --- |
| `1b1c:1ba6` | Corsair SLIPSTREAM WIRELESS USB Receiver | 2.4 GHz receiver, exposes **USB HID** keyboard/mouse — works without Bluetooth |
| `1b1c:1bb6` | Corsair K70 RGB PRO MINI wireless keyboard (USB mode) | **USB HID** keyboard when cabled |
| `0e8d:0608` | MediaTek "Wireless_Device" (internal Wi-Fi/BT combo card) | Bluetooth controller #1 (internal) |
| `0b05:190e` | ASUS USB-BT500 dongle | Bluetooth controller #2 (believed Realtek RTL8761B; to be confirmed from the firmware name Linux loads) |
| `05e3:0608` | Genesys Logic USB 2.0 hub | Hub behind which RGB/fan controllers sit; not needed for input |
| `0b05:1939`, `1b1c:0c1c`, `041e:40a0` | ASUS AURA, Corsair iCUE Commander CORE, Creative webcam | Out of scope |
| Intel I225-V (`8086:15f3`, PCI) | Ethernet | NIC target (ADR-0013) |

Both the keyboard and the receiver hang directly off root hub 003, so no hub support is
needed to reach them. The owner described the peripherals as "Bluetooth-only"; the
hardware also offers USB HID paths that need no Bluetooth stack.

## Decision
1. **Input reaches the buttons via xHCI + USB HID first.** S2.P5 (xHCI) must include a
   USB HID class driver with a **full HID report-descriptor parser** (not just the boot
   protocol), because vendor keyboards/receivers commonly expose several HID interfaces.
   Once S2.P5 lands, the Stage 1 buttons are operable on the owner's machine via the
   Corsair receiver or the cabled keyboard, independent of Bluetooth.
2. **Bluetooth targets, in order:** (a) the ASUS USB-BT500 dongle — plain USB device, simple
   vendor firmware download; (b) the internal MediaTek controller — WMT-style firmware
   download via vendor commands. The USB HCI transport and everything above it
   (L2CAP, SDP, SMP/SSP, HID/HOGP) is controller-independent; only the firmware loader differs.
3. **Firmware files** are supplied by the owner from `linux-firmware` (`rtl_bt/…`,
   `mediatek/…`) at image-build time (ADR-0013); exact filenames are taken from what the
   owner's Linux logs as loaded, not guessed.
4. The Corsair 2.4 GHz link is treated as USB HID only; no attempt is made to implement the
   proprietary radio protocol.

## Consequences
- Bluetooth is no longer on the critical path to a usable UI on the owner's machine; it
  remains a Stage 2 goal.
- HID parsing becomes a security-relevant parser (untrusted device input): it is written as
  a `no_std` host-testable crate and fuzzed, consistent with the gate rules.
