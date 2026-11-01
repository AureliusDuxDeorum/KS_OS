# ADR-0018: MediaTek MT7961 (MT7921K) Bluetooth — concrete target and firmware handling

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Supplements:** [ADR-0017](0017-bluetooth-first-internal-controller.md)
- **Stage/Phase:** S2.P6

## Context
The owner's machine carries a MediaTek **MT7921K (RZ608)** Wi-Fi 6E card (PCI `14c3:0608`)
whose Bluetooth function appears on USB as `0e8d:0608`. A second Wi-Fi card, Intel Wireless-AC
9260 (`8086:2526`), exposes no Bluetooth device and is ignored. Available firmware (from
`linux-firmware`, zstd-compressed): `BT_RAM_CODE_MT7961_1_2_hdr.bin.zst` and
`BT_RAM_CODE_MT7961_1a_2_hdr.bin.zst`.

## Decision
- The Bluetooth driver targets the **MT7961 family** over USB (`0e8d:0608`). Wi-Fi for this
  card is out of scope.
- Firmware image selection follows the published behaviour of the Linux `btmtk` driver: the
  chip's device ID, firmware version and flavour registers (read over vendor control requests)
  choose between the `_1_` and `_1a_` images; the choice is **verified against what the owner's
  Linux actually loads** (`sudo dmesg | grep -i btmtk`) before being trusted.
- `cargo xtask image --firmware-dir <dir>` accepts raw or **zstd-compressed** (`.zst`) files,
  decompresses them at image-build time, records their SHA-256 in the build log, and ships
  them to the kernel as Limine modules. Files are never committed. A missing file leaves the
  controller reported as "firmware not supplied", not guessed at.
- Bring-up order for the MT7961: USB control/bulk/interrupt endpoints → vendor "WMT" reset and
  patch download (ROM patch header parsing with strict bounds) → HCI reset and feature
  discovery → normal HCI operation. Each step logs to serial for owner-run diagnostics.

## Consequences
Exact file selection and the vendor command sequence must be validated on the real card; the
QEMU environment cannot emulate it, so S2.P6 includes an owner-run diagnostic image whose
serial log we compare against Linux's behaviour.
