//! Keeping changes across reboots.
//!
//! Flint's base system is the initramfs in RAM, fresh on every boot. When the boot disk has
//! a data partition (GPT name "Flint data", ext4), init puts an overlay over the base
//! system with that partition as the writable layer and switches into it: installed
//! packages, files in /home, changed settings all stay. The base itself can still be
//! replaced as a whole (flint-update) without touching what is on the data partition.
//!
//! On a USB stick written from the disk image there is no data partition at first: the
//! first boot creates one in the free space after the boot partition and formats it. That
//! only happens on removable or USB disks; an internal disk gets one from the installer.
//! `flint.persist=0` on the kernel command line boots without any of this (nothing saved).

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use flint_core::sys;
use flint_disk::{ext4, gpt};

pub const DATA_NAME: &str = "Flint data";
pub const DATA_LABEL: &str = "FLINTDATA";
/// Where the data partition and the pristine base stay visible after the switch.
pub const DATA_DIR: &str = "/run/flint/data";
pub const LOWER_DIR: &str = "/run/flint/lower";
/// The smallest free space worth turning into a data partition.
const MIN_DATA: u64 = 1 << 30;

const STAGE: &str = "/.flint";

/// What the boot disk is and where things are on it.
pub struct BootDisk {
    /// e.g. "sda", "nvme0n1"
    pub disk: String,
    pub sector: u64,
    pub total: u64,
}

/// The partition device name for partition `number` of `disk` (sda1, nvme0n1p2, mmcblk0p1).
pub fn part_name(disk: &str, number: u32) -> String {
    if disk.ends_with(|c: char| c.is_ascii_digit()) {
        format!("{disk}p{number}")
    } else {
        format!("{disk}{number}")
    }
}

/// The disk holding block device `dev` (a partition name like sda1), from sysfs.
pub fn disk_of(dev: &str) -> Option<String> {
    let real = fs::canonicalize(format!("/sys/class/block/{dev}")).ok()?;
    if real.join("partition").exists() {
        real.parent()?.file_name().map(|n| n.to_string_lossy().into_owned())
    } else {
        Some(dev.to_string())
    }
}

