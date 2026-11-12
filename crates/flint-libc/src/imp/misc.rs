//! System information and the rest: getauxval, dl_iterate_phdr, the vDSO, uname,
//! gethostname, sethostname, sysconf, klogctl, reboot.

use crate::imp::errno;
use crate::imp::sys::{self, ret_i, sc1, sc2, sc3, sc4};
use crate::imp::types::*;
use crate::imp::{mem, start, tls};
use core::sync::atomic::Ordering;

#[no_mangle]
pub unsafe extern "C" fn getauxval(ty: c_ulong) -> c_ulong {
    let mut a = start::AUXV.load(Ordering::Relaxed) as *const usize;
    if !a.is_null() {
        while *a != 0 {
            if *a == ty as usize {
                return *a.add(1) as c_ulong;
            }
            a = a.add(2);
        }
    }
    errno::set(errno::ENOENT);
    0
}

#[repr(C)]
pub struct DlPhdrInfo {
    addr: usize,
    name: *const c_char,
    phdr: *const c_void,
    phnum: u16,
    adds: u64,
    subs: u64,
    tls_modid: usize,
    tls_data: *mut c_void,
}

/// The program's own load bias: AT_PHDR minus PT_PHDR's address (0 for a non-PIE binary).
pub unsafe fn load_bias() -> usize {
    let phdr = getauxval(3) as usize;
    let phnum = getauxval(5) as usize;
    for i in 0..phnum {
        let p = phdr + i * 56;
        if *(p as *const u32) == 6 {
            return phdr.wrapping_sub(*((p + 16) as *const usize));
        }
    }
    0
}

/// A static program is one object: report it (the unwinder finds .eh_frame_hdr this way).
#[no_mangle]
pub unsafe extern "C" fn dl_iterate_phdr(
    cb: unsafe extern "C" fn(*mut DlPhdrInfo, size_t, *mut c_void) -> c_int,
    data: *mut c_void,
) -> c_int {
    let tls_img = &*core::ptr::addr_of!(tls::IMAGE);
    let main = tls::main_thread();
    let mut info = DlPhdrInfo {
        addr: load_bias(),
        name: b"\0".as_ptr() as *const c_char,
        phdr: getauxval(3) as *const c_void,
        phnum: getauxval(5) as u16,
        adds: 1,
        subs: 0,
        tls_modid: (tls_img.memsz > 0) as usize,
        tls_data: if tls_img.memsz > 0 { (main as usize - tls::tls_offset()) as *mut c_void } else { core::ptr::null_mut() },
    };
    cb(&mut info, core::mem::size_of::<DlPhdrInfo>(), data)
}

/// Look up a function in the kernel's vDSO by name.
pub unsafe fn vdso_symbol(name: &[u8]) -> Option<usize> {
    let base = getauxval(33 /* AT_SYSINFO_EHDR */) as usize;
    if base == 0 {
        return None;
    }
    // ELF64 header: e_phoff at 32, e_phnum at 56
    let phoff = *((base + 32) as *const u64) as usize;
    let phnum = *((base + 56) as *const u16) as usize;
    let (mut bias, mut dynamic) = (None, 0usize);
    for i in 0..phnum {
        let p = base + phoff + i * 56;
        let ty = *(p as *const u32);
        let vaddr = *((p + 16) as *const usize);
        if ty == 1 && bias.is_none() {
            // first PT_LOAD: the vDSO's link-time address of its start
            let off = *((p + 8) as *const usize);
            bias = Some(base + off - vaddr);
        } else if ty == 2 {
            dynamic = vaddr;
        }
    }
    let bias = bias?;
    if dynamic == 0 {
        return None;
    }
    let (mut strtab, mut symtab, mut hash) = (0, 0, 0);
    let mut d = (bias + dynamic) as *const usize;
    while *d != 0 {
        match *d {
            4 => hash = *d.add(1),   // DT_HASH
            5 => strtab = *d.add(1), // DT_STRTAB
            6 => symtab = *d.add(1), // DT_SYMTAB
            _ => {}
        }
        d = d.add(2);
    }
    if strtab == 0 || symtab == 0 || hash == 0 {
        return None;
    }
    let nsyms = *((bias + hash + 4) as *const u32) as usize; // nchain
    for i in 0..nsyms {
        // Elf64_Sym: st_name u32, st_info u8, st_other u8, st_shndx u16, st_value u64, st_size u64
        let s = bias + symtab + i * 24;
        let info = *((s + 4) as *const u8);
        let shndx = *((s + 6) as *const u16);
        if info & 0xf != 2 /* STT_FUNC */ || shndx == 0 {
            continue;
        }
        let n = mem::cstr((bias + strtab + *(s as *const u32) as usize) as *const u8);
        if n == name {
            return Some(bias + *((s + 8) as *const usize));
        }
    }
    None
}

