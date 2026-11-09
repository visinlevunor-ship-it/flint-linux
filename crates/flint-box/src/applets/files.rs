//! File applets: cat head tail wc mkdir rmdir rm touch ln chmod chown cp mv.

use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use flint_auth::files::{parse_group, parse_passwd, Group, User};

use crate::util::{self, error};

fn parse_or_report(prog: &str, args: &[String], flags: &str, valued: &str) -> Option<util::Parsed> {
    match util::parse(args, flags, valued) {
        Ok(p) => Some(p),
        Err(e) => {
            error(prog, e);
            None
        }
    }
}

fn read_input(name: &str) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    if name == "-" {
        io::stdin().lock().read_to_end(&mut data)?;
    } else {
        fs::File::open(name)?.read_to_end(&mut data)?;
    }
    Ok(data)
}

// ---------------------------------------------------------------- cat

pub fn cat(args: &[String]) -> i32 {
    let Some(p) = parse_or_report("cat", args, "u", "") else { return 1 };
    let files = if p.operands.is_empty() { vec!["-".to_string()] } else { p.operands };
    let mut status = 0;
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for f in &files {
        let r = if f == "-" {
            io::copy(&mut io::stdin().lock(), &mut out).map(|_| ())
        } else {
            fs::File::open(f).and_then(|mut file| io::copy(&mut file, &mut out).map(|_| ()))
        };
        if let Err(e) = r {
            if e.kind() == io::ErrorKind::BrokenPipe {
                return 0;
            }
            error("cat", format!("{f}: {e}"));
            status = 1;
        }
    }
    status
}

// ---------------------------------------------------------------- head, tail, wc

/// The first `n` lines of `data` (keeping their newlines).
pub fn head_lines(data: &[u8], n: usize) -> &[u8] {
    if n == 0 {
        return &data[..0];
    }
    let mut seen = 0;
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' {
            seen += 1;
            if seen == n {
                return &data[..=i];
            }
        }
    }
    data
}

/// The last `n` lines of `data`; with `from_start`, everything from line `n` (1-based) on.
pub fn tail_lines(data: &[u8], n: usize, from_start: bool) -> &[u8] {
    let mut starts = vec![0usize];
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' && i + 1 < data.len() {
            starts.push(i + 1);
        }
    }
    if data.is_empty() {
        return data;
    }
    let total = starts.len();
    let first = if from_start { n.saturating_sub(1).min(total) } else { total.saturating_sub(n) };
    if first >= total {
        return &data[data.len()..];
    }
    &data[starts[first]..]
}

fn parse_count(prog: &str, s: &str) -> Option<(usize, bool)> {
    let (digits, plus) = match s.strip_prefix('+') {
        Some(rest) => (rest, true),
        None => (s, false),
    };
    match digits.parse::<usize>() {
        Ok(n) => Some((n, plus)),
        Err(_) => {
            error(prog, format!("invalid number of lines: '{s}'"));
            None
        }
    }
}

fn head_tail(prog: &str, args: &[String], is_head: bool) -> i32 {
    let Some(p) = parse_or_report(prog, args, "", "n") else { return 1 };
    let Some((n, plus)) = parse_count(prog, p.value('n').unwrap_or("10")) else { return 1 };
    let files = if p.operands.is_empty() { vec!["-".to_string()] } else { p.operands.clone() };
    let mut status = 0;
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for (idx, f) in files.iter().enumerate() {
        let data = match read_input(f) {
            Ok(d) => d,
            Err(e) => {
                error(prog, format!("{f}: {e}"));
                status = 1;
                continue;
            }
        };
        let piece = if is_head { head_lines(&data, n) } else { tail_lines(&data, n, plus) };
        let mut r = Ok(());
        if files.len() > 1 {
            r = write!(out, "{}==> {} <==\n", if idx > 0 { "\n" } else { "" }, f);
        }
        let r = r.and_then(|()| out.write_all(piece));
        if let Err(e) = r {
            return if e.kind() == io::ErrorKind::BrokenPipe { 0 } else { 1 };
        }
    }
    status
}

pub fn head(args: &[String]) -> i32 {
    head_tail("head", args, true)
}

pub fn tail(args: &[String]) -> i32 {
    head_tail("tail", args, false)
}

