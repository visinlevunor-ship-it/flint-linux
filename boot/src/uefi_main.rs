//! Everything that talks to the firmware.

use core::ffi::c_void;
use core::fmt::{self, Write};
use core::ptr;

use crate::config::{self, Config, Entry};
use crate::efi::*;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const CONFIG_PATH: &str = "\\FLINT\\BOOT.CFG";
const PAGE: usize = 4096;
/// Files are read in pieces: some firmware mishandles one huge read from a USB stick.
const READ_CHUNK: usize = 4 << 20;

static mut ST: *mut SystemTable = ptr::null_mut();

fn st() -> &'static SystemTable {
    // SAFETY: set once at entry, before anything else runs; valid until ExitBootServices,
    // which only the kernel calls.
    unsafe { &*ST }
}

fn bs() -> &'static BootServices {
    // SAFETY: as above.
    unsafe { &*st().boot_services }
}

fn rt() -> &'static RuntimeServices {
    // SAFETY: as above.
    unsafe { &*st().runtime_services }
}

// ---------------------------------------------------------------------------------------
// Console

struct Con;

impl Write for Con {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let out = st().con_out;
        let mut buf = [0u16; 129];
        let mut n = 0;
        let flush = |buf: &mut [u16; 129], n: &mut usize| {
            buf[*n] = 0;
            // SAFETY: NUL-terminated UCS-2 buffer; con_out is the firmware's console.
            unsafe { ((*out).output_string)(out, buf.as_ptr()) };
            *n = 0;
        };
        for ch in s.chars() {
            if n + 2 >= 128 {
                flush(&mut buf, &mut n);
            }
            if ch == '\n' {
                buf[n] = b'\r' as u16;
                n += 1;
            }
            // Characters outside UCS-2 are shown as '?'.
            buf[n] = if (ch as u32) < 0xFFFF { ch as u16 } else { b'?' as u16 };
            n += 1;
        }
        flush(&mut buf, &mut n);
        Ok(())
    }
}

macro_rules! say {
    ($($a:tt)*) => {{ let _ = write!(Con, $($a)*); }};
}

fn attr(a: usize) {
    let out = st().con_out;
    // SAFETY: firmware console call.
    unsafe { ((*out).set_attribute)(out, a) };
}

fn clear() {
    let out = st().con_out;
    // SAFETY: firmware console call.
    unsafe {
        ((*out).set_attribute)(out, ATTR_NORMAL);
        ((*out).clear_screen)(out);
        ((*out).enable_cursor)(out, false);
    }
}

fn columns() -> usize {
    let out = st().con_out;
    let (mut c, mut r) = (80usize, 25usize);
    // SAFETY: firmware console call; mode points at the firmware's mode structure.
    unsafe {
        let mode = (*(*out).mode).mode;
        if mode >= 0 {
            ((*out).query_mode)(out, mode as usize, &mut c, &mut r);
        }
    }
    c.clamp(40, 200)
}

fn read_key() -> Option<InputKey> {
    let input = st().con_in;
    let mut key = InputKey { scan_code: 0, unicode_char: 0 };
    // SAFETY: firmware console call.
    let s = unsafe { ((*input).read_key_stroke)(input, &mut key) };
    (s == SUCCESS).then_some(key)
}

/// Wait for a key, or up to `ms` milliseconds (0 = forever). None on timeout.
fn wait_key(ms: u64) -> Option<InputKey> {
    if let Some(k) = read_key() {
        return Some(k);
    }
    // SAFETY: con_in is valid; wait_for_key is an event the firmware owns.
    let mut events: [Event; 2] = [unsafe { (*st().con_in).wait_for_key }, ptr::null_mut()];
    let mut count = 1;
    let mut timer: Event = ptr::null_mut();
    if ms > 0 && (bs().create_event)(EVT_TIMER, 0, 0, ptr::null_mut(), &mut timer) == SUCCESS {
        (bs().set_timer)(timer, TIMER_RELATIVE, ms * 10_000);
        events[1] = timer;
        count = 2;
    }
    let mut index = 0usize;
    let s = (bs().wait_for_event)(count, events.as_ptr(), &mut index);
    if !timer.is_null() {
        (bs().close_event)(timer);
    }
    if s != SUCCESS {
        return None;
    }
    if index == 0 {
        // A key event can fire without a key (some firmware); loop back through read_key.
        return read_key().or_else(|| wait_key(ms));
    }
    None
}

