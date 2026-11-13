//! Thin system call wrappers and the file, process and memory functions glibc has and
//! flint-libc does not (including glibc's *64 and __xstat names for the same functions).

use super::umem;
use crate::fl;
use crate::fl::sys::{ret, ret_i, sc0, sc1, sc2, sc3, sc4, sc5, sc6};
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

type c_char = u8;
type c_int = i32;
type c_long = i64;
type size_t = usize;
type off_t = i64;

macro_rules! sys {
    ($name:ident($($a:ident: $t:ty),*) -> c_int = $nr:expr) => {
        #[no_mangle]
        pub unsafe extern "C" fn $name($($a: $t),*) -> c_int {
            ret_i(sys!(@call $nr; $($a),*))
        }
    };
    ($name:ident($($a:ident: $t:ty),*) -> isize = $nr:expr) => {
        #[no_mangle]
        pub unsafe extern "C" fn $name($($a: $t),*) -> isize {
            ret(sys!(@call $nr; $($a),*))
        }
    };
    (@call $nr:expr;) => { sc0($nr) };
    (@call $nr:expr; $a:ident) => { sc1($nr, $a as usize) };
    (@call $nr:expr; $a:ident, $b:ident) => { sc2($nr, $a as usize, $b as usize) };
    (@call $nr:expr; $a:ident, $b:ident, $c:ident) => { sc3($nr, $a as usize, $b as usize, $c as usize) };
    (@call $nr:expr; $a:ident, $b:ident, $c:ident, $d:ident) => { sc4($nr, $a as usize, $b as usize, $c as usize, $d as usize) };
    (@call $nr:expr; $a:ident, $b:ident, $c:ident, $d:ident, $e:ident) => { sc5($nr, $a as usize, $b as usize, $c as usize, $d as usize, $e as usize) };
    (@call $nr:expr; $a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident) => { sc6($nr, $a as usize, $b as usize, $c as usize, $d as usize, $e as usize, $f as usize) };
}

type P = *mut c_void;
type CP = *const c_char;

