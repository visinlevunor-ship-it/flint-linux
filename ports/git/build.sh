#!/bin/sh
# git, the version control system (static; https through libcurl + mbedTLS).
# Usage: ports/git/build.sh OUTDIR
. "$(dirname "$0")/../common.sh"
ports_init git "${1:?usage: build.sh OUTDIR}"
lib_zlib
lib_mbedtls
lib_curl
src=$(unpack "$(fetch main/g/git/git_2.55.0.orig.tar.xz 457fdb04dc8728e007d4688695e6912e6f680727920f2a40bf11eacc17505357)")
say "git"
# curl-config --static-libs names mbedTLS and zlib for the https helper.
CURLLIBS=$("$SYSROOT/bin/curl-config" --static-libs)
gitmake() {
    make prefix=/usr gitexecdir=/usr/libexec/git-core CC="$CC" AR="$AR" CFLAGS="$CFLAGS" LDFLAGS="$LDFLAGS" \
        CURL_CONFIG="$SYSROOT/bin/curl-config" CURL_LDFLAGS="$CURLLIBS" ZLIB_PATH="$SYSROOT" \
        NO_OPENSSL=1 NO_EXPAT=1 NO_GETTEXT=1 NO_TCLTK=1 NO_PERL=1 NO_PYTHON=1 NO_ICONV=1 NO_REGEX=NeedsStartEnd NO_RUST=1 LINK_FUZZ_PROGRAMS= \
        SKIP_DASHED_BUILT_INS=1 INSTALL_SYMLINKS=1 NO_INSTALL_HARDLINKS=1 uname_S=Linux "$@"
}
(cd "$src" && gitmake -j"$JOBS" install DESTDIR="$STAGE" >>"$LOG" 2>&1)
rm -rf "$STAGE/usr/share/perl5" "$STAGE/usr/share/gitweb" "$STAGE/usr/share/git-gui" "$STAGE/usr/share/gitk" "$STAGE/usr/share/man"
mkdir -p "$STAGE/usr/share/licenses/git" && cp "$src/COPYING" "$STAGE/usr/share/licenses/git/"
ports_package 2.55.0 "Distributed version control system"
echo "depends: ca-certificates" >> "$OUT/control"
