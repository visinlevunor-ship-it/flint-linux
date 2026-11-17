//! The root filesystem programs see: the base image (read-only, cpio) with a layer in RAM on
//! top, like an overlay mount. New and changed files live in RAM; a file of the base image is
//! copied up the first time it is written; deleting leaves a "whiteout" hiding the base file.
//! Nothing in RAM survives a restart yet.

use crate::cpio::{self, S_IFDIR, S_IFLNK, S_IFMT};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

pub const S_IFREG: u32 = 0o100000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFIFO: u32 = 0o010000;

pub const ENOENT: i64 = 2;
pub const EEXIST: i64 = 17;
pub const ENOTDIR: i64 = 20;
pub const EISDIR: i64 = 21;
pub const EINVAL: i64 = 22;
pub const ELOOP: i64 = 40;
pub const ENOTEMPTY: i64 = 39;
pub const EBUSY: i64 = 16;

pub struct MemNode {
    pub mode: u32,
    pub ino: u64,
    pub mtime: u64,
    pub rdev: u64,
    pub data: Vec<u8>,
}

/// A file of the data partition (ext4): its inode number and inode.
pub struct ExtNode {
    pub n: u64,
    pub inode: crate::ext4::Inode,
}

#[derive(Clone)]
pub enum Node {
    Base(&'static cpio::Node),
    Mem(Rc<RefCell<MemNode>>),
    Ext(Rc<ExtNode>),
}

impl Node {
    pub fn mode(&self) -> u32 {
        match self {
            Node::Base(n) => n.mode,
            Node::Mem(m) => m.borrow().mode,
            Node::Ext(e) => e.inode.mode,
        }
    }
    pub fn is_dir(&self) -> bool {
        self.mode() & S_IFMT == S_IFDIR
    }
    pub fn is_link(&self) -> bool {
        self.mode() & S_IFMT == S_IFLNK
    }
    pub fn ino(&self) -> u64 {
        match self {
            Node::Base(n) => n.ino,
            Node::Mem(m) => m.borrow().ino,
            Node::Ext(e) => e.n | 1 << 40,
        }
    }
    pub fn mtime(&self) -> u64 {
        match self {
            Node::Base(n) => n.mtime,
            Node::Mem(m) => m.borrow().mtime,
            Node::Ext(e) => e.inode.mtime,
        }
    }
    pub fn rdev(&self) -> u64 {
        match self {
            Node::Base(_) => 0,
            Node::Mem(m) => m.borrow().rdev,
            Node::Ext(e) => e.inode.rdev,
        }
    }
    pub fn size(&self) -> usize {
        match self {
            Node::Base(n) => n.data.len(),
            Node::Mem(m) => m.borrow().data.len(),
            Node::Ext(e) => e.inode.size as usize,
        }
    }
    pub fn link_target(&self) -> String {
        match self {
            Node::Base(n) => String::from_utf8_lossy(n.data).into_owned(),
            Node::Mem(m) => String::from_utf8_lossy(&m.borrow().data).into_owned(),
            Node::Ext(e) => data_fs().map(|f| f.link_target(&e.inode)).unwrap_or_default(),
        }
    }
    /// Bytes from `pos` into `out`; how many.
    pub fn read_at(&self, pos: usize, out: &mut [u8]) -> usize {
        let copy = |data: &[u8], out: &mut [u8]| {
            let start = pos.min(data.len());
            let k = (data.len() - start).min(out.len());
            out[..k].copy_from_slice(&data[start..start + k]);
            k
        };
        match self {
            Node::Base(n) => copy(n.data, out),
            Node::Mem(m) => copy(&m.borrow().data, out),
            Node::Ext(e) => data_fs().map(|f| f.read_at(&e.inode, pos as u64, out)).unwrap_or(0),
        }
    }
    /// The whole contents (for loading programs).
    pub fn bytes(&self) -> Vec<u8> {
        match self {
            Node::Base(n) => n.data.to_vec(),
            Node::Mem(m) => m.borrow().data.clone(),
            Node::Ext(e) => data_fs().map(|f| f.read_all(&e.inode)).unwrap_or_default(),
        }
    }
}

// ---------------------------------------------------------------------------------------
// The data partition: Flint's overlay upper layer (root/upper on the ext4 "Flint data"
// partition), read-only here, between the RAM layer and the base image.

static mut DATA: Option<(crate::ext4::Ext4, u64)> = None;
static mut DATA_CACHE: Option<BTreeMap<String, Option<u64>>> = None;

pub fn set_data_layer(fs: crate::ext4::Ext4, upper: u64) {
    // SAFETY: start-up.
    unsafe { DATA = Some((fs, upper)) };
}

fn data_fs() -> Option<&'static crate::ext4::Ext4> {
    // SAFETY: set once at start-up.
    unsafe { (*core::ptr::addr_of!(DATA)).as_ref().map(|(f, _)| f) }
}

