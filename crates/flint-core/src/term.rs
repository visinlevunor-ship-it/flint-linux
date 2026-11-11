//! Terminal handling for full-screen and line-editing programs (less, vi, sh): raw mode,
//! window size, reading keys (including escape sequences), and display width.

use std::io::{self, Write};
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Set by SIGWINCH: the window changed size.
pub static RESIZED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_winch(_: libc::c_int) {
    RESIZED.store(true, Ordering::SeqCst);
}

pub fn watch_resize() {
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_winch as *const () as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigaction(libc::SIGWINCH, &sa, std::ptr::null_mut());
    }
}

pub fn is_tty(fd: RawFd) -> bool {
    // SAFETY: plain query.
    unsafe { libc::isatty(fd) == 1 }
}

/// (rows, columns); 24x80 if unknown.
pub fn size(fd: RawFd) -> (usize, usize) {
    // SAFETY: TIOCGWINSZ fills a winsize.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
            return (ws.ws_row as usize, ws.ws_col as usize);
        }
    }
    let env = |k: &str, d: usize| std::env::var(k).ok().and_then(|v| v.parse().ok()).filter(|&v: &usize| v > 0).unwrap_or(d);
    (env("LINES", 24), env("COLUMNS", 80))
}

/// Raw mode until dropped: no echo, no line buffering, no signal keys, no output processing
/// unless `keep_output` (then "\n" still becomes "\r\n").
pub struct Raw {
    fd: RawFd,
    saved: libc::termios,
}

impl Raw {
    pub fn enter(fd: RawFd, keep_output: bool) -> io::Result<Raw> {
        // SAFETY: tcgetattr/tcsetattr on a terminal descriptor with a filled termios.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut t) != 0 {
                return Err(io::Error::last_os_error());
            }
            let saved = t;
            t.c_iflag &= !(libc::BRKINT | libc::ICRNL | libc::INPCK | libc::ISTRIP | libc::IXON);
            if !keep_output {
                t.c_oflag &= !libc::OPOST;
            }
            t.c_cflag |= libc::CS8;
            t.c_lflag &= !(libc::ECHO | libc::ICANON | libc::IEXTEN | libc::ISIG);
            t.c_cc[libc::VMIN] = 1;
            t.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(fd, libc::TCSADRAIN, &t) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Raw { fd, saved })
        }
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        // SAFETY: restoring what was read in enter().
        unsafe { libc::tcsetattr(self.fd, libc::TCSADRAIN, &self.saved) };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    /// Ctrl plus a letter: Ctrl('a') is byte 1.
    Ctrl(char),
    Alt(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Esc,
    Up,
    Down,
    Left,
    Right,
    CtrlLeft,
    CtrlRight,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    /// Nothing arrived before the timeout (or a resize interrupted the wait).
    None,
    Eof,
}

fn read_byte(fd: RawFd, timeout: Option<Duration>) -> io::Result<Option<u8>> {
    let ms = timeout.map_or(-1, |t| t.as_millis() as i32);
    let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // SAFETY: one pollfd.
    let r = unsafe { libc::poll(&mut p, 1, ms) };
    if r < 0 {
        let e = io::Error::last_os_error();
        return if e.kind() == io::ErrorKind::Interrupted { Ok(None) } else { Err(e) };
    }
    if r == 0 {
        return Ok(None);
    }
    let mut b = [0u8; 1];
    // SAFETY: reading one byte into our buffer.
    let n = unsafe { libc::read(fd, b.as_mut_ptr().cast(), 1) };
    match n {
        1 => Ok(Some(b[0])),
        0 => Err(io::ErrorKind::UnexpectedEof.into()),
        _ => {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                Ok(None)
            } else {
                Err(e)
            }
        }
    }
}

/// Read one key. `timeout` None waits forever (but returns Key::None on a resize).
pub fn read_key(fd: RawFd, timeout: Option<Duration>) -> io::Result<Key> {
    let b = match read_byte(fd, timeout) {
        Ok(Some(b)) => b,
        Ok(None) => return Ok(Key::None),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(Key::Eof),
        Err(e) => return Err(e),
    };
    let short = Some(Duration::from_millis(40));
    Ok(match b {
        b'\r' | b'\n' => Key::Enter,
        b'\t' => Key::Tab,
        0x7F | 0x08 => Key::Backspace,
        0x1B => {
            let Some(n) = read_byte(fd, short)? else { return Ok(Key::Esc) };
            match n {
                b'[' | b'O' => {
                    // CSI / SS3: parameters then a final byte
                    let mut params = Vec::new();
                    let fin = loop {
                        let Some(c) = read_byte(fd, short)? else { return Ok(Key::Esc) };
                        if (0x40..=0x7E).contains(&c) {
                            break c;
                        }
                        params.push(c);
                        if params.len() > 16 {
                            return Ok(Key::None);
                        }
                    };
                    let p = String::from_utf8_lossy(&params).into_owned();
                    let ctrl = p.contains(";5");
                    match fin {
                        b'A' => Key::Up,
                        b'B' => Key::Down,
                        b'C' => {
                            if ctrl {
                                Key::CtrlRight
                            } else {
                                Key::Right
                            }
                        }
                        b'D' => {
                            if ctrl {
                                Key::CtrlLeft
                            } else {
                                Key::Left
                            }
                        }
                        b'H' => Key::Home,
                        b'F' => Key::End,
                        b'Z' => Key::BackTab,
                        b'~' => match p.split(';').next().unwrap_or("") {
                            "1" | "7" => Key::Home,
                            "4" | "8" => Key::End,
                            "2" => Key::Insert,
                            "3" => Key::Delete,
                            "5" => Key::PageUp,
                            "6" => Key::PageDown,
                            _ => Key::None,
                        },
                        _ => Key::None,
                    }
                }
                0x7F => Key::Alt('\x7F'),
                c if c.is_ascii() => Key::Alt(c as char),
                _ => Key::Esc,
            }
        }
        1..=26 => Key::Ctrl((b'a' + b - 1) as char),
        0 => Key::Ctrl('@'),
        0x1C..=0x1F => Key::Ctrl((b'\\' + b - 0x1C) as char),
        b if b < 0x80 => Key::Char(b as char),
        lead => {
            // A UTF-8 sequence.
            let len = if lead >= 0xF0 { 4 } else if lead >= 0xE0 { 3 } else { 2 };
            let mut buf = vec![lead];
            for _ in 1..len {
                match read_byte(fd, short)? {
                    Some(c) => buf.push(c),
                    None => break,
                }
            }
            match std::str::from_utf8(&buf).ok().and_then(|s| s.chars().next()) {
                Some(c) => Key::Char(c),
                None => Key::None,
            }
        }
    })
}