#[no_mangle]
pub unsafe extern "C" fn uname(buf: *mut c_void) -> c_int {
    ret_i(sc1(sys::UNAME, buf as usize))
}

#[no_mangle]
pub unsafe extern "C" fn gethostname(name: *mut c_char, len: size_t) -> c_int {
    let mut u = [0u8; 65 * 6];
    if uname(u.as_mut_ptr() as *mut c_void) < 0 {
        return -1;
    }
    let node = &u[65..130];
    let n = node.iter().position(|&c| c == 0).unwrap_or(65);
    if len == 0 {
        errno::set(errno::ENAMETOOLONG);
        return -1;
    }
    let copy = n.min(len - 1);
    core::ptr::copy_nonoverlapping(node.as_ptr(), name as *mut u8, copy);
    *name.add(copy) = 0;
    if n >= len {
        errno::set(errno::ENAMETOOLONG);
        return -1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn sethostname(name: *const c_char, len: size_t) -> c_int {
    ret_i(sc2(sys::SETHOSTNAME, name as usize, len))
}

#[no_mangle]
pub unsafe extern "C" fn klogctl(ty: c_int, buf: *mut c_char, len: c_int) -> c_int {
    ret_i(sc3(sys::SYSLOG, ty as usize, buf as usize, len as usize))
}

#[no_mangle]
pub unsafe extern "C" fn reboot(cmd: c_int) -> c_int {
    ret_i(sc4(sys::REBOOT, 0xfee1_dead, 672_274_793, cmd as u32 as usize, 0))
}

fn online_cpus() -> c_long {
    let mut set = [0u64; 16]; // 1024 CPUs
    // SAFETY: the kernel writes at most the size given
    let n = unsafe { sc3(sys::SCHED_GETAFFINITY, 0, core::mem::size_of_val(&set), set.as_mut_ptr() as usize) };
    if n <= 0 {
        return 1;
    }
    set.iter().map(|w| w.count_ones() as c_long).sum::<c_long>().max(1)
}

#[no_mangle]
pub unsafe extern "C" fn sysconf(name: c_int) -> c_long {
    let rlimit = |res: usize| -> c_long {
        let mut lim = [0u64; 2];
        if sc4(sys::PRLIMIT64, 0, res, 0, lim.as_mut_ptr() as usize) == 0 && lim[0] != u64::MAX {
            lim[0] as c_long
        } else {
            -1
        }
    };
    match name {
        0 => 131_072 * 16,                                       // _SC_ARG_MAX
        1 => rlimit(6),                                          // _SC_CHILD_MAX (RLIMIT_NPROC)
        2 => 100,                                                // _SC_CLK_TCK
        3 => 65_536,                                             // _SC_NGROUPS_MAX
        4 => rlimit(7),                                          // _SC_OPEN_MAX (RLIMIT_NOFILE)
        30 => start::PAGE_SIZE.load(Ordering::Relaxed) as c_long, // _SC_PAGESIZE
        69 | 70 => -1,                                           // _SC_GETGR/GETPW_R_SIZE_MAX: no limit
        83 | 84 => online_cpus(),                                // _SC_NPROCESSORS_CONF/ONLN
        85 | 86 => {
            // _SC_PHYS_PAGES, _SC_AVPHYS_PAGES from sysinfo
            let mut si = [0u64; 14];
            if sc1(99, si.as_mut_ptr() as usize) < 0 {
                return -1;
            }
            let unit = (si[13] & 0xffff_ffff).max(1); // mem_unit
            let bytes = if name == 85 { si[4] } else { si[5] } * unit; // totalram, freeram
            (bytes / start::PAGE_SIZE.load(Ordering::Relaxed) as u64) as c_long
        }
        180 => 64, // _SC_HOST_NAME_MAX
        _ => {
            errno::set(errno::EINVAL);
            -1
        }
    }
}
