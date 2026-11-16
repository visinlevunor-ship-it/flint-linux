//! The dynamic loader: load a program's libraries, relocate everything, set up TLS, run
//! constructors, and start the program.
//!
//! Every loaded object is a `Dso`, which begins with glibc's public `struct link_map`
//! (programs walk that list, and dlopen handles point at it). Objects are never unloaded.

use crate::elf::*;
use crate::fl;
use crate::sys;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt::Write;
use core::ptr;

/// glibc's `struct link_map` (the public part) followed by the loader's own data.
#[repr(C)]
pub struct Dso {
    // --- struct link_map ---
    pub l_addr: usize,
    pub l_name: *const u8,
    pub l_ld: *const Dyn,
    pub l_next: *mut Dso,
    pub l_prev: *mut Dso,
    // --- flint ---
    pub path: Vec<u8>,   // NUL-terminated; "" for the program
    pub soname: Vec<u8>, // without NUL
    pub base: usize,
    pub phdr: usize,
    pub phnum: usize,
    pub map_start: usize,
    pub map_end: usize,
    pub symtab: usize,
    pub strtab: usize,
    pub strsz: usize,
    pub gnu_hash: usize,
    pub hash: usize,
    pub versym: usize,
    pub verdef: usize,
    pub verdefnum: usize,
    pub verneed: usize,
    pub verneednum: usize,
    pub rela: usize,
    pub relasz: usize,
    pub jmprel: usize,
    pub pltrelsz: usize,
    pub relr: usize,
    pub relrsz: usize,
    pub init: usize,
    pub fini: usize,
    pub init_array: usize,
    pub init_arraysz: usize,
    pub fini_array: usize,
    pub fini_arraysz: usize,
    pub preinit_array: usize,
    pub preinit_arraysz: usize,
    pub rpath: Option<usize>,
    pub runpath: Option<usize>,
    pub needed_names: Vec<usize>, // strtab offsets
    pub deps: Vec<*mut Dso>,
    pub tls_id: usize,
    pub tls_offset: usize,
    pub tls_image: (usize, usize, usize, usize), // image, filesz, memsz, align
    pub eh_frame_hdr: usize,
    pub relro: (usize, usize),
    pub is_libc: bool,
    pub is_main: bool,
    pub global: bool,
    pub relocated: bool,
    pub inited: bool,
    pub refcount: usize,
    /// For objects opened with RTLD_LOCAL: their own dependency closure, searched after the
    /// global scope.
    pub local_scope: Vec<*mut Dso>,
}

impl Dso {
    fn new() -> Box<Dso> {
        Box::new(Dso {
            l_addr: 0,
            l_name: b"\0".as_ptr(),
            l_ld: ptr::null(),
            l_next: ptr::null_mut(),
            l_prev: ptr::null_mut(),
            path: Vec::new(),
            soname: Vec::new(),
            base: 0,
            phdr: 0,
            phnum: 0,
            map_start: 0,
            map_end: 0,
            symtab: 0,
            strtab: 0,
            strsz: 0,
            gnu_hash: 0,
            hash: 0,
            versym: 0,
            verdef: 0,
            verdefnum: 0,
            verneed: 0,
            verneednum: 0,
            rela: 0,
            relasz: 0,
            jmprel: 0,
            pltrelsz: 0,
            relr: 0,
            relrsz: 0,
            init: 0,
            fini: 0,
            init_array: 0,
            init_arraysz: 0,
            fini_array: 0,
            fini_arraysz: 0,
            preinit_array: 0,
            preinit_arraysz: 0,
            rpath: None,
            runpath: None,
            needed_names: Vec::new(),
            deps: Vec::new(),
            tls_id: 0,
            tls_offset: 0,
            tls_image: (0, 0, 0, 1),
            eh_frame_hdr: 0,
            relro: (0, 0),
            is_libc: false,
            is_main: false,
            global: true,
            relocated: false,
            inited: false,
            refcount: 1,
            local_scope: Vec::new(),
        })
    }

    pub unsafe fn str_at(&self, off: usize) -> &[u8] {
        fl::mem::cstr((self.strtab + off) as *const u8)
    }

    pub unsafe fn sym(&self, i: usize) -> &Sym {
        &*((self.symtab + i * 24) as *const Sym)
    }

    /// Display name for messages.
    pub unsafe fn display(&self) -> &str {
        let p = if self.path.len() > 1 { &self.path[..self.path.len() - 1] } else { &self.soname[..] };
        core::str::from_utf8_unchecked(p)
    }

    /// Number of entries in the dynamic symbol table (needed by dladdr).
    pub unsafe fn nsyms(&self) -> usize {
        if self.hash != 0 {
            return *((self.hash + 4) as *const u32) as usize;
        }
        if self.gnu_hash != 0 {
            let h = self.gnu_hash;
            let nbuckets = *(h as *const u32) as usize;
            let symoffset = *((h + 4) as *const u32) as usize;
            let bloom_size = *((h + 8) as *const u32) as usize;
            let buckets = h + 16 + bloom_size * 8;
            let chains = buckets + nbuckets * 4;
            let mut max = 0;
            for b in 0..nbuckets {
                max = max.max(*((buckets + b * 4) as *const u32) as usize);
            }
            if max < symoffset {
                return symoffset;
            }
            loop {
                let c = *((chains + (max - symoffset) * 4) as *const u32);
                if c & 1 != 0 {
                    return max + 1;
                }
                max += 1;
            }
        }
        0
    }

    /// The version name a symbol is defined with, and whether it is hidden (not the default).
    unsafe fn def_version(&self, symi: usize) -> (Option<&[u8]>, bool) {
        if self.versym == 0 || self.verdef == 0 {
            return (None, false);
        }
        let vs = *((self.versym + symi * 2) as *const u16);
        let idx = vs & 0x7fff;
        let hidden = vs & VERSYM_HIDDEN != 0;
        if idx <= 1 {
            return (None, hidden);
        }
        let mut vd = self.verdef;
        for _ in 0..self.verdefnum {
            let d = &*(vd as *const Verdef);
            if d.vd_ndx == idx {
                let aux = &*((vd + d.vd_aux as usize) as *const Verdaux);
                return (Some(self.str_at(aux.vda_name as usize)), hidden);
            }
            if d.vd_next == 0 {
                break;
            }
            vd += d.vd_next as usize;
        }
        (None, hidden)
    }

