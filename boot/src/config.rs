//! `\FLINT\BOOT.CFG`: the boot menu. Plain text, one setting per line:
//!
//! ```text
//! timeout 5                      seconds before the default entry boots (0 = at once)
//! default 1                      which entry is selected first (counting from 1)
//! entry Flint Linux              starts a menu entry; the rest of the line is its title
//!   kernel \FLINT\KERNEL         the Linux kernel (an EFI-stub bzImage)
//!   initrd \FLINT\BASE.IMG       initramfs images, joined in this order (up to 8)
//!   options quiet                the kernel command line
//! ```
//!
//! `#` starts a comment. Unknown or malformed lines are reported and skipped, so a typo
//! never leaves the machine unbootable: if no usable entry remains, the built-in default is
//! used.

pub const MAX_ENTRIES: usize = 8;
pub const MAX_INITRDS: usize = 8;
pub const MAX_PROBLEMS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub title: &'a str,
    pub kernel: &'a str,
    pub initrds: [&'a str; MAX_INITRDS],
    pub n_initrds: usize,
    pub options: &'a str,
}

impl<'a> Entry<'a> {
    pub const fn empty(title: &'a str) -> Entry<'a> {
        Entry { title, kernel: "", initrds: [""; MAX_INITRDS], n_initrds: 0, options: "" }
    }

    pub fn initrds(&self) -> &[&'a str] {
        &self.initrds[..self.n_initrds]
    }
}

pub const DEFAULT_ENTRY: Entry<'static> = Entry {
    title: "Flint Linux",
    kernel: "\\FLINT\\KERNEL",
    initrds: ["\\FLINT\\BASE.IMG", "\\FLINT\\MODULES.IMG", "\\FLINT\\FIRMWARE.IMG", "", "", "", "", ""],
    n_initrds: 3,
    options: "quiet",
};

