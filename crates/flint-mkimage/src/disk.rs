//! A bootable disk image: GPT with one EFI system partition, formatted FAT32, holding a
//! directory tree. Written for USB sticks: `dd` the image to the stick and any UEFI PC
//! boots it. The partition spans the whole image, so the free space on it is usable
//! (Flint keeps saved Wi-Fi networks there).
//!
//! Everything is deterministic: the same inputs give the same image, byte for byte. The
//! disk and partition GUIDs and the volume serial number are derived from a seed.

use std::fs;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

use flint_auth::sha256;

const SECTOR: u64 = 512;
/// The partition starts at 1 MiB, as every modern tool does.
const PART_START: u64 = 2048;
const GPT_ENTRIES: u64 = 128;
const GPT_ENTRY_SIZE: u64 = 128;
/// Sectors taken by the partition table copies (32 for 128 entries of 128 bytes).
const GPT_TABLE_SECTORS: u64 = GPT_ENTRIES * GPT_ENTRY_SIZE / SECTOR;
const RESERVED_SECTORS: u32 = 32;
const MIN_FAT32_CLUSTERS: u64 = 65525;
/// EFI System Partition type GUID C12A7328-F81F-11D2-BA4B-00A0C93EC93B.
const ESP_TYPE: [u8; 16] = guid_bytes(0xC12A7328, 0xF81F, 0x11D2, [0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B]);
/// 2026-01-01 00:00:00 as FAT date and time: deterministic timestamps.
const FAT_DATE: u16 = ((2026 - 1980) << 9) | (1 << 5) | 1;
const FAT_TIME: u16 = 0;

const fn guid_bytes(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let c = c.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]
}

/// A version 4 style GUID from a hash (on-disk byte order).
pub fn guid_from(seed: &[u8], what: &str) -> [u8; 16] {
    let mut h = sha256::Sha256::new();
    h.update(seed);
    h.update(what.as_bytes());
    let d = h.finalize();
    let mut g = [0u8; 16];
    g.copy_from_slice(&d[..16]);
    g[7] = (g[7] & 0x0F) | 0x40; // version 4 (the high nibble of field 3, stored little-endian)
    g[8] = (g[8] & 0x3F) | 0x80; // variant 1
    g
}

/// The GUID as text, the way Linux shows a PARTUUID.
pub fn guid_string(g: &[u8; 16]) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u32::from_le_bytes([g[0], g[1], g[2], g[3]]),
        u16::from_le_bytes([g[4], g[5]]),
        u16::from_le_bytes([g[6], g[7]]),
        g[8],
        g[9],
        g[10],
        g[11],
        g[12],
        g[13],
        g[14],
        g[15]
    )
}

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

// ---------------------------------------------------------------------------------------
// The tree to put on the FAT volume.

pub enum Node {
    File(Vec<u8>),
    Dir(Vec<(String, Node)>),
}

/// An 8.3 name as stored in a directory entry, or an error saying why it cannot be one.
/// Names must already be upper case: FAT without long names cannot keep lower case, and
/// silently changing the case would surprise whoever edits the files later.
pub fn short_name(name: &str) -> Result<[u8; 11], String> {
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) => (b, e),
        None => (name, ""),
    };
    let ok = |s: &str, max: usize| {
        !s.is_empty() && s.len() <= max && s.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || b"_-~!#$%&'()@^`{}".contains(&c))
    };
    if !ok(base, 8) || !(ext.is_empty() || ok(ext, 3)) || name.ends_with('.') {
        return Err(format!("'{name}' is not an upper-case 8.3 name (like KERNEL or BOOTX64.EFI)"));
    }
    let mut out = [b' '; 11];
    out[..base.len()].copy_from_slice(base.as_bytes());
    out[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
    Ok(out)
}

pub fn read_tree(dir: &Path) -> Result<Vec<(String, Node)>, String> {
    let mut entries: Vec<(String, Node)> = Vec::new();
    let mut names: Vec<_> = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten().collect();
    names.sort_by_key(|e| e.file_name());
    for e in names {
        let name = e.file_name().to_string_lossy().into_owned();
        short_name(&name).map_err(|m| format!("{}: {m}", e.path().display()))?;
        let p = e.path();
        let meta = fs::metadata(&p).map_err(|err| format!("{}: {err}", p.display()))?;
        if meta.is_dir() {
            entries.push((name, Node::Dir(read_tree(&p)?)));
        } else {
            entries.push((name, Node::File(fs::read(&p).map_err(|err| format!("{}: {err}", p.display()))?)));
        }
    }
    Ok(entries)
}

