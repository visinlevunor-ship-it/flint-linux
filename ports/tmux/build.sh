#!/bin/sh
# tmux, the terminal multiplexer (static). Usage: ports/tmux/build.sh OUTDIR
. "$(dirname "$0")/../common.sh"
ports_init tmux "${1:?usage: build.sh OUTDIR}"
lib_ncurses
lib_libevent
src=$(unpack "$(fetch main/t/tmux/tmux_3.8.orig.tar.gz 349d29e6921b299e9fe56a65f0ec811c75bc722f9dceae25a55676792d09755d)")
say "tmux"
(cd "$src" && { [ -x configure ] || sh autogen.sh >>"$LOG" 2>&1; } && ./configure $HOSTFLAGS --prefix=/usr --enable-static --disable-utf8proc \
    LIBEVENT_CFLAGS="-I$SYSROOT/include" LIBEVENT_LIBS="-levent" \
    LIBTINFO_CFLAGS="-I$SYSROOT/include/ncursesw" LIBTINFO_LIBS="-lncursesw" \
    LIBNCURSES_CFLAGS="-I$SYSROOT/include/ncursesw" LIBNCURSES_LIBS="-lncursesw" >>"$LOG" 2>&1 \
    && make -j"$JOBS" >>"$LOG" 2>&1 && make install-strip DESTDIR="$STAGE" >>"$LOG" 2>&1)
rm -rf "$STAGE/usr/share/man"
mkdir -p "$STAGE/usr/share/licenses/tmux" && cp "$src/COPYING" "$STAGE/usr/share/licenses/tmux/"
ports_package 3.8 "Terminal multiplexer: several shells in one terminal, sessions that survive"
