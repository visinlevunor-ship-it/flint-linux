//! Choosing the kernel modules and firmware that go into the image, and writing them as two
//! initramfs segments (`lib/modules/VERSION/...` and `lib/firmware/...`). Also generates
//! modules.dep / modules.alias / modules.softdep, which kernel packages leave to depmod.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cpio;
use crate::elf;

#[derive(Default, Debug)]
pub struct Config {
    /// Module paths (under `kernel/`) to include, by prefix.
    pub include: Vec<String>,
    /// ... and to leave out, by prefix (unless something included needs them).
    pub skip: Vec<String>,
    /// Firmware files to include besides what modules declare (glob patterns).
    pub firmware: Vec<String>,
}

impl Config {
    pub fn parse(text: &str) -> Result<Config, String> {
        let mut c = Config::default();
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (k, v) = line.split_once(char::is_whitespace).ok_or_else(|| format!("line {}: expected KEY VALUE", n + 1))?;
            let v = v.trim().to_string();
            match k {
                "module" => c.include.push(v),
                "skip" => c.skip.push(v),
                "firmware" => c.firmware.push(v),
                _ => return Err(format!("line {}: unknown key '{k}' (module, skip or firmware)", n + 1)),
            }
        }
        Ok(c)
    }
}

pub fn normalize(name: &str) -> String {
    name.replace('-', "_")
}

/// `kernel/drivers/net/foo-bar.ko.zst` -> `foo_bar`
pub fn module_name(rel: &str) -> String {
    let file = rel.rsplit('/').next().unwrap_or(rel);
    normalize(file.split(".ko").next().unwrap_or(file))
}

const COMPRESSED: [&str; 3] = [".zst", ".xz", ".gz"];

/// Strip a compression suffix: the name the kernel asks for.
pub fn logical(name: &str) -> &str {
    for ext in COMPRESSED {
        if let Some(s) = name.strip_suffix(ext) {
            return s;
        }
    }
    name
}

fn read_maybe_compressed(path: &Path) -> io::Result<Vec<u8>> {
    let s = path.to_string_lossy();
    let tool = if s.ends_with(".zst") {
        Some("zstd")
    } else if s.ends_with(".xz") {
        Some("xz")
    } else if s.ends_with(".gz") {
        Some("gzip")
    } else {
        None
    };
    match tool {
        None => fs::read(path),
        Some(t) => {
            let out = Command::new(t).arg("-dc").arg(path).output().map_err(|e| io::Error::new(e.kind(), format!("running {t} (is it installed?): {e}")))?;
            if !out.status.success() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{t} -dc {}: {}", path.display(), String::from_utf8_lossy(&out.stderr).trim())));
            }
            Ok(out.stdout)
        }
    }
}

