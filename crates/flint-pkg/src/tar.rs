//! A small ustar reader and writer. Supports regular files, directories and symlinks
//! (the only things packages contain) plus GNU long names when reading.

use std::io::{self, Read, Seek, SeekFrom, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub mtime: u64,
    pub kind: Kind,
    pub link: String,
}

const BLOCK: usize = 512;

fn put_octal(field: &mut [u8], v: u64) -> io::Result<()> {
    let digits = field.len() - 1;
    let s = format!("{v:0digits$o}");
    if s.len() > digits {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "value too large for a tar header field"));
    }
    field[..digits].copy_from_slice(s.as_bytes());
    field[digits] = 0;
    Ok(())
}

fn put_str(field: &mut [u8], s: &str) -> io::Result<()> {
    if s.len() > field.len() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("'{s}' is too long for a tar header field")));
    }
    field[..s.len()].copy_from_slice(s.as_bytes());
    Ok(())
}

/// Split a path into ustar `prefix` and `name` at a `/` so that both fit.
fn split_path(path: &str) -> io::Result<(&str, &str)> {
    if path.len() <= 100 {
        return Ok(("", path));
    }
    for (i, _) in path.match_indices('/') {
        let (prefix, name) = (&path[..i], &path[i + 1..]);
        if prefix.len() <= 155 && name.len() <= 100 && !name.is_empty() {
            return Ok((prefix, name));
        }
    }
    Err(io::Error::new(io::ErrorKind::InvalidInput, format!("path too long for a tar header: {path}")))
}

fn header_block(h: &Header) -> io::Result<[u8; BLOCK]> {
    let mut b = [0u8; BLOCK];
    let (prefix, name) = split_path(&h.name)?;
    put_str(&mut b[0..100], name)?;
    put_octal(&mut b[100..108], u64::from(h.mode & 0o7777))?;
    put_octal(&mut b[108..116], u64::from(h.uid))?;
    put_octal(&mut b[116..124], u64::from(h.gid))?;
    put_octal(&mut b[124..136], if h.kind == Kind::File { h.size } else { 0 })?;
    put_octal(&mut b[136..148], h.mtime)?;
    b[148..156].fill(b' ');
    b[156] = match h.kind {
        Kind::File => b'0',
        Kind::Dir => b'5',
        Kind::Symlink => b'2',
    };
    put_str(&mut b[157..257], &h.link)?;
    b[257..263].copy_from_slice(b"ustar\0");
    b[263..265].copy_from_slice(b"00");
    put_str(&mut b[265..297], "root")?;
    put_str(&mut b[297..329], "root")?;
    put_str(&mut b[345..500], prefix)?;
    let sum: u32 = b.iter().map(|&x| u32::from(x)).sum();
    let s = format!("{sum:06o}\0 ");
    b[148..156].copy_from_slice(s.as_bytes());
    Ok(b)
}

pub struct Writer<W: Write> {
    w: W,
}

impl<W: Write> Writer<W> {
    pub fn new(w: W) -> Self {
        Writer { w }
    }

    /// Write an entry; `data` must yield exactly `h.size` bytes for a file.
    pub fn entry(&mut self, h: &Header, data: &mut dyn Read) -> io::Result<()> {
        self.w.write_all(&header_block(h)?)?;
        if h.kind == Kind::File {
            let copied = io::copy(&mut data.take(h.size), &mut self.w)?;
            if copied != h.size {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("{} changed size while being packed", h.name)));
            }
            let pad = (BLOCK - (h.size as usize % BLOCK)) % BLOCK;
            self.w.write_all(&[0u8; BLOCK][..pad])?;
        }
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<W> {
        self.w.write_all(&[0u8; BLOCK * 2])?;
        self.w.flush()?;
        Ok(self.w)
    }
}

pub struct Entry {
    pub header: Header,
    /// Offset of the entry's data in the archive.
    pub offset: u64,
}

pub struct Reader<R: Read + Seek> {
    r: R,
    next: u64,
}

fn parse_octal(field: &[u8]) -> io::Result<u64> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "base-256 tar numbers are not supported"));
    }
    let text: String = field.iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
    let t = text.trim();
    if t.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(t, 8).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad number in tar header"))
}

