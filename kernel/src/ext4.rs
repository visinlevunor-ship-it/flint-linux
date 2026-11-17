//! ext4 (and ext2/ext3), read-only: inodes, extents and block maps, directories, symbolic
//! links. Enough to see what is on Flint's data partition.

use crate::block::{self, Part};
use alloc::string::String;
use alloc::vec::Vec;

pub struct Ext4 {
    part: Part,
    bs: u64,
    ipg: u64,
    isize: u64,
    desc_size: u64,
    gdt: u64, // byte offset of the group descriptors
    groups: u64,
}

fn le16(b: &[u8], o: usize) -> u64 {
    u16::from_le_bytes([b[o], b[o + 1]]) as u64
}
fn le32(b: &[u8], o: usize) -> u64 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as u64
}

pub struct Inode {
    pub mode: u32,
    pub size: u64,
    pub mtime: u64,
    pub rdev: u64,
    flags: u64,
    block: [u8; 60],
}

impl Ext4 {
    pub fn open(part: &Part) -> Option<Ext4> {
        let mut sb = [0u8; 1024];
        block::part_read(part, 1024, &mut sb).ok()?;
        if le16(&sb, 56) != 0xEF53 {
            return None;
        }
        let bs = 1024u64 << le32(&sb, 24);
        let incompat = le32(&sb, 96);
        let desc_size = if incompat & 0x80 != 0 { le16(&sb, 254).max(32) } else { 32 };
        let isize = if le32(&sb, 76) >= 1 { le16(&sb, 88) } else { 128 };
        let blocks = le32(&sb, 4) | if incompat & 0x80 != 0 { le32(&sb, 0x150) << 32 } else { 0 };
        let bpg = le32(&sb, 32);
        let first = le32(&sb, 20);
        Some(Ext4 { part: part.clone(), bs, ipg: le32(&sb, 40), isize, desc_size, gdt: (first + 1) * bs, groups: (blocks - first).div_ceil(bpg) })
    }

    fn read(&self, off: u64, out: &mut [u8]) -> Option<()> {
        block::part_read(&self.part, off, out).ok()
    }

    pub fn inode(&self, n: u64) -> Option<Inode> {
        if n == 0 {
            return None;
        }
        let g = (n - 1) / self.ipg;
        if g >= self.groups {
            return None;
        }
        let mut d = alloc::vec![0u8; self.desc_size as usize];
        self.read(self.gdt + g * self.desc_size, &mut d)?;
        let table = le32(&d, 8) | if self.desc_size >= 64 { le32(&d, 0x28) << 32 } else { 0 };
        let mut i = alloc::vec![0u8; self.isize as usize];
        self.read(table * self.bs + ((n - 1) % self.ipg) * self.isize, &mut i)?;
        let mut block = [0u8; 60];
        block.copy_from_slice(&i[40..100]);
        let mode = le16(&i, 0) as u32;
        // device numbers live in i_block for character and block devices
        let rdev = if mode & 0o170000 == 0o020000 || mode & 0o170000 == 0o060000 {
            let old = le32(&i, 40);
            if old != 0 { old } else { le32(&i, 44) }
        } else {
            0
        };
        Some(Inode { mode, size: le32(&i, 4) | le32(&i, 108) << 32, mtime: le32(&i, 16), rdev, flags: le32(&i, 32), block })
    }

    /// The physical block for logical block `lb` of an inode (0: a hole).
    fn map(&self, ino: &Inode, lb: u64) -> u64 {
        if ino.flags & 0x80000 != 0 {
            self.extent(&ino.block, lb).unwrap_or(0)
        } else {
            self.indirect(&ino.block, lb).unwrap_or(0)
        }
    }

    fn extent(&self, node: &[u8], lb: u64) -> Option<u64> {
        if le16(node, 0) != 0xF30A {
            return None;
        }
        let entries = le16(node, 2) as usize;
        let depth = le16(node, 6);
        if depth == 0 {
            for i in 0..entries {
                let e = &node[12 + i * 12..24 + i * 12];
                let first = le32(e, 0);
                let mut len = le16(e, 4);
                let uninit = len > 32768;
                if uninit {
                    len -= 32768;
                }
                if lb >= first && lb < first + len {
                    if uninit {
                        return Some(0); // allocated but unwritten: zeros
                    }
                    let start = le16(e, 6) << 32 | le32(e, 8);
                    return Some(start + lb - first);
                }
            }
            return Some(0);
        }
        // the last index entry starting at or before lb
        let mut child = None;
        for i in 0..entries {
            let e = &node[12 + i * 12..24 + i * 12];
            if le32(e, 0) <= lb {
                child = Some(le16(e, 8) << 32 | le32(e, 4));
            }
        }
        let mut b = alloc::vec![0u8; self.bs as usize];
        self.read(child? * self.bs, &mut b)?;
        self.extent(&b, lb)
    }

