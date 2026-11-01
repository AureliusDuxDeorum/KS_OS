# ADR-0019: BLE HID first; validation targets from the owner's Linux logs

- **Status:** Accepted
- **Date:** 2026-11-01
- **Deciders:** Project owner, Claude
- **Supplements:** [ADR-0017](0017-bluetooth-first-internal-controller.md), [ADR-0018](0018-mediatek-mt7961-bluetooth-target.md)
- **Stage/Phase:** S2.P6

## Context
The owner's kernel log shows, on the target machine:
- `hci0` (MediaTek MT7961 family): `HW/SW Version: 0x008a008a`, `Build Time: 20240219111427`,
  AOSP extensions v1.00 with quality report, and the quirk "HCI Enhanced Setup Synchronous
  Connection command is advertised, but not supported".
- `hci1` (ASUS USB-BT500): Realtek RTL8761B (`lmp_subver=8761`, `rom_version=1`), firmware
  `rtl_bt/rtl8761bu_fw.bin` + `rtl_bt/rtl8761bu_config.bin` (6-byte config, 30210 bytes total),
  loaded firmware version `0xdfc6d922`.
- The mouse is a **Logitech MX Master 3S** (`fc:34:97:31:b9:6d`) registered as a Bluetooth
  HID device with Logitech HID++ — a **Bluetooth Low Energy** (HID-over-GATT) peripheral.
  The keyboard (Corsair K70 PRO MINI WIRELESS) also offers Bluetooth; its transport (LE vs
  BR/EDR) is still to be confirmed.

## Decision
1. **Order inside S2.P6 changes: LE first.** HCI LE, SMP (LE Secure Connections pairing,
   key distribution), ATT/GATT client, HID-over-GATT (HOGP: HID service, report map, report
   characteristics, notifications, battery service tolerance) come before Classic HIDP.
   Classic BR/EDR HID follows in the same phase.
2. **Peripherals must be re-paired** with KS_OS: bonds live on the old host. The MX Master 3S
   needs an unused Easy-Switch slot in pairing mode. A pairing UI that works with only the
   keyboard (ADR-0017) comes with S2.P8.
3. **Validation targets** (real-hardware serial log must reproduce them):
   MediaTek `HW/SW Version: 0x008a008a`, `Build Time: 20240219111427`; Realtek firmware
   version `0xdfc6d922` (when the dongle is used).
4. Logitech HID++ extensions are **not** implemented; the standard HID reports of the mouse
   are used.
5. Controller quirks observed above (unsupported Enhanced Setup Synchronous Connection) are
   handled by not issuing that command.

## Consequences
LE security (SMP, LE Secure Connections with ECDH P-256, AES-CMAC) is cryptography the kernel
must implement or import: we will use audited `no_std` RustCrypto crates rather than hand-
rolled primitives, and record them in the supply-chain gate.
