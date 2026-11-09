//! Terminal helpers.

use std::io::{self, Read, Write};

/// Show `prompt` on stderr and read one line from stdin with echo off.
/// `None` on EOF or error. The trailing newline is not included.
pub fn read_secret(prompt: &str) -> Option<String> {
    // Echo goes off before the prompt is shown: tcsetattr(TCSAFLUSH) discards pending
    // input, so a fast typist (or a paste) would otherwise lose the start of the line.
    // SAFETY: tcgetattr/tcsetattr on fd 0 with zeroed-then-filled termios structs.
    let saved = unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) == 0 {
            let saved = t;
            t.c_lflag &= !(libc::ECHO | libc::ECHONL);
            libc::tcsetattr(0, libc::TCSAFLUSH, &t);
            Some(saved)
        } else {
            None
        }
    };
    eprint!("{prompt}");
    let _ = io::stderr().flush();

    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    let mut stdin = io::stdin().lock();
    let got = loop {
        match stdin.read(&mut byte) {
            Ok(0) => break !line.is_empty(),
            Ok(_) if byte[0] == b'\n' => break true,
            Ok(_) => line.push(byte[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break false,
        }
    };

    if let Some(t) = saved {
        // SAFETY: restoring settings read from the same descriptor.
        unsafe { libc::tcsetattr(0, libc::TCSAFLUSH, &t) };
    }
    eprintln!();
    got.then(|| String::from_utf8_lossy(&line).into_owned())
}
