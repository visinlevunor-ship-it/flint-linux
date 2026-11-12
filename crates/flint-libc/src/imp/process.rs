//! Processes: fork, exec, wait, ids, sessions, and posix_spawn (which std's Command uses).

use crate::imp::errno::{self, *};
use crate::imp::sys::{self, ret_i, sc0, sc1, sc2, sc3, sc4, sc5};
use crate::imp::types::*;
use crate::imp::{env, malloc, mem, start, thread};
use core::ptr;

#[no_mangle]
pub unsafe extern "C" fn fork() -> pid_t {
    // hold the heap lock across fork so the child gets a consistent heap
    let r = {
        crate::imp::malloc::lock_for_fork();
        let r = sc0(sys::FORK);
        crate::imp::malloc::unlock_after_fork(r == 0);
        r
    };
    if r == 0 {
        thread::after_fork();
    }
    ret_i(r)
}

#[no_mangle]
pub unsafe extern "C" fn execve(path: *const c_char, argv: *const *const c_char, envp: *const *const c_char) -> c_int {
    ret_i(sc3(sys::EXECVE, path as usize, argv as usize, envp as usize))
}

const DEFAULT_PATH: &[u8] = b"/usr/local/bin:/bin:/usr/bin";

/// execvp with the environment given (execvp itself passes environ). Uses only the stack, so
/// it is safe in a child forked from a threaded program.
unsafe fn exec_search(file: *const c_char, argv: *const *const c_char, envp: *const *const c_char) -> c_int {
    let f = mem::cstr(file as *const u8);
    if f.is_empty() {
        errno::set(ENOENT);
        return -1;
    }
    if f.contains(&b'/') {
        execve(file, argv, envp);
        if errno::get() == ENOEXEC {
            exec_script(file, argv, envp);
        }
        return -1;
    }
    let path = {
        let p = env::getenv(b"PATH\0".as_ptr());
        if p.is_null() { DEFAULT_PATH } else { mem::cstr(p) }
    };
    let mut seen_eacces = false;
    let mut buf = [0u8; 4096];
    for dir in path.split(|&c| c == b':') {
        let dir = if dir.is_empty() { &b"."[..] } else { dir };
        if dir.len() + f.len() + 2 > buf.len() {
            continue;
        }
        buf[..dir.len()].copy_from_slice(dir);
        buf[dir.len()] = b'/';
        buf[dir.len() + 1..dir.len() + 1 + f.len()].copy_from_slice(f);
        buf[dir.len() + 1 + f.len()] = 0;
        execve(buf.as_ptr() as *const c_char, argv, envp);
        match errno::get() {
            EACCES => seen_eacces = true,
            ENOENT | ENOTDIR => {}
            ENOEXEC => {
                exec_script(buf.as_ptr() as *const c_char, argv, envp);
                return -1;
            }
            _ => return -1,
        }
    }
    errno::set(if seen_eacces { EACCES } else { ENOENT });
    -1
}

/// A file without a #! line that the kernel would not run: hand it to /bin/sh, as POSIX asks.
unsafe fn exec_script(path: *const c_char, argv: *const *const c_char, envp: *const *const c_char) {
    let mut args: [*const c_char; 256] = [ptr::null(); 256];
    args[0] = b"sh\0".as_ptr() as *const c_char;
    args[1] = path;
    let mut i = 1;
    while i < 254 && !(*argv.add(i)).is_null() {
        args[i + 1] = *argv.add(i);
        i += 1;
    }
    execve(b"/bin/sh\0".as_ptr() as *const c_char, args.as_ptr(), envp);
    errno::set(ENOEXEC);
}

#[no_mangle]
pub unsafe extern "C" fn execvp(file: *const c_char, argv: *const *const c_char) -> c_int {
    exec_search(file, argv, env::environ as *const *const c_char)
}

#[no_mangle]
pub unsafe extern "C" fn execv(path: *const c_char, argv: *const *const c_char) -> c_int {
    execve(path, argv, env::environ as *const *const c_char)
}

#[no_mangle]
pub unsafe extern "C" fn waitpid(pid: pid_t, status: *mut c_int, options: c_int) -> pid_t {
    ret_i(sc4(sys::WAIT4, pid as isize as usize, status as usize, options as usize, 0))
}

#[no_mangle]
pub unsafe extern "C" fn wait(status: *mut c_int) -> pid_t {
    waitpid(-1, status, 0)
}

#[no_mangle]
pub unsafe extern "C" fn waitid(idtype: c_int, id: c_uint, info: *mut c_void, options: c_int) -> c_int {
    ret_i(sc5(sys::WAITID, idtype as usize, id as usize, info as usize, options as usize, 0))
}