/// (lines, words, bytes)
pub fn count(data: &[u8]) -> (usize, usize, usize) {
    let lines = data.iter().filter(|&&b| b == b'\n').count();
    let mut words = 0;
    let mut in_word = false;
    for &b in data {
        let space = b.is_ascii_whitespace();
        if !space && !in_word {
            words += 1;
        }
        in_word = !space;
    }
    (lines, words, data.len())
}

pub fn wc(args: &[String]) -> i32 {
    let Some(p) = parse_or_report("wc", args, "lwc", "") else { return 1 };
    let any = p.has('l') || p.has('w') || p.has('c');
    let (show_l, show_w, show_c) = if any { (p.has('l'), p.has('w'), p.has('c')) } else { (true, true, true) };
    let files = if p.operands.is_empty() { vec!["-".to_string()] } else { p.operands.clone() };
    let named = !p.operands.is_empty();
    let mut total = (0, 0, 0);
    let mut status = 0;
    let stdout = io::stdout();
    let mut out = stdout.lock();
    // Like GNU wc: pad the columns only when there is something to line up; a single
    // number for a single input stays bare (so `$(wc -l < file)` is just the number).
    let columns = [show_l, show_w, show_c].iter().filter(|&&b| b).count();
    let width = if columns > 1 || files.len() > 1 { 7 } else { 0 };
    let line = |c: (usize, usize, usize), name: Option<&str>| {
        let mut s = String::new();
        if show_l {
            s.push_str(&format!("{:>width$}", c.0));
        }
        if show_w {
            s.push_str(&format!("{:>width$}", c.1));
        }
        if show_c {
            s.push_str(&format!("{:>width$}", c.2));
        }
        if let Some(n) = name {
            s.push(' ');
            s.push_str(n);
        }
        s
    };
    for f in &files {
        match read_input(f) {
            Ok(d) => {
                let c = count(&d);
                total = (total.0 + c.0, total.1 + c.1, total.2 + c.2);
                if let Err(e) = writeln!(out, "{}", line(c, named.then_some(f.as_str()))) {
                    return if e.kind() == io::ErrorKind::BrokenPipe { 0 } else { 1 };
                }
            }
            Err(e) => {
                error("wc", format!("{f}: {e}"));
                status = 1;
            }
        }
    }
    if files.len() > 1 {
        let _ = writeln!(out, "{}", line(total, Some("total")));
    }
    status
}

// ---------------------------------------------------------------- mkdir, rmdir, rm, touch, ln

pub fn parse_octal(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 4 || !s.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return None;
    }
    u32::from_str_radix(s, 8).ok()
}

pub fn mkdir(args: &[String]) -> i32 {
    let Some(p) = parse_or_report("mkdir", args, "p", "m") else { return 1 };
    if p.operands.is_empty() {
        error("mkdir", "missing operand");
        return 1;
    }
    let mode = match p.value('m') {
        Some(m) => match parse_octal(m) {
            Some(v) => Some(v),
            None => {
                error("mkdir", format!("invalid mode '{m}'"));
                return 1;
            }
        },
        None => None,
    };
    let mut status = 0;
    for d in &p.operands {
        let mut b = fs::DirBuilder::new();
        b.recursive(p.has('p'));
        if let Some(m) = mode {
            b.mode(m);
        }
        if let Err(e) = b.create(d) {
            // `-p` tolerates an existing directory, but not a file in the way.
            if !(p.has('p') && e.kind() == io::ErrorKind::AlreadyExists && Path::new(d).is_dir()) {
                error("mkdir", format!("cannot create directory '{d}': {e}"));
                status = 1;
            }
        }
    }
    status
}

pub fn rmdir(args: &[String]) -> i32 {
    let Some(p) = parse_or_report("rmdir", args, "", "") else { return 1 };
    if p.operands.is_empty() {
        error("rmdir", "missing operand");
        return 1;
    }
    let mut status = 0;
    for d in &p.operands {
        if let Err(e) = fs::remove_dir(d) {
            error("rmdir", format!("failed to remove '{d}': {e}"));
            status = 1;
        }
    }
    status
}