fn parse_str(field: &[u8]) -> String {
    String::from_utf8_lossy(&field[..field.iter().position(|&b| b == 0).unwrap_or(field.len())]).into_owned()
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

impl<R: Read + Seek> Reader<R> {
    pub fn new(r: R) -> Self {
        Reader { r, next: 0 }
    }

    fn read_block(&mut self, pos: u64) -> io::Result<Option<[u8; BLOCK]>> {
        self.r.seek(SeekFrom::Start(pos))?;
        let mut b = [0u8; BLOCK];
        let mut got = 0;
        while got < BLOCK {
            let n = self.r.read(&mut b[got..])?;
            if n == 0 {
                break;
            }
            got += n;
        }
        match got {
            0 => Ok(None),
            BLOCK => Ok(Some(b)),
            _ => Err(bad("truncated tar archive")),
        }
    }

    /// The next entry, or `None` at the end of the archive.
    pub fn next_entry(&mut self) -> io::Result<Option<Entry>> {
        let mut long_name: Option<String> = None;
        loop {
            let Some(b) = self.read_block(self.next)? else { return Ok(None) };
            if b.iter().all(|&x| x == 0) {
                return Ok(None);
            }
            let want = parse_octal(&b[148..156])?;
            let sum: u32 = b.iter().enumerate().map(|(i, &x)| if (148..156).contains(&i) { u32::from(b' ') } else { u32::from(x) }).sum();
            if u64::from(sum) != want {
                return Err(bad("tar header checksum mismatch (not a tar archive?)"));
            }
            let size = parse_octal(&b[124..136])?;
            let offset = self.next + BLOCK as u64;
            let data_blocks = size.div_ceil(BLOCK as u64) * BLOCK as u64;
            let typeflag = b[156];
            if typeflag == b'L' {
                if size > 65536 {
                    return Err(bad("GNU long name too long"));
                }
                self.r.seek(SeekFrom::Start(offset))?;
                let mut buf = vec![0u8; size as usize];
                self.r.read_exact(&mut buf)?;
                long_name = Some(parse_str(&buf));
                self.next = offset + data_blocks;
                continue;
            }
            let kind = match typeflag {
                0 | b'0' => Kind::File,
                b'5' => Kind::Dir,
                b'2' => Kind::Symlink,
                b'1' => return Err(bad("hard links are not supported in packages")),
                other => return Err(bad(&format!("unsupported tar entry type {:?}", other as char))),
            };
            let name = match long_name.take() {
                Some(n) => n,
                None => {
                    let prefix = parse_str(&b[345..500]);
                    let name = parse_str(&b[0..100]);
                    if prefix.is_empty() {
                        name
                    } else {
                        format!("{prefix}/{name}")
                    }
                }
            };
            let size = if kind == Kind::File { size } else { 0 };
            self.next = offset + if kind == Kind::File { data_blocks } else { 0 };
            let header = Header {
                name: name.trim_end_matches('/').to_string(),
                mode: parse_octal(&b[100..108])? as u32 & 0o7777,
                uid: parse_octal(&b[108..116])? as u32,
                gid: parse_octal(&b[116..124])? as u32,
                size,
                mtime: parse_octal(&b[136..148])?,
                kind,
                link: parse_str(&b[157..257]),
            };
            return Ok(Some(Entry { header, offset }));
        }
    }

    /// A reader positioned at the entry's data, limited to its size.
    pub fn data(&mut self, e: &Entry) -> io::Result<impl Read + '_> {
        self.r.seek(SeekFrom::Start(e.offset))?;
        Ok((&mut self.r).take(e.header.size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn file(name: &str, size: u64) -> Header {
        Header { name: name.into(), mode: 0o644, uid: 0, gid: 0, size, mtime: 0, kind: Kind::File, link: String::new() }
    }

    fn sample() -> Vec<u8> {
        let mut w = Writer::new(Vec::new());
        w.entry(&Header { kind: Kind::Dir, size: 0, mode: 0o755, ..file("usr/bin", 0) }, &mut io::empty()).unwrap();
        w.entry(&Header { mode: 0o4755, ..file("usr/bin/tool", 5) }, &mut &b"hello"[..]).unwrap();
        w.entry(&Header { kind: Kind::Symlink, size: 0, link: "tool".into(), ..file("usr/bin/alias", 0) }, &mut io::empty()).unwrap();
        w.entry(&file("big", 1000), &mut &vec![7u8; 1000][..]).unwrap();
        w.finish().unwrap()
    }

    #[test]
    fn round_trip() {
        let bytes = sample();
        assert_eq!(bytes.len() % 512, 0);
        let mut r = Reader::new(Cursor::new(bytes));
        let a = r.next_entry().unwrap().unwrap();
        assert_eq!((a.header.name.as_str(), a.header.kind, a.header.mode), ("usr/bin", Kind::Dir, 0o755));
        let b = r.next_entry().unwrap().unwrap();
        assert_eq!((b.header.name.as_str(), b.header.size, b.header.mode), ("usr/bin/tool", 5, 0o4755));
        let mut data = String::new();
        r.data(&b).unwrap().read_to_string(&mut data).unwrap();
        assert_eq!(data, "hello");
        let c = r.next_entry().unwrap().unwrap();
        assert_eq!((c.header.kind, c.header.link.as_str()), (Kind::Symlink, "tool"));
        let d = r.next_entry().unwrap().unwrap();
        assert_eq!(d.header.size, 1000);
        let mut big = Vec::new();
        r.data(&d).unwrap().read_to_end(&mut big).unwrap();
        assert_eq!(big, vec![7u8; 1000]);
        assert!(r.next_entry().unwrap().is_none());
    }

    #[test]
    fn long_paths_use_the_prefix_field() {
        let dir = "d".repeat(60);
        let path = format!("{dir}/{dir}/{dir}/file.txt");
        assert!(path.len() > 100);
        let mut w = Writer::new(Vec::new());
        w.entry(&file(&path, 1), &mut &b"x"[..]).unwrap();
        let mut r = Reader::new(Cursor::new(w.finish().unwrap()));
        assert_eq!(r.next_entry().unwrap().unwrap().header.name, path);
        let mut w = Writer::new(Vec::new());
        assert!(w.entry(&file(&"x".repeat(300), 0), &mut io::empty()).is_err());
    }

    #[test]
    fn detects_damage() {
        let mut bytes = sample();
        bytes[10] ^= 0xff;
        assert!(Reader::new(Cursor::new(bytes)).next_entry().is_err(), "bad checksum");
        let bytes = sample();
        assert!(Reader::new(Cursor::new(bytes[..300].to_vec())).next_entry().is_err(), "truncated header");
        assert!(Reader::new(Cursor::new(b"this is not a tar file at all".repeat(30))).next_entry().is_err());
        assert!(Reader::new(Cursor::new(Vec::new())).next_entry().unwrap().is_none());
    }

    #[test]
    fn size_mismatch_is_reported() {
        let mut w = Writer::new(Vec::new());
        assert!(w.entry(&file("f", 10), &mut &b"short"[..]).is_err());
    }
}
