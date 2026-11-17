//! FAT32, as much as the Flint kernel needs on the boot partition: read a file, and replace a
//! file's contents (used to keep the RAM layer between restarts).

use crate::block::{self, Part};
use alloc::vec::Vec;

pub struct Fat {
    part: Part,
    bps: u64,
    spc: u64,
    fat_start: u64, // byte offset of the first FAT
    fat_size: u64,  // bytes per FAT
    nfats: u64,
    data_start: u64,
    root: u32,
    clusters: u32,
}

fn le16(b: &[u8], o: usize) -> u64 {
    u16::from_le_bytes([b[o], b[o + 1]]) as u64
}
fn le32(b: &[u8], o: usize) -> u64 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as u64
}

const EOC: u32 = 0x0FFF_FFF8;

/// FAT's date and time words for seconds since 1970.
fn fat_time(t: u64) -> (u16, u16) {
    let days = t / 86400;
    let secs = t % 86400;
    // civil date from days (Howard Hinnant's algorithm)
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    let date = (((y - 1980).max(0) as u16) << 9) | ((m as u16) << 5) | d as u16;
    let time = (((secs / 3600) as u16) << 11) | ((((secs / 60) % 60) as u16) << 5) | ((secs % 60) / 2) as u16;
    (date, time)
}

impl Fat {
    pub fn open(part: &Part) -> Option<Fat> {
        let mut b = [0u8; 512];
        block::part_read(part, 0, &mut b).ok()?;
        if b[510] != 0x55 || b[511] != 0xAA {
            return None;
        }
        let bps = le16(&b, 11);
        let spc = b[13] as u64;
        let reserved = le16(&b, 14);
        let nfats = b[16] as u64;
        let fat_sz = le32(&b, 36);
        if bps == 0 || spc == 0 || le16(&b, 22) != 0 || fat_sz == 0 {
            return None; // not FAT32
        }
        let total = if le16(&b, 19) != 0 { le16(&b, 19) } else { le32(&b, 32) };
        let data_start = (reserved + nfats * fat_sz) * bps;
        let clusters = ((total * bps - data_start) / (spc * bps)) as u32;
        Some(Fat { part: part.clone(), bps, spc, fat_start: reserved * bps, fat_size: fat_sz * bps, nfats, data_start, root: le32(&b, 44) as u32, clusters })
    }

    fn csize(&self) -> u64 {
        self.bps * self.spc
    }

    fn coff(&self, c: u32) -> u64 {
        self.data_start + (c as u64 - 2) * self.csize()
    }

    fn next(&self, c: u32) -> u32 {
        let mut b = [0u8; 4];
        let _ = block::part_read(&self.part, self.fat_start + c as u64 * 4, &mut b);
        u32::from_le_bytes(b) & 0x0FFF_FFFF
    }

    fn set(&self, c: u32, val: u32) -> Result<(), ()> {
        for i in 0..self.nfats {
            block::part_write(&self.part, self.fat_start + i * self.fat_size + c as u64 * 4, &val.to_le_bytes())?;
        }
        Ok(())
    }

    fn chain(&self, first: u32) -> Vec<u32> {
        let mut v = Vec::new();
        let mut c = first;
        while (2..EOC).contains(&c) && v.len() <= self.clusters as usize {
            v.push(c);
            c = self.next(c);
        }
        v
    }

    fn read_chain(&self, first: u32, size: Option<u64>) -> Result<Vec<u8>, ()> {
        let mut out = Vec::new();
        for c in self.chain(first) {
            let mut buf = alloc::vec![0u8; self.csize() as usize];
            block::part_read(&self.part, self.coff(c), &mut buf)?;
            out.extend_from_slice(&buf);
        }
        if let Some(s) = size {
            out.truncate(s as usize);
        }
        Ok(out)
    }

    /// The 32-byte entry for `name` (8.3, upper case) in directory `dir`: (byte offset on the
    /// partition, the entry).
    fn find(&self, dir: u32, name: &[u8; 11]) -> Option<(u64, [u8; 32])> {
        for c in self.chain(dir) {
            let mut buf = alloc::vec![0u8; self.csize() as usize];
            block::part_read(&self.part, self.coff(c), &mut buf).ok()?;
            for (i, e) in buf.chunks(32).enumerate() {
                if e[0] == 0 {
                    return None;
                }
                if e[0] == 0xE5 || e[11] == 0x0F {
                    continue;
                }
                if &e[0..11] == name {
                    let mut ent = [0u8; 32];
                    ent.copy_from_slice(e);
                    return Some((self.coff(c) + i as u64 * 32, ent));
                }
            }
        }
        None
    }

