#!/bin/sh
# Build fastfetch as one static musl binary and stage it as a pkg source tree.
#
#   ports/fastfetch/build.sh OUTDIR
#
# Needs a Linux host with git, cmake, ninja, python3 and musl-gcc (Debian/Ubuntu: musl-tools).
# OUTDIR gets control and files/; then: pkg build OUTDIR REPO
set -eu

VERSION=2.71.0
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${1:?usage: build.sh OUTDIR}
WORK=${WORK:-$(mktemp -d)}

command -v musl-gcc >/dev/null || { echo "musl-gcc not found (install musl-tools)" >&2; exit 1; }

if [ ! -d "$WORK/src" ]; then
    git clone -q --depth 1 --branch "$VERSION" https://github.com/fastfetch-cli/fastfetch.git "$WORK/src"
fi
[ -f "$WORK/pci.ids" ] || curl -fsSL -o "$WORK/pci.ids" https://raw.githubusercontent.com/pciutils/pciids/master/pci.ids

# musl-gcc does not see the kernel headers, and musl before 1.2.5 declares a GNU
# basename() in string.h that C23 reads as basename(void).
KH="$WORK/include"
mkdir -p "$KH"
for d in linux asm-generic mtd scsi drm sound video rdma misc; do
    [ -e "/usr/include/$d" ] && ln -sfn "/usr/include/$d" "$KH/$d"
done
for a in /usr/include/x86_64-linux-gnu/asm /usr/include/asm; do
    [ -d "$a" ] && { ln -sfn "$a" "$KH/asm"; break; }
done
MUSLINC=$(dirname "$(musl-gcc -print-file-name=include/string.h 2>/dev/null)")
[ -f "$MUSLINC/string.h" ] || MUSLINC=/usr/include/x86_64-linux-musl
sed '/^char \*basename();$/d' "$MUSLINC/string.h" > "$KH/string.h"

B="$WORK/build"
mkdir -p "$B"
cp "$WORK/pci.ids" "$B/pci.ids"
OFF=""
for o in VULKAN WAYLAND XCB_RANDR XRANDR DRM VADRM VAX11 VDPAU GIO DCONF EET DBUS SQLITE3 RPM EGL GLX OPENCL ELF \
         LUA LIBZFS ZLIB IMAGE_LOGO EMBEDDED_AMDGPUIDS; do
    OFF="$OFF -DENABLE_$o=OFF"
done
# shellcheck disable=SC2086
CC=musl-gcc CFLAGS="-isystem $KH" cmake -S "$WORK/src" -B "$B" -G Ninja -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX=/usr -DBUILD_FLASHFETCH=OFF -DBUILD_TESTS=OFF -DENABLE_EMBEDDED_PCIIDS=ON $OFF \
    -DCMAKE_EXE_LINKER_FLAGS=-static >/dev/null
ninja -C "$B" fastfetch >/dev/null
"$B/fastfetch" --version

rm -rf "$OUT"
mkdir -p "$OUT/files/usr/bin" "$OUT/files/usr/share/licenses/fastfetch" "$OUT/files/usr/share/fastfetch" "$OUT/files/etc/xdg/fastfetch"
cp "$HERE/control" "$OUT/control"
install -m 755 "$B/fastfetch" "$OUT/files/usr/bin/fastfetch"
strip "$OUT/files/usr/bin/fastfetch" 2>/dev/null || true
install -m 644 "$WORK/src/LICENSE" "$OUT/files/usr/share/licenses/fastfetch/LICENSE"
install -m 644 "$HERE/flint-logo.txt" "$OUT/files/usr/share/fastfetch/flint-logo.txt"
install -m 644 "$HERE/config.jsonc" "$OUT/files/etc/xdg/fastfetch/config.jsonc"
echo "staged $OUT"
