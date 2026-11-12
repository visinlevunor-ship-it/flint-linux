//! Files and the file system: thin wrappers over system calls, plus realpath and statvfs.

use crate::imp::sys::{self, ret, ret_i, sc1, sc2, sc3, sc4, sc5, sc6};
use crate::imp::types::*;
use crate::imp::{errno, malloc, mem};
use core::ptr;

#[no_mangle]
pub unsafe extern "C" fn open(path: *const c_char, flags: c_int, mode: mode_t) -> c_int {
    ret_i(sc3(sys::OPEN, path as usize, flags as usize, mode as usize))
}
#[no_mangle]
pub unsafe extern "C" fn openat(dir: c_int, path: *const c_char, flags: c_int, mode: mode_t) -> c_int {
    ret_i(sc4(sys::OPENAT, dir as isize as usize, path as usize, flags as usize, mode as usize))
}
#[no_mangle]
pub unsafe extern "C" fn close(fd: c_int) -> c_int {
    let r = sc1(sys::CLOSE, fd as isize as usize);
    // Linux closes the descriptor even when close is interrupted: not an error worth retrying
    if r == -(errno::EINTR as isize) {
        return 0;
    }
    ret_i(r)
}
#[no_mangle]
pub unsafe extern "C" fn read(fd: c_int, buf: *mut c_void, n: size_t) -> ssize_t {
    ret(sc3(sys::READ, fd as isize as usize, buf as usize, n))
}
#[no_mangle]
pub unsafe extern "C" fn write(fd: c_int, buf: *const c_void, n: size_t) -> ssize_t {
    ret(sc3(sys::WRITE, fd as isize as usize, buf as usize, n))
}
#[no_mangle]
pub unsafe extern "C" fn readv(fd: c_int, iov: *const iovec, n: c_int) -> ssize_t {
    ret(sc3(sys::READV, fd as isize as usize, iov as usize, n as usize))
}
#[no_mangle]
pub unsafe extern "C" fn writev(fd: c_int, iov: *const iovec, n: c_int) -> ssize_t {
    ret(sc3(sys::WRITEV, fd as isize as usize, iov as usize, n as usize))
}
#[no_mangle]
pub unsafe extern "C" fn lseek(fd: c_int, off: off_t, whence: c_int) -> off_t {
    ret(sc3(sys::LSEEK, fd as isize as usize, off as usize, whence as usize)) as off_t
}
#[no_mangle]
pub unsafe extern "C" fn fstat(fd: c_int, st: *mut c_void) -> c_int {
    ret_i(sc2(sys::FSTAT, fd as isize as usize, st as usize))
}
#[no_mangle]
pub unsafe extern "C" fn stat(path: *const c_char, st: *mut c_void) -> c_int {
    ret_i(sc2(sys::STAT, path as usize, st as usize))
}
#[no_mangle]
pub unsafe extern "C" fn lstat(path: *const c_char, st: *mut c_void) -> c_int {
    ret_i(sc2(sys::LSTAT, path as usize, st as usize))
}
#[no_mangle]
pub unsafe extern "C" fn fstatat(dir: c_int, path: *const c_char, st: *mut c_void, flags: c_int) -> c_int {
    ret_i(sc4(sys::NEWFSTATAT, dir as isize as usize, path as usize, st as usize, flags as usize))
}
#[no_mangle]
pub unsafe extern "C" fn access(path: *const c_char, mode: c_int) -> c_int {
    ret_i(sc2(sys::ACCESS, path as usize, mode as usize))
}
#[no_mangle]
pub unsafe extern "C" fn faccessat(dir: c_int, path: *const c_char, mode: c_int, _flags: c_int) -> c_int {
    ret_i(sc3(sys::FACCESSAT, dir as isize as usize, path as usize, mode as usize))
}
#[no_mangle]
pub unsafe extern "C" fn dup(fd: c_int) -> c_int {
    ret_i(sc1(sys::DUP, fd as isize as usize))
}
#[no_mangle]
pub unsafe extern "C" fn dup2(fd: c_int, to: c_int) -> c_int {
    ret_i(sc2(sys::DUP2, fd as isize as usize, to as isize as usize))
}
#[no_mangle]
pub unsafe extern "C" fn dup3(fd: c_int, to: c_int, flags: c_int) -> c_int {
    ret_i(sc3(sys::DUP3, fd as isize as usize, to as isize as usize, flags as usize))
}
#[no_mangle]
pub unsafe extern "C" fn pipe(fds: *mut c_int) -> c_int {
    ret_i(sc1(sys::PIPE, fds as usize))
}
#[no_mangle]
pub unsafe extern "C" fn pipe2(fds: *mut c_int, flags: c_int) -> c_int {
    ret_i(sc2(sys::PIPE2, fds as usize, flags as usize))
}
/// fcntl(fd, cmd, ...): every command's argument is a long or a pointer.
#[no_mangle]
pub unsafe extern "C" fn fcntl(fd: c_int, cmd: c_int, arg: usize) -> c_int {
    ret_i(sc3(sys::FCNTL, fd as isize as usize, cmd as usize, arg))
}
#[no_mangle]
pub unsafe extern "C" fn ioctl(fd: c_int, req: c_ulong, arg: usize) -> c_int {
    ret_i(sc3(sys::IOCTL, fd as isize as usize, req as usize, arg))
}
#[no_mangle]
pub unsafe extern "C" fn fsync(fd: c_int) -> c_int {
    ret_i(sc1(sys::FSYNC, fd as isize as usize))
}
#[no_mangle]
pub unsafe extern "C" fn sync() {
    sys::sc0(sys::SYNC);
}
#[no_mangle]
pub unsafe extern "C" fn getcwd(buf: *mut c_char, size: size_t) -> *mut c_char {
    let mut tmp = [0u8; 4096];
    let (b, n) = if buf.is_null() { (tmp.as_mut_ptr(), tmp.len()) } else { (buf as *mut u8, size) };
    if n == 0 {
        errno::set(errno::EINVAL);
        return ptr::null_mut();
    }
    if ret(sc2(sys::GETCWD, b as usize, n)) < 0 {
        return ptr::null_mut();
    }
    // the kernel reports an unreachable cwd (outside a chroot) as "(unreachable)/..."
    if *b != b'/' {
        errno::set(errno::ENOENT);
        return ptr::null_mut();
    }
    if buf.is_null() {
        let len = mem::cstrlen(b) + 1;
        let out = malloc::malloc(len);
        if out.is_null() {
            return ptr::null_mut();
        }
        ptr::copy_nonoverlapping(b, out, len);
        return out as *mut c_char;
    }
    buf
}
#[no_mangle]
pub unsafe extern "C" fn chdir(path: *const c_char) -> c_int {
    ret_i(sc1(sys::CHDIR, path as usize))
}
#[no_mangle]
pub unsafe extern "C" fn chroot(path: *const c_char) -> c_int {
    ret_i(sc1(sys::CHROOT, path as usize))
}
#[no_mangle]
pub unsafe extern "C" fn chmod(path: *const c_char, mode: mode_t) -> c_int {
    ret_i(sc2(sys::CHMOD, path as usize, mode as usize))
}
#[no_mangle]
pub unsafe extern "C" fn fchmod(fd: c_int, mode: mode_t) -> c_int {
    ret_i(sc2(sys::FCHMOD, fd as isize as usize, mode as usize))
}
#[no_mangle]
pub unsafe extern "C" fn fchmodat(dir: c_int, path: *const c_char, mode: mode_t, _flags: c_int) -> c_int {
    ret_i(sc3(sys::FCHMODAT, dir as isize as usize, path as usize, mode as usize))
}
#[no_mangle]
pub unsafe extern "C" fn fchown(fd: c_int, uid: uid_t, gid: gid_t) -> c_int {
    ret_i(sc3(sys::FCHOWN, fd as isize as usize, uid as usize, gid as usize))
}
#[no_mangle]
pub unsafe extern "C" fn lchown(path: *const c_char, uid: uid_t, gid: gid_t) -> c_int {
    ret_i(sc3(sys::LCHOWN, path as usize, uid as usize, gid as usize))
}
#[no_mangle]
pub unsafe extern "C" fn mkdir(path: *const c_char, mode: mode_t) -> c_int {
    ret_i(sc2(sys::MKDIR, path as usize, mode as usize))
}
#[no_mangle]
pub unsafe extern "C" fn rmdir(path: *const c_char) -> c_int {
    ret_i(sc1(sys::RMDIR, path as usize))
}
#[no_mangle]
pub unsafe extern "C" fn rename(from: *const c_char, to: *const c_char) -> c_int {
    ret_i(sc2(sys::RENAME, from as usize, to as usize))
}
#[no_mangle]
pub unsafe extern "C" fn unlink(path: *const c_char) -> c_int {
    ret_i(sc1(sys::UNLINK, path as usize))
}
#[no_mangle]
pub unsafe extern "C" fn unlinkat(dir: c_int, path: *const c_char, flags: c_int) -> c_int {
    ret_i(sc3(sys::UNLINKAT, dir as isize as usize, path as usize, flags as usize))
}
#[no_mangle]
pub unsafe extern "C" fn symlink(target: *const c_char, path: *const c_char) -> c_int {
    ret_i(sc2(sys::SYMLINK, target as usize, path as usize))
}
#[no_mangle]
pub unsafe extern "C" fn link(from: *const c_char, to: *const c_char) -> c_int {
    ret_i(sc2(sys::LINK, from as usize, to as usize))
}
#[no_mangle]
pub unsafe extern "C" fn linkat(fd1: c_int, p1: *const c_char, fd2: c_int, p2: *const c_char, flags: c_int) -> c_int {
    ret_i(sc5(sys::LINKAT, fd1 as isize as usize, p1 as usize, fd2 as isize as usize, p2 as usize, flags as usize))
}
#[no_mangle]
pub unsafe extern "C" fn readlink(path: *const c_char, buf: *mut c_char, n: size_t) -> ssize_t {
    ret(sc3(sys::READLINK, path as usize, buf as usize, n))
}
#[no_mangle]
pub unsafe extern "C" fn readlinkat(dir: c_int, path: *const c_char, buf: *mut c_char, n: size_t) -> ssize_t {
    ret(sc4(sys::READLINKAT, dir as isize as usize, path as usize, buf as usize, n))
}
#[no_mangle]
pub unsafe extern "C" fn mknod(path: *const c_char, mode: mode_t, dev: dev_t) -> c_int {
    ret_i(sc3(sys::MKNOD, path as usize, mode as usize, dev as usize))
}
#[no_mangle]
pub unsafe extern "C" fn mknodat(dir: c_int, path: *const c_char, mode: mode_t, dev: dev_t) -> c_int {
    ret_i(sc4(sys::MKNODAT, dir as isize as usize, path as usize, mode as usize, dev as usize))
}
#[no_mangle]
pub unsafe extern "C" fn mkfifo(path: *const c_char, mode: mode_t) -> c_int {
    mknod(path, mode | 0o010000, 0)
}
#[no_mangle]
pub unsafe extern "C" fn umask(mask: mode_t) -> mode_t {
    sc1(sys::UMASK, mask as usize) as mode_t
}
#[no_mangle]
pub unsafe extern "C" fn utimensat(dir: c_int, path: *const c_char, times: *const timespec, flags: c_int) -> c_int {
    ret_i(sc4(sys::UTIMENSAT, dir as isize as usize, path as usize, times as usize, flags as usize))
}
#[no_mangle]
pub unsafe extern "C" fn sendfile(out: c_int, inp: c_int, off: *mut off_t, n: size_t) -> ssize_t {
    ret(sc4(sys::SENDFILE, out as isize as usize, inp as isize as usize, off as usize, n))
}
#[no_mangle]
pub unsafe extern "C" fn splice(fd_in: c_int, off_in: *mut off_t, fd_out: c_int, off_out: *mut off_t, n: size_t, flags: c_uint) -> ssize_t {
    ret(sc6(sys::SPLICE, fd_in as isize as usize, off_in as usize, fd_out as isize as usize, off_out as usize, n, flags as usize))
}
#[no_mangle]
pub unsafe extern "C" fn poll(fds: *mut c_void, n: c_ulong, timeout: c_int) -> c_int {
    ret_i(sc3(sys::POLL, fds as usize, n as usize, timeout as isize as usize))
}
#[no_mangle]
pub unsafe extern "C" fn mmap(addr: *mut c_void, len: size_t, prot: c_int, flags: c_int, fd: c_int, off: off_t) -> *mut c_void {
    let r = sc6(sys::MMAP, addr as usize, len, prot as usize, flags as usize, fd as isize as usize, off as usize);
    if (-4095..0).contains(&r) {
        errno::set((-r) as i32);
        return usize::MAX as *mut c_void; // MAP_FAILED
    }
    r as *mut c_void
}
#[no_mangle]
pub unsafe extern "C" fn munmap(addr: *mut c_void, len: size_t) -> c_int {
    ret_i(sc2(sys::MUNMAP, addr as usize, len))
}
#[no_mangle]
pub unsafe extern "C" fn mprotect(addr: *mut c_void, len: size_t, prot: c_int) -> c_int {
    ret_i(sc3(sys::MPROTECT, addr as usize, len, prot as usize))
}
#[no_mangle]
pub unsafe extern "C" fn madvise(addr: *mut c_void, len: size_t, advice: c_int) -> c_int {
    ret_i(sc3(sys::MADVISE, addr as usize, len, advice as usize))
}
#[no_mangle]
pub unsafe extern "C" fn lgetxattr(path: *const c_char, name: *const c_char, val: *mut c_void, n: size_t) -> ssize_t {
    ret(sc4(sys::LGETXATTR, path as usize, name as usize, val as usize, n))
}
#[no_mangle]
pub unsafe extern "C" fn lsetxattr(path: *const c_char, name: *const c_char, val: *const c_void, n: size_t, flags: c_int) -> c_int {
    ret_i(sc5(sys::LSETXATTR, path as usize, name as usize, val as usize, n, flags as usize))
}
#[no_mangle]
pub unsafe extern "C" fn llistxattr(path: *const c_char, list: *mut c_char, n: size_t) -> ssize_t {
    ret(sc3(sys::LLISTXATTR, path as usize, list as usize, n))
}
#[no_mangle]
pub unsafe extern "C" fn mount(src: *const c_char, target: *const c_char, fstype: *const c_char, flags: c_ulong, data: *const c_void) -> c_int {
    ret_i(sc5(sys::MOUNT, src as usize, target as usize, fstype as usize, flags as usize, data as usize))
}
#[no_mangle]
pub unsafe extern "C" fn umount2(target: *const c_char, flags: c_int) -> c_int {
    ret_i(sc2(sys::UMOUNT2, target as usize, flags as usize))
}
#[no_mangle]
pub unsafe extern "C" fn umount(target: *const c_char) -> c_int {
    umount2(target, 0)
}

