//! Disks: the drivers register what they find here; a cache of 4 KiB blocks in front of them,
//! and the GPT partitions on each.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// A disk driver: whole sectors, by number.
pub trait Disk {
    fn name(&self) -> String;
    fn sector_size(&self) -> usize;
    fn sectors(&self) -> u64;
    /// `buf.len()` is a multiple of the sector size
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), ()>;
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), ()>;
}

#[derive(Clone)]
pub struct Part {
    pub disk: usize,
    /// first byte and size on the disk
    pub start: u64,
    pub size: u64,
    pub name: String,
    pub type_guid: [u8; 16],
    pub uuid: [u8; 16],
}

static mut DISKS: Vec<Box<dyn Disk>> = Vec::new();
static mut PARTS: Vec<Part> = Vec::new();
const BLK: u64 = 4096;
static mut CACHE: Option<BTreeMap<(usize, u64), Vec<u8>>> = None;
const CACHE_MAX: usize = 8192; // 32 MiB

fn disks() -> &'static mut Vec<Box<dyn Disk>> {
    // SAFETY: one CPU; drivers register at start-up.
    unsafe { &mut *core::ptr::addr_of_mut!(DISKS) }
}

pub fn parts() -> &'static Vec<Part> {
    // SAFETY: filled at start-up.
    unsafe { &*core::ptr::addr_of!(PARTS) }
}

fn cache() -> &'static mut BTreeMap<(usize, u64), Vec<u8>> {
    // SAFETY: one CPU.
    unsafe { (*core::ptr::addr_of_mut!(CACHE)).get_or_insert_with(BTreeMap::new) }
}

pub fn register(d: Box<dyn Disk>) {
    crate::kprintln!("[kernel] disk {}: {} MiB ({}-byte sectors)", d.name(), d.sectors() * d.sector_size() as u64 >> 20, d.sector_size());
    disks().push(d);
}

pub fn disk_name(i: usize) -> String {
    disks()[i].name()
}

/// One 4 KiB block of disk `d` (from the cache, or read).
fn block(d: usize, n: u64) -> Result<&'static mut Vec<u8>, ()> {
    if !cache().contains_key(&(d, n)) {
        let disk = &mut disks()[d];
        let ss = disk.sector_size() as u64;
        let mut buf = alloc::vec![0u8; BLK as usize];
        let lba = n * BLK / ss;
        // the end of the disk may cut the last block short
        let avail = (disk.sectors().saturating_sub(lba) * ss).min(BLK) as usize;
        if avail == 0 {
            return Err(());
        }
        disk.read(lba, &mut buf[..avail])?;
        if cache().len() >= CACHE_MAX {
            // keep it simple: start over
            cache().clear();
        }
        cache().insert((d, n), buf);
    }
    Ok(cache().get_mut(&(d, n)).unwrap())
}

/// Bytes at `off` on disk `d`.
pub fn read(d: usize, off: u64, out: &mut [u8]) -> Result<(), ()> {
    let mut done = 0usize;
    while done < out.len() {
        let pos = off + done as u64;
        let b = block(d, pos / BLK)?;
        let in_blk = (pos % BLK) as usize;
        let k = (BLK as usize - in_blk).min(out.len() - done);
        out[done..done + k].copy_from_slice(&b[in_blk..in_blk + k]);
        done += k;
    }
    Ok(())
}

/// Write bytes at `off` on disk `d` (through the cache, to the disk at once).
pub fn write(d: usize, off: u64, data: &[u8]) -> Result<(), ()> {
    let mut done = 0usize;
    let ss = disks()[d].sector_size() as u64;
    while done < data.len() {
        let pos = off + done as u64;
        let n = pos / BLK;
        let in_blk = (pos % BLK) as usize;
        let k = (BLK as usize - in_blk).min(data.len() - done);
        let b = block(d, n)?;
        b[in_blk..in_blk + k].copy_from_slice(&data[done..done + k]);
        // write back the sectors this touched
        let first = in_blk as u64 / ss;
        let last = (in_blk + k - 1) as u64 / ss;
        let bytes = b[(first * ss) as usize..((last + 1) * ss) as usize].to_vec();
        disks()[d].write(n * BLK / ss + first, &bytes)?;
        done += k;
    }
    Ok(())
}

pub fn part_read(p: &Part, off: u64, out: &mut [u8]) -> Result<(), ()> {
    if off + out.len() as u64 > p.size {
        return Err(());
    }
    read(p.disk, p.start + off, out)
}

pub fn part_write(p: &Part, off: u64, data: &[u8]) -> Result<(), ()> {
    if off + data.len() as u64 > p.size {
        return Err(());
    }
    write(p.disk, p.start + off, data)
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// Read the GPT of every disk.
pub fn scan_partitions() {
    for d in 0..disks().len() {
        let ss = disks()[d].sector_size() as u64;
        let mut h = alloc::vec![0u8; 512];
        if read(d, ss, &mut h).is_err() || &h[0..8] != b"EFI PART" {
            continue;
        }
        let entries = le64(&h, 72);
        let count = le32(&h, 80) as u64;
        let esize = le32(&h, 84) as u64;
        if esize < 128 || count > 1024 {
            continue;
        }
        let mut table = alloc::vec![0u8; (count * esize) as usize];
        if read(d, entries * ss, &mut table).is_err() {
            continue;
        }
        for i in 0..count as usize {
            let e = &table[i * esize as usize..(i + 1) * esize as usize];
            if e[0..16].iter().all(|&b| b == 0) {
                continue;
            }
            let first = le64(e, 32);
            let last = le64(e, 40);
            let name: String = e[56..128].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).map(|c| char::from_u32(c as u32).unwrap_or('?')).collect();
            let mut type_guid = [0u8; 16];
            type_guid.copy_from_slice(&e[0..16]);
            let mut uuid = [0u8; 16];
            uuid.copy_from_slice(&e[16..32]);
            crate::kprintln!("[kernel]   {} partition {}: {} MiB \"{}\"", disk_name(d), i + 1, (last + 1 - first) * ss >> 20, name);
            // SAFETY: start-up.
            unsafe { (*core::ptr::addr_of_mut!(PARTS)).push(Part { disk: d, start: first * ss, size: (last + 1 - first) * ss, name, type_guid, uuid }) };
        }
    }
}

/// A partition's unique GUID as text (as Linux prints PARTUUIDs).
pub fn guid_text(g: &[u8; 16]) -> String {
    alloc::format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u32::from_le_bytes([g[0], g[1], g[2], g[3]]),
        u16::from_le_bytes([g[4], g[5]]),
        u16::from_le_bytes([g[6], g[7]]),
        g[8], g[9], g[10], g[11], g[12], g[13], g[14], g[15]
    )
}

/// The EFI system partition's type
pub const ESP_TYPE: [u8; 16] = [0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B];
