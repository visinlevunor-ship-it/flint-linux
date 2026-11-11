//! Package control data (`.FLINTPKG`) and repository index entries.
//!
//! Both are text: `key: value` lines. An INDEX is a list of such blocks separated by blank
//! lines, each with extra `file`, `size` and `sha256` keys.

use crate::version::{valid_name, valid_version, Constraint};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Control {
    pub name: String,
    pub version: String,
    pub description: String,
    /// `x86_64` or `any`
    pub arch: String,
    pub depends: Vec<Constraint>,
    pub conflicts: Vec<String>,
    /// Paths (relative to /) of files an upgrade must not overwrite when the user changed them.
    pub config: Vec<String>,
}

pub const THIS_ARCH: &str = "x86_64";

fn split_list(v: &str) -> Vec<&str> {
    v.split(',').map(str::trim).filter(|s| !s.is_empty()).collect()
}

impl Control {
    /// Parse `key: value` lines. Unknown keys are ignored so newer tools can add fields.
    pub fn parse(text: &str) -> Result<Control, String> {
        let mut c = Control { arch: "any".to_string(), ..Control::default() };
        for (n, line) in text.lines().enumerate() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line.split_once(':').ok_or_else(|| format!("control line {}: expected 'key: value'", n + 1))?;
            let v = v.trim();
            match k.trim() {
                "name" => c.name = v.to_string(),
                "version" => c.version = v.to_string(),
                "description" => c.description = v.to_string(),
                "arch" => c.arch = v.to_string(),
                "depends" => {
                    c.depends = split_list(v).into_iter().map(Constraint::parse).collect::<Result<_, _>>()?;
                }
                "conflicts" => {
                    for name in split_list(v) {
                        if !valid_name(name) {
                            return Err(format!("bad package name '{name}' in conflicts"));
                        }
                        c.conflicts.push(name.to_string());
                    }
                }
                "config" => c.config.extend(split_list(v).into_iter().map(|s| s.trim_start_matches('/').to_string())),
                _ => {}
            }
        }
        if !valid_name(&c.name) {
            return Err(format!("control: bad or missing name '{}'", c.name));
        }
        if !valid_version(&c.version) {
            return Err(format!("control: bad or missing version '{}'", c.version));
        }
        if c.arch != "any" && c.arch != THIS_ARCH {
            return Err(format!("control: unsupported arch '{}' (this system is {THIS_ARCH})", c.arch));
        }
        Ok(c)
    }

    pub fn to_text(&self) -> String {
        let mut s = format!("name: {}\nversion: {}\narch: {}\n", self.name, self.version, self.arch);
        if !self.description.is_empty() {
            s.push_str(&format!("description: {}\n", self.description));
        }
        if !self.depends.is_empty() {
            let d: Vec<String> = self.depends.iter().map(ToString::to_string).collect();
            s.push_str(&format!("depends: {}\n", d.join(", ")));
        }
        if !self.conflicts.is_empty() {
            s.push_str(&format!("conflicts: {}\n", self.conflicts.join(", ")));
        }
        if !self.config.is_empty() {
            s.push_str(&format!("config: {}\n", self.config.join(", ")));
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub control: Control,
    pub file: String,
    pub size: u64,
    pub sha256: String,
}

pub fn parse_index(text: &str) -> Result<Vec<IndexEntry>, String> {
    let mut entries = Vec::new();
    for block in text.split("\n\n") {
        if block.trim().is_empty() {
            continue;
        }
        // The header block (timestamp: N) of a signed INDEX describes the index, not a package.
        if block.trim_start().starts_with("timestamp:") {
            continue;
        }
        let control = Control::parse(block)?;
        let get = |key: &str| block.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(':')).map(|v| v.trim().to_string()));
        let file = get("file").ok_or_else(|| format!("index: {} has no 'file'", control.name))?;
        let size = get("size").and_then(|s| s.parse().ok()).ok_or_else(|| format!("index: {} has no valid 'size'", control.name))?;
        let sha256 = get("sha256").filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).ok_or_else(|| format!("index: {} has no valid 'sha256'", control.name))?;
        if file.contains('/') || file.is_empty() || file.starts_with('.') {
            return Err(format!("index: bad file name '{file}'"));
        }
        entries.push(IndexEntry { control, file, size, sha256 });
    }
    Ok(entries)
}

/// The text of an INDEX: a header with the time it was made (pkg refuses an INDEX older
/// than one it already accepted), then one block per package.
pub fn format_index(entries: &[IndexEntry], timestamp: u64) -> String {
    let mut out = format!("timestamp: {timestamp}\n\n");
    for e in entries {
        out.push_str(&e.control.to_text());
        out.push_str(&format!("file: {}\nsize: {}\nsha256: {}\n\n", e.file, e.size, e.sha256));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "name: hello\nversion: 1.2.0\ndescription: Prints a greeting\narch: x86_64\ndepends: libgreet>=1.0, bar\nconflicts: hi\nconfig: /etc/hello.conf, etc/other.conf\n";

    #[test]
    fn parses_and_round_trips() {
        let c = Control::parse(SAMPLE).unwrap();
        assert_eq!(c.name, "hello");
        assert_eq!(c.depends.len(), 2);
        assert_eq!(c.depends[0].to_string(), "libgreet>=1.0");
        assert_eq!(c.conflicts, ["hi"]);
        assert_eq!(c.config, ["etc/hello.conf", "etc/other.conf"], "leading / is dropped");
        assert_eq!(Control::parse(&c.to_text()).unwrap(), c);
    }

    #[test]
    fn minimal_and_unknown_keys() {
        let c = Control::parse("name: a\nversion: 1\nfuture-field: whatever\n").unwrap();
        assert_eq!(c.arch, "any");
        assert!(c.depends.is_empty());
    }

    #[test]
    fn rejects_bad_control() {
        for bad in ["version: 1\n", "name: a\n", "name: A\nversion: 1\n", "name: a\nversion: 1 2\n", "name: a\nversion: 1\narch: arm64\n", "name: a\nversion: 1\ndepends: B\n", "garbage\n", "name: a\nversion: 1\nconflicts: Bad\n"] {
            assert!(Control::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn index_round_trip() {
        let e = IndexEntry { control: Control::parse(SAMPLE).unwrap(), file: "hello-1.2.0.fpk".into(), size: 1234, sha256: "ab".repeat(32) };
        let text = format_index(&[e.clone(), IndexEntry { control: Control::parse("name: z\nversion: 2\n").unwrap(), file: "z-2.fpk".into(), size: 5, sha256: "00".repeat(32) }], 1700000000);
        assert!(text.starts_with("timestamp: 1700000000\n\n"));
        let back = parse_index(&text).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0], e);
        assert_eq!(back[1].control.name, "z");
        assert_eq!(parse_index("").unwrap(), vec![]);
        assert!(parse_index("name: a\nversion: 1\nfile: a.fpk\nsize: 1\nsha256: xyz\n").is_err());
        let h = "00".repeat(32);
        assert!(parse_index(&format!("name: a\nversion: 1\nfile: ../a.fpk\nsize: 1\nsha256: {h}\n")).is_err());
        assert!(parse_index(&format!("name: a\nversion: 1\nfile: .hidden\nsize: 1\nsha256: {h}\n")).is_err());
    }
}
