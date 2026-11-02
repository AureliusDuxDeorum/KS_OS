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
# Needs: mtools (mcopy, mtype) and zstd.   Debian/Ubuntu: sudo apt install mtools zstd
set -euo pipefail

if [ "$#" -ne 2 ]; then
    sed -n '3,12p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
fi
IMG=$1
FWDIR=$2

for tool in mcopy mtype mdel mmd zstd sha256sum; do
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
    printf '    module_path: boot():/boot/fw/%s\n' "$(basename "$f")" >> "$TMP/limine.conf"
done
mcopy -o -i "$PART" "$TMP/limine.conf" ::/limine.conf

echo
echo "done: $count firmware file(s) added to $IMG"
echo "Write the image to a USB stick again before booting (see README.txt)."