// ---------------------------------------------------------------------------------------
// FAT32

struct Geometry {
    sectors: u64,
    spc: u64,
    fat_sectors: u64,
    clusters: u64,
}

impl Geometry {
    fn new(sectors: u64) -> Result<Geometry, String> {
        // Largest cluster (up to 4 KiB) that still leaves enough clusters for FAT32.
        for spc in [8u64, 4, 2, 1] {
            let mut fat_sectors = 1;
            loop {
                let data = sectors.saturating_sub(u64::from(RESERVED_SECTORS) + 2 * fat_sectors);
                let clusters = data / spc;
                let need = ((clusters + 2) * 4).div_ceil(SECTOR);
                if need <= fat_sectors {
                    if clusters >= MIN_FAT32_CLUSTERS && clusters < 0x0FFF_FFF5 {
                        return Ok(Geometry { sectors, spc, fat_sectors, clusters });
                    }
                    break;
                }
                fat_sectors = need;
            }
        }
        Err(format!("a FAT32 partition needs at least {} MiB", (MIN_FAT32_CLUSTERS * SECTOR + 2 * 1024 * 1024) / (1024 * 1024) + 1))
    }

    fn cluster_bytes(&self) -> u64 {
        self.spc * SECTOR
    }

    fn data_start(&self) -> u64 {
        u64::from(RESERVED_SECTORS) + 2 * self.fat_sectors
    }

    /// Byte offset of a cluster inside the partition.
    fn cluster_offset(&self, c: u32) -> u64 {
        (self.data_start() + u64::from(c - 2) * self.spc) * SECTOR
    }
}

struct Fat {
    g: Geometry,
    fat: Vec<u32>,
    next: u32,
    /// (byte offset in partition, data) to write
    writes: Vec<(u64, Vec<u8>)>,
}

impl Fat {
    /// Reserve a contiguous chain for `bytes` (at least one cluster). Returns the first cluster.
    fn alloc(&mut self, bytes: u64) -> Result<u32, String> {
        let n = bytes.div_ceil(self.g.cluster_bytes()).max(1) as u32;
        let first = self.next;
        if u64::from(first) + u64::from(n) > self.g.clusters + 2 {
            return Err(format!("the files do not fit in the {} MiB partition", self.g.sectors * SECTOR / (1024 * 1024)));
        }
        for i in 0..n {
            let c = first + i;
            self.fat[c as usize] = if i + 1 == n { 0x0FFF_FFFF } else { c + 1 };
        }
        self.next += n;
        Ok(first)
    }

    /// Write a directory (and, recursively, everything in it). `own` is its first cluster.
    fn write_dir(&mut self, own: u32, parent: u32, entries: &[(String, Node)], label: Option<&[u8; 11]>) -> Result<(), String> {
        let mut raw: Vec<u8> = Vec::new();
        if let Some(l) = label {
            raw.extend(dirent(l, 0x08, 0, 0));
        } else {
            raw.extend(dirent(b".          ", 0x10, own, 0));
            raw.extend(dirent(b"..         ", 0x10, parent, 0));
        }
        // Children: allocate first so their entries can point at them.
        let mut subdirs = Vec::new();
        for (name, node) in entries {
            let short = short_name(name)?;
            match node {
                Node::File(data) => {
                    let size = u32::try_from(data.len()).map_err(|_| format!("{name}: larger than 4 GiB, too big for FAT32"))?;
                    let c = if data.is_empty() { 0 } else { self.alloc(data.len() as u64)? };
                    if c != 0 {
                        self.writes.push((self.g.cluster_offset(c), data.clone()));
                    }
                    raw.extend(dirent(&short, 0x20, c, size));
                }
                Node::Dir(children) => {
                    let need = (children.len() as u64 + 3) * 32;
                    let c = self.alloc(need)?;
                    raw.extend(dirent(&short, 0x10, c, 0));
                    subdirs.push((c, children));
                }
            }
        }
        let cap = (raw.len() as u64).div_ceil(self.g.cluster_bytes()).max(1) * self.g.cluster_bytes();
        // Directory chains were sized when allocated; check the root, which was given one cluster.
        if label.is_some() && raw.len() as u64 > self.g.cluster_bytes() {
            return Err("too many files in the top directory".into());
        }
        raw.resize(cap as usize, 0);
        self.writes.push((self.g.cluster_offset(own), raw));
        // Parent of a top-level directory's ".." is written as cluster 0, per the FAT spec.
        let me_as_parent = if label.is_some() { 0 } else { own };
        for (c, children) in subdirs {
            self.write_dir(c, me_as_parent, children, None)?;
        }
        Ok(())
    }
}

