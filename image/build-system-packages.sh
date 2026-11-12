#!/bin/sh
# Build the base system packages that flint-update installs: flint-system (base image
# and bootloader), flint-kernel (the kernel and its drivers), flint-firmware and
# flint-gpu-firmware. The same files as on the boot partition of target/flint.img. Big ones
# are split into NAME-1, NAME-2, ... parts (under GitHub's file size limits); NAME depends
# on its parts, so flint-update installs them all.
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
# GPU firmware is its own package (and every big package is split into parts below).
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

# GitHub refuses files over 100 MB (and warns over 50 MB), so a big tree is split into
# parts of at most PART_KIB: whole directories where they fit, else their contents.
PART_KIB=${PART_KIB:-46080}

# units DIR REL: print "KiB<TAB>path" for each piece of DIR/REL small enough for a part
units() {
    _size=$(du -sk "$1/$2" | cut -f1)
    if [ "$_size" -gt "$PART_KIB" ] && [ -d "$1/$2" ]; then
        for _e in $(cd "$1/$2" && ls -A | LC_ALL=C sort); do
            units "$1" "$2/$_e"
        done
    else
        [ "$_size" -gt 95000 ] && { echo "$1/$2 is $_size KiB: too big for one package" >&2; exit 1; }
        printf '%s\t%s\n' "$_size" "$2"
    fi
}

# stage_split NAME VERSION DESCRIPTION DIR [FILE...]: DIR (copied into usr/lib/flint/system)
# goes into NAME-1, NAME-2, ... and NAME itself holds the FILEs and depends on the parts,
# so installing or upgrading NAME brings them all.
stage_split() {
    name=$1 version=$2 desc=$3 dir=$4
    shift 4
    base=$(dirname "$dir")
    top=$(basename "$dir")
    units "$base" "$top" > "$WORK/$name.units"
    n=0 used=0 deps=""
    while IFS="$(printf '\t')" read -r kib rel; do
        if [ "$n" -eq 0 ] || [ $((used + kib)) -gt "$PART_KIB" ]; then
            n=$((n + 1))
            used=0
            deps="${deps:+$deps, }$name-$n=$version"
            mkdir -p "$WORK/$name-$n/files/usr/lib/flint/system"
            printf 'name: %s\nversion: %s\ndescription: %s (part %s)\narch: x86_64\n' "$name-$n" "$version" "$desc" "$n" \
                > "$WORK/$name-$n/control"
        fi
        used=$((used + kib))
        dst="$WORK/$name-$n/files/usr/lib/flint/system/$rel"
        mkdir -p "$(dirname "$dst")"
        cp -R "$base/$rel" "$dst"
    done < "$WORK/$name.units"
    i=1
    while [ "$i" -le "$n" ]; do
        find "$WORK/$name-$i/files" -type f -exec chmod 644 {} +
        find "$WORK/$name-$i/files" -type d -exec chmod 755 {} +
        "$HOSTBIN/pkg" build "$WORK/$name-$i" "$REPO"
        i=$((i + 1))
    done
    # the package with the name flint-update asks for: the remaining files, and the parts
    d="$WORK/$name"
    mkdir -p "$d/files/usr/lib/flint/system"
    printf 'name: %s\nversion: %s\ndescription: %s\narch: x86_64\ndepends: %s\n' "$name" "$version" "$desc" "$deps" > "$d/control"
    for f in "$@"; do
        cp -R "$f" "$d/files/usr/lib/flint/system/"
    done
    printf '%s\n' "$n" > "$d/files/usr/lib/flint/system/$name.parts"
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
stage_split flint-kernel "$KVER.$STAMP" "Linux $KVER, the kernel Flint comes with, and its drivers" "$WORK/DRIVERS/modules" \
    "$WORK/KERNEL" "$WORK/EARLY.IMG" "$WORK/RELEASE"
stage_split flint-firmware "$FLINTVER.$STAMP" "Device firmware (network, Wi-Fi, storage, ...)" "$WORK/DRIVERS/firmware"
stage_split flint-gpu-firmware "$FLINTVER.$STAMP" "Graphics firmware (AMD, Intel)" "$WORK/gpu/firmware"
echo "next: $HOSTBIN/pkg index $REPO && $HOSTBIN/pkg sign $REPO YOUR.key"
