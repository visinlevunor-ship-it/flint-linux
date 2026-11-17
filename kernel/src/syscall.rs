//! Linux's x86-64 system calls, as far as the Flint kernel implements them. Anything else
//! answers ENOSYS (and says so once on the console), as Linux does for calls it lacks.

use crate::arch::{self, SyscallFrame};
use crate::cpio::{self, Node, S_IFDIR, S_IFMT};
use crate::mem::{self, P_WRITE, PAGE};
use crate::proc::{self, current, user_slice, user_str, user_u64, File, Kind};
use crate::kprintln;
use alloc::string::String;
use alloc::vec::Vec;

#[allow(dead_code)]
const EPERM: i64 = 1;
const ENOENT: i64 = 2;
#[allow(dead_code)]
const ESRCH: i64 = 3;
const EBADF: i64 = 9;
const ECHILD: i64 = 10;
const EAGAIN: i64 = 11;
const ENOMEM: i64 = 12;
#[allow(dead_code)]
const EFAULT: i64 = 14;
const ENOTDIR: i64 = 20;
const EISDIR: i64 = 21;
const EINVAL: i64 = 22;
const EMFILE: i64 = 24;
const ENOTTY: i64 = 25;
const ESPIPE: i64 = 29;
const EROFS: i64 = 30;
const ERANGE: i64 = 34;
const ENOSYS: i64 = 38;

const AT_FDCWD: i64 = -100;

// ---------------------------------------------------------------------------------------
// Time and randomness

static mut TSC_HZ: u64 = 2_000_000_000;
static mut BOOT_TSC: u64 = 0;
static mut SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Measure the TSC against the PIT (channel 2, ~10 ms).
pub fn init_time() {
    // SAFETY: the PIT and the speaker gate port; nothing else uses them.
    unsafe {
        use crate::console::{inb, outb};
        let gate = inb(0x61);
        outb(0x61, (gate & !0x02) | 0x01);
        outb(0x43, 0xB0); // channel 2, lobyte/hibyte, mode 0
        let count: u16 = 11932; // 1193182 Hz / 100
        outb(0x42, count as u8);
        outb(0x42, (count >> 8) as u8);
        let t0 = arch::rdtsc();
        let mut spins = 0u64;
        while inb(0x61) & 0x20 == 0 && spins < 100_000_000 {
            spins += 1;
        }
        let t1 = arch::rdtsc();
        outb(0x61, gate);
        if spins < 100_000_000 && t1 > t0 {
            TSC_HZ = (t1 - t0) * 100;
        }
        BOOT_TSC = t1;
        SEED ^= t1;
    }
}

pub fn tsc_hz() -> u64 {
    // SAFETY: set once at start-up.
    unsafe { TSC_HZ }
}

fn now_ns() -> u64 {
    // SAFETY: set once at start-up.
    let (hz, boot) = unsafe { (TSC_HZ, BOOT_TSC) };
    let t = arch::rdtsc().saturating_sub(boot);
    ((t as u128) * 1_000_000_000 / hz as u128) as u64
}

pub fn random_u64() -> u64 {
    // xorshift64*, stirred with the TSC
    // SAFETY: one CPU.
    unsafe {
        SEED ^= arch::rdtsc().rotate_left(17);
        SEED ^= SEED >> 12;
        SEED ^= SEED << 25;
        SEED ^= SEED >> 27;
        SEED.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

pub fn hwcap() -> u64 {
    let edx: u32;
    // SAFETY: cpuid leaf 1 exists on every x86-64 CPU. rbx is reserved by LLVM: saved by hand.
    unsafe {
        core::arch::asm!("push rbx", "cpuid", "pop rbx", inout("eax") 1u32 => _, out("ecx") _, out("edx") edx, options(preserves_flags));
    }
    edx as u64
}

// ---------------------------------------------------------------------------------------

fn fd_get(fd: u64) -> Result<&'static mut File, i64> {
    current().fds.get_mut(fd as usize).and_then(|f| f.as_mut()).ok_or(EBADF)
}

fn fd_new(f: File) -> Result<i64, i64> {
    let fds = &mut current().fds;
    for (i, slot) in fds.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(f);
            return Ok(i as i64);
        }
    }
    if fds.len() >= 1024 {
        return Err(EMFILE);
    }
    fds.push(Some(f));
    Ok(fds.len() as i64 - 1)
}

