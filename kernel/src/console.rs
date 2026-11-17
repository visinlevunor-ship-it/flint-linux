//! Kernel output: the first serial port (COM1, 115200 8N1) and the UEFI framebuffer, drawn
//! with an 8x16 font as a terminal (a linux/xterm-like subset).

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

/// One character cell: the character (a code point) and its colors.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Cell {
    ch: u32,
    fg: u32,
    bg: u32,
}

const BLANK: Cell = Cell { ch: b' ' as u32, fg: FG, bg: BG };

/// The screen as a terminal: a grid of cells, drawn to the framebuffer in whole rows and only
/// where something changed, at most every few milliseconds (`flush`). The framebuffer is
/// only ever written, never read, and mapped write-combining, so drawing is fast.
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
    cells: alloc::vec::Vec<Cell>,
    dirty: alloc::vec::Vec<bool>,
    line: alloc::vec::Vec<u32>,
    fg: u32,
    bg: u32,
    bold: bool,
    reverse: bool,
    cursor_on: bool,
    drawn_cursor: (usize, usize),
    top: usize,    // scrolling region, inclusive
    bottom: usize, // inclusive
    saved: (usize, usize),
    wrap_next: bool,
    esc: u8, // 0 none, 1 after ESC, 2 in ESC [, 3 in ESC ] (until BEL / ST), 4 charset designation
    private: bool,
    params: [u16; 16],
    nparams: usize,
    utf: u32,
    utf_left: u8,
    pending: bool,
}

pub static mut FB: Option<Framebuffer> = None;

const FG: u32 = 0x00D0D0D0;
const BG: u32 = 0x00101014;

/// The 16 terminal colors (a softer palette than pure VGA).
const PALETTE: [u32; 16] = [
    0x1C1C22, 0xE06C75, 0x98C379, 0xE5C07B, 0x61AFEF, 0xC678DD, 0x56B6C2, 0xD0D0D0, 0x5C6370, 0xFF7A85, 0xB5E890, 0xFFD787, 0x7CC4FF, 0xDE9BF0, 0x7FDBE6,
    0xFFFFFF,
];

fn color256(n: u16) -> u32 {
    match n {
        0..=15 => PALETTE[n as usize],
        16..=231 => {
            let n = n - 16;
            let lv = |v: u16| if v == 0 { 0 } else { 55 + v as u32 * 40 };
            (lv(n / 36) << 16) | (lv((n / 6) % 6) << 8) | lv(n % 6)
        }
        _ => {
            let g = 8 + (n as u32 - 232) * 10;
            (g << 16) | (g << 8) | g
        }
    }
}

