//! The applet table. Every applet is `fn(&[String]) -> i32` taking the arguments after
//! the program name and returning the exit status.

mod basic;
mod files;
mod ls;
mod system;

pub type Applet = fn(&[String]) -> i32;

/// Keep in sync with ../applets.txt (a test checks this).
pub const TABLE: &[(&str, Applet)] = &[
    ("cat", files::cat),
    ("chmod", files::chmod),
    ("chown", files::chown),
    ("clear", basic::clear),
    ("cp", files::cp),
    ("echo", basic::echo),
    ("env", basic::env),
    ("false", basic::false_),
    ("head", files::head),
    ("hostname", basic::hostname),
    ("id", basic::id),
    ("kill", basic::kill),
    ("ln", files::ln),
    ("ls", ls::ls),
    ("mkdir", files::mkdir),
    ("mount", system::mount),
    ("mv", files::mv),
    ("ps", system::ps),
    ("pwd", basic::pwd),
    ("rm", files::rm),
    ("rmdir", files::rmdir),
    ("sleep", basic::sleep),
    ("sync", basic::sync),
    ("tail", files::tail),
    ("touch", files::touch),
    ("true", basic::true_),
    ("umount", system::umount),
    ("uname", basic::uname),
    ("wc", files::wc),
    ("whoami", basic::whoami),
];

pub fn find(name: &str) -> Option<Applet> {
    TABLE.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_applets_txt_and_is_sorted() {
        let listed: Vec<&str> = include_str!("../../applets.txt").lines().filter(|l| !l.is_empty()).collect();
        let table: Vec<&str> = TABLE.iter().map(|(n, _)| *n).collect();
        assert_eq!(table, listed);
        let mut sorted = table.clone();
        sorted.sort_unstable();
        assert_eq!(table, sorted);
    }
}