sys!(capget(h: P, d: P) -> c_int = 125);
sys!(capset(h: P, d: P) -> c_int = 126);
sys!(unshare(f: c_int) -> c_int = 272);
sys!(setns(fd: c_int, t: c_int) -> c_int = 308);
sys!(process_vm_readv(pid: c_int, l: P, ln: u64, r: P, rn: u64, f: u64) -> isize = 310);
sys!(process_vm_writev(pid: c_int, l: P, ln: u64, r: P, rn: u64, f: u64) -> isize = 311);
sys!(setpriority(w: c_int, who: u32, p: c_int) -> c_int = 141);
sys!(getrusage(w: c_int, r: P) -> c_int = 98);
sys!(getresuid(a: P, b: P, c: P) -> c_int = 118);
sys!(getresgid(a: P, b: P, c: P) -> c_int = 120);
sys!(setresuid(a: u32, b: u32, c: u32) -> c_int = 117);
sys!(setresgid(a: u32, b: u32, c: u32) -> c_int = 119);
sys!(setreuid(a: u32, b: u32) -> c_int = 113);
sys!(setregid(a: u32, b: u32) -> c_int = 114);
sys!(seteuid_raw(a: u32) -> c_int = 105);
sys!(sched_get_priority_min(p: c_int) -> c_int = 147);
sys!(sched_get_priority_max(p: c_int) -> c_int = 146);
sys!(sched_setaffinity(pid: c_int, n: size_t, s: P) -> c_int = 203);
sys!(sched_setscheduler(pid: c_int, p: c_int, s: P) -> c_int = 144);
sys!(sched_getscheduler(pid: c_int) -> c_int = 145);
sys!(sched_setparam(pid: c_int, s: P) -> c_int = 142);
sys!(sched_getparam(pid: c_int, s: P) -> c_int = 143);
sys!(sysinfo(i: P) -> c_int = 99);
sys!(recvmmsg(fd: c_int, v: P, n: u32, f: c_int, t: P) -> c_int = 299);
sys!(sendmmsg(fd: c_int, v: P, n: u32, f: c_int) -> c_int = 307);
sys!(semget(k: c_int, n: c_int, f: c_int) -> c_int = 64);
sys!(semop(id: c_int, o: P, n: size_t) -> c_int = 65);
sys!(semtimedop(id: c_int, o: P, n: size_t, t: P) -> c_int = 220);
sys!(shmget(k: c_int, n: size_t, f: c_int) -> c_int = 29);
sys!(shmctl(id: c_int, c: c_int, b: P) -> c_int = 31);
sys!(shmdt(a: P) -> c_int = 67);
sys!(msgget(k: c_int, f: c_int) -> c_int = 68);
sys!(epoll_create1(f: c_int) -> c_int = 291);
sys!(epoll_ctl(e: c_int, op: c_int, fd: c_int, ev: P) -> c_int = 233);
sys!(epoll_wait(e: c_int, ev: P, n: c_int, t: c_int) -> c_int = 232);
sys!(epoll_pwait2(e: c_int, ev: P, n: c_int, t: P, s: P) -> c_int = 441);
sys!(inotify_init1(f: c_int) -> c_int = 294);
sys!(inotify_add_watch(fd: c_int, p: CP, m: u32) -> c_int = 254);
sys!(inotify_rm_watch(fd: c_int, w: c_int) -> c_int = 255);
sys!(timerfd_create(c: c_int, f: c_int) -> c_int = 283);
sys!(timerfd_settime(fd: c_int, f: c_int, n: P, o: P) -> c_int = 286);
sys!(timerfd_gettime(fd: c_int, o: P) -> c_int = 287);
sys!(memfd_create(n: CP, f: u32) -> c_int = 319);
sys!(msync(a: P, n: size_t, f: c_int) -> c_int = 26);
sys!(mlock(a: P, n: size_t) -> c_int = 149);
sys!(munlock(a: P, n: size_t) -> c_int = 150);
sys!(mlockall(f: c_int) -> c_int = 151);
sys!(munlockall() -> c_int = 152);
sys!(mincore(a: P, n: size_t, v: P) -> c_int = 27);
sys!(copy_file_range(a: c_int, ao: P, b: c_int, bo: P, n: size_t, f: u32) -> isize = 326);
sys!(fallocate(fd: c_int, m: c_int, o: off_t, l: off_t) -> c_int = 285);
sys!(fallocate64(fd: c_int, m: c_int, o: off_t, l: off_t) -> c_int = 285);
sys!(readahead(fd: c_int, o: off_t, n: size_t) -> isize = 187);
sys!(flock(fd: c_int, op: c_int) -> c_int = 73);
sys!(close_range(a: u32, b: u32, f: c_int) -> c_int = 436);
sys!(statx(d: c_int, p: CP, f: c_int, m: u32, b: P) -> c_int = 332);
sys!(fstatfs(fd: c_int, b: P) -> c_int = 138);
sys!(fstatfs64(fd: c_int, b: P) -> c_int = 138);
sys!(statfs(p: CP, b: P) -> c_int = 137);
sys!(statfs64(p: CP, b: P) -> c_int = 137);
sys!(getxattr(p: CP, n: CP, v: P, s: size_t) -> isize = 191);
sys!(fgetxattr(fd: c_int, n: CP, v: P, s: size_t) -> isize = 193);
sys!(setxattr(p: CP, n: CP, v: P, s: size_t, f: c_int) -> c_int = 188);
sys!(fsetxattr(fd: c_int, n: CP, v: P, s: size_t, f: c_int) -> c_int = 190);
sys!(listxattr(p: CP, l: P, s: size_t) -> isize = 194);
sys!(flistxattr(fd: c_int, l: P, s: size_t) -> isize = 196);
sys!(removexattr(p: CP, n: CP) -> c_int = 197);
sys!(lremovexattr(p: CP, n: CP) -> c_int = 198);
sys!(fremovexattr(fd: c_int, n: CP) -> c_int = 199);
sys!(mkdirat(d: c_int, p: CP, m: u32) -> c_int = 258);
sys!(fchownat(d: c_int, p: CP, u: u32, g: u32, f: c_int) -> c_int = 260);
sys!(quotactl(c: c_int, s: CP, id: c_int, a: P) -> c_int = 179);
sys!(ioperm(f: u64, n: u64, t: c_int) -> c_int = 173);
sys!(iopl(l: c_int) -> c_int = 172);
sys!(mount_setattr(d: c_int, p: CP, f: u32, a: P, s: size_t) -> c_int = 442);
sys!(move_mount(a: c_int, ap: CP, b: c_int, bp: CP, f: u32) -> c_int = 429);
sys!(open_tree(d: c_int, p: CP, f: u32) -> c_int = 428);
sys!(name_to_handle_at(d: c_int, p: CP, h: P, m: P, f: c_int) -> c_int = 303);
sys!(open_by_handle_at(d: c_int, h: P, f: c_int) -> c_int = 304);
sys!(getrandom(b: P, n: size_t, f: u32) -> isize = 318);
sys!(mq_getsetattr(q: c_int, n: P, o: P) -> c_int = 245);
sys!(personality(p: u64) -> c_int = 135);
sys!(sync_file_range(fd: c_int, o: off_t, n: off_t, f: u32) -> c_int = 277);
sys!(syncfs(fd: c_int) -> c_int = 306);
sys!(vhangup() -> c_int = 153);
sys!(swapon(p: CP, f: c_int) -> c_int = 167);
sys!(swapoff(p: CP) -> c_int = 168);
sys!(pivot_root(a: CP, b: CP) -> c_int = 155);
sys!(setfsuid(u: u32) -> c_int = 122);
sys!(setfsgid(g: u32) -> c_int = 123);
sys!(getsid(p: c_int) -> c_int = 124);
sys!(getpgid(p: c_int) -> c_int = 121);
sys!(acct(p: CP) -> c_int = 163);
sys!(fchmodat2(d: c_int, p: CP, m: u32, f: c_int) -> c_int = 452);
sys!(renameat2(a: c_int, ap: CP, b: c_int, bp: CP, f: u32) -> c_int = 316);
sys!(renameat(a: c_int, ap: CP, b: c_int, bp: CP) -> c_int = 264);
sys!(symlinkat(a: CP, d: c_int, b: CP) -> c_int = 266);
sys!(mknodat_raw(d: c_int, p: CP, m: u32, dev: u64) -> c_int = 259);
sys!(tee(a: c_int, b: c_int, n: size_t, f: u32) -> isize = 276);
sys!(vmsplice(fd: c_int, v: P, n: u64, f: u32) -> isize = 278);
sys!(pidfd_open(p: c_int, f: u32) -> c_int = 434);
sys!(pidfd_send_signal(p: c_int, s: c_int, i: P, f: u32) -> c_int = 424);
sys!(pidfd_getfd(p: c_int, fd: c_int, f: u32) -> c_int = 438);
sys!(sched_rr_get_interval(p: c_int, t: P) -> c_int = 148);
sys!(getitimer(w: c_int, v: P) -> c_int = 36);
sys!(setitimer(w: c_int, n: P, o: P) -> c_int = 38);
sys!(timer_delete_raw(t: c_int) -> c_int = 226);
sys!(clock_getres(c: c_int, t: P) -> c_int = 229);
sys!(clock_adjtime(c: c_int, t: P) -> c_int = 305);
sys!(adjtimex(t: P) -> c_int = 159);
sys!(settimeofday_raw(t: P, z: P) -> c_int = 164);
sys!(rt_sigqueueinfo(p: c_int, s: c_int, i: P) -> c_int = 129);
sys!(sigpending_raw(s: P, n: size_t) -> c_int = 127);
sys!(fanotify_init(f: u32, e: u32) -> c_int = 300);
sys!(fanotify_mark(fd: c_int, f: u32, m: u64, d: c_int, p: CP) -> c_int = 301);
sys!(mlock2(a: P, n: size_t, f: u32) -> c_int = 325);
sys!(membarrier(c: c_int, f: u32) -> c_int = 324);
sys!(kcmp(a: c_int, b: c_int, t: c_int, x: u64, y: u64) -> c_int = 312);
sys!(init_module(m: P, n: u64, a: CP) -> c_int = 175);
sys!(delete_module(n: CP, f: u32) -> c_int = 176);
sys!(finit_module(fd: c_int, a: CP, f: c_int) -> c_int = 313);

#[no_mangle]
pub unsafe extern "C" fn sched_getcpu() -> c_int {
    let mut cpu = 0u32;
    let r = sc3(309, &mut cpu as *mut u32 as usize, 0, 0);
    if r < 0 {
        fl::errno::set(-r as i32);
        return -1;
    }
    cpu as c_int
}
#[no_mangle]
pub unsafe extern "C" fn getcpu(cpu: *mut u32, node: *mut u32) -> c_int {
    ret_i(sc3(309, cpu as usize, node as usize, 0))
}

#[no_mangle]
pub unsafe extern "C" fn getpriority(w: c_int, who: u32) -> c_int {
    let r = sc2(140, w as usize, who as usize);
    if r < 0 {
        fl::errno::set(-r as i32);
        return -1;
    }
    20 - r as c_int
}