/// The canonical path for a path argument relative to `dirfd`.
fn at_path(dirfd: i64, path: &str, follow: bool) -> Result<String, i64> {
    let base = if path.starts_with('/') || dirfd == AT_FDCWD {
        current().cwd.clone()
    } else {
        match &fd_get(dirfd as u64)?.kind {
            Kind::Dir(d) => d.clone(),
            _ => return Err(ENOTDIR),
        }
    };
    cpio::resolve(&base, path, follow)
}

fn node(canon: &str) -> Result<&'static Node, i64> {
    cpio::files().get(canon).ok_or(ENOENT)
}

/// struct stat, x86-64 layout
fn write_stat(buf: u64, n: &Node, rdev: u64) -> Result<i64, i64> {
    let s = user_slice(buf, 144)?;
    s.fill(0);
    let size = n.data.len() as u64;
    let put = |s: &mut [u8], off: usize, v: u64, w: usize| s[off..off + w].copy_from_slice(&v.to_le_bytes()[..w]);
    put(s, 0, 1, 8); // st_dev
    put(s, 8, n.ino, 8);
    put(s, 16, if n.is_dir() { 2 } else { 1 }, 8); // nlink
    put(s, 24, n.mode as u64, 4);
    put(s, 40, rdev, 8);
    put(s, 48, size, 8);
    put(s, 56, 4096, 8); // blksize
    put(s, 64, size.div_ceil(512), 8);
    for off in [72, 88, 104] {
        put(s, off, n.mtime, 8);
    }
    Ok(0)
}

static CONSOLE_NODE: Node = Node { mode: 0o020620, ino: 3, mtime: 0, data: &[] };

fn open(dirfd: i64, path: u64, flags: u64) -> Result<i64, i64> {
    let path = user_str(path)?;
    if flags & 3 != 0 || flags & 0o100 != 0 {
        // O_WRONLY, O_RDWR, O_CREAT: the root filesystem is read-only, except the console
        if path == "/dev/console" || path == "/dev/tty" || path == "/dev/null" {
            return fd_new(File { kind: Kind::Console, pos: 0, flags: flags as u32, path });
        }
        return Err(EROFS);
    }
    if path == "/dev/console" || path == "/dev/tty" {
        return fd_new(File { kind: Kind::Console, pos: 0, flags: flags as u32, path });
    }
    let canon = at_path(dirfd, &path, flags & 0o400000 == 0)?;
    let n = node(&canon)?;
    let kind = if n.is_dir() {
        Kind::Dir(canon.clone())
    } else if flags & 0o200000 != 0 {
        return Err(ENOTDIR); // O_DIRECTORY
    } else if n.is_link() {
        return Err(40); // ELOOP with O_NOFOLLOW
    } else {
        Kind::File(n)
    };
    fd_new(File { kind, pos: 0, flags: flags as u32, path: alloc::format!("/{canon}") })
}

fn read(fd: u64, buf: u64, len: u64) -> Result<i64, i64> {
    let f = fd_get(fd)?;
    match &f.kind {
        Kind::Console => Ok(0), // no keyboard yet: end of input
        Kind::Dir(_) => Err(EISDIR),
        Kind::File(n) => {
            let data = n.data;
            let start = f.pos.min(data.len());
            let k = (data.len() - start).min(len as usize);
            let dst = user_slice(buf, k as u64)?;
            dst.copy_from_slice(&data[start..start + k]);
            f.pos += k;
            Ok(k as i64)
        }
    }
}

fn write(fd: u64, buf: u64, len: u64) -> Result<i64, i64> {
    let f = fd_get(fd)?;
    match f.kind {
        Kind::Console => {
            if f.path == "/dev/null" {
                return Ok(len as i64);
            }
            let s = user_slice(buf, len)?;
            crate::console::write_bytes(s);
            Ok(len as i64)
        }
        _ => Err(EBADF),
    }
}