#[no_mangle]
pub unsafe extern "C" fn getpid() -> pid_t {
    sc0(sys::GETPID) as pid_t
}
#[no_mangle]
pub unsafe extern "C" fn getppid() -> pid_t {
    sc0(110) as pid_t
}
#[no_mangle]
pub unsafe extern "C" fn gettid() -> pid_t {
    sc0(sys::GETTID) as pid_t
}
#[no_mangle]
pub unsafe extern "C" fn getuid() -> uid_t {
    sc0(sys::GETUID) as uid_t
}
#[no_mangle]
pub unsafe extern "C" fn geteuid() -> uid_t {
    sc0(sys::GETEUID) as uid_t
}
#[no_mangle]
pub unsafe extern "C" fn getgid() -> gid_t {
    sc0(sys::GETGID) as gid_t
}
#[no_mangle]
pub unsafe extern "C" fn getegid() -> gid_t {
    sc0(sys::GETEGID) as gid_t
}
#[no_mangle]
pub unsafe extern "C" fn setuid(uid: uid_t) -> c_int {
    // a threaded program would need every thread changed (the kernel's ids are per thread);
    // Flint's setuid callers (su, login) are single-threaded
    ret_i(sc1(sys::SETUID, uid as usize))
}
#[no_mangle]
pub unsafe extern "C" fn setgid(gid: gid_t) -> c_int {
    ret_i(sc1(sys::SETGID, gid as usize))
}
#[no_mangle]
pub unsafe extern "C" fn setgroups(n: size_t, list: *const gid_t) -> c_int {
    ret_i(sc2(sys::SETGROUPS, n, list as usize))
}
#[no_mangle]
pub unsafe extern "C" fn getgroups(n: c_int, list: *mut gid_t) -> c_int {
    ret_i(sc2(sys::GETGROUPS, n as usize, list as usize))
}
#[no_mangle]
pub unsafe extern "C" fn setsid() -> pid_t {
    ret_i(sc0(sys::SETSID))
}
#[no_mangle]
pub unsafe extern "C" fn setpgid(pid: pid_t, pgid: pid_t) -> c_int {
    ret_i(sc2(sys::SETPGID, pid as usize, pgid as usize))
}
#[no_mangle]
pub unsafe extern "C" fn getpgrp() -> pid_t {
    sc0(sys::GETPGRP) as pid_t
}

// --- posix_spawn ---

/// musl's posix_spawnattr_t (336 bytes).
#[repr(C)]
pub struct SpawnAttr {
    flags: c_short,
    pgrp: pid_t,
    sigdef: sigset_t,
    sigmask: sigset_t,
    prio: c_int,
    policy: c_int,
    _pad: [c_int; 16],
}

/// musl's posix_spawn_file_actions_t (80 bytes); `actions` points at our list.
#[repr(C)]
pub struct FileActions {
    allocated: c_int,
    used: c_int,
    actions: *mut Action,
    _pad: [c_int; 16],
}

#[repr(C)]
struct Action {
    next: *mut Action,
    kind: c_int,
    fd: c_int,
    fd2: c_int,
    flags: c_int,
    mode: mode_t,
    path: *mut u8, // owned copy, for open and chdir
}

const FA_CLOSE: c_int = 1;
const FA_DUP2: c_int = 2;
const FA_OPEN: c_int = 3;
const FA_CHDIR: c_int = 4;
const FA_FCHDIR: c_int = 5;

const SPAWN_RESETIDS: c_short = 1;
const SPAWN_SETPGROUP: c_short = 2;
const SPAWN_SETSIGDEF: c_short = 4;
const SPAWN_SETSIGMASK: c_short = 8;
const SPAWN_SETSID: c_short = 0x80;

