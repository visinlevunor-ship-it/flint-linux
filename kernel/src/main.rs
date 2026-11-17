//! The Flint kernel.
//!
//! flint-boot starts it like Linux: a UEFI program, with the command line in its load options
//! and the base image behind the LoadFile2 protocol on Linux's initrd media path. It takes
//! what it needs from UEFI (the base image, the framebuffer, the memory map), leaves UEFI with
//! ExitBootServices and runs the machine itself: its own page tables, interrupt table and
//! system call entry. Programs are ordinary static Linux x86-64 executables; they talk to the
//! kernel through Linux's system calls.
//!
//! Version 0.2 runs processes (fork, exec, wait, pipes, a scheduler with a timer), reads the
//! keyboard (i8042) and the serial port, and gives programs the base image with a writable
//! layer in RAM as their root filesystem. The first program is Flint's shell.

#![no_std]
#![no_main]

extern crate alloc;

mod ahci;
mod arch;
mod block;
mod console;
mod cpio;
mod dev;
mod ext4;
mod fat;
mod nvme;
mod pci;
mod persist;
mod virtio_blk;
mod efi;
mod font;
mod mem;
mod proc;
mod syscall;
mod tty;
mod vfs;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;
use efi::*;

#[global_allocator]
static HEAP: mem::Heap = mem::Heap;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kprintln!("\n[kernel] PANIC: {}", info);
    arch::halt()
}

/// What the kernel takes from UEFI before leaving it.
struct Handover {
    cmdline: [u8; 1024],
    cmdline_len: usize,
    initrd: u64,
    initrd_size: usize,
    fb: u64,
    fb_width: usize,
    fb_height: usize,
    fb_stride: usize,
    fb_bgr: bool,
    image_base: u64,
    image_size: u64,
    map: [MemoryDescriptor; 512],
    map_len: usize,
}

static mut HANDOVER: Handover = Handover {
    cmdline: [0; 1024],
    cmdline_len: 0,
    initrd: 0,
    initrd_size: 0,
    fb: 0,
    fb_width: 0,
    fb_height: 0,
    fb_stride: 0,
    fb_bgr: true,
    image_base: 0,
    image_size: 0,
    map: [MemoryDescriptor { kind: 0, _pad: 0, physical_start: 0, virtual_start: 0, number_of_pages: 0, attribute: 0 }; 512],
    map_len: 0,
};

fn uefi_fail(st: *mut SystemTable, msg: &str) -> Status {
    let _ = st;
    console::serial_init();
    kprintln!("[kernel] {}", msg);
    1
}

