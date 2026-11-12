//! A bootable disk image: GPT with one EFI system partition, formatted FAT32, holding a
//! directory tree. Written for USB sticks: `dd` the image to the stick and any UEFI PC
//! boots it. On the first boot Flint adds a data partition in the rest of the stick.
//!
//! Everything is deterministic: the same inputs give the same image, byte for byte. The
//! disk and partition GUIDs and the volume serial number are derived from a seed.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use flint_auth::sha256;
pub use flint_disk::fat::{read_tree, Node};
use flint_disk::{fat, gpt};

const SECTOR: u64 = 512;
/// The partition starts at 1 MiB, as every modern tool does.
const PART_START: u64 = 2048;

/// A version 4 style GUID from a hash (on-disk byte order).
pub fn guid_from(seed: &[u8], what: &str) -> [u8; 16] {
    let mut h = sha256::Sha256::new();
    h.update(seed);
    h.update(what.as_bytes());
    let d = h.finalize();
    let mut g = [0u8; 16];
    g.copy_from_slice(&d[..16]);
    g[7] = (g[7] & 0x0F) | 0x40;
    g[8] = (g[8] & 0x3F) | 0x80;
    g
}

pub struct Made {
    pub part_guid: String,
    pub used_bytes: u64,
    pub partition_bytes: u64,
}

/// Write the image: `size_mib` MiB, one ESP labelled `label` holding `tree`.
pub fn make(out: &Path, size_mib: u64, label: &str, seed: &[u8], tree: &[(String, Node)]) -> Result<Made, String> {
    let total = size_mib * 1024 * 1024 / SECTOR;
    let disk_guid = guid_from(seed, "disk");
    let part_guid = guid_from(seed, "esp");
    let d = guid_from(seed, "serial");
    let serial = u32::from_le_bytes([d[0], d[1], d[2], d[3]]);

    let mut table = gpt::Table::new(SECTOR, total, disk_guid).map_err(|e| e.to_string())?;
    let last_usable = table.last_usable;
    let part_end = (last_usable + 1) / 2048 * 2048 - 1;
    table.add(gpt::ESP, part_guid, PART_START, part_end, "Flint").map_err(|e| e.to_string())?;
    let sectors = part_end - PART_START + 1;

    let err = |e: io::Error| format!("{}: {e}", out.display());
    let f = fs::File::create(out).map_err(err)?;
    f.set_len(total * SECTOR).map_err(err)?;
    let mut w = io::BufWriter::new(f);
    let mut mbr = gpt::protective_mbr(total);
    fat::bios_message(&mut mbr, 0);
    w.write_all(&mbr).map_err(err)?;
    table.write(&mut w, false).map_err(err)?;
    let made = fat::format(&mut w, PART_START * SECTOR, sectors, PART_START, label, serial, tree)?;
    w.flush().map_err(err)?;
    Ok(Made { part_guid: gpt::guid_string(&part_guid), used_bytes: made.used_bytes, partition_bytes: made.partition_bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids() {
        let g = guid_from(b"x", "esp");
        let s = gpt::guid_string(&g);
        assert_eq!(&s[14..15], "4", "version 4: {s}");
        assert!(matches!(&s[19..20], "8" | "9" | "a" | "b"), "variant: {s}");
        assert_ne!(guid_from(b"x", "esp"), guid_from(b"x", "disk"));
    }
}
