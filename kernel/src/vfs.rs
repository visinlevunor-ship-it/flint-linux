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

#[derive(Clone)]
pub enum Node {
    Base(&'static cpio::Node),
    Mem(Rc<RefCell<MemNode>>),
}

impl Node {
    pub fn mode(&self) -> u32 {
        match self {
            Node::Base(n) => n.mode,
            Node::Mem(m) => m.borrow().mode,
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
        }
    }
    pub fn mtime(&self) -> u64 {
        match self {
            Node::Base(n) => n.mtime,
            Node::Mem(m) => m.borrow().mtime,
        }
    }
    pub fn rdev(&self) -> u64 {
        match self {
            Node::Base(_) => 0,
            Node::Mem(m) => m.borrow().rdev,
        }
    }
    pub fn size(&self) -> usize {
        match self {
            Node::Base(n) => n.data.len(),
            Node::Mem(m) => m.borrow().data.len(),
        }
    }
    pub fn link_target(&self) -> String {
        match self {
            Node::Base(n) => String::from_utf8_lossy(n.data).into_owned(),
            Node::Mem(m) => String::from_utf8_lossy(&m.borrow().data).into_owned(),
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
        }
    }
    /// The whole contents (for loading programs).
    pub fn bytes(&self) -> Vec<u8> {
        match self {
            Node::Base(n) => n.data.to_vec(),
            Node::Mem(m) => m.borrow().data.clone(),
        }
    }
    pub fn base_bytes(&self) -> Option<&'static [u8]> {
        match self {
            Node::Base(n) => Some(n.data),
            Node::Mem(_) => None,
        }
    }
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
        None => cpio::files().get(path).map(Node::Base),
    }
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
        Some(Node::Base(b)) => {
            if b.mode & S_IFMT == S_IFDIR {
                return Err(EISDIR);
            }
            let m = mem(b.mode, b.data.to_vec(), 0);
            layer().insert(path.to_string(), Some(m.clone()));
            Ok(m)
        }
        None if create => {
            check_parent(path)?;
            let m = mem(S_IFREG | (mode & 0o7777), Vec::new(), 0);
            layer().insert(path.to_string(), Some(m.clone()));
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
    Ok(())
}

pub fn symlink(target: &str, path: &str) -> Result<(), i64> {
    if lookup(path).is_some() {
        return Err(EEXIST);
    }
    check_parent(path)?;
    layer().insert(path.to_string(), Some(mem(S_IFLNK | 0o777, target.as_bytes().to_vec(), 0)));
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
    // hide the base image's file, or just drop the RAM one
    if cpio::files().contains_key(path) {
        layer().insert(path.to_string(), None);
    } else {
        layer().remove(path);
    }
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
            Some(Node::Base(b)) => mem(b.mode, b.data.to_vec(), 0),
            None => continue,
        };
        nodes.push((dst.clone(), node));
    }
    for (src, _) in moves.iter().rev() {
        if cpio::files().contains_key(src.as_str()) {
            layer().insert(src.clone(), None);
        } else {
            layer().remove(src.as_str());
        }
    }
    for (dst, node) in nodes {
        layer().insert(dst, Some(node));
    }
    Ok(())
}

pub fn chmod(path: &str, mode: u32) -> Result<(), i64> {
    let n = lookup(path).ok_or(ENOENT)?;
    let m = match n {
        Node::Mem(m) => m,
        Node::Base(b) => {
            let m = mem(b.mode, b.data.to_vec(), 0);
            layer().insert(path.to_string(), Some(m.clone()));
            m
        }
    };
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