fn remove_all(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

pub fn rm(args: &[String]) -> i32 {
    let Some(p) = parse_or_report("rm", args, "rfRi", "") else { return 1 };
    let recursive = p.has('r') || p.has('R');
    let force = p.has('f');
    if p.operands.is_empty() {
        if force {
            return 0;
        }
        error("rm", "missing operand");
        return 1;
    }
    let mut status = 0;
    for f in &p.operands {
        let path = Path::new(f);
        // Refuse the filesystem root (and "/." style spellings of it).
        if fs::canonicalize(path).map_or(false, |c| c == Path::new("/")) && fs::symlink_metadata(path).map_or(false, |m| m.is_dir()) {
            error("rm", "it is dangerous to operate recursively on '/'");
            status = 1;
            continue;
        }
        match fs::symlink_metadata(path) {
            Err(e) => {
                if !(force && e.kind() == io::ErrorKind::NotFound) {
                    error("rm", format!("cannot remove '{f}': {e}"));
                    status = 1;
                }
            }
            Ok(m) if m.is_dir() && !recursive => {
                error("rm", format!("cannot remove '{f}': Is a directory"));
                status = 1;
            }
            Ok(_) => {
                if let Err(e) = remove_all(path) {
                    if !force {
                        error("rm", format!("cannot remove '{f}': {e}"));
                    }
                    status = 1;
                }
            }
        }
    }
    status
}

pub fn touch(args: &[String]) -> i32 {
    let Some(p) = parse_or_report("touch", args, "c", "") else { return 1 };
    if p.operands.is_empty() {
        error("touch", "missing file operand");
        return 1;
    }
    let mut status = 0;
    for f in &p.operands {
        let exists = fs::symlink_metadata(f).is_ok();
        if !exists {
            if p.has('c') {
                continue;
            }
            if let Err(e) = OpenOptions::new().create(true).write(true).open(f) {
                error("touch", format!("cannot touch '{f}': {e}"));
                status = 1;
            }
            continue;
        }
        let Ok(c) = CString::new(f.as_bytes()) else {
            error("touch", format!("{f}: invalid name"));
            status = 1;
            continue;
        };
        // SAFETY: valid C string; a null times pointer means "now".
        if unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), std::ptr::null(), 0) } != 0 {
            error("touch", format!("cannot touch '{f}': {}", io::Error::last_os_error()));
            status = 1;
        }
    }
    status
}

fn target_in_dir(src: &str, dir: &Path) -> PathBuf {
    let name = Path::new(src).file_name().map(|n| n.to_os_string()).unwrap_or_else(|| src.into());
    dir.join(name)
}

pub fn ln(args: &[String]) -> i32 {
    let Some(p) = parse_or_report("ln", args, "sf", "") else { return 1 };
    let ops = &p.operands;
    if ops.len() < 2 {
        error("ln", "usage: ln [-sf] TARGET LINK_NAME | ln [-sf] TARGET... DIRECTORY");
        return 1;
    }
    let (sources, last) = ops.split_at(ops.len() - 1);
    let last = Path::new(&last[0]);
    let into_dir = last.is_dir();
    if sources.len() > 1 && !into_dir {
        error("ln", format!("target '{}' is not a directory", last.display()));
        return 1;
    }
    let mut status = 0;
    for src in sources {
        let link = if into_dir { target_in_dir(src, last) } else { last.to_path_buf() };
        if p.has('f') && fs::symlink_metadata(&link).is_ok() {
            let _ = fs::remove_file(&link);
        }
        let r = if p.has('s') { symlink(src, &link) } else { fs::hard_link(src, &link) };
        if let Err(e) = r {
            error("ln", format!("failed to create link '{}': {e}", link.display()));
            status = 1;
        }
    }
    status
}

// ---------------------------------------------------------------- chmod