fn pause(msg: &str) {
    attr(ATTR_NORMAL);
    say!("\n{msg}");
    while read_key().is_some() {}
    let _ = wait_key(0);
}

// ---------------------------------------------------------------------------------------
// Memory

struct Pages {
    addr: u64,
    count: usize,
}

impl Pages {
    fn new(bytes: usize) -> Result<Pages, Status> {
        let count = bytes.div_ceil(PAGE).max(1);
        let mut addr = 0u64;
        match (bs().allocate_pages)(ALLOCATE_ANY_PAGES, LOADER_DATA, count, &mut addr) {
            SUCCESS => Ok(Pages { addr, count }),
            e => Err(e),
        }
    }

    fn ptr(&self) -> *mut u8 {
        self.addr as *mut u8
    }

    fn slice(&mut self) -> &mut [u8] {
        // SAFETY: the firmware gave us `count` whole pages at `addr`; nothing else uses them,
        // and `&mut self` makes this the only borrow.
        unsafe { core::slice::from_raw_parts_mut(self.ptr(), self.count * PAGE) }
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: as above, shared.
        unsafe { core::slice::from_raw_parts(self.ptr(), self.count * PAGE) }
    }

}

impl Drop for Pages {
    fn drop(&mut self) {
        (bs().free_pages)(self.addr, self.count);
    }
}

// ---------------------------------------------------------------------------------------
// Files on the boot partition

fn ucs2<const N: usize>(s: &str, out: &mut [u16; N]) -> Result<(), Status> {
    let mut n = 0;
    for ch in s.chars() {
        if n + 1 >= N || (ch as u32) >= 0xFFFF {
            return Err(INVALID_PARAMETER);
        }
        // Accept '/' too: people write paths both ways.
        out[n] = if ch == '/' { b'\\' as u16 } else { ch as u16 };
        n += 1;
    }
    out[n] = 0;
    Ok(())
}

struct Volume {
    root: *mut File,
}

impl Volume {
    fn open(device: Handle) -> Result<Volume, Status> {
        let mut fs: *mut c_void = ptr::null_mut();
        let s = (bs().handle_protocol)(device, &SIMPLE_FILE_SYSTEM_GUID, &mut fs);
        if s != SUCCESS {
            return Err(s);
        }
        let fs = fs as *mut SimpleFileSystem;
        let mut root: *mut File = ptr::null_mut();
        // SAFETY: fs is the firmware's SimpleFileSystem protocol for our boot device.
        let s = unsafe { ((*fs).open_volume)(fs, &mut root) };
        if s != SUCCESS {
            return Err(s);
        }
        Ok(Volume { root })
    }

    fn open_file(&self, path: &str) -> Result<*mut File, Status> {
        let mut name = [0u16; 256];
        ucs2(path, &mut name)?;
        let mut f: *mut File = ptr::null_mut();
        // SAFETY: root is an open directory; name is NUL-terminated.
        let s = unsafe { ((*self.root).open)(self.root, &mut f, name.as_ptr(), FILE_MODE_READ, 0) };
        if s != SUCCESS {
            return Err(s);
        }
        Ok(f)
    }

    fn size(f: *mut File) -> Result<usize, Status> {
        // EFI_FILE_INFO: Size, FileSize, PhysicalSize, three times, Attribute, then the name.
        let mut buf = [0u64; 64];
        let mut len = core::mem::size_of_val(&buf);
        // SAFETY: f is an open file; buf is large enough for the info of a short name.
        let s = unsafe { ((*f).get_info)(f, &FILE_INFO_GUID, &mut len, buf.as_mut_ptr().cast()) };
        if s != SUCCESS {
            return Err(s);
        }
        Ok(buf[1] as usize)
    }

