#!/bin/sh
# Build flint-glibc: one shared object that is glibc's dynamic loader and C library.
#
#   rt/flint-glibc/build.sh OUTDIR
#
# OUTDIR gets ld-flint.so; install it as /lib64/ld-linux-x86-64.so.2 (the path every glibc
# program names as its interpreter). The loader answers for libc.so.6, libm.so.6,
# libpthread.so.0 and glibc's other libraries itself, so no other files are needed.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${1:?usage: build.sh OUTDIR}
mkdir -p "$out"
target=x86_64-unknown-linux-musl
tdir=${CARGO_TARGET_DIR:-$here/../target}

# position-independent code for a shared object; RUSTFLAGS replaces the static-program
# flags in .cargo/config.toml
(cd "$here" && RUSTC_BOOTSTRAP=1 \
    RUSTFLAGS="-C relocation-model=pic -C panic=abort -C link-self-contained=no -C target-feature=-crt-static -C force-unwind-tables=yes" \
    cargo build --release --target $target -p flint-glibc \
        -Zbuild-std=core,alloc,compiler_builtins --target-dir "$tdir")

lib="$tdir/$target/release/libflint_glibc.a"

# compiler_builtins carries weak, hidden copies of some libm functions (floor, fma, sqrt...).
# The linker gives a symbol the most restrictive visibility of all its definitions, so those
# copies would hide ours from programs. Make them local to compiler_builtins' object.
# LLVM's tools when installed under these names (FreeBSD's base ones work too)
AR=${AR:-$(command -v llvm-ar || echo ar)}
NM=${NM:-$(command -v llvm-nm || echo nm)}
OBJCOPY=${OBJCOPY:-$(command -v llvm-objcopy || echo objcopy)}
work="$out/.ar"
rm -rf "$work"
mkdir -p "$work"
cp "$lib" "$work/lib.a"
lib="$work/lib.a"
cb=$("$AR" t "$lib" | grep '^compiler_builtins' || true)
if [ -n "$cb" ]; then
    "$NM" "$lib" 2>/dev/null | awk '$2 == "T" { print $3 }' | sort -u > "$work/strong"
    for m in $cb; do
        (cd "$work" && "$AR" x lib.a "$m")
        "$NM" "$work/$m" | awk '$2 == "W" { print $3 }' | sort -u > "$work/weak"
        comm -12 "$work/strong" "$work/weak" > "$work/dups"
        if [ -s "$work/dups" ]; then
            "$OBJCOPY" --localize-symbols="$work/dups" "$work/$m"
            (cd "$work" && "$AR" r lib.a "$m")
        fi
        rm -f "$work/$m"
    done
fi

lld=$(command -v ld.lld || true)
if [ -z "$lld" ]; then
    lld=$(find "$(rustc --print sysroot)" -name rust-lld -type f | head -n 1)
    lld="$lld -flavor gnu"
fi

# Export the C functions; keep Rust's own symbols local.
cat > "$out/flint-glibc.map" <<'EOF'
{
  global: *;
  local: _ZN*; _R*; __rust*; rust_*; __rdl_*; __rg_*; __flint_*;
};
EOF

# glibc's aliases: several names for one variable
$lld -shared -o "$out/ld-flint.so" \
    --soname=ld-linux-x86-64.so.2 \
    -e _dl_start \
    --version-script="$out/flint-glibc.map" \
    -z now -z relro -z noexecstack -z max-page-size=4096 \
    --hash-style=both --eh-frame-hdr --gc-sections \
    --defsym=environ=__environ --defsym=_environ=__environ \
    --defsym=__progname_full=program_invocation_name \
    --defsym=__progname=program_invocation_short_name \
    --defsym=res_ninit=__res_ninit --defsym=res_init=__res_init --defsym=res_nclose=__res_nclose \
    --defsym=res_close=__res_close --defsym=res_nquery=__res_nquery --defsym=res_nsearch=__res_nsearch \
    --defsym=res_query=__res_query --defsym=res_search=__res_search \
    --defsym=dn_expand=__dn_expand --defsym=res_nmkquery=__res_nmkquery \
    --defsym=res_nsend=__res_nsend --defsym=res_send=__res_send --defsym=res_mkquery=__res_nmkquery \
    --defsym=dn_skipname=__dn_skipname \
    --defsym=__sysconf=sysconf --defsym=__getpagesize=getpagesize --defsym=sendfile64=sendfile \
    --defsym=timezone=__timezone --defsym=daylight=__daylight --defsym=tzname=__tzname \
    --whole-archive "$lib" --no-whole-archive
rm -rf "$out/flint-glibc.map" "$work"
echo "built $out/ld-flint.so"
