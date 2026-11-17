#!/bin/sh
# Build the Flint kernel: kernel/target/x86_64-unknown-uefi/release/flint-kernel.efi
#
# The UEFI target's core and alloc are built from source (rust-src), so only the
# rust-src component is needed: rustup component add rust-src
set -eu
here=$(cd "$(dirname "$0")" && pwd)
cd "$here"
RUSTC_BOOTSTRAP=1 cargo build --release --target x86_64-unknown-uefi \
    -Zbuild-std=core,alloc,compiler_builtins -Zbuild-std-features=compiler-builtins-mem
echo "built $here/target/x86_64-unknown-uefi/release/flint-kernel.efi"