#[no_mangle]
pub unsafe extern "C" fn posix_spawnattr_init(a: *mut SpawnAttr) -> c_int {
    ptr::write_bytes(a as *mut u8, 0, core::mem::size_of::<SpawnAttr>());
    0
}
#[no_mangle]
pub unsafe extern "C" fn posix_spawnattr_destroy(_a: *mut SpawnAttr) -> c_int {
    0
}
#[no_mangle]
pub unsafe extern "C" fn posix_spawnattr_setflags(a: *mut SpawnAttr, f: c_short) -> c_int {
    (*a).flags = f;
    0
}
#[no_mangle]
pub unsafe extern "C" fn posix_spawnattr_setpgroup(a: *mut SpawnAttr, pg: pid_t) -> c_int {
    (*a).pgrp = pg;
    0
}
#[no_mangle]
pub unsafe extern "C" fn posix_spawnattr_setsigdefault(a: *mut SpawnAttr, s: *const sigset_t) -> c_int {
    (*a).sigdef = *s;
    0
}
#[no_mangle]
pub unsafe extern "C" fn posix_spawnattr_setsigmask(a: *mut SpawnAttr, s: *const sigset_t) -> c_int {
    (*a).sigmask = *s;
    0
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawn_file_actions_init(fa: *mut FileActions) -> c_int {
    ptr::write_bytes(fa as *mut u8, 0, core::mem::size_of::<FileActions>());
    0
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawn_file_actions_destroy(fa: *mut FileActions) -> c_int {
    let mut a = (*fa).actions;
    while !a.is_null() {
        let next = (*a).next;
        if !(*a).path.is_null() {
            malloc::free((*a).path);
        }
        malloc::free(a as *mut u8);
        a = next;
    }
    (*fa).actions = ptr::null_mut();
    0
}

unsafe fn add_action(fa: *mut FileActions, kind: c_int, fd: c_int, fd2: c_int, path: *const c_char, flags: c_int, mode: mode_t) -> c_int {
    let a = malloc::malloc(core::mem::size_of::<Action>()) as *mut Action;
    if a.is_null() {
        return ENOMEM;
    }
    let p = if path.is_null() {
        ptr::null_mut()
    } else {
        let n = mem::cstrlen(path as *const u8) + 1;
        let p = malloc::malloc(n);
        if p.is_null() {
            malloc::free(a as *mut u8);
            return ENOMEM;
        }
        ptr::copy_nonoverlapping(path as *const u8, p, n);
        p
    };
    *a = Action { next: ptr::null_mut(), kind, fd, fd2, flags, mode, path: p };
    // keep the order they were added in
    let mut slot = &mut (*fa).actions as *mut *mut Action;
    while !(*slot).is_null() {
        slot = &mut (**slot).next;
    }
    *slot = a;
    0
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawn_file_actions_addclose(fa: *mut FileActions, fd: c_int) -> c_int {
    add_action(fa, FA_CLOSE, fd, 0, ptr::null(), 0, 0)
}
#[no_mangle]
pub unsafe extern "C" fn posix_spawn_file_actions_adddup2(fa: *mut FileActions, fd: c_int, to: c_int) -> c_int {
    add_action(fa, FA_DUP2, fd, to, ptr::null(), 0, 0)
}
#[no_mangle]
pub unsafe extern "C" fn posix_spawn_file_actions_addopen(fa: *mut FileActions, fd: c_int, path: *const c_char, flags: c_int, mode: mode_t) -> c_int {
    add_action(fa, FA_OPEN, fd, 0, path, flags, mode)
}
#[no_mangle]
pub unsafe extern "C" fn posix_spawn_file_actions_addchdir_np(fa: *mut FileActions, path: *const c_char) -> c_int {
    add_action(fa, FA_CHDIR, 0, 0, path, 0, 0)
}
#[no_mangle]
pub unsafe extern "C" fn posix_spawn_file_actions_addfchdir_np(fa: *mut FileActions, fd: c_int) -> c_int {
    add_action(fa, FA_FCHDIR, fd, 0, ptr::null(), 0, 0)
}

/// In the child: apply attributes and file actions, exec, and on failure send errno up the pipe.
unsafe fn spawn_child(
    path: *const c_char,
    fa: *const FileActions,
    attr: *const SpawnAttr,
    argv: *const *const c_char,
    envp: *const *const c_char,
    search: bool,
    report: c_int,
) -> ! {
    let fail = |e: i32| -> ! {
        sc3(sys::WRITE, report as usize, &e as *const i32 as usize, 4);
        start::_exit(127)
    };
    if let Some(a) = attr.as_ref() {
        if a.flags & SPAWN_SETSID != 0 && sc0(sys::SETSID) < 0 {
            fail(errno::EPERM);
        }
        if a.flags & SPAWN_SETPGROUP != 0 {
            let r = sc2(sys::SETPGID, 0, a.pgrp as usize);
            if r < 0 {
                fail((-r) as i32);
            }
        }
        if a.flags & SPAWN_RESETIDS != 0 {
            sc1(sys::SETGID, sc0(sys::GETGID) as usize);
            sc1(sys::SETUID, sc0(sys::GETUID) as usize);
        }
        if a.flags & SPAWN_SETSIGDEF != 0 {
            let k = [0usize; 4]; // SIG_DFL
            for sig in 1..=64usize {
                if a.sigdef.val[0] >> (sig - 1) & 1 == 1 && sig != 9 && sig != 19 {
                    sc4(sys::RT_SIGACTION, sig, k.as_ptr() as usize, 0, 8);
                }
            }
        }
    }
    let mut act = fa.as_ref().map(|f| f.actions).unwrap_or(ptr::null_mut());
    while let Some(a) = act.as_ref() {
        let r = match a.kind {
            FA_CLOSE => sc1(sys::CLOSE, a.fd as usize),
            FA_DUP2 => {
                if a.fd == a.fd2 {
                    // dup2 onto itself would keep FD_CLOEXEC: clear it instead
                    sc3(sys::FCNTL, a.fd as usize, 2, 0)
                } else {
                    sc2(sys::DUP2, a.fd as usize, a.fd2 as usize)
                }
            }
            FA_OPEN => {
                let fd = sc3(sys::OPEN, a.path as usize, a.flags as usize, a.mode as usize);
                if fd >= 0 && fd as c_int != a.fd {
                    let r = sc2(sys::DUP2, fd as usize, a.fd as usize);
                    sc1(sys::CLOSE, fd as usize);
                    r
                } else {
                    fd
                }
            }
            FA_CHDIR => sc1(sys::CHDIR, a.path as usize),
            FA_FCHDIR => sc1(81, a.fd as usize),
            _ => 0,
        };
        if r < 0 {
            fail((-r) as i32);
        }
        act = a.next;
    }
    // the signal mask last, so it does not get in the way of the steps above
    let mask = attr.as_ref().filter(|a| a.flags & SPAWN_SETSIGMASK != 0).map(|a| a.sigmask.val[0]);
    if let Some(m) = mask {
        sc4(sys::RT_SIGPROCMASK, 2 /* SIG_SETMASK */, &m as *const u64 as usize, 0, 8);
    }
    if search {
        exec_search(path, argv, envp);
    } else {
        execve(path, argv, envp);
    }
    fail(errno::get())
}

unsafe fn spawn(
    pid: *mut pid_t,
    path: *const c_char,
    fa: *const FileActions,
    attr: *const SpawnAttr,
    argv: *const *const c_char,
    envp: *const *const c_char,
    search: bool,
) -> c_int {
    let mut p = [0 as c_int; 2];
    if sc2(sys::PIPE2, p.as_mut_ptr() as usize, 0o2000000 /* O_CLOEXEC */) < 0 {
        return EAGAIN;
    }
    // block signals while forking: a handler in the child before exec would run parent code
    let all: u64 = !0;
    let mut old: u64 = 0;
    sc4(sys::RT_SIGPROCMASK, 0, &all as *const u64 as usize, &mut old as *mut u64 as usize, 8);
    let child = fork();
    if child == 0 {
        sc1(sys::CLOSE, p[0] as usize);
        // restore the parent's mask in the child (SETSIGMASK may replace it later)
        sc4(sys::RT_SIGPROCMASK, 2, &old as *const u64 as usize, 0, 8);
        spawn_child(path, fa, attr, argv, envp, search, p[1]);
    }
    sc4(sys::RT_SIGPROCMASK, 2, &old as *const u64 as usize, 0, 8);
    sc1(sys::CLOSE, p[1] as usize);
    if child < 0 {
        sc1(sys::CLOSE, p[0] as usize);
        return errno::get();
    }
    let mut e: i32 = 0;
    let n = loop {
        let n = sc3(sys::READ, p[0] as usize, &mut e as *mut i32 as usize, 4);
        if n != -(EINTR as isize) {
            break n;
        }
    };
    sc1(sys::CLOSE, p[0] as usize);
    if n == 4 {
        // exec failed: reap the child and report why
        let mut st = 0;
        sc4(sys::WAIT4, child as usize, &mut st as *mut c_int as usize, 0, 0);
        return e;
    }
    if !pid.is_null() {
        *pid = child;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawn(
    pid: *mut pid_t, path: *const c_char, fa: *const FileActions, attr: *const SpawnAttr,
    argv: *const *const c_char, envp: *const *const c_char,
) -> c_int {
    spawn(pid, path, fa, attr, argv, envp, false)
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawnp(
    pid: *mut pid_t, file: *const c_char, fa: *const FileActions, attr: *const SpawnAttr,
    argv: *const *const c_char, envp: *const *const c_char,
) -> c_int {
    spawn(pid, file, fa, attr, argv, envp, true)
}