    /// The version a reference (symbol `symi` used by this object) asks for.
    pub unsafe fn ref_version(&self, symi: usize) -> Option<&[u8]> {
        if self.versym == 0 {
            return None;
        }
        let idx = *((self.versym + symi * 2) as *const u16) & 0x7fff;
        if idx <= 1 {
            return None;
        }
        // a reference to this object's own versioned definition (libgcc_s's __cpu_model)
        if self.verdef != 0 {
            let mut vd = self.verdef;
            for _ in 0..self.verdefnum {
                let d = &*(vd as *const Verdef);
                if d.vd_ndx == idx {
                    let aux = &*((vd + d.vd_aux as usize) as *const Verdaux);
                    return Some(self.str_at(aux.vda_name as usize));
                }
                if d.vd_next == 0 {
                    break;
                }
                vd += d.vd_next as usize;
            }
        }
        if self.verneed == 0 {
            return None;
        }
        let mut vn = self.verneed;
        for _ in 0..self.verneednum {
            let n = &*(vn as *const Verneed);
            let mut va = vn + n.vn_aux as usize;
            for _ in 0..n.vn_cnt {
                let a = &*(va as *const Vernaux);
                if a.vna_other == idx {
                    return Some(self.str_at(a.vna_name as usize));
                }
                if a.vna_next == 0 {
                    break;
                }
                va += a.vna_next as usize;
            }
            if n.vn_next == 0 {
                break;
            }
            vn += n.vn_next as usize;
        }
        None
    }

    /// Find a defined symbol in this object.
    pub unsafe fn find(&self, name: &[u8], ghash: u32, version: Option<&[u8]>) -> Option<usize> {
        let ok = |i: usize| -> bool {
            let s = self.sym(i);
            if s.st_shndx == SHN_UNDEF && s.st_info & 0xf != STT_TLS {
                return false;
            }
            if s.st_shndx == SHN_UNDEF {
                return false;
            }
            let bind = s.st_info >> 4;
            if bind != STB_GLOBAL && bind != STB_WEAK && bind != STB_GNU_UNIQUE {
                return false;
            }
            if self.str_at(s.st_name as usize) != name {
                return false;
            }
            if self.is_libc {
                return true; // flint-glibc answers for every glibc version
            }
            let (defv, hidden) = self.def_version(i);
            match (version, defv) {
                (Some(want), Some(have)) => want == have,
                (Some(_), None) => true,
                (None, _) => !hidden,
            }
        };
        if self.gnu_hash != 0 {
            let h = self.gnu_hash;
            let nbuckets = *(h as *const u32) as usize;
            if nbuckets == 0 {
                return None;
            }
            let symoffset = *((h + 4) as *const u32) as usize;
            let bloom_size = *((h + 8) as *const u32) as usize;
            let bloom_shift = *((h + 12) as *const u32);
            let bloom = h + 16;
            let word = *((bloom + ((ghash as usize / 64) % bloom_size) * 8) as *const u64);
            let mask = (1u64 << (ghash % 64)) | (1u64 << ((ghash >> bloom_shift) % 64));
            if word & mask != mask {
                return None;
            }
            let buckets = bloom + bloom_size * 8;
            let chains = buckets + nbuckets * 4;
            let mut i = *((buckets + (ghash as usize % nbuckets) * 4) as *const u32) as usize;
            if i == 0 {
                return None;
            }
            loop {
                let c = *((chains + (i - symoffset) * 4) as *const u32);
                if (c | 1) == (ghash | 1) && ok(i) {
                    return Some(i);
                }
                if c & 1 != 0 {
                    return None;
                }
                i += 1;
            }
        }
        if self.hash != 0 {
            let nbucket = *(self.hash as *const u32) as usize;
            let buckets = self.hash + 8;
            let chains = buckets + nbucket * 4;
            let mut i = *((buckets + (sysv_hash(name) as usize % nbucket) * 4) as *const u32) as usize;
            while i != 0 {
                if ok(i) {
                    return Some(i);
                }
                i = *((chains + i * 4) as *const u32) as usize;
            }
        }
        None
    }
}

pub struct State {
    pub dsos: Vec<*mut Dso>,
    /// Lookup order for symbols: the program, then its libraries breadth-first, then
    /// libraries opened with RTLD_GLOBAL.
    pub global: Vec<*mut Dso>,
    pub libc: *mut Dso,
    pub main: *mut Dso,
    pub lib_path: Vec<Vec<u8>>, // LD_LIBRARY_PATH entries
    pub init_order: Vec<*mut Dso>,
    pub adds: u64,
    pub debug: bool,
    pub threads: Vec<usize>,
    pub missing: Vec<Box<[u8]>>,
}

pub static mut STATE: Option<State> = None;
/// The loader's lock. Recursive, as glibc's: constructors that dlopen run under it.
pub static LOCK: RecLock = RecLock::new();

pub struct RecLock {
    lock: fl::lock::Lock,
    owner: core::sync::atomic::AtomicUsize,
    depth: core::cell::UnsafeCell<usize>,
}
// SAFETY: depth is only touched by the owning thread.
unsafe impl Sync for RecLock {}

pub struct RecGuard<'a>(&'a RecLock);

impl RecLock {
    pub const fn new() -> RecLock {
        RecLock { lock: fl::lock::Lock::new(), owner: core::sync::atomic::AtomicUsize::new(0), depth: core::cell::UnsafeCell::new(0) }
    }
    pub fn guard(&self) -> RecGuard<'_> {
        use core::sync::atomic::Ordering;
        let me = fl::tls::current() as usize;
        if self.owner.load(Ordering::Relaxed) != me {
            self.lock.lock();
            self.owner.store(me, Ordering::Relaxed);
        }
        // SAFETY: we own the lock.
        unsafe { *self.depth.get() += 1 };
        RecGuard(self)
    }
}

impl Drop for RecGuard<'_> {
    fn drop(&mut self) {
        use core::sync::atomic::Ordering;
        // SAFETY: we own the lock.
        unsafe {
            *self.0.depth.get() -= 1;
            if *self.0.depth.get() == 0 {
                self.0.owner.store(0, Ordering::Relaxed);
                self.0.lock.unlock();
            }
        }
    }
}

pub fn st() -> &'static mut State {
    // SAFETY: set up in `start` before anything else runs; changed under LOCK afterwards.
    unsafe { STATE.as_mut().unwrap_unchecked() }
}

