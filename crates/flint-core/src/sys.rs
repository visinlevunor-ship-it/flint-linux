//! Thin, safe wrappers over the few syscalls the Flint userland needs.

use std::ffi::CString;
use std::io;
use std::ptr;

use libc::{c_ulong, c_void};

fn cstring(s: &str) -> io::Result<CString> {
    CString::new(s).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "string contains a NUL byte"))
}

fn check(rc: libc::c_int) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn mount(source: &str, target: &str, fstype: &str, flags: c_ulong, data: Option<&str>) -> io::Result<()> {
    let source = cstring(source)?;
    let target = cstring(target)?;
    let fstype = cstring(fstype)?;
    let data = data.map(cstring).transpose()?;
    let data_ptr = data.as_ref().map_or(ptr::null(), |d| d.as_ptr().cast::<c_void>());
    // SAFETY: all pointers are valid NUL-terminated strings that outlive the call.
    check(unsafe { libc::mount(source.as_ptr(), target.as_ptr(), fstype.as_ptr(), flags, data_ptr) })
}

/// Change the flags of an existing mount (e.g. `libc::MS_RDONLY`).
pub fn remount(target: &str, flags: c_ulong) -> io::Result<()> {
    let target = cstring(target)?;
    // SAFETY: `target` is a valid NUL-terminated string; null pointers are allowed for remount.
    check(unsafe { libc::mount(ptr::null(), target.as_ptr(), ptr::null(), libc::MS_REMOUNT | flags, ptr::null()) })
}

pub fn umount(target: &str) -> io::Result<()> {
    let target = cstring(target)?;
    // SAFETY: `target` is a valid NUL-terminated string.
    check(unsafe { libc::umount2(target.as_ptr(), 0) })
}

pub fn sync() {
    // SAFETY: sync(2) has no preconditions.
    unsafe { libc::sync() };
}

pub fn set_hostname(name: &str) -> io::Result<()> {
    // SAFETY: pointer and length describe a valid byte slice.
    check(unsafe { libc::sethostname(name.as_ptr().cast(), name.len()) })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    pub device: String,
    pub target: String,
    pub fstype: String,
}

/// Parse the contents of /proc/self/mounts.
pub fn parse_mounts(text: &str) -> Vec<MountEntry> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            Some(MountEntry {
                device: unescape(f.next()?),
                target: unescape(f.next()?),
                fstype: unescape(f.next()?),
            })
        })
        .collect()
}

pub fn read_mounts() -> io::Result<Vec<MountEntry>> {
    Ok(parse_mounts(&std::fs::read_to_string("/proc/self/mounts")?))
}

/// True if `target` is currently a mount point. False if the table is unreadable
/// (e.g. /proc is not mounted yet).
pub fn is_mounted(target: &str) -> bool {
    read_mounts().map_or(false, |m| m.iter().any(|e| e.target == target))
}

/// Decode the `\040`-style octal escapes the kernel uses in /proc/self/mounts.
fn unescape(field: &str) -> String {
    let b = field.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v = u32::from(b[i + 1] - b'0') * 64 + u32::from(b[i + 2] - b'0') * 8 + u32::from(b[i + 3] - b'0');
            out.push(v as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_spaces_and_backslashes() {
        assert_eq!(unescape("/mnt/my\\040disk"), "/mnt/my disk");
        assert_eq!(unescape("a\\134b"), "a\\b");
        assert_eq!(unescape("plain"), "plain");
        assert_eq!(unescape("trailing\\04"), "trailing\\04");
    }

    #[test]
    fn unescapes_escape_at_end_of_field() {
        // exactly three octal digits at the very end must still decode
        assert_eq!(unescape("x\\040"), "x ");
    }

    #[test]
    fn parses_mount_table() {
        let t = "proc /proc proc rw,nosuid 0 0\n/dev/sda1 /mnt/my\\040disk ext4 rw 0 0\n\nbroken\n";
        let m = parse_mounts(t);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0], MountEntry { device: "proc".into(), target: "/proc".into(), fstype: "proc".into() });
        assert_eq!(m[1].target, "/mnt/my disk");
        assert_eq!(m[1].fstype, "ext4");
    }
}