/// The data layer's inode for `path`, if any (cached: the layer does not change under us).
fn data_inode(path: &str) -> Option<u64> {
    // SAFETY: set once at start-up; one CPU.
    let (fs, upper) = unsafe { (*core::ptr::addr_of!(DATA)).as_ref()? };
    let cache = unsafe { (*core::ptr::addr_of_mut!(DATA_CACHE)).get_or_insert_with(BTreeMap::new) };
    if let Some(v) = cache.get(path) {
        return *v;
    }
    let n = if path.is_empty() { Some(*upper) } else { fs.walk(*upper, path) };
    if cache.len() > 50_000 {
        cache.clear();
    }
    cache.insert(path.to_string(), n);
    n
}

/// In the data layer: Some(Some(node)) a file, Some(None) a whiteout (deleted there),
/// None nothing.
fn data_lookup(path: &str) -> Option<Option<Node>> {
    let fs = data_fs()?;
    let n = data_inode(path)?;
    let inode = fs.inode(n)?;
    // overlayfs marks deleted files with a 0/0 character device
    if inode.mode & S_IFMT == S_IFCHR && inode.rdev == 0 {
        return Some(None);
    }
    Some(Some(Node::Ext(Rc::new(ExtNode { n, inode }))))
}

fn data_list(dir: &str) -> Vec<(String, Option<Node>)> {
    let Some(fs) = data_fs() else { return Vec::new() };
    let Some(n) = data_inode(dir) else { return Vec::new() };
    let Some(ino) = fs.inode(n) else { return Vec::new() };
    if ino.mode & S_IFMT != S_IFDIR {
        return Vec::new();
    }
    fs.list(&ino)
        .into_iter()
        .filter_map(|(name, cn)| {
            let child = fs.inode(cn)?;
            if child.mode & S_IFMT == S_IFCHR && child.rdev == 0 {
                return Some((name, None));
            }
            Some((name, Some(Node::Ext(Rc::new(ExtNode { n: cn, inode: child })))))
        })
        .collect()
}

static mut DIRTY: bool = false;

/// The RAM layer changed: it should be saved.
pub fn mark_dirty() {
    // SAFETY: one CPU.
    unsafe { DIRTY = true };
}

pub fn take_dirty() -> bool {
    // SAFETY: one CPU.
    unsafe { core::mem::replace(&mut *core::ptr::addr_of_mut!(DIRTY), false) }
}

/// path -> file in RAM, or None: deleted (hides the base image's file)
static mut LAYER: Option<BTreeMap<String, Option<Rc<RefCell<MemNode>>>>> = None;
static mut NEXT_INO: u64 = 1 << 32;

fn layer() -> &'static mut BTreeMap<String, Option<Rc<RefCell<MemNode>>>> {
    // SAFETY: one CPU; set up at start-up.
    unsafe { (*core::ptr::addr_of_mut!(LAYER)).get_or_insert_with(BTreeMap::new) }
}

fn new_ino() -> u64 {
    // SAFETY: one CPU.
    unsafe {
        NEXT_INO += 1;
        NEXT_INO
    }
}

pub fn now() -> u64 {
    crate::syscall::wall_seconds()
}

fn mem(mode: u32, data: Vec<u8>, rdev: u64) -> Rc<RefCell<MemNode>> {
    Rc::new(RefCell::new(MemNode { mode, ino: new_ino(), mtime: now(), rdev, data }))
}