/// glibc's own libraries: all of them are this object.
const LIBC_NAMES: &[&[u8]] = &[
    b"libc.so.6",
    b"libm.so.6",
    b"libpthread.so.0",
    b"libdl.so.2",
    b"librt.so.1",
    b"libresolv.so.2",
    b"libutil.so.1",
    b"libanl.so.1",
    b"ld-linux-x86-64.so.2",
    b"libmvec.so.1",
    b"libnsl.so.1",
];

const DEFAULT_PATHS: &[&[u8]] = &[
    b"/usr/lib/flint-glibc",
    b"/lib/x86_64-linux-gnu",
    b"/usr/lib/x86_64-linux-gnu",
    b"/lib64",
    b"/usr/lib64",
    b"/lib",
    b"/usr/lib",
];

pub fn page() -> usize {
    4096
}

fn err(msg: core::fmt::Arguments) -> ! {
    let mut w = sys::Stderr;
    let _ = w.write_fmt(msg);
    let _ = w.write_str("\n");
    sys::exit_group(127)
}

unsafe fn parse_dynamic(d: &mut Dso) {
    let mut p = d.l_ld;
    if p.is_null() {
        return;
    }
    let b = d.base;
    loop {
        let e = *p;
        let v = e.d_val as usize;
        match e.d_tag {
            DT_NULL => break,
            DT_NEEDED => d.needed_names.push(v),
            DT_STRTAB => d.strtab = b + v,
            DT_STRSZ => d.strsz = v,
            DT_SYMTAB => d.symtab = b + v,
            DT_HASH => d.hash = b + v,
            DT_GNU_HASH => d.gnu_hash = b + v,
            DT_VERSYM => d.versym = b + v,
            DT_VERDEF => d.verdef = b + v,
            DT_VERDEFNUM => d.verdefnum = v,
            DT_VERNEED => d.verneed = b + v,
            DT_VERNEEDNUM => d.verneednum = v,
            DT_RELA => d.rela = b + v,
            DT_RELASZ => d.relasz = v,
            DT_JMPREL => d.jmprel = b + v,
            DT_PLTRELSZ => d.pltrelsz = v,
            DT_RELR => d.relr = b + v,
            DT_RELRSZ => d.relrsz = v,
            DT_INIT => d.init = b + v,
            DT_FINI => d.fini = b + v,
            DT_INIT_ARRAY => d.init_array = b + v,
            DT_INIT_ARRAYSZ => d.init_arraysz = v,
            DT_FINI_ARRAY => d.fini_array = b + v,
            DT_FINI_ARRAYSZ => d.fini_arraysz = v,
            DT_PREINIT_ARRAY => d.preinit_array = b + v,
            DT_PREINIT_ARRAYSZ => d.preinit_arraysz = v,
            DT_RPATH => d.rpath = Some(v),
            DT_RUNPATH => d.runpath = Some(v),
            DT_SONAME => {} // read below, once strtab is known
            DT_DEBUG => {}
            _ => {}
        }
        p = p.add(1);
    }
    let mut p = d.l_ld;
    while (*p).d_tag != DT_NULL {
        if (*p).d_tag == DT_SONAME {
            d.soname = d.str_at((*p).d_val as usize).to_vec();
        }
        p = p.add(1);
    }
}

/// Fill in what the program headers tell (already mapped at `base`).
unsafe fn scan_phdrs(d: &mut Dso) {
    let mut lo = usize::MAX;
    let mut hi = 0;
    for i in 0..d.phnum {
        let ph = &*((d.phdr + i * 56) as *const Phdr);
        match ph.p_type {
            PT_LOAD => {
                lo = lo.min(ph.p_vaddr as usize);
                hi = hi.max((ph.p_vaddr + ph.p_memsz) as usize);
            }
            PT_DYNAMIC => d.l_ld = (d.base + ph.p_vaddr as usize) as *const Dyn,
            PT_TLS => {
                d.tls_image = (
                    d.base + ph.p_vaddr as usize,
                    ph.p_filesz as usize,
                    ph.p_memsz as usize,
                    (ph.p_align as usize).max(1),
                )
            }
            PT_GNU_EH_FRAME => d.eh_frame_hdr = d.base + ph.p_vaddr as usize,
            PT_GNU_RELRO => d.relro = (d.base + ph.p_vaddr as usize, ph.p_memsz as usize),
            _ => {}
        }
    }
    if lo != usize::MAX {
        d.map_start = d.base + (lo & !(page() - 1));
        d.map_end = d.base + hi;
    }
    d.l_addr = d.base;
}

