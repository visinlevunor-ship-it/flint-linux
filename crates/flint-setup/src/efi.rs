//! UEFI boot entries through efivarfs: add "Flint Linux" to the firmware's boot menu.

use std::fs;
use std::io::{self, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use flint_core::sys;

const EFIVARS: &str = "/sys/firmware/efi/efivars";
const GLOBAL: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";
/// NON_VOLATILE | BOOTSERVICE_ACCESS | RUNTIME_ACCESS
const ATTRS: u32 = 7;
const LOAD_OPTION_ACTIVE: u32 = 1;

pub fn is_uefi() -> bool {
    Path::new("/sys/firmware/efi").is_dir()
}

fn ensure_efivarfs() -> io::Result<()> {
    if fs::read_dir(EFIVARS).map(|mut d| d.next().is_some()).unwrap_or(false) {
        return Ok(());
    }
    sys::mount("efivarfs", EFIVARS, "efivarfs", libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC, None)
}

fn var_path(name: &str) -> PathBuf {
    Path::new(EFIVARS).join(format!("{name}-{GLOBAL}"))
}

fn read_var(name: &str) -> Option<Vec<u8>> {
    let d = fs::read(var_path(name)).ok()?;
    (d.len() >= 4).then(|| d[4..].to_vec())
}

/// efivarfs marks most variables immutable; writing needs the flag cleared first.
fn clear_immutable(path: &Path) {
    const FS_IOC_GETFLAGS: libc::c_ulong = 0x8008_6601;
    const FS_IOC_SETFLAGS: libc::c_ulong = 0x4008_6602;
    const FS_IMMUTABLE_FL: libc::c_long = 0x10;
    if let Ok(f) = fs::File::open(path) {
        let mut flags: libc::c_long = 0;
        // SAFETY: the flag ioctls on an open efivarfs file with a valid pointer.
        unsafe {
            if libc::ioctl(f.as_raw_fd(), FS_IOC_GETFLAGS as _, &mut flags) == 0 && flags & FS_IMMUTABLE_FL != 0 {
                flags &= !FS_IMMUTABLE_FL;
                libc::ioctl(f.as_raw_fd(), FS_IOC_SETFLAGS as _, &flags);
            }
        }
    }
}

fn write_var(name: &str, data: &[u8]) -> io::Result<()> {
    let p = var_path(name);
    if p.exists() {
        clear_immutable(&p);
    }
    let mut buf = ATTRS.to_le_bytes().to_vec();
    buf.extend_from_slice(data);
    // efivarfs needs the whole variable in a single write.
    let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).open(&p)?;
    f.write_all(&buf)
}

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16().chain(std::iter::once(0)).flat_map(|u| u.to_le_bytes()).collect()
}

/// EFI_LOAD_OPTION for `loader` (a path like \EFI\FLINT\BOOTX64.EFI) on a GPT partition.
pub fn load_option(description: &str, part_number: u32, start: u64, sectors: u64, part_guid: &[u8; 16], loader: &str) -> Vec<u8> {
    let mut path = Vec::new();
    // Hard drive media device path
    path.extend_from_slice(&[4, 1, 42, 0]);
    path.extend_from_slice(&part_number.to_le_bytes());
    path.extend_from_slice(&start.to_le_bytes());
    path.extend_from_slice(&sectors.to_le_bytes());
    path.extend_from_slice(part_guid);
    path.push(2); // GPT
    path.push(2); // GUID signature
    // File path media device path
    let file = utf16z(loader);
    path.extend_from_slice(&[4, 4]);
    path.extend_from_slice(&((4 + file.len()) as u16).to_le_bytes());
    path.extend_from_slice(&file);
    // End of device path
    path.extend_from_slice(&[0x7F, 0xFF, 4, 0]);

    let mut o = LOAD_OPTION_ACTIVE.to_le_bytes().to_vec();
    o.extend_from_slice(&(path.len() as u16).to_le_bytes());
    o.extend_from_slice(&utf16z(description));
    o.extend_from_slice(&path);
    o
}

fn description_of(option: &[u8]) -> String {
    let units: Vec<u16> = option.get(6..).unwrap_or(&[]).chunks(2).map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)])).take_while(|&u| u != 0).collect();
    String::from_utf16_lossy(&units)
}

/// Add (or replace) the boot entry called `description` and put it first in the boot
/// order (`first`) or last. Returns the entry number.
pub fn add_boot_entry(description: &str, option: &[u8], first: bool) -> io::Result<u16> {
    ensure_efivarfs()?;
    let mut used = Vec::new();
    let mut existing = None;
    for e in fs::read_dir(EFIVARS)?.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        let Some(num) = n.strip_prefix("Boot").and_then(|r| r.strip_suffix(&format!("-{GLOBAL}"))) else { continue };
        let Ok(v) = u16::from_str_radix(num, 16) else { continue };
        if num.len() != 4 {
            continue;
        }
        used.push(v);
        if read_var(&format!("Boot{num}")).map_or(false, |o| description_of(&o) == description) {
            existing = Some(v);
        }
    }
    let num = match existing {
        Some(n) => n,
        None => (0..=0xFFFFu16).find(|n| !used.contains(n)).ok_or_else(|| io::Error::new(io::ErrorKind::Other, "no free boot entry number"))?,
    };
    write_var(&format!("Boot{num:04X}"), option)?;
    let order: Vec<u16> = read_var("BootOrder").unwrap_or_default().chunks(2).filter(|c| c.len() == 2).map(|c| u16::from_le_bytes([c[0], c[1]])).filter(|&n| n != num).collect();
    let mut new_order = Vec::new();
    if first {
        new_order.push(num);
    }
    new_order.extend(order);
    if !first {
        new_order.push(num);
    }
    let bytes: Vec<u8> = new_order.iter().flat_map(|n| n.to_le_bytes()).collect();
    write_var("BootOrder", &bytes)?;
    Ok(num)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_layout() {
        let o = load_option("Flint Linux", 3, 2048, 4096, &[9; 16], "\\EFI\\FLINT\\BOOTX64.EFI");
        assert_eq!(&o[0..4], &1u32.to_le_bytes());
        assert_eq!(description_of(&o), "Flint Linux");
        let path_len = u16::from_le_bytes([o[4], o[5]]) as usize;
        let desc_len = ("Flint Linux".len() + 1) * 2;
        assert_eq!(o.len(), 6 + desc_len + path_len);
        let p = &o[6 + desc_len..];
        assert_eq!(&p[0..4], &[4, 1, 42, 0]);
        assert_eq!(&p[p.len() - 4..], &[0x7F, 0xFF, 4, 0]);
    }
}
