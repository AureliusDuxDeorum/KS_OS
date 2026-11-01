# Hardware test guide

How to test KS_OS on the owner's machine and what to send back. Nothing here needs a serial port.

## 1. Build the image with the Bluetooth firmware
Copy the firmware files from Linux into one folder (they are never committed):

```bash
mkdir -p ~/ksfw
cp /lib/firmware/mediatek/BT_RAM_CODE_MT7961_1_2_hdr.bin.zst ~/ksfw/
cp /lib/firmware/mediatek/BT_RAM_CODE_MT7961_1a_2_hdr.bin.zst ~/ksfw/
```

On the development machine (Rust via `rustup`, `git`):

```bash
cargo xtask image --firmware-dir ~/ksfw
```

The build prints each file's size and SHA-256. Write `target/ks_os.img` to a USB stick (**this
erases the stick**; replace `/dev/sdX` after checking with `lsblk`):

```bash
sudo dd if=target/ks_os.img of=/dev/sdX bs=4M conv=fsync status=progress
```

Boot it from the firmware boot menu. Secure Boot must be off (the image is not signed).

## 2. What you should see
- `OS operating`, then the two buttons (Power off / Boot into UEFI).
- Within about 10–20 seconds the status line reports Bluetooth progress, e.g. *"Scanning for Bluetooth
  keyboards and mice (put yours in pairing mode)"*.
- If **no input device works at all**, the screen automatically shows a live kernel log instead.
  It refreshes every second and returns to the buttons as soon as any input device works.

## 3. Pairing
Put a device into pairing mode while the machine is scanning (scans repeat every ~30 s):
- **Logitech MX Master 3S:** press and hold an Easy-Switch button until its LED blinks quickly.
  Use a slot that is *not* currently paired to your Linux machine.
- **Corsair K70 Mini:** switch it to Bluetooth mode and hold the pairing key combination until it
  blinks (see its manual).

If the keyboard asks for a passkey, the screen shows *"Pairing: type NNNNNN on the keyboard, then
press Enter"*. After pairing, the bond is saved in firmware storage, so later boots reconnect on their
own without pairing mode.

## 4. What to send back
Press `L` (if you have any working input) or let the diagnostic screen run, then photograph the log
screen. The lines that matter start with `usb:` and `bt:`. In particular:
- `usb: ... 0e8d:0608 ... class e0` — the MediaTek controller was found on USB.
- `bt: MediaTek chip id ...`, `bt: firmware file wanted: ...`, `bt: firmware ... HW/SW Version:
  0x008a008a, Build Time: 20240219111427` — expected values from your Linux log.
- `bt: section N announce -> ...` and `bt: enable Bluetooth -> status ...` — the firmware download.
- `bt: controller ready: ...` — Bluetooth is up. After that: scanning, connecting, pairing lines.

If something fails, the status line and the log say which step; the photo is enough to fix it.

## 5. Safe fallbacks
- Nothing in this image writes to disks. The only persistent write is the Bluetooth bond variable
  `KSOS-BT-BONDS` in firmware NV storage (removable with any UEFI variable editor).
- Power off with the `P` key or the button; the machine's own power button always works.
