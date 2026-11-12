//! Installing, removing and verifying packages on disk.
//!
//! Safety rules: every path is checked to stay inside the root; files are written to a
//! temporary name in the target directory and renamed into place (so each file appears
//! atomically); a file that belongs to another package, or an unowned file that already
//! exists, is a conflict; and the database record is written last. If a fresh install
//! fails half way, the files it created are removed again.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use flint_auth::hash::hex;
use flint_auth::sha256::Sha256;

use crate::control::Control;
use crate::db::{Db, FileRec, Installed};
use crate::pack::{hash_file, valid_link_target, CONTROL_NAME};
use crate::paths::{normalize_rel, safe_join};
use crate::resolve::{Source, Step};
use crate::tar::{Entry, Kind, Reader};

pub struct Ctx {
    pub root: PathBuf,
    pub force: bool,
    pub db: Db,
}

/// Exclusive lock for commands that change the system.
pub struct Lock(PathBuf);

impl Lock {
    pub fn acquire(root: &Path) -> Result<Lock, String> {
        let p = root.join("var/lib/pkg/lock");
        if let Some(dir) = p.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        match fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(mut f) => {
                let _ = writeln!(f, "{}", std::process::id());
                Ok(Lock(p))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(format!("another pkg seems to be running; if it is not, remove {}", p.display()))
            }
            Err(e) => Err(format!("{}: {e}", p.display())),
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn is_root() -> bool {
    // SAFETY: plain libc query.
    unsafe { libc::geteuid() == 0 }
}

/// Set ownership (only when running as root). Must come *before* setting the mode:
/// on Linux a chown clears the setuid and setgid bits.
fn chown(path: &Path, uid: u32, gid: u32) {
    if !is_root() {
        return;
    }
    if let Ok(c) = CString::new(path.as_os_str().as_bytes()) {
        // SAFETY: valid NUL-terminated path; a failure is not fatal (the file stays root-owned).
        unsafe { libc::lchown(c.as_ptr(), uid, gid) };
    }
}

fn io(e: std::io::Error, what: &Path) -> String {
    format!("{}: {e}", what.display())
}

/// Path -> owning package, for every non-directory entry in the database.
fn owners(db: &Db) -> Result<HashMap<String, String>, String> {
    let mut m = HashMap::new();
    for i in db.all()? {
        for f in i.files.iter().filter(|f| f.kind != 'd') {
            m.insert(f.path.clone(), i.control.name.clone());
        }
    }
    Ok(m)
}

/// Directories listed by any package other than `except`.
fn shared_dirs(db: &Db, except: &str) -> Result<HashSet<String>, String> {
    let mut s = HashSet::new();
    for i in db.all()? {
        if i.control.name != except {
            s.extend(i.files.iter().filter(|f| f.kind == 'd').map(|f| f.path.clone()));
        }
    }
    Ok(s)
}

/// Remove now-unused directories, deepest first. Non-empty or shared ones stay.
fn prune_dirs(ctx: &Ctx, dirs: Vec<&String>, keep: &HashSet<String>) {
    let mut dirs: Vec<&String> = dirs.into_iter().filter(|d| !keep.contains(*d)).collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.len()));
    for d in dirs {
        if let Ok(full) = safe_join(&ctx.root, d) {
            let _ = fs::remove_dir(&full); // fails harmlessly while something is still inside
        }
    }
}

/// A user-modified config file is one whose content no longer matches the recorded hash.
fn config_modified(full: &Path, recorded: &str) -> bool {
    hash_file(full).is_ok_and(|(h, _)| h != recorded)
}

/// Undo log for a fresh install: what was created, so a failure can remove it.
#[derive(Default)]
struct Created {
    files: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
}

impl Created {
    fn undo(&self) {
        for f in self.files.iter().rev() {
            let _ = fs::remove_file(f);
        }
        for d in self.dirs.iter().rev() {
            let _ = fs::remove_dir(d);
        }
    }
}