/// syscall(n, ...): up to six integer arguments, passed straight through.
#[no_mangle]
pub unsafe extern "C" fn syscall(n: c_long, a: usize, b: usize, c: usize, d: usize, e: usize, f: usize) -> c_long {
    ret(sc6(n as usize, a, b, c, d, e, f)) as c_long
}

#[repr(C)]
struct KStatfs {
    f_type: i64,
    f_bsize: i64,
    f_blocks: u64,
    f_bfree: u64,
    f_bavail: u64,
    f_files: u64,
    f_ffree: u64,
    f_fsid: [i32; 2],
    f_namelen: i64,
    f_frsize: i64,
    f_flags: i64,
    f_spare: [i64; 4],
}

/// musl's struct statvfs on x86-64.
#[repr(C)]
pub struct Statvfs {
    f_bsize: c_ulong,
    f_frsize: c_ulong,
    f_blocks: u64,
    f_bfree: u64,
    f_bavail: u64,
    f_files: u64,
    f_ffree: u64,
    f_favail: u64,
    f_fsid: c_ulong,
    f_flag: c_ulong,
    f_namemax: c_ulong,
    spare: [c_int; 6],
}

#[no_mangle]
pub unsafe extern "C" fn statvfs(path: *const c_char, out: *mut Statvfs) -> c_int {
    let mut k: KStatfs = core::mem::zeroed();
    if ret(sc2(sys::STATFS, path as usize, &mut k as *mut _ as usize)) < 0 {
        return -1;
    }
    *out = Statvfs {
        f_bsize: k.f_bsize as c_ulong,
        f_frsize: if k.f_frsize != 0 { k.f_frsize } else { k.f_bsize } as c_ulong,
        f_blocks: k.f_blocks,
        f_bfree: k.f_bfree,
        f_bavail: k.f_bavail,
        f_files: k.f_files,
        f_ffree: k.f_ffree,
        f_favail: k.f_ffree,
        f_fsid: k.f_fsid[0] as u32 as c_ulong,
        // ST_VALID (0x20) is the kernel's own marker, not part of the POSIX flags
        f_flag: (k.f_flags & !0x20) as c_ulong,
        f_namemax: k.f_namelen as c_ulong,
        spare: [0; 6],
    };
    0
}