#[no_mangle]
pub unsafe extern "C" fn nice(inc: c_int) -> c_int {
    let p = getpriority(0, 0);
    if setpriority(0, 0, p + inc) < 0 {
        return -1;
    }
    getpriority(0, 0)
}

#[no_mangle]
pub unsafe extern "C" fn getrlimit(r: c_int, l: P) -> c_int {
    ret_i(sc4(302, 0, r as usize, 0, l as usize))
}
#[no_mangle]
pub unsafe extern "C" fn getrlimit64(r: c_int, l: P) -> c_int {
    getrlimit(r, l)
}
#[no_mangle]
pub unsafe extern "C" fn setrlimit(r: c_int, l: P) -> c_int {
    ret_i(sc4(302, 0, r as usize, l as usize, 0))
}
#[no_mangle]
pub unsafe extern "C" fn setrlimit64(r: c_int, l: P) -> c_int {
    setrlimit(r, l)
}
#[no_mangle]
pub unsafe extern "C" fn prlimit(p: c_int, r: c_int, n: P, o: P) -> c_int {
    ret_i(sc4(302, p as usize, r as usize, n as usize, o as usize))
}
#[no_mangle]
pub unsafe extern "C" fn prlimit64(p: c_int, r: c_int, n: P, o: P) -> c_int {
    prlimit(p, r, n, o)
}

#[no_mangle]
pub unsafe extern "C" fn prctl(op: c_int, a: u64, b: u64, c: u64, d: u64) -> c_int {
    ret_i(sc5(157, op as usize, a as usize, b as usize, c as usize, d as usize))
}

#[no_mangle]
pub unsafe extern "C" fn ptrace(req: c_int, pid: c_int, addr: P, data: P) -> c_long {
    // PTRACE_PEEK{TEXT,DATA,USER} return the word through a pointer at the system call level
    if (1..=3).contains(&req) {
        let mut word: c_long = 0;
        let r = sc4(101, req as usize, pid as usize, addr as usize, &mut word as *mut c_long as usize);
        if r < 0 {
            fl::errno::set(-r as i32);
            return -1;
        }
        fl::errno::set(0);
        return word;
    }
    ret(sc4(101, req as usize, pid as usize, addr as usize, data as usize)) as c_long
}

#[no_mangle]
pub unsafe extern "C" fn select(n: c_int, r: P, w: P, e: P, tv: P) -> c_int {
    ret_i(sc5(23, n as usize, r as usize, w as usize, e as usize, tv as usize))
}
#[no_mangle]
pub unsafe extern "C" fn pselect(n: c_int, r: P, w: P, e: P, ts: P, mask: P) -> c_int {
    let data = [mask as usize, 8usize];
    ret_i(sc6(270, n as usize, r as usize, w as usize, e as usize, ts as usize, if mask.is_null() { 0 } else { data.as_ptr() as usize }))
}
#[no_mangle]
pub unsafe extern "C" fn ppoll(fds: P, n: u64, ts: *const c_void, mask: *const c_void) -> c_int {
    // the kernel updates the timeout: pass a copy
    let mut t = [0i64; 2];
    let tp = if ts.is_null() {
        0
    } else {
        t.copy_from_slice(core::slice::from_raw_parts(ts as *const i64, 2));
        t.as_mut_ptr() as usize
    };
    ret_i(sc5(271, fds as usize, n as usize, tp, mask as usize, 8))
}
#[no_mangle]
pub unsafe extern "C" fn epoll_create(n: c_int) -> c_int {
    if n <= 0 {
        fl::errno::set(fl::errno::EINVAL);
        return -1;
    }
    epoll_create1(0)
}
#[no_mangle]
pub unsafe extern "C" fn epoll_pwait(e: c_int, ev: P, n: c_int, t: c_int, s: P) -> c_int {
    ret_i(sc6(281, e as usize, ev as usize, n as usize, t as usize, s as usize, 8))
}
#[no_mangle]
pub unsafe extern "C" fn eventfd(v: u32, f: c_int) -> c_int {
    ret_i(sc2(290, v as usize, f as usize))
}
#[no_mangle]
pub unsafe extern "C" fn eventfd_read(fd: c_int, v: *mut u64) -> c_int {
    if sc3(fl::sys::READ, fd as usize, v as usize, 8) == 8 { 0 } else { -1 }
}
#[no_mangle]
pub unsafe extern "C" fn eventfd_write(fd: c_int, v: u64) -> c_int {
    if sc3(fl::sys::WRITE, fd as usize, &v as *const u64 as usize, 8) == 8 { 0 } else { -1 }
}
#[no_mangle]
pub unsafe extern "C" fn inotify_init() -> c_int {
    inotify_init1(0)
}
#[no_mangle]
pub unsafe extern "C" fn signalfd(fd: c_int, mask: P, flags: c_int) -> c_int {
    ret_i(sc4(289, fd as usize, mask as usize, 8, flags as usize))
}
#[no_mangle]
pub unsafe extern "C" fn mremap(old: P, os: size_t, ns: size_t, flags: c_int, new: P) -> P {
    let r = sc5(25, old as usize, os, ns, flags as usize, new as usize);
    if r < 0 && r > -4096 {
        fl::errno::set(-r as i32);
        return usize::MAX as P;
    }
    r as P
}
#[no_mangle]
pub unsafe extern "C" fn shmat(id: c_int, a: P, f: c_int) -> P {
    let r = sc3(30, id as usize, a as usize, f as usize);
    if r < 0 && r > -4096 {
        fl::errno::set(-r as i32);
        return usize::MAX as P;
    }
    r as P
}
#[no_mangle]
pub unsafe extern "C" fn semctl(id: c_int, n: c_int, cmd: c_int, arg: usize) -> c_int {
    ret_i(sc4(66, id as usize, n as usize, (cmd | 0x100 /* IPC_64 */) as usize, arg))
}
#[no_mangle]
pub unsafe extern "C" fn ftok(path: CP, id: c_int) -> c_int {
    let mut st = [0u64; 18];
    if fl::file::stat(path as *const _, st.as_mut_ptr() as P) < 0 {
        return -1;
    }
    let dev = st[0];
    let ino = st[1];
    ((ino & 0xffff) | ((dev & 0xff) << 16) | (((id as u64) & 0xff) << 24)) as c_int
}
#[no_mangle]
pub unsafe extern "C" fn mq_getattr(q: c_int, a: P) -> c_int {
    mq_getsetattr(q, ptr::null_mut(), a)
}
#[no_mangle]
pub unsafe extern "C" fn posix_fadvise(fd: c_int, o: off_t, n: off_t, a: c_int) -> c_int {
    let r = sc4(221, fd as usize, o as usize, n as usize, a as usize);
    if r < 0 { -r as c_int } else { 0 }
}
#[no_mangle]
pub unsafe extern "C" fn posix_fadvise64(fd: c_int, o: off_t, n: off_t, a: c_int) -> c_int {
    posix_fadvise(fd, o, n, a)
}
#[no_mangle]
pub unsafe extern "C" fn posix_fallocate(fd: c_int, o: off_t, n: off_t) -> c_int {
    let r = sc4(285, fd as usize, 0, o as usize, n as usize);
    if r < 0 { -r as c_int } else { 0 }
}
#[no_mangle]
pub unsafe extern "C" fn posix_fallocate64(fd: c_int, o: off_t, n: off_t) -> c_int {
    posix_fallocate(fd, o, n)
}
#[no_mangle]
pub unsafe extern "C" fn posix_madvise(a: P, n: size_t, adv: c_int) -> c_int {
    if adv == 4 {
        return 0; // POSIX_MADV_DONTNEED: ignored, as glibc does
    }
    let r = sc3(28, a as usize, n, adv as usize);
    if r < 0 { -r as c_int } else { 0 }
}
#[no_mangle]
pub unsafe extern "C" fn lockf(fd: c_int, cmd: c_int, len: off_t) -> c_int {
    // F_LOCK 1, F_TLOCK 2, F_ULOCK 0, F_TEST 3 through fcntl record locks
    #[repr(C)]
    struct Flock {
        ty: i16,
        whence: i16,
        start: i64,
        len: i64,
        pid: i32,
    }
    let mut l = Flock { ty: 1 /* F_WRLCK */, whence: 1, start: 0, len, pid: 0 };
    match cmd {
        0 => {
            l.ty = 2; // F_UNLCK
            ret_i(sc3(fl::sys::FCNTL, fd as usize, 6 /* F_SETLK */, &l as *const Flock as usize))
        }
        1 => ret_i(sc3(fl::sys::FCNTL, fd as usize, 7 /* F_SETLKW */, &l as *const Flock as usize)),
        2 => ret_i(sc3(fl::sys::FCNTL, fd as usize, 6, &l as *const Flock as usize)),
        3 => {
            l.ty = 0; // F_RDLCK
            let r = sc3(fl::sys::FCNTL, fd as usize, 5 /* F_GETLK */, &mut l as *mut Flock as usize);
            if r < 0 {
                return ret_i(r);
            }
            if l.ty == 2 || l.pid == sc0(39) as i32 {
                0
            } else {
                fl::errno::set(13);
                -1
            }
        }
        _ => {
            fl::errno::set(fl::errno::EINVAL);
            -1
        }
    }
}
#[no_mangle]
pub unsafe extern "C" fn lockf64(fd: c_int, cmd: c_int, len: off_t) -> c_int {
    lockf(fd, cmd, len)
}

