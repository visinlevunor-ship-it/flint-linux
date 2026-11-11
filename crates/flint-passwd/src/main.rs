//! flint-passwd: change a password in /etc/shadow.
//!
//! Installed setuid root so that ordinary users can change their own password. Because
//! it runs with root's rights on behalf of anyone, it is kept small and strict:
//!
//! * who is asking comes from the *real* uid, never from arguments or the environment;
//! * a normal user can only change their own password, and must give the current one;
//! * it reads nothing from the environment and only touches /etc/passwd and /etc/shadow;
//! * /etc/shadow is replaced atomically, always with mode 0600 owned by root.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::thread;
use std::time::Duration;

use flint_auth::files::{find_user, find_user_by_uid, parse_passwd, set_shadow_hash, shadow_hash, User};
use flint_auth::hash::{hash_new, is_locked, verify};
use flint_core::tty::read_secret;

const SHADOW: &str = "/etc/shadow";
const PASSWD: &str = "/etc/passwd";
const WRONG_PASSWORD_DELAY: Duration = Duration::from_secs(2);

fn fail(msg: &str) -> i32 {
    eprintln!("passwd: {msg}");
    1
}

/// May the caller (real uid `caller`) change the password of `target`?
fn may_change(caller: u32, target: &User) -> Result<(), &'static str> {
    if caller == 0 || caller == target.uid {
        Ok(())
    } else {
        Err("you may only change your own password")
    }
}

/// Only non-root callers have to prove they know the current password.
fn needs_current_password(caller: u32) -> bool {
    caller != 0
}

/// Replace the file atomically: write a private temp file next to it, then rename over it.
fn write_shadow(text: &str) -> std::io::Result<()> {
    let tmp = format!("{SHADOW}.new");
    let _ = fs::remove_file(&tmp);
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp, SHADOW)
}

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    // SAFETY: plain libc queries.
    let (caller, effective) = unsafe { (libc::getuid(), libc::geteuid()) };
    if effective != 0 {
        return fail("must run as root (this program has to be installed setuid root)");
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() > 1 || args.first().is_some_and(|a| a.starts_with('-')) {
        eprintln!("usage: passwd [USER]");
        return 2;
    }

    let users = parse_passwd(&fs::read_to_string(PASSWD).unwrap_or_default());
    let target = match args.first() {
        Some(name) => match find_user(&users, name) {
            Some(u) => u.clone(),
            None => return fail(&format!("unknown user {name}")),
        },
        None => match find_user_by_uid(&users, caller) {
            Some(u) => u.clone(),
            None => return fail("cannot find your user name"),
        },
    };
    if let Err(msg) = may_change(caller, &target) {
        return fail(msg);
    }

    let shadow_text = fs::read_to_string(SHADOW).unwrap_or_default();

    if needs_current_password(caller) {
        let stored = shadow_hash(&shadow_text, &target.name).unwrap_or("");
        let Some(current) = read_secret("Current password: ") else { return 1 };
        let ok = !is_locked(stored) && verify(&current, stored) == Ok(true);
        if !ok {
            thread::sleep(WRONG_PASSWORD_DELAY);
            return fail("incorrect password");
        }
    }

    println!("Changing password for {}", target.name);
    let Some(new) = read_secret("New password: ") else { return 1 };
    if new.is_empty() {
        return fail("the password cannot be empty");
    }
    let Some(again) = read_secret("Retype new password: ") else { return 1 };
    if new != again {
        return fail("the passwords do not match");
    }

    let hash = match hash_new(&new) {
        Ok(h) => h,
        Err(e) => return fail(&format!("cannot get random bytes: {e}")),
    };
    match write_shadow(&set_shadow_hash(&shadow_text, &target.name, &hash)) {
        Ok(()) => {
            println!("Password updated");
            0
        }
        Err(e) => fail(&format!("cannot write {SHADOW}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(uid: u32) -> User {
        User { name: "u".into(), passwd: "x".into(), uid, gid: uid, gecos: String::new(), home: "/".into(), shell: "/bin/sh".into() }
    }

    #[test]
    fn who_may_change_what() {
        assert!(may_change(0, &user(1000)).is_ok(), "root may change anyone");
        assert!(may_change(1000, &user(1000)).is_ok(), "own password");
        assert!(may_change(1000, &user(1001)).is_err(), "someone else's");
        assert!(may_change(1000, &user(0)).is_err(), "root's");
    }

    #[test]
    fn only_non_root_needs_the_current_password() {
        assert!(!needs_current_password(0));
        assert!(needs_current_password(1000));
    }
}
