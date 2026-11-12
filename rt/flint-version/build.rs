// A libc-free static binary: no C startup files, no default libraries.
fn main() {
    println!("cargo:rustc-link-arg-bins=-nostdlib");
    println!("cargo:rustc-link-arg-bins=-static");
    println!("cargo:rustc-link-arg-bins=-no-pie");
}
