# ADR-0025: Bluetooth host stack — design, security decisions and verification boundary

- **Status:** Accepted
- **Date:** 2026-11-02
- **Deciders:** Project owner, Claude
- **Supplements:** [ADR-0017](0017-bluetooth-first-internal-controller.md), [ADR-0018](0018-mediatek-mt7961-bluetooth-target.md), [ADR-0019](0019-ble-hid-first-and-validation-targets.md)
- **Stage/Phase:** S2.P6

## Context
Bluetooth is the owner's only dependable input path (ADR-0017). It is also a large attack surface:
every byte from the radio — advertisements, ATT responses, SMP PDUs — comes from a peer we do not
trust, and pairing is where keys are created.

## Decision
**Structure.** `crates/ksbt` (`no_std`, `forbid(unsafe_code)`) holds everything that can be pure:
HCI codec, L2CAP reassembly, ATT/GATT client, SMP, HID-over-GATT, bond storage, USB framing and the
MediaTek codec. It talks to the world through three small traits (`Transport`, `Rng`, `PairingUi`),
so the entire stack runs on the host against a simulated controller and BLE keyboard/mouse. The
kernel (`kernel/src/bt/`) supplies the USB transport, the MediaTek bring-up and the service loop.

**Synchronous host.** Calls pump the transport until the awaited response or a timeout. This keeps
the protocol code linear and testable; the kernel runs it in short slices from the UI idle loop
(scan windows of 250 ms; connect/pair/encrypt block for up to a few seconds). A real scheduler
(Stage 3) turns this into a task.

**Security decisions.**
- **LE Secure Connections only.** Legacy pairing (weak, passively crackable) is refused.
- P-256 peer public keys are validated on the curve; a reflected key (peer key = ours) is refused;
  a peer offering < 16-byte keys is refused (no KNOB-style downgrade).
- All cryptography comes from audited RustCrypto crates; f4/f5/f6/g2/ah and ECDH are checked against
  the Core Specification Appendix D sample data, AES-CMAC against RFC 4493.
- Random numbers come **only** from RDSEED/RDRAND; with neither, Bluetooth is disabled rather than
  falling back to a weak source.
- Our IO capability is **DisplayOnly**: keyboards that can type digits get *Passkey Entry* (the six
  digits are shown on screen — authenticated, MITM-protected); devices without input get *Just Works*
  (unauthenticated; logged as such). Numeric Comparison exists in the stack but is not offered until
  the OS has a way to ask the user.
- Bonds are stored in a CRC-protected, versioned record in a UEFI NV variable (`KSOS-BT-BONDS`);
  Debug output never prints key material; keys are zeroised on drop.

**Known security gaps (recorded for Gate G2).**
1. **Bonds are stored unencrypted** in firmware NV storage: anything that can read UEFI variables
   (another OS, a firmware tool) can read the LTK. Encrypting them needs a platform secret (TPM).
2. **Pairing authorisation:** the kernel pairs automatically with any *discoverable HID device in
   range* during scan windows, because it may have no other input device to ask. A hostile device
   could therefore pair as a keyboard. Needed before the OS handles anything sensitive: restrict
   pairing to a time window after boot or require confirmation from an already-trusted input device.
3. Just Works pairs are open to an active man-in-the-middle during pairing.
4. No BR/EDR (Classic) HID and no legacy-pairing fallback yet; one controller at a time.

**MediaTek bring-up (`bt/mediatek.rs`) is experimental and unverified on hardware.** It identifies
the chip by vendor register reads, selects the firmware file as Linux does, validates the patch file,
announces and downloads sections through WMT, and enables the Bluetooth function. Every step is logged,
and nothing is written to the controller before chip identity and firmware have been validated.
The expected validation values from the owner's Linux log (`HW/SW Version: 0x008a008a`, build time
`20240219111427`) are parsed from the firmware file and printed, so the owner can check the file
that was shipped even before the controller accepts it.

## Verification boundary (what is and is not tested)
- **Verified on the host:** crypto vectors; the full pairing → encryption → HID flow for Just Works,
  Passkey Entry and Numeric Comparison against an independently written SMP responder; reconnection
  from a serialised bond; wrong passkey / wrong key / off-curve key / reflected key / legacy-only /
  weak key / hostile ATT server / garbage packets; USB packet reassembly; MediaTek codec on synthetic
  data; 880 k libFuzzer executions with GATT, HOGP and SMP driven by hostile peers.
- **Verified in QEMU:** firmware blobs reach the kernel as boot modules; the RNG; graceful behaviour
  with no controller; the diagnostic log screen. QEMU 8.2 has no Bluetooth controller model.
- **Not verifiable here:** the USB HCI transport against a real controller, the MediaTek vendor
  protocol, radio timing, real peripherals' quirks (the MX Master 3S, the K70 Mini). These are the
  subject of the owner's hardware test (`docs/hardware-testing.md`).

## Consequences
The first real-hardware run is expected to need at least one iteration on the MediaTek sequence;
the on-screen log and the automatic diagnostic view exist to make that iteration fast.