/// A glyph for code point `cp`: the font's for ASCII, drawn here for box-drawing and block
/// characters, and a few common symbols mapped to look-alikes.
fn glyph_bits(cp: u32) -> [u8; 16] {
    let ascii = |c: u8| font::GLYPHS[(c - font::FIRST) as usize];
    if (font::FIRST as u32..font::FIRST as u32 + 95).contains(&cp) {
        return font::GLYPHS[(cp - font::FIRST as u32) as usize];
    }
    // box drawing: which arms (up, down, left, right) and whether doubled
    let lines = |up: bool, down: bool, left: bool, right: bool, heavy: bool| {
        let mut g = [0u8; 16];
        let v: u8 = if heavy { 0x18 } else { 0x10 };
        let mid = 7;
        for (y, row) in g.iter_mut().enumerate() {
            if (up && y <= mid) || (down && y >= mid) {
                *row |= v;
            }
        }
        let h_rows: &[usize] = if heavy { &[7, 8] } else { &[7] };
        for &y in h_rows {
            if left {
                g[y] |= 0xF0;
            }
            if right {
                g[y] |= 0x0F | 0x10;
            }
            if left && !right {
                g[y] |= 0x10;
            }
        }
        g
    };
    match cp {
        0x2500 | 0x2504 | 0x2508 | 0x254C => lines(false, false, true, true, false),
        0x2501 | 0x2550 => lines(false, false, true, true, true),
        0x2502 | 0x2506 | 0x250A | 0x254E => lines(true, true, false, false, false),
        0x2503 | 0x2551 => lines(true, true, false, false, true),
        0x250C | 0x256D => lines(false, true, false, true, false),
        0x2510 | 0x256E => lines(false, true, true, false, false),
        0x2514 | 0x2570 => lines(true, false, false, true, false),
        0x2518 | 0x256F => lines(true, false, true, false, false),
        0x251C => lines(true, true, false, true, false),
        0x2524 => lines(true, true, true, false, false),
        0x252C => lines(false, true, true, true, false),
        0x2534 => lines(true, false, true, true, false),
        0x253C => lines(true, true, true, true, false),
        0x2554 => lines(false, true, false, true, true),
        0x2557 => lines(false, true, true, false, true),
        0x255A => lines(true, false, false, true, true),
        0x255D => lines(true, false, true, false, true),
        0x2588 => [0xFF; 16],
        0x2580 => {
            let mut g = [0u8; 16];
            g[..8].fill(0xFF);
            g
        }
        0x2584 => {
            let mut g = [0u8; 16];
            g[8..].fill(0xFF);
            g
        }
        0x258C => [0xF0; 16],
        0x2590 => [0x0F; 16],
        0x2591 => core::array::from_fn(|y| if y % 2 == 0 { 0x88 } else { 0x22 }),
        0x2592 => core::array::from_fn(|y| if y % 2 == 0 { 0xAA } else { 0x55 }),
        0x2593 => core::array::from_fn(|y| if y % 2 == 0 { 0xEE } else { 0xBB }),
        0x2581..=0x2587 => {
            let h = (cp - 0x2580) as usize * 2;
            core::array::from_fn(|y| if y >= 16 - h { 0xFF } else { 0 })
        }
        0x2589..=0x258F => {
            let w = 8 - (cp - 0x2588) as u32;
            [(0xFFu16 << (8 - w)) as u8; 16]
        }
        0x25CF | 0x2022 | 0x25CB => {
            let mut g = [0u8; 16];
            g[6] = 0x3C;
            g[7] = 0x7E;
            g[8] = 0x7E;
            g[9] = 0x3C;
            g
        }
        0xB7 | 0x2219 | 0x22C5 => {
            let mut g = [0u8; 16];
            g[7] = 0x18;
            g[8] = 0x18;
            g
        }
        0x2018 | 0x2019 | 0xB4 => ascii(b'\''),
        0x201C | 0x201D => ascii(b'"'),
        0x2013 | 0x2014 | 0x2212 => ascii(b'-'),
        0x2026 => ascii(b'.'),
        0x2190 => ascii(b'<'),
        0x2192 | 0x276F | 0x25B6 | 0xE0B0 => ascii(b'>'),
        0x2191 | 0x25B2 => ascii(b'^'),
        0x2193 | 0x25BC => ascii(b'v'),
        0x2713 | 0x2714 => ascii(b'v'),
        0x2717 | 0x2718 | 0xD7 => ascii(b'x'),
        0xA0 => ascii(b' '),
        0xC0..=0xC5 | 0xE0..=0xE5 => ascii(if cp < 0xE0 { b'A' } else { b'a' }),
        0xC8..=0xCB | 0xE8..=0xEB => ascii(if cp < 0xE0 { b'E' } else { b'e' }),
        0xCC..=0xCF | 0xEC..=0xEF => ascii(if cp < 0xE0 { b'I' } else { b'i' }),
        0xD2..=0xD6 | 0xF2..=0xF6 | 0xD8 | 0xF8 => ascii(if cp < 0xE0 { b'O' } else { b'o' }),
        0xD9..=0xDC | 0xF9..=0xFC => ascii(if cp < 0xE0 { b'U' } else { b'u' }),
        0xC7 => ascii(b'C'),
        0xE7 => ascii(b'c'),
        0xD1 => ascii(b'N'),
        0xF1 => ascii(b'n'),
        0x141 => ascii(b'L'),
        0x142 => ascii(b'l'),
        0x15A | 0x160 => ascii(b'S'),
        0x15B | 0x161 => ascii(b's'),
        0x179 | 0x17B | 0x17D => ascii(b'Z'),
        0x17A | 0x17C | 0x17E => ascii(b'z'),
        0x106 | 0x10C => ascii(b'C'),
        0x107 | 0x10D => ascii(b'c'),
        0x104 => ascii(b'A'),
        0x105 => ascii(b'a'),
        0x118 => ascii(b'E'),
        0x119 => ascii(b'e'),
        0x143 => ascii(b'N'),
        0x144 => ascii(b'n'),
        0xD3 => ascii(b'O'),
        0xDF => ascii(b's'),
        _ => ascii(b'?'),
    }
}

