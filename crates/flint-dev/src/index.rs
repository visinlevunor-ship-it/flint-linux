//! The kernel's module index files in /lib/modules/VERSION: modules.dep, modules.alias,
//! modules.softdep and modules.builtin.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::pattern;

/// `foo-bar` and `foo_bar` are the same module.
pub fn normalize(name: &str) -> String {
    name.replace('-', "_")
}

/// Module name from a path in modules.dep: `kernel/net/foo-bar.ko.zst` -> `foo_bar`.
pub fn name_from_path(p: &str) -> String {
    let file = p.rsplit('/').next().unwrap_or(p);
    let stem = file.split(".ko").next().unwrap_or(file);
    normalize(stem)
}

#[derive(Default, Debug)]
pub struct Index {
    pub dir: PathBuf,
    /// module -> path relative to `dir`
    pub path: HashMap<String, String>,
    /// module -> dependencies (all of them, as listed)
    pub deps: HashMap<String, Vec<String>>,
    /// (pattern, module), grouped by the pattern's literal prefix length for quick filtering
    pub aliases: Vec<(String, String)>,
    pub softdep_pre: HashMap<String, Vec<String>>,
    pub softdep_post: HashMap<String, Vec<String>>,
    pub builtin: HashSet<String>,
}

pub fn parse_dep(text: &str) -> (HashMap<String, String>, HashMap<String, Vec<String>>) {
    let mut path = HashMap::new();
    let mut deps = HashMap::new();
    for line in text.lines() {
        let Some((m, rest)) = line.split_once(':') else { continue };
        let name = name_from_path(m.trim());
        path.insert(name.clone(), m.trim().to_string());
        deps.insert(name, rest.split_whitespace().map(name_from_path).collect());
    }
    (path, deps)
}

pub fn parse_alias(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            (f.next()? == "alias").then_some(())?;
            Some((f.next()?.to_string(), normalize(f.next()?)))
        })
        .collect()
}

pub fn parse_softdep(text: &str) -> (HashMap<String, Vec<String>>, HashMap<String, Vec<String>>) {
    let (mut pre, mut post) = (HashMap::new(), HashMap::new());
    for line in text.lines() {
        let mut f = line.split_whitespace();
        if f.next() != Some("softdep") {
            continue;
        }
        let Some(m) = f.next() else { continue };
        let m = normalize(m);
        let mut cur: Option<&mut HashMap<String, Vec<String>>> = None;
        for w in f {
            match w {
                "pre:" => cur = Some(&mut pre),
                "post:" => cur = Some(&mut post),
                other => {
                    if let Some(map) = cur.as_deref_mut() {
                        map.entry(m.clone()).or_default().push(normalize(other));
                    }
                }
            }
        }
    }
    (pre, post)
}

impl Index {
    pub fn load(dir: &Path) -> io::Result<Index> {
        let read = |f: &str| fs::read_to_string(dir.join(f));
        let (path, deps) = parse_dep(&read("modules.dep")?);
        let aliases = parse_alias(&read("modules.alias").unwrap_or_default());
        let (softdep_pre, softdep_post) = parse_softdep(&read("modules.softdep").unwrap_or_default());
        let builtin = read("modules.builtin").unwrap_or_default().lines().map(name_from_path).collect();
        Ok(Index { dir: dir.to_path_buf(), path, deps, aliases, softdep_pre, softdep_post, builtin })
    }

    /// The default index for the running kernel.
    pub fn running() -> io::Result<Index> {
        let mut u: libc::utsname = unsafe { std::mem::zeroed() };
        // SAFETY: uname fills a zeroed struct.
        if unsafe { libc::uname(&mut u) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the kernel NUL-terminates the field.
        let release = unsafe { std::ffi::CStr::from_ptr(u.release.as_ptr()) }.to_string_lossy().into_owned();
        Index::load(&Path::new("/lib/modules").join(release))
    }

    /// Modules whose alias pattern matches `modalias`, in index order, without duplicates.
    pub fn resolve(&self, modalias: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (pat, m) in &self.aliases {
            if modalias.starts_with(pattern::literal_prefix(pat)) && pattern::matches(pat, modalias) && !out.contains(m) {
                out.push(m.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(name_from_path("kernel/drivers/net/wireless/intel/iwlwifi/iwlwifi.ko.zst"), "iwlwifi");
        assert_eq!(name_from_path("kernel/drivers/hid/hid-generic.ko"), "hid_generic");
        assert_eq!(name_from_path("snd-hda-intel.ko.xz"), "snd_hda_intel");
        assert_eq!(normalize("i2c-hid-acpi"), "i2c_hid_acpi");
    }

    #[test]
    fn dep_alias_softdep() {
        let (path, deps) = parse_dep("kernel/a/iwlmvm.ko.zst: kernel/a/iwlwifi.ko.zst kernel/n/mac80211.ko.zst kernel/n/cfg80211.ko.zst\nkernel/n/cfg80211.ko.zst:\n");
        assert_eq!(path["iwlmvm"], "kernel/a/iwlmvm.ko.zst");
        assert_eq!(deps["iwlmvm"], ["iwlwifi", "mac80211", "cfg80211"]);
        assert!(deps["cfg80211"].is_empty());

        let a = parse_alias("# Aliases\nalias pci:v00008086d00002723sv*sd*bc*sc*i* iwlwifi\nalias usb:v0BDAp8153d*dc*dsc*dp*ic*isc*ip*in* r8152\nalias fs-vfat vfat\n");
        assert_eq!(a.len(), 3);
        assert_eq!(a[1], ("usb:v0BDAp8153d*dc*dsc*dp*ic*isc*ip*in*".into(), "r8152".into()));

        let (pre, post) = parse_softdep("softdep iwlwifi post: iwlmvm iwldvm\nsoftdep ext4 pre: crc32c\n");
        assert_eq!(post["iwlwifi"], ["iwlmvm", "iwldvm"]);
        assert_eq!(pre["ext4"], ["crc32c"]);
    }

    #[test]
    fn resolve_by_alias() {
        let idx = Index {
            aliases: parse_alias("alias pci:v00008086d00002723sv*sd*bc*sc*i* iwlwifi\nalias pci:v*d*sv*sd*bc0Csc03i30* xhci_pci\nalias pci:v00008086d00002723sv*sd*bc*sc*i* iwlwifi\n"),
            ..Index::default()
        };
        assert_eq!(idx.resolve("pci:v00008086d00002723sv00008086sd00000084bc02sc80i00"), ["iwlwifi"]);
        assert_eq!(idx.resolve("pci:v00001022d000015D4sv000017AAsd00003814bc0Csc03i30"), ["xhci_pci"]);
        assert!(idx.resolve("pci:v0000FFFFd0000FFFFsv0sd0bc00sc00i00").is_empty());
    }
}