#[no_mangle]
extern "efiapi" fn efi_main(image: Handle, st: *mut SystemTable) -> Status {
    // SAFETY: the firmware gives a valid system table; everything below follows UEFI's rules
    // while boot services are still up, and HANDOVER is written only here.
    unsafe {
        let bs = &*(*st).boot_services;
        (bs.set_watchdog_timer)(0, 0, 0, ptr::null());
        let h = &mut *ptr::addr_of_mut!(HANDOVER);

        // the command line and where we were loaded
        let mut li: *mut c_void = ptr::null_mut();
        if (bs.handle_protocol)(image, &LOADED_IMAGE_GUID, &mut li) != SUCCESS {
            return uefi_fail(st, "no loaded image protocol");
        }
        let li = &*(li as *const LoadedImage);
        h.image_base = li.image_base as u64;
        h.image_size = li.image_size;
        if !li.load_options.is_null() {
            let words = core::slice::from_raw_parts(li.load_options as *const u16, li.load_options_size as usize / 2);
            for &w in words {
                if w == 0 || h.cmdline_len == h.cmdline.len() {
                    break;
                }
                h.cmdline[h.cmdline_len] = if w < 128 { w as u8 } else { b'?' };
                h.cmdline_len += 1;
            }
        }

        // the base image, as Linux's EFI stub fetches it
        let mut dp: *const DevicePathNode = ptr::addr_of!(INITRD_PATH) as *const DevicePathNode;
        let mut handle: Handle = ptr::null_mut();
        if (bs.locate_device_path)(&LOAD_FILE2_GUID, &mut dp, &mut handle) == SUCCESS {
            let mut lf: *mut c_void = ptr::null_mut();
            if (bs.handle_protocol)(handle, &LOAD_FILE2_GUID, &mut lf) == SUCCESS {
                let lf = lf as *mut LoadFile2;
                let path = ptr::addr_of!(INITRD_PATH) as *const DevicePathNode;
                let mut size = 0usize;
                let s = ((*lf).load_file)(lf, path, false, &mut size, ptr::null_mut());
                if s == BUFFER_TOO_SMALL && size > 0 {
                    let mut buf = 0u64;
                    if (bs.allocate_pages)(ALLOCATE_ANY_PAGES, LOADER_DATA, size.div_ceil(4096), &mut buf) == SUCCESS
                        && ((*lf).load_file)(lf, path, false, &mut size, buf as *mut c_void) == SUCCESS
                    {
                        h.initrd = buf;
                        h.initrd_size = size;
                    }
                }
            }
        }

        // the framebuffer
        let mut gop: *mut c_void = ptr::null_mut();
        if (bs.locate_protocol)(&GOP_GUID, ptr::null_mut(), &mut gop) == SUCCESS {
            let mode = &*(*(gop as *const Gop)).mode;
            let info = &*mode.info;
            if info.pixel_format <= 1 {
                h.fb = mode.frame_buffer_base;
                h.fb_width = info.width as usize;
                h.fb_height = info.height as usize;
                h.fb_stride = info.pixels_per_scan_line as usize;
                h.fb_bgr = info.pixel_format == 1;
            }
        }

        // the memory map, then leave boot services (retry once if the map changed meanwhile)
        let mut buf: *mut c_void = ptr::null_mut();
        let buf_size = 64 * 1024;
        if (bs.allocate_pool)(LOADER_DATA, buf_size, &mut buf) != SUCCESS {
            return uefi_fail(st, "cannot allocate the memory map");
        }
        let mut exited = false;
        let mut desc_size = 0usize;
        let mut map_size = 0usize;
        for _ in 0..3 {
            map_size = buf_size;
            let mut key = 0usize;
            let mut ver = 0u32;
            if (bs.get_memory_map)(&mut map_size, buf as *mut u8, &mut key, &mut desc_size, &mut ver) != SUCCESS {
                continue;
            }
            if (bs.exit_boot_services)(image, key) == SUCCESS {
                exited = true;
                break;
            }
        }
        if !exited {
            return uefi_fail(st, "ExitBootServices failed");
        }
        // boot services are gone: copy the map out of firmware memory we may reuse
        let n = (map_size / desc_size).min(h.map.len());
        for i in 0..n {
            h.map[i] = ptr::read_unaligned((buf as *const u8).add(i * desc_size) as *const MemoryDescriptor);
        }
        h.map_len = n;

        // continue on the kernel's own stack: UEFI's is in memory we hand to programs
        core::arch::asm!(
            "mov rsp, {top}",
            "xor ebp, ebp",
            "call {main}",
            top = in(reg) arch::stack_top(ptr::addr_of!(arch::KSTACK)),
            main = sym kmain,
            options(noreturn)
        );
    }
}

