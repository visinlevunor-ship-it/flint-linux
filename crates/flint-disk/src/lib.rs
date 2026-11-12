//! Disk layout for Flint: GPT partition tables, FAT32 (the boot partition) and ext4 (the
//! data partition). Used by flint-mkimage on the build machine and by the installer and
//! the first-boot setup on Flint itself, so it is plain std Rust that builds anywhere.

// Links flint-libc, Flint's C library, into every program built for Linux (no musl).
#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "musl"))]
extern crate flint_libc as _;

pub mod ext4;
pub mod fat;
pub mod gpt;

/// CRC-32 (IEEE), as GPT uses.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}