pub fn boot_disk(disk: &str) -> io::Result<BootDisk> {
    let sector: u64 = fs::read_to_string(format!("/sys/class/block/{disk}/queue/logical_block_size")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(512);
    let size512: u64 = fs::read_to_string(format!("/sys/class/block/{disk}/size")).ok().and_then(|s| s.trim().parse().ok()).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{disk}: no size in sysfs")))?;
    Ok(BootDisk { disk: disk.to_string(), sector, total: size512 * 512 / sector })
}

/// True for USB sticks, SD cards and other disks meant to be carried around.
pub fn removable(disk: &str) -> bool {
    let flag = fs::read_to_string(format!("/sys/block/{disk}/removable")).map_or(false, |s| s.trim() == "1");
    let usb = fs::canonicalize(format!("/sys/block/{disk}")).map_or(false, |p| p.to_string_lossy().contains("/usb"));
    flag || usb
}

pub fn read_table(d: &BootDisk) -> io::Result<gpt::Table> {
    let mut f = fs::File::open(format!("/dev/{}", d.disk))?;
    gpt::Table::read(&mut f, d.sector, d.total)
}

pub fn find_data(t: &gpt::Table) -> Option<&gpt::Partition> {
    t.partitions.iter().find(|p| p.type_guid == gpt::LINUX && p.name == DATA_NAME)
}

#[repr(C)]
struct BlkpgPartition {
    start: i64,
    length: i64,
    pno: i32,
    devname: [u8; 64],
    volname: [u8; 64],
}

#[repr(C)]
struct BlkpgIoctlArg {
    op: i32,
    flags: i32,
    datalen: i32,
    data: *mut libc::c_void,
}

const BLKPG: libc::c_ulong = 0x1269;
const BLKPG_ADD_PARTITION: i32 = 1;

/// Tell the kernel about a partition just added to the table (the disk's other partitions
/// may be mounted, so re-reading the whole table is not possible).
pub fn announce_partition(disk: &str, sector: u64, p: &gpt::Partition) -> io::Result<()> {
    let f = fs::OpenOptions::new().read(true).open(format!("/dev/{disk}"))?;
    let mut part = BlkpgPartition { start: (p.first * sector) as i64, length: (p.sectors() * sector) as i64, pno: p.number as i32, devname: [0; 64], volname: [0; 64] };
    let mut arg = BlkpgIoctlArg { op: BLKPG_ADD_PARTITION, flags: 0, datalen: std::mem::size_of::<BlkpgPartition>() as i32, data: (&mut part as *mut BlkpgPartition).cast() };
    use std::os::unix::io::AsRawFd;
    // SAFETY: BLKPG with a correctly laid out argument that lives across the call.
    let rc = unsafe { libc::ioctl(f.as_raw_fd(), BLKPG as _, &mut arg) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn wait_for(path: &str, timeout: Duration) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if Path::new(path).exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    Path::new(path).exists()
}

/// Format a partition device as Flint's data filesystem.
pub fn format_data(dev: &str) -> io::Result<()> {
    let mut f = fs::OpenOptions::new().write(true).custom_flags(libc::O_EXCL).open(dev)?;
    let size = {
        use std::io::Seek;
        f.seek(io::SeekFrom::End(0))?
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32);
    ext4::mkfs(&mut f, 0, size, &ext4::Options { label: DATA_LABEL.into(), uuid: None, time: now })?;
    f.sync_all()
}

/// Add a data partition in the largest free space of the disk and format it.
fn create_data(d: &BootDisk, t: &mut gpt::Table) -> Result<String, String> {
    let (first, last) = t.free().into_iter().max_by_key(|(f, l)| l - f).ok_or("no free space on the boot disk")?;
    let bytes = (last - first + 1) * d.sector;
    if bytes < MIN_DATA {
        return Err(format!("only {} MiB free on the boot disk", bytes >> 20));
    }
    eprintln!("flint: setting up {} GiB of the boot disk to keep your files and settings (this happens once)", bytes >> 30);
    let guid = gpt::random_guid().map_err(|e| e.to_string())?;
    let number = t.add(gpt::LINUX, guid, first, last, DATA_NAME).map_err(|e| e.to_string())?;
    {
        let mut f = fs::OpenOptions::new().read(true).write(true).open(format!("/dev/{}", d.disk)).map_err(|e| format!("/dev/{}: {e}", d.disk))?;
        t.write(&mut f, false).map_err(|e| format!("writing the partition table: {e}"))?;
        gpt::fix_protective_mbr(&mut f, d.total).map_err(|e| format!("writing the partition table: {e}"))?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    let p = t.partitions.iter().find(|p| p.number == number).cloned().ok_or("partition vanished")?;
    let name = part_name(&d.disk, number);
    if !Path::new(&format!("/dev/{name}")).exists() {
        announce_partition(&d.disk, d.sector, &p).map_err(|e| format!("cannot tell the kernel about the new partition: {e}"))?;
    }
    if !wait_for(&format!("/dev/{name}"), Duration::from_secs(5)) {
        return Err(format!("/dev/{name} did not appear"));
    }
    format_data(&format!("/dev/{name}")).map_err(|e| format!("formatting /dev/{name}: {e}"))?;
    Ok(name)
}

fn load_module(name: &str) {
    if let Ok(idx) = crate::index::Index::running() {
        let _ = crate::loader::Loader::new(&idx).load(name, "");
    }
}

fn mkdir(p: &str) -> Result<(), String> {
    fs::create_dir_all(p).map_err(|e| format!("{p}: {e}"))
}

fn mount(src: &str, dst: &str, fstype: &str, flags: libc::c_ulong, data: Option<&str>) -> Result<(), String> {
    sys::mount(src, dst, fstype, flags, data).map_err(|e| format!("mount {src} on {dst}: {e}"))
}

fn move_mount(from: &str, to: &str) -> Result<(), String> {
    mkdir(to)?;
    mount(from, to, "", libc::MS_MOVE, None)
}

/// The device the boot partition (/boot) is mounted from, as a name like "sda1".
fn boot_partition() -> Option<String> {
    sys::read_mounts().ok()?.into_iter().find(|m| m.target == "/boot").and_then(|m| m.device.strip_prefix("/dev/").map(str::to_string))
}

/// Switch to the persistent root. `Ok(None)`: nothing to do (live session).
pub fn setup() -> Result<Option<String>, String> {
    let cmdline = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    if cmdline.split_whitespace().any(|w| w == "flint.persist=0") {
        return Ok(None);
    }
    if sys::is_mounted(DATA_DIR) {
        return Ok(None); // already done
    }
    let Some(boot) = boot_partition() else {
        return Err("the boot partition is not mounted, so nothing will be saved".into());
    };
    let disk = disk_of(&boot).ok_or_else(|| format!("cannot find the disk of {boot}"))?;
    let d = boot_disk(&disk).map_err(|e| e.to_string())?;
    let mut t = read_table(&d).map_err(|e| format!("/dev/{disk}: {e}"))?;
    let dev = match find_data(&t) {
        Some(p) => {
            let name = part_name(&disk, p.number);
            if !wait_for(&format!("/dev/{name}"), Duration::from_secs(3)) {
                return Err(format!("/dev/{name} (the data partition) is missing"));
            }
            name
        }
        None if removable(&disk) => create_data(&d, &mut t)?,
        None => return Ok(None),
    };

    load_module("ext4");
    load_module("overlay");
    let data = format!("{STAGE}/data");
    let lower = format!("{STAGE}/lower");
    let root = format!("{STAGE}/root");
    for p in [&data, &lower, &root] {
        mkdir(p)?;
    }
    mount(&format!("/dev/{dev}"), &data, "ext4", libc::MS_NOATIME, None)?;
    let undo = |e: String| {
        let _ = sys::umount(&data);
        e
    };
    let (upper, work) = (format!("{data}/root/upper"), format!("{data}/root/work"));
    mkdir(&upper).map_err(undo)?;
    mkdir(&work).map_err(undo)?;
    mount("/", &lower, "", libc::MS_BIND, None).map_err(undo)?;
    let opts = format!("lowerdir={lower},upperdir={upper},workdir={work}");
    if let Err(e) = mount("overlay", &root, "overlay", 0, Some(&opts)) {
        let _ = sys::umount(&lower);
        return Err(undo(e));
    }

    // Everything mounted at the top level moves into the new root (with what is below it).
    let mut tops: Vec<String> = sys::read_mounts()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|m| m.target)
        .filter(|t| t != "/" && !t.starts_with(STAGE) && t.matches('/').count() == 1)
        .collect();
    tops.dedup();
    for t in &tops {
        move_mount(t, &format!("{root}{t}"))?;
    }
    move_mount(&data, &format!("{root}{DATA_DIR}"))?;
    move_mount(&lower, &format!("{root}{LOWER_DIR}"))?;

    let c = CString::new(root.clone()).map_err(|e| e.to_string())?;
    // SAFETY: chdir/chroot with a valid path; the move makes the overlay the root mount.
    unsafe {
        if libc::chdir(c.as_ptr()) != 0 {
            return Err(format!("chdir {root}: {}", io::Error::last_os_error()));
        }
    }
    mount(".", "/", "", libc::MS_MOVE, None)?;
    // SAFETY: as above.
    unsafe {
        if libc::chroot(b".\0".as_ptr().cast()) != 0 {
            return Err(format!("chroot: {}", io::Error::last_os_error()));
        }
        libc::chdir(b"/\0".as_ptr().cast());
    }
    Ok(Some(format!("/dev/{dev}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(part_name("sda", 2), "sda2");
        assert_eq!(part_name("nvme0n1", 2), "nvme0n1p2");
        assert_eq!(part_name("mmcblk0", 1), "mmcblk0p1");
    }
}