impl Framebuffer {
    pub fn new(base: *mut u32, width: usize, height: usize, stride: usize, bgr: bool) -> Framebuffer {
        let scale = if width >= 2400 { 2 } else { 1 };
        let cols = (width / (font::WIDTH * scale)).max(1);
        let rows = (height / (font::HEIGHT * scale)).max(1);
        Framebuffer {
            base,
            width,
            height,
            stride,
            bgr,
            scale,
            col: 0,
            row: 0,
            cols,
            rows,
            cells: alloc::vec![BLANK; cols * rows],
            dirty: alloc::vec![true; rows],
            line: alloc::vec![0; width],
            fg: FG,
            bg: BG,
            bold: false,
            reverse: false,
            cursor_on: true,
            drawn_cursor: (0, 0),
            top: 0,
            bottom: rows - 1,
            saved: (0, 0),
            wrap_next: false,
            esc: 0,
            private: false,
            params: [0; 16],
            nparams: 0,
            utf: 0,
            utf_left: 0,
            pending: true,
        }
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

    fn touch(&mut self, row: usize) {
        if row < self.rows {
            self.dirty[row] = true;
        }
        self.pending = true;
    }

    pub fn clear(&mut self) {
        self.cells.fill(Cell { ch: b' ' as u32, fg: self.fg, bg: self.bg });
        self.dirty.fill(true);
        self.pending = true;
        self.col = 0;
        self.row = 0;
        // and the margins the text grid does not cover
        let bg = self.pixel(BG);
        for y in 0..self.height {
            for x in 0..self.width {
                // SAFETY: inside the framebuffer the firmware gave us.
                unsafe { self.base.add(y * self.stride + x).write_volatile(bg) };
            }
        }
    }

    /// Draw the rows that changed (and the cursor).
    pub fn flush(&mut self) {
        if !self.pending {
            return;
        }
        self.pending = false;
        let (cr, cc) = (self.row, self.col.min(self.cols - 1));
        if self.drawn_cursor.0 < self.rows {
            self.dirty[self.drawn_cursor.0] = true;
        }
        self.dirty[cr] = true;
        let s = self.scale;
        let cw = font::WIDTH * s;
        for r in 0..self.rows {
            if !self.dirty[r] {
                continue;
            }
            self.dirty[r] = false;
            let row_cells = &self.cells[r * self.cols..(r + 1) * self.cols];
            let glyphs: alloc::vec::Vec<([u8; 16], u32, u32)> = row_cells
                .iter()
                .enumerate()
                .map(|(c, cell)| {
                    let (mut fg, mut bg) = (self.pixel(cell.fg), self.pixel(cell.bg));
                    if self.cursor_on && r == cr && c == cc {
                        core::mem::swap(&mut fg, &mut bg);
                    }
                    (glyph_bits(cell.ch), fg, bg)
                })
                .collect();
            for gy in 0..font::HEIGHT {
                for (c, (bits, fg, bg)) in glyphs.iter().enumerate() {
                    let b = bits[gy];
                    let x0 = c * cw;
                    for gx in 0..font::WIDTH {
                        let px = if b & (0x80 >> gx) != 0 { *fg } else { *bg };
                        for dx in 0..s {
                            self.line[x0 + gx * s + dx] = px;
                        }
                    }
                }
                let used = self.cols * cw;
                for dy in 0..s {
                    let y = (r * font::HEIGHT + gy) * s + dy;
                    // SAFETY: y < height and `used` <= width: inside the framebuffer.
                    unsafe { core::ptr::copy_nonoverlapping(self.line.as_ptr(), self.base.add(y * self.stride), used) };
                }
            }
        }
        self.drawn_cursor = (cr, cc);
    }

    fn blank(&self) -> Cell {
        Cell { ch: b' ' as u32, fg: self.fg, bg: self.bg }
    }

    /// Move lines [top, bottom] up by n (new blank lines at the bottom).
    fn scroll_up(&mut self, n: usize) {
        let (t, b) = (self.top, self.bottom);
        let n = n.min(b - t + 1);
        let cols = self.cols;
        self.cells.copy_within((t + n) * cols..(b + 1) * cols, t * cols);
        let blank = self.blank();
        self.cells[(b + 1 - n) * cols..(b + 1) * cols].fill(blank);
        for r in t..=b {
            self.touch(r);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let (t, b) = (self.top, self.bottom);
        let n = n.min(b - t + 1);
        let cols = self.cols;
        self.cells.copy_within(t * cols..(b + 1 - n) * cols, (t + n) * cols);
        let blank = self.blank();
        self.cells[t * cols..(t + n) * cols].fill(blank);
        for r in t..=b {
            self.touch(r);
        }
    }

    fn newline(&mut self) {
        self.wrap_next = false;
        if self.row == self.bottom {
            self.scroll_up(1);
        } else if self.row + 1 < self.rows {
            self.row += 1;
        }
        self.pending = true;
    }

    /// Clear cells [c0, c1) of text row `row`.
    fn clear_cells(&mut self, row: usize, c0: usize, c1: usize) {
        let blank = self.blank();
        let c1 = c1.min(self.cols);
        if c0 < c1 {
            self.cells[row * self.cols + c0..row * self.cols + c1].fill(blank);
        }
        self.touch(row);
    }

    fn sgr(&mut self) {
        if self.nparams == 0 {
            self.nparams = 1;
            self.params[0] = 0;
        }
        let mut i = 0;
        while i < self.nparams {
            let p = self.params[i];
            match p {
                0 => {
                    self.fg = FG;
                    self.bg = BG;
                    self.bold = false;
                    self.reverse = false;
                }
                1 => self.bold = true,
                22 => self.bold = false,
                7 => self.reverse = true,
                27 => self.reverse = false,
                30..=37 => self.fg = PALETTE[(p - 30) as usize],
                39 => self.fg = FG,
                40..=47 => self.bg = PALETTE[(p - 40) as usize],
                49 => self.bg = BG,
                90..=97 => self.fg = PALETTE[(p - 90 + 8) as usize],
                100..=107 => self.bg = PALETTE[(p - 100 + 8) as usize],
                38 | 48 => {
                    let c = match self.params.get(i + 1) {
                        Some(5) if i + 2 < self.nparams => {
                            let c = color256(self.params[i + 2]);
                            i += 2;
                            Some(c)
                        }
                        Some(2) if i + 4 < self.nparams => {
                            let c = ((self.params[i + 2] as u32 & 255) << 16) | ((self.params[i + 3] as u32 & 255) << 8) | (self.params[i + 4] as u32 & 255);
                            i += 4;
                            Some(c)
                        }
                        _ => None,
                    };
                    if let Some(c) = c {
                        if p == 38 {
                            self.fg = c;
                        } else {
                            self.bg = c;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn csi(&mut self, cmd: u8) {
        let p = |i: usize, d: u16| -> usize {
            let v = if i < self.nparams { self.params[i] } else { 0 };
            (if v == 0 { d } else { v }) as usize
        };
        let raw0 = if self.nparams > 0 { self.params[0] } else { 0 };
        self.wrap_next = false;
        if self.private {
            // DEC modes: the cursor (25) and the alternate screen (47, 1047, 1049)
            let on = cmd == b'h';
            if cmd == b'h' || cmd == b'l' {
                for i in 0..self.nparams.max(1) {
                    match self.params[i] {
                        25 => self.cursor_on = on,
                        47 | 1047 | 1049 => {
                            let blank = Cell { ch: b' ' as u32, fg: FG, bg: BG };
                            self.cells.fill(blank);
                            self.dirty.fill(true);
                            if self.params[i] == 1049 {
                                if on {
                                    self.saved = (self.row, self.col);
                                } else {
                                    (self.row, self.col) = self.saved;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            self.pending = true;
            return;
        }
        match cmd {
            b'm' => self.sgr(),
            b'K' => match raw0 {
                0 => self.clear_cells(self.row, self.col, self.cols),
                1 => self.clear_cells(self.row, 0, self.col + 1),
                _ => self.clear_cells(self.row, 0, self.cols),
            },
            b'J' => match raw0 {
                0 => {
                    self.clear_cells(self.row, self.col, self.cols);
                    for r in self.row + 1..self.rows {
                        self.clear_cells(r, 0, self.cols);
                    }
                }
                1 => {
                    for r in 0..self.row {
                        self.clear_cells(r, 0, self.cols);
                    }
                    self.clear_cells(self.row, 0, self.col + 1);
                }
                _ => {
                    for r in 0..self.rows {
                        self.clear_cells(r, 0, self.cols);
                    }
                }
            },
            b'H' | b'f' => {
                self.row = (p(0, 1) - 1).min(self.rows - 1);
                self.col = (p(1, 1) - 1).min(self.cols - 1);
            }
            b'A' => self.row = self.row.saturating_sub(p(0, 1)).max(if self.row >= self.top { self.top } else { 0 }),
            b'B' | b'e' => self.row = (self.row + p(0, 1)).min(if self.row <= self.bottom { self.bottom } else { self.rows - 1 }),
            b'C' | b'a' => self.col = (self.col + p(0, 1)).min(self.cols - 1),
            b'D' => self.col = self.col.saturating_sub(p(0, 1)),
            b'E' => {
                self.row = (self.row + p(0, 1)).min(self.rows - 1);
                self.col = 0;
            }
            b'F' => {
                self.row = self.row.saturating_sub(p(0, 1));
                self.col = 0;
            }
            b'G' | b'`' => self.col = (p(0, 1) - 1).min(self.cols - 1),
            b'd' => self.row = (p(0, 1) - 1).min(self.rows - 1),
            b'r' => {
                let t = p(0, 1) - 1;
                let b = p(1, self.rows as u16) - 1;
                if t < b && b < self.rows {
                    self.top = t;
                    self.bottom = b;
                } else {
                    self.top = 0;
                    self.bottom = self.rows - 1;
                }
                self.row = 0;
                self.col = 0;
            }
            b'S' => self.scroll_up(p(0, 1)),
            b'T' => self.scroll_down(p(0, 1)),
            b'L' | b'M' => {
                if self.row >= self.top && self.row <= self.bottom {
                    let saved = self.top;
                    self.top = self.row;
                    if cmd == b'L' {
                        self.scroll_down(p(0, 1));
                    } else {
                        self.scroll_up(p(0, 1));
                    }
                    self.top = saved;
                }
            }
            b'P' | b'@' | b'X' => {
                let n = p(0, 1).min(self.cols - self.col);
                let base = self.row * self.cols;
                let (c, cols) = (self.col, self.cols);
                let blank = self.blank();
                match cmd {
                    b'P' => {
                        self.cells.copy_within(base + c + n..base + cols, base + c);
                        self.cells[base + cols - n..base + cols].fill(blank);
                    }
                    b'@' => {
                        self.cells.copy_within(base + c..base + cols - n, base + c + n);
                        self.cells[base + c..base + c + n].fill(blank);
                    }
                    _ => self.cells[base + c..base + c + n].fill(blank),
                }
                self.touch(self.row);
            }
            b's' => self.saved = (self.row, self.col),
            b'u' => (self.row, self.col) = self.saved,
            _ => {}
        }
        self.pending = true;
    }

    /// One byte of terminal output.
    pub fn feed(&mut self, b: u8) {
        match self.esc {
            1 => {
                self.esc = 0;
                match b {
                    b'[' => {
                        self.esc = 2;
                        self.params = [0; 16];
                        self.nparams = 0;
                        self.private = false;
                    }
                    b']' => self.esc = 3,
                    b'(' | b')' | b'*' | b'+' | b'#' | b'%' => self.esc = 4,
                    b'7' => self.saved = (self.row, self.col),
                    b'8' => (self.row, self.col) = self.saved,
                    b'M' => {
                        if self.row == self.top {
                            self.scroll_down(1);
                        } else {
                            self.row = self.row.saturating_sub(1);
                        }
                    }
                    b'D' => self.newline(),
                    b'E' => {
                        self.newline();
                        self.col = 0;
                    }
                    b'c' => {
                        self.fg = FG;
                        self.bg = BG;
                        self.top = 0;
                        self.bottom = self.rows - 1;
                        self.clear();
                    }
                    _ => {}
                }
                self.pending = true;
                return;
            }
            2 => {
                match b {
                    b'0'..=b'9' => {
                        if self.nparams == 0 {
                            self.nparams = 1;
                        }
                        let i = self.nparams - 1;
                        self.params[i] = self.params[i].saturating_mul(10).saturating_add((b - b'0') as u16);
                    }
                    b';' | b':' => {
                        if self.nparams == 0 {
                            self.nparams = 1;
                        }
                        self.nparams = (self.nparams + 1).min(16);
                    }
                    b'?' => self.private = true,
                    b'>' | b'=' | b' ' | b'!' | b'"' | b'\'' => {}
                    0x40..=0x7E => {
                        self.esc = 0;
                        self.csi(b);
                    }
                    _ => self.esc = 0,
                }
                return;
            }
            3 => {
                // operating system command (window title...): ignored up to BEL or ESC \
                if b == 7 {
                    self.esc = 0;
                } else if b == 0x1B {
                    self.esc = 1;
                }
                return;
            }
            4 => {
                self.esc = 0;
                return;
            }
            _ => {}
        }
        if b == 0x1B {
            self.esc = 1;
            return;
        }
        // UTF-8
        if self.utf_left > 0 {
            if b & 0xC0 == 0x80 {
                self.utf = (self.utf << 6) | (b & 0x3F) as u32;
                self.utf_left -= 1;
                if self.utf_left == 0 {
                    let cp = self.utf;
                    self.put_char(cp);
                }
                return;
            }
            self.utf_left = 0;
        }
        match b {
            0xC0..=0xDF => {
                self.utf = (b & 0x1F) as u32;
                self.utf_left = 1;
            }
            0xE0..=0xEF => {
                self.utf = (b & 0x0F) as u32;
                self.utf_left = 2;
            }
            0xF0..=0xF7 => {
                self.utf = (b & 0x07) as u32;
                self.utf_left = 3;
            }
            0x80..=0xFF => {}
            _ => self.put(b),
        }
    }

    pub fn put(&mut self, b: u8) {
        match b {
            // the terminal does what the tty's ONLCR would: a new line starts at the left
            b'\n' | 0x0B | 0x0C => {
                self.col = 0;
                self.newline();
            }
            b'\r' => {
                self.col = 0;
                self.wrap_next = false;
                self.pending = true;
            }
            b'\t' => {
                let next = ((self.col / 8) + 1) * 8;
                self.col = next.min(self.cols - 1);
                self.pending = true;
            }
            0x08 => {
                self.col = self.col.saturating_sub(1);
                self.wrap_next = false;
                self.pending = true;
            }
            0x20..=0x7E => self.put_char(b as u32),
            _ => {}
        }
    }

    fn put_char(&mut self, cp: u32) {
        // combining marks and zero-width characters take no cell
        if (0x300..0x370).contains(&cp) || (0x200B..=0x200F).contains(&cp) || cp == 0xFE0F {
            return;
        }
        if self.wrap_next {
            self.col = 0;
            self.newline();
        }
        let (mut fg, mut bg) = (self.fg, self.bg);
        if self.bold && PALETTE[..8].contains(&fg) {
            fg = PALETTE[PALETTE.iter().position(|&c| c == fg).unwrap() + 8];
        }
        if self.reverse {
            core::mem::swap(&mut fg, &mut bg);
        }
        let i = self.row * self.cols + self.col;
        self.cells[i] = Cell { ch: cp, fg, bg };
        self.touch(self.row);
        if self.col + 1 >= self.cols {
            self.wrap_next = true;
        } else {
            self.col += 1;
        }
    }
}

/// Draw what changed on the screen. From the timer, the idle loop and before reading input.
pub fn flush() {
    // SAFETY: one CPU, interrupts off in the kernel.
    unsafe {
        if let Some(fb) = (*core::ptr::addr_of_mut!(FB)).as_mut() {
            fb.flush();
        }
    }
}

/// Bytes from a program or the kernel: to the serial port as they are, and to the screen
/// through the terminal above (colors, cursor movement, scrolling regions, UTF-8). The screen
/// is drawn by `flush`, so a burst of output costs one redraw.
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