// --- *64 names and glibc's old stat entry points ----------------------------------------

#[no_mangle]
pub unsafe extern "C" fn open64(p: CP, f: c_int, m: u32) -> c_int {
    fl::file::open(p as *const _, f, m)
}
#[no_mangle]
pub unsafe extern "C" fn openat64(d: c_int, p: CP, f: c_int, m: u32) -> c_int {
    fl::file::openat(d, p as *const _, f, m)
}
#[no_mangle]
pub unsafe extern "C" fn creat(p: CP, m: u32) -> c_int {
    fl::file::open(p as *const _, 0o1101 /* WRONLY|CREAT|TRUNC */, m)
}
#[no_mangle]
pub unsafe extern "C" fn creat64(p: CP, m: u32) -> c_int {
    creat(p, m)
}
#[no_mangle]
pub unsafe extern "C" fn lseek64(fd: c_int, o: off_t, w: c_int) -> off_t {
    fl::file::lseek(fd, o, w)
}
#[no_mangle]
pub unsafe extern "C" fn pread64(fd: c_int, b: P, n: size_t, o: off_t) -> isize {
    fl::file::pread(fd, b, n, o)
}
#[no_mangle]
pub unsafe extern "C" fn pwrite64(fd: c_int, b: *const c_void, n: size_t, o: off_t) -> isize {
    fl::file::pwrite(fd, b, n, o)
}
#[no_mangle]
pub unsafe extern "C" fn preadv(fd: c_int, v: P, n: c_int, o: off_t) -> isize {
    ret(sc5(295, fd as usize, v as usize, n as usize, o as usize, 0))
}
#[no_mangle]
pub unsafe extern "C" fn pwritev(fd: c_int, v: P, n: c_int, o: off_t) -> isize {
    ret(sc5(296, fd as usize, v as usize, n as usize, o as usize, 0))
}
#[no_mangle]
pub unsafe extern "C" fn preadv64(fd: c_int, v: P, n: c_int, o: off_t) -> isize {
    preadv(fd, v, n, o)
}
#[no_mangle]
pub unsafe extern "C" fn pwritev64(fd: c_int, v: P, n: c_int, o: off_t) -> isize {
    pwritev(fd, v, n, o)
}
#[no_mangle]
pub unsafe extern "C" fn truncate64(p: CP, l: off_t) -> c_int {
    fl::file::truncate(p as *const _, l)
}
#[no_mangle]
pub unsafe extern "C" fn ftruncate64(fd: c_int, l: off_t) -> c_int {
    fl::file::ftruncate(fd, l)
}
#[no_mangle]
pub unsafe extern "C" fn fcntl64(fd: c_int, cmd: c_int, arg: usize) -> c_int {
    fl::file::fcntl(fd, cmd, arg)
}
#[no_mangle]
pub unsafe extern "C" fn mmap64(a: P, n: size_t, p: c_int, f: c_int, fd: c_int, o: off_t) -> P {
    fl::file::mmap(a, n, p, f, fd, o)
}
#[no_mangle]
pub unsafe extern "C" fn stat64(p: CP, s: P) -> c_int {
    fl::file::stat(p as *const _, s)
}
#[no_mangle]
pub unsafe extern "C" fn lstat64(p: CP, s: P) -> c_int {
    fl::file::lstat(p as *const _, s)
}
#[no_mangle]
pub unsafe extern "C" fn fstat64(fd: c_int, s: P) -> c_int {
    fl::file::fstat(fd, s)
}
#[no_mangle]
pub unsafe extern "C" fn fstatat64(d: c_int, p: CP, s: P, f: c_int) -> c_int {
    fl::file::fstatat(d, p as *const _, s, f)
}
#[no_mangle]
pub unsafe extern "C" fn __xstat(_v: c_int, p: CP, s: P) -> c_int {
    fl::file::stat(p as *const _, s)
}
#[no_mangle]
pub unsafe extern "C" fn __xstat64(_v: c_int, p: CP, s: P) -> c_int {
    fl::file::stat(p as *const _, s)
}
#[no_mangle]
pub unsafe extern "C" fn __lxstat(_v: c_int, p: CP, s: P) -> c_int {
    fl::file::lstat(p as *const _, s)
}
#[no_mangle]
pub unsafe extern "C" fn __lxstat64(_v: c_int, p: CP, s: P) -> c_int {
    fl::file::lstat(p as *const _, s)
}
#[no_mangle]
pub unsafe extern "C" fn __fxstat(_v: c_int, fd: c_int, s: P) -> c_int {
    fl::file::fstat(fd, s)
}
#[no_mangle]
pub unsafe extern "C" fn __fxstat64(_v: c_int, fd: c_int, s: P) -> c_int {
    fl::file::fstat(fd, s)
}
#[no_mangle]
pub unsafe extern "C" fn __fxstatat(_v: c_int, d: c_int, p: CP, s: P, f: c_int) -> c_int {
    fl::file::fstatat(d, p as *const _, s, f)
}
#[no_mangle]
pub unsafe extern "C" fn __fxstatat64(_v: c_int, d: c_int, p: CP, s: P, f: c_int) -> c_int {
    fl::file::fstatat(d, p as *const _, s, f)
}
#[no_mangle]
pub unsafe extern "C" fn statvfs64(p: CP, s: P) -> c_int {
    fl::file::statvfs(p as *const _, s as *mut _)
}
#[no_mangle]
pub unsafe extern "C" fn fstatvfs(fd: c_int, out: P) -> c_int {
    // fstatfs, then the statvfs layout flint-libc's statvfs fills
    let mut sf = [0u64; 15];
    let r = sc2(138, fd as usize, sf.as_mut_ptr() as usize);
    if r < 0 {
        return ret_i(r);
    }
    statfs_to_statvfs(&sf, out);
    0
}
#[no_mangle]
pub unsafe extern "C" fn fstatvfs64(fd: c_int, out: P) -> c_int {
    fstatvfs(fd, out)
}

