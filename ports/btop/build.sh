#!/bin/sh
# btop, the resource monitor (static, C++ with zig's libc++). Usage: build.sh OUTDIR
. "$(dirname "$0")/../common.sh"
ports_init btop "${1:?usage: build.sh OUTDIR}"
src=$(unpack "$(fetch universe/b/btop/btop_1.4.7.orig.tar.gz 23d182c4efd5836f1a3cdd20004e72d09ee06b26ff9edf762b586063f7a73dd3)")
say "btop"
(cd "$src" && make -j"$JOBS" CXX="$CXX" STATIC=true GPU_SUPPORT=false ADDFLAGS="-static" VERBOSE=true \
    >>"$LOG" 2>&1 && make install PREFIX=/usr DESTDIR="$STAGE" >>"$LOG" 2>&1)
rm -rf "$STAGE/usr/share/applications" "$STAGE/usr/share/icons" "$STAGE/usr/share/man"
mkdir -p "$STAGE/usr/share/licenses/btop" && cp "$src/LICENSE" "$STAGE/usr/share/licenses/btop/"
ports_package 1.4.7 "Resource monitor: processes, CPU, memory, disks and network, with graphs"
