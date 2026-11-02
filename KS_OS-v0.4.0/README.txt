KS_OS v0.4.0
================

A work-in-progress operating system for x86-64 PCs that boot with UEFI.
Version 0.4.0: the first "0" means "not finished"; the project stays at 0.x.y until the
end goal is reached (see docs/VERSIONING.md).

What is in this folder
----------------------
  KS_OS-v0.4.0.img        the bootable disk image (this is the file you write to a USB stick)
  add-bluetooth-firmware.sh   adds Bluetooth firmware to the image (see below)
  secure-boot.sh              optional: UEFI Secure Boot with your own keys (see below)
  set-cmdline.sh              optional: change the kernel options stored in the image (e.g. iommu=off)
  SHA256SUMS                  checksums:  sha256sum -c SHA256SUMS
  LICENSE, NOTICE             the KS_OS licence (Apache-2.0) and its notice
  THIRD-PARTY-LICENSES.txt    licences of every third-party part that is inside the image
  README.txt                  this file

Write it to a USB stick (this ERASES the stick)
-----------------------------------------------
Linux:    lsblk                                  # find the stick, e.g. /dev/sdX
          sudo dd if=KS_OS-v0.4.0.img of=/dev/sdX bs=4M conv=fsync status=progress
macOS:    sudo dd if=KS_OS-v0.4.0.img of=/dev/rdiskN bs=4m
Windows:  Rufus ("DD Image" mode) or balenaEtcher: select the .img file and the stick.

Then boot the PC from the stick (firmware boot menu). Secure Boot must be OFF unless you sign the
image yourself (next section). Nothing in this image writes to your disks.

Tamper protection and Secure Boot
---------------------------------
The kernel, the firmware files and the boot configuration are pinned by hash: the bootloader refuses to
start (and says so on screen) if any of them was changed on the stick. That protects against modified
sticks even with Secure Boot off.

To also keep the bootloader itself from being replaced, use Secure Boot with YOUR OWN keys (the private keys
never leave your PC, nobody else can sign images for it):

    ./secure-boot.sh keys                          # once; creates ~/.ks_os-secureboot
    ./add-bluetooth-firmware.sh KS_OS-v0.4.0.img ~/ksfw     # if you use Bluetooth (BEFORE signing)
    ./secure-boot.sh sign KS_OS-v0.4.0.img     # signs the bootloader inside the image

Then, once, in your PC's firmware setup: put Secure Boot into Setup Mode and enrol db.auth, KEK.auth and
PK.auth from ~/.ks_os-secureboot (names and menus differ per vendor; some want the .cer files).
Back up the *.key files. If you lose them, switch Secure Boot off again to boot anything unsigned.

DMA protection (IOMMU)
----------------------
The kernel turns on the IOMMU (Intel VT-d / AMD-Vi) when the firmware has it enabled, so a device can only
reach memory its driver gave it. If a machine misbehaves with it (devices stop working), run
    ./set-cmdline.sh KS_OS-v0.4.0.img "iommu=off"
write the image again, and send back the log (press L): lines starting with "iommu:".

What you will see
-----------------
"OS operating", then two buttons: Power off and Boot into UEFI.
Keys:  Tab / arrow keys + Enter,   P = power off,   U = UEFI setup,   L = show the kernel log.
Mouse: click the buttons.
If no input device works at all, the screen shows a live log instead (it refreshes each second).

Bluetooth keyboard / mouse
--------------------------
The MediaTek Bluetooth controller needs firmware files that cannot be redistributed. Copy them
from a Linux install (they are in /lib/firmware/mediatek/):

    mkdir ~/ksfw
    cp /lib/firmware/mediatek/BT_RAM_CODE_MT7961_1_2_hdr.bin.zst  ~/ksfw/
    cp /lib/firmware/mediatek/BT_RAM_CODE_MT7961_1a_2_hdr.bin.zst ~/ksfw/
    ./add-bluetooth-firmware.sh KS_OS-v0.4.0.img ~/ksfw

and write the image to the stick again. Put your keyboard / mouse into pairing mode while the PC
is scanning (the status line says so). Pairing keys are remembered between boots.

Bluetooth support in this version is EXPERIMENTAL and untested on real hardware.

What to send back if something does not work
--------------------------------------------
Press L (or let the diagnostic screen run) and photograph the log. The lines starting with
"usb:" and "bt:" are the interesting ones.

Licence
-------
KS_OS is licensed under the Apache License 2.0 (see LICENSE and NOTICE). The names "KS" and "KS_OS" and
the KS logos are not covered by that licence. Third-party components: THIRD-PARTY-LICENSES.txt.
