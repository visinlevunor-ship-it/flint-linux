#!/bin/sh
# htop, the interactive process viewer (static). Usage: ports/htop/build.sh OUTDIR
. "$(dirname "$0")/../common.sh"
ports_init htop "${1:?usage: build.sh OUTDIR}"
lib_ncurses
src=$(unpack "$(fetch main/h/htop/htop_3.5.3.orig.tar.gz edf25ee020a5263ffbef9eef5a8c14392bf74e78b3d5c8bc64d9343dd9a82605)")
say "htop"
(cd "$src" && { [ -x configure ] || ./autogen.sh >>"$LOG" 2>&1; } && ./configure $HOSTFLAGS --prefix=/usr --enable-static --enable-unicode \
    --disable-sensors --disable-capabilities --disable-delayacct --disable-hwloc \
    CPPFLAGS="$CPPFLAGS -I$SYSROOT/include/ncursesw" >>"$LOG" 2>&1 && make -j"$JOBS" >>"$LOG" 2>&1 && make install-strip DESTDIR="$STAGE" >>"$LOG" 2>&1)
rm -rf "$STAGE/usr/share/man" "$STAGE/usr/share/applications" "$STAGE/usr/share/icons" "$STAGE/usr/share/pixmaps"
mkdir -p "$STAGE/usr/share/licenses/htop" && cp "$src/COPYING" "$STAGE/usr/share/licenses/htop/"
ports_package 3.5.3 "Interactive process viewer"