/// Apply an octal (`755`) or symbolic (`u+x,go-w`, `a=r`, `+X`) mode to `current`.
pub fn apply_mode(spec: &str, current: u32, is_dir: bool) -> Result<u32, String> {
    if let Some(v) = parse_octal(spec) {
        return Ok(v);
    }
    let mut mode = current & 0o7777;
    for clause in spec.split(',') {
        let bytes = clause.as_bytes();
        let mut i = 0;
        let mut who = 0u32; // bit mask: 1=u 2=g 4=o
        while i < bytes.len() {
            match bytes[i] {
                b'u' => who |= 1,
                b'g' => who |= 2,
                b'o' => who |= 4,
                b'a' => who |= 7,
                _ => break,
            }
            i += 1;
        }
        if who == 0 {
            who = 7;
        }
        if i >= bytes.len() {
            return Err(format!("invalid mode '{spec}'"));
        }
        // One or more `op perms` pairs, e.g. "u+x-w".
        while i < bytes.len() {
            let op = bytes[i];
            if !matches!(op, b'+' | b'-' | b'=') {
                return Err(format!("invalid mode '{spec}'"));
            }
            i += 1;
            let (mut rwx, mut setid, mut sticky, mut cond_x) = (0u32, false, false, false);
            while i < bytes.len() && !matches!(bytes[i], b'+' | b'-' | b'=') {
                match bytes[i] {
                    b'r' => rwx |= 4,
                    b'w' => rwx |= 2,
                    b'x' => rwx |= 1,
                    b'X' => cond_x = true,
                    b's' => setid = true,
                    b't' => sticky = true,
                    _ => return Err(format!("invalid mode '{spec}'")),
                }
                i += 1;
            }
            if cond_x && (is_dir || mode & 0o111 != 0) {
                rwx |= 1;
            }
            let mut bits = 0u32;
            let mut clear = 0u32;
            for (flag, shift) in [(1u32, 6u32), (2, 3), (4, 0)] {
                if who & flag != 0 {
                    bits |= rwx << shift;
                    clear |= 0o7 << shift;
                }
            }
            if setid {
                if who & 1 != 0 {
                    bits |= 0o4000;
                }
                if who & 2 != 0 {
                    bits |= 0o2000;
                }
            }
            if sticky {
                bits |= 0o1000;
            }
            match op {
                b'+' => mode |= bits,
                b'-' => mode &= !bits,
                _ => {
                    mode &= !clear;
                    mode |= bits;
                }
            }
        }
    }
    Ok(mode)
}

fn chmod_one(spec: &str, path: &Path, recursive: bool) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("cannot access '{}': {e}", path.display()))?;
    if !meta.file_type().is_symlink() {
        let mode = apply_mode(spec, meta.mode(), meta.is_dir())?;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|e| format!("changing permissions of '{}': {e}", path.display()))?;
    }
    if recursive && meta.is_dir() {
        for entry in fs::read_dir(path).map_err(|e| format!("cannot read '{}': {e}", path.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            chmod_one(spec, &entry.path(), true)?;
        }
    }
    Ok(())
}

pub fn chmod(args: &[String]) -> i32 {
    // A leading "-x"-style mode would look like an option, so only -R is parsed first.
    let (recursive, rest) = match args.first().map(String::as_str) {
        Some("-R") => (true, &args[1..]),
        _ => (false, args),
    };
    if rest.len() < 2 {
        error("chmod", "usage: chmod [-R] MODE FILE...");
        return 1;
    }
    let mut status = 0;
    for f in &rest[1..] {
        if let Err(e) = chmod_one(&rest[0], Path::new(f), recursive) {
            error("chmod", e);
            status = 1;
        }
    }
    status
}

// ---------------------------------------------------------------- chown

fn lookup_id(spec: &str, by_name: impl Fn(&str) -> Option<u32>) -> Result<u32, String> {
    spec.parse::<u32>().ok().or_else(|| by_name(spec)).ok_or_else(|| format!("invalid user or group '{spec}'"))
}

/// `USER`, `USER:GROUP`, `:GROUP` or numeric ids.
pub fn parse_owner(spec: &str, users: &[User], groups: &[Group]) -> Result<(Option<u32>, Option<u32>), String> {
    let (u, g) = match spec.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (spec, None),
    };
    let uid = if u.is_empty() {
        None
    } else {
        Some(lookup_id(u, |n| users.iter().find(|x| x.name == n).map(|x| x.uid))?)
    };
    let gid = match g {
        None | Some("") => None,
        Some(g) => Some(lookup_id(g, |n| groups.iter().find(|x| x.name == n).map(|x| x.gid))?),
    };
    if uid.is_none() && gid.is_none() {
        return Err(format!("invalid owner '{spec}'"));
    }
    Ok((uid, gid))
}

fn chown_one(path: &Path, uid: Option<u32>, gid: Option<u32>, recursive: bool) -> Result<(), String> {
    let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| "invalid name".to_string())?;
    // SAFETY: valid C string; u32::MAX means "leave unchanged".
    let rc = unsafe { libc::lchown(c.as_ptr(), uid.unwrap_or(u32::MAX), gid.unwrap_or(u32::MAX)) };
    if rc != 0 {
        return Err(format!("changing ownership of '{}': {}", path.display(), io::Error::last_os_error()));
    }
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if recursive && meta.is_dir() {
        for entry in fs::read_dir(path).map_err(|e| format!("cannot read '{}': {e}", path.display()))? {
            chown_one(&entry.map_err(|e| e.to_string())?.path(), uid, gid, true)?;
        }
    }
    Ok(())
}

