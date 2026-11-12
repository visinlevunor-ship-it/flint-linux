#!/bin/sh
# Download the Linux kernel, its modules and the device firmware Flint uses (Ubuntu's
# packages: a well-tested kernel with drivers for nearly every PC), check them against
# known SHA-256 sums and unpack them.
#
#   image/fetch-kernel.sh [DIR]      (default build/linux)
#
# Afterwards DIR/kernel holds boot/vmlinuz-VERSION and lib/modules/VERSION, and
# DIR/firmware/lib/firmware the firmware. image/build-disk.sh reads both.
#
# Needs: fetch, curl or wget; sha256 or sha256sum; tar that reads .deb/.tar.zst
# (FreeBSD's bsdtar does; elsewhere ar + GNU tar with zstd).
#
# POSIX sh only: runs on FreeBSD and Linux hosts.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIR=${1:-$ROOT/build/linux}
MIRROR=${MIRROR:-http://archive.ubuntu.com/ubuntu}
# Packages leave the mirror once newer ones replace them; Launchpad keeps every file.
FALLBACK=https://launchpad.net/ubuntu/+archive/primary/+files

KVER=7.0.0-42-generic
# SHA-256  path in the archive
PACKAGES="
25f708dd38682e77012703983d2b0cbfa7e6fa279ece8d9d69d8a092a0000c3f pool/main/l/linux-hwe-7.0/linux-image-unsigned-7.0.0-42-generic_7.0.0-42.42~24.04.1_amd64.deb kernel
c59cd39101398113ca84a813f43c21058f17a2fc0810ccb7f0fd15c804ffd3a0 pool/main/l/linux-hwe-7.0/linux-modules-7.0.0-42-generic_7.0.0-42.42~24.04.1_amd64.deb kernel
dfe9774a008185a15578e381a5a2a642b44734af47dabbe7f45096f6802185c7 pool/main/l/linux-main-signed-hwe-7.0/linux-main-modules-iwlwifi-7.0.0-42-generic_7.0.0-42.42~24.04.1_amd64.deb kernel
e4ee541d99f009f1225100fead946907afe3ad9b614fd5707f7363d7a9c03d39 pool/main/w/wireless-regdb/wireless-regdb_2026.05.30-0ubuntu1~24.04.1_all.deb firmware
c69233da5e4927a8a9e1c2e963fd52c5f5cb2e14c4823d68cba98e59cab509e9 pool/main/l/linux-firmware-broadcom-wireless/linux-firmware-broadcom-wireless_20240318.git3b128b60-0ubuntu3.1_all.deb firmware
07905796399e96d1116545f60c0fde6d979e55301422f9fecafb98cff1e6ab7e pool/main/l/linux-firmware-intel-misc/linux-firmware-intel-misc_20240318.git3b128b60-0ubuntu3.1_all.deb firmware
01ee15acefe58449c33371621cf457d510ec6ac87e90609f0ba19ed78d398060 pool/main/l/linux-firmware-intel-wireless/linux-firmware-intel-wireless_20240318.git3b128b60-0ubuntu3.1_all.deb firmware
36825d20cb9bbc6818bc82129ba085859947c090f09069a08ab69801fcdf14de pool/main/l/linux-firmware-marvell-wireless/linux-firmware-marvell-wireless_20240318.git3b128b60-0ubuntu3.1_all.deb firmware
473ea1b5d5ad4c276d618d90f24210cce85a9daf8e626fb5bcd130c990f6e257 pool/main/l/linux-firmware-mediatek/linux-firmware-mediatek_20240318.git3b128b60-0ubuntu3.1_all.deb firmware
284873f9312a9a267d96213cfab229c1a6e3924a6149c3b0fdc1a42a853e7b22 pool/main/l/linux-firmware-misc/linux-firmware-misc_20240318.git3b128b60-0ubuntu3.1_all.deb firmware
f333f7c1525949720b87e67dfee80e129ffff33d61fa97050428cd720392563c pool/main/l/linux-firmware-qualcomm-wireless/linux-firmware-qualcomm-wireless_20240318.git3b128b60-0ubuntu3.1_all.deb firmware
93b11d4f7d086727f6d0fa980c4ee8439dbad50305e8958ca3f71be272f64833 pool/main/l/linux-firmware-realtek/linux-firmware-realtek_20240318.git3b128b60-0ubuntu3.1_all.deb firmware
1d6745350c99dd4b54f576994def3dee1c2e6752762731022df7ee7af39b3b55 pool/main/l/linux-firmware-amd-graphics/linux-firmware-amd-graphics_20261110.git69e51bb2-0ubuntu1_all.deb firmware
f417c24bcf4d0e8d2f4a4d696ab14f8bffa8ffc9870b31f3d63aea0b73cdfa8b pool/main/l/linux-firmware-intel-graphics/linux-firmware-intel-graphics_20261110.git69e51bb2-0ubuntu1_all.deb firmware
"

download() { # URL OUT
    if command -v fetch >/dev/null 2>&1; then
        fetch -q -o "$2" "$1"
    elif command -v curl >/dev/null 2>&1; then
        curl -fsSL -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$2" "$1"
    else
        echo "need fetch, curl or wget" >&2
        exit 1
    fi
}

sha256_of() {
    if command -v sha256 >/dev/null 2>&1; then
        sha256 -q "$1"
    else
        sha256sum "$1" | cut -d' ' -f1
    fi
}

# Unpack a .deb's data archive into a directory.
unpack_deb() { # DEB DEST
    work=$(mktemp -d "${TMPDIR:-/tmp}/flint-deb.XXXXXX")
    if tar -tf "$1" >/dev/null 2>&1; then
        (cd "$work" && tar -xf "$1")          # bsdtar reads ar archives
    else
        (cd "$work" && ar x "$1")
    fi
    data=$(ls "$work"/data.tar* 2>/dev/null | head -n 1)
    [ -n "$data" ] || { echo "$1: no data.tar inside" >&2; exit 1; }
    mkdir -p "$2"
    case "$data" in
        *.zst) if tar -xf "$data" -C "$2" 2>/dev/null; then :; else zstd -dc "$data" | tar -xf - -C "$2"; fi ;;
        *) tar -xf "$data" -C "$2" ;;
    esac
    rm -rf "$work"
}

