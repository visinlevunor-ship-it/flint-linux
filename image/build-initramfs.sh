#!/bin/sh
# Build Flint's root filesystem as an initramfs (newc cpio).
#
#   BUSYBOX=/path/to/static-busybox image/build-initramfs.sh [output.img]
#
# Environment:
#   BUSYBOX   (required) static Linux x86_64 busybox: stopgap for the few tools Flint does
#             not have yet (grep, sed, date, ...)
#   TARGET    cargo target the Flint binaries were built for (default x86_64-unknown-linux-musl)
#   PROFILE   cargo profile directory                         (default release)
#   BINDIR    the built Linux binaries flint, su, passwd     (default target/$TARGET/$PROFILE)
#   HOSTBIN   tools built for THIS machine: flint-mkimage, pkg
#             (default target/release; build them with: cargo build --release -p flint-mkimage -p flint-pkg)
#   SERVICES  services file to install                        (default image/etc/flint/services.qemu)
#   BOOT      boot tasks file to install                      (default image/etc/flint/boot.qemu)
#   OVERLAY   directory copied over the finished rootfs       (optional, for test files)
#   RTBINDIR  libc-free flint-rt programs, installed under /usr/lib/flint if present
#                                                             (default rt/target/$TARGET/$PROFILE)
#
# POSIX sh only: runs on FreeBSD and Linux hosts.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
TARGET=${TARGET:-x86_64-unknown-linux-musl}
PROFILE=${PROFILE:-release}
BINDIR=${BINDIR:-$ROOT/target/$TARGET/$PROFILE}
HOSTBIN=${HOSTBIN:-$ROOT/target/release}
RTBINDIR=${RTBINDIR:-$ROOT/rt/target/$TARGET/$PROFILE}
SERVICES=${SERVICES:-$ROOT/image/etc/flint/services.qemu}
BOOT=${BOOT:-$ROOT/image/etc/flint/boot.qemu}
OUT=${1:-$ROOT/target/flint-base.img}
: "${BUSYBOX:?set BUSYBOX to a static Linux x86_64 busybox binary}"

for f in flint su passwd; do
    [ -x "$BINDIR/$f" ] || { echo "missing $BINDIR/$f (build with: cargo build --release --workspace --target $TARGET)" >&2; exit 1; }
done
MKIMAGE="$HOSTBIN/flint-mkimage"
[ -x "$MKIMAGE" ] || { echo "missing $MKIMAGE (build with: cargo build --release -p flint-mkimage)" >&2; exit 1; }
[ -f "$BUSYBOX" ] || { echo "BUSYBOX=$BUSYBOX does not exist" >&2; exit 1; }

RFS=$(mktemp -d "${TMPDIR:-/tmp}/flint-rootfs.XXXXXX")
trap 'rm -rf "$RFS"' EXIT
# mktemp -d makes the directory 0700, and the archive would make / unreadable to non-root users.
chmod 755 "$RFS"

mkdir -p "$RFS/bin" "$RFS/sbin" "$RFS/etc/flint" "$RFS/dev" "$RFS/proc" "$RFS/sys" \
         "$RFS/run" "$RFS/tmp" "$RFS/root" "$RFS/home" "$RFS/usr/bin" "$RFS/usr/sbin" \
         "$RFS/usr/lib/flint" "$RFS/var/lib" "$RFS/lib"
chmod 1777 "$RFS/tmp"
chmod 700 "$RFS/root"

# One binary for every Flint program, installed under the programs' normal names.
cp "$BINDIR/flint" "$RFS/bin/flint"
chmod 755 "$RFS/bin/flint"
while read -r dir name; do
    case "$dir" in ''|'#'*) continue ;; esac
    if [ "$dir" = bin ]; then
        ln -s flint "$RFS/bin/$name"
    else
        ln -s ../bin/flint "$RFS/$dir/$name"
    fi
done < "$ROOT/crates/flint-apps/apps.txt"
# The kernel starts /init as PID 1.
ln -s sbin/init "$RFS/init"

# su and passwd are setuid root, so they are separate small binaries. Never set that bit on
# /bin/flint: it would run every program as root.
cp "$BINDIR/su" "$RFS/bin/su"
cp "$BINDIR/passwd" "$RFS/bin/passwd"
chmod 4755 "$RFS/bin/su" "$RFS/bin/passwd"

# Programs on flint-rt (no libc at all), kept out of PATH.
if [ -x "$RTBINDIR/flint-version" ]; then
    cp "$RTBINDIR/flint-version" "$RFS/usr/lib/flint/version-nolibc"
fi

# Stopgap: busybox for whatever Flint does not provide yet.
cp "$BUSYBOX" "$RFS/bin/busybox"
chmod 755 "$RFS/bin/busybox"
for a in dmesg free date grep sed awk find sort uniq tr cut xargs less vi tee basename dirname \
         which readlink stat df du mknod mkfifo; do
    [ -e "$RFS/bin/$a" ] || [ -h "$RFS/bin/$a" ] || ln -s busybox "$RFS/bin/$a"
done

cp "$BOOT" "$RFS/etc/flint/boot"
cp "$SERVICES" "$RFS/etc/flint/services"
cp "$ROOT/image/etc/flint/modules.blacklist" "$RFS/etc/flint/modules.blacklist"
echo "flint" > "$RFS/etc/hostname"
printf '127.0.0.1\tlocalhost flint\n::1\tlocalhost\n' > "$RFS/etc/hosts"
# netd writes DNS servers to /run (a tmpfs); saved Wi-Fi networks live on the boot partition.
ln -s ../run/resolv.conf "$RFS/etc/resolv.conf"
ln -s /boot/FLINT/NETWORKS "$RFS/etc/flint/networks"
for f in passwd group shadow motd shells os-release; do
    cp "$ROOT/image/etc/$f" "$RFS/etc/$f"
done
sed "s/@VERSION@/$(cat "$ROOT/VERSION")/" "$ROOT/image/etc/os-release" > "$RFS/etc/os-release"
chmod 644 "$RFS/etc/hosts" "$RFS/etc/passwd" "$RFS/etc/group" "$RFS/etc/motd" "$RFS/etc/shells" "$RFS/etc/os-release"
chmod 600 "$RFS/etc/shadow"

# Example package repository from packages/ (optional: needs a pkg for this machine).
HOSTPKG="$HOSTBIN/pkg"
if [ -x "$HOSTPKG" ] && "$HOSTPKG" --version >/dev/null 2>&1; then
    REPO="$RFS/var/cache/pkg/repo"
    mkdir -p "$REPO"
    for d in "$ROOT"/packages/*/; do
        if [ -f "$d/control" ]; then
            "$HOSTPKG" build "$d" "$REPO" >/dev/null
        fi
    done
    "$HOSTPKG" index "$REPO" >/dev/null
    echo "repo /var/cache/pkg/repo" > "$RFS/etc/flint/pkg.conf"
else
    echo "note: no usable pkg for this machine at $HOSTPKG; skipping the example repository" >&2
    echo "      (build one with: cargo build --release -p flint-pkg)" >&2
fi

if [ -n "${OVERLAY:-}" ]; then
    cp -R "$OVERLAY"/. "$RFS"/
fi

mkdir -p "$(dirname "$OUT")"
"$MKIMAGE" cpio "$RFS" "$OUT"
echo "wrote $OUT ($(wc -c < "$OUT" | tr -d ' ') bytes)"
