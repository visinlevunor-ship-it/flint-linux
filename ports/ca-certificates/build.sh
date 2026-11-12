#!/bin/sh
# The Mozilla CA certificate list (from Ubuntu's ca-certificates), as the bundle curl, git
# and others read: /etc/ssl/certs/ca-certificates.crt. Usage: build.sh OUTDIR
. "$(dirname "$0")/../common.sh"
ports_init ca-certificates "${1:?usage: build.sh OUTDIR}"
deb=$(fetch main/c/ca-certificates/ca-certificates_20260816_all.deb 2ccfb29b1ccdf266948f2a9f9e6353b003e341004a8f67ddda166c416dfd8a68)
mkdir -p "$WORK/deb" && (cd "$WORK/deb" && ar x "$deb" && tar -xf data.tar.*)
mkdir -p "$STAGE/etc/ssl/certs"
n=0
for c in "$WORK"/deb/usr/share/ca-certificates/mozilla/*.crt; do
    cat "$c" >> "$STAGE/etc/ssl/certs/ca-certificates.crt"
    n=$((n + 1))
done
[ "$n" -gt 100 ] || die "only $n certificates found"
chmod 644 "$STAGE/etc/ssl/certs/ca-certificates.crt"
ln -s certs/ca-certificates.crt "$STAGE/etc/ssl/cert.pem"
say "$n certificates"
ports_package 20260816 "Trusted certificate authorities (Mozilla's list) for TLS"