mkdir -p "$DIR/debs"
echo "$PACKAGES" | while read -r sum path where; do
    [ -n "$sum" ] || continue
    file="$DIR/debs/$(basename "$path")"
    if [ ! -f "$file" ] || [ "$(sha256_of "$file")" != "$sum" ]; then
        echo "downloading $(basename "$path")"
        download "$MIRROR/$path" "$file.part" 2>/dev/null || download "$FALLBACK/$(basename "$path")" "$file.part"
        mv "$file.part" "$file"
    fi
    got=$(sha256_of "$file")
    if [ "$got" != "$sum" ]; then
        echo "$(basename "$path"): checksum mismatch (got $got), refusing to use it" >&2
        rm -f "$file"
        exit 1
    fi
done

rm -rf "$DIR/kernel" "$DIR/firmware"
echo "$PACKAGES" | while read -r sum path where; do
    [ -n "$sum" ] || continue
    unpack_deb "$DIR/debs/$(basename "$path")" "$DIR/$where"
done
# Newer packages keep firmware under usr/lib/firmware; build-disk.sh expects lib/firmware.
if [ -d "$DIR/firmware/usr/lib/firmware" ]; then
    mkdir -p "$DIR/firmware/lib/firmware"
    cp -R "$DIR/firmware/usr/lib/firmware/." "$DIR/firmware/lib/firmware/"
    rm -rf "$DIR/firmware/usr"
fi
[ -f "$DIR/kernel/boot/vmlinuz-$KVER" ] || { echo "kernel image missing after unpacking" >&2; exit 1; }
echo "kernel $KVER and firmware unpacked in $DIR"
