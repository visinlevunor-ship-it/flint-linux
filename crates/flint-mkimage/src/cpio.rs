//! newc cpio archives, the format the Linux kernel unpacks as its initial root filesystem.
//! Everything is stored as owned by root with mtime 0, in sorted order, so the same input
//! tree always gives the same bytes.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

pub struct Writer<W: Write> {
    w: W,
    ino: u32,
    written: u64,
}

const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;

impl<W: Write> Writer<W> {
    pub fn new(w: W) -> Self {
        Writer { w, ino: 1, written: 0 }
    }

    fn put(&mut self, b: &[u8]) -> io::Result<()> {
        self.w.write_all(b)?;
        self.written += b.len() as u64;
        Ok(())
    }

    fn pad4(&mut self) -> io::Result<()> {
        let n = (4 - (self.written % 4) as usize) % 4;
        self.put(&[0u8; 3][..n])
    }

    /// One entry. `name` has no leading slash.
    pub fn entry(&mut self, name: &str, mode: u32, data: &[u8]) -> io::Result<()> {
        let ino = self.ino;
        self.ino += 1;
        let nlink = if mode & 0o170000 == S_IFDIR { 2 } else { 1 };
        let header = format!(
            "070701{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
            ino,
            mode,
            0, // uid
            0, // gid
            nlink,
            0, // mtime
            data.len(),
            0,
            0,
            0,
            0,
            name.len() + 1,
            0 // checksum (unused in newc)
        );
        self.put(header.as_bytes())?;
        self.put(name.as_bytes())?;
        self.put(&[0])?;
        self.pad4()?;
        self.put(data)?;
        self.pad4()
    }

    pub fn dir(&mut self, name: &str, perm: u32) -> io::Result<()> {
        self.entry(name, S_IFDIR | (perm & 0o7777), &[])
    }

    pub fn file(&mut self, name: &str, perm: u32, data: &[u8]) -> io::Result<()> {
        self.entry(name, S_IFREG | (perm & 0o7777), data)
    }

    pub fn symlink(&mut self, name: &str, target: &str) -> io::Result<()> {
        self.entry(name, S_IFLNK | 0o777, target.as_bytes())
    }

    pub fn finish(mut self) -> io::Result<W> {
        self.ino = 0;
        self.entry("TRAILER!!!", 0, &[])?;
        // The kernel reads whole 512-byte blocks.
        let n = (512 - (self.written % 512) as usize) % 512;
        self.put(&vec![0u8; n])?;
        self.w.flush()?;
        Ok(self.w)
    }
}

/// Add the tree under `src` (its contents, not `src` itself) with names below `prefix`.
pub fn add_tree<W: Write>(w: &mut Writer<W>, src: &Path, prefix: &str) -> io::Result<()> {
    let mut names: Vec<_> = fs::read_dir(src)?.collect::<Result<Vec<_>, _>>()?;
    names.sort_by_key(|e| e.file_name());
    for e in names {
        let path = e.path();
        let fname = e.file_name();
        let fname = fname.to_str().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("{}: file name is not UTF-8", path.display())))?;
        let name = if prefix.is_empty() { fname.to_string() } else { format!("{prefix}/{fname}") };
        let meta = fs::symlink_metadata(&path)?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            let target = fs::read_link(&path)?;
            w.symlink(&name, &target.to_string_lossy())?;
        } else if ft.is_dir() {
            w.dir(&name, meta.permissions().mode())?;
            add_tree(w, &path, &name)?;
        } else if ft.is_file() {
            let data = fs::read(&path)?;
            w.file(&name, meta.mode(), &data)?;
        } else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{}: only files, directories and symlinks are supported", path.display())));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(mut b: &[u8]) -> Vec<(String, u32, Vec<u8>)> {
        let mut out = Vec::new();
        let mut off = 0usize;
        loop {
            let h = std::str::from_utf8(&b[..110]).unwrap();
            assert_eq!(&h[..6], "070701");
            let field = |i: usize| u32::from_str_radix(&h[6 + i * 8..14 + i * 8], 16).unwrap() as usize;
            let (mode, size, namesize) = (field(1) as u32, field(6), field(11));
            let name = std::str::from_utf8(&b[110..110 + namesize - 1]).unwrap().to_string();
            let mut pos = 110 + namesize;
            pos += (4 - (off + pos) % 4) % 4;
            let data = b[pos..pos + size].to_vec();
            pos += size;
            pos += (4 - (off + pos) % 4) % 4;
            if name == "TRAILER!!!" {
                return out;
            }
            out.push((name, mode, data));
            b = &b[pos..];
            off += pos;
        }
    }

    #[test]
    fn round_trip_and_alignment() {
        let mut w = Writer::new(Vec::new());
        w.dir("bin", 0o755).unwrap();
        w.file("bin/x", 0o4755, b"hello").unwrap();
        w.symlink("bin/sh", "flint").unwrap();
        w.file("e", 0o644, b"").unwrap();
        let bytes = w.finish().unwrap();
        assert_eq!(bytes.len() % 512, 0);
        let e = parse(&bytes);
        assert_eq!(e.len(), 4);
        assert_eq!(e[0], ("bin".into(), 0o040755, vec![]));
        assert_eq!(e[1], ("bin/x".into(), 0o104755, b"hello".to_vec()));
        assert_eq!(e[2], ("bin/sh".into(), 0o120777, b"flint".to_vec()));
        assert_eq!(e[3].0, "e");
    }

    #[test]
    fn trees_are_deterministic() {
        let d = std::env::temp_dir().join(format!("mkimage-cpio-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("b/c")).unwrap();
        fs::write(d.join("b/c/f"), "1").unwrap();
        fs::write(d.join("a"), "2").unwrap();
        std::os::unix::fs::symlink("a", d.join("z")).unwrap();
        let build = || {
            let mut w = Writer::new(Vec::new());
            add_tree(&mut w, &d, "").unwrap();
            w.finish().unwrap()
        };
        let one = build();
        assert_eq!(one, build());
        let names: Vec<String> = parse(&one).into_iter().map(|e| e.0).collect();
        assert_eq!(names, ["a", "b", "b/c", "b/c/f", "z"]);
        let _ = fs::remove_dir_all(&d);
    }
}