    /// Read `path` into `dst` (which must be big enough). `progress` is shown every chunk.
    fn read_into(f: *mut File, dst: &mut [u8], size: usize, progress: bool) -> Result<(), Status> {
        let mut done = 0;
        while done < size {
            let mut n = (size - done).min(READ_CHUNK);
            // SAFETY: dst[done..done+n] is within the slice; f is open.
            let s = unsafe { ((*f).read)(f, &mut n, dst[done..].as_mut_ptr().cast()) };
            if s != SUCCESS {
                return Err(s);
            }
            if n == 0 {
                return Err(DEVICE_ERROR); // file shorter than its size said
            }
            done += n;
            if progress {
                say!(".");
            }
        }
        Ok(())
    }

    fn close(f: *mut File) {
        // SAFETY: f was opened by open_file.
        unsafe { ((*f).close)(f) };
    }

    /// The whole file in fresh pages.
    fn read(&self, path: &str, progress: bool) -> Result<(Pages, usize), Status> {
        let f = self.open_file(path)?;
        let r = (|| {
            let size = Self::size(f)?;
            let mut pages = Pages::new(size)?;
            Self::read_into(f, pages.slice(), size, progress)?;
            Ok((pages, size))
        })();
        Self::close(f);
        r
    }
}

// ---------------------------------------------------------------------------------------
// Device paths

fn node_len(n: *const DevicePathNode) -> usize {
    // SAFETY: n points at a node header inside a firmware device path.
    let l = unsafe { (*n).length };
    u16::from_le_bytes(l) as usize
}

/// The GPT partition GUID from the HardDrive node of a device path.
fn partition_guid(mut n: *const DevicePathNode) -> Option<[u8; 16]> {
    for _ in 0..64 {
        // SAFETY: walking a terminated device path, node by node, by each node's length.
        let (kind, sub) = unsafe { ((*n).kind, (*n).subtype) };
        let len = node_len(n);
        if kind == DP_END || len < 4 {
            return None;
        }
        // HardDrive node: header(4) partition number(4) start(8) size(8) signature(16) format(1) type(1)
        if kind == DP_MEDIA && sub == DP_MEDIA_HARD_DRIVE && len >= 42 {
            // SAFETY: the node is at least 42 bytes long.
            let (format, sig_type) = unsafe { (*(n as *const u8).add(40), *(n as *const u8).add(41)) };
            if format == 2 && sig_type == 2 {
                let mut g = [0u8; 16];
                // SAFETY: as above.
                unsafe { ptr::copy_nonoverlapping((n as *const u8).add(24), g.as_mut_ptr(), 16) };
                return Some(g);
            }
        }
        n = (n as *const u8).wrapping_add(len).cast();
    }
    None
}

/// `device` + `\path\to\file`: the full device path of a file on our partition.
fn file_device_path(device: Handle, path: &str) -> Option<Pages> {
    let mut dp: *mut c_void = ptr::null_mut();
    if (bs().handle_protocol)(device, &DEVICE_PATH_GUID, &mut dp) != SUCCESS {
        return None;
    }
    // Length of the device's path without its End node.
    let mut prefix = 0usize;
    let mut n = dp as *const DevicePathNode;
    for _ in 0..64 {
        // SAFETY: walking a terminated device path.
        if unsafe { (*n).kind } == DP_END {
            break;
        }
        let l = node_len(n);
        if l < 4 {
            return None;
        }
        prefix += l;
        n = (n as *const u8).wrapping_add(l).cast();
    }
    let chars = path.chars().count() + 1;
    let file_node = 4 + 2 * chars;
    let mut pages = Pages::new(prefix + file_node + 4).ok()?;
    let out = pages.slice();
    // SAFETY: copying `prefix` bytes of the firmware's device path into our pages.
    unsafe { ptr::copy_nonoverlapping(dp as *const u8, out.as_mut_ptr(), prefix) };
    let f = &mut out[prefix..];
    f[0] = DP_MEDIA;
    f[1] = DP_MEDIA_FILE_PATH;
    f[2..4].copy_from_slice(&(file_node as u16).to_le_bytes());
    for (i, ch) in path.chars().chain(core::iter::once('\0')).enumerate() {
        let c = if ch == '/' { b'\\' as u16 } else { ch as u16 };
        f[4 + 2 * i..6 + 2 * i].copy_from_slice(&c.to_le_bytes());
    }
    let e = &mut f[file_node..];
    e[..4].copy_from_slice(&[DP_END, DP_END_ENTIRE, 4, 0]);
    Some(pages)
}

