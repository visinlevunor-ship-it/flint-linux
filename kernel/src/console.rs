//! Kernel output: the first serial port (COM1, 115200 8N1) and the UEFI framebuffer, drawn
//! with an 8x16 font.

use crate::font;
use core::fmt;

const COM1: u16 = 0x3F8;

pub unsafe fn outb(port: u16, v: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack, preserves_flags));
}

pub unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    core::arch::asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags));
    v
}

pub fn serial_init() {
    // SAFETY: COM1's registers; nothing else drives the port.
    unsafe {
        outb(COM1 + 1, 0x00); // no interrupts
        outb(COM1 + 3, 0x80); // divisor latch
        outb(COM1, 0x01); // 115200
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x03); // 8N1
        outb(COM1 + 2, 0xC7); // FIFOs on, cleared
        outb(COM1 + 4, 0x03);
    }
}

fn serial_byte(b: u8) {
    // SAFETY: COM1's registers.
    unsafe {
        let mut spins = 0;
        while inb(COM1 + 5) & 0x20 == 0 && spins < 5_000_000 {
            spins += 1;
        }
        outb(COM1, b);
    }
}

pub struct Framebuffer {
    pub base: *mut u32,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub bgr: bool,
    pub scale: usize,
    col: usize,
    row: usize,
    cols: usize,
    rows: usize,
    esc: u8, // 0 none, 1 after ESC, 2 in ESC [
    params: [u16; 4],
    nparams: usize,
}

pub static mut FB: Option<Framebuffer> = None;

const FG: u32 = 0x00D8D8D8;
const BG: u32 = 0x00000000;

impl Framebuffer {
    pub fn new(base: *mut u32, width: usize, height: usize, stride: usize, bgr: bool) -> Framebuffer {
        let scale = if width >= 2400 { 2 } else { 1 };
        let cols = width / (font::WIDTH * scale);
        let rows = height / (font::HEIGHT * scale);
        Framebuffer { base, width, height, stride, bgr, scale, col: 0, row: 0, cols, rows, esc: 0, params: [0; 4], nparams: 0 }
    }

    /// (rows, columns) of text
    pub fn size(&self) -> (usize, usize) {
        (self.rows, self.cols)
    }

    fn pixel(&self, color: u32) -> u32 {
        if self.bgr {
            color
        } else {
            // RGB byte order: swap red and blue
            ((color & 0xFF) << 16) | (color & 0xFF00) | ((color >> 16) & 0xFF)
        }
    }

    pub fn clear(&mut self) {
        let bg = self.pixel(BG);
        for y in 0..self.height {
            for x in 0..self.width {
                // SAFETY: inside the framebuffer the firmware gave us.
                unsafe { self.base.add(y * self.stride + x).write_volatile(bg) };
            }
        }
        self.col = 0;
        self.row = 0;
    }

    fn glyph(&mut self, ch: u8, col: usize, row: usize) {
        let idx = if (font::FIRST..font::FIRST + 95).contains(&ch) { (ch - font::FIRST) as usize } else { (b'?' - font::FIRST) as usize };
        let g = &font::GLYPHS[idx];
        let (fg, bg) = (self.pixel(FG), self.pixel(BG));
        let s = self.scale;
        let x0 = col * font::WIDTH * s;
        let y0 = row * font::HEIGHT * s;
        for (gy, bits) in g.iter().enumerate() {
            for gx in 0..font::WIDTH {
                let on = bits & (0x80 >> gx) != 0;
                for dy in 0..s {
                    for dx in 0..s {
                        let (x, y) = (x0 + gx * s + dx, y0 + gy * s + dy);
                        // SAFETY: col < cols and row < rows keep (x, y) on the screen.
                        unsafe { self.base.add(y * self.stride + x).write_volatile(if on { fg } else { bg }) };
                    }
                }
            }
        }
    }

    fn scroll(&mut self) {
        let line = font::HEIGHT * self.scale;
        let keep = (self.rows - 1) * line;
        // SAFETY: both ranges are inside the framebuffer; copy moves rows upwards.
        unsafe {
            for y in 0..keep {
                core::ptr::copy(self.base.add((y + line) * self.stride), self.base.add(y * self.stride), self.width);
            }
            let bg = self.pixel(BG);
            for y in keep..self.rows * line {
                for x in 0..self.width {
                    self.base.add(y * self.stride + x).write_volatile(bg);
                }
            }
        }
        self.row = self.rows - 1;
    }

    fn newline(&mut self) {
        self.col = 0;
        self.row += 1;
        if self.row >= self.rows {
            self.scroll();
        }
    }