fn dirent(name: &[u8; 11], attr: u8, cluster: u32, size: u32) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[..11].copy_from_slice(name);
    e[11] = attr;
    e[14..16].copy_from_slice(&FAT_TIME.to_le_bytes());
    e[16..18].copy_from_slice(&FAT_DATE.to_le_bytes());
    e[18..20].copy_from_slice(&FAT_DATE.to_le_bytes());
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    e[22..24].copy_from_slice(&FAT_TIME.to_le_bytes());
    e[24..26].copy_from_slice(&FAT_DATE.to_le_bytes());
    e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    e
}

fn boot_sector(g: &Geometry, hidden: u64, serial: u32, label: &[u8; 11]) -> [u8; 512] {
    let mut b = [0u8; 512];
    b[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    b[3..11].copy_from_slice(b"FLINT   ");
    b[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
    b[13] = g.spc as u8;
    b[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
    b[16] = 2; // FATs
    b[21] = 0xF8; // fixed disk
    b[24..26].copy_from_slice(&63u16.to_le_bytes()); // sectors per track (legacy)
    b[26..28].copy_from_slice(&255u16.to_le_bytes()); // heads (legacy)
    b[28..32].copy_from_slice(&(hidden as u32).to_le_bytes());
    b[32..36].copy_from_slice(&(g.sectors as u32).to_le_bytes());
    b[36..40].copy_from_slice(&(g.fat_sectors as u32).to_le_bytes());
    b[44..48].copy_from_slice(&2u32.to_le_bytes()); // root directory cluster
    b[48..50].copy_from_slice(&1u16.to_le_bytes()); // FSInfo sector
    b[50..52].copy_from_slice(&6u16.to_le_bytes()); // backup boot sector
    b[64] = 0x80; // drive number
    b[66] = 0x29; // extended boot signature
    b[67..71].copy_from_slice(&serial.to_le_bytes());
    b[71..82].copy_from_slice(label);
    b[82..90].copy_from_slice(b"FAT32   ");
    bios_message(&mut b, 90);
    b[510] = 0x55;
    b[511] = 0xAA;
    b
}

/// Code for a legacy BIOS that boots this disk anyway: print a hint and halt, instead of
/// running whatever bytes happen to be there.
fn bios_message(b: &mut [u8; 512], start: usize) {
    let msg = b"This is a UEFI boot disk. Turn on UEFI boot in the firmware setup.\r\n\0";
    let code: [u8; 18] = [
        0x31, 0xC0, // xor ax,ax
        0x8E, 0xD8, // mov ds,ax
        0xBE, 0x00, 0x00, // mov si,msg (patched below)
        0xAC, // lodsb
        0x84, 0xC0, // test al,al
        0x74, 0x06, // jz hang
        0xB4, 0x0E, // mov ah,0x0e
        0xCD, 0x10, // int 0x10
        0xEB, 0xF5, // jmp lodsb
    ];
    b[start..start + code.len()].copy_from_slice(&code);
    let hang = start + code.len();
    b[hang..hang + 3].copy_from_slice(&[0xF4, 0xEB, 0xFD]); // hlt; jmp hlt
    let msg_at = hang + 3;
    b[msg_at..msg_at + msg.len()].copy_from_slice(msg);
    let addr = (0x7C00 + msg_at) as u16;
    b[start + 5..start + 7].copy_from_slice(&addr.to_le_bytes());
}

fn fsinfo(free: u64, next: u32) -> [u8; 512] {
    let mut b = [0u8; 512];
    b[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
    b[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
    b[488..492].copy_from_slice(&(free as u32).to_le_bytes());
    b[492..496].copy_from_slice(&next.to_le_bytes());
    b[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());
    b
}

/// The volume label as an 11-byte field.
pub fn label_field(label: &str) -> Result<[u8; 11], String> {
    if label.is_empty() || label.len() > 11 || !label.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_' || c == b'-') {
        return Err(format!("volume label '{label}': up to 11 upper-case letters, digits, - or _"));
    }
    let mut l = [b' '; 11];
    l[..label.len()].copy_from_slice(label.as_bytes());
    Ok(l)
}

// ---------------------------------------------------------------------------------------
// GPT

fn protective_mbr(total: u64) -> [u8; 512] {
    let mut b = [0u8; 512];
    bios_message(&mut b, 0);
    let e = 446;
    b[e + 1..e + 4].copy_from_slice(&[0x00, 0x02, 0x00]); // CHS of LBA 1
    b[e + 4] = 0xEE;
    b[e + 5..e + 8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    b[e + 8..e + 12].copy_from_slice(&1u32.to_le_bytes());
    b[e + 12..e + 16].copy_from_slice(&((total - 1).min(0xFFFF_FFFF) as u32).to_le_bytes());
    b[510] = 0x55;
    b[511] = 0xAA;
    b
}

fn gpt_entries(part_guid: &[u8; 16], first: u64, last: u64, name: &str) -> Vec<u8> {
    let mut t = vec![0u8; (GPT_ENTRIES * GPT_ENTRY_SIZE) as usize];
    t[0..16].copy_from_slice(&ESP_TYPE);
    t[16..32].copy_from_slice(part_guid);
    t[32..40].copy_from_slice(&first.to_le_bytes());
    t[40..48].copy_from_slice(&last.to_le_bytes());
    for (i, u) in name.encode_utf16().take(36).enumerate() {
        t[56 + 2 * i..58 + 2 * i].copy_from_slice(&u.to_le_bytes());
    }
    t
}

fn gpt_header(disk_guid: &[u8; 16], current: u64, backup: u64, total: u64, entries_lba: u64, entries_crc: u32) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[0..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&current.to_le_bytes());
    h[32..40].copy_from_slice(&backup.to_le_bytes());
    h[40..48].copy_from_slice(&(2 + GPT_TABLE_SECTORS).to_le_bytes());
    h[48..56].copy_from_slice(&(total - 2 - GPT_TABLE_SECTORS).to_le_bytes());
    h[56..72].copy_from_slice(disk_guid);
    h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    h[80..84].copy_from_slice(&(GPT_ENTRIES as u32).to_le_bytes());
    h[84..88].copy_from_slice(&(GPT_ENTRY_SIZE as u32).to_le_bytes());
    h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    let crc = crc32(&h[..92]);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    h
}

pub struct Made {
    pub part_guid: String,
    pub used_bytes: u64,
    pub partition_bytes: u64,
}

/// Write the image: `size_mib` MiB, one ESP labelled `label` holding `tree`.
pub fn make(out: &Path, size_mib: u64, label: &str, seed: &[u8], tree: &[(String, Node)]) -> Result<Made, String> {
    let label = label_field(label)?;
    let total = size_mib * 1024 * 1024 / SECTOR;
    if total < PART_START + 2 + GPT_TABLE_SECTORS + 1 {
        return Err("image too small".into());
    }
    // The partition ends before the backup table, aligned down to 1 MiB.
    let last_usable = total - 2 - GPT_TABLE_SECTORS;
    let part_end = (last_usable + 1) / 2048 * 2048 - 1;
    let part_sectors = part_end - PART_START + 1;
    let g = Geometry::new(part_sectors)?;

    let mut fat = Fat { fat: vec![0; (g.clusters + 2) as usize], g, next: 3, writes: Vec::new() };
    fat.fat[0] = 0x0FFF_FFF8;
    fat.fat[1] = 0x0FFF_FFFF;
    fat.fat[2] = 0x0FFF_FFFF; // root directory: one cluster
    fat.write_dir(2, 0, tree, Some(&label))?;
    let used = u64::from(fat.next - 2);
    let free = fat.g.clusters - used;

    let disk_guid = guid_from(seed, "disk");
    let part_guid = guid_from(seed, "esp");
    let d = guid_from(seed, "serial");
    let serial = u32::from_le_bytes([d[0], d[1], d[2], d[3]]);

    let f = fs::File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    f.set_len(total * SECTOR).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut w = io::BufWriter::new(f);
    let at = |w: &mut io::BufWriter<fs::File>, off: u64, data: &[u8]| -> Result<(), String> {
        w.seek(SeekFrom::Start(off)).and_then(|_| w.write_all(data)).map_err(|e| format!("{}: {e}", out.display()))
    };

    // GPT: primary at the start, backup at the end.
    let entries = gpt_entries(&part_guid, PART_START, part_end, "Flint");
    let ecrc = crc32(&entries);
    at(&mut w, 0, &protective_mbr(total))?;
    at(&mut w, SECTOR, &gpt_header(&disk_guid, 1, total - 1, total, 2, ecrc))?;
    at(&mut w, 2 * SECTOR, &entries)?;
    at(&mut w, (total - 1 - GPT_TABLE_SECTORS) * SECTOR, &entries)?;
    at(&mut w, (total - 1) * SECTOR, &gpt_header(&disk_guid, total - 1, 1, total, total - 1 - GPT_TABLE_SECTORS, ecrc))?;

    // FAT32 volume.
    let p = PART_START * SECTOR;
    let bs = boot_sector(&fat.g, PART_START, serial, &label);
    let info = fsinfo(free, fat.next);
    at(&mut w, p, &bs)?;
    at(&mut w, p + SECTOR, &info)?;
    at(&mut w, p + 6 * SECTOR, &bs)?;
    at(&mut w, p + 7 * SECTOR, &info)?;
    let mut fat_bytes = Vec::with_capacity(fat.fat.len() * 4);
    for v in &fat.fat {
        fat_bytes.extend_from_slice(&v.to_le_bytes());
    }
    for copy in 0..2 {
        at(&mut w, p + (u64::from(RESERVED_SECTORS) + copy * fat.g.fat_sectors) * SECTOR, &fat_bytes)?;
    }
    for (off, data) in &fat.writes {
        at(&mut w, p + off, data)?;
    }
    w.flush().map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(Made { part_guid: guid_string(&part_guid), used_bytes: used * fat.g.cluster_bytes(), partition_bytes: part_sectors * SECTOR })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_and_guids() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(guid_string(&ESP_TYPE), "c12a7328-f81f-11d2-ba4b-00a0c93ec93b");
        let g = guid_from(b"x", "esp");
        let s = guid_string(&g);
        assert_eq!(&s[14..15], "4", "version 4: {s}");
        assert!(matches!(&s[19..20], "8" | "9" | "a" | "b"), "variant: {s}");
        assert_ne!(guid_from(b"x", "esp"), guid_from(b"x", "disk"));
    }

    #[test]
    fn names() {
        assert_eq!(&short_name("BOOTX64.EFI").unwrap(), b"BOOTX64 EFI");
        assert_eq!(&short_name("KERNEL").unwrap(), b"KERNEL     ");
        assert!(short_name("kernel").is_err());
        assert!(short_name("TOOLONGNAME.EFI").is_err());
        assert!(short_name("A.LONG").is_err());
        assert!(short_name("A B").is_err());
        assert!(short_name("A.").is_err());
        assert!(label_field("FLINT").is_ok());
        assert!(label_field("flint").is_err());
    }

    #[test]
    fn geometry() {
        let g = Geometry::new(512 * 2048 - 4096).unwrap(); // ~510 MiB
        assert_eq!(g.spc, 8);
        assert!(g.clusters >= MIN_FAT32_CLUSTERS);
        assert!((g.clusters + 2) * 4 <= g.fat_sectors * SECTOR);
        let small = Geometry::new(64 * 2048).unwrap();
        assert_eq!(small.spc, 1);
        assert!(Geometry::new(16 * 2048).is_err());
    }
}
