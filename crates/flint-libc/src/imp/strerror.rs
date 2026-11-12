//! Error messages: strerror, __xpg_strerror_r (what std's io::Error prints).

use crate::imp::types::*;

fn message(e: c_int) -> &'static [u8] {
    match e {
        0 => b"No error information",
        1 => b"Operation not permitted",
        2 => b"No such file or directory",
        3 => b"No such process",
        4 => b"Interrupted system call",
        5 => b"I/O error",
        6 => b"No such device or address",
        7 => b"Argument list too long",
        8 => b"Exec format error",
        9 => b"Bad file descriptor",
        10 => b"No child process",
        11 => b"Resource temporarily unavailable",
        12 => b"Out of memory",
        13 => b"Permission denied",
        14 => b"Bad address",
        15 => b"Block device required",
        16 => b"Resource busy",
        17 => b"File exists",
        18 => b"Cross-device link",
        19 => b"No such device",
        20 => b"Not a directory",
        21 => b"Is a directory",
        22 => b"Invalid argument",
        23 => b"Too many open files in system",
        24 => b"No file descriptors available",
        25 => b"Not a tty",
        26 => b"Text file busy",
        27 => b"File too large",
        28 => b"No space left on device",
        29 => b"Invalid seek",
        30 => b"Read-only file system",
        31 => b"Too many links",
        32 => b"Broken pipe",
        33 => b"Domain error",
        34 => b"Result not representable",
        35 => b"Resource deadlock would occur",
        36 => b"Filename too long",
        37 => b"No locks available",
        38 => b"Function not implemented",
        39 => b"Directory not empty",
        40 => b"Symbolic link loop",
        42 => b"No message of desired type",
        43 => b"Identifier removed",
        61 => b"No data available",
        62 => b"Device timeout",
        71 => b"Protocol error",
        74 => b"Bad message",
        75 => b"Value too large for data type",
        84 => b"Illegal byte sequence",
        88 => b"Not a socket",
        89 => b"Destination address required",
        90 => b"Message too large",
        91 => b"Protocol wrong type for socket",
        92 => b"Protocol not available",
        93 => b"Protocol not supported",
        95 => b"Not supported",
        97 => b"Address family not supported by protocol",
        98 => b"Address in use",
        99 => b"Address not available",
        100 => b"Network is down",
        101 => b"Network unreachable",
        102 => b"Connection reset by network",
        103 => b"Connection aborted",
        104 => b"Connection reset by peer",
        105 => b"No buffer space available",
        106 => b"Socket is connected",
        107 => b"Socket not connected",
        108 => b"Cannot send after socket shutdown",
        110 => b"Operation timed out",
        111 => b"Connection refused",
        112 => b"Host is down",
        113 => b"Host is unreachable",
        114 => b"Operation already in progress",
        115 => b"Operation in progress",
        116 => b"Stale file handle",
        122 => b"Quota exceeded",
        123 => b"No medium found",
        125 => b"Operation canceled",
        130 => b"Previous owner died",
        131 => b"State not recoverable",
        _ => b"No error information",
    }
}

/// The XSI strerror_r: fills `buf`, returns 0 or ERANGE when it did not fit.
#[no_mangle]
pub unsafe extern "C" fn __xpg_strerror_r(e: c_int, buf: *mut c_char, len: size_t) -> c_int {
    let m = message(e);
    if len == 0 {
        return 34;
    }
    let n = m.len().min(len - 1);
    core::ptr::copy_nonoverlapping(m.as_ptr(), buf as *mut u8, n);
    *buf.add(n) = 0;
    if n < m.len() {
        34
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn strerror(e: c_int) -> *mut c_char {
    // every message is static and NUL-terminated below
    static mut BUF: [u8; 64] = [0; 64];
    let buf = &mut *core::ptr::addr_of_mut!(BUF);
    __xpg_strerror_r(e, buf.as_mut_ptr() as *mut c_char, buf.len());
    buf.as_mut_ptr() as *mut c_char
}
