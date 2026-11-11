//! The applet table. Every applet is `fn(&[String]) -> i32` taking the arguments after
//! the program name and returning the exit status.

mod awk;
mod basic;
mod files;
mod ls;
mod more;
mod pager;
mod system;
pub mod text;
mod vi;

pub type Applet = fn(&[String]) -> i32;

/// Keep in sync with ../applets.txt (a test checks this).
pub const TABLE: &[(&str, Applet)] = &[
    ("awk", awk::awk),
    ("basename", more::basename),
    ("cat", files::cat),
    ("chmod", files::chmod),
    ("chown", files::chown),
    ("clear", basic::clear),
    ("cp", files::cp),
    ("cut", text::cut),
    ("date", more::date),
    ("df", more::df),
    ("dirname", more::dirname),
    ("dmesg", more::dmesg),
    ("du", more::du),
    ("echo", basic::echo),
    ("env", basic::env),
    ("false", basic::false_),
    ("find", more::find),
    ("free", more::free),
    ("grep", text::grep),
    ("head", files::head),
    ("hostname", basic::hostname),
    ("id", basic::id),
    ("kill", basic::kill),
    ("less", pager::less),
    ("ln", files::ln),
    ("ls", ls::ls),
    ("mkdir", files::mkdir),
    ("mkfifo", more::mkfifo),
    ("mknod", more::mknod),
    ("mount", system::mount),
    ("mv", files::mv),
    ("ps", system::ps),
    ("pwd", basic::pwd),
    ("readlink", more::readlink),
    ("rm", files::rm),
    ("rmdir", files::rmdir),
    ("sed", text::sed),
    ("sleep", basic::sleep),
    ("sort", text::sort),
    ("stat", more::stat),
    ("sync", basic::sync),
    ("tail", files::tail),
    ("tee", text::tee),
    ("touch", files::touch),
    ("tr", text::tr),
    ("true", basic::true_),
    ("umount", system::umount),
    ("uname", basic::uname),
    ("uniq", text::uniq),
    ("vi", vi::vi),
    ("wc", files::wc),
    ("which", more::which),
    ("whoami", basic::whoami),
    ("xargs", text::xargs),
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