fn create_parents(full: &Path, created: &mut Created) -> Result<(), String> {
    let Some(parent) = full.parent() else { return Ok(()) };
    let mut missing = Vec::new();
    let mut p = parent;
    while fs::symlink_metadata(p).is_err() {
        missing.push(p.to_path_buf());
        match p.parent() {
            Some(pp) => p = pp,
            None => break,
        }
    }
    for d in missing.into_iter().rev() {
        fs::create_dir(&d).map_err(|e| io(e, &d))?;
        created.dirs.push(d);
    }
    Ok(())
}

pub fn install_step(ctx: &Ctx, step: &Step, old: Option<&Installed>, notes: &mut Vec<String>) -> Result<Installed, String> {
    // 1. Where the package comes from; repository packages must match the index exactly.
    let pkg_path: PathBuf = match &step.source {
        Source::Repo { path, size, sha256, .. } => {
            let (h, s) = hash_file(path).map_err(|e| io(e, path))?;
            if s != *size || h != *sha256 {
                return Err(format!("checksum mismatch for {}: the file does not match the repository index, refusing to install it", path.display()));
            }
            path.clone()
        }
        Source::File(p) => p.clone(),
    };

    // 2. Read and check the whole archive before touching the system.
    let file = crate::pack::open_archive(&pkg_path).map_err(|e| io(e, &pkg_path))?;
    let mut reader = Reader::new(file);
    let bad = |m: String| format!("{}: {m}", pkg_path.display());
    let first = reader.next_entry().map_err(|e| bad(e.to_string()))?.ok_or_else(|| bad("empty archive".into()))?;
    if first.header.name != CONTROL_NAME || first.header.kind != Kind::File || first.header.size > 65536 {
        return Err(bad("not a flint package".into()));
    }
    let mut text = String::new();
    reader.data(&first).and_then(|mut d| d.read_to_string(&mut text)).map_err(|e| bad(e.to_string()))?;
    let control = Control::parse(&text).map_err(bad)?;
    if control.name != step.control.name || control.version != step.control.version {
        return Err(bad(format!("contains {} {} but {} {} was expected", control.name, control.version, step.control.name, step.control.version)));
    }

    let mut entries: Vec<(String, Entry)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut symlinks: HashSet<String> = HashSet::new();
    while let Some(e) = reader.next_entry().map_err(|e| bad(e.to_string()))? {
        let rel = normalize_rel(&e.header.name).map_err(bad)?;
        if rel == CONTROL_NAME || rel.split('/').any(|c| c.starts_with(".pkg-tmp-")) || rel.starts_with("var/lib/pkg") {
            return Err(bad(format!("reserved path {rel}")));
        }
        if !seen.insert(rel.clone()) {
            return Err(bad(format!("{rel} appears twice")));
        }
        if e.header.kind == Kind::Symlink {
            if !valid_link_target(&e.header.link) {
                return Err(bad(format!("symlink {rel} has a bad target")));
            }
            symlinks.insert(rel.clone());
        }
        entries.push((rel, e));
    }
    // Nothing may be placed below a symlink from the same archive (the classic escape).
    for (rel, _) in &entries {
        for (i, _) in rel.match_indices('/') {
            if symlinks.contains(&rel[..i]) {
                return Err(bad(format!("{rel} lies below the symlink {}", &rel[..i])));
            }
        }
    }

    // 3. Conflicts, checked for every entry before anything is written.
    let owner_of = owners(&ctx.db)?;
    let old_files: HashMap<&str, &FileRec> = old.map(|o| o.files.iter().map(|f| (f.path.as_str(), f)).collect()).unwrap_or_default();
    for (rel, e) in &entries {
        let full = safe_join(&ctx.root, rel)?;
        let meta = fs::symlink_metadata(&full).ok();
        if e.header.kind == Kind::Dir {
            if let Some(m) = &meta {
                let dir_like = m.is_dir() || (m.file_type().is_symlink() && fs::metadata(&full).is_ok_and(|t| t.is_dir()));
                if !dir_like {
                    return Err(format!("/{rel} is a directory in the package but not on the system"));
                }
            }
            continue;
        }
        match owner_of.get(rel.as_str()) {
            Some(o) if *o != control.name => return Err(format!("/{rel} is already owned by package {o}")),
            Some(_) => {}
            None => {
                if meta.is_some() && !old_files.contains_key(rel.as_str()) && !ctx.force {
                    return Err(format!("/{rel} already exists and belongs to no package (use --force to overwrite it)"));
                }
            }
        }
        if meta.as_ref().is_some_and(|m| m.is_dir()) {
            return Err(format!("/{rel} is a file in the package but a directory on the system"));
        }
    }

    // 4. Extract.
    let mut created = Created::default();
    let fresh = old.is_none();
    let result = (|| -> Result<Vec<FileRec>, String> {
        let mut records: Vec<FileRec> = Vec::new();
        for (idx, (rel, e)) in entries.iter().enumerate() {
            let full = safe_join(&ctx.root, rel)?;
            let h = &e.header;
            create_parents(&full, &mut created)?;
            match h.kind {
                Kind::Dir => {
                    if fs::symlink_metadata(&full).is_err() {
                        fs::create_dir(&full).map_err(|e| io(e, &full))?;
                        created.dirs.push(full.clone());
                        chown(&full, h.uid, h.gid);
                        fs::set_permissions(&full, fs::Permissions::from_mode(h.mode)).map_err(|e| io(e, &full))?;
                    }
                    records.push(FileRec { kind: 'd', mode: h.mode, data: String::new(), path: rel.clone() });
                }
                Kind::Symlink => {
                    let existed = fs::symlink_metadata(&full).is_ok();
                    if existed {
                        fs::remove_file(&full).map_err(|e| io(e, &full))?;
                    }
                    symlink(&h.link, &full).map_err(|e| io(e, &full))?;
                    if !existed {
                        created.files.push(full.clone());
                    }
                    chown(&full, h.uid, h.gid);
                    records.push(FileRec { kind: 'l', mode: 0o777, data: h.link.clone(), path: rel.clone() });
                }
                Kind::File => {
                    let parent = full.parent().ok_or("bad path")?.to_path_buf();
                    let tmp = parent.join(format!(".pkg-tmp-{}-{idx}", std::process::id()));
                    let _ = fs::remove_file(&tmp);
                    let written = (|| -> Result<String, String> {
                        let mut out = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).map_err(|e| io(e, &tmp))?;
                        let mut hasher = Sha256::new();
                        let mut src = reader.data(e).map_err(|e| bad(e.to_string()))?;
                        let mut buf = [0u8; 65536];
                        let mut total = 0u64;
                        loop {
                            let n = src.read(&mut buf).map_err(|e| bad(e.to_string()))?;
                            if n == 0 {
                                break;
                            }
                            hasher.update(&buf[..n]);
                            out.write_all(&buf[..n]).map_err(|e| io(e, &tmp))?;
                            total += n as u64;
                        }
                        if total != h.size {
                            return Err(bad(format!("{rel} is truncated")));
                        }
                        out.flush().map_err(|e| io(e, &tmp))?;
                        drop(out);
                        chown(&tmp, h.uid, h.gid);
                        fs::set_permissions(&tmp, fs::Permissions::from_mode(h.mode)).map_err(|e| io(e, &tmp))?;
                        Ok(hex(&hasher.finalize()))
                    })();
                    let hash = match written {
                        Ok(h) => h,
                        Err(e) => {
                            let _ = fs::remove_file(&tmp);
                            return Err(e);
                        }
                    };
                    let existed = fs::symlink_metadata(&full).is_ok();
                    let mut dest = full.clone();
                    // A config file the user edited is kept; the package's version goes next to it.
                    if control.config.contains(rel) && existed {
                        if let Some(rec) = old_files.get(rel.as_str()) {
                            let current = hash_file(&full).map(|(h, _)| h).unwrap_or_default();
                            if current != rec.data && current != hash {
                                dest = PathBuf::from(format!("{}.flint-new", full.display()));
                                notes.push(format!("/{rel} was modified; the new version was saved as /{rel}.flint-new"));
                            }
                        }
                    }
                    fs::rename(&tmp, &dest).map_err(|e| {
                        let _ = fs::remove_file(&tmp);
                        io(e, &dest)
                    })?;
                    if !existed {
                        created.files.push(full.clone());
                    }
                    // The record keeps the hash of what the package ships, so a later upgrade
                    // still sees a user's edit as an edit.
                    records.push(FileRec { kind: 'f', mode: h.mode, data: hash, path: rel.clone() });
                }
            }
        }
        Ok(records)
    })();
    let records = match result {
        Ok(r) => r,
        Err(e) => {
            if fresh {
                created.undo();
                return Err(format!("{e} (the partial install was rolled back)"));
            }
            return Err(format!("{e} (the upgrade stopped half way; run the upgrade again, or `pkg verify {}`)", control.name));
        }
    };

    // 5. Upgrade: remove what the new version no longer has.
    if let Some(o) = old {
        let new_paths: HashSet<&str> = records.iter().map(|r| r.path.as_str()).collect();
        let mut dirs: Vec<&String> = Vec::new();
        for f in &o.files {
            if new_paths.contains(f.path.as_str()) {
                continue;
            }
            if f.kind == 'd' {
                dirs.push(&f.path);
                continue;
            }
            let full = safe_join(&ctx.root, &f.path)?;
            if f.kind == 'f' && o.control.config.contains(&f.path) && config_modified(&full, &f.data) {
                notes.push(format!("kept modified config file /{} (no longer part of the package)", f.path));
                continue;
            }
            let _ = fs::remove_file(&full);
        }
        let mut keep = shared_dirs(&ctx.db, &control.name)?;
        keep.extend(records.iter().filter(|r| r.kind == 'd').map(|r| r.path.clone()));
        prune_dirs(ctx, dirs, &keep);
    }

    let installed_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let rec = Installed { control, files: records, installed_at, explicit: step.explicit || old.is_some_and(|o| o.explicit) };
    if let Err(e) = ctx.db.save(&rec) {
        if fresh {
            created.undo();
        }
        return Err(e);
    }
    Ok(rec)
}