    fn indirect(&self, blocks: &[u8], lb: u64) -> Option<u64> {
        let per = self.bs / 4;
        let ptr = |blk: u64, i: u64| -> Option<u64> {
            if blk == 0 {
                return Some(0);
            }
            let mut b = [0u8; 4];
            self.read(blk * self.bs + i * 4, &mut b)?;
            Some(u32::from_le_bytes(b) as u64)
        };
        if lb < 12 {
            return Some(le32(blocks, lb as usize * 4));
        }
        let lb = lb - 12;
        if lb < per {
            return ptr(le32(blocks, 48), lb);
        }
        let lb = lb - per;
        if lb < per * per {
            return ptr(ptr(le32(blocks, 52), lb / per)?, lb % per);
        }
        let lb = lb - per * per;
        ptr(ptr(ptr(le32(blocks, 56), lb / (per * per))?, (lb / per) % per)?, lb % per)
    }

    /// A file's contents.
    pub fn read_all(&self, ino: &Inode) -> Vec<u8> {
        if ino.flags & 0x1000_0000 != 0 {
            // inline data (the first 60 bytes; longer inline files keep the rest in an xattr)
            return ino.block[..(ino.size as usize).min(60)].to_vec();
        }
        let mut out = alloc::vec![0u8; ino.size as usize];
        let blocks = ino.size.div_ceil(self.bs);
        for lb in 0..blocks {
            let pb = self.map(ino, lb);
            let start = (lb * self.bs) as usize;
            let end = (start + self.bs as usize).min(out.len());
            if pb != 0 && self.read(pb * self.bs, &mut out[start..end]).is_none() {
                break;
            }
        }
        out
    }

    /// Bytes at `pos` of a file into `out`: how many.
    pub fn read_at(&self, ino: &Inode, pos: u64, out: &mut [u8]) -> usize {
        if pos >= ino.size {
            return 0;
        }
        if ino.flags & 0x1000_0000 != 0 {
            let all = self.read_all(ino);
            let k = (all.len() - pos as usize).min(out.len());
            out[..k].copy_from_slice(&all[pos as usize..pos as usize + k]);
            return k;
        }
        let k = ((ino.size - pos) as usize).min(out.len());
        let mut done = 0usize;
        while done < k {
            let p = pos + done as u64;
            let lb = p / self.bs;
            let in_b = (p % self.bs) as usize;
            let n = (self.bs as usize - in_b).min(k - done);
            let pb = self.map(ino, lb);
            if pb == 0 {
                out[done..done + n].fill(0);
            } else if self.read(pb * self.bs + in_b as u64, &mut out[done..done + n]).is_none() {
                return done;
            }
            done += n;
        }
        k
    }

    pub fn link_target(&self, ino: &Inode) -> String {
        if ino.size < 60 && ino.flags & 0x80000 == 0 && ino.flags & 0x1000_0000 == 0 {
            return String::from_utf8_lossy(&ino.block[..ino.size as usize]).into_owned();
        }
        String::from_utf8_lossy(&self.read_all(ino)).into_owned()
    }

    /// A directory's entries: (name, inode number).
    pub fn list(&self, ino: &Inode) -> Vec<(String, u64)> {
        let data = self.read_all(ino);
        let mut out = Vec::new();
        let mut off = 0usize;
        while off + 8 <= data.len() {
            let n = le32(&data, off);
            let rec = le16(&data, off + 4) as usize;
            let nlen = data[off + 6] as usize;
            if rec < 8 {
                break;
            }
            if n != 0 && off + 8 + nlen <= data.len() {
                let name = String::from_utf8_lossy(&data[off + 8..off + 8 + nlen]).into_owned();
                if name != "." && name != ".." {
                    out.push((name, n));
                }
            }
            off += rec;
        }
        out
    }

    /// The inode for `path` (components below `from`), not following symbolic links.
    pub fn walk(&self, from: u64, path: &str) -> Option<u64> {
        let mut n = from;
        for c in path.split('/').filter(|c| !c.is_empty()) {
            let ino = self.inode(n)?;
            if ino.mode & 0o170000 != 0o040000 {
                return None;
            }
            n = self.list(&ino).into_iter().find(|(name, _)| name == c)?.1;
        }
        Some(n)
    }
}
