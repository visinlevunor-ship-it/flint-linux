#!/bin/sh
# nano, the simple terminal text editor (static). Usage: ports/nano/build.sh OUTDIR
. "$(dirname "$0")/../common.sh"
ports_init nano "${1:?usage: build.sh OUTDIR}"
lib_ncurses
src=$(unpack "$(fetch main/n/nano/nano_9.3.orig.tar.xz a9affc52c9596ee0682bcdd4ceb3c0b235eb221d83d733dfd9b0dff45897cb0d)")
say "nano"
(cd "$src" && ./configure $HOSTFLAGS --prefix=/usr --sysconfdir=/etc --disable-nls --disable-libmagic --enable-utf8 \
    NCURSESW_CFLAGS="-I$SYSROOT/include/ncursesw" NCURSESW_LIBS="-lncursesw" >>"$LOG" 2>&1 \
    && make -j"$JOBS" >>"$LOG" 2>&1 && make install-strip DESTDIR="$STAGE" >>"$LOG" 2>&1)
rm -rf "$STAGE/usr/share/info" "$STAGE/usr/share/man" "$STAGE/usr/share/doc"
mkdir -p "$STAGE/etc"
# Syntax highlighting for everything nano knows, on by default.
echo 'include "/usr/share/nano/*.nanorc"' > "$STAGE/etc/nanorc"
mkdir -p "$STAGE/usr/share/licenses/nano" && cp "$src/COPYING" "$STAGE/usr/share/licenses/nano/"
ports_package 9.3 "Simple, friendly terminal text editor" etc/nanorc
