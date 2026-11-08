#!/bin/sh
# Boot a Flint initramfs in QEMU on the serial console.
#
#   KERNEL=/path/to/bzImage image/run-qemu.sh
#
# Environment:
#   KERNEL   (required) Linux x86_64 kernel image (bzImage / vmlinuz)
#   INITRD   initramfs to boot        (default target/flint-initramfs.cpio.gz)
#   MEM      guest memory             (default 256M)
#   ACCEL    extra qemu acceleration flags, e.g. "-enable-kvm" on a Linux host
#   APPEND   extra kernel command line words
#
# Quit QEMU with Ctrl-A then X. -no-reboot makes `reboot` exit QEMU instead of restarting.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
INITRD=${INITRD:-$ROOT/target/flint-initramfs.cpio.gz}
: "${KERNEL:?set KERNEL to a Linux x86_64 kernel image}"

# shellcheck disable=SC2086
exec qemu-system-x86_64 \
    -m "${MEM:-256M}" \
    -kernel "$KERNEL" \
    -initrd "$INITRD" \
    -append "console=ttyS0 panic=-1 ${APPEND:-}" \
    -nographic -no-reboot ${ACCEL:-}