pub fn chown(args: &[String]) -> i32 {
    let Some(p) = parse_or_report("chown", args, "R", "") else { return 1 };
    if p.operands.len() < 2 {
        error("chown", "usage: chown [-R] OWNER[:GROUP] FILE...");
        return 1;
    }
    let users = parse_passwd(&fs::read_to_string("/etc/passwd").unwrap_or_default());
    let groups = parse_group(&fs::read_to_string("/etc/group").unwrap_or_default());
    let (uid, gid) = match parse_owner(&p.operands[0], &users, &groups) {
        Ok(v) => v,
        Err(e) => {
            error("chown", e);
            return 1;
        }
    };
    let mut status = 0;
    for f in &p.operands[1..] {
        if let Err(e) = chown_one(Path::new(f), uid, gid, p.has('R')) {
            error("chown", e);
            status = 1;
        }
    }
    status
}

// ---------------------------------------------------------------- cp, mv

/// Copy `src` to exactly `dst` (not "into" it).
fn copy_path(src: &Path, dst: &Path, recursive: bool) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        let target = fs::read_link(src)?;
        if fs::symlink_metadata(dst).is_ok() {
            fs::remove_file(dst)?;
        }
        symlink(target, dst)
    } else if ft.is_dir() {
        if !recursive {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("-r not specified; omitting directory '{}'", src.display())));
        }
        if let (Ok(s), Ok(d)) = (fs::canonicalize(src), fs::canonicalize(dst.parent().unwrap_or(Path::new("."))).map(|p| p.join(dst.file_name().unwrap_or_default()))) {
            if d.starts_with(&s) {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("cannot copy '{}' into itself", src.display())));
            }
        }
        match fs::create_dir(dst) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dst.is_dir() => {}
            Err(e) => return Err(e),
        }
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_path(&entry.path(), &dst.join(entry.file_name()), true)?;
        }
        fs::set_permissions(dst, fs::Permissions::from_mode(meta.mode() & 0o7777))
    } else if ft.is_file() {
        fs::copy(src, dst).map(|_| ())
    } else {
        Err(io::Error::new(io::ErrorKind::InvalidInput, format!("'{}': special files are not copied", src.display())))
    }
}

/// Work out where each source goes: into `last` if it is a directory, else exactly `last`.
fn destinations<'a>(prog: &str, sources: &'a [String], last: &str) -> Option<Vec<(&'a String, PathBuf)>> {
    let last_path = Path::new(last);
    let into_dir = last_path.is_dir();
    if sources.len() > 1 && !into_dir {
        error(prog, format!("target '{last}' is not a directory"));
        return None;
    }
    Some(sources.iter().map(|s| (s, if into_dir { target_in_dir(s, last_path) } else { last_path.to_path_buf() })).collect())
}

pub fn cp(args: &[String]) -> i32 {
    let Some(p) = parse_or_report("cp", args, "rRfpaiv", "") else { return 1 };
    if p.operands.len() < 2 {
        error("cp", "usage: cp [-rRfpa] SOURCE... DEST");
        return 1;
    }
    let recursive = p.has('r') || p.has('R') || p.has('a');
    let (sources, last) = p.operands.split_at(p.operands.len() - 1);
    let Some(pairs) = destinations("cp", sources, &last[0]) else { return 1 };
    let mut status = 0;
    for (src, dst) in pairs {
        if let Err(e) = copy_path(Path::new(src), &dst, recursive) {
            error("cp", format!("cannot copy '{src}' to '{}': {e}", dst.display()));
            status = 1;
        }
    }
    status
}

