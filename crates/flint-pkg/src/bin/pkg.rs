//! Standalone `pkg`, for building packages and repositories on the build machine
//! (any OS). On Flint itself pkg runs inside the `flint` binary.

fn main() {
    std::process::exit(flint_pkg::main(std::env::args().collect()));
}
