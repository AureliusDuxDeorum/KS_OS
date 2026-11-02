#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Changes the kernel command line stored in a KS_OS boot image, in place, and keeps the hash-pinned
# boot chain valid (the new limine.conf hash is enrolled in the bootloader).
#
#   ./set-cmdline.sh KS_OS-vX.Y.Z.img "iommu=off"     set the command line
#   ./set-cmdline.sh KS_OS-vX.Y.Z.img ""              remove it (the default)
#
# Options the kernel understands:  iommu=off   do not turn on DMA remapping (use if a machine's
#                                              devices stop working with the IOMMU; please report it)
#                                  apic=x2     use x2APIC mode
# If you use Secure Boot, run secure-boot.sh sign AFTER this script.
# Needs: mtools and coreutils (b2sum).   Debian/Ubuntu: sudo apt install mtools
set -euo pipefail
if [ "$#" -ne 2 ]; then sed -n '3,15p' "$0" | sed 's/^# \{0,1\}//'; exit 2; fi
IMG=$1
CMD=$2
for tool in mcopy mtype b2sum dd grep; do
    command -v "$tool" >/dev/null || { echo "error: '$tool' is not installed"; exit 1; }
done
[ -f "$IMG" ] || { echo "error: image '$IMG' not found"; exit 1; }
case "$CMD" in *$'\n'*) echo "error: the command line must be a single line"; exit 1 ;; esac
PART="$IMG@@1M"
export MTOOLS_SKIP_CHECK=1
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

mtype -i "$PART" ::/limine.conf | grep -v '^    cmdline:' > "$TMP/limine.conf"
[ -z "$CMD" ] || printf '    cmdline: %s\n' "$CMD" >> "$TMP/limine.conf"
mcopy -o -i "$PART" "$TMP/limine.conf" ::/limine.conf

mcopy -o -i "$PART" ::/EFI/BOOT/BOOTX64.EFI "$TMP/BOOTX64.EFI"
marker_off=$(grep -abo '++CONFIG_B2SUM_SIGNATURE++' "$TMP/BOOTX64.EFI" | head -n1 | cut -d: -f1)
[ -n "$marker_off" ] || { echo "error: config-hash marker not found in BOOTX64.EFI"; exit 1; }
printf '%s' "$(b2sum "$TMP/limine.conf" | cut -d' ' -f1)" |
    dd of="$TMP/BOOTX64.EFI" bs=1 seek=$((marker_off + 26)) conv=notrunc status=none
mcopy -o -i "$PART" "$TMP/BOOTX64.EFI" ::/EFI/BOOT/BOOTX64.EFI
echo "done: kernel command line of $IMG is now: '${CMD}'"
echo "Write the image to the USB stick again before booting."
