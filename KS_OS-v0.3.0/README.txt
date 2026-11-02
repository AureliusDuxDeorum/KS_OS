KS_OS v0.3.0
================

A work-in-progress operating system for x86-64 PCs that boot with UEFI.
Version 0.3.0: the first "0" means "not finished"; the project stays at 0.x.y until the
end goal is reached (see docs/VERSIONING.md).

What is in this folder
----------------------
  KS_OS-v0.3.0.img        the bootable disk image (this is the file you write to a USB stick)
  add-bluetooth-firmware.sh   adds Bluetooth firmware to the image (see below)
  SHA256SUMS                  checksums:  sha256sum -c SHA256SUMS
  LICENSE, NOTICE             the KS_OS licence (Apache-2.0) and its notice
  THIRD-PARTY-LICENSES.txt    licences of every third-party part that is inside the image
  README.txt                  this file

Write it to a USB stick (this ERASES the stick)
-----------------------------------------------
Linux:    lsblk                                  # find the stick, e.g. /dev/sdX
          sudo dd if=KS_OS-v0.3.0.img of=/dev/sdX bs=4M conv=fsync status=progress
macOS:    sudo dd if=KS_OS-v0.3.0.img of=/dev/rdiskN bs=4m
Windows:  Rufus ("DD Image" mode) or balenaEtcher: select the .img file and the stick.

Then boot the PC from the stick (firmware boot menu). Secure Boot must be OFF: the image is not
signed. Nothing in this image writes to your disks.

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
    ./add-bluetooth-firmware.sh KS_OS-v0.3.0.img ~/ksfw

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
