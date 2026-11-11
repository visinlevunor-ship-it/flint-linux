//! `mountboot`: mount the partition Flint booted from (the EFI system partition on the USB
//! stick or disk) at /boot, so settings such as saved Wi-Fi networks survive a reboot.
//!
//! flint-boot puts `flint.esp=PARTUUID` on the kernel command line. The partition is found
//! by reading each disk's GPT, because the kernel does not publish PARTUUIDs in sysfs. If
//! the command line has no `flint.esp` (another bootloader started Flint), a FAT partition
//! labelled FLINT is used instead.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::thread;
use std::time::{Duration, Instant};

use flint_core::sys;

pub const MOUNTPOINT: &str = "/boot";

/// `flint.esp=...` from a kernel command line.
pub fn esp_from_cmdline(cmdline: &str) -> Option<String> {
    cmdline.split_whitespace().find_map(|w| w.strip_prefix("flint.esp=")).map(|s| s.to_ascii_lowercase())
}

/// GPT GUID bytes as text (the first three fields are little-endian on disk).
pub fn guid_text(g: &[u8]) -> String {
    let mut s = String::with_capacity(36);
    for (i, &idx) in [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15].iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push_str(&format!("{:02x}", g[idx]));
    }
    s
}

/// The unique GUID of partition `number` (1-based) in the GPT of `disk`.
pub fn gpt_partition_guid<R: Read + Seek>(disk: &mut R, sector: u64, number: u32) -> io::Result<Option<String>> {
    let mut hdr = [0u8; 92];
    disk.seek(SeekFrom::Start(sector))?;
    disk.read_exact(&mut hdr)?;
    if &hdr[0..8] != b"EFI PART" {
        return Ok(None);
    }
    let entries_lba = u64::from_le_bytes(hdr[72..80].try_into().unwrap());
    let count = u32::from_le_bytes(hdr[80..84].try_into().unwrap());
    let size = u32::from_le_bytes(hdr[84..88].try_into().unwrap());
    if number == 0 || number > count || !(128..=4096).contains(&size) {
        return Ok(None);
    }
    let mut entry = vec![0u8; size as usize];
    disk.seek(SeekFrom::Start(entries_lba * sector + u64::from(number - 1) * u64::from(size)))?;
    disk.read_exact(&mut entry)?;
    if entry[0..16].iter().all(|&b| b == 0) {
        return Ok(None); // unused slot
    }
    Ok(Some(guid_text(&entry[16..32])))
}

/// The volume label of a FAT32 (or FAT16) file system, if `dev` holds one.
pub fn fat_label<R: Read + Seek>(dev: &mut R) -> Option<String> {
    let mut b = [0u8; 512];
    dev.seek(SeekFrom::Start(0)).ok()?;
    dev.read_exact(&mut b).ok()?;
    if b[510] != 0x55 || b[511] != 0xAA {
        return None;
    }
    let label = if &b[82..87] == b"FAT32" {
        &b[71..82]
    } else if &b[54..59] == b"FAT16" || &b[54..59] == b"FAT12" {
        &b[43..54]
    } else {
        return None;
    };
    Some(String::from_utf8_lossy(label).trim_end().to_string())
}

struct Partition {
    name: String,
    disk: String,
    number: u32,
}

