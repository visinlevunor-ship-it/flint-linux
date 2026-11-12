//! FAT32: format a partition and fill it with a directory tree in one go. Long file names
//! are written (with generated 8.3 aliases), so any name works. Used for Flint's boot
//! partition, both in disk images and by the installer.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const SECTOR: u64 = 512;
const RESERVED_SECTORS: u32 = 32;
const MIN_FAT32_CLUSTERS: u64 = 65525;
/// 2026-01-01 00:00:00 as FAT date and time: deterministic timestamps.
const FAT_DATE: u16 = ((2026 - 1980) << 9) | (1 << 5) | 1;
const FAT_TIME: u16 = 0;

pub enum Data {
    Bytes(Vec<u8>),
    File(PathBuf, u64),
}

impl Data {
    fn len(&self) -> u64 {
        match self {
            Data::Bytes(b) => b.len() as u64,
            Data::File(_, n) => *n,
        }
    }
}

pub enum Node {
    File(Data),
    Dir(Vec<(String, Node)>),
}

/// Read a directory tree from disk (file contents are read when the volume is written).
pub fn read_tree(dir: &Path) -> Result<Vec<(String, Node)>, String> {
    let mut entries = Vec::new();
    let mut names: Vec<_> = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten().collect();
    names.sort_by_key(|e| e.file_name());
    for e in names {
        let name = e.file_name().to_string_lossy().into_owned();
        valid_name(&name).map_err(|m| format!("{}: {m}", e.path().display()))?;
        let p = e.path();
        let meta = fs::metadata(&p).map_err(|err| format!("{}: {err}", p.display()))?;
        if meta.is_dir() {
            entries.push((name, Node::Dir(read_tree(&p)?)));
        } else {
            entries.push((name, Node::File(Data::File(p, meta.len()))));
        }
    }
    Ok(entries)
}

pub fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 255 || name == "." || name == ".." || name.ends_with('.') || name.ends_with(' ') {
        return Err(format!("'{name}' cannot be a FAT file name"));
    }
    if name.chars().any(|c| (c as u32) < 0x20 || "\"*/:<>?\\|".contains(c)) {
        return Err(format!("'{name}' has a character FAT does not allow"));
    }
    Ok(())
}

fn short_ok(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || b"_-~!#$%&'()@^`{}".contains(&c))
}

/// The name as an 8.3 entry if it is one exactly (upper case, short): no long name needed.
pub fn exact_short(name: &str) -> Option<[u8; 11]> {
    let (base, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    if !short_ok(base, 8) || !(ext.is_empty() || short_ok(ext, 3)) || name.ends_with('.') {
        return None;
    }
    let mut out = [b' '; 11];
    out[..base.len()].copy_from_slice(base.as_bytes());
    out[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
    Some(out)
}

fn alias(name: &str, taken: &[[u8; 11]]) -> [u8; 11] {
    let clean = |s: &str| -> Vec<u8> { s.chars().filter_map(|c| c.to_ascii_uppercase().is_ascii_alphanumeric().then(|| c.to_ascii_uppercase() as u8)).collect() };
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) if !b.is_empty() => (clean(b), clean(e)),
        _ => (clean(name), Vec::new()),
    };
    let base = if base.is_empty() { b"FILE".to_vec() } else { base };
    for n in 1u32.. {
        let tail = format!("~{n}");
        let keep = (8 - tail.len()).min(base.len());
        let mut out = [b' '; 11];
        out[..keep].copy_from_slice(&base[..keep]);
        out[keep..keep + tail.len()].copy_from_slice(tail.as_bytes());
        let e = ext.len().min(3);
        out[8..8 + e].copy_from_slice(&ext[..e]);
        if !taken.contains(&out) {
            return out;
        }
    }
    unreachable!()
}

fn lfn_checksum(short: &[u8; 11]) -> u8 {
    short.iter().fold(0u8, |s, &c| s.rotate_right(1).wrapping_add(c))
}

