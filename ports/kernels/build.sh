#!/bin/sh
# Build a kernel package (linux-FLAVOR) for `pkg kernels install FLAVOR`.
#
#   ports/kernels/build.sh FLAVOR OUTDIR
#
# FLAVOR:
#   zen        Arch Linux's linux-zen (desktop-tuned)
#   lts        Arch Linux's linux-lts (the newest long-term kernel)
#   upstream   Arch Linux's linux (the newest stable kernel, close to kernel.org)
#   NAME       with SRC=DIR: a kernel unpacked in DIR (boot/vmlinuz-VERSION and
#              lib/modules/VERSION), for example one you built yourself
#
# OUTDIR gets control and files/; then: pkg build OUTDIR REPO
#
# Arch packages are checked against the SHA-256 in Arch's (HTTPS-downloaded) package list.
# Modules are stored gzip-compressed (KEEP_MODULES=1 leaves them as they come).
#
# Environment: ARCH_MIRROR (default https://geo.mirror.pkgbuild.com), FIRMWARE (a firmware
# tree for flint-mkimage; default build/linux/firmware/lib/firmware), HOSTBIN (where
# flint-mkimage is; default target/release), DRIVERS (default image/drivers.conf).
#
# Needs: fetch, curl or wget; sha256 or sha256sum; tar that reads .tar.zst (FreeBSD's
# bsdtar does, or GNU tar with zstd); zstd and xz. POSIX sh: runs on FreeBSD and Linux.
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
FLAVOR=${1:?usage: build.sh FLAVOR OUTDIR}
OUT=${2:?usage: build.sh FLAVOR OUTDIR}
MIRROR=${ARCH_MIRROR:-https://geo.mirror.pkgbuild.com}
FIRMWARE=${FIRMWARE:-$ROOT/build/linux/firmware/lib/firmware}
HOSTBIN=${HOSTBIN:-$ROOT/target/release}
DRIVERS=${DRIVERS:-$ROOT/image/drivers.conf}
SRC=${SRC:-}

case "$FLAVOR" in
    *[!a-z0-9_-]*|'') echo "flavor: lower-case letters, digits, - and _" >&2; exit 1 ;;
esac
[ -x "$HOSTBIN/flint-mkimage" ] || { echo "missing $HOSTBIN/flint-mkimage (cargo build --release -p flint-mkimage)" >&2; exit 1; }
[ -d "$FIRMWARE" ] || { echo "no firmware tree at $FIRMWARE (run image/fetch-kernel.sh, or set FIRMWARE)" >&2; exit 1; }

download() { # URL OUT
    if command -v fetch >/dev/null 2>&1; then fetch -q -o "$2" "$1"
    elif command -v curl >/dev/null 2>&1; then curl -fsSL -o "$2" "$1"
    else wget -q -O "$2" "$1"; fi
}
sha256_of() {
    if command -v sha256 >/dev/null 2>&1; then sha256 -q "$1"; else sha256sum "$1" | cut -d' ' -f1; fi
}
untar() { # ARCHIVE DIR
    mkdir -p "$2"
    if tar -xf "$1" -C "$2" 2>/dev/null; then :; else zstd -dc "$1" | tar -xf - -C "$2"; fi
}
# The value of %KEY% in an Arch package description file.
field() { # FILE KEY
    awk -v k="%$2%" '$0 == k { getline; print; exit }' "$1"
}