/// Map an ELF file. For a program (ET_EXEC) the addresses are fixed.
unsafe fn map_file(path: &[u8]) -> Result<Box<Dso>, &'static str> {
    let fd = sys::open_ro(path).map_err(|_| "cannot open shared object file")?;
    let mut hdr = [0u8; 64];
    if !sys::pread_exact(fd, &mut hdr, 0) {
        sys::close(fd);
        return Err("file too short");
    }
    let eh = ptr::read_unaligned(hdr.as_ptr() as *const Ehdr);
    if &eh.e_ident[..4] != b"\x7fELF" || eh.e_ident[4] != 2 || eh.e_machine != EM_X86_64 {
        sys::close(fd);
        return Err("invalid ELF header (not a 64-bit x86-64 object)");
    }
    let phnum = eh.e_phnum as usize;
    let mut ph = alloc::vec![0u8; phnum * 56];
    if !sys::pread_exact(fd, &mut ph, eh.e_phoff as usize) {
        sys::close(fd);
        return Err("cannot read program headers");
    }
    let phs: &[Phdr] = core::slice::from_raw_parts(ph.as_ptr() as *const Phdr, phnum);
    let pg = page();
    let (mut lo, mut hi) = (usize::MAX, 0usize);
    for p in phs.iter().filter(|p| p.p_type == PT_LOAD) {
        lo = lo.min(p.p_vaddr as usize & !(pg - 1));
        hi = hi.max((p.p_vaddr + p.p_memsz) as usize);
    }
    if lo == usize::MAX {
        sys::close(fd);
        return Err("no loadable segments");
    }
    let fixed = eh.e_type == ET_EXEC;
    let span = (hi - lo + pg - 1) & !(pg - 1);
    // reserve the whole range, then map the segments into it
    let reserve = sys::mmap(if fixed { lo } else { 0 }, span, 0, 0x22 | if fixed { 0x10 } else { 0 }, usize::MAX, 0)
        .map_err(|_| "cannot reserve memory")?;
    let base = reserve - lo;
    for p in phs.iter().filter(|p| p.p_type == PT_LOAD) {
        let prot = (if p.p_flags & PF_R != 0 { 1 } else { 0 })
            | (if p.p_flags & PF_W != 0 { 2 } else { 0 })
            | (if p.p_flags & PF_X != 0 { 4 } else { 0 });
        let va = p.p_vaddr as usize;
        let start = base + (va & !(pg - 1));
        let off = p.p_offset as usize & !(pg - 1);
        let file_end = base + va + p.p_filesz as usize;
        let mem_end = base + va + p.p_memsz as usize;
        if p.p_filesz > 0 {
            let len = file_end - start;
            sys::mmap(start, len, prot | 2, 0x02 | 0x10, fd, off).map_err(|_| "cannot map segment")?;
        }
        if p.p_memsz > p.p_filesz {
            // zero the rest of the last file page, then map zero pages for the rest
            let page_end = (file_end + pg - 1) & !(pg - 1);
            if p.p_filesz > 0 && page_end > file_end {
                ptr::write_bytes(file_end as *mut u8, 0, page_end - file_end);
            }
            let zstart = if p.p_filesz > 0 { page_end } else { start };
            if mem_end > zstart {
                let zlen = (mem_end - zstart + pg - 1) & !(pg - 1);
                sys::mmap(zstart, zlen, prot | 2, 0x22 | 0x10, usize::MAX, 0).map_err(|_| "cannot map bss")?;
            }
        }
        if prot & 2 == 0 {
            sys::sc3(sys::MPROTECT, start, ((mem_end + pg - 1) & !(pg - 1)) - start, prot);
        }
    }
    sys::close(fd);
    let mut d = Dso::new();
    d.base = base;
    // program headers: find them in a loaded segment, else keep our copy
    let mut phaddr = 0;
    for p in phs {
        if p.p_type == PT_PHDR {
            phaddr = base + p.p_vaddr as usize;
        }
    }
    if phaddr == 0 {
        for p in phs.iter().filter(|p| p.p_type == PT_LOAD) {
            if p.p_offset as usize <= eh.e_phoff as usize
                && (eh.e_phoff as usize + phnum * 56) <= (p.p_offset + p.p_filesz) as usize
            {
                phaddr = base + p.p_vaddr as usize + (eh.e_phoff - p.p_offset) as usize;
            }
        }
    }
    if phaddr == 0 {
        phaddr = Box::leak(ph.into_boxed_slice()).as_ptr() as usize;
    }
    d.phdr = phaddr;
    d.phnum = phnum;
    let mut pth = path.to_vec();
    if pth.last() != Some(&0) {
        pth.push(0);
    }
    d.path = pth;
    scan_phdrs(&mut d);
    parse_dynamic(&mut d);
    d.l_name = d.path.as_ptr();
    Ok(d)
}

unsafe fn file_exists(path: &[u8]) -> bool {
    sys::sc2(sys::ACCESS, path.as_ptr() as usize, 4 /* R_OK */) == 0
}

