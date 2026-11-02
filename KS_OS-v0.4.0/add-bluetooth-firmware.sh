#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Adds Bluetooth firmware files to a KS_OS boot image, in place. No Rust toolchain needed.
#
#   ./add-bluetooth-firmware.sh KS_OS-vX.Y.Z.img  /path/to/firmware-folder
#
# The folder may contain firmware straight from Linux (e.g. /lib/firmware/mediatek/): files named
# *.bin or *.bin.zst are picked up (zstd files are decompressed first). The firmware is NOT
# redistributed with KS_OS, so it has to come from your own system.
#
# The boot chain is hash-pinned (ADR-0030 S-15): this script pins each firmware file in limine.conf
# and re-enrols the hash of the new limine.conf in the bootloader. If you use Secure Boot, run
# secure-boot.sh sign AFTER this script (changing the bootloader invalidates an earlier signature).
#
# Needs: mtools (mcopy, mtype), zstd and coreutils (b2sum).   Debian/Ubuntu: sudo apt install mtools zstd
set -euo pipefail

if [ "$#" -ne 2 ]; then
    sed -n '3,18p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
fi
IMG=$1
FWDIR=$2

for tool in mcopy mtype mdel mmd zstd sha256sum b2sum dd grep; do
    command -v "$tool" >/dev/null || { echo "error: '$tool' is not installed (sudo apt install mtools zstd)"; exit 1; }
done
[ -f "$IMG" ] || { echo "error: image '$IMG' not found"; exit 1; }
[ -d "$FWDIR" ] || { echo "error: folder '$FWDIR' not found"; exit 1; }

# The EFI System Partition starts at sector 2048 (1 MiB) of the image.
PART="$IMG@@1M"
export MTOOLS_SKIP_CHECK=1

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

count=0
while IFS= read -r -d '' f; do
    base=$(basename "$f")
    case "$base" in
        *.bin.zst) out="${base%.zst}"; zstd -d -q -f "$f" -o "$TMP/$out" ;;
        *.bin)     out="$base";        cp "$f" "$TMP/$out" ;;
        *) continue ;;
    esac
    size=$(stat -c %s "$TMP/$out")
    if [ "$size" -eq 0 ] || [ "$size" -gt 8388608 ]; then
        echo "skipping $out: unreasonable size ($size bytes)"; rm -f "$TMP/$out"; continue
    fi
    echo "firmware: $out ($size bytes, sha256 $(sha256sum "$TMP/$out" | cut -d' ' -f1))"
    count=$((count + 1))
done < <(find "$FWDIR" -maxdepth 3 -type f \( -name '*.bin' -o -name '*.bin.zst' \) -print0 | sort -z)

[ "$count" -gt 0 ] || { echo "error: no *.bin or *.bin.zst files found in '$FWDIR'"; exit 1; }

# Copy the blobs onto the boot partition and rewrite limine.conf with one module line per blob.
mcopy -o -i "$PART" -s "$TMP"/*.bin ::/boot/fw/
mtype -i "$PART" ::/limine.conf | grep -v 'module_path:' > "$TMP/limine.conf"
for f in "$TMP"/*.bin; do
    printf '    module_path: boot():/boot/fw/%s#%s\n' "$(basename "$f")" "$(b2sum "$f" | cut -d' ' -f1)" >> "$TMP/limine.conf"
done
mcopy -o -i "$PART" "$TMP/limine.conf" ::/limine.conf

# Enrol the BLAKE2b of the new limine.conf in BOOTX64.EFI (what `limine enroll-config` does):
# the 128 hex characters that follow the marker are overwritten.
mcopy -o -i "$PART" ::/EFI/BOOT/BOOTX64.EFI "$TMP/BOOTX64.EFI"
marker_off=$(grep -abo '++CONFIG_B2SUM_SIGNATURE++' "$TMP/BOOTX64.EFI" | head -n1 | cut -d: -f1)
[ -n "$marker_off" ] || { echo "error: config-hash marker not found in BOOTX64.EFI"; exit 1; }
printf '%s' "$(b2sum "$TMP/limine.conf" | cut -d' ' -f1)" |
    dd of="$TMP/BOOTX64.EFI" bs=1 seek=$((marker_off + 26)) conv=notrunc status=none
mcopy -o -i "$PART" "$TMP/BOOTX64.EFI" ::/EFI/BOOT/BOOTX64.EFI
echo "boot chain: limine.conf re-pinned and its hash re-enrolled"

echo
echo "done: $count firmware file(s) added to $IMG"
echo "Write the image to a USB stick again before booting (see README.txt)."