/// struct statfs (x86-64): type, bsize, blocks, bfree, bavail, files, ffree, fsid(2x i32),
/// namelen, frsize, flags, spare[4]. struct statvfs: bsize, frsize, blocks, bfree, bavail,
/// files, ffree, favail, fsid, flag, namemax, spare[6].
unsafe fn statfs_to_statvfs(sf: &[u64; 15], out: P) {
    let o = out as *mut u64;
    ptr::write_bytes(o, 0, 14);
    *o.add(0) = sf[1];
    *o.add(1) = if sf[9] != 0 { sf[9] } else { sf[1] };
    *o.add(2) = sf[2];
    *o.add(3) = sf[3];
    *o.add(4) = sf[4];
    *o.add(5) = sf[5];
    *o.add(6) = sf[6];
    *o.add(7) = sf[6];
    *o.add(8) = sf[7];
    *o.add(9) = sf[10];
    *o.add(10) = sf[8];
}

#[no_mangle]
pub unsafe extern "C" fn lchmod(p: CP, m: u32) -> c_int {
    // fchmodat2 can change a symlink's mode where the file system allows; else ENOTSUP
    let r = sc4(452, (-100isize) as usize, p as usize, m as usize, 0x100);
    if r == -38 {
        fl::errno::set(95);
        return -1;
    }
    ret_i(r)
}
#[no_mangle]
pub unsafe extern "C" fn eaccess(p: CP, m: c_int) -> c_int {
    ret_i(sc4(439 /* faccessat2 */, (-100isize) as usize, p as usize, m as usize, 0x200 /* AT_EACCESS */))
}
#[no_mangle]
pub unsafe extern "C" fn euidaccess(p: CP, m: c_int) -> c_int {
    eaccess(p, m)
}

#[repr(C)]
struct Timeval {
    s: i64,
    us: i64,
}

unsafe fn utimes_at(p: CP, tv: *const Timeval, flags: c_int) -> c_int {
    if tv.is_null() {
        return ret_i(sc4(280, (-100isize) as usize, p as usize, 0, flags as usize));
    }
    let ts = [(*tv).s, (*tv).us * 1000, (*tv.add(1)).s, (*tv.add(1)).us * 1000];
    ret_i(sc4(280, (-100isize) as usize, p as usize, ts.as_ptr() as usize, flags as usize))
}
#[no_mangle]
pub unsafe extern "C" fn utimes(p: CP, tv: *const c_void) -> c_int {
    utimes_at(p, tv as *const Timeval, 0)
}
#[no_mangle]
pub unsafe extern "C" fn lutimes(p: CP, tv: *const c_void) -> c_int {
    utimes_at(p, tv as *const Timeval, 0x100)
}
#[no_mangle]
pub unsafe extern "C" fn futimes(fd: c_int, tv: *const c_void) -> c_int {
    let tv = tv as *const Timeval;
    let ts = if tv.is_null() { None } else { Some([(*tv).s, (*tv).us * 1000, (*tv.add(1)).s, (*tv.add(1)).us * 1000]) };
    ret_i(sc4(280, fd as usize, 0, ts.as_ref().map_or(0, |t| t.as_ptr() as usize), 0))
}
#[no_mangle]
pub unsafe extern "C" fn futimens(fd: c_int, ts: *const c_void) -> c_int {
    ret_i(sc4(280, fd as usize, 0, ts as usize, 0))
}
#[no_mangle]
pub unsafe extern "C" fn utime(p: CP, b: *const i64) -> c_int {
    if b.is_null() {
        return utimes_at(p, ptr::null(), 0);
    }
    let tv = [Timeval { s: *b, us: 0 }, Timeval { s: *b.add(1), us: 0 }];
    utimes_at(p, tv.as_ptr(), 0)
}

// --- directories ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn readdir64(d: P) -> P {
    fl::dir::readdir(d as *mut _)
}

/// dirent: d_ino u64, d_off i64, d_reclen u16, d_type u8, d_name[...]
unsafe fn dname(e: *const c_void) -> *const c_char {
    (e as *const u8).add(19)
}

#[no_mangle]
pub unsafe extern "C" fn alphasort(a: *const *const c_void, b: *const *const c_void) -> c_int {
    super::string::strcoll(dname(*a), dname(*b))
}
#[no_mangle]
pub unsafe extern "C" fn alphasort64(a: *const *const c_void, b: *const *const c_void) -> c_int {
    alphasort(a, b)
}
#[no_mangle]
pub unsafe extern "C" fn versionsort(a: *const *const c_void, b: *const *const c_void) -> c_int {
    super::string::strverscmp(dname(*a), dname(*b))
}
#[no_mangle]
pub unsafe extern "C" fn versionsort64(a: *const *const c_void, b: *const *const c_void) -> c_int {
    versionsort(a, b)
}