// ---------------------------------------------------------------------------------------
// The initrd, handed to Linux through LoadFile2

#[repr(C)]
struct InitrdLoader {
    proto: LoadFile2,
    data: *const u8,
    size: usize,
}

static mut INITRD: InitrdLoader = InitrdLoader { proto: LoadFile2 { load_file: initrd_load_file }, data: ptr::null(), size: 0 };

static mut INITRD_PATH: InitrdDevicePath = InitrdDevicePath {
    vendor: DevicePathNode { kind: DP_MEDIA, subtype: DP_MEDIA_VENDOR, length: [20, 0] },
    guid: LINUX_INITRD_MEDIA_GUID,
    end: DevicePathNode { kind: DP_END, subtype: DP_END_ENTIRE, length: [4, 0] },
};

extern "efiapi" fn initrd_load_file(this: *mut LoadFile2, _path: *const DevicePathNode, boot_policy: bool, size: *mut usize, buf: *mut c_void) -> Status {
    if this.is_null() || size.is_null() {
        return INVALID_PARAMETER;
    }
    if boot_policy {
        return UNSUPPORTED;
    }
    let me = this as *mut InitrdLoader;
    // SAFETY: `this` is our InitrdLoader (LoadFile2 is its first field); size is valid.
    unsafe {
        let have = (*me).size;
        if buf.is_null() || *size < have {
            *size = have;
            return BUFFER_TOO_SMALL;
        }
        ptr::copy_nonoverlapping((*me).data, buf as *mut u8, have);
        *size = have;
    }
    SUCCESS
}

struct InitrdHandle(Handle);

impl InitrdHandle {
    fn install(data: *const u8, size: usize) -> Result<InitrdHandle, Status> {
        let mut h: Handle = ptr::null_mut();
        // SAFETY: the statics live forever; only this function and Drop touch them, and the
        // firmware runs us on one CPU.
        unsafe {
            INITRD.data = data;
            INITRD.size = size;
            let s = (bs().install_protocol_interface)(&mut h, &DEVICE_PATH_GUID, NATIVE_INTERFACE, ptr::addr_of_mut!(INITRD_PATH).cast());
            if s != SUCCESS {
                return Err(s);
            }
            let s = (bs().install_protocol_interface)(&mut h, &LOAD_FILE2_GUID, NATIVE_INTERFACE, ptr::addr_of_mut!(INITRD).cast());
            if s != SUCCESS {
                (bs().uninstall_protocol_interface)(h, &DEVICE_PATH_GUID, ptr::addr_of_mut!(INITRD_PATH).cast());
                return Err(s);
            }
        }
        Ok(InitrdHandle(h))
    }
}

impl Drop for InitrdHandle {
    fn drop(&mut self) {
        // Undoing install(); addr_of_mut! on a static mut needs no unsafe.
        (bs().uninstall_protocol_interface)(self.0, &LOAD_FILE2_GUID, ptr::addr_of_mut!(INITRD).cast());
        (bs().uninstall_protocol_interface)(self.0, &DEVICE_PATH_GUID, ptr::addr_of_mut!(INITRD_PATH).cast());
    }
}

// ---------------------------------------------------------------------------------------
// Variables

fn get_u64_var(name: &str) -> Option<u64> {
    let mut n = [0u16; 64];
    ucs2(name, &mut n).ok()?;
    let mut v = 0u64;
    let mut size = 8usize;
    let mut attrs = 0u32;
    let s = (rt().get_variable)(n.as_ptr(), &GLOBAL_VARIABLE_GUID, &mut attrs, &mut size, ptr::addr_of_mut!(v).cast());
    match s {
        SUCCESS if size <= 8 => Some(v),
        _ => None,
    }
}

