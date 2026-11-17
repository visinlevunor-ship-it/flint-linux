//! PCI: finding devices through the configuration space (the 0xCF8/0xCFC ports every PC
//! has), their BARs, and turning on memory decoding and bus mastering for drivers.

use crate::console::{inb, outb};
use alloc::vec::Vec;

unsafe fn outl(port: u16, v: u32) {
    core::arch::asm!("out dx, eax", in("dx") port, in("eax") v, options(nomem, nostack, preserves_flags));
}

unsafe fn inl(port: u16) -> u32 {
    let v: u32;
    core::arch::asm!("in eax, dx", out("eax") v, in("dx") port, options(nomem, nostack, preserves_flags));
    v
}

pub unsafe fn outw(port: u16, v: u16) {
    core::arch::asm!("out dx, ax", in("dx") port, in("ax") v, options(nomem, nostack, preserves_flags));
}

pub unsafe fn inw(port: u16) -> u16 {
    let v: u16;
    core::arch::asm!("in ax, dx", out("ax") v, in("dx") port, options(nomem, nostack, preserves_flags));
    v
}

pub unsafe fn outd(port: u16, v: u32) {
    outl(port, v)
}

pub unsafe fn ind(port: u16) -> u32 {
    inl(port)
}

pub unsafe fn outbp(port: u16, v: u8) {
    outb(port, v)
}

pub unsafe fn inbp(port: u16) -> u8 {
    inb(port)
}

#[derive(Clone, Copy)]
pub struct Dev {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
}

pub fn read32(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    let addr = 0x8000_0000u32 | (bus as u32) << 16 | (dev as u32) << 11 | (func as u32) << 8 | (off as u32 & 0xFC);
    // SAFETY: the PCI configuration ports.
    unsafe {
        outl(0xCF8, addr);
        inl(0xCFC)
    }
}

pub fn write32(bus: u8, dev: u8, func: u8, off: u8, v: u32) {
    let addr = 0x8000_0000u32 | (bus as u32) << 16 | (dev as u32) << 11 | (func as u32) << 8 | (off as u32 & 0xFC);
    // SAFETY: the PCI configuration ports.
    unsafe {
        outl(0xCF8, addr);
        outl(0xCFC, v);
    }
}

impl Dev {
    pub fn read(&self, off: u8) -> u32 {
        read32(self.bus, self.dev, self.func, off)
    }
    pub fn write(&self, off: u8, v: u32) {
        write32(self.bus, self.dev, self.func, off, v)
    }
    /// BAR n: (address, is I/O space). A 64-bit memory BAR takes two slots.
    pub fn bar(&self, n: u8) -> (u64, bool) {
        let lo = self.read(0x10 + n * 4);
        if lo & 1 != 0 {
            return ((lo & !3) as u64, true);
        }
        let mut a = (lo & !0xF) as u64;
        if (lo >> 1) & 3 == 2 {
            a |= (self.read(0x14 + n * 4) as u64) << 32;
        }
        (a, false)
    }
    /// Memory and I/O decoding on, bus mastering (DMA) on, legacy interrupts off.
    pub fn enable(&self) {
        let cmd = self.read(0x04) & 0xFFFF;
        self.write(0x04, cmd | 0x7 | (1 << 10));
    }
}

/// Every function on every bus.
pub fn scan() -> Vec<Dev> {
    let mut out = Vec::new();
    for bus in 0..=255u8 {
        for dev in 0..32u8 {
            for func in 0..8u8 {
                let id = read32(bus, dev, func, 0);
                if id & 0xFFFF == 0xFFFF {
                    if func == 0 {
                        break;
                    }
                    continue;
                }
                let class = read32(bus, dev, func, 8);
                out.push(Dev {
                    bus,
                    dev,
                    func,
                    vendor: id as u16,
                    device: (id >> 16) as u16,
                    class: (class >> 24) as u8,
                    subclass: (class >> 16) as u8,
                    prog_if: (class >> 8) as u8,
                });
                // single-function devices answer on every function number
                if func == 0 && (read32(bus, dev, 0, 0x0C) >> 16) & 0x80 == 0 {
                    break;
                }
            }
        }
    }
    out
}
