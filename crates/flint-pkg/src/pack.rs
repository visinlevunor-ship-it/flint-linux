//! Building `.fpk` packages and repository indexes.

use std::fs;
use std::io::{self, Read, Seek};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use flint_auth::hash::hex;
use flint_auth::sha256::Sha256;

use crate::control::{format_index, Control, IndexEntry};
use crate::paths::normalize_rel;
use crate::tar::{Header, Kind, Reader, Writer};

pub const CONTROL_NAME: &str = ".FLINTPKG";

pub fn hash_file(path: &Path) -> io::Result<(String, u64)> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 65536];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex(&h.finalize()), total))
}

/// A symlink target is stored as-is; it only has to be printable on one line.
pub fn valid_link_target(t: &str) -> bool {
    !t.is_empty() && t.len() <= 100 && !t.chars().any(|c| matches!(c, '\0' | '\t' | '\n' | '\r'))
}

struct Item {
    rel: String,
    path: PathBuf,
    meta: fs::Metadata,
}

fn walk(dir: &Path, rel: &str, out: &mut Vec<Item>) -> Result<(), String> {
    let mut names: Vec<String> = Vec::new();
    for e in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let e = e.map_err(|e| format!("{}: {e}", dir.display()))?;
        let n = e.file_name().into_string().map_err(|n| format!("{}: file name is not UTF-8: {n:?}", dir.display()))?;
        names.push(n);
    }
    names.sort();
    for n in names {
        let path = dir.join(&n);
        let r = if rel.is_empty() { n.clone() } else { format!("{rel}/{n}") };
        let meta = fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let is_dir = meta.is_dir();
        out.push(Item { rel: normalize_rel(&r)?, path: path.clone(), meta });
        if is_dir {
            walk(&path, &r, out)?;
        }
    }
    Ok(())
}

/// Pack `src/files` into `out_dir/NAME-VERSION.fpk` using `src/control`.
/// Every entry is stored as owned by root; mtimes come from $SOURCE_DATE_EPOCH (default 0),
/// so the same input gives a byte-identical package.
pub fn build(src: &Path, out_dir: &Path) -> Result<(PathBuf, String, u64), String> {
    let control_text = fs::read_to_string(src.join("control")).map_err(|e| format!("{}: {e}", src.join("control").display()))?;
    let control = Control::parse(&control_text)?;
    let mut items = Vec::new();
    let files = src.join("files");
    if files.is_dir() {
        walk(&files, "", &mut items)?;
    }
    for c in &control.config {
        let ok = items.iter().any(|i| &i.rel == c && i.meta.is_file());
        if !ok {
            return Err(format!("config file '{c}' is not a regular file in the package"));
        }
    }
    let mtime: u64 = std::env::var("SOURCE_DATE_EPOCH").ok().and_then(|v| v.parse().ok()).unwrap_or(0);

    fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let out = out_dir.join(format!("{}-{}.fpk", control.name, control.version));
    let tmp = out_dir.join(format!(".{}-{}.fpk.tmp", control.name, control.version));
    let file = fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let result = (|| -> Result<(), String> {
        let mut w = Writer::new(io::BufWriter::new(file));
        let err = |e: io::Error| e.to_string();
        let ctext = control.to_text();
        w.entry(&Header { name: CONTROL_NAME.into(), mode: 0o644, uid: 0, gid: 0, size: ctext.len() as u64, mtime, kind: Kind::File, link: String::new() }, &mut ctext.as_bytes()).map_err(err)?;
        for it in &items {
            let mode = it.meta.mode() & 0o7777;
            let ft = it.meta.file_type();
            if ft.is_dir() {
                w.entry(&Header { name: it.rel.clone(), mode, uid: 0, gid: 0, size: 0, mtime, kind: Kind::Dir, link: String::new() }, &mut io::empty()).map_err(err)?;
            } else if ft.is_symlink() {
                let target = fs::read_link(&it.path).map_err(|e| e.to_string())?.to_string_lossy().into_owned();
                if !valid_link_target(&target) {
                    return Err(format!("symlink {} has an unsupported target {target:?}", it.rel));
                }
                w.entry(&Header { name: it.rel.clone(), mode: 0o777, uid: 0, gid: 0, size: 0, mtime, kind: Kind::Symlink, link: target }, &mut io::empty()).map_err(err)?;
            } else if ft.is_file() {
                let mut f = fs::File::open(&it.path).map_err(|e| format!("{}: {e}", it.path.display()))?;
                w.entry(&Header { name: it.rel.clone(), mode, uid: 0, gid: 0, size: it.meta.len(), mtime, kind: Kind::File, link: String::new() }, &mut f).map_err(err)?;
            } else {
                return Err(format!("{}: only files, directories and symlinks can be packaged", it.rel));
            }
        }
        w.finish().map_err(err)?;
        Ok(())
    })()
    .and_then(|()| {
        // Packages are gzip-compressed tar archives (readers also take plain tar).
        let tar = fs::read(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        fs::write(&tmp, flint_gzip::compress(&tar, flint_gzip::Level::DEFAULT)).map_err(|e| format!("{}: {e}", tmp.display()))
    });
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::rename(&tmp, &out).map_err(|e| e.to_string())?;
    let (sha, size) = hash_file(&out).map_err(|e| e.to_string())?;
    Ok((out, sha, size))
}

/// A package's tar archive: the file itself (old, uncompressed packages) or the
/// decompressed contents of a gzip-compressed one.
pub enum Archive {
    File(fs::File),
    Memory(io::Cursor<Vec<u8>>),
}

impl io::Read for Archive {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        match self {
            Archive::File(f) => f.read(b),
            Archive::Memory(c) => c.read(b),
        }
    }
}

