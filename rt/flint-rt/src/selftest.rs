//! Runtime self-test, run as a real program (`flint-version --rt-selftest`).
//! Exercises the allocator, mem*, args, and syscalls. Returns the number of failures.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::{println, sys};

#[allow(dead_code)] // only its address and alignment are checked
#[repr(align(64))]
struct Aligned64([u8; 64]);
#[allow(dead_code)]
#[repr(align(4096))]
struct Aligned4096([u8; 16]);

fn check(name: &str, ok: bool, failures: &mut u32) {
    if ok {
        println!("ok   {name}");
    } else {
        println!("FAIL {name}");
        *failures += 1;
    }
}

pub fn run() -> u32 {
    let mut f = 0;

    // mem*: memcpy/memset/memcmp directly, memmove in both overlap directions.
    let mut a = [0u8; 64];
    for (i, b) in a.iter_mut().enumerate() {
        *b = i as u8;
    }
    let mut copy = [0u8; 64];
    copy.copy_from_slice(&a);
    check("memcpy", copy == a, &mut f);

    let mut fwd = a;
    fwd.copy_within(8..40, 0); // dst below src
    check("memmove dst<src", fwd[..32] == a[8..40], &mut f);
    let mut back = a;
    back.copy_within(0..32, 8); // dst above src, overlapping
    check("memmove dst>src overlap", back[8..40] == a[0..32], &mut f);

    let mut z = [0xffu8; 33];
    z.fill(0);
    check("memset", z.iter().all(|&b| b == 0), &mut f);
    check("memcmp", a[..10] == copy[..10] && a[..10] != fwd[..10], &mut f);

    // Heap: growth (realloc), many small blocks, big block (own mmap), reuse, alignment.
    let mut v: Vec<u64> = Vec::new();
    for i in 0..200_000u64 {
        v.push(i);
    }
    check("vec growth to 1.6 MB", v.iter().sum::<u64>() == 199_999 * 200_000 / 2, &mut f);
    drop(v);

    let mut m = BTreeMap::new();
    for i in 0..2000u32 {
        m.insert(i, format!("value-{i}"));
    }
    check("btreemap of strings", m.len() == 2000 && m[&1234] == "value-1234", &mut f);
    drop(m);

    let big = alloc::vec![7u8; 1 << 20];
    check("1 MiB block", big.iter().all(|&b| b == 7), &mut f);
    drop(big);

    let mut reused_ok = true;
    for _ in 0..1000 {
        let b = Box::new([1u8; 100]);
        reused_ok &= b[99] == 1;
    }
    check("alloc/free cycles", reused_ok, &mut f);

    let a64 = Box::new(Aligned64([0; 64]));
    let a4k = Box::new(Aligned4096([0; 16]));
    check(
        "alignment 64 and 4096",
        (a64.as_ref() as *const _ as usize) % 64 == 0 && (a4k.as_ref() as *const _ as usize) % 4096 == 0,
        &mut f,
    );

    let s: String = (0..50).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
    check("string collect", s.len() == 50 && s.starts_with("abc"), &mut f);

    // Process and syscalls.
    check("args has argv[0]", crate::args().len() >= 1, &mut f);
    check("getpid", sys::getpid() > 0, &mut f);
    let fd = sys::open(b"/proc/self/status\0", sys::O_RDONLY | sys::O_CLOEXEC);
    let mut ok = false;
    if let Ok(fd) = fd {
        let mut buf = [0u8; 64];
        if let Ok(n) = sys::read(fd, &mut buf) {
            ok = n >= 5 && &buf[..5] == b"Name:";
        }
        let _ = sys::close(fd);
    }
    check("open/read/close /proc/self/status", ok, &mut f);
    check("open missing file gives ENOENT", sys::open(b"/nonexistent-flint\0", sys::O_RDONLY) == Err(sys::Errno::ENOENT), &mut f);

    f
}
