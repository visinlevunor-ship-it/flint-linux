//! `mkfs` for ext4: an empty filesystem with a journal, extents and uninitialised block
//! groups (so formatting writes only a few megabytes, whatever the size). Only what Flint
//! needs: 4 KiB blocks, 256-byte inodes, no flex_bg, no metadata checksums. The Linux
//! kernel mounts it as ext4 and e2fsck finds nothing to fix.

use std::io::{self, Seek, SeekFrom, Write};

use crate::gpt::fs_random;

const BLOCK: u64 = 4096;
const BLOCKS_PER_GROUP: u64 = 32768;
const INODE_SIZE: u64 = 256;
const BYTES_PER_INODE: u64 = 16384;
const FIRST_INO: u32 = 11;
const ROOT_INO: u32 = 2;
const JOURNAL_INO: u32 = 8;
const LOST_FOUND_INO: u32 = 11;
const LOST_FOUND_BLOCKS: u64 = 4;

const COMPAT_HAS_JOURNAL: u32 = 0x4;
const COMPAT_EXT_ATTR: u32 = 0x8;
const COMPAT_DIR_INDEX: u32 = 0x20;
const INCOMPAT_FILETYPE: u32 = 0x2;
const INCOMPAT_EXTENTS: u32 = 0x40;
const RO_SPARSE_SUPER: u32 = 0x1;
const RO_LARGE_FILE: u32 = 0x2;
const RO_HUGE_FILE: u32 = 0x8;
const RO_GDT_CSUM: u32 = 0x10;
const RO_DIR_NLINK: u32 = 0x20;
const RO_EXTRA_ISIZE: u32 = 0x40;

const BG_INODE_UNINIT: u16 = 0x1;
const BG_BLOCK_UNINIT: u16 = 0x2;

const EXTENTS_FL: u32 = 0x80000;
const S_IFDIR: u16 = 0o040000;
const S_IFREG: u16 = 0o100000;

pub struct Options {
    pub label: String,
    /// Used by tests for reproducible output; otherwise random.
    pub uuid: Option<[u8; 16]>,
    /// Creation time (seconds since 1970).
    pub time: u32,
}

pub struct Made {
    pub uuid: [u8; 16],
    pub blocks: u64,
    pub groups: u64,
}

fn has_super(group: u64) -> bool {
    if group <= 1 {
        return true;
    }
    for base in [3u64, 5, 7] {
        let mut p = base;
        while p < group {
            p *= base;
        }
        if p == group {
            return true;
        }
    }
    false
}

/// CRC-16 (polynomial 0x8005, reflected), as ext4 uses for group descriptor checksums.
fn crc16(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc ^= u16::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
        }
    }
    crc
}

fn journal_blocks(blocks: u64) -> u64 {
    match blocks {
        0..=32767 => 1024,
        32768..=262143 => 4096,
        262144..=524287 => 8192,
        _ => 16384,
    }
}

struct Layout {
    blocks: u64,
    itable_blocks: u64,
    gdt_blocks: u64,
}

impl Layout {
    fn group_start(&self, g: u64) -> u64 {
        g * BLOCKS_PER_GROUP
    }

    fn group_blocks(&self, g: u64) -> u64 {
        (self.blocks - self.group_start(g)).min(BLOCKS_PER_GROUP)
    }

    /// Metadata blocks at the start of group `g`: superblock + descriptors (if a backup
    /// group), block bitmap, inode bitmap, inode table.
    fn overhead(&self, g: u64) -> u64 {
        (if has_super(g) { 1 + self.gdt_blocks } else { 0 }) + 2 + self.itable_blocks
    }

    fn block_bitmap(&self, g: u64) -> u64 {
        self.group_start(g) + if has_super(g) { 1 + self.gdt_blocks } else { 0 }
    }
}

fn le16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}

fn le32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

fn set_bits(bitmap: &mut [u8], from: u64, to: u64) {
    for i in from..to {
        bitmap[(i / 8) as usize] |= 1 << (i % 8);
    }
}