/// All files and symlinks under `root`, as paths relative to it.
fn walk(root: &Path) -> io::Result<Vec<(String, bool)>> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        for e in fs::read_dir(root.join(&rel))? {
            let e = e?;
            let r = rel.join(e.file_name());
            let ft = e.file_type()?;
            let rs = r.to_string_lossy().into_owned();
            if ft.is_dir() {
                stack.push(r);
            } else {
                out.push((rs, ft.is_symlink()));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// The `lib/modules/VERSION` directory under an unpacked kernel package.
pub fn find_module_dir(kernel_root: &Path) -> Result<(String, PathBuf), String> {
    for base in ["lib/modules", "usr/lib/modules"] {
        let d = kernel_root.join(base);
        let Ok(rd) = fs::read_dir(&d) else { continue };
        let mut vers: Vec<String> = rd.flatten().filter(|e| e.path().join("kernel").is_dir()).filter_map(|e| e.file_name().into_string().ok()).collect();
        vers.sort();
        match vers.len() {
            0 => continue,
            1 => {
                let v = vers.remove(0);
                return Ok((v.clone(), d.join(v)));
            }
            _ => return Err(format!("{}: more than one kernel version ({}); unpack only one", d.display(), vers.join(", "))),
        }
    }
    Err(format!("{}: no lib/modules/VERSION/kernel directory found", kernel_root.display()))
}

pub struct Selection {
    pub version: String,
    /// module name -> path relative to the module directory
    pub modules: BTreeMap<String, String>,
    pub deps: BTreeMap<String, Vec<String>>,
    pub aliases: Vec<(String, String)>,
    pub softdeps: Vec<(String, String)>,
    pub firmware_wanted: BTreeSet<String>,
}

fn matches_prefix(rel: &str, prefixes: &[String]) -> bool {
    let r = rel.strip_prefix("kernel/").unwrap_or(rel);
    prefixes.iter().any(|p| r.starts_with(p.as_str()))
}

pub fn select_modules(moddir: &Path, version: &str, cfg: &Config) -> Result<Selection, String> {
    let all: BTreeMap<String, String> = walk(&moddir.join("kernel"))
        .map_err(|e| format!("{}: {e}", moddir.display()))?
        .into_iter()
        .filter(|(r, _)| r.contains(".ko"))
        .map(|(r, _)| (module_name(&r), format!("kernel/{r}")))
        .collect();
    let mut queue: VecDeque<String> = all.iter().filter(|(_, rel)| matches_prefix(rel, &cfg.include) && !matches_prefix(rel, &cfg.skip)).map(|(n, _)| n.clone()).collect();
    let mut sel = Selection { version: version.to_string(), modules: BTreeMap::new(), deps: BTreeMap::new(), aliases: Vec::new(), softdeps: Vec::new(), firmware_wanted: BTreeSet::new() };
    while let Some(name) = queue.pop_front() {
        if sel.modules.contains_key(&name) {
            continue;
        }
        let Some(rel) = all.get(&name) else { continue };
        let data = read_maybe_compressed(&moddir.join(rel)).map_err(|e| format!("{rel}: {e}"))?;
        let mut deps = Vec::new();
        for (k, v) in elf::modinfo(&data) {
            match k.as_str() {
                "alias" => sel.aliases.push((v, name.clone())),
                "depends" => deps.extend(v.split(',').filter(|d| !d.is_empty()).map(normalize)),
                "firmware" => {
                    sel.firmware_wanted.insert(v);
                }
                "softdep" => {
                    for w in v.split_whitespace().filter(|w| !w.ends_with(':')) {
                        queue.push_back(normalize(w));
                    }
                    sel.softdeps.push((name.clone(), v));
                }
                _ => {}
            }
        }
        for d in &deps {
            queue.push_back(d.clone());
        }
        sel.deps.insert(name.clone(), deps);
        sel.modules.insert(name, rel.clone());
    }
    Ok(sel)
}

/// All modules `name` needs, directly or not, nearest first.
fn closure(sel: &Selection, name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut queue: VecDeque<String> = sel.deps.get(name).cloned().unwrap_or_default().into();
    while let Some(d) = queue.pop_front() {
        if out.contains(&d) || d == name {
            continue;
        }
        queue.extend(sel.deps.get(&d).cloned().unwrap_or_default());
        out.push(d);
    }
    out
}

fn glob(p: &str, t: &str) -> bool {
    fn m(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') => (0..=t.len()).any(|k| m(&p[1..], &t[k..])),
            Some(b'?') => !t.is_empty() && m(&p[1..], &t[1..]),
            Some(&c) => !t.is_empty() && t[0] == c && m(&p[1..], &t[1..]),
        }
    }
    m(p.as_bytes(), t.as_bytes())
}

/// `dir/name-89.ucode` -> (`dir/name-`, 89, `.ucode`)
fn split_version(name: &str) -> Option<(&str, u64, &str)> {
    let dot = name.rfind('.')?;
    let (stem, ext) = name.split_at(dot);
    let digits = stem.bytes().rev().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 9 {
        return None;
    }
    let (prefix, num) = stem.split_at(stem.len() - digits);
    Some((prefix, num.parse().ok()?, ext))
}

pub struct FirmwareChoice {
    /// Paths (relative to the firmware root) to copy.
    pub files: BTreeSet<String>,
    /// Declared names nothing was found for.
    pub missing: Vec<String>,
}

/// Pick firmware: everything declared (newest available version not above the declared one,
/// since drivers fall back to older firmware) plus the extra patterns; then whatever the
/// chosen symlinks point at.
pub fn select_firmware(fw_root: &Path, wanted: &BTreeSet<String>, extra: &[String]) -> Result<FirmwareChoice, String> {
    let tree = walk(fw_root).map_err(|e| format!("{}: {e}", fw_root.display()))?;
    let by_logical: BTreeMap<&str, &str> = tree.iter().map(|(r, _)| (logical(r), r.as_str())).collect();
    let mut chosen: BTreeSet<String> = BTreeSet::new();
    let mut missing = Vec::new();

    for w in wanted {
        if w.contains(['*', '?']) {
            let hits: Vec<&str> = by_logical.iter().filter(|(l, _)| glob(w, l)).map(|(_, r)| *r).collect();
            if hits.is_empty() {
                missing.push(w.clone());
            }
            chosen.extend(hits.into_iter().map(String::from));
            continue;
        }
        if let Some(r) = by_logical.get(w.as_str()) {
            chosen.insert((*r).to_string());
            continue;
        }
        let fallback = split_version(w).and_then(|(prefix, max, ext)| {
            by_logical
                .range(prefix..)
                .take_while(|(l, _)| l.starts_with(prefix))
                .filter_map(|(l, r)| {
                    let mid = l.strip_prefix(prefix)?.strip_suffix(ext)?;
                    let n: u64 = mid.parse().ok()?;
                    (n <= max && mid.bytes().all(|b| b.is_ascii_digit())).then_some((n, *r))
                })
                .max()
                .map(|(_, r)| r.to_string())
        });
        match fallback {
            Some(r) => {
                chosen.insert(r);
            }
            None => missing.push(w.clone()),
        }
    }
    for pat in extra {
        chosen.extend(by_logical.iter().filter(|(l, _)| glob(pat, l)).map(|(_, r)| r.to_string()));
    }

    // Follow symlinks so their targets come along.
    let links: BTreeSet<&str> = tree.iter().filter(|(_, l)| *l).map(|(r, _)| r.as_str()).collect();
    let mut queue: Vec<String> = chosen.iter().cloned().collect();
    while let Some(r) = queue.pop() {
        if !links.contains(r.as_str()) {
            continue;
        }
        let target = fs::read_link(fw_root.join(&r)).map_err(|e| format!("{r}: {e}"))?;
        let base = Path::new(&r).parent().unwrap_or(Path::new(""));
        let mut parts: Vec<String> = Vec::new();
        for c in base.join(&target).components() {
            match c {
                std::path::Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
                std::path::Component::ParentDir => {
                    parts.pop();
                }
                _ => {}
            }
        }
        let t = parts.join("/");
        if tree.iter().any(|(x, _)| *x == t) && chosen.insert(t.clone()) {
            queue.push(t);
        }
    }
    missing.sort();
    missing.dedup();
    Ok(FirmwareChoice { files: chosen, missing })
}

/// Directory entries for `prefix`, its parents, and every parent of `paths` below it.
fn parents(prefix: &str, paths: impl Iterator<Item = String>) -> BTreeSet<String> {
    let mut dirs = BTreeSet::new();
    let mut add = |full: &str| {
        let mut cur = Some(Path::new(full));
        while let Some(d) = cur {
            let s = d.to_string_lossy();
            if !s.is_empty() {
                dirs.insert(s.into_owned());
            }
            cur = d.parent();
        }
    };
    add(prefix);
    for p in paths {
        if let Some(parent) = Path::new(&format!("{prefix}/{p}")).parent() {
            add(&parent.to_string_lossy());
        }
    }
    dirs
}

pub fn write_modules_img(moddir: &Path, sel: &Selection, out: &Path) -> Result<u64, String> {
    let prefix = format!("lib/modules/{}", sel.version);
    let f = fs::File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut w = cpio::Writer::new(BufWriter::new(f));
    let err = |e: io::Error| e.to_string();
    for d in parents(&prefix, sel.modules.values().cloned()) {
        w.dir(&d, 0o755).map_err(err)?;
    }
    let mut total = 0u64;
    for rel in sel.modules.values() {
        let data = fs::read(moddir.join(rel)).map_err(|e| format!("{rel}: {e}"))?;
        total += data.len() as u64;
        w.file(&format!("{prefix}/{rel}"), 0o644, &data).map_err(err)?;
    }
    let mut dep = String::new();
    for (name, rel) in &sel.modules {
        let deps: Vec<String> = closure(sel, name).iter().filter_map(|d| sel.modules.get(d).cloned()).collect();
        dep.push_str(&format!("{rel}: {}\n", deps.join(" ")));
    }
    let mut alias = String::from("# Aliases extracted from modules themselves.\n");
    for (a, m) in &sel.aliases {
        alias.push_str(&format!("alias {a} {m}\n"));
    }
    let mut soft = String::from("# Soft dependencies extracted from modules themselves.\n");
    for (m, s) in &sel.softdeps {
        soft.push_str(&format!("softdep {m} {s}\n"));
    }
    w.file(&format!("{prefix}/modules.dep"), 0o644, dep.as_bytes()).map_err(err)?;
    w.file(&format!("{prefix}/modules.alias"), 0o644, alias.as_bytes()).map_err(err)?;
    w.file(&format!("{prefix}/modules.softdep"), 0o644, soft.as_bytes()).map_err(err)?;
    for extra in ["modules.builtin", "modules.builtin.modinfo", "modules.order"] {
        if let Ok(data) = fs::read(moddir.join(extra)) {
            w.file(&format!("{prefix}/{extra}"), 0o644, &data).map_err(err)?;
        }
    }
    w.finish().map_err(err)?;
    Ok(total)
}

pub fn write_firmware_img(fw_root: &Path, files: &BTreeSet<String>, out: &Path) -> Result<u64, String> {
    let prefix = "lib/firmware";
    let f = fs::File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut w = cpio::Writer::new(BufWriter::new(f));
    let err = |e: io::Error| e.to_string();
    for d in parents(prefix, files.iter().cloned()) {
        w.dir(&d, 0o755).map_err(err)?;
    }
    let mut total = 0u64;
    for rel in files {
        let p = fw_root.join(rel);
        let meta = fs::symlink_metadata(&p).map_err(|e| format!("{rel}: {e}"))?;
        if meta.file_type().is_symlink() {
            let t = fs::read_link(&p).map_err(|e| format!("{rel}: {e}"))?;
            w.symlink(&format!("{prefix}/{rel}"), &t.to_string_lossy()).map_err(err)?;
        } else {
            let data = fs::read(&p).map_err(|e| format!("{rel}: {e}"))?;
            total += data.len() as u64;
            w.file(&format!("{prefix}/{rel}"), 0o644, &data).map_err(err)?;
        }
    }
    w.finish().map_err(err)?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn config_file() {
        let c = Config::parse("# x\nmodule drivers/net/\nskip drivers/net/can/  # no CAN\nfirmware brcm/brcmfmac*\n").unwrap();
        assert_eq!(c.include, ["drivers/net/"]);
        assert_eq!(c.skip, ["drivers/net/can/"]);
        assert_eq!(c.firmware, ["brcm/brcmfmac*"]);
        assert!(Config::parse("bogus x\n").is_err());
        assert!(Config::parse("module\n").is_err());
    }

    #[test]
    fn names_and_versions() {
        assert_eq!(module_name("kernel/drivers/hid/hid-generic.ko.zst"), "hid_generic");
        assert_eq!(logical("iwlwifi-cc-a0-77.ucode.zst"), "iwlwifi-cc-a0-77.ucode");
        assert_eq!(logical("regulatory.db"), "regulatory.db");
        assert_eq!(split_version("iwlwifi-cc-a0-89.ucode"), Some(("iwlwifi-cc-a0-", 89, ".ucode")));
        assert_eq!(split_version("ath10k/QCA6174/hw3.0/firmware-6.bin"), Some(("ath10k/QCA6174/hw3.0/firmware-", 6, ".bin")));
        assert_eq!(split_version("rtl_nic/rtl8168h-2.fw"), Some(("rtl_nic/rtl8168h-", 2, ".fw")));
        assert_eq!(split_version("noversion.bin"), None);
        assert!(glob("ath11k/QCA6390/hw2.0/*", "ath11k/QCA6390/hw2.0/amss.bin"));
        assert!(!glob("ath11k/QCA6390/hw2.0/*", "ath11k/WCN6855/hw2.0/amss.bin"));
    }

    #[test]
    fn firmware_selection_with_fallback_globs_and_symlinks() {
        let d = std::env::temp_dir().join(format!("mkimage-fw-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("intel/iwlwifi")).unwrap();
        fs::create_dir_all(d.join("ath11k/QCA6390/hw2.0")).unwrap();
        fs::create_dir_all(d.join("brcm")).unwrap();
        for f in ["intel/iwlwifi/iwlwifi-cc-a0-72.ucode.zst", "intel/iwlwifi/iwlwifi-cc-a0-77.ucode.zst", "intel/iwlwifi/iwlwifi-cc-a0-99.ucode.zst", "ath11k/QCA6390/hw2.0/amss.bin.zst", "ath11k/QCA6390/hw2.0/m3.bin.zst", "brcm/brcmfmac4356-pcie.bin.zst", "brcm/brcmfmac4356-pcie.Lenovo.txt.zst", "other.bin.zst", "regulatory.db"] {
            fs::write(d.join(f), f).unwrap();
        }
        for v in ["72", "77", "99"] {
            symlink(format!("intel/iwlwifi/iwlwifi-cc-a0-{v}.ucode.zst"), d.join(format!("iwlwifi-cc-a0-{v}.ucode.zst"))).unwrap();
        }
        let wanted: BTreeSet<String> = ["iwlwifi-cc-a0-89.ucode", "ath11k/QCA6390/hw2.0/*", "nothere-1.bin", "brcm/brcmfmac4356-pcie.bin"].iter().map(|s| s.to_string()).collect();
        let c = select_firmware(&d, &wanted, &["brcm/brcmfmac*".to_string(), "regulatory.db".to_string()]).unwrap();
        let files: Vec<&str> = c.files.iter().map(String::as_str).collect();
        assert_eq!(
            files,
            [
                "ath11k/QCA6390/hw2.0/amss.bin.zst",
                "ath11k/QCA6390/hw2.0/m3.bin.zst",
                "brcm/brcmfmac4356-pcie.Lenovo.txt.zst",
                "brcm/brcmfmac4356-pcie.bin.zst",
                "intel/iwlwifi/iwlwifi-cc-a0-77.ucode.zst",
                "iwlwifi-cc-a0-77.ucode.zst",
                "regulatory.db",
            ],
            "newest version not above the declared one, plus the symlink's target"
        );
        assert_eq!(c.missing, ["nothere-1.bin"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn parent_directories() {
        let p = parents("lib/firmware", ["a/b/c.bin".to_string(), "x.bin".to_string()].into_iter());
        assert_eq!(p.into_iter().collect::<Vec<_>>(), ["lib", "lib/firmware", "lib/firmware/a", "lib/firmware/a/b"]);
    }
}