/// Replace $ORIGIN in a search path entry.
unsafe fn expand_origin(entry: &[u8], requester: &Dso) -> Vec<u8> {
    let origin: &[u8] = {
        let p = &requester.path;
        let p = if p.last() == Some(&0) { &p[..p.len() - 1] } else { &p[..] };
        match p.iter().rposition(|&c| c == b'/') {
            Some(i) => &p[..i.max(1)],
            None => b".",
        }
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < entry.len() {
        if entry[i..].starts_with(b"${ORIGIN}") {
            out.extend_from_slice(origin);
            i += 9;
        } else if entry[i..].starts_with(b"$ORIGIN") {
            out.extend_from_slice(origin);
            i += 7;
        } else {
            out.push(entry[i]);
            i += 1;
        }
    }
    out
}

unsafe fn search(name: &[u8], requester: &Dso) -> Option<Vec<u8>> {
    let try_dir = |dir: &[u8]| -> Option<Vec<u8>> {
        if dir.is_empty() {
            return None;
        }
        let mut p = dir.to_vec();
        p.push(b'/');
        p.extend_from_slice(name);
        p.push(0);
        if file_exists(&p) {
            Some(p)
        } else {
            None
        }
    };
    let s = st();
    // DT_RPATH (only if there is no DT_RUNPATH), of the requester, then of the program
    if requester.runpath.is_none() {
        for d in [requester as *const Dso, s.main as *const Dso] {
            let d = &*d;
            if let (Some(rp), None) = (d.rpath, d.runpath) {
                for e in d.str_at(rp).split(|&c| c == b':') {
                    if let Some(p) = try_dir(&expand_origin(e, d)) {
                        return Some(p);
                    }
                }
            }
        }
    }
    for e in &s.lib_path {
        if let Some(p) = try_dir(e) {
            return Some(p);
        }
    }
    if let Some(rp) = requester.runpath {
        for e in requester.str_at(rp).split(|&c| c == b':') {
            if let Some(p) = try_dir(&expand_origin(e, requester)) {
                return Some(p);
            }
        }
    }
    for d in DEFAULT_PATHS {
        if let Some(p) = try_dir(d) {
            return Some(p);
        }
    }
    None
}

fn base_name(p: &[u8]) -> &[u8] {
    let p = if p.last() == Some(&0) { &p[..p.len() - 1] } else { p };
    match p.iter().rposition(|&c| c == b'/') {
        Some(i) => &p[i + 1..],
        None => p,
    }
}

/// Find an already loaded object by the name a DT_NEEDED or dlopen uses.
unsafe fn find_loaded(name: &[u8]) -> Option<*mut Dso> {
    let s = st();
    if LIBC_NAMES.iter().any(|n| *n == name) {
        return Some(s.libc);
    }
    for &d in &s.dsos {
        let dd = &*d;
        if dd.soname == name {
            return Some(d);
        }
        let p = &dd.path;
        if !p.is_empty() && &p[..p.len() - 1] == name {
            return Some(d);
        }
    }
    None
}

/// Load one object (and register it), or find it loaded.
pub unsafe fn load(name: &[u8], requester: *mut Dso) -> Result<*mut Dso, alloc::string::String> {
    if let Some(d) = find_loaded(name) {
        return Ok(d);
    }
    let path = if name.contains(&b'/') {
        let mut p = name.to_vec();
        p.push(0);
        p
    } else {
        match search(name, &*requester) {
            Some(p) => p,
            None => {
                return Err(alloc::format!(
                    "{}: cannot open shared object file: No such file or directory",
                    core::str::from_utf8_unchecked(name)
                ))
            }
        }
    };
    // the same file under another name (a symlink): compare sonames after mapping
    let d = map_file(&path).map_err(|e| alloc::format!("{}: {}", core::str::from_utf8_unchecked(name), e))?;
    if !d.soname.is_empty() {
        if let Some(existing) = find_loaded(&d.soname.clone()) {
            // already have it: drop this mapping
            sys::sc2(sys::MUNMAP, d.map_start, d.map_end - d.map_start);
            return Ok(existing);
        }
    }
    let d = Box::into_raw(d);
    if (*d).soname.is_empty() {
        (*d).soname = base_name(name).to_vec();
    }
    register(d);
    Ok(d)
}

unsafe fn register(d: *mut Dso) {
    let s = st();
    if let Some(&last) = s.dsos.last() {
        (*last).l_next = d;
        (*d).l_prev = last;
    }
    s.dsos.push(d);
    s.adds += 1;
}

/// Load `root`'s dependencies breadth-first; returns the objects in load order (root first).
pub unsafe fn load_deps(root: *mut Dso) -> Result<Vec<*mut Dso>, alloc::string::String> {
    let mut order = alloc::vec![root];
    let mut i = 0;
    while i < order.len() {
        let d = order[i];
        i += 1;
        if (*d).is_libc || !(*d).deps.is_empty() {
            for &dep in &(*d).deps {
                if !order.contains(&dep) {
                    order.push(dep);
                }
            }
            continue;
        }
        let names: Vec<Vec<u8>> = (*d).needed_names.iter().map(|&n| (*d).str_at(n).to_vec()).collect();
        for n in names {
            let dep = load(&n, d)?;
            (*d).deps.push(dep);
            if !order.contains(&dep) {
                order.push(dep);
            }
        }
    }
    Ok(order)
}

pub struct Found {
    pub dso: *mut Dso,
    pub sym: *const Sym,
}

/// Look a symbol up in `scope`, in order. `skip` excludes one object (for COPY relocations).
pub unsafe fn lookup(name: &[u8], version: Option<&[u8]>, scope: &[*mut Dso], skip: *mut Dso) -> Option<Found> {
    let h = gnu_hash(name);
    for &d in scope {
        if d == skip {
            continue;
        }
        if let Some(i) = (*d).find(name, h, version) {
            return Some(Found { dso: d, sym: (*d).sym(i) });
        }
    }
    None
}

/// The address a found symbol stands for (calls an ifunc's resolver).
pub unsafe fn sym_addr(f: &Found) -> usize {
    let s = &*f.sym;
    if s.st_shndx == SHN_ABS {
        return s.st_value as usize;
    }
    let a = (*f.dso).base + s.st_value as usize;
    if s.st_info & 0xf == STT_GNU_IFUNC {
        let resolver: extern "C" fn() -> usize = core::mem::transmute(a);
        return resolver();
    }
    a
}

/// A function stub for a symbol nobody defines: calling it says which and stops.
unsafe fn missing_stub(name: &[u8]) -> usize {
    static mut PAGE: usize = 0;
    static mut USED: usize = 4096;
    let s = st();
    let mut n = name.to_vec();
    n.push(0);
    let n = n.into_boxed_slice();
    let nptr = n.as_ptr() as usize;
    s.missing.push(n);
    if USED + 32 > 4096 {
        PAGE = sys::mmap(0, 4096, 3, 0x22, usize::MAX, 0).unwrap_or(0);
        USED = 0;
        if PAGE == 0 {
            return 0;
        }
    } else {
        sys::sc3(sys::MPROTECT, PAGE, 4096, 3);
    }
    let code = (PAGE + USED) as *mut u8;
    // movabs rdi, name; movabs rax, handler; jmp rax
    let handler = missing_called as usize;
    let mut b = [0u8; 22];
    b[0] = 0x48;
    b[1] = 0xbf;
    b[2..10].copy_from_slice(&nptr.to_le_bytes());
    b[10] = 0x48;
    b[11] = 0xb8;
    b[12..20].copy_from_slice(&handler.to_le_bytes());
    b[20] = 0xff;
    b[21] = 0xe0;
    ptr::copy_nonoverlapping(b.as_ptr(), code, 22);
    USED += 32;
    sys::sc3(sys::MPROTECT, PAGE, 4096, 5);
    code as usize
}

extern "C" fn missing_called(name: *const u8) -> ! {
    let mut w = sys::Stderr;
    // SAFETY: name is the NUL-terminated copy made by missing_stub.
    let n = unsafe { core::str::from_utf8_unchecked(fl::mem::cstr(name)) };
    let _ = write!(w, "flint-glibc: {n} is not implemented yet (the program called it)\n");
    sys::exit_group(127)
}

/// The static TLS offset (below the thread pointer) of a module, by id.
pub unsafe fn tls_offset_of(id: usize) -> usize {
    (*ptr::addr_of!(fl::tls::MODULES))[id - 1].offset
}

/// Give an object a place in static TLS.
pub unsafe fn assign_tls(d: *mut Dso) -> Result<(), &'static str> {
    let (img, filesz, memsz, align) = (*d).tls_image;
    if memsz == 0 || (*d).tls_id != 0 {
        return Ok(());
    }
    if fl::tls::NMODULES >= fl::tls::MAX_MODULES {
        return Err("too many TLS modules");
    }
    let started = fl::tls::main_thread() as usize != 0 && fl::tls::RESERVE != 0;
    if started && align > 64 {
        return Err("TLS alignment too large for a library opened at run time");
    }
    let id = fl::tls::add_module(img, filesz, memsz, align);
    let off = tls_offset_of(id);
    if started && off > fl::tls::RESERVE {
        fl::tls::NMODULES -= 1;
        return Err("no room left in static TLS for this library");
    }
    (*d).tls_id = id;
    (*d).tls_offset = off;
    Ok(())
}