pub fn remove_pkg(ctx: &Ctx, name: &str, notes: &mut Vec<String>) -> Result<(), String> {
    let rec = ctx.db.load(name)?.ok_or_else(|| format!("{name} is not installed"))?;
    let mut dirs: Vec<&String> = Vec::new();
    for f in rec.files.iter().rev() {
        if f.kind == 'd' {
            dirs.push(&f.path);
            continue;
        }
        let full = safe_join(&ctx.root, &f.path)?;
        if f.kind == 'f' && rec.control.config.contains(&f.path) && config_modified(&full, &f.data) {
            notes.push(format!("kept modified config file /{}", f.path));
            continue;
        }
        match fs::remove_file(&full) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(e, &full)),
        }
    }
    let keep = shared_dirs(&ctx.db, name)?;
    prune_dirs(ctx, dirs, &keep);
    ctx.db.delete(name)
}

/// Compare the system with what the database recorded. Returns one line per problem.
pub fn verify(ctx: &Ctx, rec: &Installed) -> Vec<String> {
    let mut problems = Vec::new();
    for f in &rec.files {
        let Ok(full) = safe_join(&ctx.root, &f.path) else {
            problems.push(format!("bad path in database: {}", f.path));
            continue;
        };
        let Ok(meta) = fs::symlink_metadata(&full) else {
            problems.push(format!("missing: /{}", f.path));
            continue;
        };
        match f.kind {
            'd' => {
                if !meta.is_dir() && !fs::metadata(&full).is_ok_and(|m| m.is_dir()) {
                    problems.push(format!("not a directory: /{}", f.path));
                }
            }
            'l' => {
                if fs::read_link(&full).map(|t| t.to_string_lossy().into_owned()).ok().as_deref() != Some(f.data.as_str()) {
                    problems.push(format!("symlink changed: /{}", f.path));
                }
            }
            _ => {
                if !meta.is_file() {
                    problems.push(format!("not a regular file: /{}", f.path));
                    continue;
                }
                let Ok((h, _)) = hash_file(&full) else {
                    problems.push(format!("unreadable: /{}", f.path));
                    continue;
                };
                if h != f.data {
                    let what = if rec.control.config.contains(&f.path) { "modified config" } else { "modified" };
                    problems.push(format!("{what}: /{}", f.path));
                } else {
                    let mode = meta.mode() & 0o7777;
                    if mode != f.mode {
                        problems.push(format!("mode: /{} is {mode:o}, expected {:o}", f.path, f.mode));
                    }
                }
            }
        }
    }
    problems
}
