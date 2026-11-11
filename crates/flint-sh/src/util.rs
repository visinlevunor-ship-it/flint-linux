//! Direct, unbuffered output on file descriptors. The shell forks a lot, and Rust's buffered
//! stdout would either lose output or duplicate it across forks, so everything goes straight
//! through write(2).

pub fn write_fd(fd: i32, mut data: &[u8]) {
    while !data.is_empty() {
        // SAFETY: pointer and length come from a valid slice.
        let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        if n < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return;
        }
        data = &data[n as usize..];
    }
}

pub fn out(s: &str) {
    write_fd(1, s.as_bytes());
}

pub fn out_bytes(b: &[u8]) {
    write_fd(1, b);
}

/// `sh: message` on stderr.
pub fn errln(msg: &str) {
    write_fd(2, format!("sh: {msg}\n").as_bytes());
}

pub fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

pub fn strerror(e: i32) -> String {
    let s = std::io::Error::from_raw_os_error(e).to_string();
    // "No such file or directory (os error 2)" -> "No such file or directory"
    match s.find(" (os error") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}