type Filter = Option<unsafe extern "C" fn(*const c_void) -> c_int>;
type Compar = Option<unsafe extern "C" fn(*const *const c_void, *const *const c_void) -> c_int>;

#[no_mangle]
pub unsafe extern "C" fn scandirat(dfd: c_int, path: CP, out: *mut *mut P, filter: Filter, cmp: Compar) -> c_int {
    let fd = fl::file::openat(dfd, path as *const _, 0o200000 | 0o2000000 /* O_DIRECTORY|O_CLOEXEC */, 0);
    if fd < 0 {
        return -1;
    }
    let d = fl::dir::fdopendir(fd);
    if d.is_null() {
        sc1(fl::sys::CLOSE, fd as usize);
        return -1;
    }
    let mut v: Vec<P> = Vec::new();
    loop {
        let e = fl::dir::readdir(d);
        if e.is_null() {
            break;
        }
        if let Some(f) = filter {
            if f(e) == 0 {
                continue;
            }
        }
        let reclen = *((e as *const u8).add(16) as *const u16) as usize;
        let c = umem::malloc(reclen);
        if c.is_null() {
            break;
        }
        ptr::copy_nonoverlapping(e as *const u8, c as *mut u8, reclen);
        v.push(c);
    }
    fl::dir::closedir(d);
    if let Some(c) = cmp {
        v.sort_by(|a, b| {
            let r = c(a as *const P as *const *const c_void, b as *const P as *const *const c_void);
            r.cmp(&0)
        });
    }
    let arr = umem::malloc(v.len().max(1) * 8) as *mut P;
    if arr.is_null() {
        return -1;
    }
    ptr::copy_nonoverlapping(v.as_ptr(), arr, v.len());
    *out = arr;
    v.len() as c_int
}
#[no_mangle]
pub unsafe extern "C" fn scandir(path: CP, out: *mut *mut P, filter: Filter, cmp: Compar) -> c_int {
    scandirat(-100, path, out, filter, cmp)
}
#[no_mangle]
pub unsafe extern "C" fn scandir64(path: CP, out: *mut *mut P, filter: Filter, cmp: Compar) -> c_int {
    scandirat(-100, path, out, filter, cmp)
}
#[no_mangle]
pub unsafe extern "C" fn scandirat64(dfd: c_int, path: CP, out: *mut *mut P, filter: Filter, cmp: Compar) -> c_int {
    scandirat(dfd, path, out, filter, cmp)
}
#[no_mangle]
pub unsafe extern "C" fn get_current_dir_name() -> *mut c_char {
    let pwd = fl::env::getenv(b"PWD\0".as_ptr());
    if !pwd.is_null() {
        let mut a = [0u64; 18];
        let mut b = [0u64; 18];
        if fl::file::stat(pwd as *const _, a.as_mut_ptr() as P) == 0 && fl::file::stat(b".\0".as_ptr() as *const _, b.as_mut_ptr() as P) == 0 && a[0] == b[0] && a[1] == b[1] {
            return super::string::strdup(pwd);
        }
    }
    fl::file::getcwd(ptr::null_mut(), 0) as *mut c_char
}

#[no_mangle]
pub unsafe extern "C" fn pathconf(_p: CP, name: c_int) -> c_long {
    conf(name)
}
#[no_mangle]
pub unsafe extern "C" fn fpathconf(_fd: c_int, name: c_int) -> c_long {
    conf(name)
}
fn conf(name: c_int) -> c_long {
    match name {
        0 => 127,   // _PC_LINK_MAX
        1 => 255,   // _PC_MAX_CANON
        2 => 255,   // _PC_MAX_INPUT
        3 => 255,   // _PC_NAME_MAX
        4 => 4096,  // _PC_PATH_MAX
        5 => 4096,  // _PC_PIPE_BUF
        6 => 1,     // _PC_CHOWN_RESTRICTED
        7 => 1,     // _PC_NO_TRUNC
        8 => 0,     // _PC_VDISABLE
        _ => -1,
    }
}

// --- memory -------------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn reallocarray(p: P, n: size_t, s: size_t) -> P {
    match n.checked_mul(s) {
        Some(t) => umem::realloc(p, t),
        None => {
            fl::errno::set(fl::errno::ENOMEM);
            ptr::null_mut()
        }
    }
}
#[no_mangle]
pub unsafe extern "C" fn memalign(align: size_t, n: size_t) -> P {
    let mut p = ptr::null_mut();
    if fl::malloc::posix_memalign(&mut p, align.max(8).next_power_of_two(), n) != 0 {
        return ptr::null_mut();
    }
    p as P
}
#[no_mangle]
pub unsafe extern "C" fn valloc(n: size_t) -> P {
    memalign(4096, n)
}
#[no_mangle]
pub unsafe extern "C" fn pvalloc(n: size_t) -> P {
    memalign(4096, (n + 4095) & !4095)
}
#[no_mangle]
pub unsafe extern "C" fn malloc_trim(_pad: size_t) -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn malloc_stats() {}
#[no_mangle]
pub unsafe extern "C" fn mallopt(_p: c_int, _v: c_int) -> c_int {
    1
}
#[no_mangle]
pub unsafe extern "C" fn mallinfo(out: *mut [c_int; 10]) -> *mut [c_int; 10] {
    // returned in memory (struct of 10 ints): all zero
    *out = [0; 10];
    out
}
#[no_mangle]
pub unsafe extern "C" fn mallinfo2(out: *mut [usize; 10]) -> *mut [usize; 10] {
    *out = [0; 10];
    out
}

// --- processes ------------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn getpagesize() -> c_int {
    4096
}

unsafe fn count_cpus() -> c_int {
    let mut set = [0u8; 128];
    let r = sc3(fl::sys::SCHED_GETAFFINITY, 0, 128, set.as_mut_ptr() as usize);
    if r <= 0 {
        return 1;
    }
    set[..r as usize].iter().map(|b| b.count_ones() as c_int).sum::<c_int>().max(1)
}
#[no_mangle]
pub unsafe extern "C" fn get_nprocs() -> c_int {
    count_cpus()
}
#[no_mangle]
pub unsafe extern "C" fn get_nprocs_conf() -> c_int {
    // /sys/devices/system/cpu/possible would be exact; the online count is close enough
    count_cpus()
}
#[no_mangle]
pub unsafe extern "C" fn get_phys_pages() -> c_long {
    let mut info = [0u64; 14];
    sc1(99, info.as_mut_ptr() as usize);
    (info[4] * (info[13] & 0xffff_ffff).max(1) / 4096) as c_long
}
#[no_mangle]
pub unsafe extern "C" fn get_avphys_pages() -> c_long {
    let mut info = [0u64; 14];
    sc1(99, info.as_mut_ptr() as usize);
    (info[5] * (info[13] & 0xffff_ffff).max(1) / 4096) as c_long
}

