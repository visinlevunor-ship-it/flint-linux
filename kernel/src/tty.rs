//! The console terminal: Linux's terminal settings (termios), line editing when a program
//! reads whole lines (canonical mode), echo, and Ctrl-C / Ctrl-\ for the foreground process
//! group.

use crate::console;
use crate::proc;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

const ICRNL: u32 = 0x100;
const ISIG: u32 = 0x1;
const ICANON: u32 = 0x2;
const ECHO: u32 = 0x8;
const ECHOE: u32 = 0x10;
const ECHOCTL: u32 = 0x200;

const VINTR: usize = 0;
const VQUIT: usize = 1;
const VERASE: usize = 2;
const VKILL: usize = 3;
const VEOF: usize = 4;
const VTIME: usize = 5;
const VMIN: usize = 6;

/// struct termios as the kernel passes it (TCGETS/TCSETS): 36 bytes
#[derive(Clone, Copy)]
pub struct Termios {
    pub iflag: u32,
    pub oflag: u32,
    pub cflag: u32,
    pub lflag: u32,
    pub line: u8,
    pub cc: [u8; 19],
}

impl Termios {
    pub fn to_bytes(&self) -> [u8; 36] {
        let mut b = [0u8; 36];
        b[0..4].copy_from_slice(&self.iflag.to_le_bytes());
        b[4..8].copy_from_slice(&self.oflag.to_le_bytes());
        b[8..12].copy_from_slice(&self.cflag.to_le_bytes());
        b[12..16].copy_from_slice(&self.lflag.to_le_bytes());
        b[16] = self.line;
        b[17..36].copy_from_slice(&self.cc);
        b
    }
    pub fn from_bytes(b: &[u8]) -> Termios {
        let u = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let mut cc = [0u8; 19];
        cc.copy_from_slice(&b[17..36]);
        Termios { iflag: u(0), oflag: u(4), cflag: u(8), lflag: u(12), line: b[16], cc }
    }
}

pub struct Tty {
    pub termios: Termios,
    /// bytes ready for read()
    ready: VecDeque<u8>,
    /// the line being edited (canonical mode)
    line: Vec<u8>,
    /// Ctrl-D on an empty line: the next read returns 0
    eof: usize,
    pub fg_pgrp: i32,
}

static mut TTY: Tty = Tty {
    termios: Termios {
        iflag: 0x500,  // ICRNL | IXON
        oflag: 0x5,    // OPOST | ONLCR
        cflag: 0xBF,   // B38400 | CS8 | CREAD | HUPCL
        lflag: 0x8A3B, // ISIG ICANON ECHO ECHOE ECHOK ECHOCTL ECHOKE IEXTEN
        line: 0,
        cc: [3, 0x1C, 0x7F, 0x15, 4, 0, 1, 0, 0x11, 0x13, 0x1A, 0, 0x12, 0x0F, 0x17, 0x16, 0, 0, 0],
    },
    ready: VecDeque::new(),
    line: Vec::new(),
    eof: 0,
    fg_pgrp: 1,
};

pub fn tty() -> &'static mut Tty {
    // SAFETY: one CPU; interrupt handlers and system calls never run at the same time
    // (interrupts are off in the kernel except while it waits).
    unsafe { &mut *core::ptr::addr_of_mut!(TTY) }
}

fn echo(b: &[u8]) {
    console::write_bytes(b);
}

/// A byte from the keyboard or the serial port.
pub fn input(mut b: u8) {
    let t = tty();
    let tio = t.termios;
    if b == b'\r' && tio.iflag & ICRNL != 0 {
        b = b'\n';
    }
    if tio.lflag & ISIG != 0 && (b == tio.cc[VINTR] || b == tio.cc[VQUIT]) && b != 0 {
        let sig = if b == tio.cc[VINTR] { 2 } else { 3 };
        if tio.lflag & ECHO != 0 {
            echo(if sig == 2 { b"^C\n" } else { b"^\\\n" });
        }
        t.line.clear();
        t.ready.clear();
        proc::signal_group(t.fg_pgrp, sig);
        proc::wake_all();
        return;
    }
    if tio.lflag & ICANON != 0 {
        if b == tio.cc[VERASE] || b == 0x08 {
            if t.line.pop().is_some() && tio.lflag & ECHO != 0 && tio.lflag & ECHOE != 0 {
                echo(b"\x08 \x08");
            }
        } else if b == tio.cc[VKILL] {
            while t.line.pop().is_some() {
                if tio.lflag & ECHO != 0 {
                    echo(b"\x08 \x08");
                }
            }
        } else if b == tio.cc[VEOF] {
            if t.line.is_empty() {
                t.eof += 1;
            } else {
                t.ready.extend(t.line.drain(..));
            }
        } else if b == b'\n' {
            t.line.push(b'\n');
            t.ready.extend(t.line.drain(..));
            if tio.lflag & ECHO != 0 {
                echo(b"\n");
            }
        } else {
            t.line.push(b);
            if tio.lflag & ECHO != 0 {
                if b < 0x20 && b != b'\t' && tio.lflag & ECHOCTL != 0 {
                    echo(&[b'^', b + 0x40]);
                } else {
                    echo(&[b]);
                }
            }
        }
    } else {
        t.ready.push_back(b);
        if tio.lflag & ECHO != 0 {
            echo(&[b]);
        }
    }
    proc::wake_all();
}

/// read() from the terminal: None while it would have to wait.
pub fn try_read(out: &mut [u8]) -> Option<usize> {
    let t = tty();
    let canon = t.termios.lflag & ICANON != 0;
    if canon {
        if let Some(i) = t.ready.iter().position(|&c| c == b'\n') {
            let k = (i + 1).min(out.len());
            for o in out.iter_mut().take(k) {
                *o = t.ready.pop_front().unwrap();
            }
            return Some(k);
        }
        if t.eof > 0 {
            if !t.ready.is_empty() {
                let k = t.ready.len().min(out.len());
                for o in out.iter_mut().take(k) {
                    *o = t.ready.pop_front().unwrap();
                }
                return Some(k);
            }
            t.eof -= 1;
            return Some(0);
        }
        return None;
    }
    let vmin = t.termios.cc[VMIN];
    if t.ready.is_empty() {
        // VMIN 0 with VTIME 0: answer at once
        return if vmin == 0 && t.termios.cc[VTIME] == 0 { Some(0) } else { None };
    }
    let k = t.ready.len().min(out.len());
    for o in out.iter_mut().take(k) {
        *o = t.ready.pop_front().unwrap();
    }
    Some(k)
}

/// Is there something to read (poll)?
pub fn readable() -> bool {
    let t = tty();
    if t.termios.lflag & ICANON != 0 {
        t.eof > 0 || t.ready.contains(&b'\n')
    } else {
        !t.ready.is_empty()
    }
}