/// The device files programs expect.
pub fn init() {
    let l = layer();
    for (name, rdev) in [("null", 0x0103), ("zero", 0x0105), ("full", 0x0107), ("random", 0x0108), ("urandom", 0x0109), ("tty", 0x0500), ("console", 0x0501), ("tty0", 0x0400), ("tty1", 0x0401)] {
        l.insert(alloc::format!("dev/{name}"), Some(mem(S_IFCHR | 0o666, Vec::new(), rdev)));
    }
    for d in ["dev", "dev/pts", "dev/shm", "tmp", "run", "proc", "sys", "root"] {
        if lookup(d).is_none() {
            l.insert(d.to_string(), Some(mem(S_IFDIR | 0o755, Vec::new(), 0)));
        }
    }
    l.insert("dev/stdin".into(), Some(mem(S_IFLNK | 0o777, b"/proc/self/fd/0".to_vec(), 0)));
    l.insert("dev/stdout".into(), Some(mem(S_IFLNK | 0o777, b"/proc/self/fd/1".to_vec(), 0)));
    l.insert("dev/stderr".into(), Some(mem(S_IFLNK | 0o777, b"/proc/self/fd/2".to_vec(), 0)));
}

/// The file at canonical `path` ("" is the root).
pub fn lookup(path: &str) -> Option<Node> {
    match layer().get(path) {
        Some(Some(m)) => Some(Node::Mem(m.clone())),
        Some(None) => None,
        None => match data_lookup(path) {
            Some(n) => n,
            None => cpio::files().get(path).map(Node::Base),
        },
    }
}

/// Does a lower layer (the data partition or the base image) have `path`? Deleting it then
/// needs a whiteout.
fn in_lower(path: &str) -> bool {
    matches!(data_lookup(path), Some(Some(_))) || (data_lookup(path).is_none() && cpio::files().contains_key(path))
}

/// The canonical path `path` names (relative to `cwd`), following symbolic links; the last one
/// only if `follow`.
pub fn resolve(cwd: &str, path: &str, follow: bool) -> Result<String, i64> {
    resolve_inner(cwd, path, follow, true)
}

/// Like resolve, but the last component need not exist (for creating it). Its parent must.
pub fn resolve_parent(cwd: &str, path: &str) -> Result<String, i64> {
    resolve_inner(cwd, path, false, false)
}

fn resolve_inner(cwd: &str, path: &str, follow: bool, must_exist: bool) -> Result<String, i64> {
    if path.is_empty() {
        return Err(ENOENT);
    }
    let mut queue: Vec<String> = Vec::new();
    let push_path = |q: &mut Vec<String>, p: &str| {
        for c in p.split('/').rev() {
            q.push(c.to_string());
        }
    };
    let mut done: Vec<String> = if path.starts_with('/') { Vec::new() } else { cwd.split('/').filter(|c| !c.is_empty()).map(|c| c.to_string()).collect() };
    push_path(&mut queue, path);
    let mut links = 0;
    while let Some(c) = queue.pop() {
        if c.is_empty() || c == "." {
            continue;
        }
        if c == ".." {
            done.pop();
            continue;
        }
        done.push(c);
        let p = done.join("/");
        let last = queue.iter().all(|c| c.is_empty() || c == ".");
        let Some(node) = lookup(&p) else {
            if last && !must_exist {
                return Ok(p);
            }
            return Err(ENOENT);
        };
        if node.is_link() && (!last || follow) {
            links += 1;
            if links > 40 {
                return Err(ELOOP);
            }
            done.pop();
            let target = node.link_target();
            if target.starts_with('/') {
                done.clear();
            }
            push_path(&mut queue, &target);
        } else if !last && !node.is_dir() {
            return Err(ENOTDIR);
        }
    }
    Ok(done.join("/"))
}

fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

fn check_parent(path: &str) -> Result<(), i64> {
    match lookup(parent_of(path)) {
        Some(n) if n.is_dir() => Ok(()),
        Some(_) => Err(ENOTDIR),
        None => Err(ENOENT),
    }
}

/// The entries of directory `dir`: (name, file), sorted.
pub fn list(dir: &str) -> Vec<(String, Node)> {
    let mut out: BTreeMap<String, Node> = BTreeMap::new();
    for (name, n) in cpio::list(dir) {
        out.insert(name.to_string(), Node::Base(n));
    }
    for (name, n) in data_list(dir) {
        match n {
            Some(n) => {
                out.insert(name, n);
            }
            None => {
                out.remove(&name);
            }
        }
    }
    let prefix = if dir.is_empty() { String::new() } else { alloc::format!("{dir}/") };
    for (k, v) in layer().range(prefix.clone()..) {
        if !k.starts_with(&prefix) {
            break;
        }
        let rest = &k[prefix.len()..];
        if rest.is_empty() || rest.contains('/') {
            continue;
        }
        match v {
            Some(m) => {
                out.insert(rest.to_string(), Node::Mem(m.clone()));
            }
            None => {
                out.remove(rest);
            }
        }
    }
    out.into_iter().collect()
}

