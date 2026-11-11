#!/bin/sh
# Build a bootable Flint disk image for USB sticks and disks (any x86-64 UEFI PC).
#
#   image/build-disk.sh [OUTPUT]                  (default target/flint.img)
#
# Then write it to a USB stick (this erases the stick):
#   FreeBSD:  dd if=target/flint.img of=/dev/da0 bs=1m conv=sync
#   Linux:    dd if=target/flint.img of=/dev/sdX bs=1M conv=fsync
#
# Environment:
#   LINUX     unpacked kernel + firmware from image/fetch-kernel.sh   (default build/linux)
#   BASE      Flint root filesystem image; built with the bare-metal services and boot
#             tasks when missing                                     (default target/flint-base.img)
#   BOOTEFI   the bootloader     (default boot/target/x86_64-unknown-uefi/release/flint-boot.efi)
#   BOOTCFG   boot menu file     (default image/boot.cfg)
#   DRIVERS   driver selection   (default image/drivers.conf)
#   SIZE      image size in MiB  (default 512; the free space holds saved settings)
#   HOSTBIN   flint-mkimage for this machine                         (default target/release)
#
# POSIX sh only: runs on FreeBSD and Linux hosts.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:-$ROOT/target/flint.img}
LINUX=${LINUX:-$ROOT/build/linux}
BASE=${BASE:-$ROOT/target/flint-base.img}
BOOTEFI=${BOOTEFI:-$ROOT/boot/target/x86_64-unknown-uefi/release/flint-boot.efi}
BOOTCFG=${BOOTCFG:-$ROOT/image/boot.cfg}
DRIVERS=${DRIVERS:-$ROOT/image/drivers.conf}
SIZE=${SIZE:-512}
HOSTBIN=${HOSTBIN:-$ROOT/target/release}
MKIMAGE="$HOSTBIN/flint-mkimage"

[ -x "$MKIMAGE" ] || { echo "missing $MKIMAGE (build with: cargo build --release -p flint-mkimage)" >&2; exit 1; }
[ -f "$BOOTEFI" ] || { echo "missing $BOOTEFI (build with: cd boot && cargo build --release --target x86_64-unknown-uefi)" >&2; exit 1; }
KERNEL=$(ls "$LINUX"/kernel/boot/vmlinuz-* 2>/dev/null | head -n 1)
[ -n "$KERNEL" ] || { echo "no kernel in $LINUX/kernel/boot (run image/fetch-kernel.sh first)" >&2; exit 1; }
[ -d "$LINUX/firmware/lib/firmware" ] || { echo "no firmware in $LINUX/firmware/lib/firmware (run image/fetch-kernel.sh first)" >&2; exit 1; }

if [ ! -f "$BASE" ]; then
    echo "building the root filesystem ($BASE)"
    SERVICES=${SERVICES:-$ROOT/image/etc/flint/services} BOOT=${BOOT:-$ROOT/image/etc/flint/boot} \
        "$ROOT/image/build-initramfs.sh" "$BASE"
fi

STAGE=$(mktemp -d "${TMPDIR:-/tmp}/flint-disk.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/EFI/BOOT" "$STAGE/FLINT"
cp "$BOOTEFI" "$STAGE/EFI/BOOT/BOOTX64.EFI"
cp "$KERNEL" "$STAGE/FLINT/KERNEL"
cp "$BASE" "$STAGE/FLINT/BASE.IMG"
cp "$BOOTCFG" "$STAGE/FLINT/BOOT.CFG"
"$MKIMAGE" drivers "$DRIVERS" "$LINUX/kernel" "$LINUX/firmware/lib/firmware" \
    "$STAGE/FLINT/MODULES.IMG" "$STAGE/FLINT/FIRMWARE.IMG"
mkdir -p "$(dirname "$OUT")"
"$MKIMAGE" disk "$OUT" "$SIZE" "$STAGE"