impl io::Seek for Archive {
    fn seek(&mut self, p: io::SeekFrom) -> io::Result<u64> {
        match self {
            Archive::File(f) => f.seek(p),
            Archive::Memory(c) => c.seek(p),
        }
    }
}

/// Largest package contents pkg will unpack into memory.
const MAX_UNPACKED: usize = 8 << 30;

pub fn open_archive(fpk: &Path) -> io::Result<Archive> {
    let mut f = fs::File::open(fpk)?;
    let mut magic = [0u8; 3];
    let n = f.read(&mut magic)?;
    if n == 3 && flint_gzip::is_gzip(&magic) {
        let data = fs::read(fpk)?;
        let tar = flint_gzip::decompress(&data, MAX_UNPACKED).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        return Ok(Archive::Memory(io::Cursor::new(tar)));
    }
    f.seek(io::SeekFrom::Start(0))?;
    Ok(Archive::File(f))
}

/// The control data stored in a package (its first entry).
pub fn read_control(fpk: &Path) -> Result<Control, String> {
    let f = open_archive(fpk).map_err(|e| format!("{}: {e}", fpk.display()))?;
    let mut r = Reader::new(f);
    let e = r.next_entry().map_err(|e| format!("{}: {e}", fpk.display()))?.ok_or_else(|| format!("{}: empty archive", fpk.display()))?;
    if e.header.name != CONTROL_NAME || e.header.kind != Kind::File {
        return Err(format!("{}: not a flint package (first entry is not {CONTROL_NAME})", fpk.display()));
    }
    if e.header.size > 65536 {
        return Err(format!("{}: control data is too large", fpk.display()));
    }
    let mut text = String::new();
    r.data(&e).and_then(|mut d| d.read_to_string(&mut text)).map_err(|e| format!("{}: {e}", fpk.display()))?;
    Control::parse(&text).map_err(|e| format!("{}: {e}", fpk.display()))
}