fn secure_boot_on() -> bool {
    get_u64_var("SecureBoot").is_some_and(|v| v & 0xFF == 1)
}

fn can_enter_setup() -> bool {
    get_u64_var("OsIndicationsSupported").is_some_and(|v| v & OS_INDICATIONS_BOOT_TO_FW_UI != 0)
}

fn reboot_to_setup() -> ! {
    let mut n = [0u16; 32];
    let _ = ucs2("OsIndications", &mut n);
    let v = get_u64_var("OsIndications").unwrap_or(0) | OS_INDICATIONS_BOOT_TO_FW_UI;
    (rt().set_variable)(n.as_ptr(), &GLOBAL_VARIABLE_GUID, VARIABLE_NON_VOLATILE | VARIABLE_BOOTSERVICE_ACCESS | VARIABLE_RUNTIME_ACCESS, 8, ptr::addr_of!(v).cast());
    (rt().reset_system)(RESET_COLD, SUCCESS, 0, ptr::null())
}

// ---------------------------------------------------------------------------------------
// Booting an entry

/// Kernel command line: the entry's options, then `flint.esp=`.
fn command_line(options: &str, esp: Option<&[u8; 16]>, out: &mut [u16; 1024]) -> usize {
    let mut n = 0;
    let mut push = |c: u16, n: &mut usize| {
        if *n + 1 < out.len() {
            out[*n] = c;
            *n += 1;
        }
    };
    for ch in options.chars() {
        push(if (ch as u32) < 0x80 { ch as u16 } else { b'?' as u16 }, &mut n);
    }
    if let Some(g) = esp {
        let mut text = [0u8; 36];
        config::esp_option(g, &mut text);
        if n > 0 {
            push(b' ' as u16, &mut n);
        }
        for b in b"flint.esp=".iter().chain(text.iter()) {
            push(u16::from(*b), &mut n);
        }
    }
    out[n] = 0;
    n + 1
}

fn boot(image: Handle, device: Handle, vol: &Volume, e: &Entry, esp: Option<&[u8; 16]>) -> Status {
    clear();
    attr(ATTR_TITLE);
    say!("Booting {}\n\n", e.title);
    attr(ATTR_NORMAL);

    // Initramfs images, one after another. Each cpio archive must start 4-byte aligned.
    let mut total = 0usize;
    for path in e.initrds() {
        let f = match vol.open_file(path) {
            Ok(f) => f,
            Err(s) => return fail("cannot open", path, s),
        };
        let size = Volume::size(f);
        Volume::close(f);
        match size {
            Ok(sz) => total = (total + sz + 3) & !3,
            Err(s) => return fail("cannot read", path, s),
        }
    }
    let initrd = if total > 0 {
        let mut pages = match Pages::new(total) {
            Ok(p) => p,
            Err(s) => return fail("not enough memory for", "the initramfs", s),
        };
        let mut at = 0usize;
        for path in e.initrds() {
            say!("Loading {path} ");
            let r = vol.open_file(path).and_then(|f| {
                let r = Volume::size(f).and_then(|sz| Volume::read_into(f, &mut pages.slice()[at..], sz, true).map(|_| sz));
                Volume::close(f);
                r
            });
            match r {
                Ok(sz) => {
                    // Zero the alignment padding: the kernel reads it as part of the archive.
                    let end = (at + sz + 3) & !3;
                    pages.slice()[at + sz..end].fill(0);
                    at = end;
                    say!(" {} MiB\n", sz.div_ceil(1 << 20));
                }
                Err(s) => return fail("cannot read", path, s),
            }
        }
        Some(pages)
    } else {
        None
    };

    say!("Loading {} ", e.kernel);
    let (kernel, ksize) = match vol.read(e.kernel, true) {
        Ok(k) => k,
        Err(s) => return fail("cannot read", e.kernel, s),
    };
    say!(" {} MiB\n", ksize.div_ceil(1 << 20));

    let dp = file_device_path(device, e.kernel);
    let dp_ptr = dp.as_ref().map_or(ptr::null(), |p| p.ptr() as *const DevicePathNode);
    let mut kh: Handle = ptr::null_mut();
    let s = (bs().load_image)(false, image, dp_ptr, kernel.ptr().cast(), ksize, &mut kh);
    drop(kernel);
    if s != SUCCESS {
        if s == SECURITY_VIOLATION || (s == ACCESS_DENIED && secure_boot_on()) {
            attr(ATTR_ERROR);
            say!("\nSecure Boot refused to start the kernel.\n");
            attr(ATTR_NORMAL);
            say!("Flint is not signed for Secure Boot. Turn Secure Boot off in the firmware setup\n(often under Security or Boot), then boot again.\n");
            return s;
        }
        return fail("cannot start", e.kernel, s);
    }

    // The command line goes in the kernel image's LoadOptions.
    static mut CMDLINE: [u16; 1024] = [0; 1024];
    // SAFETY: only used here, single-threaded; it must outlive StartImage, hence static.
    let cmd = unsafe { &mut *ptr::addr_of_mut!(CMDLINE) };
    let len = command_line(e.options, esp, cmd);
    let mut li: *mut c_void = ptr::null_mut();
    if (bs().handle_protocol)(kh, &LOADED_IMAGE_GUID, &mut li) == SUCCESS {
        let li = li as *mut LoadedImage;
        // SAFETY: li is the kernel image's LoadedImage protocol.
        unsafe {
            (*li).load_options = cmd.as_mut_ptr().cast();
            (*li).load_options_size = (len * 2) as u32;
        }
    }

    let handle = match &initrd {
        Some(p) => match InitrdHandle::install(p.ptr(), total) {
            Ok(h) => Some(h),
            Err(s) => {
                (bs().unload_image)(kh);
                return fail("cannot hand over", "the initramfs", s);
            }
        },
        None => None,
    };
    let mut exit_size = 0usize;
    let mut exit_data: *mut u16 = ptr::null_mut();
    // Linux does not return unless its EFI stub fails.
    let s = (bs().start_image)(kh, &mut exit_size, &mut exit_data);
    drop(handle);
    if let Some(p) = initrd {
        drop(p);
    }
    fail("the kernel returned:", e.kernel, s)
}

