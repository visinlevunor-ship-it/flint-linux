//! Who may open which devices.
//!
//! devtmpfs creates device nodes that only root can open. The X server runs as the user who
//! started it and opens the graphics card and the input devices itself, so `devd` gives
//! those to the `video` and `input` groups (the members are listed in /etc/group). A group
//! that does not exist leaves the device as it is.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Device name prefix (under /dev), group, mode. The rest of the name must be a number.
const RULES: &[(&str, &str, u32)] = &[
    ("dri/card", "video", 0o660),
    ("dri/renderD", "video", 0o660),
    ("fb", "video", 0o660),
    ("input/event", "input", 0o660),
    ("input/mouse", "input", 0o660),
    ("input/mice", "input", 0o660),
];

/// The group and mode for a device name like `dri/card0`, if it has a rule.
pub fn rule_for(devname: &str) -> Option<(&'static str, u32)> {
    RULES.iter().find_map(|&(prefix, group, mode)| {
        let rest = devname.strip_prefix(prefix)?;
        rest.bytes().all(|b| b.is_ascii_digit()).then_some((group, mode))
    })
}

/// A group's id from a group file (`name:x:gid:members`).
pub fn group_id(group_file: &str, name: &str) -> Option<u32> {
    group_file.lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next()? == name).then_some(())?;
        f.next()?;
        f.next()?.trim().parse().ok()
    })
}

/// Give one device (`devname` relative to `dev`) its group and mode, if it has a rule.
pub fn apply(dev: &Path, devname: &str, groups: &str) -> io::Result<bool> {
    let Some((group, mode)) = rule_for(devname) else { return Ok(false) };
    let Some(gid) = group_id(groups, group) else { return Ok(false) };
    let path = dev.join(devname);
    std::os::unix::fs::chown(&path, None, Some(gid))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
    Ok(true)
}

/// Apply the rules to every device already in `dev`. Returns how many were changed.
pub fn apply_all(dev: &Path, groups: &str) -> usize {
    let mut n = 0;
    for sub in ["", "dri", "input"] {
        let Ok(rd) = fs::read_dir(dev.join(sub)) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let devname = if sub.is_empty() { name } else { format!("{sub}/{name}") };
            if matches!(apply(dev, &devname, groups), Ok(true)) {
                n += 1;
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    const GROUPS: &str = "root:x:0:\ntty:x:5:flint\nvideo:x:44:flint\ninput:x:105:flint\n";

    #[test]
    fn rules_match_whole_device_names() {
        assert_eq!(rule_for("dri/card0"), Some(("video", 0o660)));
        assert_eq!(rule_for("dri/renderD128"), Some(("video", 0o660)));
        assert_eq!(rule_for("input/event12"), Some(("input", 0o660)));
        assert_eq!(rule_for("input/mice"), Some(("input", 0o660)));
        assert_eq!(rule_for("fb0"), Some(("video", 0o660)));
        assert_eq!(rule_for("input/js0"), None);
        assert_eq!(rule_for("dri/by-path"), None);
        assert_eq!(rule_for("fbdev"), None);
        assert_eq!(rule_for("sda"), None);
    }

    #[test]
    fn group_ids_come_from_the_group_file() {
        assert_eq!(group_id(GROUPS, "video"), Some(44));
        assert_eq!(group_id(GROUPS, "input"), Some(105));
        assert_eq!(group_id(GROUPS, "audio"), None);
        assert_eq!(group_id("vid:x:7:\n", "video"), None);
    }

    #[test]
    fn applies_group_and_mode() {
        let dir = std::env::temp_dir().join(format!("flint-access-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("input")).unwrap();
        fs::create_dir_all(dir.join("dri")).unwrap();
        for f in ["input/event3", "input/js0", "dri/card1", "null"] {
            fs::write(dir.join(f), b"").unwrap();
            fs::set_permissions(dir.join(f), fs::Permissions::from_mode(0o600)).unwrap();
        }
        // our own group, which we may always chown to
        // SAFETY: getgid has no preconditions.
        let gid = unsafe { libc::getgid() };
        let groups = format!("video:x:{gid}:\ninput:x:{gid}:\n");
        assert_eq!(apply_all(&dir, &groups), 2);
        let mode = |f: &str| fs::metadata(dir.join(f)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode("input/event3"), 0o660);
        assert_eq!(mode("dri/card1"), 0o660);
        assert_eq!(mode("input/js0"), 0o600);
        assert_eq!(mode("null"), 0o600);
        // no such group: nothing changes
        fs::set_permissions(dir.join("dri/card1"), fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!apply(&dir, "dri/card1", "root:x:0:\n").unwrap());
        assert_eq!(mode("dri/card1"), 0o600);
        fs::remove_dir_all(&dir).unwrap();
    }
}