fn iovecs(iov: u64, cnt: u64) -> Result<Vec<(u64, u64)>, i64> {
    if cnt > 1024 {
        return Err(EINVAL);
    }
    let mut v = Vec::new();
    for i in 0..cnt {
        v.push((user_u64(iov + i * 16)?, user_u64(iov + i * 16 + 8)?));
    }
    Ok(v)
}

fn getdents64(fd: u64, buf: u64, len: u64) -> Result<i64, i64> {
    let f = fd_get(fd)?;
    let Kind::Dir(dir) = &f.kind else { return Err(ENOTDIR) };
    let mut entries: Vec<(&str, u64, u8)> = alloc::vec![(".", 1, 4), ("..", 1, 4)];
    for (name, n) in cpio::list(dir) {
        let t = match n.mode & S_IFMT {
            S_IFDIR => 4,
            cpio::S_IFLNK => 10,
            _ => 8,
        };
        entries.push((name, n.ino, t));
    }
    let out = user_slice(buf, len)?;
    let mut used = 0usize;
    let mut i = f.pos;
    while i < entries.len() {
        let (name, ino, t) = entries[i];
        let reclen = (19 + name.len() + 1 + 7) & !7;
        if used + reclen > out.len() {
            if used == 0 {
                return Err(EINVAL);
            }
            break;
        }
        let r = &mut out[used..used + reclen];
        r.fill(0);
        r[0..8].copy_from_slice(&ino.to_le_bytes());
        r[8..16].copy_from_slice(&((i + 1) as u64).to_le_bytes());
        r[16..18].copy_from_slice(&(reclen as u16).to_le_bytes());
        r[18] = t;
        r[19..19 + name.len()].copy_from_slice(name.as_bytes());
        used += reclen;
        i += 1;
    }
    f.pos = i;
    Ok(used as i64)
}

fn mmap(addr: u64, len: u64, _prot: u64, flags: u64, fd: u64, off: u64) -> Result<i64, i64> {
    if len == 0 {
        return Err(EINVAL);
    }
    let len = (len + PAGE - 1) & !(PAGE - 1);
    let p = current();
    let fixed = flags & 0x10 != 0;
    let start = if fixed {
        if addr & (PAGE - 1) != 0 {
            return Err(EINVAL);
        }
        proc::unmap_range(p.pml4, addr, addr + len);
        addr
    } else {
        let a = p.mmap_top.checked_sub(len).ok_or(ENOMEM)?;
        p.mmap_top = a;
        a
    };
    proc::map_range(p.pml4, start, start + len, P_WRITE).map_err(|_| ENOMEM)?;
    if flags & 0x20 == 0 {
        // a file mapping: a private copy of the file's bytes
        let f = fd_get(fd)?;
        let Kind::File(n) = &f.kind else { return Err(EBADF) };
        let data = n.data;
        let o = (off as usize).min(data.len());
        let k = (data.len() - o).min(len as usize);
        // SAFETY: mapped just above, in the current address space.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr().add(o), start as *mut u8, k) };
    }
    Ok(start as i64)
}

fn brk(addr: u64) -> Result<i64, i64> {
    let p = current();
    if addr == 0 || addr < p.brk_start {
        return Ok(p.brk as i64);
    }
    if addr > p.brk && proc::map_range(p.pml4, p.brk, addr, P_WRITE).is_err() {
        return Ok(p.brk as i64);
    }
    p.brk = addr;
    Ok(addr as i64)
}

fn uname(buf: u64) -> Result<i64, i64> {
    let s = user_slice(buf, 65 * 6)?;
    s.fill(0);
    let fields: [&str; 6] = ["Flint", "flint", env!("CARGO_PKG_VERSION"), "#1 Flint kernel", "x86_64", "(none)"];
    for (i, v) in fields.iter().enumerate() {
        s[i * 65..i * 65 + v.len()].copy_from_slice(v.as_bytes());
    }
    Ok(0)
}

fn sleep_until(ns: u64) {
    while now_ns() < ns {
        core::hint::spin_loop();
    }
}

fn timespec(p: u64, ns: u64) -> Result<i64, i64> {
    let s = user_slice(p, 16)?;
    s[0..8].copy_from_slice(&(ns / 1_000_000_000).to_le_bytes());
    s[8..16].copy_from_slice(&(ns % 1_000_000_000).to_le_bytes());
    Ok(0)
}