fn fail(what: &str, path: &str, s: Status) -> Status {
    attr(ATTR_ERROR);
    say!("\n{what} {path}: {} ({:#x})\n", status_text(s), s);
    attr(ATTR_NORMAL);
    if s == SUCCESS {
        LOAD_ERROR
    } else {
        s
    }
}

// ---------------------------------------------------------------------------------------
// Menu

enum Choice {
    Boot(usize),
    Reboot,
    Setup,
}

fn draw_menu(c: &Config, sel: usize, setup: bool, countdown: Option<u32>) {
    clear();
    let width = columns();
    attr(ATTR_TITLE);
    say!("\n  Flint boot menu  (flint-boot {VERSION})\n\n");
    let n = c.entries().len();
    let extra: &[&str] = if setup { &["Reboot", "Firmware setup"] } else { &["Reboot"] };
    for i in 0..n + extra.len() {
        let title = if i < n { c.entries()[i].title } else { extra[i - n] };
        attr(if i == sel { ATTR_HIGHLIGHT } else { ATTR_NORMAL });
        let shown = title.chars().take(width.saturating_sub(10)).count();
        say!("    {} {}", if i == sel { '>' } else { ' ' }, title);
        for _ in shown..width.saturating_sub(10).min(48) {
            say!(" ");
        }
        attr(ATTR_NORMAL);
        say!("\n");
        if i + 1 == n {
            say!("\n");
        }
    }
    attr(ATTR_NORMAL);
    say!("\n  Up/Down to choose, Enter to start.");
    if let Some(t) = countdown {
        say!("  Starting in {t} s ");
    }
    say!("\n");
    for (line, what) in c.problems() {
        attr(ATTR_ERROR);
        if *line > 0 {
            say!("\n  {CONFIG_PATH} line {line}: {what}");
        } else {
            say!("\n  {CONFIG_PATH}: {what}");
        }
    }
    attr(ATTR_NORMAL);
}

