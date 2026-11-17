//! Keeping what changes under the Flint kernel: the RAM layer is saved as one file on the
//! boot partition (FAT32), \FLINT\STATE.IMG, a few seconds after a change, on sync, and before
//! the machine stops; it is loaded at the next start. Flint's data partition (ext4) is only
//! read: the Flint kernel does not write ext4 yet.

use crate::fat::Fat;
use crate::{block, dev, kprintln, vfs};

static mut ESP: Option<Fat> = None;
static mut LAST_CHANGE: u64 = 0;
static mut PENDING: bool = false;

/// Find the boot partition (flint.esp=PARTUUID from flint-boot, else the first EFI system
/// partition) and the data partition; load the saved state.
pub fn init(cmdline: &str) {
    let want = cmdline.split(' ').find_map(|w| w.strip_prefix("flint.esp=")).map(|s| s.to_ascii_lowercase());
    let parts = block::parts();
    let esp = match &want {
        Some(id) => parts.iter().find(|p| block::guid_text(&p.uuid) == *id),
        None => None,
    }
    .or_else(|| parts.iter().find(|p| p.type_guid == block::ESP_TYPE));
    if let Some(p) = esp {
        match Fat::open(p) {
            Some(fat) => {
                kprintln!("[kernel] boot partition: {} on {}", block::guid_text(&p.uuid), block::disk_name(p.disk));
                if let Some(img) = fat.read_file(&["FLINT"], "STATE.IMG") {
                    let n = vfs::import(&img);
                    kprintln!("[kernel] restored {} saved files and directories", n);
                }
                // SAFETY: start-up.
                unsafe { ESP = Some(fat) };
            }
            None => kprintln!("[kernel] the boot partition is not FAT32: changes are not saved"),
        }
    } else {
        kprintln!("[kernel] no boot partition found: changes are not saved");
    }
    if let Some(p) = parts.iter().find(|p| p.name == "Flint data") {
        match crate::ext4::Ext4::open(p) {
            Some(fs) => match fs.walk(2, "root/upper") {
                Some(upper) => {
                    kprintln!("[kernel] data partition: installed packages and your files (read-only)");
                    vfs::set_data_layer(fs, upper);
                }
                None => kprintln!("[kernel] data partition has no root/upper yet"),
            },
            None => kprintln!("[kernel] data partition: not ext4"),
        }
    }
}

pub fn available() -> bool {
    // SAFETY: set once at start-up.
    unsafe { (*core::ptr::addr_of!(ESP)).is_some() }
}

/// Save now (sync, before stopping).
pub fn save() {
    // SAFETY: one CPU.
    let fat = unsafe { (*core::ptr::addr_of!(ESP)).as_ref() };
    let Some(fat) = fat else { return };
    vfs::take_dirty();
    // SAFETY: one CPU.
    unsafe { PENDING = false };
    let img = vfs::export();
    if fat.write_file(&["FLINT"], "STATE.IMG", &img).is_err() {
        kprintln!("[kernel] could not save changes to the boot partition");
    }
}

/// Called while idle: save a few seconds after the last change.
pub fn tick() {
    if !available() {
        return;
    }
    // SAFETY: one CPU.
    unsafe {
        if vfs::take_dirty() {
            PENDING = true;
            LAST_CHANGE = dev::ticks();
        }
        if PENDING && dev::ticks() - LAST_CHANGE > 2 * dev::HZ {
            save();
        }
    }
}