extern "C" fn kmain() -> ! {
    console::serial_init();
    // SAFETY: start-up, one CPU, interrupts off; HANDOVER was filled before we got here.
    let h = unsafe { &*ptr::addr_of!(HANDOVER) };
    unsafe {
        arch::init_gdt();
        arch::init_idt();
        mem::init_frames(&h.map[..h.map_len]);
        // map physical memory, the framebuffer and the base image
        let mut limit = 4u64 << 30;
        for d in &h.map[..h.map_len] {
            limit = limit.max(d.physical_start + d.number_of_pages * 4096);
        }
        if h.fb != 0 {
            limit = limit.max(h.fb + (h.fb_stride * h.fb_height * 4) as u64);
        }
        mem::init_paging(limit.min(1 << 46), h.image_base, h.image_size);
        mem::init_heap();
        if h.fb != 0 {
            let mut fb = console::Framebuffer::new(mem::phys_to_virt(h.fb) as *mut u32, h.fb_width, h.fb_height, h.fb_stride, h.fb_bgr);
            fb.clear();
            console::FB = Some(fb);
        }
        arch::init_syscall();
    }
    syscall::init_time();
    let cmdline = core::str::from_utf8(&h.cmdline[..h.cmdline_len]).unwrap_or("");
    // SAFETY: counters read on one CPU.
    let (total, used) = unsafe { (mem::TOTAL_PAGES * 4096, mem::USED_PAGES * 4096) };
    kprintln!("Flint kernel {} (x86-64)", env!("CARGO_PKG_VERSION"));
    kprintln!("[kernel] memory: {} MiB usable, {} MiB used by the kernel", total >> 20, used >> 20);
    kprintln!("[kernel] CPU clock: {} MHz", syscall::tsc_hz() / 1_000_000);
    if h.fb != 0 {
        kprintln!("[kernel] screen: {}x{}", h.fb_width, h.fb_height);
    }
    kprintln!("[kernel] command line: {}", cmdline);
    if h.initrd == 0 {
        kprintln!("[kernel] no base image from the boot loader; nothing to run");
        arch::halt();
    }
    // SAFETY: the base image stays where UEFI put it (loader data, never handed out).
    let img: &'static [u8] = unsafe { core::slice::from_raw_parts(mem::phys_to_virt(h.initrd), h.initrd_size) };
    let n = cpio::load(img);
    kprintln!("[kernel] base image: {} KiB, {} files", h.initrd_size / 1024, n);

    // disks: NVMe, SATA (AHCI), virtio
    let devs = pci::scan();
    kprintln!("[kernel] PCI: {} devices", devs.len());
    for d in &devs {
        if let Some(disk) = nvme::probe(d, nvme_count(&devs, d)) {
            block::register(disk);
        }
        for disk in ahci::probe(d, 0) {
            block::register(disk);
        }
        if let Some(disk) = virtio_blk::probe(d, 0) {
            block::register(disk);
        }
    }
    block::scan_partitions();
    persist::init(cmdline);
    vfs::init();
    dev::init();

    let env: Vec<String> = ["PATH=/bin:/sbin:/usr/bin:/usr/sbin", "HOME=/root", "TERM=linux", "USER=root", "LOGNAME=root", "SHELL=/bin/sh", "LANG=C.UTF-8"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    // the first program: init=PROGRAM[,ARG...] as on Linux, else Flint's shell
    let mut argv: Vec<String> = alloc::vec!["sh".to_string()];
    for w in cmdline.split(' ') {
        if let Some(v) = w.strip_prefix("init=") {
            argv = v.split(',').filter(|a| !a.is_empty()).map(|a| a.to_string()).collect();
        }
    }
    let path = find_program(&argv[0]);
    kprintln!("[kernel] starting {}\n", path);
    if let Err(e) = proc::start_init(&path, &argv, &env, proc::console_fds()) {
        kprintln!("[kernel] cannot start {}: error {}", path, e);
    }
    arch::halt()
}

fn nvme_count(all: &[pci::Dev], d: &pci::Dev) -> usize {
    all.iter().take_while(|x| !(x.bus == d.bus && x.dev == d.dev && x.func == d.func)).filter(|x| x.class == 1 && x.subclass == 8).count()
}

/// A program named without a slash is looked up in PATH's directories.
fn find_program(name: &str) -> String {
    if name.contains('/') {
        return name.to_string();
    }
    for dir in ["bin", "sbin", "usr/bin", "usr/sbin"] {
        let p = alloc::format!("/{dir}/{name}");
        if vfs::resolve("", &p, true).is_ok() {
            return p;
        }
    }
    name.to_string()
}
