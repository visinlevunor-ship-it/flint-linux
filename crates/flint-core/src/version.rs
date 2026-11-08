//! The Flint Linux version, embedded at build time from ./VERSION.
//!
//! Policy: a rewrite of a component or any major architectural change bumps
//! MAJOR (and resets MINOR to 0); everything else bumps MINOR.

pub const NAME: &str = "Flint Linux";
/// `MAJOR.MINOR`, from the VERSION file.
pub const NUMBER: &str = env!("FLINT_VERSION");
/// Short git hash, `$FLINT_BUILD_ID`, or "nogit".
pub const BUILD: &str = env!("FLINT_BUILD");

/// e.g. `Flint Linux 0.1 (a1b2c3d)`
pub fn long() -> String {
    format!("{NAME} {NUMBER} ({BUILD})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_is_major_dot_minor() {
        let parts: Vec<&str> = NUMBER.split('.').collect();
        assert_eq!(parts.len(), 2, "got {NUMBER:?}");
        assert!(parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())));
    }

    #[test]
    fn long_contains_all_parts() {
        let s = long();
        assert!(s.starts_with("Flint Linux "));
        assert!(s.contains(NUMBER));
        assert!(s.ends_with(&format!("({BUILD})")));
    }
}
