//! Path safety: archive paths must stay inside the install root.

use std::fs;
use std::path::{Path, PathBuf};

/// Validate a path from an archive or database. Returns it without trailing slashes.
/// Rejects absolute paths, `.`/`..`/empty components and characters that would break the
/// tab-separated database.
pub fn normalize_rel(path: &str) -> Result<String, String> {
    let p = path.trim_end_matches('/');
    if p.is_empty() {
        return Err("empty path".into());
    }
    if p.starts_with('/') {
        return Err(format!("absolute path not allowed: {path}"));
    }
    if p.chars().any(|c| c == '\0' || c == '\t' || c == '\n' || c == '\r') {
        return Err(format!("path contains a control character: {path:?}"));
    }
    for comp in p.split('/') {
        if comp.is_empty() || comp == "." || comp == ".." {
            return Err(format!("path escapes or is not normalized: {path}"));
        }
    }
    Ok(p.to_string())
}

/// `root/rel`, after checking that following any symlinks already on disk leaves the
/// directory inside `root`.
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = normalize_rel(rel)?;
    let full = root.join(&rel);
    let croot = fs::canonicalize(root).map_err(|e| format!("install root {}: {e}", root.display()))?;
    let mut ancestor = full.parent();
    while let Some(a) = ancestor {
        if fs::symlink_metadata(a).is_ok() {
            let canon = fs::canonicalize(a).map_err(|e| format!("{}: {e}", a.display()))?;
            if !canon.starts_with(&croot) {
                return Err(format!("{rel} would be written outside the install root (through a symlink)"));
            }
            break;
        }
        ancestor = a.parent();
    }
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn normalization() {
        assert_eq!(normalize_rel("usr/bin/x").unwrap(), "usr/bin/x");
        assert_eq!(normalize_rel("usr/bin/").unwrap(), "usr/bin");
        for bad in ["", "/", "/etc/passwd", "../x", "a/../b", "a/./b", "a//b", "a\tb", "a\nb", "."] {
            assert!(normalize_rel(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn symlinks_cannot_escape_the_root() {
        let base = std::env::temp_dir().join(format!("flint-pkg-paths-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("root");
        let outside = base.join("outside");
        fs::create_dir_all(root.join("usr")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("evil")).unwrap();
        symlink("usr", root.join("lib")).unwrap();
        assert!(safe_join(&root, "evil/passwd").is_err());
        assert!(safe_join(&root, "evil/deeper/passwd").is_err());
        assert_eq!(safe_join(&root, "lib/x").unwrap(), root.join("lib/x"), "a symlink inside the root is fine");
        assert!(safe_join(&root, "usr/new/dir/file").is_ok());
        assert!(safe_join(&root, "../x").is_err());
        let _ = fs::remove_dir_all(&base);
    }
}
