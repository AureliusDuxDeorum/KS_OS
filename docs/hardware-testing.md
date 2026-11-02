# Hardware test guide

How to test KS_OS on the owner's machine and what to send back. Nothing here needs a serial port.

## 1. Get the image (no build tools needed)
The ready-made boot image lives in the repository's top-level folder **`KS_OS-v0.3.0/`**, with its
own `README.txt`. Add the Bluetooth firmware (copied from your Linux install; it is not redistributed)
with the helper script in the same folder:

```bash
mkdir -p ~/ksfw
cp /lib/firmware/mediatek/BT_RAM_CODE_MT7961_1_2_hdr.bin.zst  ~/ksfw/
cp /lib/firmware/mediatek/BT_RAM_CODE_MT7961_1a_2_hdr.bin.zst ~/ksfw/
sudo apt install mtools zstd            # once
cd KS_OS-v0.3.0
./add-bluetooth-firmware.sh KS_OS-v0.3.0.img ~/ksfw
```

Then write the image to a USB stick (**this erases the stick**; check the device with `lsblk`):

```bash
sudo dd if=KS_OS-v0.3.0.img of=/dev/sdX bs=4M conv=fsync status=progress
```

Boot it from the firmware boot menu. Secure Boot must be off (the image is not signed).

(Developers can instead build their own with `cargo xtask image --firmware-dir ~/ksfw`.)

## 2. What you should see
- `OS operating`, then the two buttons (Power off / Boot into UEFI).
- Within about 10–20 seconds the status line reports Bluetooth progress, e.g. *"Scanning for Bluetooth
  keyboards and mice (put yours in pairing mode)"*.
- If **no input device works at all**, the screen automatically shows a live kernel log instead.
  It refreshes every second and returns to the buttons as soon as any input device works.

## 3. Pairing
**Keyboards must pair with a passkey** (since 0.3.0). If your keyboard only offers "Just Works", the status line
says `pairing refused: ... only offers Just Works - press J to allow it for this pairing window`. Pressing **J**
(on any working input device) allows it once for the current window; it is logged as unauthenticated.
Mice may use Just Works.
Since 0.2.1 the machine only pairs *new* devices during a pairing window. With no other keyboard or mouse
connected (your situation) it opens by itself for 120 s after boot; if you miss it, press **B** on any
working input device (or reboot). Already-paired devices reconnect on their own at any time.

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

## 4b. Network (Intel I225-V)
Plug the LAN cable in **before** powering on. Look for these `net:` lines on the log screen (`L`):
- `net: Intel I225-V/I226-V (igc) ready, mac …; link 1000 Mb/s full duplex` — the NIC was found, reset and
  its link negotiated (2500 Mb/s if your switch/router supports it). `no link` at first is normal; it
  switches to `link up` within a few seconds.
- `net: phy BMCR=… BMSR=…` — the PHY state; `net: phy not reachable over MDIO` would be a bug to report.
- `net: …: 192.168.x.y/24 gateway … dns …` — DHCP worked.
- `net: … interrupts via MSI-X, vector …` — receive is interrupt-driven (`polled mode` means MSI-X failed; still works).
- `net: …: IPv6 address …` — SLAAC worked (needs an IPv6-capable router; absence is not an error).
- `net: …: gateway … reachable (N ms)` — ICMP echo to the router worked: the whole path
  (NIC → stack → router) is verified. `does not answer ping` means the router ignores ping (not necessarily a fault).
If the NIC is not listed at all, send the `pci:` line containing `8086:15f3`.

## 5. Safe fallbacks
- Nothing in this image writes to disks. The only persistent write is the Bluetooth bond variable
  `KSOS-BT-BONDS` in firmware NV storage (removable with any UEFI variable editor).
- Power off with the `P` key or the button; the machine's own power button always works.
