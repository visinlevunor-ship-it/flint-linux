//! The root filesystem: the base image flint-boot hands over (cpio "newc" archives, one after
//! another; a later file replaces an earlier one with the same name, as on Linux). Read-only
//! for now.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub const S_IFMT: u32 = 0o170000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFLNK: u32 = 0o120000;

pub struct Node {
    pub mode: u32,
    pub ino: u64,
    pub mtime: u64,
    pub data: &'static [u8],
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }
    pub fn is_link(&self) -> bool {
        self.mode & S_IFMT == S_IFLNK
    }
}

/// path (no leading slash; "" is the root) -> file
static mut FILES: Option<BTreeMap<String, Node>> = None;

pub fn files() -> &'static BTreeMap<String, Node> {
    // SAFETY: filled once at start-up, read-only afterwards.
    unsafe { (*core::ptr::addr_of!(FILES)).as_ref().expect("root filesystem not loaded") }
}

fn hex(b: &[u8]) -> Option<u64> {
    let s = core::str::from_utf8(b).ok()?;
    u64::from_str_radix(s, 16).ok()
}

/// Read every archive in `img`; returns how many files there are.
pub fn load(img: &'static [u8]) -> usize {
    let mut map = BTreeMap::new();
    map.insert(String::new(), Node { mode: S_IFDIR | 0o755, ino: 1, mtime: 0, data: &[] });
    let mut off = 0usize;
    let mut ino = 2;
    while off + 110 <= img.len() {
        let h = &img[off..off + 110];
        if &h[..6] != b"070701" && &h[..6] != b"070702" {
            // zero padding between archives: skip to the next 4-byte boundary with data
            if h[0] == 0 {
                off += 4;
                continue;
            }
            break; // something else (a compressed archive): stop here
        }
        let field = |i: usize| hex(&h[6 + i * 8..14 + i * 8]).unwrap_or(0);
        let mode = field(1) as u32;
        let mtime = field(5);
        let size = field(6) as usize;
        let namesize = field(11) as usize;
        let name_start = off + 110;
        let Some(name_bytes) = img.get(name_start..name_start + namesize.saturating_sub(1)) else { break };
        let data_start = (name_start + namesize + 3) & !3;
        let Some(data) = img.get(data_start..data_start + size) else { break };
        off = (data_start + size + 3) & !3;
        let name = core::str::from_utf8(name_bytes).unwrap_or("");
        if name == "TRAILER!!!" {
            continue;
        }
        let name = name.trim_start_matches("./").trim_start_matches('/').trim_end_matches('/');
        if name.is_empty() || name == "." {
            continue;
        }
        map.insert(name.to_string(), Node { mode, ino, mtime, data });
        ino += 1;
    }
    // directories an archive only implies (it lists a/b/c but not a/b)
    let mut parents = Vec::new();
    for k in map.keys() {
        let mut p = k.as_str();
        while let Some(i) = p.rfind('/') {
            p = &p[..i];
            if !map.contains_key(p) {
                parents.push(p.to_string());
            }
        }
    }
    for p in parents {
        map.entry(p).or_insert_with(|| {
            ino += 1;
            Node { mode: S_IFDIR | 0o755, ino, mtime: 0, data: &[] }
        });
    }
    let n = map.len();
    // SAFETY: start-up, before anything reads FILES.
    unsafe { FILES = Some(map) };
    n
}

pub const ENOENT: i64 = 2;
pub const ENOTDIR: i64 = 20;
pub const ELOOP: i64 = 40;

/// The file `path` names (relative to `cwd`), following symbolic links; the last one only if
/// `follow`. Returns its canonical path.
pub fn resolve(cwd: &str, path: &str, follow: bool) -> Result<String, i64> {
    let mut queue: Vec<String> = Vec::new();
    // components to visit, in reverse so pop() takes the next one
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
        let Some(node) = files().get(&p) else { return Err(ENOENT) };
        if node.is_link() && (!last || follow) {
            links += 1;
            if links > 40 {
                return Err(ELOOP);
            }
            done.pop();
            let target = core::str::from_utf8(node.data).unwrap_or("");
            if target.starts_with('/') {
                done.clear();
            }
            push_path(&mut queue, target);
        } else if !last && !node.is_dir() {
            return Err(ENOTDIR);
        }
    }
    Ok(done.join("/"))
}

/// The entries of directory `dir` (canonical path): (name, file).
pub fn list(dir: &str) -> Vec<(&'static str, &'static Node)> {
    let prefix = if dir.is_empty() { String::new() } else { alloc::format!("{dir}/") };
    let mut out = Vec::new();
    for (k, v) in files().range(prefix.clone()..) {
        if !k.starts_with(&prefix) {
            break;
        }
        let rest = &k[prefix.len()..];
        if rest.is_empty() || rest.contains('/') {
            continue;
        }
        out.push((rest, v));
    }
    out
}