/// Copy a library's (relocated) TLS image into every running thread. Call it after
/// relocation: the image may hold relocated pointers (Mesa's TLS context does).
pub unsafe fn copy_tls_to_threads(d: *mut Dso) {
    let (img, filesz, memsz, _align) = (*d).tls_image;
    if memsz == 0 || (*d).tls_id == 0 {
        return;
    }
    let off = (*d).tls_offset;
    for &t in &st().threads {
        let block = t - off;
        if filesz > 0 {
            ptr::copy_nonoverlapping(img as *const u8, block as *mut u8, filesz);
        }
        ptr::write_bytes((block + filesz) as *mut u8, 0, memsz - filesz);
    }
}

/// Relocate one object against `scope`.
pub unsafe fn relocate(d: *mut Dso, scope: &[*mut Dso]) -> Result<(), alloc::string::String> {
    let dd = &mut *d;
    if dd.relocated {
        return Ok(());
    }
    let base = dd.base;
    // RELR first (only relative entries)
    let mut r = dd.relr;
    let mut where_ = 0usize;
    while r < dd.relr + dd.relrsz {
        let e = *(r as *const usize);
        if e & 1 == 0 {
            where_ = base + e;
            *(where_ as *mut usize) = (*(where_ as *mut usize)).wrapping_add(base);
            where_ += 8;
        } else {
            let mut bits = e >> 1;
            let mut i = 0;
            while bits != 0 {
                if bits & 1 != 0 {
                    let p = (where_ + i * 8) as *mut usize;
                    *p = (*p).wrapping_add(base);
                }
                bits >>= 1;
                i += 1;
            }
            where_ += 63 * 8;
        }
        r += 8;
    }
    // a read-only segment with relocations in it (text relocations): make it writable
    let textrel = {
        let mut p = dd.l_ld;
        let mut t = false;
        while !p.is_null() && (*p).d_tag != DT_NULL {
            if (*p).d_tag == DT_TEXTREL || ((*p).d_tag == DT_FLAGS && (*p).d_val & 4 != 0) {
                t = true;
            }
            p = p.add(1);
        }
        t
    };
    if textrel {
        sys::sc3(sys::MPROTECT, dd.map_start, dd.map_end - dd.map_start, 7);
    }
    let mut irel: Vec<(usize, usize)> = Vec::new();
    for (start, size) in [(dd.rela, dd.relasz), (dd.jmprel, dd.pltrelsz)] {
        let mut r = start;
        while r < start + size {
            let rel = *(r as *const Rela);
            r += 24;
            let ty = (rel.r_info & 0xffff_ffff) as u32;
            let symi = (rel.r_info >> 32) as usize;
            let at = base + rel.r_offset as usize;
            let addend = rel.r_addend as usize;
            if ty == R_X86_64_RELATIVE {
                *(at as *mut usize) = base.wrapping_add(addend);
                continue;
            }
            if ty == R_X86_64_NONE {
                continue;
            }
            if ty == R_X86_64_IRELATIVE {
                irel.push((at, base.wrapping_add(addend)));
                continue;
            }
            // resolve the symbol
            let mut found: Option<Found> = None;
            let mut sym_value = 0usize;
            let mut sym_size = 0usize;
            if symi != 0 {
                let s = dd.sym(symi);
                let name = dd.str_at(s.st_name as usize);
                let bind = s.st_info >> 4;
                if bind == STB_LOCAL {
                    found = Some(Found { dso: d, sym: s });
                } else {
                    let ver = dd.ref_version(symi);
                    let skip = if ty == R_X86_64_COPY { d } else { ptr::null_mut() };
                    found = lookup(name, ver, scope, skip);
                    if found.is_none() && !dd.local_scope.is_empty() {
                        found = lookup(name, ver, &dd.local_scope.clone(), skip);
                    }
                }
                match &found {
                    Some(f) => {
                        sym_size = (*f.sym).st_size as usize;
                        sym_value = if (*f.sym).st_info & 0xf == STT_TLS { 0 } else { sym_addr(f) };
                    }
                    None => {
                        if bind != STB_WEAK {
                            let name = dd.str_at(s.st_name as usize);
                            let st = st();
                            if st.debug {
                                let mut w = sys::Stderr;
                                let _ = write!(w, "flint-glibc: {}: undefined symbol {}\n", dd.display(), core::str::from_utf8_unchecked(name));
                            }
                            if s.st_info & 0xf == STT_FUNC || s.st_info & 0xf == STT_NOTYPE
                                || ty == R_X86_64_JUMP_SLOT
                            {
                                sym_value = missing_stub(name);
                            } else {
                                return Err(alloc::format!(
                                    "{}: undefined symbol: {}",
                                    dd.display(),
                                    core::str::from_utf8_unchecked(name)
                                ));
                            }
                        }
                    }
                }
            }
            match ty {
                R_X86_64_64 => *(at as *mut usize) = sym_value.wrapping_add(addend),
                R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT => *(at as *mut usize) = sym_value,
                R_X86_64_PC32 => *(at as *mut u32) = sym_value.wrapping_add(addend).wrapping_sub(at) as u32,
                R_X86_64_PC64 => *(at as *mut usize) = sym_value.wrapping_add(addend).wrapping_sub(at),
                R_X86_64_32 => *(at as *mut u32) = sym_value.wrapping_add(addend) as u32,
                R_X86_64_COPY => {
                    if sym_value != 0 {
                        ptr::copy_nonoverlapping(sym_value as *const u8, at as *mut u8, sym_size);
                    }
                }
                R_X86_64_DTPMOD64 => {
                    let m = match &found { Some(f) => (*f.dso).tls_id, None => dd.tls_id };
                    *(at as *mut usize) = m;
                }
                R_X86_64_DTPOFF64 => {
                    let v = found.as_ref().map(|f| (*f.sym).st_value as usize).unwrap_or(0);
                    *(at as *mut usize) = v.wrapping_add(addend);
                }
                R_X86_64_TPOFF64 => {
                    let (v, off) = match &found {
                        Some(f) => ((*f.sym).st_value as usize, (*f.dso).tls_offset),
                        None => (0, dd.tls_offset),
                    };
                    *(at as *mut usize) = v.wrapping_add(addend).wrapping_sub(off);
                }
                R_X86_64_TLSDESC => {
                    let (v, off) = match &found {
                        Some(f) => ((*f.sym).st_value as usize, (*f.dso).tls_offset),
                        None => (0, dd.tls_offset),
                    };
                    *(at as *mut usize) = crate::dl::tlsdesc_static();
                    *((at + 8) as *mut usize) = v.wrapping_add(addend).wrapping_sub(off);
                }
                _ => {
                    return Err(alloc::format!("{}: unsupported relocation type {}", dd.display(), ty));
                }
            }
        }
    }
    for (at, resolver) in irel {
        let f: extern "C" fn() -> usize = core::mem::transmute(resolver);
        *(at as *mut usize) = f();
    }
    if textrel {
        // back to what the segments say (approximately: read+execute for text)
        for i in 0..dd.phnum {
            let ph = &*((dd.phdr + i * 56) as *const Phdr);
            if ph.p_type == PT_LOAD && ph.p_flags & PF_W == 0 {
                let s = base + (ph.p_vaddr as usize & !(page() - 1));
                let e = base + (ph.p_vaddr + ph.p_memsz) as usize;
                let prot = (if ph.p_flags & PF_R != 0 { 1 } else { 0 }) | (if ph.p_flags & PF_X != 0 { 4 } else { 0 });
                sys::sc3(sys::MPROTECT, s, e - s, prot);
            }
        }
    }
    // RELRO: read-only now
    if dd.relro.1 > 0 && !dd.is_libc {
        let s = dd.relro.0 & !(page() - 1);
        let e = (dd.relro.0 + dd.relro.1) & !(page() - 1);
        if e > s {
            sys::sc3(sys::MPROTECT, s, e - s, 1);
        }
    }
    dd.relocated = true;
    Ok(())
}

