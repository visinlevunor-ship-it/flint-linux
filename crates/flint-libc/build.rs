// Rust's std for x86_64-unknown-linux-musl asks the linker for -lc and -lunwind. flint-libc
// provides every one of those functions itself, so it hands the linker two empty archives
// under those names: nothing of musl or LLVM's libunwind gets linked.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_env != "musl" || target_os != "linux" {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    for name in ["libc.a", "libunwind.a"] {
        fs::write(out.join(name), b"!<arch>\n").expect("writing an empty archive");
    }
    println!("cargo:rustc-link-search=native={}", out.display());
}