/// A file of the RAM layer to write to: the existing one, a copy of the base image's, or a
/// new empty one (when `create`).
pub fn writable(path: &str, create: bool, mode: u32) -> Result<Rc<RefCell<MemNode>>, i64> {
    match lookup(path) {
        Some(Node::Mem(m)) => {
            if m.borrow().mode & S_IFMT == S_IFDIR {
                return Err(EISDIR);
            }
            Ok(m)
        }
        Some(n @ (Node::Base(_) | Node::Ext(_))) => {
            if n.is_dir() {
                return Err(EISDIR);
            }
            let m = mem(n.mode(), n.bytes(), 0);
            layer().insert(path.to_string(), Some(m.clone()));
            mark_dirty();
            Ok(m)
        }
        None if create => {
            check_parent(path)?;
            let m = mem(S_IFREG | (mode & 0o7777), Vec::new(), 0);
            layer().insert(path.to_string(), Some(m.clone()));
            mark_dirty();
            Ok(m)
        }
        None => Err(ENOENT),
    }
}

pub fn mkdir(path: &str, mode: u32) -> Result<(), i64> {
    if lookup(path).is_some() {
        return Err(EEXIST);
    }
    check_parent(path)?;
    layer().insert(path.to_string(), Some(mem(S_IFDIR | (mode & 0o7777), Vec::new(), 0)));
    mark_dirty();
    Ok(())
}

pub fn symlink(target: &str, path: &str) -> Result<(), i64> {
    if lookup(path).is_some() {
        return Err(EEXIST);
    }
    check_parent(path)?;
    layer().insert(path.to_string(), Some(mem(S_IFLNK | 0o777, target.as_bytes().to_vec(), 0)));
    mark_dirty();
    Ok(())
}

pub fn mknod_fifo(path: &str, mode: u32) -> Result<(), i64> {
    if lookup(path).is_some() {
        return Err(EEXIST);
    }
    check_parent(path)?;
    layer().insert(path.to_string(), Some(mem(S_IFIFO | (mode & 0o7777), Vec::new(), 0)));
    Ok(())
}

/// unlink (dir = false) or rmdir (dir = true)
pub fn remove(path: &str, dir: bool) -> Result<(), i64> {
    if path.is_empty() {
        return Err(EBUSY);
    }
    let n = lookup(path).ok_or(ENOENT)?;
    if dir {
        if !n.is_dir() {
            return Err(ENOTDIR);
        }
        if !list(path).is_empty() {
            return Err(ENOTEMPTY);
        }
    } else if n.is_dir() {
        return Err(EISDIR);
    }
    // hide a lower layer's file, or just drop the RAM one
    if in_lower(path) {
        layer().insert(path.to_string(), None);
    } else {
        layer().remove(path);
    }
    mark_dirty();
    Ok(())
}

pub fn rename(from: &str, to: &str) -> Result<(), i64> {
    let n = lookup(from).ok_or(ENOENT)?;
    if from == to {
        return Ok(());
    }
    check_parent(to)?;
    if let Some(existing) = lookup(to) {
        if existing.is_dir() != n.is_dir() {
            return Err(if existing.is_dir() { EISDIR } else { ENOTDIR });
        }
        if existing.is_dir() && !list(to).is_empty() {
            return Err(ENOTEMPTY);
        }
    }
    if to.starts_with(&alloc::format!("{from}/")) {
        return Err(EINVAL);
    }
    // move the file and, for a directory, everything under it
    let mut moves: Vec<(String, String)> = alloc::vec![(from.to_string(), to.to_string())];
    if n.is_dir() {
        let mut stack = alloc::vec![from.to_string()];
        while let Some(d) = stack.pop() {
            for (name, child) in list(&d) {
                let src = alloc::format!("{d}/{name}");
                let dst = alloc::format!("{to}{}", &src[from.len()..]);
                if child.is_dir() {
                    stack.push(src.clone());
                }
                moves.push((src, dst));
            }
        }
    }
    let mut nodes = Vec::new();
    for (src, dst) in &moves {
        let node = match lookup(src) {
            Some(Node::Mem(m)) => m,
            Some(n) => mem(n.mode(), n.bytes(), 0),
            None => continue,
        };
        nodes.push((dst.clone(), node));
    }
    for (src, _) in moves.iter().rev() {
        if in_lower(src) {
            layer().insert(src.clone(), None);
        } else {
            layer().remove(src.as_str());
        }
    }
    for (dst, node) in nodes {
        layer().insert(dst, Some(node));
    }
    mark_dirty();
    Ok(())
}