fn partitions() -> Vec<Partition> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir("/sys/class/block") else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        let Ok(num) = fs::read_to_string(p.join("partition")) else { continue };
        let Ok(number) = num.trim().parse() else { continue };
        // /sys/class/block/sda1 -> ../../devices/.../block/sda/sda1: the parent is the disk.
        let Ok(real) = fs::canonicalize(&p) else { continue };
        let Some(disk) = real.parent().and_then(|d| d.file_name()).map(|d| d.to_string_lossy().into_owned()) else { continue };
        out.push(Partition { name, disk, number });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn sector_size(disk: &str) -> u64 {
    fs::read_to_string(format!("/sys/class/block/{disk}/queue/logical_block_size")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(512)
}

fn find(esp: Option<&str>) -> Option<String> {
    for p in partitions() {
        match esp {
            Some(want) => {
                let Ok(mut d) = fs::File::open(format!("/dev/{}", p.disk)) else { continue };
                if let Ok(Some(g)) = gpt_partition_guid(&mut d, sector_size(&p.disk), p.number) {
                    if g == want {
                        return Some(p.name);
                    }
                }
            }
            None => {
                let Ok(mut d) = fs::File::open(format!("/dev/{}", p.name)) else { continue };
                if fat_label(&mut d).as_deref() == Some("FLINT") {
                    return Some(p.name);
                }
            }
        }
    }
    None
}

pub fn mountboot(args: Vec<String>) -> i32 {
    let mut wait = 15u64;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--wait" => match it.next().and_then(|s| s.parse().ok()) {
                Some(w) => wait = w,
                None => {
                    eprintln!("usage: mountboot [--wait SECONDS]");
                    return 2;
                }
            },
            _ => {
                eprintln!("usage: mountboot [--wait SECONDS]\nMounts the partition Flint booted from at {MOUNTPOINT}.");
                return 2;
            }
        }
    }
    if sys::is_mounted(MOUNTPOINT) {
        return 0;
    }
    let cmdline = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let esp = esp_from_cmdline(&cmdline);
    // A USB stick can show up a few seconds after its driver loads.
    let deadline = Instant::now() + Duration::from_secs(wait);
    let dev = loop {
        if let Some(d) = find(esp.as_deref()) {
            break d;
        }
        if Instant::now() >= deadline {
            match &esp {
                Some(g) => eprintln!("mountboot: the boot partition {g} did not show up; settings will not be saved"),
                None => eprintln!("mountboot: no partition labelled FLINT found; settings will not be saved"),
            }
            return 1;
        }
        thread::sleep(Duration::from_millis(250));
    };
    let _ = fs::create_dir_all(MOUNTPOINT);
    // The default FAT character set lives in a module; load it so the mount does not fail.
    if let Ok(idx) = crate::index::Index::running() {
        let _ = crate::loader::Loader::new(&idx).load("nls_iso8859_1", "");
    }
    let src = format!("/dev/{dev}");
    let opts = "umask=0077,shortname=mixed,flush";
    match sys::mount(&src, MOUNTPOINT, "vfat", libc::MS_NOATIME | libc::MS_NOSUID | libc::MS_NODEV, Some(opts)) {
        Ok(()) => {
            eprintln!("mountboot: {src} mounted on {MOUNTPOINT}");
            0
        }
        Err(e) => {
            eprintln!("mountboot: cannot mount {src} on {MOUNTPOINT}: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn cmdline() {
        assert_eq!(esp_from_cmdline("quiet flint.esp=9E2144AD-d294-4ca7-b35d-40fb50403f76 x=1").as_deref(), Some("9e2144ad-d294-4ca7-b35d-40fb50403f76"));
        assert_eq!(esp_from_cmdline("quiet"), None);
    }

    #[test]
    fn gpt_and_label() {
        // A tiny disk: header at sector 1, entries at sector 2, entry 1 in use.
        let mut d = vec![0u8; 512 * 40];
        d[512..520].copy_from_slice(b"EFI PART");
        d[512 + 72..512 + 80].copy_from_slice(&2u64.to_le_bytes());
        d[512 + 80..512 + 84].copy_from_slice(&128u32.to_le_bytes());
        d[512 + 84..512 + 88].copy_from_slice(&128u32.to_le_bytes());
        let e = 1024;
        d[e] = 0x28; // type GUID, not zero
        let uniq = [0xAD, 0x44, 0x21, 0x9E, 0x94, 0xD2, 0xA7, 0x4C, 0xB3, 0x5D, 0x40, 0xFB, 0x50, 0x40, 0x3F, 0x76];
        d[e + 16..e + 32].copy_from_slice(&uniq);
        let mut c = Cursor::new(d);
        assert_eq!(gpt_partition_guid(&mut c, 512, 1).unwrap().as_deref(), Some("9e2144ad-d294-4ca7-b35d-40fb50403f76"));
        assert_eq!(gpt_partition_guid(&mut c, 512, 2).unwrap(), None, "empty slot");
        assert_eq!(gpt_partition_guid(&mut c, 512, 0).unwrap(), None);
        assert_eq!(gpt_partition_guid(&mut Cursor::new(vec![0u8; 2048]), 512, 1).unwrap(), None, "no GPT");

        let mut fat = vec![0u8; 512];
        fat[71..82].copy_from_slice(b"FLINT      ");
        fat[82..90].copy_from_slice(b"FAT32   ");
        fat[510] = 0x55;
        fat[511] = 0xAA;
        assert_eq!(fat_label(&mut Cursor::new(fat)).as_deref(), Some("FLINT"));
        assert_eq!(fat_label(&mut Cursor::new(vec![0u8; 512])), None);
    }
}