/// Columns a character takes on screen: 0 for combining marks, 2 for wide East Asian
/// characters and emoji, 1 otherwise. (A compact table of the common ranges.)
pub fn char_width(c: char) -> usize {
    let u = c as u32;
    if u < 0x300 {
        return 1;
    }
    if (0x300..=0x36F).contains(&u) || (0x200B..=0x200F).contains(&u) || (0xFE00..=0xFE0F).contains(&u) || (0x20D0..=0x20FF).contains(&u) {
        return 0;
    }
    let wide = (0x1100..=0x115F).contains(&u)
        || (0x2E80..=0xA4CF).contains(&u) && u != 0x303F
        || (0xAC00..=0xD7A3).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
        || (0xFE30..=0xFE4F).contains(&u)
        || (0xFF00..=0xFF60).contains(&u)
        || (0xFFE0..=0xFFE6).contains(&u)
        || (0x1F300..=0x1F64F).contains(&u)
        || (0x1F900..=0x1F9FF).contains(&u)
        || (0x20000..=0x3FFFD).contains(&u);
    if wide {
        2
    } else {
        1
    }
}

pub fn str_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// A screen buffer written in one go (less flicker than many small writes).
pub struct Screen {
    pub buf: String,
}

impl Screen {
    pub fn new() -> Screen {
        Screen { buf: String::with_capacity(16384) }
    }

    pub fn goto(&mut self, row: usize, col: usize) {
        self.buf.push_str(&format!("\x1b[{};{}H", row + 1, col + 1));
    }

    pub fn clear_line(&mut self) {
        self.buf.push_str("\x1b[K");
    }

    pub fn clear(&mut self) {
        self.buf.push_str("\x1b[H\x1b[2J");
    }

    pub fn reverse(&mut self, on: bool) {
        self.buf.push_str(if on { "\x1b[7m" } else { "\x1b[0m" });
    }

    pub fn hide_cursor(&mut self, hide: bool) {
        self.buf.push_str(if hide { "\x1b[?25l" } else { "\x1b[?25h" });
    }

    pub fn push(&mut self, s: &str) {
        self.buf.push_str(s);
    }

    pub fn flush(&mut self, out: &mut impl Write) -> io::Result<()> {
        out.write_all(self.buf.as_bytes())?;
        out.flush()?;
        self.buf.clear();
        Ok(())
    }
}

impl Default for Screen {
    fn default() -> Self {
        Self::new()
    }
}

/// Switch to the alternate screen (full-screen programs leave the scrollback alone).
pub fn alt_screen(out: &mut impl Write, on: bool) -> io::Result<()> {
    out.write_all(if on { b"\x1b[?1049h\x1b[H" } else { b"\x1b[?1049l" })?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths() {
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width('é'), 1);
        assert_eq!(char_width('\u{301}'), 0);
        assert_eq!(char_width('漢'), 2);
        assert_eq!(char_width('😀'), 2);
        assert_eq!(str_width("ab漢"), 4);
    }

    #[test]
    fn keys_from_a_pipe() {
        let mut fds = [0; 2];
        // SAFETY: a fresh pipe.
        unsafe { libc::pipe(fds.as_mut_ptr()) };
        let input = b"a\x1b[A\x1b[3~\x01\x1bx\xc3\xa9\r\x1b";
        // SAFETY: writing our buffer to the pipe.
        unsafe { libc::write(fds[1], input.as_ptr().cast(), input.len()) };
        let k = |_: ()| read_key(fds[0], Some(Duration::from_millis(100))).unwrap();
        assert_eq!(k(()), Key::Char('a'));
        assert_eq!(k(()), Key::Up);
        assert_eq!(k(()), Key::Delete);
        assert_eq!(k(()), Key::Ctrl('a'));
        assert_eq!(k(()), Key::Alt('x'));
        assert_eq!(k(()), Key::Char('é'));
        assert_eq!(k(()), Key::Enter);
        assert_eq!(k(()), Key::Esc);
        assert_eq!(k(()), Key::None);
        // SAFETY: closing our pipe.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }
}