const PATH_MAX: usize = 4096;

/// realpath: an absolute path with no ".", ".." or symbolic links. Resolved by hand (not
/// through /proc), so it works before /proc is mounted.
#[no_mangle]
pub unsafe extern "C" fn realpath(path: *const c_char, resolved: *mut c_char) -> *mut c_char {
    if path.is_null() {
        errno::set(errno::EINVAL);
        return ptr::null_mut();
    }
    let input = mem::cstr(path as *const u8);
    if input.is_empty() {
        errno::set(errno::ENOENT);
        return ptr::null_mut();
    }
    if input.len() >= PATH_MAX {
        errno::set(errno::ENAMETOOLONG);
        return ptr::null_mut();
    }
    // `rest` holds what is still to be resolved; `out` the resolved prefix
    let mut rest = [0u8; PATH_MAX];
    let mut rest_len = input.len();
    rest[..rest_len].copy_from_slice(input);
    let mut out = [0u8; PATH_MAX];
    let mut out_len;
    if input[0] == b'/' {
        out[0] = b'/';
        out_len = 1;
    } else {
        if getcwd(out.as_mut_ptr() as *mut c_char, PATH_MAX).is_null() {
            return ptr::null_mut();
        }
        out_len = mem::cstrlen(out.as_ptr());
    }
    let mut links = 0;
    let mut pos = 0;
    while pos < rest_len {
        while pos < rest_len && rest[pos] == b'/' {
            pos += 1;
        }
        let start = pos;
        while pos < rest_len && rest[pos] != b'/' {
            pos += 1;
        }
        let comp = &rest[start..pos];
        if comp.is_empty() || comp == b"." {
            continue;
        }
        if comp == b".." {
            while out_len > 1 && out[out_len - 1] != b'/' {
                out_len -= 1;
            }
            if out_len > 1 {
                out_len -= 1;
            }
            continue;
        }
        let save = out_len;
        if out_len > 1 {
            out[out_len] = b'/';
            out_len += 1;
        }
        if out_len + comp.len() + 1 > PATH_MAX {
            errno::set(errno::ENAMETOOLONG);
            return ptr::null_mut();
        }
        out[out_len..out_len + comp.len()].copy_from_slice(comp);
        out_len += comp.len();
        out[out_len] = 0;
        let mut link = [0u8; PATH_MAX];
        let n = sc3(sys::READLINK, out.as_ptr() as usize, link.as_mut_ptr() as usize, PATH_MAX - 1);
        if n == -(errno::EINVAL as isize) {
            // not a symbolic link: it must exist (and be a directory if more follows)
            continue;
        }
        if n < 0 {
            errno::set((-n) as i32);
            return ptr::null_mut();
        }
        links += 1;
        if links > 40 {
            errno::set(errno::ELOOP);
            return ptr::null_mut();
        }
        let n = n as usize;
        // new rest = link target + what was left
        let left = rest_len - pos;
        if n + left >= PATH_MAX {
            errno::set(errno::ENAMETOOLONG);
            return ptr::null_mut();
        }
        let mut next = [0u8; PATH_MAX];
        next[..n].copy_from_slice(&link[..n]);
        next[n..n + left].copy_from_slice(&rest[pos..rest_len]);
        rest = next;
        rest_len = n + left;
        pos = 0;
        if link[0] == b'/' {
            out_len = 1;
        } else {
            out_len = save;
        }
    }
    // the final path must exist
    out[out_len] = 0;
    let mut st = [0u8; 144];
    if ret(sc2(sys::STAT, out.as_ptr() as usize, st.as_mut_ptr() as usize)) < 0 {
        return ptr::null_mut();
    }
    let dst = if resolved.is_null() {
        let m = malloc::malloc(out_len + 1);
        if m.is_null() {
            return ptr::null_mut();
        }
        m
    } else {
        resolved as *mut u8
    };
    ptr::copy_nonoverlapping(out.as_ptr(), dst, out_len + 1);
    dst as *mut c_char
}

