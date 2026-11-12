#!/bin/sh
# Build the base system packages that flint-update installs: flint-system (base image
# and bootloader), flint-kernel (the kernel and its drivers), flint-firmware and
# flint-gpu-firmware. The same files as on the boot partition of target/flint.img.
#
#   image/build-system-packages.sh REPO_DIR
#
# Then: pkg index REPO_DIR && pkg sign REPO_DIR KEY. Uses the same environment variables
# as image/build-disk.sh (LINUX, BASE, BOOTEFI, DRIVERS, HOSTBIN). Package versions are
# the Flint version plus the build time, so every build is an upgrade.
#
# POSIX sh only: runs on FreeBSD and Linux hosts.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
REPO=${1:?usage: build-system-packages.sh REPO_DIR}
LINUX=${LINUX:-$ROOT/build/linux}
BASE=${BASE:-$ROOT/target/flint-base-pc.img}
BOOTEFI=${BOOTEFI:-$ROOT/boot/target/x86_64-unknown-uefi/release/flint-boot.efi}
DRIVERS=${DRIVERS:-$ROOT/image/drivers.conf}
HOSTBIN=${HOSTBIN:-$ROOT/target/release}

for f in "$HOSTBIN/flint-mkimage" "$HOSTBIN/pkg" "$BOOTEFI" "$BASE"; do
    [ -e "$f" ] || { echo "missing $f (build Flint and the disk image first: image/build-disk.sh)" >&2; exit 1; }
done
KERNEL=$(ls "$LINUX"/kernel/boot/vmlinuz-* 2>/dev/null | head -n 1)
[ -n "$KERNEL" ] || { echo "no kernel in $LINUX/kernel/boot (run image/fetch-kernel.sh)" >&2; exit 1; }
KVER=${KERNEL##*/vmlinuz-}
FLINTVER=$(cat "$ROOT/VERSION")
STAMP=$(date -u +%Y%m%d%H%M)

WORK=$(mktemp -d "${TMPDIR:-/tmp}/flint-syspkg.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

"$HOSTBIN/flint-mkimage" drivers "$DRIVERS" "$LINUX/kernel" "$LINUX/firmware/lib/firmware" \
    "$WORK/EARLY.IMG" "$WORK/DRIVERS" | head -n 1
# GPU firmware is its own package, so no package is over 100 MB (GitHub's file limit).
GPUDIRS="amdgpu radeon i915 xe nvidia"
mkdir -p "$WORK/gpufw"
for g in $GPUDIRS; do
    [ -d "$WORK/DRIVERS/firmware/$g" ] && mv "$WORK/DRIVERS/firmware/$g" "$WORK/gpufw/"
done

stage() { # NAME VERSION DESCRIPTION FILE-OR-DIR...  (each copied into usr/lib/flint/system)
    name=$1 version=$2 desc=$3
    shift 3
    d="$WORK/$name"
    mkdir -p "$d/files/usr/lib/flint/system"
    printf 'name: %s\nversion: %s\ndescription: %s\narch: x86_64\n' "$name" "$version" "$desc" > "$d/control"
    for f in "$@"; do
        cp -R "$f" "$d/files/usr/lib/flint/system/"
    done
    find "$d/files" -type f -exec chmod 644 {} +
    find "$d/files" -type d -exec chmod 755 {} +
    "$HOSTBIN/pkg" build "$d" "$REPO"
}

echo "$FLINTVER" > "$WORK/VERSION"
cp "$BASE" "$WORK/BASE.IMG"
cp "$BOOTEFI" "$WORK/BOOTX64.EFI"
cp "$KERNEL" "$WORK/KERNEL"
echo "$KVER" > "$WORK/RELEASE"
mkdir -p "$WORK/gpu/firmware"
mv "$WORK"/gpufw/* "$WORK/gpu/firmware/" 2>/dev/null || true
stage flint-system "$FLINTVER.$STAMP" "Flint Linux $FLINTVER base system and bootloader" "$WORK/BASE.IMG" "$WORK/BOOTX64.EFI" "$WORK/VERSION"
stage flint-kernel "$KVER.$STAMP" "Linux $KVER, the kernel Flint comes with, and its drivers" "$WORK/KERNEL" "$WORK/EARLY.IMG" "$WORK/RELEASE" "$WORK/DRIVERS/modules"
stage flint-firmware "$FLINTVER.$STAMP" "Device firmware (network, Wi-Fi, storage, ...)" "$WORK/DRIVERS/firmware"
stage flint-gpu-firmware "$FLINTVER.$STAMP" "Graphics firmware (AMD, Intel)" "$WORK/gpu/firmware"
echo "next: $HOSTBIN/pkg index $REPO && $HOSTBIN/pkg sign $REPO YOUR.key"