#[no_mangle]
pub unsafe extern "C" fn sleep(s: u32) -> u32 {
    let mut ts = [s as i64, 0];
    let r = sc2(35, ts.as_ptr() as usize, ts.as_mut_ptr() as usize);
    if r == -4 {
        return ts[0] as u32 + (ts[1] > 0) as u32;
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn usleep(us: u32) -> c_int {
    let ts = [(us / 1_000_000) as i64, ((us % 1_000_000) * 1000) as i64];
    ret_i(sc2(35, ts.as_ptr() as usize, 0))
}

#[no_mangle]
pub unsafe extern "C" fn system(cmd: CP) -> c_int {
    if cmd.is_null() {
        return 1;
    }
    let pid = fl::process::fork();
    if pid < 0 {
        return -1;
    }
    if pid == 0 {
        let argv = [b"sh\0".as_ptr(), b"-c\0".as_ptr(), cmd, ptr::null()];
        sc3(fl::sys::EXECVE, b"/bin/sh\0".as_ptr() as usize, argv.as_ptr() as usize, *fl::env::CELL as usize);
        sc1(fl::sys::EXIT_GROUP, 127);
    }
    let mut st = 0i32;
    loop {
        let r = sc4(fl::sys::WAIT4, pid as usize, &mut st as *mut i32 as usize, 0, 0);
        if r != -4 {
            break;
        }
    }
    st
}

use super::printf::{RawVa, Va};

unsafe fn collect_args(first: CP, args: &mut Va) -> Vec<CP> {
    let mut v = alloc::vec![first];
    if first.is_null() {
        return v;
    }
    loop {
        let p: CP = args.int() as CP;
        v.push(p);
        if p.is_null() {
            break;
        }
    }
    v
}

#[no_mangle]
pub unsafe extern "C" fn execl(path: CP, a0: CP, mut args: ...) -> c_int {
    let mut va = Va(&mut args as *mut _ as *mut RawVa);
    let v = collect_args(a0, &mut va);
    fl::process::execve(path as *const _, v.as_ptr() as *const _, *fl::env::CELL as *const _)
}
#[no_mangle]
pub unsafe extern "C" fn execlp(file: CP, a0: CP, mut args: ...) -> c_int {
    let mut va = Va(&mut args as *mut _ as *mut RawVa);
    let v = collect_args(a0, &mut va);
    fl::process::execvp(file as *const _, v.as_ptr() as *const _)
}
#[no_mangle]
pub unsafe extern "C" fn execle(path: CP, a0: CP, mut args: ...) -> c_int {
    let mut va = Va(&mut args as *mut _ as *mut RawVa);
    let v = collect_args(a0, &mut va);
    let env = va.int() as *const *const c_char;
    fl::process::execve(path as *const _, v.as_ptr() as *const _, env as *const _)
}
#[no_mangle]
pub unsafe extern "C" fn execvpe(file: CP, argv: *const CP, envp: *const CP) -> c_int {
    // search PATH with the given environment
    let f = fl::mem::cstr(file);
    if f.contains(&b'/') {
        return fl::process::execve(file as *const _, argv as *const _, envp as *const _);
    }
    let path = fl::env::getenv(b"PATH\0".as_ptr());
    let path: &[u8] = if path.is_null() { b"/usr/local/bin:/bin:/usr/bin" } else { fl::mem::cstr(path) };
    let mut last = 2;
    for dir in path.split(|&c| c == b':') {
        let mut full: Vec<u8> = Vec::new();
        full.extend_from_slice(if dir.is_empty() { b"." } else { dir });
        full.push(b'/');
        full.extend_from_slice(f);
        full.push(0);
        fl::process::execve(full.as_ptr() as *const _, argv as *const _, envp as *const _);
        let e = fl::errno::get();
        if e != 2 && e != 20 {
            last = e;
        }
    }
    fl::errno::set(last);
    -1
}
#[no_mangle]
pub unsafe extern "C" fn fexecve(fd: c_int, argv: *const CP, envp: *const CP) -> c_int {
    ret_i(sc5(322 /* execveat */, fd as usize, b"\0".as_ptr() as usize, argv as usize, envp as usize, 0x1000 /* AT_EMPTY_PATH */))
}

// glibc's clone(fn, stack, flags, arg, parent_tid, tls, child_tid)
core::arch::global_asm!(
    ".globl clone",
    ".type clone,@function",
    "clone:",
    "test rdi, rdi",
    "jz 9f",
    "test rsi, rsi",
    "jz 9f",
    // put fn and arg on the child's stack
    "and rsi, -16",
    "sub rsi, 16",
    "mov qword ptr [rsi + 8], rcx", // arg
    "mov qword ptr [rsi], rdi",     // fn
    // syscall(flags, stack, ptid, ctid, tls)
    "mov rdi, rdx",
    "mov rdx, r8",
    "mov r8, r9",
    "mov r10, qword ptr [rsp + 8]",
    "mov eax, 56",
    "syscall",
    "test rax, rax",
    "jl 8f",
    "jz 1f",
    "ret",
    "1:",
    "xor ebp, ebp",
    "pop rax",
    "pop rdi",
    "call rax",
    "mov edi, eax",
    "mov eax, 60",
    "syscall",
    "hlt",
    "8:",
    "neg eax",
    "mov edi, eax",
    "jmp {seterr}",
    "9:",
    "mov edi, 22",
    "jmp {seterr}",
    seterr = sym clone_err,
);

unsafe extern "C" fn clone_err(e: c_int) -> c_int {
    fl::errno::set(e);
    -1
}

// --- environment ------------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn secure_getenv(name: CP) -> *mut c_char {
    if fl::misc::getauxval(23 /* AT_SECURE */) != 0 {
        return ptr::null_mut();
    }
    fl::env::getenv(name)
}
#[no_mangle]
pub unsafe extern "C" fn __secure_getenv(name: CP) -> *mut c_char {
    secure_getenv(name)
}

#[no_mangle]
pub unsafe extern "C" fn putenv(s: *mut c_char) -> c_int {
    let b = fl::mem::cstr(s);
    let Some(eq) = b.iter().position(|&c| c == b'=') else {
        // no '=': remove the variable, as glibc does
        let mut n = b.to_vec();
        n.push(0);
        return fl::env::unsetenv(n.as_ptr());
    };
    let name = &b[..eq];
    let env = *fl::env::CELL;
    let mut n = 0;
    if !env.is_null() {
        while !(*env.add(n)).is_null() {
            let e = fl::mem::cstr(*env.add(n));
            if e.len() > eq && e[eq] == b'=' && &e[..eq] == name {
                *env.add(n) = s;
                return 0;
            }
            n += 1;
        }
    }
    // a new array (the old one may be the kernel's)
    let a = fl::malloc::imalloc((n + 2) * 8) as *mut *mut c_char;
    if a.is_null() {
        return -1;
    }
    if n > 0 {
        ptr::copy_nonoverlapping(env, a, n);
    }
    *a.add(n) = s;
    *a.add(n + 1) = ptr::null_mut();
    *fl::env::CELL = a;
    0
}

// --- randomness -------------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn getentropy(b: P, n: size_t) -> c_int {
    if n > 256 {
        fl::errno::set(5 /* EIO */);
        return -1;
    }
    let mut done = 0;
    while done < n {
        let r = sc3(318, b as usize + done, n - done, 0);
        if r == -4 {
            continue;
        }
        if r < 0 {
            fl::errno::set(-r as i32);
            return -1;
        }
        done += r as usize;
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn arc4random_buf(b: P, n: size_t) {
    let mut done = 0;
    while done < n {
        let r = sc3(318, b as usize + done, n - done, 0);
        if r > 0 {
            done += r as usize;
        }
    }
}
#[no_mangle]
pub unsafe extern "C" fn arc4random() -> u32 {
    let mut v = 0u32;
    arc4random_buf(&mut v as *mut u32 as P, 4);
    v
}
#[no_mangle]
pub unsafe extern "C" fn arc4random_uniform(upper: u32) -> u32 {
    if upper < 2 {
        return 0;
    }
    let min = upper.wrapping_neg() % upper;
    loop {
        let r = arc4random();
        if r >= min {
            return r % upper;
        }
    }
}

// --- signals ------------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn sigwait(set: *const u64, sig: *mut c_int) -> c_int {
    loop {
        let r = sc4(128 /* rt_sigtimedwait */, set as usize, 0, 0, 8);
        if r == -4 {
            continue;
        }
        if r < 0 {
            return -r as c_int;
        }
        *sig = r as c_int;
        return 0;
    }
}
#[no_mangle]
pub unsafe extern "C" fn sigwaitinfo(set: *const u64, info: P) -> c_int {
    ret_i(sc4(128, set as usize, info as usize, 0, 8))
}
#[no_mangle]
pub unsafe extern "C" fn sigisemptyset(set: *const u64) -> c_int {
    (*set == 0) as c_int
}
#[no_mangle]
pub unsafe extern "C" fn sigandset(d: *mut u64, a: *const u64, b: *const u64) -> c_int {
    *d = *a & *b;
    0
}
#[no_mangle]
pub unsafe extern "C" fn sigorset(d: *mut u64, a: *const u64, b: *const u64) -> c_int {
    *d = *a | *b;
    0
}
#[no_mangle]
pub unsafe extern "C" fn sigpending(s: *mut u64) -> c_int {
    sigpending_raw(s as P, 8)
}
#[no_mangle]
pub unsafe extern "C" fn sigsuspend(s: *const u64) -> c_int {
    ret_i(sc2(130, s as usize, 8))
}
#[no_mangle]
pub unsafe extern "C" fn siginterrupt(sig: c_int, flag: c_int) -> c_int {
    let mut sa = [0usize; 4 + 16];
    if fl::signal::sigaction(sig, ptr::null(), sa.as_mut_ptr() as *mut _) < 0 {
        return -1;
    }
    // sa_flags is the int after handler and the 128-byte mask
    let flags = (sa.as_mut_ptr() as *mut u8).add(8 + 128) as *mut c_int;
    if flag != 0 {
        *flags &= !0x10000000;
    } else {
        *flags |= 0x10000000; // SA_RESTART
    }
    fl::signal::sigaction(sig, sa.as_ptr() as *const _, ptr::null_mut())
}
#[no_mangle]
pub unsafe extern "C" fn sigqueue(pid: c_int, sig: c_int, val: usize) -> c_int {
    let mut info = [0u8; 128];
    *(info.as_mut_ptr() as *mut c_int) = sig;
    *(info.as_mut_ptr().add(8) as *mut c_int) = -1; // SI_QUEUE
    *(info.as_mut_ptr().add(16) as *mut c_int) = sc0(39) as c_int;
    *(info.as_mut_ptr().add(20) as *mut u32) = sc0(102) as u32;
    *(info.as_mut_ptr().add(24) as *mut usize) = val;
    rt_sigqueueinfo(pid, sig, info.as_mut_ptr() as P)
}
#[no_mangle]
pub unsafe extern "C" fn killpg(pg: c_int, sig: c_int) -> c_int {
    ret_i(sc2(62, (-pg) as usize, sig as usize))
}
#[no_mangle]
pub unsafe extern "C" fn psignal(sig: c_int, s: CP) {
    let f = super::stdio::stderr_file();
    let _l = super::stdio::locked(f);
    if !s.is_null() && *s != 0 {
        super::stdio::put_bytes(f, fl::mem::cstr(s));
        super::stdio::put_bytes(f, b": ");
    }
    super::stdio::put_bytes(f, fl::mem::cstr(super::string::strsignal(sig)));
    super::stdio::put_bytes(f, b"\n");
}

#[no_mangle]
pub unsafe extern "C" fn shm_open(name: CP, flags: c_int, mode: u32) -> c_int {
    let n = fl::mem::cstr(name);
    let n = if n.first() == Some(&b'/') { &n[1..] } else { n };
    let mut p: Vec<u8> = b"/dev/shm/".to_vec();
    p.extend_from_slice(n);
    p.push(0);
    fl::file::open(p.as_ptr() as *const _, flags | 0o2000000 | 0o400000 /* CLOEXEC|NOFOLLOW */, mode)
}
#[no_mangle]
pub unsafe extern "C" fn shm_unlink(name: CP) -> c_int {
    let n = fl::mem::cstr(name);
    let n = if n.first() == Some(&b'/') { &n[1..] } else { n };
    let mut p: Vec<u8> = b"/dev/shm/".to_vec();
    p.extend_from_slice(n);
    p.push(0);
    fl::file::unlink(p.as_ptr() as *const _)
}
