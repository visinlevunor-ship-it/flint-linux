//! The installed-package database: one text file per package under
//! `<root>/var/lib/flint-pkg/installed/`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::control::Control;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRec {
    /// `f` file, `d` directory, `l` symlink
    pub kind: char,
    pub mode: u32,
    /// SHA-256 for files, the target for symlinks, empty for directories
    pub data: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub control: Control,
    pub files: Vec<FileRec>,
    pub installed_at: u64,
    /// Installed because the user asked for it (not only as someone's dependency).
    pub explicit: bool,
}

pub struct Db {
    dir: PathBuf,
}

impl Installed {
    pub fn to_text(&self) -> String {
        let mut s = self.control.to_text();
        s.push_str(&format!("installed: {}\nreason: {}\nfiles:\n", self.installed_at, if self.explicit { "explicit" } else { "dependency" }));
        for f in &self.files {
            s.push_str(&format!("{}\t{:o}\t{}\t{}\n", f.kind, f.mode, f.data, f.path));
        }
        s
    }

    pub fn parse(text: &str) -> Result<Installed, String> {
        let (head, files_text) = text.split_once("\nfiles:\n").ok_or("database record has no 'files:' section")?;
        let control = Control::parse(head)?;
        let get = |key: &str| head.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(':')).map(|v| v.trim().to_string()));
        let installed_at = get("installed").and_then(|v| v.parse().ok()).unwrap_or(0);
        let explicit = get("reason").as_deref() != Some("dependency");
        let mut files = Vec::new();
        for line in files_text.lines().filter(|l| !l.is_empty()) {
            let f: Vec<&str> = line.splitn(4, '\t').collect();
            if f.len() != 4 {
                return Err(format!("bad database line: {line:?}"));
            }
            let kind = f[0].chars().next().filter(|c| matches!(c, 'f' | 'd' | 'l')).ok_or_else(|| format!("bad file kind in {line:?}"))?;
            let mode = u32::from_str_radix(f[1], 8).map_err(|_| format!("bad mode in {line:?}"))?;
            files.push(FileRec { kind, mode, data: f[2].to_string(), path: f[3].to_string() });
        }
        Ok(Installed { control, files, installed_at, explicit })
    }
}

impl Db {
    pub fn open(root: &Path) -> Db {
        Db { dir: root.join("var/lib/flint-pkg/installed") }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn load(&self, name: &str) -> Result<Option<Installed>, String> {
        if !crate::version::valid_name(name) {
            return Ok(None);
        }
        match fs::read_to_string(self.file(name)) {
            Ok(t) => Installed::parse(&t).map(Some).map_err(|e| format!("package database entry {name}: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("reading package database: {e}")),
        }
    }

    pub fn all(&self) -> Result<Vec<Installed>, String> {
        let rd = match fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("reading package database: {e}")),
        };
        let mut names: Vec<String> = rd.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| !n.starts_with('.')).collect();
        names.sort();
        let mut out = Vec::new();
        for n in names {
            if let Some(i) = self.load(&n)? {
                out.push(i);
            }
        }
        Ok(out)
    }

    /// Write atomically: temp file, then rename.
    pub fn save(&self, rec: &Installed) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("creating {}: {e}", self.dir.display()))?;
        let tmp = self.dir.join(format!(".{}.tmp", rec.control.name));
        let mut f = fs::File::create(&tmp).map_err(|e| format!("writing package database: {e}"))?;
        f.write_all(rec.to_text().as_bytes()).and_then(|()| f.sync_all()).map_err(|e| format!("writing package database: {e}"))?;
        fs::rename(&tmp, self.file(&rec.control.name)).map_err(|e| format!("writing package database: {e}"))
    }

    pub fn delete(&self, name: &str) -> Result<(), String> {
        match fs::remove_file(self.file(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("removing package database entry: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Installed {
        Installed {
            control: Control::parse("name: hello\nversion: 1.0\ndepends: lib>=1\nconfig: etc/hello.conf\n").unwrap(),
            files: vec![
                FileRec { kind: 'd', mode: 0o755, data: String::new(), path: "usr/bin".into() },
                FileRec { kind: 'f', mode: 0o4755, data: "ab".repeat(32), path: "usr/bin/hello".into() },
                FileRec { kind: 'l', mode: 0o777, data: "hello".into(), path: "usr/bin/hi there".into() },
            ],
            installed_at: 1234,
            explicit: false,
        }
    }

    #[test]
    fn text_round_trip() {
        let r = sample();
        assert_eq!(Installed::parse(&r.to_text()).unwrap(), r);
        let explicit = Installed { explicit: true, ..sample() };
        assert!(Installed::parse(&explicit.to_text()).unwrap().explicit);
    }

    #[test]
    fn rejects_damaged_records() {
        assert!(Installed::parse("name: a\nversion: 1\n").is_err());
        assert!(Installed::parse("name: a\nversion: 1\nfiles:\nf\t644\n").is_err());
        assert!(Installed::parse("name: a\nversion: 1\nfiles:\nx\t644\t\tp\n").is_err());
        assert!(Installed::parse("name: a\nversion: 1\nfiles:\nf\tzz\t\tp\n").is_err());
    }

    #[test]
    fn save_load_list_delete() {
        let root = std::env::temp_dir().join(format!("flint-pkg-db-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let db = Db::open(&root);
        assert!(db.all().unwrap().is_empty());
        assert!(db.load("hello").unwrap().is_none());
        assert!(db.load("../etc").unwrap().is_none(), "names are validated before touching the disk");
        db.save(&sample()).unwrap();
        let other = Installed { control: Control::parse("name: aaa\nversion: 2\n").unwrap(), files: vec![], installed_at: 1, explicit: true };
        db.save(&other).unwrap();
        assert_eq!(db.load("hello").unwrap().unwrap(), sample());
        let names: Vec<String> = db.all().unwrap().iter().map(|i| i.control.name.clone()).collect();
        assert_eq!(names, ["aaa", "hello"]);
        db.delete("hello").unwrap();
        db.delete("hello").unwrap();
        assert!(db.load("hello").unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
    }
}