#[no_mangle]
pub unsafe extern "C" fn isatty(fd: c_int) -> c_int {
    let mut t = [0u8; 60];
    let r = sc3(sys::IOCTL, fd as isize as usize, 0x5401 /* TCGETS */, t.as_mut_ptr() as usize);
    if r < 0 {
        errno::set(errno::ENOTTY);
        return 0;
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn ftruncate(fd: c_int, len: off_t) -> c_int {
    ret_i(sys::sc2(77, fd as isize as usize, len as usize))
}
#[no_mangle]
pub unsafe extern "C" fn truncate(path: *const c_char, len: off_t) -> c_int {
    ret_i(sys::sc2(76, path as usize, len as usize))
}
#[no_mangle]
pub unsafe extern "C" fn fdatasync(fd: c_int) -> c_int {
    ret_i(sys::sc1(75, fd as isize as usize))
}
#[no_mangle]
pub unsafe extern "C" fn pread(fd: c_int, buf: *mut c_void, n: size_t, off: off_t) -> ssize_t {
    ret(sc4(17, fd as isize as usize, buf as usize, n, off as usize))
}
#[no_mangle]
pub unsafe extern "C" fn pwrite(fd: c_int, buf: *const c_void, n: size_t, off: off_t) -> ssize_t {
    ret(sc4(18, fd as isize as usize, buf as usize, n, off as usize))
}
#[no_mangle]
pub unsafe extern "C" fn fchdir(fd: c_int) -> c_int {
    ret_i(sys::sc1(81, fd as isize as usize))
}