    fn short(name: &str) -> [u8; 11] {
        let mut out = [b' '; 11];
        let (base, ext) = name.split_once('.').unwrap_or((name, ""));
        for (i, b) in base.bytes().take(8).enumerate() {
            out[i] = b.to_ascii_uppercase();
        }
        for (i, b) in ext.bytes().take(3).enumerate() {
            out[8 + i] = b.to_ascii_uppercase();
        }
        out
    }

    fn first_cluster(e: &[u8; 32]) -> u32 {
        (le16(e, 20) << 16 | le16(e, 26)) as u32
    }

    fn dir_of(&self, path: &[&str]) -> Option<u32> {
        let mut d = self.root;
        for p in path {
            let (_, e) = self.find(d, &Self::short(p))?;
            if e[11] & 0x10 == 0 {
                return None;
            }
            d = Self::first_cluster(&e);
        }
        Some(d)
    }

    /// A file's contents: `dir` names directories under the root (8.3 names).
    pub fn read_file(&self, dir: &[&str], name: &str) -> Option<Vec<u8>> {
        let d = self.dir_of(dir)?;
        let (_, e) = self.find(d, &Self::short(name))?;
        self.read_chain(Self::first_cluster(&e), Some(le32(&e, 28))).ok()
    }

    /// Replace (or create) a file in an existing directory with `data`.
    pub fn write_file(&self, dir: &[&str], name: &str, data: &[u8]) -> Result<(), ()> {
        let d = self.dir_of(dir).ok_or(())?;
        let short = Self::short(name);
        // the entry: the existing one, or a free slot
        let (pos, old) = match self.find(d, &short) {
            Some((pos, e)) => (pos, Some(e)),
            None => (self.free_slot(d)?, None),
        };
        // free the old clusters
        if let Some(e) = &old {
            for c in self.chain(Self::first_cluster(e)) {
                self.set(c, 0)?;
            }
        }
        // new clusters for the data
        let need = (data.len() as u64).div_ceil(self.csize()) as usize;
        let mut new = Vec::with_capacity(need);
        let mut c = 2u32;
        while new.len() < need {
            if c >= self.clusters + 2 {
                for &n in &new {
                    let _ = self.set(n, 0);
                }
                return Err(()); // full
            }
            if self.next(c) == 0 {
                new.push(c);
            }
            c += 1;
        }
        for (i, &cl) in new.iter().enumerate() {
            let start = i * self.csize() as usize;
            let end = (start + self.csize() as usize).min(data.len());
            let mut buf = alloc::vec![0u8; self.csize() as usize];
            buf[..end - start].copy_from_slice(&data[start..end]);
            block::part_write(&self.part, self.coff(cl), &buf)?;
            self.set(cl, if i + 1 < new.len() { new[i + 1] } else { 0x0FFF_FFFF })?;
        }
        let first = new.first().copied().unwrap_or(0);
        let mut e = old.unwrap_or([0u8; 32]);
        e[0..11].copy_from_slice(&short);
        e[11] = 0x20; // archive
        // modification (and creation) time and date, from the clock
        let (date, time) = fat_time(crate::syscall::wall_seconds());
        for (o, v) in [(14, time), (16, date), (18, date), (22, time), (24, date)] {
            e[o..o + 2].copy_from_slice(&v.to_le_bytes());
        }
        e[20..22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
        e[26..28].copy_from_slice(&(first as u16).to_le_bytes());
        e[28..32].copy_from_slice(&(data.len() as u32).to_le_bytes());
        block::part_write(&self.part, pos, &e)?;
        // the free-cluster count in FSInfo is now unknown
        let mut b = [0u8; 512];
        block::part_read(&self.part, 0, &mut b)?;
        let fsinfo = le16(&b, 48);
        if fsinfo != 0 && fsinfo != 0xFFFF {
            block::part_write(&self.part, fsinfo * self.bps + 488, &[0xFF; 8])?;
        }
        Ok(())
    }

    fn free_slot(&self, dir: u32) -> Result<u64, ()> {
        for c in self.chain(dir) {
            let mut buf = alloc::vec![0u8; self.csize() as usize];
            block::part_read(&self.part, self.coff(c), &mut buf)?;
            for (i, e) in buf.chunks(32).enumerate() {
                if e[0] == 0 || e[0] == 0xE5 {
                    return Ok(self.coff(c) + i as u64 * 32);
                }
            }
        }
        Err(()) // a full directory: not extended
    }
}