/// Directory entries for one name: long-name entries (if needed), then the short entry.
fn entries_for(name: &str, short: &[u8; 11], attr: u8, cluster: u32, size: u32, long: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if long {
        let mut units: Vec<u16> = name.encode_utf16().collect();
        let n = units.len().div_ceil(13);
        if units.len() % 13 != 0 {
            units.push(0);
        }
        units.resize(n * 13, 0xFFFF);
        let sum = lfn_checksum(short);
        for seq in (1..=n).rev() {
            let chunk = &units[(seq - 1) * 13..seq * 13];
            let mut e = [0u8; 32];
            e[0] = seq as u8 | if seq == n { 0x40 } else { 0 };
            e[11] = 0x0F;
            e[13] = sum;
            let slots = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
            for (k, &o) in slots.iter().enumerate() {
                e[o..o + 2].copy_from_slice(&chunk[k].to_le_bytes());
            }
            out.extend_from_slice(&e);
        }
    }
    out.extend_from_slice(&dirent(short, attr, cluster, size));
    out
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

pub struct Geometry {
    pub sectors: u64,
    spc: u64,
    fat_sectors: u64,
    clusters: u64,
}

impl Geometry {
    pub fn new(sectors: u64) -> Result<Geometry, String> {
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

    fn cluster_offset(&self, c: u32) -> u64 {
        (self.data_start() + u64::from(c - 2) * self.spc) * SECTOR
    }
}

enum Pending<'a> {
    Bytes(Vec<u8>),
    Data(&'a Data),
}

struct Fat<'a> {
    g: Geometry,
    fat: Vec<u32>,
    next: u32,
    writes: Vec<(u64, Pending<'a>)>,
}

impl<'a> Fat<'a> {
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

    fn dir_bytes(entries: &[(String, Node)]) -> u64 {
        let mut n = 2 * 32 + 32;
        for (name, _) in entries {
            n += 32 + if exact_short(name).is_some() { 0 } else { 32 * name.encode_utf16().count().div_ceil(13) as u64 };
        }
        n
    }

    fn write_dir(&mut self, own: u32, parent: u32, entries: &'a [(String, Node)], label: Option<&[u8; 11]>) -> Result<(), String> {
        let mut raw: Vec<u8> = Vec::new();
        if let Some(l) = label {
            raw.extend(dirent(l, 0x08, 0, 0));
        } else {
            raw.extend(dirent(b".          ", 0x10, own, 0));
            raw.extend(dirent(b"..         ", 0x10, parent, 0));
        }
        let mut taken: Vec<[u8; 11]> = entries.iter().filter_map(|(n, _)| exact_short(n)).collect();
        let mut subdirs = Vec::new();
        for (name, node) in entries {
            valid_name(name)?;
            let (short, long) = match exact_short(name) {
                Some(s) => (s, false),
                None => {
                    let s = alias(name, &taken);
                    taken.push(s);
                    (s, true)
                }
            };
            match node {
                Node::File(data) => {
                    let len = data.len();
                    let size = u32::try_from(len).map_err(|_| format!("{name}: larger than 4 GiB, too big for FAT32"))?;
                    let c = if len == 0 { 0 } else { self.alloc(len)? };
                    if c != 0 {
                        self.writes.push((self.g.cluster_offset(c), Pending::Data(data)));
                    }
                    raw.extend(entries_for(name, &short, 0x20, c, size, long));
                }
                Node::Dir(children) => {
                    let c = self.alloc(Self::dir_bytes(children))?;
                    raw.extend(entries_for(name, &short, 0x10, c, 0, long));
                    subdirs.push((c, children));
                }
            }
        }
        if label.is_some() && raw.len() as u64 > self.root_bytes() {
            return Err("too many files in the top directory".into());
        }
        let cap = (raw.len() as u64).div_ceil(self.g.cluster_bytes()).max(1) * self.g.cluster_bytes();
        raw.resize(cap as usize, 0);
        self.writes.push((self.g.cluster_offset(own), Pending::Bytes(raw)));
        let me_as_parent = if label.is_some() { 0 } else { own };
        for (c, children) in subdirs {
            self.write_dir(c, me_as_parent, children, None)?;
        }
        Ok(())
    }

    fn root_bytes(&self) -> u64 {
        ROOT_CLUSTERS as u64 * self.g.cluster_bytes()
    }
}

/// Clusters given to the root directory (room for a few hundred long names).
const ROOT_CLUSTERS: u32 = 4;

fn boot_sector(g: &Geometry, hidden: u64, serial: u32, label: &[u8; 11]) -> [u8; 512] {
    let mut b = [0u8; 512];
    b[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    b[3..11].copy_from_slice(b"FLINT   ");
    b[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
    b[13] = g.spc as u8;
    b[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
    b[16] = 2;
    b[21] = 0xF8;
    b[24..26].copy_from_slice(&63u16.to_le_bytes());
    b[26..28].copy_from_slice(&255u16.to_le_bytes());
    b[28..32].copy_from_slice(&(hidden.min(u64::from(u32::MAX)) as u32).to_le_bytes());
    b[32..36].copy_from_slice(&(g.sectors as u32).to_le_bytes());
    b[36..40].copy_from_slice(&(g.fat_sectors as u32).to_le_bytes());
    b[44..48].copy_from_slice(&2u32.to_le_bytes());
    b[48..50].copy_from_slice(&1u16.to_le_bytes());
    b[50..52].copy_from_slice(&6u16.to_le_bytes());
    b[64] = 0x80;
    b[66] = 0x29;
    b[67..71].copy_from_slice(&serial.to_le_bytes());
    b[71..82].copy_from_slice(label);
    b[82..90].copy_from_slice(b"FAT32   ");
    bios_message(&mut b, 90);
    b[510] = 0x55;
    b[511] = 0xAA;
    b
}

/// Code for a legacy BIOS that boots this disk anyway: print a hint and halt.
pub fn bios_message(b: &mut [u8; 512], start: usize) {
    let msg = b"This is a UEFI boot disk. Turn on UEFI boot in the firmware setup.\r\n\0";
    let code: [u8; 18] = [0x31, 0xC0, 0x8E, 0xD8, 0xBE, 0x00, 0x00, 0xAC, 0x84, 0xC0, 0x74, 0x06, 0xB4, 0x0E, 0xCD, 0x10, 0xEB, 0xF5];
    b[start..start + code.len()].copy_from_slice(&code);
    let hang = start + code.len();
    b[hang..hang + 3].copy_from_slice(&[0xF4, 0xEB, 0xFD]);
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

pub fn label_field(label: &str) -> Result<[u8; 11], String> {
    if label.is_empty() || label.len() > 11 || !label.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_' || c == b'-') {
        return Err(format!("volume label '{label}': up to 11 upper-case letters, digits, - or _"));
    }
    let mut l = [b' '; 11];
    l[..label.len()].copy_from_slice(label.as_bytes());
    Ok(l)
}

pub struct Made {
    pub used_bytes: u64,
    pub partition_bytes: u64,
}

/// Format `sectors` sectors at byte `offset` of `dev` as FAT32 labelled `label`, holding
/// `tree`. `hidden` is the partition's first sector on its disk.
pub fn format<W: Write + Seek>(dev: &mut W, offset: u64, sectors: u64, hidden: u64, label: &str, serial: u32, tree: &[(String, Node)]) -> Result<Made, String> {
    let label = label_field(label)?;
    let g = Geometry::new(sectors)?;
    let mut fat = Fat { fat: vec![0; (g.clusters + 2) as usize], g, next: 2, writes: Vec::new() };
    fat.fat[0] = 0x0FFF_FFF8;
    fat.fat[1] = 0x0FFF_FFFF;
    let root = fat.alloc(fat.root_bytes())?;
    fat.write_dir(root, 0, tree, Some(&label))?;
    let used = u64::from(fat.next - 2);
    let free = fat.g.clusters - used;

    let err = |e: io::Error| e.to_string();
    let mut at = |off: u64, data: &[u8]| -> Result<(), String> { dev.seek(SeekFrom::Start(offset + off)).and_then(|_| dev.write_all(data)).map_err(err) };
    let bs = boot_sector(&fat.g, hidden, serial, &label);
    let info = fsinfo(free, fat.next);
    at(0, &bs)?;
    at(SECTOR, &info)?;
    at(6 * SECTOR, &bs)?;
    at(7 * SECTOR, &info)?;
    let mut fat_bytes = Vec::with_capacity(fat.fat_sectors_bytes());
    for v in &fat.fat {
        fat_bytes.extend_from_slice(&v.to_le_bytes());
    }
    fat_bytes.resize(fat.fat_sectors_bytes(), 0);
    for copy in 0..2 {
        at((u64::from(RESERVED_SECTORS) + copy * fat.g.fat_sectors) * SECTOR, &fat_bytes)?;
    }
    let mut buf = vec![0u8; 1 << 20];
    for (off, data) in &fat.writes {
        match data {
            Pending::Bytes(b) => at(*off, b)?,
            Pending::Data(Data::Bytes(b)) => at(*off, b)?,
            Pending::Data(Data::File(p, len)) => {
                let mut f = fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
                let mut done = 0u64;
                while done < *len {
                    let n = f.read(&mut buf).map_err(|e| format!("{}: {e}", p.display()))?;
                    if n == 0 {
                        return Err(format!("{}: changed while being copied", p.display()));
                    }
                    let n = (n as u64).min(*len - done) as usize;
                    at(*off + done, &buf[..n])?;
                    done += n as u64;
                }
            }
        }
    }
    Ok(Made { used_bytes: used * fat.g.cluster_bytes(), partition_bytes: sectors * SECTOR })
}

impl Fat<'_> {
    fn fat_sectors_bytes(&self) -> usize {
        (self.g.fat_sectors * SECTOR) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(&exact_short("BOOTX64.EFI").unwrap(), b"BOOTX64 EFI");
        assert!(exact_short("kernel").is_none());
        assert_eq!(&alias("linux-7.0.0-zen.img", &[]), b"LINUX7~1IMG");
        let a = alias("linux-7.0.0-zen.img", &[*b"LINUX7~1IMG"]);
        assert_eq!(&a, b"LINUX7~2IMG");
        assert!(valid_name("a:b").is_err());
        assert!(valid_name("normal name.txt").is_ok());
        // checksum from the Microsoft spec example style: stable and order-dependent
        assert_ne!(lfn_checksum(b"LINUX7~1IMG"), lfn_checksum(b"LINUX7~2IMG"));
    }

    #[test]
    fn long_name_entries() {
        let e = entries_for("abcdefghijklmn", b"ABCDEF~1   ", 0x20, 5, 10, true);
        assert_eq!(e.len(), 3 * 32, "14 characters: two long entries and the short one");
        assert_eq!(e[0], 0x42);
        assert_eq!(e[32], 0x01);
        assert_eq!(e[11], 0x0F);
    }

    #[test]
    fn geometry() {
        let g = Geometry::new(512 * 2048 - 4096).unwrap();
        assert_eq!(g.spc, 8);
        assert!(g.clusters >= MIN_FAT32_CLUSTERS);
        assert!(Geometry::new(16 * 2048).is_err());
    }
}