#[derive(Debug)]
pub struct Config<'a> {
    pub timeout: u32,
    pub default: usize,
    pub entries: [Entry<'a>; MAX_ENTRIES],
    pub n_entries: usize,
    /// (line number, what is wrong)
    pub problems: [(usize, &'static str); MAX_PROBLEMS],
    pub n_problems: usize,
}

impl<'a> Config<'a> {
    pub fn entries(&self) -> &[Entry<'a>] {
        &self.entries[..self.n_entries]
    }

    pub fn problems(&self) -> &[(usize, &'static str)] {
        &self.problems[..self.n_problems]
    }

    fn problem(&mut self, line: usize, what: &'static str) {
        if self.n_problems < MAX_PROBLEMS {
            self.problems[self.n_problems] = (line, what);
            self.n_problems += 1;
        }
    }

    pub fn builtin() -> Config<'static> {
        let mut c = Config { timeout: 3, default: 0, entries: [Entry::empty(""); MAX_ENTRIES], n_entries: 1, problems: [(0, ""); MAX_PROBLEMS], n_problems: 0 };
        c.entries[0] = DEFAULT_ENTRY;
        c
    }

    pub fn parse(text: &'a str) -> Config<'a> {
        let mut c = Config { timeout: 3, default: 0, entries: [Entry::empty(""); MAX_ENTRIES], n_entries: 0, problems: [(0, ""); MAX_PROBLEMS], n_problems: 0 };
        let mut current: Option<usize> = None;
        let mut too_many = false;
        for (i, raw) in text.lines().enumerate() {
            let n = i + 1;
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = match line.find(|ch: char| ch.is_ascii_whitespace()) {
                Some(p) => (&line[..p], line[p..].trim()),
                None => (line, ""),
            };
            match key {
                "timeout" => match value.parse::<u32>() {
                    Ok(t) if t <= 600 => c.timeout = t,
                    _ => c.problem(n, "timeout must be a number of seconds (0 to 600)"),
                },
                "default" => match value.parse::<usize>() {
                    Ok(d) if d >= 1 => c.default = d - 1,
                    _ => c.problem(n, "default must be an entry number, counting from 1"),
                },
                "entry" => {
                    if c.n_entries == MAX_ENTRIES {
                        if !too_many {
                            c.problem(n, "too many entries (at most 8)");
                            too_many = true;
                        }
                        current = None;
                        continue;
                    }
                    c.entries[c.n_entries] = Entry::empty(if value.is_empty() { "Linux" } else { value });
                    current = Some(c.n_entries);
                    c.n_entries += 1;
                }
                "kernel" | "initrd" | "options" => {
                    let Some(e) = current else {
                        if !too_many {
                            c.problem(n, "kernel, initrd and options belong after an entry line");
                        }
                        continue;
                    };
                    let entry = &mut c.entries[e];
                    match key {
                        "kernel" if !value.is_empty() => entry.kernel = value,
                        "initrd" if !value.is_empty() => {
                            if entry.n_initrds < MAX_INITRDS {
                                entry.initrds[entry.n_initrds] = value;
                                entry.n_initrds += 1;
                            } else {
                                c.problem(n, "too many initrd lines in one entry (at most 8)");
                            }
                        }
                        "options" => entry.options = value,
                        _ => c.problem(n, "missing file name"),
                    }
                }
                _ => c.problem(n, "unknown setting (timeout, default, entry, kernel, initrd, options)"),
            }
        }
        // An entry without a kernel cannot boot: drop it.
        let mut kept = 0;
        for i in 0..c.n_entries {
            if c.entries[i].kernel.is_empty() {
                c.problem(0, "an entry has no kernel line and was left out");
            } else {
                c.entries[kept] = c.entries[i];
                kept += 1;
            }
        }
        c.n_entries = kept;
        if c.n_entries == 0 {
            c.problem(0, "no usable entries; using the built-in one");
            c.entries[0] = DEFAULT_ENTRY;
            c.n_entries = 1;
        }
        if c.default >= c.n_entries {
            c.default = 0;
        }
        c
    }
}

/// Text after `PARTUUID` substitution goes on the kernel command line; this appends the
/// boot partition so Flint can find its own USB stick or disk again.
pub fn esp_option(guid: &[u8; 16], out: &mut [u8; 36]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    // Mixed-endian GUID text: the first three fields are little-endian on disk.
    let order = [3, 2, 1, 0, 255, 5, 4, 255, 7, 6, 255, 8, 9, 255, 10, 11, 12, 13, 14, 15];
    let mut o = 0;
    for &i in &order {
        if i == 255 {
            out[o] = b'-';
            o += 1;
        } else {
            out[o] = HEX[(guid[i] >> 4) as usize];
            out[o + 1] = HEX[(guid[i] & 15) as usize];
            o += 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_config() {
        let c = Config::parse(
            "# comment\ntimeout 7\ndefault 2\n\nentry Flint Linux\n  kernel \\FLINT\\KERNEL\n  initrd \\FLINT\\BASE.IMG\n  initrd \\FLINT\\MODULES.IMG  # drivers\n  options quiet loglevel=3\nentry Verbose\n kernel \\FLINT\\KERNEL\n options\n",
        );
        assert_eq!(c.timeout, 7);
        assert_eq!(c.default, 1);
        assert_eq!(c.entries().len(), 2);
        let e = &c.entries()[0];
        assert_eq!(e.title, "Flint Linux");
        assert_eq!(e.kernel, "\\FLINT\\KERNEL");
        assert_eq!(e.initrds(), ["\\FLINT\\BASE.IMG", "\\FLINT\\MODULES.IMG"]);
        assert_eq!(e.options, "quiet loglevel=3");
        assert_eq!(c.entries()[1].options, "");
        assert!(c.problems().is_empty());
    }

    #[test]
    fn mistakes_never_leave_nothing_to_boot() {
        let c = Config::parse("timeout soon\nbogus line\nkernel \\X\nentry Broken\n");
        assert_eq!(c.timeout, 3);
        assert_eq!(c.entries(), [DEFAULT_ENTRY]);
        assert!(c.problems().len() >= 3);
        assert_eq!(c.problems()[0].0, 1);

        let c = Config::parse("default 9\nentry A\nkernel \\K\n");
        assert_eq!(c.default, 0);

        let empty = Config::parse("");
        assert_eq!(empty.entries(), [DEFAULT_ENTRY]);
    }

    #[test]
    fn limits() {
        let mut s = String::new();
        for i in 0..10 {
            s.push_str(&format!("entry E{i}\nkernel \\K\n"));
        }
        for _ in 0..9 {
            s.push_str("initrd \\I\n");
        }
        let c = Config::parse(&s);
        assert_eq!(c.entries().len(), MAX_ENTRIES);
        assert_eq!(c.entries()[7].title, "E7");
        assert_eq!(c.entries()[7].kernel, "\\K", "lines after the 8th entry do not change the 8th");
        assert_eq!(c.entries()[7].n_initrds, 0);
    }

    #[test]
    fn guid_text() {
        // C12A7328-F81F-11D2-BA4B-00A0C93EC93B as stored on disk
        let g = [0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B];
        let mut out = [0u8; 36];
        esp_option(&g, &mut out);
        assert_eq!(core::str::from_utf8(&out).unwrap(), "c12a7328-f81f-11d2-ba4b-00a0c93ec93b");
    }
}
