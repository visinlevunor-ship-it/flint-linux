//! `flint-install`: put Flint on a disk, next to other systems or on its own.
//!
//! What it makes is the same layout as a Flint USB stick: a boot partition (FAT32, the
//! bootloader, kernel and base system copied from the one Flint is running from) and a
//! data partition (ext4) for everything that changes. Either in the free space of a disk,
//! leaving every existing partition alone, or on a whole disk, erasing it. Then it adds
//! "Flint Linux" to the firmware's boot menu.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::time::Duration;

use flint_dev::persist::{self, part_name, BootDisk, DATA_NAME};
use flint_disk::{fat, gpt};

use crate::copy::copy_tree;
use crate::efi;

pub const BOOT_NAME: &str = "Flint boot";
const LOADER: &str = "\\EFI\\FLINT\\BOOTX64.EFI";
const GIB: u64 = 1 << 30;
const MIN_TOTAL: u64 = 4 * GIB;

struct Disk {
    name: String,
    bytes: u64,
    model: String,
    removable: bool,
}

fn disks() -> Vec<Disk> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir("/sys/block") else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if ["loop", "ram", "zram", "sr", "dm-", "md", "fd", "nbd"].iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let bytes = fs::read_to_string(format!("/sys/block/{name}/size")).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0) * 512;
        if bytes == 0 {
            continue;
        }
        let model = fs::read_to_string(format!("/sys/block/{name}/device/model")).map(|s| s.trim().to_string()).unwrap_or_default();
        out.push(Disk { removable: persist::removable(&name), name, bytes, model });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB as f64)
}

fn type_name(t: &gpt::Guid) -> &'static str {
    const KNOWN: &[(&str, &str)] = &[
        ("c12a7328-f81f-11d2-ba4b-00a0c93ec93b", "EFI system"),
        ("0fc63daf-8483-4772-8e79-3d69d8477de4", "Linux"),
        ("0657fd6d-a4ab-43c4-84e5-0933c84b4f4f", "Linux swap"),
        ("516e7cb6-6ecf-11d6-8ff8-00022d09712b", "FreeBSD UFS"),
        ("516e7cba-6ecf-11d6-8ff8-00022d09712b", "FreeBSD ZFS"),
        ("516e7cb5-6ecf-11d6-8ff8-00022d09712b", "FreeBSD swap"),
        ("83bd6b9d-7f41-11dc-be0b-001560b84f0f", "FreeBSD boot"),
        ("ebd0a0a2-b9e5-4433-87c0-68b6b72699c7", "Windows data"),
        ("e3c9e316-0b5c-4db8-817d-f92df00215ae", "Windows reserved"),
        ("de94bba4-06d1-4d40-a16a-bfd50179d6ac", "Windows recovery"),
    ];
    let s = gpt::guid_string(t);
    KNOWN.iter().find(|(g, _)| *g == s).map_or("other", |(_, n)| n)
}

fn ask(prompt: &str) -> Result<String, String> {
    print!("{prompt}");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())? == 0 {
        return Err("no answer (end of input), nothing changed".into());
    }
    Ok(line.trim().to_string())
}

fn yes(prompt: &str, default: bool) -> Result<bool, String> {
    let a = ask(&format!("{prompt} [{}] ", if default { "Y/n" } else { "y/N" }))?;
    Ok(match a.to_ascii_lowercase().as_str() {
        "" => default,
        "y" | "yes" => true,
        _ => false,
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Free,
    Whole,
}

struct Opts {
    disk: Option<String>,
    mode: Option<Mode>,
    copy: Option<bool>,
    first: Option<bool>,
    yes: bool,
}

const USAGE: &str = "usage: flint-install [--disk NAME] [--free-space | --whole-disk] [--copy | --no-copy]
                    [--default | --not-default] [--yes]

Installs Flint on a disk: a boot partition and a data partition, either in free space
(other partitions are not touched) or on the whole disk (it is erased). Without options it
asks. --copy also copies what you changed on this system (packages, files, settings).
--default makes Flint the first entry in the firmware boot menu.";

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts { disk: None, mode: None, copy: None, first: None, yes: false };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--disk" => o.disk = Some(it.next().ok_or("--disk needs a name, like nvme0n1")?.trim_start_matches("/dev/").to_string()),
            "--free-space" => o.mode = Some(Mode::Free),
            "--whole-disk" => o.mode = Some(Mode::Whole),
            "--copy" => o.copy = Some(true),
            "--no-copy" => o.copy = Some(false),
            "--default" => o.first = Some(true),
            "--not-default" => o.first = Some(false),
            "--yes" => o.yes = true,
            "-h" | "--help" => return Err(USAGE.into()),
            _ => return Err(format!("unknown option {a}\n{USAGE}")),
        }
    }
    Ok(o)
}