WORK=$(mktemp -d "${TMPDIR:-/tmp}/flint-kernel.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
K="$WORK/k"

if [ -n "$SRC" ]; then
    KVER=$(ls "$SRC/lib/modules" | head -n 1)
    [ -n "$KVER" ] && [ -f "$SRC/boot/vmlinuz-$KVER" ] || { echo "$SRC: expected boot/vmlinuz-VERSION and lib/modules/VERSION" >&2; exit 1; }
    mkdir -p "$K/boot" "$K/lib/modules"
    cp "$SRC/boot/vmlinuz-$KVER" "$K/boot/"
    cp -R "$SRC/lib/modules/$KVER" "$K/lib/modules/"
    VERSION=$(echo "$KVER" | tr -c 'A-Za-z0-9.+_~\n-' '_')
    DESC="Linux $KVER ($FLAVOR)"
else
    case "$FLAVOR" in
        zen) REPO=extra; PKG=linux-zen; WHAT="desktop-tuned (zen)" ;;
        lts) REPO=core; PKG=linux-lts; WHAT="long-term support" ;;
        upstream) REPO=core; PKG=linux; WHAT="newest stable" ;;
        *) echo "unknown flavor $FLAVOR (zen, lts, upstream, or set SRC)" >&2; exit 1 ;;
    esac
    echo "reading the $REPO package list from $MIRROR"
    download "$MIRROR/$REPO/os/x86_64/$REPO.db" "$WORK/db"
    untar "$WORK/db" "$WORK/dbx"
    DESCF=""
    for d in "$WORK"/dbx/*/desc; do
        if [ "$(field "$d" NAME)" = "$PKG" ]; then DESCF=$d; break; fi
    done
    [ -n "$DESCF" ] || { echo "$PKG not found in $REPO" >&2; exit 1; }
    FILE=$(field "$DESCF" FILENAME)
    SUM=$(field "$DESCF" SHA256SUM)
    AVER=$(field "$DESCF" VERSION)
    echo "downloading $FILE"
    download "$MIRROR/$REPO/os/x86_64/$FILE" "$WORK/pkg"
    GOT=$(sha256_of "$WORK/pkg")
    [ "$GOT" = "$SUM" ] || { echo "$FILE: checksum mismatch (got $GOT, want $SUM)" >&2; exit 1; }
    untar "$WORK/pkg" "$WORK/x"
    KVER=$(ls "$WORK/x/usr/lib/modules" | grep -v '^extramodules' | head -n 1)
    [ -f "$WORK/x/usr/lib/modules/$KVER/vmlinuz" ] || { echo "$FILE: no kernel image inside" >&2; exit 1; }
    mkdir -p "$K/boot" "$K/lib/modules"
    mv "$WORK/x/usr/lib/modules/$KVER/vmlinuz" "$K/boot/vmlinuz-$KVER"
    mv "$WORK/x/usr/lib/modules/$KVER" "$K/lib/modules/$KVER"
    VERSION=$(echo "$AVER" | tr ':' '_')
    DESC="Linux $KVER, $WHAT (Arch Linux's $PKG $AVER)"
fi

# Modules as .ko.gz: a kernel that cannot decompress modules itself (that is optional,
# and only for the format it was built with) gets them unpacked by Flint's modload, which
# reads gzip. Plain modules would take four times the space (and RAM).
if [ -z "${KEEP_MODULES:-}" ]; then
    find "$K/lib/modules" -name '*.ko.zst' -exec zstd -dq --rm {} +
    find "$K/lib/modules" -name '*.ko.xz' -exec xz -d {} +
    find "$K/lib/modules" -name '*.ko' -print0 | xargs -0 -n 64 -P "${JOBS:-8}" gzip -9n
fi

"$HOSTBIN/flint-mkimage" drivers "$DRIVERS" "$K" "$FIRMWARE" "$WORK/MODULES.IMG" "$WORK/fw.img" "$WORK/gpu.img" > "$WORK/drivers.log"
head -n 1 "$WORK/drivers.log"

D="$OUT/files/usr/lib/flint/kernels/$FLAVOR"
rm -rf "$OUT"
mkdir -p "$D"
cp "$K/boot/vmlinuz-$KVER" "$D/KERNEL"
cp "$WORK/MODULES.IMG" "$D/MODULES.IMG"
echo "$KVER" > "$D/RELEASE"
chmod 644 "$D/KERNEL" "$D/MODULES.IMG" "$D/RELEASE"
cat > "$OUT/control" <<EOF
name: linux-$FLAVOR
version: $VERSION
description: $DESC
arch: x86_64
EOF
echo "staged linux-$FLAVOR $VERSION in $OUT (pkg build $OUT REPO)"
