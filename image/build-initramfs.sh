#!/bin/sh
# Build a Flint initramfs for QEMU testing.
#
#   BUSYBOX=/path/to/static-busybox image/build-initramfs.sh [output.cpio.gz]
#
# Environment:
#   BUSYBOX   (required) static Linux x86_64 busybox, used as the stopgap shell/tools
#   TARGET    cargo target the binaries were built for   (default x86_64-unknown-linux-musl)
#   PROFILE   cargo profile directory                    (default release)
#   BINDIR    built std binaries: flint-init, flint-power  (default target/$TARGET/$PROFILE)
#   RTBINDIR  built flint-rt binaries: flint-version       (default rt/target/$TARGET/$PROFILE)
#   SERVICES  services file to install                   (default image/etc/flint/services.qemu)
#   OVERLAY   directory copied over the finished rootfs  (optional, for test files)
#
# POSIX sh only: runs on FreeBSD and Linux hosts.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
TARGET=${TARGET:-x86_64-unknown-linux-musl}
PROFILE=${PROFILE:-release}
BINDIR=${BINDIR:-$ROOT/target/$TARGET/$PROFILE}
RTBINDIR=${RTBINDIR:-$ROOT/rt/target/$TARGET/$PROFILE}
SERVICES=${SERVICES:-$ROOT/image/etc/flint/services.qemu}
OUT=${1:-$ROOT/target/flint-initramfs.cpio.gz}
: "${BUSYBOX:?set BUSYBOX to a static Linux x86_64 busybox binary}"

for f in flint-init flint-power; do
    [ -x "$BINDIR/$f" ] || { echo "missing $BINDIR/$f (build first, or set BINDIR/TARGET)" >&2; exit 1; }
done
[ -x "$RTBINDIR/flint-version" ] || { echo "missing $RTBINDIR/flint-version (cd rt && cargo build --release ...)" >&2; exit 1; }
[ -f "$BUSYBOX" ] || { echo "BUSYBOX=$BUSYBOX does not exist" >&2; exit 1; }

RFS=$(mktemp -d "${TMPDIR:-/tmp}/flint-rootfs.XXXXXX")
trap 'rm -rf "$RFS"' EXIT

mkdir -p "$RFS/bin" "$RFS/sbin" "$RFS/etc/flint" "$RFS/dev" "$RFS/proc" "$RFS/sys" \
         "$RFS/run" "$RFS/tmp" "$RFS/root" "$RFS/usr/bin" "$RFS/usr/sbin"
chmod 1777 "$RFS/tmp"
chmod 700 "$RFS/root"

# Flint's own pieces. The kernel runs /init as PID 1 for an initramfs.
cp "$BINDIR/flint-init" "$RFS/init"
cp "$BINDIR/flint-init" "$RFS/sbin/flint-init"
cp "$BINDIR/flint-power" "$RFS/sbin/flint-power"
cp "$RTBINDIR/flint-version" "$RFS/bin/flint-version"
for a in poweroff reboot halt; do ln -s flint-power "$RFS/sbin/$a"; done

# Stopgap tools until Flint has its own: busybox plus applet symlinks.
cp "$BUSYBOX" "$RFS/bin/busybox"
chmod 755 "$RFS/bin/busybox"
for a in sh ls cat echo mkdir rm cp mv ln mount umount ps dmesg sleep kill uname hostname \
         grep head tail wc date false true id env chmod chown touch df free; do
    ln -s busybox "$RFS/bin/$a"
done

cp "$ROOT/image/etc/flint/boot" "$RFS/etc/flint/boot"
cp "$SERVICES" "$RFS/etc/flint/services"
echo "flint" > "$RFS/etc/hostname"

if [ -n "${OVERLAY:-}" ]; then
    cp -R "$OVERLAY"/. "$RFS"/
fi

mkdir -p "$(dirname "$OUT")"
# -R 0:0 makes everything root-owned whoever runs the script.
( cd "$RFS" && find . | cpio -o -H newc -R 0:0 2>/dev/null ) | gzip -9 > "$OUT"
echo "wrote $OUT ($(wc -c < "$OUT") bytes)"