fn mounted_devices() -> Vec<String> {
    flint_core::sys::read_mounts().map(|m| m.into_iter().filter_map(|e| e.device.strip_prefix("/dev/").map(str::to_string)).collect()).unwrap_or_default()
}

fn table_of(d: &BootDisk) -> Option<gpt::Table> {
    persist::read_table(d).ok()
}

/// Re-read a whole disk's partitions (only works when none of them is in use).
fn reread(disk: &str) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    const BLKRRPART: libc::c_ulong = 0x125F;
    let f = fs::File::open(format!("/dev/{disk}"))?;
    // SAFETY: BLKRRPART takes no argument.
    let rc = unsafe { libc::ioctl(f.as_raw_fd(), BLKRRPART as _, 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn main(args: Vec<String>) -> i32 {
    match run(&args[1..]) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("flint-install: {e}");
            1
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let o = parse(args)?;
    // SAFETY: plain query.
    if unsafe { libc::geteuid() } != 0 {
        return Err("run it as root".into());
    }
    if !efi::is_uefi() {
        return Err("this computer did not start in UEFI mode; Flint only boots with UEFI".into());
    }
    if !flint_core::sys::is_mounted("/boot") {
        return Err("the Flint boot partition is not mounted at /boot, so there is nothing to copy".into());
    }
    let boot_dev = flint_core::sys::read_mounts().ok().and_then(|m| m.into_iter().find(|e| e.target == "/boot")).map(|e| e.device).unwrap_or_default();
    let running_disk = boot_dev.strip_prefix("/dev/").and_then(persist::disk_of).unwrap_or_default();

    let all = disks();
    let candidates: Vec<&Disk> = all.iter().filter(|d| d.name != running_disk && d.bytes >= MIN_TOTAL).collect();
    if candidates.is_empty() {
        return Err("no disk to install on (the one Flint is running from does not count)".into());
    }
    let disk = match &o.disk {
        Some(n) => candidates.iter().find(|d| &d.name == n).copied().ok_or_else(|| format!("{n}: no such disk, or it is the one Flint runs from, or smaller than 4 GiB"))?,
        None => {
            println!("Disks:");
            for (i, d) in candidates.iter().enumerate() {
                println!("  {}) {:<10} {:>10}  {}{}", i + 1, d.name, gib(d.bytes), d.model, if d.removable { "  (removable)" } else { "" });
                let bd = persist::boot_disk(&d.name).map_err(|e| e.to_string())?;
                match table_of(&bd) {
                    Some(t) => {
                        for p in &t.partitions {
                            println!("       partition {}: {:>10}  {}  {}", p.number, gib(p.sectors() * bd.sector), type_name(&p.type_guid), p.name);
                        }
                        let free: u64 = t.free().iter().map(|(f, l)| (l - f + 1) * bd.sector).sum();
                        println!("       free: {}", gib(free));
                    }
                    None => println!("       no GPT partition table"),
                }
            }
            let a = ask("Install on which disk (number)? ")?;
            let i: usize = a.parse().map_err(|_| "not a number".to_string())?;
            *candidates.get(i.wrapping_sub(1)).ok_or("no such disk")?
        }
    };
    let bd = persist::boot_disk(&disk.name).map_err(|e| e.to_string())?;
    if bd.sector != 512 {
        return Err(format!("{}: {}-byte sectors are not supported yet", disk.name, bd.sector));
    }
    let table = table_of(&bd);
    let largest_free = table.as_ref().and_then(|t| t.free().into_iter().max_by_key(|(f, l)| l - f));
    let free_ok = largest_free.map_or(false, |(f, l)| (l - f + 1) * bd.sector >= MIN_TOTAL);

    let mode = match o.mode {
        Some(m) => m,
        None => {
            println!("\nHow should Flint go on {} ({})?", disk.name, gib(disk.bytes));
            if free_ok {
                let (f, l) = largest_free.unwrap();
                println!("  1) in the free space ({}), keeping every partition that is there now", gib((l - f + 1) * bd.sector));
            } else {
                println!("  1) (not possible: no free space of 4 GiB or more)");
            }
            println!("  2) on the whole disk, ERASING everything on it");
            match ask("Choose 1 or 2: ")?.as_str() {
                "1" if free_ok => Mode::Free,
                "2" => Mode::Whole,
                _ => return Err("nothing chosen, nothing changed".into()),
            }
        }
    };
    if mode == Mode::Free && !free_ok {
        return Err(format!("{} has no free space of 4 GiB or more; shrink a partition first, or use the whole disk", disk.name));
    }
    let busy: Vec<String> = mounted_devices().into_iter().filter(|d| d.starts_with(&disk.name)).collect();
    if mode == Mode::Whole && !busy.is_empty() {
        return Err(format!("{} is in use ({} mounted); unmount it first", disk.name, busy.join(", ")));
    }
    if mode == Mode::Whole && !o.yes {
        println!("\nEverything on {} ({} {}) will be erased.", disk.name, gib(disk.bytes), disk.model);
        if ask(&format!("Type the disk name ({}) to go ahead: ", disk.name))? != disk.name {
            return Err("not confirmed, nothing changed".into());
        }
    }
    let copy_changes = Path::new(persist::DATA_DIR).join("root/upper").is_dir()
        && match o.copy {
            Some(c) => c,
            None => yes("Copy what you changed on this system (packages, files, settings)?", true)?,
        };
    let first = match o.first {
        Some(f) => f,
        None => yes("Start Flint by default when the computer starts? (otherwise pick it from the boot menu)", false)?,
    };
    if mode == Mode::Free && !o.yes && !yes(&format!("Create Flint's partitions in the free space of {}?", disk.name), true)? {
        return Err("nothing changed".into());
    }

    // Partition.
    let mut t = match mode {
        Mode::Free => table.clone().ok_or("no partition table")?,
        Mode::Whole => gpt::Table::new(bd.sector, bd.total, gpt::random_guid().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?,
    };
    let (first_s, last_s) = match mode {
        Mode::Free => largest_free.unwrap(),
        Mode::Whole => *t.free().first().ok_or("disk too small")?,
    };
    let span = last_s - first_s + 1;
    let boot_sectors = if span * bd.sector >= 16 * GIB { GIB / bd.sector } else { GIB / 2 / bd.sector };
    let boot_guid = gpt::random_guid().map_err(|e| e.to_string())?;
    let data_guid = gpt::random_guid().map_err(|e| e.to_string())?;
    let boot_no = t.add(gpt::ESP, boot_guid, first_s, first_s + boot_sectors - 1, BOOT_NAME).map_err(|e| e.to_string())?;
    let data_no = t.add(gpt::LINUX, data_guid, first_s + boot_sectors, last_s, DATA_NAME).map_err(|e| e.to_string())?;
    println!("writing the partition table");
    {
        let mut f = fs::OpenOptions::new().read(true).write(true).open(format!("/dev/{}", disk.name)).map_err(|e| e.to_string())?;
        if mode == Mode::Whole {
            // Old signatures at the start would confuse other tools.
            use std::io::Seek;
            f.seek(io::SeekFrom::Start(0)).map_err(|e| e.to_string())?;
            f.write_all(&vec![0u8; 1 << 20]).map_err(|e| e.to_string())?;
        }
        t.write(&mut f, mode == Mode::Whole).map_err(|e| e.to_string())?;
        gpt::fix_protective_mbr(&mut f, bd.total).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    let boot_p = t.partitions.iter().find(|p| p.number == boot_no).cloned().ok_or("lost the boot partition")?;
    let data_p = t.partitions.iter().find(|p| p.number == data_no).cloned().ok_or("lost the data partition")?;
    let (boot_name, data_name) = (part_name(&disk.name, boot_no), part_name(&disk.name, data_no));
    if mode == Mode::Whole && reread(&disk.name).is_ok() {
        // the kernel now knows the new layout
    } else {
        for p in [&boot_p, &data_p] {
            let n = part_name(&disk.name, p.number);
            if !Path::new(&format!("/dev/{n}")).exists() {
                persist::announce_partition(&disk.name, bd.sector, p).map_err(|e| format!("cannot tell the kernel about {n}: {e}"))?;
            }
        }
    }
    for n in [&boot_name, &data_name] {
        if !persist::wait_for(&format!("/dev/{n}"), Duration::from_secs(10)) {
            return Err(format!("/dev/{n} did not appear; reboot and run flint-install again"));
        }
    }

    // Boot partition: a copy of the running one, plus the loader where the boot entry points.
    println!("copying the boot partition to /dev/{boot_name}");
    let mut tree = fat::read_tree(Path::new("/boot"))?;
    let loader = Path::new("/boot/EFI/BOOT/BOOTX64.EFI");
    let len = fs::metadata(loader).map_err(|e| format!("{}: {e}", loader.display()))?.len();
    let efi_dir = tree.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case("EFI"));
    let flint_dir = ("FLINT".to_string(), fat::Node::Dir(vec![("BOOTX64.EFI".to_string(), fat::Node::File(fat::Data::File(loader.to_path_buf(), len)))]));
    match efi_dir {
        Some((_, fat::Node::Dir(children))) => {
            children.retain(|(n, _)| !n.eq_ignore_ascii_case("FLINT"));
            children.push(flint_dir);
        }
        _ => tree.push(("EFI".to_string(), fat::Node::Dir(vec![flint_dir]))),
    }
    let mut serial = [0u8; 4];
    fs::File::open("/dev/urandom").and_then(|mut f| io::Read::read_exact(&mut f, &mut serial)).map_err(|e| e.to_string())?;
    {
        let mut f = fs::OpenOptions::new().write(true).open(format!("/dev/{boot_name}")).map_err(|e| e.to_string())?;
        fat::format(&mut f, 0, boot_p.sectors(), boot_p.first, "FLINT", u32::from_le_bytes(serial), &tree)?;
        f.sync_all().map_err(|e| e.to_string())?;
    }

    println!("formatting /dev/{data_name} ({})", gib(data_p.sectors() * bd.sector));
    persist::format_data(&format!("/dev/{data_name}")).map_err(|e| format!("/dev/{data_name}: {e}"))?;
    if copy_changes {
        println!("copying your changes");
        let target = "/run/flint/install";
        fs::create_dir_all(target).map_err(|e| e.to_string())?;
        flint_core::sys::mount(&format!("/dev/{data_name}"), target, "ext4", libc::MS_NOATIME, None).map_err(|e| format!("mount /dev/{data_name}: {e}"))?;
        let upper = format!("{target}/root/upper");
        let result = fs::create_dir_all(&upper)
            .and_then(|_| fs::create_dir_all(format!("{target}/root/work")))
            .and_then(|_| copy_tree(&Path::new(persist::DATA_DIR).join("root/upper"), Path::new(&upper)));
        flint_core::sys::sync();
        let _ = flint_core::sys::umount(target);
        let c = result.map_err(|e| format!("copying: {e}"))?;
        println!("copied {} files ({} MiB)", c.files, c.bytes >> 20);
    }

    let option = efi::load_option("Flint Linux", boot_no, boot_p.first, boot_p.sectors(), &boot_guid, LOADER);
    match efi::add_boot_entry("Flint Linux", &option, first) {
        Ok(n) => println!("added boot entry Boot{n:04X} \"Flint Linux\"{}", if first { " (started by default)" } else { " (pick it from the firmware boot menu)" }),
        Err(e) => println!("could not add a boot menu entry ({e}); the firmware may still find {} on its own", "\\EFI\\BOOT\\BOOTX64.EFI"),
    }
    flint_core::sys::sync();
    println!("\nDone. Flint is installed on {}. Remove the USB stick and restart.", disk.name);
    Ok(())
}