fn menu(c: &Config, mut timeout: Option<u32>) -> Choice {
    let setup = can_enter_setup();
    let n = c.entries().len();
    let total = n + 1 + usize::from(setup);
    let mut sel = c.default;
    if timeout == Some(0) {
        // Booting at once, unless a key is already held.
        if read_key().is_none() {
            return Choice::Boot(sel);
        }
        timeout = None;
    }
    loop {
        draw_menu(c, sel, setup, timeout);
        let key = match timeout {
            Some(0) => return Choice::Boot(sel),
            Some(t) => match wait_key(1000) {
                Some(k) => {
                    timeout = None; // any key stops the countdown
                    k
                }
                None => {
                    timeout = Some(t - 1);
                    continue;
                }
            },
            None => match wait_key(0) {
                Some(k) => k,
                None => continue,
            },
        };
        match (key.scan_code, key.unicode_char) {
            (SCAN_UP, _) => sel = (sel + total - 1) % total,
            (SCAN_DOWN, _) => sel = (sel + 1) % total,
            (_, 0x0D) | (_, 0x0A) => {
                return match sel {
                    s if s < n => Choice::Boot(s),
                    s if s == n => Choice::Reboot,
                    _ => Choice::Setup,
                }
            }
            (_, ch) if (u16::from(b'1')..=u16::from(b'9')).contains(&ch) && usize::from(ch - u16::from(b'1')) < n => {
                return Choice::Boot(usize::from(ch - u16::from(b'1')));
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------

#[no_mangle]
extern "efiapi" fn efi_main(image: Handle, system_table: *mut SystemTable) -> Status {
    // SAFETY: the one write to ST, before anything reads it.
    unsafe { ST = system_table };
    // The firmware reboots after 5 minutes in a boot application unless told otherwise.
    (bs().set_watchdog_timer)(0, 0, 0, ptr::null());
    // SAFETY: con_in is the firmware's console input.
    unsafe { ((*st().con_in).reset)(st().con_in, false) };

    let mut li: *mut c_void = ptr::null_mut();
    if (bs().handle_protocol)(image, &LOADED_IMAGE_GUID, &mut li) != SUCCESS {
        say!("flint-boot: cannot find out where it was started from\n");
        pause("Press a key to return to the firmware.");
        return LOAD_ERROR;
    }
    // SAFETY: our own LoadedImage protocol.
    let device = unsafe { (*(li as *mut LoadedImage)).device_handle };
    let mut dp: *mut c_void = ptr::null_mut();
    let esp = if (bs().handle_protocol)(device, &DEVICE_PATH_GUID, &mut dp) == SUCCESS { partition_guid(dp as *const DevicePathNode) } else { None };

    let vol = match Volume::open(device) {
        Ok(v) => v,
        Err(s) => {
            say!("flint-boot: cannot read the boot partition: {}\n", status_text(s));
            pause("Press a key to return to the firmware.");
            return s;
        }
    };

    // The config file stays in memory for the whole run: entries point into it.
    let cfg_pages = vol.read(CONFIG_PATH, false).ok();
    let text = cfg_pages.as_ref().and_then(|(p, size)| core::str::from_utf8(&p.bytes()[..*size]).ok());
    let cfg = match text {
        Some(t) => Config::parse(t),
        None => Config::builtin(),
    };

    let mut timeout = Some(cfg.timeout);
    loop {
        match menu(&cfg, timeout) {
            Choice::Boot(i) => {
                let s = boot(image, device, &vol, &cfg.entries()[i], esp.as_ref());
                if is_error(s) {
                    pause("Press a key to go back to the menu.");
                }
            }
            Choice::Reboot => (rt().reset_system)(RESET_COLD, SUCCESS, 0, ptr::null()),
            Choice::Setup => reboot_to_setup(),
        }
        timeout = None; // after a failure, wait for the user
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // SAFETY: if ST is not set yet there is no console to print on.
    if unsafe { !ST.is_null() } {
        attr(ATTR_ERROR);
        say!("\nflint-boot: internal error: {info}\n");
        (bs().stall)(10_000_000);
        (rt().reset_system)(RESET_COLD, ABORTED, 0, ptr::null())
    }
    loop {}
}