/// Constructors of `root` and its dependencies, dependencies first.
pub unsafe fn init_order(root: *mut Dso, out: &mut Vec<*mut Dso>, seen: &mut Vec<*mut Dso>) {
    if seen.contains(&root) {
        return;
    }
    seen.push(root);
    for &d in &(*root).deps.clone() {
        init_order(d, out, seen);
    }
    out.push(root);
}

pub unsafe fn run_init(d: *mut Dso, argc: i32, argv: *mut *mut u8, envp: *mut *mut u8) {
    let dd = &mut *d;
    if dd.inited || dd.is_libc || dd.is_main {
        return;
    }
    dd.inited = true;
    st().init_order.push(d);
    if dd.init != 0 {
        let f: extern "C" fn(i32, *mut *mut u8, *mut *mut u8) = core::mem::transmute(dd.init);
        f(argc, argv, envp);
    }
    run_array(dd.init_array, dd.init_arraysz, argc, argv, envp);
}

pub unsafe fn run_array(start: usize, size: usize, argc: i32, argv: *mut *mut u8, envp: *mut *mut u8) {
    let mut f = start;
    while f < start + size {
        let p = *(f as *const usize);
        if p != 0 && p != usize::MAX {
            let func: extern "C" fn(i32, *mut *mut u8, *mut *mut u8) = core::mem::transmute(p);
            func(argc, argv, envp);
        }
        f += 8;
    }
}

/// rtld_fini: destructors of every library, in the reverse order of their constructors.
pub unsafe extern "C" fn dl_fini() {
    let order = core::mem::take(&mut st().init_order);
    for &d in order.iter().rev() {
        let dd = &*d;
        let mut f = dd.fini_array + dd.fini_arraysz;
        while f > dd.fini_array {
            f -= 8;
            let p = *(f as *const usize);
            if p != 0 && p != usize::MAX {
                let func: extern "C" fn() = core::mem::transmute(p);
                func();
            }
        }
        if dd.fini != 0 {
            let func: extern "C" fn() = core::mem::transmute(dd.fini);
            func();
        }
    }
}

extern "C" {
    static __ehdr_start: u8;
    static _DYNAMIC: u8;
}

#[repr(C)]
pub struct StartResult {
    pub entry: usize,
    pub sp: usize,
}

const AT_PHDR: usize = 3;
const AT_PHNUM: usize = 5;
const AT_PAGESZ: usize = 6;
const AT_BASE: usize = 7;
const AT_ENTRY: usize = 9;
const AT_RANDOM: usize = 25;
const AT_EXECFN: usize = 31;