pub fn mv(args: &[String]) -> i32 {
    let Some(p) = parse_or_report("mv", args, "fiv", "") else { return 1 };
    if p.operands.len() < 2 {
        error("mv", "usage: mv SOURCE... DEST");
        return 1;
    }
    let (sources, last) = p.operands.split_at(p.operands.len() - 1);
    let Some(pairs) = destinations("mv", sources, &last[0]) else { return 1 };
    let mut status = 0;
    for (src, dst) in pairs {
        let r = match fs::rename(src, &dst) {
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                // Different filesystems: copy, then remove the original.
                copy_path(Path::new(src), &dst, true).and_then(|()| remove_all(Path::new(src)))
            }
            other => other,
        };
        if let Err(e) = r {
            error("mv", format!("cannot move '{src}' to '{}': {e}", dst.display()));
            status = 1;
        }
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_and_tail_pieces() {
        let d = b"1\n2\n3\n4\n5\n";
        assert_eq!(head_lines(d, 2), b"1\n2\n");
        assert_eq!(head_lines(d, 0), b"");
        assert_eq!(head_lines(d, 99), d);
        assert_eq!(tail_lines(d, 2, false), b"4\n5\n");
        assert_eq!(tail_lines(d, 99, false), d);
        assert_eq!(tail_lines(d, 0, false), b"");
        assert_eq!(tail_lines(d, 4, true), b"4\n5\n");
        assert_eq!(tail_lines(d, 1, true), d);
        assert_eq!(tail_lines(d, 9, true), b"");
        // last line without a trailing newline still counts as a line
        assert_eq!(tail_lines(b"a\nb\nc", 2, false), b"b\nc");
        assert_eq!(head_lines(b"a\nb", 5), b"a\nb");
        assert_eq!(tail_lines(b"", 3, false), b"");
    }

    #[test]
    fn counting() {
        assert_eq!(count(b""), (0, 0, 0));
        assert_eq!(count(b"hello world\nfoo\n"), (2, 3, 16));
        assert_eq!(count(b"  a   b\t\nc"), (1, 3, 10));
        assert_eq!(count(b"nonl"), (0, 1, 4));
    }

    #[test]
    fn octal_modes() {
        assert_eq!(parse_octal("755"), Some(0o755));
        assert_eq!(parse_octal("0644"), Some(0o644));
        assert_eq!(parse_octal("4755"), Some(0o4755));
        assert_eq!(parse_octal("789"), None);
        assert_eq!(parse_octal(""), None);
        assert_eq!(parse_octal("77777"), None);
    }

    #[test]
    fn symbolic_modes() {
        let m = |s: &str, cur: u32| apply_mode(s, cur, false).unwrap();
        assert_eq!(m("755", 0o600), 0o755);
        assert_eq!(m("u+x", 0o644), 0o744);
        assert_eq!(m("go-rwx", 0o755), 0o700);
        assert_eq!(m("a=r", 0o755), 0o444);
        assert_eq!(m("u=rw,g=r,o=", 0o777), 0o640);
        assert_eq!(m("+x", 0o644), 0o755);
        assert_eq!(m("u+s", 0o644), 0o4644);
        assert_eq!(m("g+s", 0o755), 0o2755);
        assert_eq!(m("+t", 0o777), 0o1777);
        assert_eq!(m("u+x-w", 0o644), 0o544);
        assert_eq!(m("o+X", 0o644), 0o644, "X needs an existing x bit on a file");
        assert_eq!(m("o+X", 0o744), 0o745);
        assert_eq!(apply_mode("o+X", 0o700, true), Ok(0o701), "X applies to directories");
        for bad in ["", "u", "u+q", "x+u", "u+r,", "z=r"] {
            assert!(apply_mode(bad, 0o644, false).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn owner_specs() {
        let users = parse_passwd("root:x:0:0::/:/bin/sh\nbob:x:1000:1000::/:/bin/sh\n");
        let groups = parse_group("root:x:0:\nstaff:x:50:\n");
        assert_eq!(parse_owner("bob", &users, &groups), Ok((Some(1000), None)));
        assert_eq!(parse_owner("bob:staff", &users, &groups), Ok((Some(1000), Some(50))));
        assert_eq!(parse_owner(":staff", &users, &groups), Ok((None, Some(50))));
        assert_eq!(parse_owner("12:34", &users, &groups), Ok((Some(12), Some(34))));
        assert_eq!(parse_owner("bob:", &users, &groups), Ok((Some(1000), None)));
        assert!(parse_owner("nobody", &users, &groups).is_err());
        assert!(parse_owner("bob:nogroup", &users, &groups).is_err());
        assert!(parse_owner(":", &users, &groups).is_err());
    }
}