    /// Clear cells [c0, c1) of text row `row`.
    fn clear_cells(&mut self, row: usize, c0: usize, c1: usize) {
        let bg = self.pixel(BG);
        let s = self.scale;
        for y in row * font::HEIGHT * s..(row + 1) * font::HEIGHT * s {
            for x in c0 * font::WIDTH * s..(c1 * font::WIDTH * s).min(self.width) {
                // SAFETY: inside the screen.
                unsafe { self.base.add(y * self.stride + x).write_volatile(bg) };
            }
        }
    }

    fn csi(&mut self, cmd: u8) {
        let p = |i: usize, d: u16| -> usize {
            let v = if i < self.nparams { self.params[i] } else { 0 };
            (if v == 0 { d } else { v }) as usize
        };
        match cmd {
            b'K' => match if self.nparams > 0 { self.params[0] } else { 0 } {
                0 => self.clear_cells(self.row, self.col, self.cols),
                1 => self.clear_cells(self.row, 0, self.col + 1),
                _ => self.clear_cells(self.row, 0, self.cols),
            },
            b'J' => {
                let mode = if self.nparams > 0 { self.params[0] } else { 0 };
                if mode >= 2 {
                    self.clear();
                } else {
                    self.clear_cells(self.row, self.col, self.cols);
                    for r in self.row + 1..self.rows {
                        self.clear_cells(r, 0, self.cols);
                    }
                }
            }
            b'H' | b'f' => {
                self.row = (p(0, 1) - 1).min(self.rows - 1);
                self.col = (p(1, 1) - 1).min(self.cols - 1);
            }
            b'A' => self.row = self.row.saturating_sub(p(0, 1)),
            b'B' => self.row = (self.row + p(0, 1)).min(self.rows - 1),
            b'C' => self.col = (self.col + p(0, 1)).min(self.cols - 1),
            b'D' => self.col = self.col.saturating_sub(p(0, 1)),
            b'G' => self.col = (p(0, 1) - 1).min(self.cols - 1),
            _ => {}
        }
    }

    /// One byte of terminal output.
    pub fn feed(&mut self, b: u8) {
        match self.esc {
            1 => {
                if b == b'[' {
                    self.esc = 2;
                    self.params = [0; 4];
                    self.nparams = 0;
                } else {
                    self.esc = 0;
                }
                return;
            }
            2 => {
                match b {
                    b'0'..=b'9' => {
                        if self.nparams == 0 {
                            self.nparams = 1;
                        }
                        let i = self.nparams - 1;
                        if i < 4 {
                            self.params[i] = self.params[i].saturating_mul(10).saturating_add((b - b'0') as u16);
                        }
                    }
                    b';' => {
                        if self.nparams == 0 {
                            self.nparams = 1;
                        }
                        self.nparams = (self.nparams + 1).min(4);
                    }
                    b'?' | b'>' | b'=' => {}
                    0x40..=0x7E => {
                        self.esc = 0;
                        self.csi(b);
                    }
                    _ => self.esc = 0,
                }
                return;
            }
            _ => {}
        }
        if b == 0x1B {
            self.esc = 1;
            return;
        }
        self.put(b);
    }

    pub fn put(&mut self, b: u8) {
        match b {
            b'\n' => self.newline(),
            b'\r' => self.col = 0,
            b'\t' => {
                for _ in 0..(8 - self.col % 8) {
                    self.put(b' ');
                }
            }
            0x08 => self.col = self.col.saturating_sub(1),
            0x20..=0x7E => {
                if self.col >= self.cols {
                    self.newline();
                }
                self.glyph(b, self.col, self.row);
                self.col += 1;
            }
            _ => {}
        }
    }
}

/// Bytes from a program or the kernel: to the serial port as they are, and to the screen
/// through a small terminal: the cursor and erase sequences line editors use (ESC [ ... K, J,
/// H, A, B, C, D); colors and the rest are dropped.
pub fn write_bytes(bytes: &[u8]) {
    for &b in bytes {
        if b == b'\n' {
            serial_byte(b'\r');
        }
        serial_byte(b);
        // SAFETY: one CPU, interrupts off in the kernel: nothing else touches FB meanwhile.
        unsafe {
            if let Some(fb) = (*core::ptr::addr_of_mut!(FB)).as_mut() {
                fb.feed(b);
            }
        }
    }
}

pub struct Out;

impl fmt::Write for Out {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_bytes(s.as_bytes());
        Ok(())
    }
}

#[macro_export]
macro_rules! kprint {
    ($($a:tt)*) => {{ let _ = core::fmt::Write::write_fmt(&mut $crate::console::Out, format_args!($($a)*)); }};
}

#[macro_export]
macro_rules! kprintln {
    ($($a:tt)*) => {{ $crate::kprint!($($a)*); $crate::console::write_bytes(b"\n"); }};
}
