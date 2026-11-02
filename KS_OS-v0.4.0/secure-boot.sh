#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# UEFI Secure Boot for KS_OS with YOUR OWN keys. The private keys never leave your machine.
#
#   ./secure-boot.sh keys [KEYDIR]             create Platform/KEK/db keys (default ~/.ks_os-secureboot)
#   ./secure-boot.sh sign KS_OS-vX.Y.Z.img [KEYDIR]   sign the bootloader inside the image, in place
#
# One-time setup in your firmware (names differ per vendor): enter setup, put Secure Boot into
# "Setup Mode" (clear/delete the Platform Key), then enrol, in this order, the files from KEYDIR:
#   db.auth (or db.cer), KEK.auth (or KEK.cer), and last PK.auth (or PK.cer). Re-enable Secure Boot.
# After that only images signed with your db key will start; the signed bootloader in turn refuses
# a kernel, firmware file or configuration that was changed (hash-pinned, see README.txt).
#
# If you add Bluetooth firmware with add-bluetooth-firmware.sh, do that BEFORE signing.
# Needs: openssl, sbsigntool (sbsign, sbverify), efitools (cert-to-efi-sig-list, sign-efi-sig-list), mtools.
#   Debian/Ubuntu: sudo apt install openssl sbsigntool efitools mtools
set -euo pipefail

usage() { sed -n '3,15p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
need() { for t in "$@"; do command -v "$t" >/dev/null || { echo "error: '$t' is not installed (see the header of this script)"; exit 1; }; done; }
newguid() { cat /proc/sys/kernel/random/uuid; }

cmd=${1:-}
case "$cmd" in
keys)
    need openssl cert-to-efi-sig-list sign-efi-sig-list
    KEYDIR=${2:-$HOME/.ks_os-secureboot}
    if [ -e "$KEYDIR/db.key" ]; then
        echo "error: keys already exist in $KEYDIR (refusing to overwrite them)"; exit 1
    fi
    umask 077
    mkdir -p "$KEYDIR"
    cd "$KEYDIR"
    GUID=$(newguid)
    echo "$GUID" > owner-guid.txt
    for name in PK KEK db; do
        openssl req -new -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
            -subj "/CN=KS_OS $name/" -keyout "$name.key" -out "$name.crt" 2>/dev/null
        openssl x509 -in "$name.crt" -outform DER -out "$name.cer"
        cert-to-efi-sig-list -g "$GUID" "$name.crt" "$name.esl"
    done
    sign-efi-sig-list -k PK.key  -c PK.crt  PK  PK.esl  PK.auth
    sign-efi-sig-list -k PK.key  -c PK.crt  KEK KEK.esl KEK.auth
    sign-efi-sig-list -k KEK.key -c KEK.crt db  db.esl  db.auth
    chmod 600 ./*.key
    echo "keys written to $KEYDIR (back up the *.key files; keep them private)"
    echo "firmware enrolment files: db.auth KEK.auth PK.auth  (or the .cer files)"
    ;;
sign)
    need sbsign sbverify mcopy
    IMG=${2:-}
    [ -f "$IMG" ] || usage
    KEYDIR=${3:-$HOME/.ks_os-secureboot}
    [ -f "$KEYDIR/db.key" ] || { echo "error: no keys in $KEYDIR - run: $0 keys"; exit 1; }
    PART="$IMG@@1M"
    export MTOOLS_SKIP_CHECK=1
    TMP=$(mktemp -d)
    trap 'rm -rf "$TMP"' EXIT
    mcopy -o -i "$PART" ::/EFI/BOOT/BOOTX64.EFI "$TMP/BOOTX64.EFI"
    if sbverify --list "$TMP/BOOTX64.EFI" 2>/dev/null | grep -q 'signature'; then
        echo "error: the bootloader in $IMG is already signed (sign a fresh image, or after add-bluetooth-firmware.sh)"; exit 1
    fi
    sbsign --key "$KEYDIR/db.key" --cert "$KEYDIR/db.crt" --output "$TMP/BOOTX64.signed.EFI" "$TMP/BOOTX64.EFI"
    mcopy -o -i "$PART" "$TMP/BOOTX64.signed.EFI" ::/EFI/BOOT/BOOTX64.EFI
    sbverify --cert "$KEYDIR/db.crt" "$TMP/BOOTX64.signed.EFI"
    echo "done: $IMG is signed with your db key"
    ;;
*) usage ;;
esac
