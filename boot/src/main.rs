//! flint-boot: Flint Linux's UEFI bootloader.
//!
//! The firmware starts it from `\EFI\BOOT\BOOTX64.EFI` on the USB stick or disk. It reads
//! `\FLINT\BOOT.CFG` from the same partition, shows a menu, loads the chosen kernel with
//! LoadImage/StartImage (the kernel's EFI stub does the rest) and hands the kernel its
//! initramfs images through the LoadFile2 protocol Linux looks for. The kernel command line
//! gets `flint.esp=PARTUUID`, naming the partition Flint booted from.
//!
//! No allocation crate, no external dependencies: memory comes from the firmware.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

mod config;
#[cfg(not(test))]
mod efi;
#[cfg(not(test))]
mod uefi_main;

#[cfg(test)]
fn main() {}