pub fn chmod(path: &str, mode: u32) -> Result<(), i64> {
    let n = lookup(path).ok_or(ENOENT)?;
    let m = match n {
        Node::Mem(m) => m,
        n => {
            let m = mem(n.mode(), n.bytes(), 0);
            layer().insert(path.to_string(), Some(m.clone()));
            m
        }
    };
    mark_dirty();
    let mut m = m.borrow_mut();
    m.mode = (m.mode & S_IFMT) | (mode & 0o7777);
    Ok(())
}

pub fn touch(path: &str, t: u64) {
    if let Some(Node::Mem(m)) = lookup(path) {
        m.borrow_mut().mtime = t;
    }
}

/// Bytes in the RAM layer (for sysinfo and the like).
pub fn ram_bytes() -> usize {
    layer().values().flatten().map(|m| m.borrow().data.len()).sum()
}

/// Not kept between restarts: device files, /tmp, /run.
fn volatile(path: &str) -> bool {
    path == "dev" || path.starts_with("dev/") || path == "tmp" || path.starts_with("tmp/") || path == "run" || path.starts_with("run/") || path == "proc" || path == "sys"
}

fn cpio_entry(out: &mut Vec<u8>, name: &str, mode: u32, mtime: u64, data: &[u8]) {
    let hdr = alloc::format!(
        "070701{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}",
        0, mode, 0, 0, 1, mtime as u32, data.len(), 0, 0, 0, 0, name.len() + 1, 0
    );
    out.extend_from_slice(hdr.as_bytes());
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out.extend_from_slice(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

/// The RAM layer as a cpio archive (deleted files as 0/0 character devices, as overlayfs
/// marks them).
pub fn export() -> Vec<u8> {
    let mut out = Vec::new();
    for (path, v) in layer().iter() {
        if volatile(path) {
            continue;
        }
        match v {
            Some(m) => {
                let m = m.borrow();
                cpio_entry(&mut out, path, m.mode, m.mtime, &m.data);
            }
            None => cpio_entry(&mut out, path, S_IFCHR, 0, &[]),
        }
    }
    cpio_entry(&mut out, "TRAILER!!!", 0, 0, &[]);
    out
}

/// Put back a RAM layer saved with export().
pub fn import(img: &[u8]) -> usize {
    let mut off = 0usize;
    let mut n = 0;
    let hex = |b: &[u8]| core::str::from_utf8(b).ok().and_then(|s| u64::from_str_radix(s, 16).ok()).unwrap_or(0);
    while off + 110 <= img.len() && &img[off..off + 6] == b"070701" {
        let h = &img[off..off + 110];
        let field = |i: usize| hex(&h[6 + i * 8..14 + i * 8]);
        let mode = field(1) as u32;
        let mtime = field(5);
        let size = field(6) as usize;
        let namesize = field(11) as usize;
        let name_start = off + 110;
        let Some(name) = img.get(name_start..name_start + namesize.saturating_sub(1)) else { break };
        let data_start = (name_start + namesize + 3) & !3;
        let Some(data) = img.get(data_start..data_start + size) else { break };
        off = (data_start + size + 3) & !3;
        let name = String::from_utf8_lossy(name).into_owned();
        if name == "TRAILER!!!" {
            break;
        }
        if mode == S_IFCHR && size == 0 {
            layer().insert(name, None);
        } else {
            layer().insert(name, Some(Rc::new(RefCell::new(MemNode { mode, ino: new_ino(), mtime, rdev: 0, data: data.to_vec() }))));
        }
        n += 1;
    }
    n
}