/// Build the INDEX for every `*.fpk` in `dir`.
pub fn index(dir: &Path) -> Result<Vec<IndexEntry>, String> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "fpk") && !p.file_name().and_then(|n| n.to_str()).unwrap_or(".").starts_with('.'))
        .collect();
    files.sort();
    let mut entries = Vec::new();
    for f in files {
        let control = read_control(&f)?;
        let (sha256, size) = hash_file(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        let file = f.file_name().and_then(|n| n.to_str()).ok_or("bad file name")?.to_string();
        entries.push(IndexEntry { control, file, size, sha256 });
    }
    entries.sort_by(|a, b| a.control.name.cmp(&b.control.name).then_with(|| crate::version::compare(&a.control.version, &b.control.version)));
    Ok(entries)
}

pub fn write_index(dir: &Path) -> Result<usize, String> {
    let entries = index(dir)?;
    // Never go backwards, even if the clock is wrong: clients refuse an older INDEX.
    let old = fs::read_to_string(dir.join("INDEX")).map_or(0, |t| crate::remote::timestamp(&t));
    let ts = crate::remote::now().max(old + 1);
    let tmp = dir.join(".INDEX.tmp");
    fs::write(&tmp, format_index(&entries, ts)).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, dir.join("INDEX")).map_err(|e| e.to_string())?;
    Ok(entries.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn stage(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("flint-pkg-pack-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("files/usr/bin")).unwrap();
        fs::write(d.join("control"), "name: hello\nversion: 1.0\ndescription: test\nconfig: etc/hello.conf\n").unwrap();
        fs::write(d.join("files/usr/bin/hello"), "#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(d.join("files/usr/bin/hello"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir_all(d.join("files/etc")).unwrap();
        fs::write(d.join("files/etc/hello.conf"), "key=1\n").unwrap();
        symlink("hello", d.join("files/usr/bin/hi")).unwrap();
        symlink("../../usr/bin/hello", d.join("files/etc/relative-link")).unwrap();
        d
    }

    #[test]
    fn build_is_deterministic_and_readable() {
        let src = stage("a");
        let out = std::env::temp_dir().join(format!("flint-pkg-pack-out-{}", std::process::id()));
        let (p1, h1, size) = build(&src, &out).unwrap();
        let (_, h2, _) = build(&src, &out).unwrap();
        assert_eq!(h1, h2, "same input, same package");
        assert_eq!(p1.file_name().unwrap(), "hello-1.0.fpk");
        assert_eq!(fs::metadata(&p1).unwrap().len(), size);
        let c = read_control(&p1).unwrap();
        assert_eq!((c.name.as_str(), c.version.as_str()), ("hello", "1.0"));

        let n = write_index(&out).unwrap();
        assert_eq!(n, 1);
        let idx = crate::control::parse_index(&fs::read_to_string(out.join("INDEX")).unwrap()).unwrap();
        assert_eq!(idx[0].sha256, h1);
        assert_eq!(idx[0].file, "hello-1.0.fpk");
        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&out);
    }

    #[test]
    fn build_validates() {
        let src = stage("b");
        fs::write(src.join("control"), "name: hello\nversion: 1.0\nconfig: etc/missing.conf\n").unwrap();
        let out = std::env::temp_dir().join(format!("flint-pkg-pack-out2-{}", std::process::id()));
        assert!(build(&src, &out).unwrap_err().contains("missing.conf"));
        assert!(build(&src.join("nonexistent"), &out).is_err());
        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&out);
    }

    #[test]
    fn read_control_rejects_other_archives() {
        let d = std::env::temp_dir().join(format!("flint-pkg-pack-x-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("junk.fpk"), b"not a package").unwrap();
        assert!(read_control(&d.join("junk.fpk")).is_err());
        assert!(index(&d).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn link_targets() {
        assert!(valid_link_target("../lib/x"));
        assert!(valid_link_target("/usr/bin/x"));
        assert!(!valid_link_target(""));
        assert!(!valid_link_target("a\nb"));
        assert!(!valid_link_target(&"x".repeat(101)));
    }
}