/// An inode whose data is one extent (or none).
fn inode(mode: u16, links: u16, size: u64, start: u64, len: u64, time: u32) -> [u8; INODE_SIZE as usize] {
    let mut i = [0u8; INODE_SIZE as usize];
    le16(&mut i, 0, mode);
    le32(&mut i, 4, size as u32);
    for o in [8, 12, 16] {
        le32(&mut i, o, time);
    }
    le16(&mut i, 26, links);
    le32(&mut i, 28, (len * BLOCK / 512) as u32);
    le32(&mut i, 32, EXTENTS_FL);
    // extent header + one extent in i_block
    le16(&mut i, 40, 0xF30A);
    le16(&mut i, 42, u16::from(len > 0));
    le16(&mut i, 44, 4);
    le16(&mut i, 46, 0);
    if len > 0 {
        le32(&mut i, 52, 0);
        le16(&mut i, 56, len as u16);
        le16(&mut i, 58, (start >> 32) as u16);
        le32(&mut i, 60, start as u32);
    }
    le32(&mut i, 108, (size >> 32) as u32);
    le16(&mut i, 128, 32); // i_extra_isize
    le32(&mut i, 144, time); // i_crtime
    i
}

fn dirent(out: &mut Vec<u8>, ino: u32, name: &str, ftype: u8, rec_len: usize) {
    let start = out.len();
    out.extend_from_slice(&ino.to_le_bytes());
    out.extend_from_slice(&(rec_len as u16).to_le_bytes());
    out.push(name.len() as u8);
    out.push(ftype);
    out.extend_from_slice(name.as_bytes());
    out.resize(start + rec_len, 0);
}

fn dir_block(entries: &[(u32, &str)]) -> Vec<u8> {
    let mut b = Vec::with_capacity(BLOCK as usize);
    for (k, (ino, name)) in entries.iter().enumerate() {
        let rec = if k + 1 == entries.len() { BLOCK as usize - b.len() } else { (8 + name.len()).div_ceil(4) * 4 };
        dirent(&mut b, *ino, name, 2, rec);
    }
    b
}

fn empty_dir_block() -> Vec<u8> {
    let mut b = Vec::new();
    dirent(&mut b, 0, "", 0, BLOCK as usize);
    b
}

