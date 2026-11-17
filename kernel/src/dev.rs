//! Interrupts from devices: the legacy interrupt controllers (8259), the timer (PIT, 100 Hz),
//! the keyboard (i8042: PS/2, which is also how most laptops connect their own keyboard) and
//! input from the serial port.

use crate::console::{inb, outb};
use crate::{proc, tty};

pub const HZ: u64 = 100;
static mut TICKS: u64 = 0;

pub fn ticks() -> u64 {
    // SAFETY: a read of a counter only the timer interrupt writes.
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(TICKS)) }
}

pub fn init() {
    // SAFETY: the 8259s, the PIT and the i8042 at their fixed ports; nothing else drives them.
    unsafe {
        // 8259s: lines 0-7 at vectors 32-39, 8-15 at 40-47
        outb(0x20, 0x11);
        outb(0xA0, 0x11);
        outb(0x21, 32);
        outb(0xA1, 40);
        outb(0x21, 4);
        outb(0xA1, 2);
        outb(0x21, 1);
        outb(0xA1, 1);
        // only the timer (0), the keyboard (1) and the cascade (2)
        outb(0x21, !0b0000_0111);
        outb(0xA1, 0xFF);
        // PIT channel 0: square wave at HZ
        let div = (1_193_182 / HZ) as u16;
        outb(0x43, 0x36);
        outb(0x40, div as u8);
        outb(0x40, (div >> 8) as u8);
        // the keyboard: drop anything left from the firmware
        for _ in 0..32 {
            if inb(0x64) & 1 == 0 {
                break;
            }
            inb(0x60);
        }
    }
}

fn eoi(line: u8) {
    // SAFETY: end-of-interrupt to the 8259s.
    unsafe {
        if line >= 8 {
            outb(0xA0, 0x20);
        }
        outb(0x20, 0x20);
    }
}

/// Devices that are polled rather than interrupting (called while idle).
pub fn poll_devices() {
    crate::xhci::poll();
}

/// Restart the machine: the keyboard controller's reset line, then the PCI reset register.
pub fn reboot() -> ! {
    crate::kprintln!("[kernel] restarting");
    // SAFETY: reset requests to the i8042 and the chipset's reset control register.
    unsafe {
        for _ in 0..100_000 {
            if inb(0x64) & 2 == 0 {
                break;
            }
        }
        outb(0x64, 0xFE);
        outb(0xCF9, 0x06);
    }
    crate::arch::halt()
}

/// Switch off: QEMU's and Bochs' power-off ports; on other machines (without ACPI support
/// yet) the machine just stops.
pub fn power_off() -> ! {
    crate::kprintln!("[kernel] switching off");
    // SAFETY: power-off requests on the emulators' ports; harmless elsewhere.
    unsafe {
        crate::pci::outw(0x604, 0x2000);
        crate::pci::outw(0xB004, 0x2000);
        crate::pci::outw(0x4004, 0x3400);
    }
    crate::kprintln!("[kernel] the machine can be switched off now");
    crate::arch::halt()
}

/// Bytes waiting on the serial port.
pub fn poll_serial() {
    // SAFETY: COM1's registers.
    unsafe {
        let mut n = 0;
        while inb(0x3F8 + 5) & 1 != 0 && inb(0x3F8 + 5) != 0xFF && n < 64 {
            tty::input(inb(0x3F8));
            n += 1;
        }
    }
}

/// A device interrupt on legacy line `line`. `from_user`: it arrived while a program ran.
pub fn irq(line: u8, frame: Option<&mut crate::arch::IntFrame>) {
    match line {
        0 => {
            // SAFETY: only this interrupt writes TICKS.
            unsafe { TICKS += 1 };
            poll_serial();
            poll_devices();
            eoi(0);
            proc::tick(frame);
        }
        1 => {
            // SAFETY: the i8042's data port.
            unsafe {
                while inb(0x64) & 1 != 0 {
                    let status = inb(0x64);
                    let code = inb(0x60);
                    if status & 0x20 == 0 {
                        keyboard(code);
                    }
                }
            }
            eoi(1);
        }
        7 | 15 => {} // spurious: no end-of-interrupt
        _ => eoi(line),
    }
}

// ---------------------------------------------------------------------------------------
// The keyboard: scancode set 1 (what the i8042 translates to), US layout

static mut SHIFT: bool = false;
static mut CTRL: bool = false;
static mut ALT: bool = false;
static mut CAPS: bool = false;
static mut E0: bool = false;

const NORMAL: &[u8; 58] = b"\0\x1b1234567890-=\x7f\tqwertyuiop[]\n\0asdfghjkl;'`\0\\zxcvbnm,./\0*\0 ";
const SHIFTED: &[u8; 58] = b"\0\x1b!@#$%^&*()_+\x7f\tQWERTYUIOP{}\n\0ASDFGHJKL:\"~\0|ZXCVBNM<>?\0*\0 ";

fn send(bytes: &[u8]) {
    for &b in bytes {
        tty::input(b);
    }
}

fn keyboard(code: u8) {
    // SAFETY: only the keyboard interrupt touches the modifier state.
    unsafe {
        if code == 0xE0 {
            E0 = true;
            return;
        }
        let ext = core::mem::replace(&mut E0, false);
        let up = code & 0x80 != 0;
        let key = code & 0x7F;
        match key {
            0x2A | 0x36 => {
                SHIFT = !up;
                return;
            }
            0x1D => {
                CTRL = !up;
                return;
            }
            0x38 => {
                ALT = !up;
                return;
            }
            0x3A => {
                if !up {
                    CAPS = !CAPS;
                }
                return;
            }
            _ => {}
        }
        if up {
            return;
        }
        if ext {
            match key {
                0x48 => send(b"\x1b[A"),
                0x50 => send(b"\x1b[B"),
                0x4D => send(b"\x1b[C"),
                0x4B => send(b"\x1b[D"),
                0x47 => send(b"\x1b[H"),
                0x4F => send(b"\x1b[F"),
                0x53 => send(b"\x1b[3~"),
                0x49 => send(b"\x1b[5~"),
                0x51 => send(b"\x1b[6~"),
                0x52 => send(b"\x1b[2~"),
                0x1C => send(b"\n"),
                0x35 => send(b"/"),
                _ => {}
            }
            return;
        }
        if key as usize >= NORMAL.len() {
            return;
        }
        let mut c = if SHIFT { SHIFTED[key as usize] } else { NORMAL[key as usize] };
        if CAPS && c.is_ascii_alphabetic() {
            c ^= 0x20;
        }
        if c == 0 {
            return;
        }
        if CTRL && c.is_ascii_alphabetic() {
            c = c.to_ascii_lowercase() & 0x1F;
        } else if CTRL && c == b' ' {
            c = 0;
        }
        if ALT {
            send(&[0x1B, c]);
        } else {
            send(&[c]);
        }
    }
}
