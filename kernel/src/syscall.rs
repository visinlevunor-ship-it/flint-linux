//! Linux's x86-64 system calls, as far as the Flint kernel implements them. Anything else
//! answers ENOSYS (and is named once on the console), as Linux does for calls it lacks.

use crate::arch::{self, SyscallFrame};
use crate::cpio::{S_IFDIR, S_IFLNK, S_IFMT};
use crate::mem::{self, P_WRITE, PAGE};
use crate::proc::{self, current, user_slice, user_str, user_u64};
use crate::vfs::{self, Node, S_IFCHR, S_IFIFO};
use crate::{kprintln, tty};
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

pub(crate) const ENOENT: i64 = 2;
pub(crate) const ESRCH: i64 = 3;
pub(crate) const EBADF: i64 = 9;
pub(crate) const EAGAIN: i64 = 11;
pub(crate) const ENOMEM: i64 = 12;
pub(crate) const EACCES: i64 = 13;
pub(crate) const EEXIST: i64 = 17;
pub(crate) const ENOTDIR: i64 = 20;
pub(crate) const EISDIR: i64 = 21;
pub(crate) const EINVAL: i64 = 22;
pub(crate) const EMFILE: i64 = 24;
pub(crate) const ENOTTY: i64 = 25;
pub(crate) const ENOSPC: i64 = 28;
pub(crate) const ESPIPE: i64 = 29;
pub(crate) const EPIPE: i64 = 32;
pub(crate) const ERANGE: i64 = 34;
pub(crate) const ENOSYS: i64 = 38;
pub(crate) const EPERM: i64 = 1;
pub(crate) const ENXIO: i64 = 6;
pub(crate) const EAFNOSUPPORT: i64 = 97;

pub(crate) const AT_FDCWD: i64 = -100;
pub(crate) const O_ACCMODE: u32 = 3;
pub(crate) const O_CREAT: u32 = 0o100;
pub(crate) const O_EXCL: u32 = 0o200;
pub(crate) const O_TRUNC: u32 = 0o1000;
pub(crate) const O_APPEND: u32 = 0o2000;
pub(crate) const O_NONBLOCK: u32 = 0o4000;
pub(crate) const O_DIRECTORY: u32 = 0o200000;
pub(crate) const O_NOFOLLOW: u32 = 0o400000;
pub(crate) const O_CLOEXEC: u32 = 0o2000000;

// ---------------------------------------------------------------------------------------
// Time and randomness

static mut TSC_HZ: u64 = 2_000_000_000;
static mut BOOT_TSC: u64 = 0;
static mut SEED: u64 = 0x9E37_79B9_7F4A_7C15;
/// seconds since 1970 at boot, from the real-time clock
static mut BOOT_EPOCH: u64 = 0;

/// Measure the TSC against the PIT (channel 2, ~10 ms), and read the real-time clock.
pub fn init_time() {
    // SAFETY: the PIT, the speaker gate port and the CMOS clock; nothing else uses them yet.
    unsafe {
        use crate::console::{inb, outb};
        let gate = inb(0x61);
        outb(0x61, (gate & !0x02) | 0x01);
        outb(0x43, 0xB0);
        let count: u16 = 11932;
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
        BOOT_EPOCH = read_rtc();
    }
}

unsafe fn cmos(reg: u8) -> u8 {
    use crate::console::{inb, outb};
    outb(0x70, reg);
    inb(0x71)
}