/// Format `bytes` bytes at `offset` of `dev` (a disk or partition) as ext4.
pub fn mkfs<W: Write + Seek>(dev: &mut W, offset: u64, bytes: u64, opts: &Options) -> io::Result<Made> {
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidInput, m.to_string());
    if opts.label.len() > 16 {
        return Err(bad("an ext4 label is at most 16 bytes"));
    }
    let blocks = (bytes / BLOCK).min(u64::from(u32::MAX));
    if blocks < 4096 {
        return Err(bad("an ext4 filesystem needs at least 16 MiB"));
    }
    let mut groups = blocks.div_ceil(BLOCKS_PER_GROUP);
    let inodes_per_group = {
        let want = (blocks * BLOCK / BYTES_PER_INODE).div_ceil(groups);
        let per_block = BLOCK / INODE_SIZE;
        want.div_ceil(per_block).max(1) * per_block
    }
    .min(BLOCK * 8);
    let itable_blocks = inodes_per_group * INODE_SIZE / BLOCK;
    let gdt_blocks = (groups * 32).div_ceil(BLOCK);
    let mut l = Layout { blocks, itable_blocks, gdt_blocks };
    // A last group too small for its own metadata is dropped.
    if l.group_blocks(groups - 1) < l.overhead(groups - 1) + 50 {
        groups -= 1;
        l = Layout { blocks: groups * BLOCKS_PER_GROUP, ..l };
    }
    let blocks = l.blocks;
    let jblocks = journal_blocks(blocks);

    let mut uuid = match opts.uuid {
        Some(u) => u,
        None => {
            let mut u = [0u8; 16];
            fs_random(&mut u)?;
            u
        }
    };
    uuid[6] = (uuid[6] & 0x0F) | 0x40;
    uuid[8] = (uuid[8] & 0x3F) | 0x80;
    let mut seed = [0u8; 16];
    if opts.uuid.is_some() {
        seed = uuid;
        seed[0] ^= 0x5a;
    } else {
        fs_random(&mut seed)?;
    }

    // Group 0 data: root directory, lost+found, journal, in that order.
    let g0_data = l.overhead(0);
    let root_block = g0_data;
    let lf_block = root_block + 1;
    let journal_start = lf_block + LOST_FOUND_BLOCKS;
    if journal_start + jblocks > l.group_blocks(0) {
        return Err(bad("filesystem too small"));
    }
    let used0 = g0_data + 1 + LOST_FOUND_BLOCKS + jblocks;

    let at = |dev: &mut W, block: u64, data: &[u8]| -> io::Result<()> {
        dev.seek(SeekFrom::Start(offset + block * BLOCK))?;
        dev.write_all(data)
    };

    // Group descriptors.
    let mut gdt = vec![0u8; (gdt_blocks * BLOCK) as usize];
    let mut free_total = 0u64;
    for g in 0..groups {
        let d = &mut gdt[(g * 32) as usize..(g * 32 + 32) as usize];
        let bb = l.block_bitmap(g);
        le32(d, 0, bb as u32);
        le32(d, 4, (bb + 1) as u32);
        le32(d, 8, (bb + 2) as u32);
        let used = if g == 0 { used0 } else { l.overhead(g) };
        let free = l.group_blocks(g) - used;
        free_total += free;
        le16(d, 12, free as u16);
        let used_inodes = if g == 0 { u64::from(FIRST_INO) } else { 0 };
        le16(d, 14, (inodes_per_group - used_inodes) as u16);
        le16(d, 16, if g == 0 { 2 } else { 0 });
        let mut flags = 0;
        if g != 0 {
            flags |= BG_INODE_UNINIT;
            if g != groups - 1 {
                flags |= BG_BLOCK_UNINIT;
            }
        }
        le16(d, 18, flags);
        le16(d, 28, (inodes_per_group - used_inodes) as u16); // bg_itable_unused
        let mut c = crc16(0xFFFF, &uuid);
        c = crc16(c, &(g as u32).to_le_bytes());
        c = crc16(c, &d[..30]);
        le16(d, 30, c);
    }

    let inodes = inodes_per_group * groups;
    let mut sb = [0u8; 1024];
    le32(&mut sb, 0, inodes as u32);
    le32(&mut sb, 4, blocks as u32);
    le32(&mut sb, 8, (blocks / 100) as u32); // 1% reserved for root
    le32(&mut sb, 12, free_total as u32);
    le32(&mut sb, 16, (inodes - u64::from(FIRST_INO)) as u32);
    le32(&mut sb, 20, 0); // first data block
    le32(&mut sb, 24, 2); // 4096 = 1024 << 2
    le32(&mut sb, 28, 2);
    le32(&mut sb, 32, BLOCKS_PER_GROUP as u32);
    le32(&mut sb, 36, BLOCKS_PER_GROUP as u32);
    le32(&mut sb, 40, inodes_per_group as u32);
    le32(&mut sb, 48, opts.time);
    le16(&mut sb, 54, 0xFFFF); // no forced checks
    le16(&mut sb, 56, 0xEF53);
    le16(&mut sb, 58, 1); // clean
    le16(&mut sb, 60, 1); // errors: continue
    le32(&mut sb, 64, opts.time);
    le32(&mut sb, 76, 1); // dynamic revision
    le32(&mut sb, 84, FIRST_INO);
    le16(&mut sb, 88, INODE_SIZE as u16);
    le32(&mut sb, 92, COMPAT_HAS_JOURNAL | COMPAT_EXT_ATTR | COMPAT_DIR_INDEX);
    le32(&mut sb, 96, INCOMPAT_FILETYPE | INCOMPAT_EXTENTS);
    le32(&mut sb, 100, RO_SPARSE_SUPER | RO_LARGE_FILE | RO_HUGE_FILE | RO_GDT_CSUM | RO_DIR_NLINK | RO_EXTRA_ISIZE);
    sb[104..120].copy_from_slice(&uuid);
    sb[120..120 + opts.label.len()].copy_from_slice(opts.label.as_bytes());
    le32(&mut sb, 224, JOURNAL_INO);
    sb[236..252].copy_from_slice(&seed);
    sb[252] = 1; // half_md4 directory hashes
    sb[253] = 1; // s_jnl_backup_type: copy of the journal inode's blocks below
    le32(&mut sb, 256, 0x0004 | 0x0008); // default mount options: user_xattr, acl
    le32(&mut sb, 264, opts.time);
    le16(&mut sb, 348, 32);
    le16(&mut sb, 350, 32);
    le32(&mut sb, 352, 0x1); // signed directory hash (x86)
    let jinode = inode(S_IFREG | 0o600, 1, jblocks * BLOCK, journal_start, jblocks, opts.time);
    sb[268..328].copy_from_slice(&jinode[40..100]); // s_jnl_blocks[0..15] = i_block
    le32(&mut sb, 328, (jblocks * BLOCK >> 32) as u32); // [15] = i_size_high
    le32(&mut sb, 332, (jblocks * BLOCK) as u32); // [16] = i_size

    // Superblock and descriptor copies.
    for g in 0..groups {
        if !has_super(g) {
            continue;
        }
        let mut s = sb;
        le16(&mut s, 90, g as u16);
        if g == 0 {
            let mut first = vec![0u8; BLOCK as usize];
            first[1024..2048].copy_from_slice(&s);
            at(dev, 0, &first)?;
        } else {
            let mut blk = vec![0u8; BLOCK as usize];
            blk[..1024].copy_from_slice(&s);
            at(dev, l.group_start(g), &blk)?;
        }
        at(dev, l.group_start(g) + 1, &gdt)?;
    }

    // Bitmaps for group 0 and the last group (the others are left uninitialised).
    for g in [0, groups - 1] {
        let mut bb = vec![0u8; BLOCK as usize];
        let used = if g == 0 { used0 } else { l.overhead(g) };
        set_bits(&mut bb, 0, used);
        set_bits(&mut bb, l.group_blocks(g), BLOCK * 8); // past the end of the filesystem
        at(dev, l.block_bitmap(g), &bb)?;
        if g == 0 {
            let mut ib = vec![0u8; BLOCK as usize];
            set_bits(&mut ib, 0, u64::from(FIRST_INO));
            set_bits(&mut ib, inodes_per_group, BLOCK * 8);
            at(dev, l.block_bitmap(0) + 1, &ib)?;
        }
        if g == 0 && groups == 1 {
            break;
        }
    }

    // Group 0 inode table, first block (inodes 1-16). The rest is zeroed by the kernel in
    // the background (bg_itable_unused tells it and e2fsck that nothing is there).
    let mut itable = vec![0u8; BLOCK as usize];
    let put = |t: &mut Vec<u8>, ino: u32, data: &[u8; INODE_SIZE as usize]| {
        let o = ((ino - 1) as u64 * INODE_SIZE) as usize;
        t[o..o + INODE_SIZE as usize].copy_from_slice(data);
    };
    put(&mut itable, ROOT_INO, &inode(S_IFDIR | 0o755, 3, BLOCK, root_block, 1, opts.time));
    put(&mut itable, JOURNAL_INO, &jinode);
    put(&mut itable, LOST_FOUND_INO, &inode(S_IFDIR | 0o700, 2, LOST_FOUND_BLOCKS * BLOCK, lf_block, LOST_FOUND_BLOCKS, opts.time));
    // Inode 1 (bad blocks) and the other reserved ones: empty but with valid times.
    for ino in [1u32, 3, 4, 5, 6, 7, 9, 10] {
        let mut i = [0u8; INODE_SIZE as usize];
        for o in [8, 12, 16] {
            le32(&mut i, o, opts.time);
        }
        le16(&mut i, 128, 32);
        put(&mut itable, ino, &i);
    }
    at(dev, l.block_bitmap(0) + 2, &itable)?;

    // Directories.
    at(dev, root_block, &dir_block(&[(ROOT_INO, "."), (ROOT_INO, ".."), (LOST_FOUND_INO, "lost+found")]))?;
    at(dev, lf_block, &dir_block(&[(LOST_FOUND_INO, "."), (ROOT_INO, "..")]))?;
    for k in 1..LOST_FOUND_BLOCKS {
        at(dev, lf_block + k, &empty_dir_block())?;
    }

    // Journal: a clean v2 superblock, the rest zeroed (big-endian fields).
    let mut js = vec![0u8; BLOCK as usize];
    let be = |b: &mut [u8], o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_be_bytes());
    be(&mut js, 0, 0xC03B_3998);
    be(&mut js, 4, 4);
    be(&mut js, 12, BLOCK as u32);
    be(&mut js, 16, jblocks as u32);
    be(&mut js, 20, 1);
    be(&mut js, 24, 1);
    js[48..64].copy_from_slice(&uuid);
    be(&mut js, 64, 1);
    at(dev, journal_start, &js)?;
    let zeros = vec![0u8; (BLOCK * 256) as usize];
    let mut b = journal_start + 1;
    while b < journal_start + jblocks {
        let n = (journal_start + jblocks - b).min(256);
        at(dev, b, &zeros[..(n * BLOCK) as usize])?;
        b += n;
    }
    dev.flush()?;
    Ok(Made { uuid, blocks, groups })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_groups() {
        let g: Vec<u64> = (0..100).filter(|&g| has_super(g)).collect();
        assert_eq!(g, [0, 1, 3, 5, 7, 9, 25, 27, 49, 81]);
    }

    #[test]
    fn crc16_known() {
        // CRC-16/ARC of "123456789" starting from 0 is 0xBB3D
        assert_eq!(crc16(0, b"123456789"), 0xBB3D);
    }
}
