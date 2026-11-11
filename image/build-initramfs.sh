#!/bin/sh
# Build a Flint initramfs for QEMU testing.
#
#   BUSYBOX=/path/to/static-busybox image/build-initramfs.sh [output.cpio.gz]
#
# Environment:
#   BUSYBOX   (required) static Linux x86_64 busybox: stopgap for the few tools flint-box
#             does not have yet (grep, sed, date, ...)
#   TARGET    cargo target the binaries were built for   (default x86_64-unknown-linux-musl)
#   PROFILE   cargo profile directory                    (default release)
#   BINDIR    built std binaries: flint-init, flint-power, flint-login, flint-passwd, flint-su,
#             flint-sh, flint-box, flint-pkg
#                                                          (default target/$TARGET/$PROFILE)
#   RTBINDIR  built flint-rt binaries: flint-version       (default rt/target/$TARGET/$PROFILE)
#   SERVICES  services file to install                   (default image/etc/flint/services.qemu)
#   BOOT      boot tasks file to install                 (default image/etc/flint/boot.qemu)
#   OVERLAY   directory copied over the finished rootfs  (optional, for test files)
#   HOSTPKG   a flint-pkg that runs on THIS machine, used to build the example package
#             repository   (default target/release/flint-pkg: cargo build --release -p flint-pkg)
#
# POSIX sh only: runs on FreeBSD and Linux hosts.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
TARGET=${TARGET:-x86_64-unknown-linux-musl}
PROFILE=${PROFILE:-release}
BINDIR=${BINDIR:-$ROOT/target/$TARGET/$PROFILE}
RTBINDIR=${RTBINDIR:-$ROOT/rt/target/$TARGET/$PROFILE}
SERVICES=${SERVICES:-$ROOT/image/etc/flint/services.qemu}
BOOT=${BOOT:-$ROOT/image/etc/flint/boot.qemu}
OUT=${1:-$ROOT/target/flint-initramfs.cpio.gz}
: "${BUSYBOX:?set BUSYBOX to a static Linux x86_64 busybox binary}"

for f in flint-init flint-power flint-login flint-passwd flint-su flint-sh flint-box flint-pkg; do
    [ -x "$BINDIR/$f" ] || { echo "missing $BINDIR/$f (build first, or set BINDIR/TARGET)" >&2; exit 1; }
done
[ -x "$RTBINDIR/flint-version" ] || { echo "missing $RTBINDIR/flint-version (cd rt && cargo build --release ...)" >&2; exit 1; }
[ -f "$BUSYBOX" ] || { echo "BUSYBOX=$BUSYBOX does not exist" >&2; exit 1; }

RFS=$(mktemp -d "${TMPDIR:-/tmp}/flint-rootfs.XXXXXX")
trap 'rm -rf "$RFS"' EXIT
# mktemp -d makes the directory 0700, and the archive would make / unreadable to non-root users.
chmod 755 "$RFS"

mkdir -p "$RFS/bin" "$RFS/sbin" "$RFS/etc/flint" "$RFS/dev" "$RFS/proc" "$RFS/sys" \
         "$RFS/run" "$RFS/tmp" "$RFS/root" "$RFS/home" "$RFS/usr/bin" "$RFS/usr/sbin"
chmod 1777 "$RFS/tmp"
chmod 700 "$RFS/root"

# Flint's own pieces. The kernel runs /init as PID 1 for an initramfs.
cp "$BINDIR/flint-init" "$RFS/init"
cp "$BINDIR/flint-init" "$RFS/sbin/flint-init"
cp "$BINDIR/flint-power" "$RFS/sbin/flint-power"
cp "$BINDIR/flint-login" "$RFS/sbin/flint-login"
cp "$BINDIR/flint-box" "$RFS/bin/flint-box"
# passwd and su must be setuid root. Never set that bit on flint-box: it would run every
# applet as root.
cp "$BINDIR/flint-passwd" "$RFS/bin/passwd"
cp "$BINDIR/flint-su" "$RFS/bin/su"
chmod 4755 "$RFS/bin/passwd" "$RFS/bin/su"
cp "$BINDIR/flint-sh" "$RFS/bin/flint-sh"
cp "$BINDIR/flint-pkg" "$RFS/bin/flint-pkg"
ln -s flint-sh "$RFS/bin/sh"
cp "$RTBINDIR/flint-version" "$RFS/bin/flint-version"
for a in poweroff reboot halt; do ln -s flint-power "$RFS/sbin/$a"; done

# flint-box applets: one symlink per name in crates/flint-box/applets.txt.
while read -r a; do
    [ -n "$a" ] && ln -s flint-box "$RFS/bin/$a"
done < "$ROOT/crates/flint-box/applets.txt"

# Stopgap: busybox for whatever flint-box does not provide yet.
cp "$BUSYBOX" "$RFS/bin/busybox"
chmod 755 "$RFS/bin/busybox"
for a in dmesg free date grep sed awk find sort uniq tr cut xargs less vi tee basename dirname \
         which readlink stat df du mknod mkfifo insmod rmmod lsmod modprobe ifconfig ip ping; do
    [ -e "$RFS/bin/$a" ] || [ -h "$RFS/bin/$a" ] || ln -s busybox "$RFS/bin/$a"
done

cp "$BOOT" "$RFS/etc/flint/boot"
cp "$SERVICES" "$RFS/etc/flint/services"
echo "flint" > "$RFS/etc/hostname"
for f in passwd group shadow motd shells; do
    cp "$ROOT/image/etc/$f" "$RFS/etc/$f"
done
chmod 644 "$RFS/etc/passwd" "$RFS/etc/group" "$RFS/etc/motd" "$RFS/etc/shells"
chmod 600 "$RFS/etc/shadow"

# Example package repository from packages/ (optional: needs a flint-pkg for this host).
HOSTPKG=${HOSTPKG:-$ROOT/target/release/flint-pkg}
if [ -x "$HOSTPKG" ] && "$HOSTPKG" --version >/dev/null 2>&1; then
    REPO="$RFS/var/cache/flint-pkg/repo"
    mkdir -p "$REPO"
    for d in "$ROOT"/packages/*/; do
        if [ -f "$d/control" ]; then
            "$HOSTPKG" build "$d" "$REPO" >/dev/null
        fi
    done
    "$HOSTPKG" index "$REPO" >/dev/null
    echo "repo /var/cache/flint-pkg/repo" > "$RFS/etc/flint/pkg.conf"
else
    echo "note: no usable flint-pkg for this host at $HOSTPKG; skipping the example repository" >&2
    echo "      (build one with: cargo build --release -p flint-pkg)" >&2
fi

if [ -n "${OVERLAY:-}" ]; then
    cp -R "$OVERLAY"/. "$RFS"/
fi

mkdir -p "$(dirname "$OUT")"
# -R 0:0 makes everything root-owned whoever runs the script.
( cd "$RFS" && find . | cpio -o -H newc -R 0:0 2>/dev/null ) | gzip -9 > "$OUT"
echo "wrote $OUT ($(wc -c < "$OUT") bytes)"