/// From the kernel's stack to the program's entry point.
pub unsafe extern "C" fn start(sp: *mut usize) -> StartResult {
    crate::dl::__libc_stack_end = sp as *mut core::ffi::c_void;
    let mut sp = sp;
    let mut argc = *sp as usize;
    let mut argv = sp.add(1) as *mut *mut u8;
    let envp = argv.add(argc + 1);
    let mut p = envp;
    while !(*p).is_null() {
        p = p.add(1);
    }
    let auxv = p.add(1) as *mut usize;
    let mut a = auxv;
    let (mut at_phdr, mut at_phnum, mut at_entry, mut random, mut at_execfn) = (0, 0, 0, 0, 0usize);
    while *a != 0 {
        match *a {
            AT_PHDR => at_phdr = *a.add(1),
            AT_EXECFN => at_execfn = *a.add(1),
            AT_PHNUM => at_phnum = *a.add(1),
            AT_ENTRY => at_entry = *a.add(1),
            AT_RANDOM => random = *a.add(1),
            AT_PAGESZ => fl::start::PAGE_SIZE.store(*a.add(1), core::sync::atomic::Ordering::Relaxed),
            _ => {}
        }
        a = a.add(2);
    }
    fl::start::AUXV.store(auxv as usize, core::sync::atomic::Ordering::Relaxed);
    // a first thread block (no TLS yet), so errno and locks work while loading
    fl::tls::init_main_block(random);

    let my_base = ptr::addr_of!(__ehdr_start) as usize;
    let mut me = Dso::new();
    me.base = my_base;
    let eh = &*(my_base as *const Ehdr);
    me.phdr = my_base + eh.e_phoff as usize;
    me.phnum = eh.e_phnum as usize;
    me.is_libc = true;
    me.relocated = true;
    me.soname = b"libc.so.6".to_vec();
    me.path = b"/lib64/ld-linux-x86-64.so.2\0".to_vec();
    scan_phdrs(&mut me);
    me.l_ld = ptr::addr_of!(_DYNAMIC) as *const Dyn;
    parse_dynamic(&mut me);
    me.soname = b"libc.so.6".to_vec();
    me.l_name = me.path.as_ptr();
    let me = Box::into_raw(me);

    let mut env_get = |name: &[u8]| -> Option<&'static [u8]> {
        let mut e = envp;
        while !(*e).is_null() {
            let s = fl::mem::cstr(*e);
            if s.len() > name.len() && s[name.len()] == b'=' && &s[..name.len()] == name {
                return Some(&s[name.len() + 1..]);
            }
            e = e.add(1);
        }
        None
    };
    let lib_path: Vec<Vec<u8>> = env_get(b"LD_LIBRARY_PATH")
        .map(|v| v.split(|&c| c == b':' || c == b';').filter(|e| !e.is_empty()).map(|e| e.to_vec()).collect())
        .unwrap_or_default();
    let debug = env_get(b"FLINT_LD_DEBUG").is_some();
    STATE = Some(State {
        dsos: Vec::new(),
        global: Vec::new(),
        libc: me,
        main: ptr::null_mut(),
        lib_path,
        init_order: Vec::new(),
        adds: 0,
        debug,
        threads: Vec::new(),
        missing: Vec::new(),
    });

    // the program: mapped by the kernel, or named on our command line
    let main: *mut Dso;
    let self_entry = my_base + eh.e_entry as usize;
    if at_entry == self_entry {
        if argc < 2 {
            err(format_args!(
                "usage: ld-linux-x86-64.so.2 PROGRAM [ARGUMENTS...]\n\
                 Runs a program built for glibc with flint-glibc, Flint's glibc-compatible C library."
            ));
        }
        let prog = fl::mem::cstr(*argv.add(1));
        let d = match map_file(prog) {
            Ok(d) => d,
            Err(e) => err(format_args!("{}: {}", core::str::from_utf8_unchecked(prog), e)),
        };
        let d = Box::into_raw(d);
        let entry = {
            let mut hdr = [0u8; 64];
            let fd = sys::open_ro(&(*d).path).unwrap_or(usize::MAX);
            sys::pread_exact(fd, &mut hdr, 0);
            sys::close(fd);
            let eh = ptr::read_unaligned(hdr.as_ptr() as *const Ehdr);
            (*d).base + eh.e_entry as usize
        };
        // drop our own name from the arguments, tell the program where it is
        argc -= 1;
        sp = sp.add(1);
        *sp = argc;
        argv = sp.add(1) as *mut *mut u8;
        let mut a = auxv;
        while *a != 0 {
            match *a {
                AT_PHDR => *a.add(1) = (*d).phdr,
                AT_PHNUM => *a.add(1) = (*d).phnum,
                AT_ENTRY => *a.add(1) = entry,
                AT_BASE => *a.add(1) = my_base,
                AT_EXECFN => *a.add(1) = *argv as usize,
                _ => {}
            }
            a = a.add(2);
        }
        at_entry = entry;
        main = d;
    } else {
        let mut d = Dso::new();
        d.phdr = at_phdr;
        // the program's path, for $ORIGIN in its RPATH: the kernel's AT_EXECFN when it is
        // absolute, else /proc/self/exe
        let mut path: Vec<u8> = Vec::new();
        if at_execfn != 0 && *(at_execfn as *const u8) == b'/' {
            path.extend_from_slice(fl::mem::cstr(at_execfn as *const u8));
        } else {
            let mut buf = [0u8; 4096];
            let n = fl::sys::sc4(89 /* readlink */, b"/proc/self/exe\0".as_ptr() as usize, buf.as_mut_ptr() as usize, 4095, 0);
            if n > 0 {
                path.extend_from_slice(&buf[..n as usize]);
            } else if at_execfn != 0 {
                path.extend_from_slice(fl::mem::cstr(at_execfn as *const u8));
            }
        }
        path.push(0);
        d.phnum = at_phnum;
        for i in 0..at_phnum {
            let ph = &*((at_phdr + i * 56) as *const Phdr);
            if ph.p_type == PT_PHDR {
                d.base = at_phdr - ph.p_vaddr as usize;
            }
        }
        d.path = path;
        scan_phdrs(&mut d);
        parse_dynamic(&mut d);
        d.l_name = d.path.as_ptr();
        main = Box::into_raw(d);
    }
    (*main).is_main = true;
    if (*main).soname.is_empty() {
        let name = fl::mem::cstr(*argv);
        (*main).soname = base_name(name).to_vec();
    }
    st().main = main;
    register(main);
    register(me);

    let order = match load_deps(main) {
        Ok(o) => o,
        Err(e) => err(format_args!("{}: error while loading shared libraries: {}", core::str::from_utf8_unchecked(fl::mem::cstr(*argv)), e)),
    };
    // our own object goes into the global scope where the program's libraries put it
    let s = st();
    s.global = order.clone();
    if !s.global.contains(&me) {
        s.global.push(me);
    }

    // static TLS for everything loaded now, plus room for libraries opened later
    for &d in &order {
        if let Err(e) = assign_tls(d) {
            err(format_args!("{}: {}", (*d).display(), e));
        }
    }
    fl::tls::STATIC_ALIGN = fl::tls::STATIC_ALIGN.max(64);
    fl::tls::RESERVE = fl::tls::STATIC_SIZE + 64 * 1024;
    {
        let old = fl::tls::current();
        let (size, _) = fl::tls::block_size();
        let mem = match sys::mmap(0, size, 3, 0x22, usize::MAX, 0) {
            Ok(m) => m,
            Err(_) => err(format_args!("flint-glibc: out of memory")),
        };
        let t = fl::tls::setup_block(mem);
        (*t).stack_guard = (*old).stack_guard;
        (*t).pointer_guard = (*old).pointer_guard;
        (*t).tid.store((*old).tid.load(core::sync::atomic::Ordering::Relaxed), core::sync::atomic::Ordering::Relaxed);
        fl::tls::set_fs(t);
        fl::tls::set_main_thread(t);
        st().threads.push(t as usize);
    }
    crate::dl::install_thread_hooks();

    // relocate: libraries first (deepest last loaded first), the program last
    let scope = st().global.clone();
    for &d in order.iter().rev() {
        if let Err(e) = relocate(d, &scope) {
            err(format_args!("{}: error while loading shared libraries: {}", core::str::from_utf8_unchecked(fl::mem::cstr(*argv)), e));
        }
    }
    // our own references to interposable symbols (malloc, environ, ...) now follow the
    // program's choices
    crate::libc::bind_interposed(&scope);
    crate::libc::init(argc as i32, argv, envp);

    // constructors, dependencies first
    let mut out = Vec::new();
    let mut seen = Vec::new();
    init_order(main, &mut out, &mut seen);
    for d in out {
        run_init(d, argc as i32, argv, envp);
    }
    StartResult { entry: at_entry, sp: sp as usize }
}
