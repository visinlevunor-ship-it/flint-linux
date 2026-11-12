//! Copy a directory tree exactly: owners, modes, times, symlinks, device nodes (overlay
//! whiteouts are 0:0 character devices), extended attributes (overlay keeps its opaque
//! markers there) and hard links.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::Path;

fn cstr(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))
}

fn check(rc: libc::c_int) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn copy_xattrs(from: &Path, to: &Path) -> io::Result<()> {
    let (f, t) = (cstr(from)?, cstr(to)?);
    let mut names = vec![0u8; 4096];
    // SAFETY: buffers are valid for the sizes passed.
    let n = unsafe { libc::llistxattr(f.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
    if n <= 0 {
        return Ok(()); // none, or not supported
    }
    for name in names[..n as usize].split(|&b| b == 0).filter(|s| !s.is_empty()) {
        let cname = CString::new(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad xattr name"))?;
        let mut val = vec![0u8; 65536];
        // SAFETY: as above.
        let len = unsafe { libc::lgetxattr(f.as_ptr(), cname.as_ptr(), val.as_mut_ptr().cast(), val.len()) };
        if len < 0 {
            continue;
        }
        // SAFETY: as above.
        let rc = unsafe { libc::lsetxattr(t.as_ptr(), cname.as_ptr(), val.as_ptr().cast(), len as usize, 0) };
        if rc != 0 {
            let e = io::Error::last_os_error();
            // security.* may be refused (no LSM); everything else matters
            if !name.starts_with(b"security.") {
                return Err(io::Error::new(e.kind(), format!("{}: xattr {}: {e}", to.display(), String::from_utf8_lossy(name))));
            }
        }
    }
    Ok(())
}

fn set_meta(to: &Path, m: &fs::Metadata, link: bool) -> io::Result<()> {
    let t = cstr(to)?;
    // SAFETY: valid path; lchown does not follow symlinks.
    check(unsafe { libc::lchown(t.as_ptr(), m.uid(), m.gid()) })?;
    if !link {
        fs::set_permissions(to, fs::Permissions::from_mode(m.mode() & 0o7777))?;
    }
    let times = [
        libc::timespec { tv_sec: m.atime() as _, tv_nsec: m.atime_nsec() as _ },
        libc::timespec { tv_sec: m.mtime() as _, tv_nsec: m.mtime_nsec() as _ },
    ];
    // SAFETY: valid path and two timespecs.
    check(unsafe { libc::utimensat(libc::AT_FDCWD, t.as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) })
}

pub struct Copied {
    pub files: u64,
    pub bytes: u64,
}

/// Copy everything inside `from` into the existing directory `to`.
pub fn copy_tree(from: &Path, to: &Path) -> io::Result<Copied> {
    let mut stats = Copied { files: 0, bytes: 0 };
    let mut links: HashMap<(u64, u64), std::path::PathBuf> = HashMap::new();
    copy_dir(from, to, &mut stats, &mut links)?;
    let m = fs::symlink_metadata(from)?;
    copy_xattrs(from, to)?;
    set_meta(to, &m, false)?;
    Ok(stats)
}

fn copy_dir(from: &Path, to: &Path, stats: &mut Copied, links: &mut HashMap<(u64, u64), std::path::PathBuf>) -> io::Result<()> {
    for e in fs::read_dir(from)? {
        let e = e?;
        let src = e.path();
        let dst = to.join(e.file_name());
        let m = fs::symlink_metadata(&src)?;
        let ft = m.file_type();
        let wrap = |r: io::Result<()>| r.map_err(|err| io::Error::new(err.kind(), format!("{}: {err}", src.display())));
        if ft.is_dir() {
            if !dst.is_dir() {
                wrap(fs::create_dir(&dst))?;
            }
            copy_dir(&src, &dst, stats, links)?;
            wrap(copy_xattrs(&src, &dst))?;
            wrap(set_meta(&dst, &m, false))?;
            continue;
        }
        let _ = fs::remove_file(&dst);
        if m.nlink() > 1 && !ft.is_dir() {
            if let Some(first) = links.get(&(m.dev(), m.ino())) {
                wrap(fs::hard_link(first, &dst))?;
                continue;
            }
            links.insert((m.dev(), m.ino()), dst.clone());
        }
        if ft.is_symlink() {
            wrap(symlink(fs::read_link(&src)?, &dst))?;
            wrap(set_meta(&dst, &m, true))?;
        } else if ft.is_file() {
            wrap(fs::copy(&src, &dst).map(|n| stats.bytes += n))?;
            wrap(copy_xattrs(&src, &dst))?;
            wrap(set_meta(&dst, &m, false))?;
        } else if ft.is_char_device() || ft.is_block_device() || ft.is_fifo() || ft.is_socket() {
            let c = cstr(&dst)?;
            // SAFETY: valid path; mode and dev come from the source.
            wrap(check(unsafe { libc::mknod(c.as_ptr(), m.mode() as libc::mode_t, m.rdev() as libc::dev_t) }))?;
            wrap(copy_xattrs(&src, &dst))?;
            wrap(set_meta(&dst, &m, false))?;
        }
        stats.files += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_links_modes_and_symlinks() {
        let base = std::env::temp_dir().join(format!("flint-copy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (a, b) = (base.join("a"), base.join("b"));
        fs::create_dir_all(a.join("sub")).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("sub/f"), "data").unwrap();
        fs::set_permissions(a.join("sub/f"), fs::Permissions::from_mode(0o751)).unwrap();
        fs::hard_link(a.join("sub/f"), a.join("hl")).unwrap();
        symlink("sub/f", a.join("ln")).unwrap();
        let s = copy_tree(&a, &b).unwrap();
        assert_eq!(s.files, 3);
        assert_eq!(fs::read_to_string(b.join("sub/f")).unwrap(), "data");
        assert_eq!(fs::metadata(b.join("sub/f")).unwrap().mode() & 0o777, 0o751);
        assert_eq!(fs::read_link(b.join("ln")).unwrap(), Path::new("sub/f"));
        assert_eq!(fs::metadata(b.join("hl")).unwrap().ino(), fs::metadata(b.join("sub/f")).unwrap().ino());
        let _ = fs::remove_dir_all(&base);
    }
}
