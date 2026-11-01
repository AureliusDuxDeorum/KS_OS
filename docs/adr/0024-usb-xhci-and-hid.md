# ADR-0024: USB stack — xHCI driver, enumeration, hubs, HID, and the on-screen kernel log

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Stage/Phase:** S2.P5 (prerequisite of S2.P6 Bluetooth; ADR-0016/0017)

## Context
Bluetooth controllers are USB devices, so the xHCI driver is on the critical path to the owner's
Bluetooth keyboard and mouse. Cabled/2.4 GHz USB HID devices fall out of the same work.

## Decision
**Controller (`usb/xhci.rs`).** Full xHCI bring-up: BIOS→OS ownership hand-off and SMI disable,
halt/reset, scratchpads, DCBAA, command/event/transfer rings (256 TRBs with link TRBs and cycle
toggling), 32- or 64-byte contexts, 32- or 64-bit DMA (buffers below 4 GiB when `AC64` is clear).
Events use **MSI-X/MSI**; every wait *also* polls the event ring, so correctness never depends on
the interrupt being delivered. One `IrqMutex` guards controller state and is never held while
waiting; completions travel through lock-free mailboxes, and streaming endpoints (HID) get a
per-endpoint handler called from the event path.

**Enumeration (`usb/mod.rs`).** Port power/debounce/reset → Enable Slot → Address Device → read
the first 8 bytes for `bMaxPacketSize0` (Evaluate Context if full-speed changes it) → full device
and configuration descriptors (parsed by the fuzzed `ksparse::usb`) → Set Configuration → class
binding. A device registry lets other drivers (Bluetooth, storage) find their interfaces.

**Hubs (`usb/hub.rs`).** High-speed and slower hubs: hub descriptor, slot context hub fields,
port power/reset/status, route strings (one nibble per tier) and transaction-translator
information for full/low-speed devices behind high-speed hubs. SuperSpeed hubs are reported
unsupported.

**HID (`usb/hid.rs`).** The report descriptor is fetched and parsed by `ksparse::hid` (bounded
against hostile descriptors); incoming reports are decoded against it. Keyboards (modifiers,
key arrays, edge detection) and relative pointers (X/Y/wheel/buttons) publish events into
`input.rs`, which the UI drains alongside PS/2. Absolute pointers (tablets) are recognised and
declined for now.

**PS/2 on USB-only machines.** The i8042 driver now detects an absent controller (status reads
all-ones) and leaves it alone instead of timing out on every command.

**Kernel log on screen.** All console output is mirrored into a 64 KiB ring buffer; the `L` key
opens it full-screen. This exists so hardware bring-up on machines without a serial port (the
owner's) can be diagnosed from a photo of the screen.

## Limits (deliberate, recorded)
- **No hot-plug**: devices present at boot are enumerated; port-change events are only logged.
- No SuperSpeed hubs, isochronous endpoints, streams or SuperSpeed+ lane details; one in-flight
  control transfer per controller; control data limited to 4 KiB; at most 4 controllers / 63 slots.
- DMA is **not IOMMU-protected** (Gate G2 item): a malicious USB controller is trusted.
- Verified on QEMU's xHCI only; validation on real controllers is an owner-run step (below).

## Verification
Host: USB/HID parsers (unit, mutation, noise, hostile descriptors, 4 M-execution fuzz run).
QEMU (no PS/2 controller, so input can only travel the USB path): xHCI init with MSI-X, keyboard,
mouse, hub and a keyboard behind the hub; injected key and mouse events decoded through the report
parser into the input queue; the *real UI* powered off by a USB keypress and by a USB mouse click;
a bulk-only mass-storage transfer (CBW, 512-byte data stage, CSW) over bulk IN/OUT endpoints.

## Owner validation on real hardware
Boot the image on the target machine, press `L` to open the on-screen log, and compare the `usb:`
lines against the devices plugged in (expect your Corsair receiver/keyboard as HID and the
MediaTek and ASUS Bluetooth controllers listed as `class e0` devices). A photo of that screen is
enough to diagnose controller-specific problems.
