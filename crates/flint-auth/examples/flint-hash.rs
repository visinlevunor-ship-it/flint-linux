//! Print a Flint password hash for the password read from stdin (one line).
//! Builds on any host: `echo 'secret' | cargo run -q -p flint-auth --example flint-hash`
//! The output goes in the second field of an /etc/shadow line.

use std::io::BufRead;

fn main() {
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        eprintln!("flint-hash: no password on stdin");
        std::process::exit(1);
    }
    let password = line.trim_end_matches(['\n', '\r']);
    match flint_auth::hash::hash_new(password) {
        Ok(h) => println!("{h}"),
        Err(e) => {
            eprintln!("flint-hash: cannot get random bytes: {e}");
            std::process::exit(1);
        }
    }
}
