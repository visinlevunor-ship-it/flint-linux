//! Parsing of /etc/flint/services and /etc/flint/boot.
//!
//! Both are line based: `#` starts a comment, blank lines are ignored, fields
//! are split on whitespace (no quoting, no shell).

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Respawn,
    Once,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub mode: Mode,
    pub tty: Option<String>,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub line: usize,
    pub msg: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.msg)
    }
}

impl std::error::Error for ConfigError {}

fn err(line: usize, msg: impl Into<String>) -> ConfigError {
    ConfigError { line, msg: msg.into() }
}

/// (1-based line number, whitespace-separated fields) for every non-empty line.
fn fields(text: &str) -> Vec<(usize, Vec<&str>)> {
    text.lines()
        .enumerate()
        .filter_map(|(i, raw)| {
            let content = raw.split('#').next().unwrap_or("");
            let f: Vec<&str> = content.split_whitespace().collect();
            (!f.is_empty()).then_some((i + 1, f))
        })
        .collect()
}

pub fn parse_services(text: &str) -> Result<Vec<Service>, ConfigError> {
    let mut services: Vec<Service> = Vec::new();
    for (n, f) in fields(text) {
        if f.len() < 4 {
            return Err(err(n, "expected: name mode tty command [args...]"));
        }
        let name = f[0];
        if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err(err(n, format!("invalid service name '{name}' (use letters, digits, - and _)")));
        }
        if services.iter().any(|s| s.name == name) {
            return Err(err(n, format!("duplicate service name '{name}'")));
        }
        let mode = match f[1] {
            "respawn" => Mode::Respawn,
            "once" => Mode::Once,
            other => return Err(err(n, format!("unknown mode '{other}' (use respawn or once)"))),
        };
        let tty = match f[2] {
            "-" => None,
            t if t.starts_with("/dev/") => Some(t.to_string()),
            other => return Err(err(n, format!("tty must be '-' or a /dev path, got '{other}'"))),
        };
        services.push(Service {
            name: name.to_string(),
            mode,
            tty,
            argv: f[3..].iter().map(|s| s.to_string()).collect(),
        });
    }
    Ok(services)
}

pub fn parse_tasks(text: &str) -> Result<Vec<Vec<String>>, ConfigError> {
    Ok(fields(text)
        .into_iter()
        .map(|(_, f)| f.into_iter().map(String::from).collect())
        .collect())
}

fn read_optional(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// `Ok(None)` means the file does not exist.
pub fn load_services(dir: &Path) -> Result<Option<Vec<Service>>, String> {
    let path = dir.join("services");
    read_optional(&path)?
        .map(|t| parse_services(&t).map_err(|e| format!("{}: {e}", path.display())))
        .transpose()
}

/// `Ok(None)` means the file does not exist.
pub fn load_tasks(dir: &Path) -> Result<Option<Vec<Vec<String>>>, String> {
    let path = dir.join("boot");
    read_optional(&path)?
        .map(|t| parse_tasks(&t).map_err(|e| format!("{}: {e}", path.display())))
        .transpose()
}

/// Used when /etc/flint/services is missing or broken, so the system stays usable.
pub fn default_services() -> Vec<Service> {
    let program = ["/sbin/flint-login", "/bin/flint-sh", "/bin/sh"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .unwrap_or("/bin/sh");
    vec![Service {
        name: "console".to_string(),
        mode: Mode::Respawn,
        tty: Some("/dev/tty1".to_string()),
        argv: vec![program.to_string()],
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_services_with_comments_and_blanks() {
        let text = "\n# comment\ntty1 respawn /dev/tty1 /sbin/flint-login   # trailing\n\nlog once - /bin/logger -t boot hello\n";
        let s = parse_services(text).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "tty1");
        assert_eq!(s[0].mode, Mode::Respawn);
        assert_eq!(s[0].tty.as_deref(), Some("/dev/tty1"));
        assert_eq!(s[0].argv, vec!["/sbin/flint-login"]);
        assert_eq!(s[1].mode, Mode::Once);
        assert_eq!(s[1].tty, None);
        assert_eq!(s[1].argv, vec!["/bin/logger", "-t", "boot", "hello"]);
    }

    #[test]
    fn empty_is_valid() {
        assert_eq!(parse_services("").unwrap(), vec![]);
        assert_eq!(parse_services("# only a comment\n").unwrap(), vec![]);
    }

    #[test]
    fn rejects_too_few_fields() {
        let e = parse_services("tty1 respawn /dev/tty1\n").unwrap_err();
        assert_eq!(e.line, 1);
    }

    #[test]
    fn rejects_bad_mode() {
        let e = parse_services("# c\na always - /bin/true\n").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(e.msg.contains("always"));
    }

    #[test]
    fn rejects_bad_tty() {
        assert!(parse_services("a once tty1 /bin/true\n").is_err());
        assert!(parse_services("a once /tmp/x /bin/true\n").is_err());
    }

    #[test]
    fn rejects_duplicate_and_bad_names() {
        assert!(parse_services("a once - /bin/true\na once - /bin/true\n").is_err());
        assert!(parse_services("a/b once - /bin/true\n").is_err());
    }

    #[test]
    fn parses_tasks() {
        let t = parse_tasks("# x\n/sbin/flint-modload\n/bin/hostname -F /etc/hostname\n").unwrap();
        assert_eq!(t, vec![vec!["/sbin/flint-modload"], vec!["/bin/hostname", "-F", "/etc/hostname"]]);
    }

    #[test]
    fn shipped_example_configs_parse() {
        let services = include_str!("../../../image/etc/flint/services");
        let boot = include_str!("../../../image/etc/flint/boot");
        assert!(!parse_services(services).unwrap().is_empty());
        assert!(parse_tasks(boot).is_ok());
    }

    #[test]
    fn default_service_is_respawning() {
        let d = default_services();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].mode, Mode::Respawn);
        assert!(!d[0].argv.is_empty());
    }
}