/// The CMOS real-time clock as seconds since 1970 (UTC, as PCs usually keep it for Linux).
unsafe fn read_rtc() -> u64 {
    let mut spins = 0;
    while cmos(0x0A) & 0x80 != 0 && spins < 1_000_000 {
        spins += 1;
    }
    let b = cmos(0x0B);
    let bcd = |v: u8| if b & 4 == 0 { (v & 0x0F) + (v >> 4) * 10 } else { v };
    let sec = bcd(cmos(0x00)) as u64;
    let min = bcd(cmos(0x02)) as u64;
    let mut hour = cmos(0x04);
    let pm = hour & 0x80 != 0;
    hour = bcd(hour & 0x7F);
    if b & 2 == 0 && pm {
        hour = (hour % 12) + 12;
    }
    let day = bcd(cmos(0x07)) as u64;
    let mon = bcd(cmos(0x08)) as u64;
    let year = 2000 + bcd(cmos(0x09)) as u64;
    if !(1..=12).contains(&mon) || day == 0 {
        return 0;
    }
    // days from 1970-01-01
    let y = if mon <= 2 { year - 1 } else { year };
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = (mon + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    days * 86400 + hour as u64 * 3600 + min * 60 + sec
}

pub fn tsc_hz() -> u64 {
    // SAFETY: set once at start-up.
    unsafe { TSC_HZ }
}

pub fn now_ns() -> u64 {
    // SAFETY: set once at start-up.
    let (hz, boot) = unsafe { (TSC_HZ, BOOT_TSC) };
    let t = arch::rdtsc().saturating_sub(boot);
    ((t as u128) * 1_000_000_000 / hz as u128) as u64
}

fn realtime_ns() -> u64 {
    // SAFETY: set once at start-up.
    unsafe { BOOT_EPOCH * 1_000_000_000 + now_ns() }
}

pub fn wall_seconds() -> u64 {
    realtime_ns() / 1_000_000_000
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
// Open files

const PIPE_CAP: usize = 64 * 1024;

pub struct Pipe {
    buf: VecDeque<u8>,
    readers: usize,
    writers: usize,
}

pub enum Kind {
    Tty,
    Null,
    Zero,
    Full,
    Random,
    File(Node),
    Dir(String),
    PipeR(Rc<RefCell<Pipe>>),
    PipeW(Rc<RefCell<Pipe>>),
    Sock(usize),
    Unix(Rc<RefCell<crate::sock::Unix>>),
}

/// An open file: shared by the descriptors dup and fork make, like Linux's struct file.
pub struct OpenFile {
    pub kind: Kind,
    pub pos: usize,
    pub flags: u32,
    pub path: String,
}

impl OpenFile {
    pub fn console() -> OpenFile {
        OpenFile { kind: Kind::Tty, pos: 0, flags: 2, path: "/dev/console".to_string() }
    }
}

impl Drop for OpenFile {
    fn drop(&mut self) {
        match &self.kind {
            Kind::PipeR(p) => p.borrow_mut().readers -= 1,
            Kind::PipeW(p) => p.borrow_mut().writers -= 1,
            Kind::Sock(i) => crate::sock::closed(*i),
            _ => return,
        }
        proc::wake_all();
    }
}

#[derive(Clone)]
pub struct Fd {
    pub file: Rc<RefCell<OpenFile>>,
    pub cloexec: bool,
}

pub(crate) fn fd_file(fd: u64) -> Result<Rc<RefCell<OpenFile>>, i64> {
    current().fds.borrow().get(fd as usize).and_then(|f| f.as_ref()).map(|f| f.file.clone()).ok_or(EBADF)
}

pub(crate) fn fd_install(file: OpenFile, cloexec: bool) -> Result<i64, i64> {
    fd_install_rc(Rc::new(RefCell::new(file)), cloexec, 0)
}

pub(crate) fn fd_install_rc(file: Rc<RefCell<OpenFile>>, cloexec: bool, min: usize) -> Result<i64, i64> {
    let table = current().fds.clone();
    let mut fds = table.borrow_mut();
    for (i, slot) in fds.iter_mut().enumerate().skip(min) {
        if slot.is_none() {
            *slot = Some(Fd { file, cloexec });
            return Ok(i as i64);
        }
    }
    let i = fds.len().max(min);
    if i >= 1024 {
        return Err(EMFILE);
    }
    while fds.len() < i {
        fds.push(None);
    }
    fds.push(Some(Fd { file, cloexec }));
    Ok(i as i64)
}

/// The canonical path for a path argument relative to `dirfd`.
fn base_dir(dirfd: i64, path: &str) -> Result<String, i64> {
    if path.starts_with('/') || dirfd == AT_FDCWD {
        return Ok(current().cwd.borrow().clone());
    }
    match &fd_file(dirfd as u64)?.borrow().kind {
        Kind::Dir(d) => Ok(d.clone()),
        _ => Err(ENOTDIR),
    }
}

fn at_path(dirfd: i64, path: &str, follow: bool) -> Result<String, i64> {
    let base = base_dir(dirfd, path)?;
    vfs::resolve(&base, path, follow)
}

fn at_new_path(dirfd: i64, path: &str) -> Result<String, i64> {
    let base = base_dir(dirfd, path)?;
    vfs::resolve_parent(&base, path)
}

fn node(canon: &str) -> Result<Node, i64> {
    vfs::lookup(canon).ok_or(ENOENT)
}

/// struct stat, x86-64 layout
fn put_stat(buf: u64, mode: u32, ino: u64, size: u64, mtime: u64, rdev: u64) -> Result<i64, i64> {
    let s = user_slice(buf, 144)?;
    s.fill(0);
    let put = |s: &mut [u8], off: usize, v: u64, w: usize| s[off..off + w].copy_from_slice(&v.to_le_bytes()[..w]);
    put(s, 0, 1, 8);
    put(s, 8, ino, 8);
    put(s, 16, if mode & S_IFMT == S_IFDIR { 2 } else { 1 }, 8);
    put(s, 24, mode as u64, 4);
    put(s, 40, rdev, 8);
    put(s, 48, size, 8);
    put(s, 56, 4096, 8);
    put(s, 64, size.div_ceil(512), 8);
    for off in [72, 88, 104] {
        put(s, off, mtime, 8);
    }
    Ok(0)
}

fn stat_node(buf: u64, n: &Node) -> Result<i64, i64> {
    put_stat(buf, n.mode(), n.ino(), n.size() as u64, n.mtime(), n.rdev())
}

fn stat_open(buf: u64, f: &OpenFile) -> Result<i64, i64> {
    match &f.kind {
        Kind::Tty => put_stat(buf, S_IFCHR | 0o620, 3, 0, 0, 0x0501),
        Kind::Null | Kind::Zero | Kind::Full | Kind::Random => put_stat(buf, S_IFCHR | 0o666, 4, 0, 0, 0x0103),
        Kind::File(n) => stat_node(buf, n),
        Kind::Dir(d) => stat_node(buf, &node(d)?),
        Kind::PipeR(_) | Kind::PipeW(_) => put_stat(buf, S_IFIFO | 0o600, 5, 0, 0, 0),
        Kind::Sock(_) | Kind::Unix(_) => put_stat(buf, 0o140000 | 0o777, 6, 0, 0, 0),
    }
}

fn device(canon: &str) -> Option<Kind> {
    Some(match canon {
        "dev/null" => Kind::Null,
        "dev/zero" => Kind::Zero,
        "dev/full" => Kind::Full,
        "dev/random" | "dev/urandom" => Kind::Random,
        "dev/tty" | "dev/console" | "dev/tty0" | "dev/tty1" => Kind::Tty,
        _ => return None,
    })
}

fn open(dirfd: i64, path: u64, flags: u64, mode: u64) -> Result<i64, i64> {
    let path = user_str(path)?;
    let flags = flags as u32;
    let acc = flags & O_ACCMODE;
    let write = acc != 0;
    let follow = flags & O_NOFOLLOW == 0;
    let canon = match at_path(dirfd, &path, follow) {
        Ok(c) => c,
        Err(ENOENT) if flags & O_CREAT != 0 => {
            let mut c = at_new_path(dirfd, &path)?;
            // a dangling symbolic link: create what it points to, as Linux does
            let mut hops = 0;
            while follow && flags & O_EXCL == 0 {
                let Some(n) = vfs::lookup(&c) else { break };
                if !n.is_link() || hops > 40 {
                    break;
                }
                hops += 1;
                let dir = alloc::format!("/{}", c.rsplit_once('/').map(|x| x.0).unwrap_or(""));
                c = vfs::resolve_parent(&dir, &n.link_target())?;
            }
            c
        }
        Err(e) => return Err(e),
    };
    let cloexec = flags & O_CLOEXEC != 0;
    let disp = alloc::format!("/{canon}");
    if let Some(kind) = device(&canon) {
        return fd_install(OpenFile { kind, pos: 0, flags, path: disp }, cloexec);
    }
    let existing = vfs::lookup(&canon);
    if let Some(n) = &existing {
        if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
            return Err(EEXIST);
        }
        if n.is_dir() {
            if write {
                return Err(EISDIR);
            }
            return fd_install(OpenFile { kind: Kind::Dir(canon), pos: 0, flags, path: disp }, cloexec);
        }
        if n.is_link() {
            return Err(40); // ELOOP: O_NOFOLLOW on a symbolic link
        }
        if n.mode() & S_IFMT == S_IFIFO {
            return Err(ENXIO);
        }
    } else if flags & O_DIRECTORY != 0 {
        return Err(ENOENT);
    }
    if flags & O_DIRECTORY != 0 {
        return Err(ENOTDIR);
    }
    let node = if write || flags & (O_CREAT | O_TRUNC) != 0 {
        let umask = current().umask;
        let m = vfs::writable(&canon, flags & O_CREAT != 0, (mode as u32) & !umask)?;
        if flags & O_TRUNC != 0 {
            m.borrow_mut().data.clear();
        }
        Node::Mem(m)
    } else {
        existing.ok_or(ENOENT)?
    };
    fd_install(OpenFile { kind: Kind::File(node), pos: 0, flags, path: disp }, cloexec)
}

pub(crate) fn nonblock(f: &Rc<RefCell<OpenFile>>) -> bool {
    f.borrow().flags & O_NONBLOCK != 0
}

fn read(fd: u64, buf: u64, len: u64) -> Result<i64, i64> {
    let file = fd_file(fd)?;
    if let Some(r) = crate::sock::read(&file, buf, len) {
        return r;
    }
    if file.borrow().flags & O_ACCMODE == 1 {
        return Err(EBADF);
    }
    loop {
        let out = user_slice(buf, len)?;
        let mut f = file.borrow_mut();
        let pos = f.pos;
        match &f.kind {
            Kind::Tty => {
                if let Some(n) = tty::try_read(out) {
                    return Ok(n as i64);
                }
            }
            Kind::PipeR(p) => {
                let mut p = p.borrow_mut();
                if !p.buf.is_empty() {
                    let k = p.buf.len().min(out.len());
                    for o in out.iter_mut().take(k) {
                        *o = p.buf.pop_front().unwrap();
                    }
                    drop(p);
                    proc::wake_all();
                    return Ok(k as i64);
                }
                if p.writers == 0 {
                    return Ok(0);
                }
            }
            Kind::File(n) => {
                let k = n.read_at(pos, out);
                f.pos += k;
                return Ok(k as i64);
            }
            Kind::Dir(_) => return Err(EISDIR),
            Kind::Null => return Ok(0),
            Kind::Zero | Kind::Full => {
                out.fill(0);
                return Ok(len as i64);
            }
            Kind::Random => {
                fill_random(out);
                return Ok(len as i64);
            }
            Kind::PipeW(_) | Kind::Sock(_) | Kind::Unix(_) => return Err(EBADF),
        }
        drop(f);
        if nonblock(&file) {
            return Err(EAGAIN);
        }
        if proc::block() {
            return Err(proc::ERESTART);
        }
    }
}

fn fill_random(out: &mut [u8]) {
    for chunk in out.chunks_mut(8) {
        let r = random_u64().to_le_bytes();
        chunk.copy_from_slice(&r[..chunk.len()]);
    }
}

fn write(fd: u64, buf: u64, len: u64) -> Result<i64, i64> {
    let file = fd_file(fd)?;
    if let Some(r) = crate::sock::write(&file, buf, len) {
        return r;
    }
    if file.borrow().flags & O_ACCMODE == 0 && !matches!(file.borrow().kind, Kind::Tty | Kind::PipeW(_)) {
        return Err(EBADF);
    }
    let mut done = 0u64;
    loop {
        let data = user_slice(buf + done, len - done)?;
        let mut f = file.borrow_mut();
        let append = f.flags & O_APPEND != 0;
        let pos = f.pos;
        match &f.kind {
            Kind::Tty => {
                crate::console::write_bytes(data);
                return Ok(len as i64);
            }
            Kind::Null | Kind::Zero | Kind::Random => return Ok(len as i64),
            Kind::Full => return Err(ENOSPC),
            Kind::File(Node::Mem(m)) => {
                let mut m = m.borrow_mut();
                let at = if append { m.data.len() } else { pos };
                if m.data.len() < at + data.len() {
                    m.data.resize(at + data.len(), 0);
                }
                m.data[at..at + data.len()].copy_from_slice(data);
                m.mtime = vfs::now();
                vfs::mark_dirty();
                drop(m);
                f.pos = at + data.len();
                return Ok(len as i64);
            }
            Kind::File(Node::Base(_) | Node::Ext(_)) | Kind::Dir(_) | Kind::PipeR(_) | Kind::Sock(_) | Kind::Unix(_) => return Err(EBADF),
            Kind::PipeW(p) => {
                let mut p = p.borrow_mut();
                if p.readers == 0 {
                    drop(p);
                    drop(f);
                    proc::signal(proc::pid(), 13); // SIGPIPE
                    return Err(EPIPE);
                }
                let room = PIPE_CAP - p.buf.len();
                if room > 0 {
                    let k = room.min(data.len());
                    p.buf.extend(&data[..k]);
                    done += k as u64;
                    drop(p);
                    proc::wake_all();
                    if done == len {
                        return Ok(len as i64);
                    }
                    continue;
                }
            }
        }
        drop(f);
        if nonblock(&file) {
            return if done > 0 { Ok(done as i64) } else { Err(EAGAIN) };
        }
        if proc::block() {
            return if done > 0 { Ok(done as i64) } else { Err(proc::ERESTART) };
        }
    }
}

pub(crate) fn iovecs(iov: u64, cnt: u64) -> Result<Vec<(u64, u64)>, i64> {
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
    let file = fd_file(fd)?;
    let mut f = file.borrow_mut();
    let Kind::Dir(dir) = &f.kind else { return Err(ENOTDIR) };
    let mut entries: Vec<(String, u64, u8)> = alloc::vec![(".".into(), 1, 4), ("..".into(), 1, 4)];
    for (name, n) in vfs::list(dir) {
        let t = match n.mode() & S_IFMT {
            S_IFDIR => 4,
            S_IFLNK => 10,
            S_IFCHR => 2,
            S_IFIFO => 1,
            _ => 8,
        };
        entries.push((name, n.ino(), t));
    }
    let out = user_slice(buf, len)?;
    let mut used = 0usize;
    let mut i = f.pos;
    while i < entries.len() {
        let (name, ino, t) = &entries[i];
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
        r[18] = *t;
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
    let mm = current().mm.clone();
    let mut m = mm.borrow_mut();
    let start = if flags & 0x10 != 0 {
        if addr & (PAGE - 1) != 0 {
            return Err(EINVAL);
        }
        proc::unmap_range(m.pml4, addr, addr + len);
        addr
    } else {
        let a = m.mmap_top.checked_sub(len).ok_or(ENOMEM)?;
        m.mmap_top = a;
        a
    };
    proc::map_range(m.pml4, start, start + len, P_WRITE).map_err(|_| ENOMEM)?;
    drop(m);
    if flags & 0x20 == 0 {
        // a file mapping: a private copy of the file's bytes
        let file = fd_file(fd)?;
        let f = file.borrow();
        let Kind::File(n) = &f.kind else { return Err(EACCES) };
        // SAFETY: mapped just above, in the current address space.
        let dst = unsafe { core::slice::from_raw_parts_mut(start as *mut u8, len as usize) };
        n.read_at(off as usize, dst);
    }
    Ok(start as i64)
}

fn brk(addr: u64) -> Result<i64, i64> {
    let mm = current().mm.clone();
    let mut m = mm.borrow_mut();
    if addr == 0 || addr < m.brk_start {
        return Ok(m.brk as i64);
    }
    if addr > m.brk && proc::map_range(m.pml4, m.brk, addr, P_WRITE).is_err() {
        return Ok(m.brk as i64);
    }
    m.brk = addr;
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

fn sleep_until(ns: u64) -> Result<i64, i64> {
    while now_ns() < ns {
        if proc::block() {
            // the timer wakes everyone every tick; a signal ends the sleep
            return Err(proc::EINTR);
        }
    }
    Ok(0)
}

// poll: is the file ready for reading / writing?
fn ready(file: &Rc<RefCell<OpenFile>>) -> (bool, bool, bool) {
    let f = file.borrow();
    if let Some(r) = crate::sock::ready(&f.kind) {
        return r;
    }
    match &f.kind {
        Kind::Tty => (tty::readable(), true, false),
        Kind::PipeR(p) => {
            let p = p.borrow();
            (!p.buf.is_empty() || p.writers == 0, false, p.writers == 0)
        }
        Kind::PipeW(p) => {
            let p = p.borrow();
            (false, p.buf.len() < PIPE_CAP || p.readers == 0, p.readers == 0)
        }
        _ => (true, true, false),
    }
}

fn poll(fds: u64, n: u64, timeout_ms: i64) -> Result<i64, i64> {
    if n > 1024 {
        return Err(EINVAL);
    }
    let deadline = if timeout_ms < 0 { u64::MAX } else { now_ns() + timeout_ms as u64 * 1_000_000 };
    loop {
        let list = user_slice(fds, n * 8)?;
        let mut count = 0;
        for i in 0..n as usize {
            let e = &mut list[i * 8..i * 8 + 8];
            let fd = i32::from_le_bytes(e[0..4].try_into().unwrap());
            let events = u16::from_le_bytes(e[4..6].try_into().unwrap());
            let mut rev: u16 = 0;
            if fd >= 0 {
                match fd_file(fd as u64) {
                    Ok(f) => {
                        let (r, w, hup) = ready(&f);
                        if r && events & 0x1 != 0 {
                            rev |= 0x1;
                        }
                        if w && events & 0x4 != 0 {
                            rev |= 0x4;
                        }
                        if hup {
                            rev |= 0x10;
                        }
                    }
                    Err(_) => rev = 0x20, // POLLNVAL
                }
            }
            e[6..8].copy_from_slice(&rev.to_le_bytes());
            if rev != 0 {
                count += 1;
            }
        }
        if count > 0 || now_ns() >= deadline {
            return Ok(count);
        }
        if proc::block() {
            return Err(proc::ERESTART);
        }
    }
}

fn select(n: u64, rd: u64, wr: u64, ex: u64, timeout_ns: Option<u64>) -> Result<i64, i64> {
    let n = n.min(1024);
    let bytes = n.div_ceil(64) * 8;
    let get = |p: u64| -> Result<Vec<u8>, i64> { if p == 0 { Ok(Vec::new()) } else { Ok(user_slice(p, bytes)?.to_vec()) } };
    let (rin, win) = (get(rd)?, get(wr)?);
    let deadline = timeout_ns.map(|t| now_ns() + t).unwrap_or(u64::MAX);
    loop {
        let mut rout = alloc::vec![0u8; bytes as usize];
        let mut wout = alloc::vec![0u8; bytes as usize];
        let mut count = 0;
        for fd in 0..n as usize {
            let (bit, byte) = (1u8 << (fd % 8), fd / 8);
            let want_r = rin.get(byte).is_some_and(|b| b & bit != 0);
            let want_w = win.get(byte).is_some_and(|b| b & bit != 0);
            if !want_r && !want_w {
                continue;
            }
            let f = fd_file(fd as u64)?;
            let (r, w, _) = ready(&f);
            if want_r && r {
                rout[byte] |= bit;
                count += 1;
            }
            if want_w && w {
                wout[byte] |= bit;
                count += 1;
            }
        }
        if count > 0 || now_ns() >= deadline {
            if rd != 0 {
                user_slice(rd, bytes)?.copy_from_slice(&rout);
            }
            if wr != 0 {
                user_slice(wr, bytes)?.copy_from_slice(&wout);
            }
            if ex != 0 {
                user_slice(ex, bytes)?.fill(0);
            }
            return Ok(count);
        }
        if proc::block() {
            return Err(proc::ERESTART);
        }
    }
}

fn pipe(fds: u64, flags: u64) -> Result<i64, i64> {
    let p = Rc::new(RefCell::new(Pipe { buf: VecDeque::new(), readers: 1, writers: 1 }));
    let cloexec = flags as u32 & O_CLOEXEC != 0;
    let fl = flags as u32 & O_NONBLOCK;
    let r = fd_install(OpenFile { kind: Kind::PipeR(p.clone()), pos: 0, flags: fl, path: "pipe".into() }, cloexec)?;
    let w = fd_install(OpenFile { kind: Kind::PipeW(p), pos: 0, flags: fl | 1, path: "pipe".into() }, cloexec)?;
    let s = user_slice(fds, 8)?;
    s[0..4].copy_from_slice(&(r as i32).to_le_bytes());
    s[4..8].copy_from_slice(&(w as i32).to_le_bytes());
    Ok(0)
}

fn kill(pid: i64, sig: u64) -> Result<i64, i64> {
    let sig = sig as i32;
    if !(0..=64).contains(&sig) {
        return Err(EINVAL);
    }
    let me = proc::pid();
    let found = match pid {
        p if p > 0 => proc::signal(p as i32, sig),
        0 => proc::signal_group(current().pgid, sig) > 0,
        -1 => {
            let mut any = false;
            for p in proc::all_pids() {
                if p != 1 && p != me {
                    any |= proc::signal(p, sig);
                }
            }
            any
        }
        p => proc::signal_group(-p as i32, sig) > 0,
    };
    if !found {
        return Err(ESRCH);
    }
    Ok(0)
}

/// The interface requests of the old ioctl interface (ifconfig and friends): lo and eth0.
fn net_ioctl(cmd: u64, arg: u64) -> Result<i64, i64> {
    let (ip, mask, mac, card, up) = crate::net::with(|n| (n.ip, n.mask, n.mac(), n.has_card(), n.link_up()));
    let put_addr = |at: &mut [u8], fam: u16, a: u32| {
        at[..16].fill(0);
        at[0..2].copy_from_slice(&fam.to_le_bytes());
        at[4..8].copy_from_slice(&a.to_be_bytes());
    };
    if cmd == 0x8912 {
        // SIOCGIFCONF
        let len = user_slice(arg, 16)?;
        let room = i32::from_le_bytes(len[0..4].try_into().unwrap()).max(0) as usize;
        let p = u64::from_le_bytes(len[8..16].try_into().unwrap());
        let mut list: Vec<(&str, u32)> = alloc::vec![("lo", 0x7F00_0001)];
        if card && ip != 0 {
            list.push(("eth0", ip));
        }
        let mut used = 0;
        for (name, a) in list {
            if used + 40 > room || p == 0 {
                break;
            }
            let r = user_slice(p + used as u64, 40)?;
            r.fill(0);
            r[..name.len()].copy_from_slice(name.as_bytes());
            put_addr(&mut r[16..32], 2, a);
            used += 40;
        }
        user_slice(arg, 4)?.copy_from_slice(&(used as i32).to_le_bytes());
        return Ok(0);
    }
    let r = user_slice(arg, 40)?;
    let end = r[..16].iter().position(|&c| c == 0).unwrap_or(16);
    let lo = &r[..end] == b"lo";
    if !lo && (&r[..end] != b"eth0" || !card) {
        return Err(ENXIO);
    }
    match cmd {
        0x8913 => {
            // SIOCGIFFLAGS: UP, BROADCAST/LOOPBACK, RUNNING
            let f: u16 = if lo { 0x1 | 0x8 | 0x40 } else { 0x1 | 0x2 | 0x1000 | if up { 0x40 } else { 0 } };
            r[16..18].copy_from_slice(&f.to_le_bytes());
        }
        0x8915 => put_addr(&mut r[16..32], 2, if lo { 0x7F00_0001 } else { ip }),
        0x891B => put_addr(&mut r[16..32], 2, if lo { 0xFF00_0000 } else { mask }),
        0x8919 => put_addr(&mut r[16..32], 2, if lo { 0 } else { ip | !mask }),
        0x8927 => {
            r[16..32].fill(0);
            r[16..18].copy_from_slice(&(if lo { 772u16 } else { 1 }).to_le_bytes());
            if !lo {
                r[18..24].copy_from_slice(&mac);
            }
        }
        0x8921 => r[16..20].copy_from_slice(&(if lo { 65536i32 } else { 1500 }).to_le_bytes()),
        0x8933 => r[16..20].copy_from_slice(&(if lo { 1i32 } else { 2 }).to_le_bytes()),
        _ => return Err(EINVAL),
    }
    Ok(0)
}

fn tty_ioctl(cmd: u64, arg: u64) -> Result<i64, i64> {
    let t = tty::tty();
    match cmd {
        0x5401 => {
            user_slice(arg, 36)?.copy_from_slice(&t.termios.to_bytes());
            Ok(0)
        }
        0x5402..=0x5404 => {
            t.termios = tty::Termios::from_bytes(user_slice(arg, 36)?);
            Ok(0)
        }
        0x540F => {
            user_slice(arg, 4)?.copy_from_slice(&t.fg_pgrp.to_le_bytes());
            Ok(0)
        }
        0x5410 => {
            let s = user_slice(arg, 4)?;
            t.fg_pgrp = i32::from_le_bytes(s[0..4].try_into().unwrap());
            Ok(0)
        }
        0x5413 => {
            let s = user_slice(arg, 8)?;
            // SAFETY: a read-only look at the console's size.
            let (rows, cols) = unsafe { (*core::ptr::addr_of!(crate::console::FB)).as_ref().map(|fb| fb.size()).unwrap_or((25, 80)) };
            s[0..2].copy_from_slice(&(rows as u16).to_le_bytes());
            s[2..4].copy_from_slice(&(cols as u16).to_le_bytes());
            s[4..8].fill(0);
            Ok(0)
        }
        0x5414 | 0x540E | 0x5422 | 0x540B => Ok(0), // TIOCSWINSZ, TIOCSCTTY, TIOCNOTTY, TCFLSH
        0x541B => {
            let n = if tty::readable() { 1i32 } else { 0 };
            user_slice(arg, 4)?.copy_from_slice(&n.to_le_bytes());
            Ok(0)
        }
        0x5429 => {
            // TIOCGSID
            user_slice(arg, 4)?.copy_from_slice(&current().sid.to_le_bytes());
            Ok(0)
        }
        _ => Err(EINVAL),
    }
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

fn exec(f: &mut SyscallFrame, path: u64, argv_p: u64, envp_p: u64) -> Result<i64, i64> {
    let path = user_str(path)?;
    exec_path(f, &path, argv_p, envp_p)
}

fn exec_path(f: &mut SyscallFrame, path: &str, argv_p: u64, envp_p: u64) -> Result<i64, i64> {
    let mut argv = Vec::new();
    let mut envp = Vec::new();
    for (list, out) in [(argv_p, &mut argv), (envp_p, &mut envp)] {
        if list == 0 {
            continue;
        }
        for i in 0.. {
            let p = user_u64(list + i * 8)?;
            if p == 0 {
                break;
            }
            out.push(user_str(p)?);
            if i > 8192 {
                return Err(7); // E2BIG
            }
        }
    }
    let (entry, sp) = proc::replace(path, &argv, &envp)?;
    *f = proc::user_frame(entry, sp);
    // SAFETY: a new program has no thread pointer yet.
    unsafe { arch::wrmsr(arch::MSR_FS_BASE, 0) };
    Ok(0)
}

fn handle(f: &mut SyscallFrame) -> Result<i64, i64> {
    let (a0, a1, a2, a3, a4, a5) = (f.rdi, f.rsi, f.rdx, f.r10, f.r8, f.r9);
    match f.rax {
        0 => read(a0, a1, a2),
        1 => write(a0, a1, a2),
        2 => open(AT_FDCWD, a0, a1, a2),
        3 => {
            let table = current().fds.clone();
            let mut fds = table.borrow_mut();
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
            stat_node(a1, &node(&canon)?)
        }
        5 => stat_open(a1, &fd_file(a0)?.borrow()),
        7 => poll(a0, a1, a2 as i32 as i64),
        8 => {
            let file = fd_file(a0)?;
            let mut file = file.borrow_mut();
            let size = match &file.kind {
                Kind::File(n) => n.size() as i64,
                Kind::Dir(_) => 0,
                _ => return Err(ESPIPE),
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
            proc::unmap_range(current().pml4(), a0, a0.saturating_add(a1));
            Ok(0)
        }
        12 => brk(a0),
        13 => {
            // rt_sigaction(sig, act, oldact): handler, flags, restorer, mask
            let sig = a0 as usize;
            if sig == 0 || sig > 64 {
                return Err(EINVAL);
            }
            let table = current().sig.clone();
            if a2 != 0 {
                let old = table.borrow()[sig];
                let s = user_slice(a2, 32)?;
                for (i, v) in [old.handler, old.flags, old.restorer, old.mask].iter().enumerate() {
                    s[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
                }
            }
            if a1 != 0 {
                if sig == 9 || sig == 19 {
                    return Err(EINVAL);
                }
                let act = proc::SigAction { handler: user_u64(a1)?, flags: user_u64(a1 + 8)?, restorer: user_u64(a1 + 16)?, mask: user_u64(a1 + 24)? };
                table.borrow_mut()[sig] = act;
            }
            Ok(0)
        }
        14 => {
            // rt_sigprocmask(how, set, oldset)
            let p = current();
            if a2 != 0 {
                user_slice(a2, 8)?.copy_from_slice(&p.blocked.to_le_bytes());
            }
            if a1 != 0 {
                let set = user_u64(a1)? & !((1 << 8) | (1 << 18));
                match a0 {
                    0 => p.blocked |= set,
                    1 => p.blocked &= !set,
                    2 => p.blocked = set,
                    _ => return Err(EINVAL),
                }
            }
            Ok(0)
        }
        15 => {
            // rt_sigreturn: the registers from the signal frame
            let mut r = proc::Regs::from_sys(f);
            if proc::sigreturn(&mut r).is_err() {
                proc::exit_signal(11);
            }
            r.to_sys(f);
            Ok(r.r[0] as i64)
        }
        16 => {
            let file = fd_file(a0)?;
            match a1 {
                0x5421 => {
                    // FIONBIO
                    let on = user_slice(a2, 4)?.iter().any(|&b| b != 0);
                    let mut fb = file.borrow_mut();
                    if on { fb.flags |= O_NONBLOCK } else { fb.flags &= !O_NONBLOCK }
                    return Ok(0);
                }
                0x5451 | 0x5450 => return Ok(0), // FIOCLEX, FIONCLEX
                0x541B => {
                    // FIONREAD
                    let fb = file.borrow();
                    let n = match &fb.kind {
                        Kind::PipeR(p) => Some(p.borrow().buf.len()),
                        k => crate::sock::pending(k),
                    };
                    if let Some(n) = n {
                        user_slice(a2, 4)?.copy_from_slice(&(n as i32).to_le_bytes());
                        return Ok(0);
                    }
                }
                _ => {}
            }
            let fb = file.borrow();
            match &fb.kind {
                Kind::Tty => tty_ioctl(a1, a2),
                Kind::Sock(_) | Kind::Unix(_) => net_ioctl(a1, a2),
                _ => Err(ENOTTY),
            }
        }
        17 => {
            // pread64
            let file = fd_file(a0)?;
            let saved = file.borrow().pos;
            file.borrow_mut().pos = a3 as usize;
            let r = read(a0, a1, a2);
            file.borrow_mut().pos = saved;
            r
        }
        18 => {
            // pwrite64
            let file = fd_file(a0)?;
            let saved = file.borrow().pos;
            file.borrow_mut().pos = a3 as usize;
            let r = write(a0, a1, a2);
            file.borrow_mut().pos = saved;
            r
        }
        19 => {
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
        22 => pipe(a0, 0),
        23 => {
            let t = if a4 != 0 { Some(user_u64(a4)?.saturating_mul(1_000_000_000).saturating_add(user_u64(a4 + 8)? * 1000)) } else { None };
            select(a0, a1, a2, a3, t)
        }
        24 => {
            proc::yield_now();
            Ok(0)
        }
        25 => Err(ENOMEM), // mremap: callers fall back to mmap and copy
        28 => Ok(0),       // madvise
        32 => {
            let file = fd_file(a0)?;
            fd_install_rc(file, false, 0)
        }
        33 | 292 => {
            // dup2, dup3
            let file = fd_file(a0)?;
            let n = a1 as usize;
            if n >= 1024 {
                return Err(EBADF);
            }
            if a0 == a1 {
                return if f.rax == 292 { Err(EINVAL) } else { Ok(n as i64) };
            }
            let table = current().fds.clone();
            let mut fds = table.borrow_mut();
            while fds.len() <= n {
                fds.push(None);
            }
            fds[n] = Some(Fd { file, cloexec: f.rax == 292 && a2 as u32 & O_CLOEXEC != 0 });
            Ok(n as i64)
        }
        34 => loop {
            // pause: until a signal
            if proc::block() {
                return Err(proc::EINTR);
            }
        },
        35 => {
            let ns = read_timespec(a0)?;
            sleep_until(now_ns() + ns)
        }
        230 => {
            let ns = read_timespec(a2)?;
            let until = if a1 & 1 != 0 {
                if a0 == 0 {
                    ns.saturating_sub(realtime_ns()) + now_ns()
                } else {
                    ns
                }
            } else {
                now_ns() + ns
            };
            sleep_until(until)
        }
        37 => Ok(0), // alarm
        38 => {
            if a2 != 0 {
                user_slice(a2, 32)?.fill(0);
            }
            Ok(0)
        } // setitimer
        39 => Ok(current().tgid as i64),
        40 => {
            // sendfile(out, in, offset, count)
            let src = fd_file(a1)?;
            let start = if a2 != 0 { user_u64(a2)? as usize } else { src.borrow().pos };
            let mut buf = alloc::vec![0u8; (a3 as usize).min(1 << 20)];
            let k = match &src.borrow().kind {
                Kind::File(n) => n.read_at(start, &mut buf),
                _ => return Err(EINVAL),
            };
            // write through the normal path: a kernel buffer is not a user one, so go by parts
            let out = fd_file(a0)?;
            match &out.borrow().kind {
                Kind::Tty => crate::console::write_bytes(&buf[..k]),
                Kind::Null => {}
                Kind::File(Node::Mem(m)) => {
                    let mut m = m.borrow_mut();
                    m.data.extend_from_slice(&buf[..k]);
                }
                _ => return Err(EINVAL),
            }
            if a2 != 0 {
                user_slice(a2, 8)?.copy_from_slice(&((start + k) as u64).to_le_bytes());
            } else {
                src.borrow_mut().pos = start + k;
            }
            Ok(k as i64)
        }
        41 => crate::sock::socket(a0, a1, a2),
        42 => crate::sock::connect(a0, a1, a2),
        43 => crate::sock::accept(a0, a1, a2, 0),
        288 => crate::sock::accept(a0, a1, a2, a3),
        44 => crate::sock::send(a0, a1, a2, a3, a4, a5),
        45 => crate::sock::recvfrom(a0, a1, a2, a3, a4, a5),
        46 => crate::sock::sendmsg(a0, a1, a2),
        47 => crate::sock::recvmsg(a0, a1, a2),
        48 => crate::sock::shutdown(a0, a1),
        49 => crate::sock::bind(a0, a1, a2),
        50 => crate::sock::listen(a0, a1),
        51 => crate::sock::getname(a0, a1, a2, false),
        52 => crate::sock::getname(a0, a1, a2, true),
        53 => crate::sock::socketpair(a0, a1, a2, a3),
        54 => crate::sock::setsockopt(a0, a1, a2, a3, a4),
        55 => crate::sock::getsockopt(a0, a1, a2, a3, a4),
        56 => proc::clone(f, a0, a1, a2, a3, a4).map(|p| p as i64), // clone(flags, stack, ptid, ctid, tls)
        57 => proc::clone(f, 17, 0, 0, 0, 0).map(|p| p as i64),
        58 => proc::clone(f, proc::CLONE_VM | proc::CLONE_VFORK | 17, 0, 0, 0, 0).map(|p| p as i64),
        59 => exec(f, a0, a1, a2),
        322 => {
            // execveat with a path (AT_EMPTY_PATH is not supported): resolve, then execve
            let path = user_str(a1)?;
            let canon = at_path(a0 as i64, &path, true)?;
            exec_path(f, &alloc::format!("/{canon}"), a2, a3)
        }
        60 => proc::exit_thread((a0 & 0xFF) as i64),
        231 => proc::exit_current((a0 & 0xFF) as i64),
        61 => {
            // wait4
            match proc::wait(a0 as i32, a2 & 1 != 0)? {
                Some((pid, st)) => {
                    if a1 != 0 {
                        user_slice(a1, 4)?.copy_from_slice(&st.to_le_bytes());
                    }
                    if a3 != 0 {
                        user_slice(a3, 144)?.fill(0);
                    }
                    Ok(pid as i64)
                }
                None => Ok(0),
            }
        }
        247 => {
            // waitid(idtype, id, infop, options)
            let pid = match a0 {
                0 => -1,
                1 => a1 as i32,
                2 => -(a1 as i32),
                _ => return Err(EINVAL),
            };
            match proc::wait(pid, a3 & 1 != 0)? {
                Some((p, st)) => {
                    if a2 != 0 {
                        let s = user_slice(a2, 128)?;
                        s.fill(0);
                        let (code, status) = if st & 0x7F == 0 { (1, (st >> 8) & 0xFF) } else { (2, st & 0x7F) };
                        s[0..4].copy_from_slice(&17i32.to_le_bytes());
                        s[8..12].copy_from_slice(&(code as i32).to_le_bytes());
                        s[16..20].copy_from_slice(&p.to_le_bytes());
                        s[24..28].copy_from_slice(&status.to_le_bytes());
                    }
                    Ok(0)
                }
                None => {
                    if a2 != 0 {
                        user_slice(a2, 128)?.fill(0);
                    }
                    Ok(0)
                }
            }
        }
        62 => kill(a0 as i32 as i64, a1),
        200 => if proc::signal(a0 as i32, a1 as i32) { Ok(0) } else { Err(ESRCH) }, // tkill
        234 => if proc::signal(a1 as i32, a2 as i32) { Ok(0) } else { Err(ESRCH) }, // tgkill
        63 => uname(a0),
        72 => {
            // fcntl
            let file = fd_file(a0)?;
            match a1 {
                0 | 1030 => fd_install_rc(file, a1 == 1030, a2 as usize),
                1 => {
                    let p = current();
                    let fds = p.fds.borrow();
                    Ok(if fds.get(a0 as usize).and_then(|f| f.as_ref()).is_some_and(|f| f.cloexec) { 1 } else { 0 })
                }
                2 => {
                    if let Some(Some(fd)) = current().fds.borrow_mut().get_mut(a0 as usize) {
                        fd.cloexec = a2 & 1 != 0;
                    }
                    Ok(0)
                }
                3 => Ok((file.borrow().flags & !O_CLOEXEC) as i64),
                4 => {
                    let mut fl = file.borrow_mut();
                    fl.flags = (fl.flags & !(O_NONBLOCK | O_APPEND)) | (a2 as u32 & (O_NONBLOCK | O_APPEND));
                    Ok(0)
                }
                5..=7 | 36..=38 => Ok(0), // locks: one user, no conflicts
                _ => Err(EINVAL),
            }
        }
        74 | 75 | 162 => {
            // fsync, fdatasync, sync: save the changes to the boot partition now
            crate::persist::save();
            Ok(0)
        }
        169 => {
            // reboot(magic, magic2, cmd)
            if a0 != 0xFEE1_DEAD {
                return Err(EINVAL);
            }
            crate::persist::save();
            match a2 {
                0x0123_4567 => crate::dev::reboot(),
                0x4321_FEDC | 0xCDEF_0123 => crate::dev::power_off(),
                _ => Ok(0),
            }
        }
        76 | 77 => {
            // truncate, ftruncate
            let m = if f.rax == 76 {
                let canon = at_path(AT_FDCWD, &user_str(a0)?, true)?;
                vfs::writable(&canon, false, 0)?
            } else {
                match &fd_file(a0)?.borrow().kind {
                    Kind::File(Node::Mem(m)) => m.clone(),
                    _ => return Err(EINVAL),
                }
            };
            m.borrow_mut().data.resize(a1 as usize, 0);
            Ok(0)
        }
        79 => {
            let cwd = alloc::format!("/{}", current().cwd.borrow());
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
            *current().cwd.borrow_mut() = canon;
            Ok(0)
        }
        81 => {
            let d = match &fd_file(a0)?.borrow().kind {
                Kind::Dir(d) => d.clone(),
                _ => return Err(ENOTDIR),
            };
            *current().cwd.borrow_mut() = d;
            Ok(0)
        }
        82 | 264 | 316 => {
            // rename, renameat, renameat2
            let (fd1, p1, fd2, p2) = if f.rax == 82 { (AT_FDCWD, a0, AT_FDCWD, a1) } else { (a0 as i64, a1, a2 as i64, a3) };
            let from = at_path(fd1, &user_str(p1)?, false)?;
            let to = at_new_path(fd2, &user_str(p2)?)?;
            vfs::rename(&from, &to)?;
            Ok(0)
        }
        83 | 258 => {
            let (fd, p, mode) = if f.rax == 83 { (AT_FDCWD, a0, a1) } else { (a0 as i64, a1, a2) };
            let canon = at_new_path(fd, &user_str(p)?)?;
            vfs::mkdir(&canon, mode as u32 & !current().umask)?;
            Ok(0)
        }
        84 => {
            let canon = at_path(AT_FDCWD, &user_str(a0)?, false)?;
            vfs::remove(&canon, true)?;
            Ok(0)
        }
        87 | 263 => {
            let (fd, p, flags) = if f.rax == 87 { (AT_FDCWD, a0, 0) } else { (a0 as i64, a1, a2) };
            let canon = at_path(fd, &user_str(p)?, false)?;
            vfs::remove(&canon, flags & 0x200 != 0)?;
            Ok(0)
        }
        86 | 265 => Err(EPERM), // hard links: not supported
        88 | 266 => {
            let (target, fd, p) = if f.rax == 88 { (a0, AT_FDCWD, a1) } else { (a0, a1 as i64, a2) };
            let target = user_str(target)?;
            let canon = at_new_path(fd, &user_str(p)?)?;
            vfs::symlink(&target, &canon)?;
            Ok(0)
        }
        89 | 267 => {
            // readlink, readlinkat
            let (dirfd, p, buf, len) = if f.rax == 89 { (AT_FDCWD, a0, a1, a2) } else { (a0 as i64, a1, a2, a3) };
            let path = user_str(p)?;
            let target = if path == "/proc/self/exe" {
                alloc::format!("/bin/{}", current().name)
            } else if let Some(n) = path.strip_prefix("/proc/self/fd/") {
                let fd: u64 = n.parse().map_err(|_| ENOENT)?;
                fd_file(fd)?.borrow().path.clone()
            } else {
                let canon = at_path(dirfd, &path, false)?;
                let n = node(&canon)?;
                if !n.is_link() {
                    return Err(EINVAL);
                }
                n.link_target()
            };
            let k = target.len().min(len as usize);
            user_slice(buf, k as u64)?.copy_from_slice(&target.as_bytes()[..k]);
            Ok(k as i64)
        }
        90 | 268 => {
            let (fd, p, mode) = if f.rax == 90 { (AT_FDCWD, a0, a1) } else { (a0 as i64, a1, a2) };
            let canon = at_path(fd, &user_str(p)?, true)?;
            vfs::chmod(&canon, mode as u32)?;
            Ok(0)
        }
        91 => {
            if let Kind::File(Node::Mem(m)) = &fd_file(a0)?.borrow().kind {
                let mut m = m.borrow_mut();
                m.mode = (m.mode & S_IFMT) | (a1 as u32 & 0o7777);
            }
            Ok(0)
        }
        92..=94 | 260 => Ok(0), // chown and friends: one user
        95 => {
            let p = current();
            let old = p.umask;
            p.umask = a0 as u32 & 0o777;
            Ok(old as i64)
        }
        96 => {
            if a0 != 0 {
                let ns = realtime_ns();
                let s = user_slice(a0, 16)?;
                s[0..8].copy_from_slice(&(ns / 1_000_000_000).to_le_bytes());
                s[8..16].copy_from_slice(&((ns % 1_000_000_000) / 1000).to_le_bytes());
            }
            Ok(0)
        }
        97 | 302 => {
            // getrlimit, prlimit64
            let (res, old) = if f.rax == 97 { (a0, a1) } else { (a1, a3) };
            if old != 0 {
                let s = user_slice(old, 16)?;
                let cur = if res == 3 { proc::stack_limit() } else if res == 7 { 1024 } else { u64::MAX };
                let max = if res == 7 { 1024 } else { u64::MAX };
                s[0..8].copy_from_slice(&cur.to_le_bytes());
                s[8..16].copy_from_slice(&max.to_le_bytes());
            }
            Ok(0)
        }
        98 => {
            user_slice(a1, 144)?.fill(0);
            Ok(0)
        } // getrusage
        99 => {
            let s = user_slice(a0, 112)?;
            s.fill(0);
            // SAFETY: counters read on one CPU.
            let (total, used) = unsafe { (mem::TOTAL_PAGES * PAGE, mem::USED_PAGES * PAGE) };
            s[0..8].copy_from_slice(&(now_ns() / 1_000_000_000).to_le_bytes());
            s[32..40].copy_from_slice(&total.to_le_bytes());
            s[40..48].copy_from_slice(&(total - used.min(total)).to_le_bytes());
            s[80..82].copy_from_slice(&(proc::count() as u16).to_le_bytes());
            s[104..108].copy_from_slice(&1u32.to_le_bytes());
            Ok(0)
        }
        100 => {
            if a0 != 0 {
                user_slice(a0, 32)?.fill(0);
            }
            Ok((now_ns() / 10_000_000) as i64)
        } // times
        102 | 104 | 107 | 108 => Ok(0), // root
        105 | 106 | 113 | 114 | 116 | 117 | 119 => Ok(0), // setuid and friends
        115 => Ok(0),                    // getgroups: none
        118 | 120 => {
            for p in [a0, a1, a2] {
                if p != 0 {
                    user_slice(p, 4)?.fill(0);
                }
            }
            Ok(0)
        } // getresuid, getresgid
        109 => {
            // setpgid
            let target = if a0 == 0 { proc::pid() } else { a0 as i32 };
            let pgid = if a1 == 0 { target } else { a1 as i32 };
            let me = proc::pid();
            let p = proc::get(target).ok_or(ESRCH)?;
            if p.pid != me && p.ppid != me {
                return Err(ESRCH);
            }
            let tg = p.tgid;
            for t in proc::all_tasks_of(tg) {
                if let Some(t) = proc::get(t) {
                    t.pgid = pgid;
                }
            }
            Ok(0)
        }
        110 => Ok(current().ppid as i64),
        111 => Ok(current().pgid as i64),
        112 => {
            let p = current();
            p.sid = p.pid;
            p.pgid = p.pid;
            Ok(p.pid as i64)
        }
        121 => {
            let target = if a0 == 0 { proc::pid() } else { a0 as i32 };
            Ok(proc::get(target).ok_or(ESRCH)?.pgid as i64)
        }
        124 => {
            let target = if a0 == 0 { proc::pid() } else { a0 as i32 };
            Ok(proc::get(target).ok_or(ESRCH)?.sid as i64)
        }
        127 => {
            let p = current();
            let v = p.pending & p.blocked;
            user_slice(a0, 8)?.copy_from_slice(&v.to_le_bytes());
            Ok(0)
        } // rt_sigpending
        130 => {
            // rt_sigsuspend: wait with this mask until a signal
            current().blocked = user_u64(a0)? & !((1 << 8) | (1 << 18));
            loop {
                if proc::block() {
                    return Err(proc::EINTR);
                }
            }
        }
        131 => {
            // sigaltstack(new, old): stack_t { sp, flags, size }
            let p = current();
            if a1 != 0 {
                let s = user_slice(a1, 24)?;
                s.fill(0);
                s[0..8].copy_from_slice(&p.altstack.0.to_le_bytes());
                let flags: u32 = if p.altstack.1 == 0 { 2 } else { 0 };
                s[8..12].copy_from_slice(&flags.to_le_bytes());
                s[16..24].copy_from_slice(&p.altstack.1.to_le_bytes());
            }
            if a0 != 0 {
                let sp = user_u64(a0)?;
                let flags = user_u64(a0 + 8)? as u32;
                let size = user_u64(a0 + 16)?;
                p.altstack = if flags & 2 != 0 { (0, 0) } else { (sp, size) };
            }
            Ok(0)
        }
        133 | 259 => {
            // mknod, mknodat: FIFOs only
            let (fd, p, mode) = if f.rax == 133 { (AT_FDCWD, a0, a1) } else { (a0 as i64, a1, a2) };
            if mode as u32 & S_IFMT != S_IFIFO {
                return Err(EPERM);
            }
            let canon = at_new_path(fd, &user_str(p)?)?;
            vfs::mknod_fifo(&canon, mode as u32)?;
            Ok(0)
        }
        137 | 138 => {
            let s = user_slice(a1, 120)?;
            s.fill(0);
            s[0..8].copy_from_slice(&0x0102_1994u64.to_le_bytes()); // tmpfs
            s[8..16].copy_from_slice(&4096u64.to_le_bytes());
            Ok(0)
        } // statfs, fstatfs
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
        186 => Ok(proc::pid() as i64), // gettid
        201 => {
            let t = wall_seconds() as i64;
            if a0 != 0 {
                user_slice(a0, 8)?.copy_from_slice(&t.to_le_bytes());
            }
            Ok(t)
        }
        202 => {
            // futex(addr, op, val, timeout / val2, addr2, val3)
            match a1 & 0x7F {
                0 => {
                    let deadline = if a3 != 0 { Some(now_ns() + read_timespec(a3)?) } else { None };
                    proc::futex_wait(a0, a2 as u32, deadline)
                }
                9 => {
                    let deadline = if a3 != 0 {
                        let t = read_timespec(a3)?;
                        Some(if a1 & 256 != 0 { t.saturating_sub(realtime_ns()) + now_ns() } else { t })
                    } else {
                        None
                    };
                    proc::futex_wait(a0, a2 as u32, deadline)
                }
                1 | 10 => Ok(proc::futex_wake(a0, a2 as u32)),
                3 | 4 => {
                    // requeue: waking the ones to move as well is allowed (they check again)
                    Ok(proc::futex_wake(a0, (a2 as u32).saturating_add(a3 as u32)))
                }
                _ => {
                    report(202);
                    Err(ENOSYS)
                }
            }
        }
        204 => {
            if a1 < 8 {
                return Err(EINVAL);
            }
            user_slice(a2, 8)?.copy_from_slice(&1u64.to_le_bytes());
            Ok(8)
        } // sched_getaffinity: CPU 0
        217 => getdents64(a0, a1, a2),
        218 => {
            current().clear_tid = a0;
            Ok(proc::pid() as i64)
        }
        228 => timespec(a1, if a0 == 0 { realtime_ns() } else { now_ns() }),
        229 => {
            if a1 != 0 {
                timespec(a1, 1)?;
            }
            Ok(0)
        }
        257 => open(a0 as i64, a1, a2, a3),
        262 => {
            // newfstatat
            let path = user_str(a1)?;
            if path.is_empty() && a3 & 0x1000 != 0 {
                return stat_open(a2, &fd_file(a0)?.borrow());
            }
            let canon = at_path(a0 as i64, &path, a3 & 0x100 == 0)?;
            if let Some(kind) = device(&canon) {
                return stat_open(a2, &OpenFile { kind, pos: 0, flags: 0, path: String::new() });
            }
            stat_node(a2, &node(&canon)?)
        }
        269 | 439 => {
            at_path(a0 as i64, &user_str(a1)?, true)?;
            Ok(0)
        } // faccessat, faccessat2
        270 => {
            let t = if a4 != 0 { Some(read_timespec(a4)?) } else { None };
            select(a0, a1, a2, a3, t)
        } // pselect6
        271 => {
            let t = if a2 != 0 { (read_timespec(a2)? / 1_000_000) as i64 } else { -1 };
            poll(a0, a1, t)
        } // ppoll
        273 => Ok(0), // set_robust_list
        280 => {
            // utimensat
            if a1 != 0 {
                let canon = at_path(a0 as i64, &user_str(a1)?, a3 & 0x100 == 0)?;
                vfs::touch(&canon, wall_seconds());
            }
            Ok(0)
        }
        293 => pipe(a0, a1),
        318 => {
            fill_random(user_slice(a0, a1)?);
            Ok(a1 as i64)
        }
        // statx, rseq, clone3, membarrier: missing on purpose; the C library falls back
        332 | 334 | 435 | 324 => Err(ENOSYS),
        nr => {
            report(nr);
            Err(ENOSYS)
        }
    }
}

/// From the `syscall` entry: the program's registers; the result goes back in rax. On the way
/// back, pending signals are acted on (a handler gets its frame here).
pub extern "sysv64" fn dispatch(f: &mut SyscallFrame) {
    let nr = f.rax;
    let mut restart = None;
    match handle(f) {
        Ok(v) => f.rax = v as u64,
        Err(proc::ERESTART) => restart = Some(nr),
        Err(e) => f.rax = (-e) as u64,
    }
    let mut r = proc::Regs::from_sys(f);
    proc::deliver_signals(&mut r, restart);
    r.to_sys(f);
    if f.rip >= proc::USER_TOP {
        proc::exit_signal(11);
    }
}

#[allow(dead_code)]
fn unused() -> [i64; 3] {
    [ENOENT, ENOSPC, ENXIO]
}