fn read_timespec(p: u64) -> Result<u64, i64> {
    let sec = user_u64(p)?;
    let nsec = user_u64(p + 8)?;
    Ok(sec.saturating_mul(1_000_000_000).saturating_add(nsec))
}

/// Calls not implemented yet are reported once each.
static mut REPORTED: [u64; 8] = [0; 8];

fn report(nr: u64) {
    // SAFETY: one CPU.
    unsafe {
        if nr < 512 {
            let (w, b) = ((nr / 64) as usize, nr % 64);
            if REPORTED[w] & (1 << b) != 0 {
                return;
            }
            REPORTED[w] |= 1 << b;
        }
    }
    kprintln!("[kernel] {}: system call {} is not supported yet", proc::name(), nr);
}

fn handle(f: &mut SyscallFrame) -> Result<i64, i64> {
    let (a0, a1, a2, a3, a4, a5) = (f.rdi, f.rsi, f.rdx, f.r10, f.r8, f.r9);
    match f.rax {
        0 => read(a0, a1, a2),
        1 => write(a0, a1, a2),
        2 => open(AT_FDCWD, a0, a1),
        3 => {
            let fds = &mut current().fds;
            match fds.get_mut(a0 as usize) {
                Some(s @ Some(_)) => {
                    *s = None;
                    Ok(0)
                }
                _ => Err(EBADF),
            }
        }
        4 | 6 => {
            let canon = at_path(AT_FDCWD, &user_str(a0)?, f.rax == 4)?;
            write_stat(a1, node(&canon)?, 0)
        }
        5 => {
            let file = fd_get(a0)?;
            match &file.kind {
                Kind::Console => write_stat(a1, &CONSOLE_NODE, 0x0501),
                Kind::File(n) => write_stat(a1, n, 0),
                Kind::Dir(d) => write_stat(a1, node(&d.clone())?, 0),
            }
        }
        7 => Ok(0), // poll: nothing is ever ready to report
        8 => {
            let file = fd_get(a0)?;
            let size = match &file.kind {
                Kind::File(n) => n.data.len() as i64,
                Kind::Console => return Err(ESPIPE),
                Kind::Dir(_) => 0,
            };
            let base = match a2 {
                0 => 0,
                1 => file.pos as i64,
                2 => size,
                _ => return Err(EINVAL),
            };
            let pos = base + a1 as i64;
            if pos < 0 {
                return Err(EINVAL);
            }
            file.pos = pos as usize;
            Ok(pos)
        }
        9 => mmap(a0, a1, a2, a3, a4, a5),
        10 => Ok(0), // mprotect: every page is readable and writable for now
        11 => {
            if a0 & (PAGE - 1) != 0 {
                return Err(EINVAL);
            }
            proc::unmap_range(current().pml4, a0, a0 + a1);
            Ok(0)
        }
        12 => brk(a0),
        13 | 14 => {
            if f.rax == 14 && a2 != 0 {
                user_slice(a2, 8)?.fill(0);
            }
            Ok(0)
        } // rt_sigaction, rt_sigprocmask
        16 => match fd_get(a0)?.kind {
            Kind::Console => match a1 {
                0x5401 => {
                    // TCGETS: a terminal
                    if a2 != 0 {
                        user_slice(a2, 60)?.fill(0);
                    }
                    Ok(0)
                }
                0x5413 => {
                    // TIOCGWINSZ
                    let s = user_slice(a2, 8)?;
                    // SAFETY: read-only look at the console's size.
                    let (rows, cols) = unsafe { (*core::ptr::addr_of!(crate::console::FB)).as_ref().map(|fb| fb.size()).unwrap_or((25, 80)) };
                    s[0..2].copy_from_slice(&(rows as u16).to_le_bytes());
                    s[2..4].copy_from_slice(&(cols as u16).to_le_bytes());
                    s[4..8].fill(0);
                    Ok(0)
                }
                _ => Ok(0),
            },
            _ => Err(ENOTTY),
        },
        17 => {
            // pread64
            let file = fd_get(a0)?;
            let saved = file.pos;
            file.pos = a3 as usize;
            let r = read(a0, a1, a2);
            fd_get(a0)?.pos = saved;
            r
        }
        19 => {
            // readv
            let mut total = 0;
            for (b, l) in iovecs(a1, a2)? {
                let n = read(a0, b, l)?;
                total += n;
                if (n as u64) < l {
                    break;
                }
            }
            Ok(total)
        }
        20 => {
            // writev
            let mut total = 0;
            for (b, l) in iovecs(a1, a2)? {
                total += write(a0, b, l)?;
            }
            Ok(total)
        }
        21 => {
            at_path(AT_FDCWD, &user_str(a0)?, true)?;
            Ok(0)
        }
        24 => Ok(0), // sched_yield
        28 => Ok(0), // madvise
        32 => {
            let file = fd_get(a0)?.clone();
            fd_new(File { flags: file.flags & !0o2000000, ..file })
        }
        33 | 292 => {
            // dup2, dup3
            let file = fd_get(a0)?.clone();
            let fds = &mut current().fds;
            let n = a1 as usize;
            if n >= 1024 {
                return Err(EBADF);
            }
            while fds.len() <= n {
                fds.push(None);
            }
            let cloexec = if f.rax == 292 { (a2 as u32) & 0o2000000 } else { 0 };
            fds[n] = Some(File { flags: (file.flags & !0o2000000) | cloexec, ..file });
            Ok(n as i64)
        }
        35 | 230 => {
            // nanosleep, clock_nanosleep
            let req = if f.rax == 35 { a0 } else { a2 };
            let ns = read_timespec(req)?;
            let until = if f.rax == 230 && a1 & 1 != 0 { ns } else { now_ns() + ns };
            sleep_until(until);
            Ok(0)
        }
        39 | 186 => Ok(1), // getpid, gettid
        59 => {
            // execve
            let path = user_str(a0)?;
            let mut argv = Vec::new();
            let mut envp = Vec::new();
            for (list, out) in [(a1, &mut argv), (a2, &mut envp)] {
                if list == 0 {
                    continue;
                }
                for i in 0.. {
                    let p = user_u64(list + i * 8)?;
                    if p == 0 {
                        break;
                    }
                    out.push(user_str(p)?);
                    if i > 4096 {
                        return Err(7); // E2BIG
                    }
                }
            }
            let (entry, sp) = proc::replace(&path, &argv, &envp)?;
            // the new program starts with clean registers at its entry point
            *f = SyscallFrame { r15: 0, r14: 0, r13: 0, r12: 0, rbp: 0, rbx: 0, r9: 0, r8: 0, r10: 0, rdx: 0, rsi: 0, rdi: 0, rax: 0, rflags: 0x202 & !0x200 | 0x2, rip: entry, rsp: sp };
            // SAFETY: a new program has no thread pointer yet.
            unsafe { arch::wrmsr(arch::MSR_FS_BASE, 0) };
            Ok(0)
        }
        60 | 231 => proc::exit_current((a0 & 0xFF) as i64),
        61 => Err(ECHILD), // wait4: no children without fork
        62 | 200 | 234 => {
            // kill, tkill, tgkill: a program signalling itself ends (abort, for instance)
            let sig = if f.rax == 234 { a2 } else { a1 };
            if sig == 0 {
                return Ok(0);
            }
            kprintln!("[kernel] {}: killed by signal {}", proc::name(), sig);
            proc::exit_current(128 + sig as i64)
        }
        63 => uname(a0),
        72 => {
            // fcntl
            let file = fd_get(a0)?;
            match a1 {
                0 | 1030 => {
                    let mut copy = file.clone();
                    copy.flags = (copy.flags & !0o2000000) | if a1 == 1030 { 0o2000000 } else { 0 };
                    fd_new(copy)
                }
                1 => Ok(if file.flags & 0o2000000 != 0 { 1 } else { 0 }),
                2 => {
                    file.flags = (file.flags & !0o2000000) | if a2 & 1 != 0 { 0o2000000 } else { 0 };
                    Ok(0)
                }
                3 => Ok((file.flags & !0o2000000) as i64),
                4 => Ok(0),
                _ => Err(EINVAL),
            }
        }
        79 => {
            let cwd = alloc::format!("/{}", current().cwd);
            if cwd.len() + 1 > a1 as usize {
                return Err(ERANGE);
            }
            let s = user_slice(a0, cwd.len() as u64 + 1)?;
            s[..cwd.len()].copy_from_slice(cwd.as_bytes());
            s[cwd.len()] = 0;
            Ok(cwd.len() as i64 + 1)
        }
        80 => {
            let canon = at_path(AT_FDCWD, &user_str(a0)?, true)?;
            if !node(&canon)?.is_dir() {
                return Err(ENOTDIR);
            }
            current().cwd = canon;
            Ok(0)
        }
        81 => {
            let Kind::Dir(d) = &fd_get(a0)?.kind else { return Err(ENOTDIR) };
            current().cwd = d.clone();
            Ok(0)
        }
        89 | 267 => {
            // readlink, readlinkat
            let (dirfd, p, buf, len) = if f.rax == 89 { (AT_FDCWD, a0, a1, a2) } else { (a0 as i64, a1, a2, a3) };
            let path = user_str(p)?;
            if path == "/proc/self/exe" {
                let name = current().name.clone();
                let k = name.len().min(len as usize);
                user_slice(buf, k as u64)?.copy_from_slice(&name.as_bytes()[..k]);
                return Ok(k as i64);
            }
            let canon = at_path(dirfd, &path, false)?;
            let n = node(&canon)?;
            if !n.is_link() {
                return Err(EINVAL);
            }
            let k = n.data.len().min(len as usize);
            user_slice(buf, k as u64)?.copy_from_slice(&n.data[..k]);
            Ok(k as i64)
        }
        95 => Ok(0o022), // umask
        96 => {
            // gettimeofday: no clock yet; time since boot
            if a0 != 0 {
                let ns = now_ns();
                let s = user_slice(a0, 16)?;
                s[0..8].copy_from_slice(&(ns / 1_000_000_000).to_le_bytes());
                s[8..16].copy_from_slice(&((ns % 1_000_000_000) / 1000).to_le_bytes());
            }
            Ok(0)
        }
        97 | 302 => {
            // getrlimit, prlimit64: the stack limit, the rest unlimited
            let (res, old) = if f.rax == 97 { (a0, a1) } else { (a1, a3) };
            if old != 0 {
                let s = user_slice(old, 16)?;
                let cur = if res == 3 { proc::stack_limit() } else if res == 7 { 1024 } else { u64::MAX };
                let max = if res == 7 { 4096 } else { u64::MAX };
                s[0..8].copy_from_slice(&cur.to_le_bytes());
                s[8..16].copy_from_slice(&max.to_le_bytes());
            }
            Ok(0)
        }
        99 => {
            // sysinfo
            let s = user_slice(a0, 112)?;
            s.fill(0);
            // SAFETY: counters read on one CPU.
            let (total, used) = unsafe { (mem::TOTAL_PAGES * PAGE, mem::USED_PAGES * PAGE) };
            s[0..8].copy_from_slice(&(now_ns() / 1_000_000_000).to_le_bytes());
            s[32..40].copy_from_slice(&total.to_le_bytes());
            s[40..48].copy_from_slice(&(total - used.min(total)).to_le_bytes());
            s[80..82].copy_from_slice(&1u16.to_le_bytes());
            s[104..108].copy_from_slice(&1u32.to_le_bytes());
            Ok(0)
        }
        102 | 104 | 107 | 108 => Ok(0), // getuid, getgid, geteuid, getegid: root
        105 | 106 => Ok(0),             // setuid, setgid
        110 => Ok(0),                    // getppid
        111 | 112 => Ok(1),              // getpgrp, setsid
        109 | 121 | 124 => Ok(1),        // setpgid, getpgid, getsid
        131 => {
            if a1 != 0 {
                let s = user_slice(a1, 24)?;
                s.fill(0);
                s[8..12].copy_from_slice(&2u32.to_le_bytes()); // SS_DISABLE
            }
            Ok(0)
        } // sigaltstack
        157 => Ok(0), // prctl
        158 => {
            // arch_prctl
            match a0 {
                0x1002 => {
                    // SAFETY: setting the program's thread pointer.
                    unsafe { arch::wrmsr(arch::MSR_FS_BASE, a1) };
                    Ok(0)
                }
                0x1003 => {
                    // SAFETY: reading it.
                    let v = unsafe { arch::rdmsr(arch::MSR_FS_BASE) };
                    user_slice(a1, 8)?.copy_from_slice(&v.to_le_bytes());
                    Ok(0)
                }
                _ => Err(EINVAL),
            }
        }
        201 => {
            let t = (now_ns() / 1_000_000_000) as i64;
            if a0 != 0 {
                user_slice(a0, 8)?.copy_from_slice(&t.to_le_bytes());
            }
            Ok(t)
        }
        202 => {
            // futex: one thread, so a wait could only wait forever
            match a1 & 0x7F {
                1 => Ok(0),
                0 | 9 => Err(EAGAIN),
                _ => Ok(0),
            }
        }
        204 => {
            // sched_getaffinity: CPU 0
            if a1 < 8 {
                return Err(EINVAL);
            }
            let s = user_slice(a2, 8)?;
            s.copy_from_slice(&1u64.to_le_bytes());
            Ok(8)
        }
        217 => getdents64(a0, a1, a2),
        218 => {
            current().tid_address = a0;
            Ok(1)
        }
        228 => timespec(a1, now_ns()), // clock_gettime: every clock counts from boot for now
        229 => timespec(a1, 1),        // clock_getres
        257 => open(a0 as i64, a1, a2),
        262 => {
            // newfstatat
            let path = user_str(a1)?;
            if path.is_empty() && a3 & 0x1000 != 0 {
                let file = fd_get(a0)?;
                return match &file.kind {
                    Kind::Console => write_stat(a2, &CONSOLE_NODE, 0x0501),
                    Kind::File(n) => write_stat(a2, n, 0),
                    Kind::Dir(d) => write_stat(a2, node(&d.clone())?, 0),
                };
            }
            let canon = at_path(a0 as i64, &path, a3 & 0x100 == 0)?;
            write_stat(a2, node(&canon)?, 0)
        }
        269 | 439 => {
            at_path(a0 as i64, &user_str(a1)?, true)?;
            Ok(0)
        } // faccessat, faccessat2
        273 => Ok(0), // set_robust_list
        318 => {
            // getrandom
            let s = user_slice(a0, a1)?;
            for chunk in s.chunks_mut(8) {
                let r = random_u64().to_le_bytes();
                chunk.copy_from_slice(&r[..chunk.len()]);
            }
            Ok(a1 as i64)
        }
        40 => {
            // sendfile(out, in, offset, count): from a file to the console
            let (data, start) = {
                let src = fd_get(a1)?;
                let Kind::File(n) = &src.kind else { return Err(EINVAL) };
                let start = if a2 != 0 { user_u64(a2)? as usize } else { src.pos };
                (n.data, start.min(n.data.len()))
            };
            let k = (data.len() - start).min(a3 as usize);
            let out = fd_get(a0)?;
            let Kind::Console = out.kind else { return Err(EINVAL) };
            if out.path != "/dev/null" {
                crate::console::write_bytes(&data[start..start + k]);
            }
            if a2 != 0 {
                user_slice(a2, 8)?.copy_from_slice(&((start + k) as u64).to_le_bytes());
            } else {
                fd_get(a1)?.pos = start + k;
            }
            Ok(k as i64)
        }
        // statx, rseq, clone3: missing on purpose; the C library falls back
        332 | 334 | 435 => Err(ENOSYS),
        // things that change files: the root filesystem is read-only
        82 | 83 | 84 | 86 | 87 | 88 | 90 | 92 | 258 | 263 | 264 | 265 | 266 | 268 | 316 => Err(EROFS),
        nr => {
            report(nr);
            Err(ENOSYS)
        }
    }
}

/// From the `syscall` entry: the program's registers; the result goes back in rax.
pub extern "sysv64" fn dispatch(f: &mut SyscallFrame) {
    let r = match handle(f) {
        Ok(v) => v,
        Err(e) => -e,
    };
    f.rax = r as u64;
}
