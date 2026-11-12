//! GUID partition tables: read, change, write (both copies, with their checksums).

use std::io::{self, Read, Seek, SeekFrom, Write};

use crate::crc32;

pub const ENTRIES: u32 = 128;
pub const ENTRY_SIZE: u32 = 128;

/// A GUID in on-disk (mixed-endian) byte order.
pub type Guid = [u8; 16];

pub const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> Guid {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let c = c.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]
}

/// EFI System Partition C12A7328-F81F-11D2-BA4B-00A0C93EC93B.
pub const ESP: Guid = guid(0xC12A7328, 0xF81F, 0x11D2, [0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B]);
/// Linux filesystem data 0FC63DAF-8483-4772-8E79-3D69D8477DE4.
pub const LINUX: Guid = guid(0x0FC63DAF, 0x8483, 0x4772, [0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4]);

/// The GUID as text, the way Linux shows a PARTUUID.
pub fn guid_string(g: &Guid) -> String {
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

/// A random (version 4) GUID.
pub fn random_guid() -> io::Result<Guid> {
    let mut g = [0u8; 16];
    fs_random(&mut g)?;
    g[7] = (g[7] & 0x0F) | 0x40;
    g[8] = (g[8] & 0x3F) | 0x80;
    Ok(g)
}

pub(crate) fn fs_random(buf: &mut [u8]) -> io::Result<()> {
    std::fs::File::open("/dev/urandom")?.read_exact(buf)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partition {
    /// 1-based slot number (what Linux calls sdX1, nvme0n1p1, ...).
    pub number: u32,
    pub type_guid: Guid,
    pub guid: Guid,
    pub first: u64,
    pub last: u64,
    pub attrs: u64,
    pub name: String,
}

impl Partition {
    pub fn sectors(&self) -> u64 {
        self.last - self.first + 1
    }
}

#[derive(Clone, Debug)]
pub struct Table {
    pub sector: u64,
    /// Sectors on the whole disk.
    pub total: u64,
    pub disk_guid: Guid,
    pub first_usable: u64,
    pub last_usable: u64,
    pub partitions: Vec<Partition>,
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn read_at<R: Read + Seek>(r: &mut R, off: u64, len: usize) -> io::Result<Vec<u8>> {
    r.seek(SeekFrom::Start(off))?;
    let mut b = vec![0u8; len];
    r.read_exact(&mut b)?;
    Ok(b)
}

fn table_sectors(sector: u64) -> u64 {
    (u64::from(ENTRIES) * u64::from(ENTRY_SIZE)).div_ceil(sector)
}

impl Table {
    /// A new, empty table for a disk of `total` sectors.
    pub fn new(sector: u64, total: u64, disk_guid: Guid) -> io::Result<Table> {
        let ts = table_sectors(sector);
        if total < 2 * (2 + ts) + 2048 {
            return Err(bad("disk too small for a partition table"));
        }
        Ok(Table { sector, total, disk_guid, first_usable: 2 + ts, last_usable: total - 2 - ts, partitions: Vec::new() })
    }

    /// Read the primary table (or, if it is damaged, the backup at the end of the disk).
    pub fn read<R: Read + Seek>(r: &mut R, sector: u64, total: u64) -> io::Result<Table> {
        match Self::read_header(r, sector, total, 1) {
            Ok(t) => Ok(t),
            Err(e) => Self::read_header(r, sector, total, total - 1).map_err(|_| e),
        }
    }

    fn read_header<R: Read + Seek>(r: &mut R, sector: u64, total: u64, lba: u64) -> io::Result<Table> {
        let h = read_at(r, lba * sector, sector as usize)?;
        if &h[0..8] != b"EFI PART" {
            return Err(bad("no GPT partition table"));
        }
        let size = u32::from_le_bytes(h[12..16].try_into().unwrap()) as usize;
        if !(92..=sector as usize).contains(&size) {
            return Err(bad("bad GPT header size"));
        }
        let mut hc = h[..size].to_vec();
        hc[16..20].fill(0);
        if crc32(&hc) != u32::from_le_bytes(h[16..20].try_into().unwrap()) {
            return Err(bad("GPT header checksum mismatch"));
        }
        let u64_at = |o: usize| u64::from_le_bytes(h[o..o + 8].try_into().unwrap());
        let u32_at = |o: usize| u32::from_le_bytes(h[o..o + 4].try_into().unwrap());
        let (first_usable, last_usable, entries_lba) = (u64_at(40), u64_at(48), u64_at(72));
        let (count, esize, ecrc) = (u32_at(80), u32_at(84), u32_at(88));
        if !(128..=4096).contains(&esize) || count > 1024 || entries_lba >= total {
            return Err(bad("bad GPT entry array"));
        }
        let raw = read_at(r, entries_lba * sector, (count * esize) as usize)?;
        if crc32(&raw) != ecrc {
            return Err(bad("GPT entry array checksum mismatch"));
        }
        let mut disk_guid = [0u8; 16];
        disk_guid.copy_from_slice(&h[56..72]);
        let mut partitions = Vec::new();
        for i in 0..count as usize {
            let e = &raw[i * esize as usize..(i + 1) * esize as usize];
            if e[..16].iter().all(|&b| b == 0) {
                continue;
            }
            let mut type_guid = [0u8; 16];
            type_guid.copy_from_slice(&e[0..16]);
            let mut guid = [0u8; 16];
            guid.copy_from_slice(&e[16..32]);
            let units: Vec<u16> = e[56..128].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
            partitions.push(Partition {
                number: i as u32 + 1,
                type_guid,
                guid,
                first: u64::from_le_bytes(e[32..40].try_into().unwrap()),
                last: u64::from_le_bytes(e[40..48].try_into().unwrap()),
                attrs: u64::from_le_bytes(e[48..56].try_into().unwrap()),
                name: String::from_utf16_lossy(&units),
            });
        }
        // A table copied onto a bigger disk (dd of an image to a USB stick) still describes
        // the image's size: the usable area grows to the real end of the disk when written.
        Ok(Table { sector, total, disk_guid, first_usable, last_usable: last_usable.min(total - 2 - table_sectors(sector)), partitions })
    }

    /// Unused stretches of the disk, aligned to 1 MiB: (first sector, last sector).
    pub fn free(&self) -> Vec<(u64, u64)> {
        let align = (1024 * 1024 / self.sector).max(1);
        let usable_last = self.total - 2 - table_sectors(self.sector);
        let mut used: Vec<(u64, u64)> = self.partitions.iter().map(|p| (p.first, p.last)).collect();
        used.sort_unstable();
        let mut out = Vec::new();
        let mut at = self.first_usable;
        for (f, l) in used.into_iter().chain(std::iter::once((usable_last + 1, usable_last + 1))) {
            let start = at.div_ceil(align) * align;
            if f > start {
                let end = ((f) / align * align).saturating_sub(1).min(f - 1);
                if end >= start + align - 1 {
                    out.push((start, end));
                }
            }
            at = at.max(l + 1);
        }
        out
    }

    /// Add a partition in slot `number` (or the first free slot).
    pub fn add(&mut self, type_guid: Guid, guid: Guid, first: u64, last: u64, name: &str) -> io::Result<u32> {
        let usable_last = self.total - 2 - table_sectors(self.sector);
        if first < self.first_usable || last > usable_last || last < first {
            return Err(bad("partition outside the usable area"));
        }
        if self.partitions.iter().any(|p| first <= p.last && p.first <= last) {
            return Err(bad("partition overlaps another"));
        }
        let number = (1..=ENTRIES).find(|n| self.partitions.iter().all(|p| p.number != *n)).ok_or_else(|| bad("partition table full"))?;
        self.partitions.push(Partition { number, type_guid, guid, first, last, attrs: 0, name: name.to_string() });
        self.partitions.sort_by_key(|p| p.number);
        Ok(number)
    }

    fn entries(&self) -> Vec<u8> {
        let mut t = vec![0u8; (ENTRIES * ENTRY_SIZE) as usize];
        for p in &self.partitions {
            let e = &mut t[(p.number as usize - 1) * ENTRY_SIZE as usize..p.number as usize * ENTRY_SIZE as usize];
            e[0..16].copy_from_slice(&p.type_guid);
            e[16..32].copy_from_slice(&p.guid);
            e[32..40].copy_from_slice(&p.first.to_le_bytes());
            e[40..48].copy_from_slice(&p.last.to_le_bytes());
            e[48..56].copy_from_slice(&p.attrs.to_le_bytes());
            for (i, u) in p.name.encode_utf16().take(36).enumerate() {
                e[56 + 2 * i..58 + 2 * i].copy_from_slice(&u.to_le_bytes());
            }
        }
        t
    }

    fn header(&self, current: u64, backup: u64, entries_lba: u64, ecrc: u32) -> Vec<u8> {
        let ts = table_sectors(self.sector);
        let mut h = vec![0u8; self.sector as usize];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&current.to_le_bytes());
        h[32..40].copy_from_slice(&backup.to_le_bytes());
        h[40..48].copy_from_slice(&self.first_usable.to_le_bytes());
        h[48..56].copy_from_slice(&(self.total - 2 - ts).to_le_bytes());
        h[56..72].copy_from_slice(&self.disk_guid);
        h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
        h[80..84].copy_from_slice(&ENTRIES.to_le_bytes());
        h[84..88].copy_from_slice(&ENTRY_SIZE.to_le_bytes());
        h[88..92].copy_from_slice(&ecrc.to_le_bytes());
        let crc = crc32(&h[..92]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        h
    }

    /// Write the protective MBR (only if `mbr`), both headers and both entry arrays. The
    /// backup goes to the real end of the disk.
    pub fn write<W: Write + Seek>(&self, w: &mut W, mbr: bool) -> io::Result<()> {
        let s = self.sector;
        let ts = table_sectors(s);
        let entries = self.entries();
        let ecrc = crc32(&entries);
        if mbr {
            w.seek(SeekFrom::Start(0))?;
            w.write_all(&protective_mbr(self.total))?;
        }
        // backup first: if writing is cut short, the primary still describes the old state
        w.seek(SeekFrom::Start((self.total - 1 - ts) * s))?;
        w.write_all(&entries)?;
        w.seek(SeekFrom::Start((self.total - 1) * s))?;
        w.write_all(&self.header(self.total - 1, 1, self.total - 1 - ts, ecrc))?;
        w.seek(SeekFrom::Start(2 * s))?;
        w.write_all(&entries)?;
        w.seek(SeekFrom::Start(s))?;
        w.write_all(&self.header(1, self.total - 1, 2, ecrc))?;
        w.flush()
    }
}

/// Make an existing protective MBR cover the whole disk again (after an image was written
/// to a bigger disk), keeping its boot code.
pub fn fix_protective_mbr<F: Read + Write + Seek>(f: &mut F, total: u64) -> io::Result<()> {
    let mut b = [0u8; 512];
    f.seek(SeekFrom::Start(0))?;
    f.read_exact(&mut b)?;
    if b[510] != 0x55 || b[511] != 0xAA || b[446 + 4] != 0xEE {
        return Ok(());
    }
    let size = ((total - 1).min(0xFFFF_FFFF) as u32).to_le_bytes();
    if b[446 + 12..446 + 16] != size {
        b[446 + 12..446 + 16].copy_from_slice(&size);
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&b)?;
    }
    Ok(())
}

pub fn protective_mbr(total: u64) -> [u8; 512] {
    let mut b = [0u8; 512];
    let e = 446;
    b[e + 1..e + 4].copy_from_slice(&[0x00, 0x02, 0x00]);
    b[e + 4] = 0xEE;
    b[e + 5..e + 8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    b[e + 8..e + 12].copy_from_slice(&1u32.to_le_bytes());
    b[e + 12..e + 16].copy_from_slice(&((total - 1).min(0xFFFF_FFFF) as u32).to_le_bytes());
    b[510] = 0x55;
    b[511] = 0xAA;
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn create_read_grow_add() {
        let total = 64 * 2048; // 64 MiB
        let mut disk = Cursor::new(vec![0u8; (total * 512) as usize]);
        let mut t = Table::new(512, total, [7; 16]).unwrap();
        t.add(ESP, [1; 16], 2048, 2048 * 17 - 1, "boot").unwrap();
        t.write(&mut disk, true).unwrap();
        let back = Table::read(&mut disk, 512, total).unwrap();
        assert_eq!(back.partitions, t.partitions);
        assert_eq!(back.partitions[0].name, "boot");
        assert_eq!(back.free(), vec![(2048 * 17, (total - 34) / 2048 * 2048 - 1)]);

        // the image written to a bigger disk: the free space reaches the new end
        let big = 128 * 2048;
        let mut v = disk.into_inner();
        v.resize((big * 512) as usize, 0);
        let mut disk = Cursor::new(v);
        let mut t = Table::read(&mut disk, 512, big).unwrap();
        let (f, l) = *t.free().last().unwrap();
        assert!(l > total, "free space grows to the real end");
        assert!(t.add(LINUX, [2; 16], 2048 * 10, 2048 * 20, "x").is_err(), "overlap");
        let n = t.add(LINUX, [2; 16], f, l, "data").unwrap();
        assert_eq!(n, 2);
        t.write(&mut disk, false).unwrap();
        let back = Table::read(&mut disk, 512, big).unwrap();
        assert_eq!(back.partitions.len(), 2);
        assert!(back.free().is_empty());

        // damage the primary: the backup still reads
        let mut v = disk.into_inner();
        v[512 + 100] ^= 1;
        let back = Table::read(&mut Cursor::new(v), 512, big).unwrap();
        assert_eq!(back.partitions.len(), 2);
        assert_eq!(guid_string(&ESP), "c12a7328-f81f-11d2-ba4b-00a0c93ec93b");
    }
}
