#!/bin/sh
# curl, the URL transfer tool (static, TLS by mbedTLS). Usage: ports/curl/build.sh OUTDIR
. "$(dirname "$0")/../common.sh"
ports_init curl "${1:?usage: build.sh OUTDIR}"
lib_zlib
lib_mbedtls
lib_curl
mkdir -p "$STAGE/usr/bin" "$STAGE/usr/share/licenses/curl"
cp "$SYSROOT/bin/curl" "$STAGE/usr/bin/curl"
cp "$WORK"/src/curl_8.23.0/*/COPYING "$STAGE/usr/share/licenses/curl/"
ports_package 8.23.0 "Transfer data with URLs (HTTP, HTTPS, FTP, ...)"
echo "depends: ca-certificates" >> "$OUT/control"
